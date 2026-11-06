//! Disk use: identical files hard-linked into one ([`link_duplicates`]) and old files of a kind
//! removed ([`remove_older`]), for `ror clean` and tools built on ror.
//!
//! Linking is lossless for what ror writes: nothing is ever written into an existing file, only
//! replaced by a rename ([`crate::output::write_atomic`], ror-assets' cache), which leaves the other
//! names of a linked file on the old content. Copying over a linked file in place (`cp` onto an
//! existing file, Python's `shutil.copyfile`, an editor saving in place) changes every name: delete
//! or rename the target first (`cp --remove-destination`). NTFS allows 1023 names per file; past
//! that a group keeps a second file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use serde::Serialize;

/// Names written within this long are left alone: a writer may be about to replace them again.
pub const SETTLE: Duration = Duration::from_secs(10 * 60);
/// Smaller files are left alone: they live in the file table (or one cluster) and free next to nothing.
const MIN_SIZE: u64 = 4096;
/// Names one file may have (NTFS).
const MAX_LINKS: u32 = 1023;

/// What [`link_duplicates`] found and did.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Linked {
	/// Names looked at, and their bytes (each name counted).
	pub files: usize,
	pub bytes: u64,
	/// Bytes names already shared with other names before (hard links).
	pub shared: u64,
	/// Contents held in more than one file, and the names made links into one of them.
	pub groups: usize,
	pub links: usize,
	/// Bytes freed (in a dry run: that would be).
	pub freed: u64,
	/// Names left as they were: written in the last [`SETTLE`], changed meanwhile, or failing to link.
	pub skipped: usize,
}

/// What [`remove_older`] removed.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
	pub files: usize,
	/// Bytes freed: a file goes with its last name (in a dry run: would).
	pub freed: u64,
	pub failed: usize,
}

/// A file as the walk found it.
struct Found {
	path: PathBuf,
	size: u64,
	modified: SystemTime,
}

/// One file on disk (whatever its names): its volume and index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FileId {
	volume: u64,
	index: u64,
}

/// A file and the names the walk found for it.
struct Held<'a> {
	size: u64,
	modified: SystemTime,
	/// Its names on disk, all of them (not only those found).
	links: u32,
	names: Vec<&'a Found>,
}

/// Every file under `root` (symbolic links not followed) that `accept` takes, without temporary
/// names of writes in progress.
fn walk(root: &Path, accept: &(dyn Fn(&Path) -> bool + Sync), found: &mut Vec<Found>) {
	let Ok(entries) = std::fs::read_dir(root) else {
		return;
	};
	for entry in entries.flatten() {
		let Ok(kind) = entry.file_type() else {
			continue;
		};
		let path = entry.path();
		if kind.is_dir() {
			walk(&path, accept, found);
		} else if kind.is_file()
			&& !temporary(&path)
			&& accept(&path)
			&& let Ok(metadata) = entry.metadata()
		{
			found.push(Found { path, size: metadata.len(), modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH) });
		}
	}
}

/// A temporary name of an atomic write (ror's `<name>.tmp<pid>`, ror-assets' and this module's
/// `.<name>.….tmp`, and `.part`).
fn temporary(path: &Path) -> bool {
	let name = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
	name.ends_with(".tmp") || name.ends_with(".part") || path.extension().is_some_and(|extension| extension.to_string_lossy().starts_with("tmp"))
}

/// The file a name names, and how many names it has.
#[cfg(windows)]
fn identity(path: &Path) -> Option<(FileId, u32)> {
	use std::os::windows::io::AsRawHandle;
	use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
	let file = std::fs::File::open(path).ok()?;
	// SAFETY: a plain struct of integers, filled by the call below
	let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
	// SAFETY: the handle of a file open for the whole call
	if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
		return None;
	}
	Some((FileId { volume: info.dwVolumeSerialNumber as u64, index: (info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64 }, info.nNumberOfLinks))
}

#[cfg(not(windows))]
fn identity(path: &Path) -> Option<(FileId, u32)> {
	use std::os::unix::fs::MetadataExt;
	let metadata = std::fs::metadata(path).ok()?;
	Some((FileId { volume: metadata.dev(), index: metadata.ino() }, metadata.nlink() as u32))
}

fn hash(path: &Path) -> Option<blake3::Hash> {
	let file = std::fs::File::open(path).ok()?;
	let mut hasher = blake3::Hasher::new();
	hasher.update_reader(file).ok()?;
	Some(hasher.finalize())
}

