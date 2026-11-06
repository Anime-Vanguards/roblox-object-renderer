//! Decodes the real meshes on this machine: Roblox's content cache (`rbx-storage` files and the
//! blobs inline in `rbx-storage.db`) and the newest Studio install. Skips (passes) when they are
//! absent. Nothing from these locations is committed.
//!
//! A file (or the database) this test binary already passed, unchanged in size and time since, is not
//! read again ([`Passed`]): the whole sweep, 2.9 GB in 22,731 files and a 169 MB database, took 155 s
//! of every workspace test run, a decoder change or not (2026-11-02). A rebuilt binary (ror-mesh
//! changed) checks everything again.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ror_mesh::validate::{self, Severity};
use ror_mesh::{Kind, MeshError, rbx_storage, sniff};

#[derive(Default)]
struct Outcome {
	decoded: usize,
	failures: Vec<String>,
	errors: Vec<String>,
	/// Files that passed before and were skipped
	skipped: usize,
}

/// The files this test binary has passed: path, size and modification time a line, in the temp dir
/// under the binary's own size and time.
struct Passed {
	file: Option<PathBuf>,
	known: HashSet<String>,
	now: Vec<String>,
}

impl Passed {
	fn load() -> Self {
		let stamp = std::env::current_exe().ok().and_then(|exe| stamp(&exe));
		let file = stamp.map(|stamp| std::env::temp_dir().join(format!("ror-mesh-local-cache-{}.txt", stamp.replace(['\t', ':'], "-"))));
		let known = file.as_ref().and_then(|file| std::fs::read_to_string(file).ok()).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default();
		Self { file, known, now: Vec::new() }
	}

	/// The line `path` passes under, unless it was passed before (`None`).
	fn unseen(&self, path: &Path) -> Option<String> {
		let line = format!("{}\t{}", path.display(), stamp(path)?);
		(!self.known.contains(&line)).then_some(line)
	}

	fn save(&mut self, outcome: &Outcome) {
		if let Some(file) = &self.file
			&& outcome.failures.is_empty()
			&& outcome.errors.is_empty()
			&& !self.now.is_empty()
		{
			self.known.extend(self.now.drain(..));
			let mut lines: Vec<&String> = self.known.iter().collect();
			lines.sort();
			let _ = std::fs::write(file, lines.into_iter().map(|line| format!("{line}\n")).collect::<String>());
		}
	}
}

/// A file's size and modification time.
fn stamp(path: &Path) -> Option<String> {
	let metadata = std::fs::metadata(path).ok()?;
	let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
	Some(format!("{}\t{modified}", metadata.len()))
}

fn visit(origin: &str, bytes: &[u8], outcome: &mut Outcome) {
	let content = rbx_storage::parse_rbxh(bytes).map_or(bytes, |entry| entry.content);
	match sniff(content) {
		Kind::Unknown => return,
		Kind::Model if !content.windows(8).any(|window| window == b"MeshData") => return,
		_ => {}
	}
	match ror_mesh::decode_any(content) {
		Ok(mesh) => {
			outcome.decoded += 1;
			for issue in validate::check(&mesh) {
				if issue.severity == Severity::Error {
					outcome.errors.push(format!("{origin}: {} {}", issue.check, issue.detail));
				}
			}
		}
		Err(MeshError::NoUnionData) => {}
		Err(error) => outcome.failures.push(format!("{origin}: {error}")),
	}
}

fn walk(path: &Path, outcome: &mut Outcome, passed: &mut Passed) {
	if path.is_dir() {
		let Ok(entries) = std::fs::read_dir(path) else {
			return;
		};
		for entry in entries.flatten() {
			walk(&entry.path(), outcome, passed);
		}
		return;
	}
	let wanted = match path.extension().and_then(|extension| extension.to_str()) {
		None => true,
		Some(extension) => extension.eq_ignore_ascii_case("mesh"),
	};
	if !wanted {
		return;
	}
	let Some(line) = passed.unseen(path) else {
		outcome.skipped += 1;
		return;
	};
	if let Ok(bytes) = std::fs::read(path) {
		let failed = outcome.failures.len() + outcome.errors.len();
		visit(&path.display().to_string(), &bytes, outcome);
		if outcome.failures.len() + outcome.errors.len() == failed {
			passed.now.push(line);
		}
	}
}

fn read_db(db: &Path, outcome: &mut Outcome, passed: &mut Passed) {
	let Some(line) = passed.unseen(db) else {
		outcome.skipped += 1;
		return;
	};
	let failed = outcome.failures.len() + outcome.errors.len();
	read_db_copy(db, outcome);
	if outcome.failures.len() + outcome.errors.len() == failed {
		passed.now.push(line);
	}
}

fn read_db_copy(db: &Path, outcome: &mut Outcome) {
	// Read a copy so the live database (possibly open in Studio) is never touched.
	let temp = std::env::temp_dir().join(format!("ror-mesh-test-{}", std::process::id()));
	if std::fs::create_dir_all(&temp).is_err() {
		return;
	}
	let copy = temp.join("rbx-storage.db");
	if std::fs::copy(db, &copy).is_ok() {
		for suffix in ["-wal", "-shm"] {
			let mut from = db.as_os_str().to_owned();
			from.push(suffix);
			let mut to = copy.as_os_str().to_owned();
			to.push(suffix);
			let _ = std::fs::copy(PathBuf::from(from), PathBuf::from(to));
		}
		if let Ok(connection) = rusqlite::Connection::open(&copy)
			&& let Ok(mut statement) =
				connection.prepare("select id, content from files where content is not null and length(content) > 0")
			&& let Ok(mut rows) = statement.query([])
		{
			while let Ok(Some(row)) = rows.next() {
				let (Ok(id), Ok(content)) = (row.get::<_, Vec<u8>>(0), row.get::<_, Vec<u8>>(1)) else {
					continue;
				};
				let origin = format!("db:{}", id.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
				visit(&origin, &content, outcome);
			}
		}
	}
	let _ = std::fs::remove_dir_all(&temp);
}

fn studio_install() -> Option<PathBuf> {
	let versions = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("Versions");
	std::fs::read_dir(versions)
		.ok()?
		.flatten()
		.map(|entry| entry.path())
		.filter(|path| path.join("RobloxStudioBeta.exe").is_file() && path.join("content").is_dir())
		.max_by_key(|path| {
			std::fs::metadata(path.join("RobloxStudioBeta.exe")).and_then(|metadata| metadata.modified()).ok()
		})
}

#[test]
fn local_meshes_decode_cleanly() {
	let mut outcome = Outcome::default();
	let mut passed = Passed::load();
	if let Some(dir) = rbx_storage::default_dir() {
		walk(&dir, &mut outcome, &mut passed);
		let db = dir.with_extension("db");
		if db.is_file() {
			read_db(&db, &mut outcome, &mut passed);
		}
	}
	if let Some(install) = studio_install() {
		walk(&install.join("content"), &mut outcome, &mut passed);
	}
	passed.save(&outcome);
	if outcome.decoded == 0 && outcome.failures.is_empty() && outcome.skipped == 0 {
		eprintln!("no local Roblox data; skipping");
		return;
	}
	eprintln!("decoded {} local meshes, {} files passed before skipped", outcome.decoded, outcome.skipped);
	assert!(outcome.failures.is_empty(), "{} failures:\n{}", outcome.failures.len(), outcome.failures.join("\n"));
	assert!(outcome.errors.is_empty(), "{} sanity errors:\n{}", outcome.errors.len(), outcome.errors.join("\n"));
}