/// Makes `name` another name of `keeper`'s file: a hard link under a temporary name, renamed over
/// `name`, so a reader finds either name complete at every moment. Left alone when `name` no
/// longer names the file `expected` (replaced since the walk).
fn link(keeper: &Path, name: &Path, expected: FileId) -> std::io::Result<()> {
	if identity(name).map(|(id, _)| id) != Some(expected) {
		return Err(std::io::Error::other("replaced since it was read"));
	}
	let file_name = name.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
	let temporary = name.with_file_name(format!(".{file_name}.{}.link.tmp", std::process::id()));
	let _ = std::fs::remove_file(&temporary);
	std::fs::hard_link(keeper, &temporary)?;
	std::fs::rename(&temporary, name).inspect_err(|_| {
		let _ = std::fs::remove_file(&temporary);
	})
}

/// Makes the identical files under `roots` (those `accept` takes, 4 KiB and more) one file with
/// several names: each other name becomes a hard link to the most recently written of them (so no
/// name looks older than it was). Contents are compared by BLAKE3 hash, files already one file are
/// read once. Names written in the last [`SETTLE`] stay as they are. With `dry_run` it only counts.
pub fn link_duplicates(roots: &[PathBuf], accept: &(dyn Fn(&Path) -> bool + Sync), dry_run: bool) -> Linked {
	let mut found = Vec::new();
	for root in roots {
		walk(root, accept, &mut found);
	}
	let mut report = Linked { files: found.len(), bytes: found.iter().map(|file| file.size).sum(), ..Linked::default() };
	// a file of a size no other one has has no copy
	let mut sizes: HashMap<u64, usize> = HashMap::new();
	for file in &found {
		*sizes.entry(file.size).or_default() += 1;
	}
	let candidates: Vec<&Found> = found.iter().filter(|file| file.size >= MIN_SIZE && sizes[&file.size] > 1).collect();
	let ids: Vec<Option<(FileId, u32)>> = candidates.par_iter().map(|file| identity(&file.path)).collect();
	let mut files: HashMap<FileId, Held> = HashMap::new();
	for (file, id) in candidates.iter().zip(ids) {
		let Some((id, links)) = id else {
			report.skipped += 1;
			continue;
		};
		files.entry(id).or_insert_with(|| Held { size: file.size, modified: file.modified, links, names: Vec::new() }).names.push(file);
	}
	report.shared = files.values().map(|held| held.size * (held.names.len() as u64 - 1)).sum();
	// the contents of files sharing their size with another file
	let mut by_size: HashMap<u64, Vec<FileId>> = HashMap::new();
	for (id, held) in &files {
		by_size.entry(held.size).or_default().push(*id);
	}
	let to_hash: Vec<FileId> = by_size.into_values().filter(|ids| ids.len() > 1).flatten().collect();
	let hashes: Vec<Option<blake3::Hash>> = to_hash.par_iter().map(|id| hash(&files[id].names[0].path)).collect();
	let mut groups: HashMap<(u64, blake3::Hash), Vec<FileId>> = HashMap::new();
	for (id, hash) in to_hash.iter().zip(hashes) {
		match hash {
			Some(hash) => groups.entry((files[id].size, hash)).or_default().push(*id),
			None => report.skipped += files[id].names.len(),
		}
	}
	let now = SystemTime::now();
	for ids in groups.values_mut().filter(|ids| ids.len() > 1) {
		report.groups += 1;
		ids.sort_by_key(|id| std::cmp::Reverse((files[id].modified, files[id].links)));
		let (mut keeper, mut keeper_links) = (ids[0], files[&ids[0]].links);
		for id in &ids[1..] {
			let held = &files[id];
			// a keeper at the link limit: this file keeps its names and takes the next ones
			if keeper_links + held.names.len() as u32 > MAX_LINKS {
				(keeper, keeper_links) = (*id, held.links);
				continue;
			}
			let keeper_path = &files[&keeper].names[0].path;
			let mut relinked = 0;
			for name in &held.names {
				let settled = now.duration_since(name.modified).unwrap_or_default() >= SETTLE;
				if settled && (dry_run || link(keeper_path, &name.path, *id).is_ok()) {
					relinked += 1;
					keeper_links += 1;
					report.links += 1;
				} else {
					report.skipped += 1;
				}
			}
			// its data goes with its last name
			if relinked == held.links {
				report.freed += held.size;
			}
		}
	}
	report
}

/// Removes the files under `roots` that `accept` takes and that were last written more than `age`
/// ago. With `dry_run` it only counts.
pub fn remove_older(roots: &[PathBuf], accept: &(dyn Fn(&Path) -> bool + Sync), age: Duration, dry_run: bool) -> Removed {
	let mut found = Vec::new();
	for root in roots {
		walk(root, accept, &mut found);
	}
	let now = SystemTime::now();
	let old: Vec<&Found> = found.iter().filter(|file| now.duration_since(file.modified).unwrap_or_default() > age).collect();
	let ids: Vec<Option<(FileId, u32)>> = old.par_iter().map(|file| identity(&file.path)).collect();
	let mut report = Removed::default();
	// per file: its names, those removed, its size
	let mut removed: HashMap<FileId, (u32, u32, u64)> = HashMap::new();
	for (file, id) in old.iter().zip(ids) {
		if !dry_run && std::fs::remove_file(&file.path).is_err() {
			report.failed += 1;
			continue;
		}
		report.files += 1;
		match id {
			Some((id, links)) => removed.entry(id).or_insert((links, 0, file.size)).1 += 1,
			None => report.freed += file.size,
		}
	}
	report.freed += removed.values().filter(|(links, gone, _)| gone >= links).map(|(_, _, size)| size).sum::<u64>();
	report
}

#[cfg(test)]
mod tests {
	use super::*;

	fn file(path: &Path, bytes: &[u8], age: Duration) {
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(path, bytes).unwrap();
		std::fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - age).unwrap();
	}

	fn same_file(a: &Path, b: &Path) -> bool {
		identity(a).map(|(id, _)| id) == identity(b).map(|(id, _)| id)
	}

	#[test]
	fn identical_files_become_one() {
		let dir = std::env::temp_dir().join(format!("ror-clean-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let (minute, hour, day) = (Duration::from_secs(60), Duration::from_secs(3600), Duration::from_secs(86400));
		let a = vec![7u8; 10_000];
		let b = vec![9u8; 10_000];
		let (one, two, three, other, fresh, fresher) =
			(dir.join("x").join("one.png"), dir.join("y").join("two.png"), dir.join("y").join("three.png"), dir.join("other.png"), dir.join("fresh.png"), dir.join("fresher.png"));
		file(&one, &a, day);
		file(&two, &a, hour);
		file(&three, &a, 2 * day);
		file(&other, &b, day);
		file(&fresh, &a, 2 * minute);
		file(&fresher, &a, minute);
		file(&dir.join("small.png"), b"tiny", day);
		file(&dir.join("small2.png"), b"tiny", day);
		file(&dir.join("skipped.txt"), &a, day);
		let png = |path: &Path| path.extension().is_some_and(|extension| extension == "png");
		let roots = [dir.clone()];

		let dry = link_duplicates(&roots, &png, true);
		assert_eq!((dry.files, dry.groups, dry.links, dry.freed, dry.skipped), (8, 1, 3, 30_000, 1));
		assert!(!same_file(&one, &two), "a dry run changes nothing");

		let report = link_duplicates(&roots, &png, false);
		assert_eq!((report.groups, report.links, report.freed, report.skipped), (1, 3, 30_000, 1));
		// the most recently written one keeps its name and the settled ones are names of it now; a name
		// written two minutes ago stays as it was
		assert!(same_file(&fresher, &one) && same_file(&one, &two) && same_file(&two, &three));
		assert!(!same_file(&two, &other) && !same_file(&two, &fresh));
		assert_eq!(std::fs::read(&three).unwrap(), a);
		// nothing more to do; what is shared is counted
		let again = link_duplicates(&roots, &png, false);
		assert_eq!((again.groups, again.links, again.shared, again.skipped), (1, 0, 30_000, 1));
		// a name replaced by a rename leaves the file to the other names
		crate::output::write_atomic(&two, &b).unwrap();
		assert_eq!((std::fs::read(&one).unwrap(), std::fs::read(&two).unwrap()), (a.clone(), b.clone()));
		// unchanged content keeps the file: the names stay one
		crate::output::write_atomic(&one, &a).unwrap();
		assert!(same_file(&one, &three));

		// removal by age: a file goes with its last name
		let old = dir.join("old");
		file(&old.join("p.png"), &a, 2 * day);
		file(&old.join("q.png"), &b, 2 * day);
		std::fs::hard_link(old.join("q.png"), dir.join("q-elsewhere.png")).unwrap();
		let dry = remove_older(std::slice::from_ref(&old), &png, day, true);
		assert_eq!((dry.files, dry.freed), (2, 10_000), "q.png keeps another name");
		let gone = remove_older(std::slice::from_ref(&old), &png, day, false);
		assert_eq!((gone.files, gone.freed, gone.failed), (2, 10_000, 0));
		assert!(!old.join("p.png").exists() && std::fs::read(dir.join("q-elsewhere.png")).unwrap() == b);
		assert_eq!(remove_older(&roots, &png, day + day / 2, true).files, 1, "only q-elsewhere.png is two days old");
		let _ = std::fs::remove_dir_all(&dir);
	}
}
