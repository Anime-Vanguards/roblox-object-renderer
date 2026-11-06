//! Disk cache (shared by processes using one state dir) and the in-memory cache.
//!
//! Disk layout: `<state_dir>/assets/<2 hex>/<32 hex>.bin` (content) and `.json` (metadata: uri,
//! fetch time, source host, size, sniffed kind), named by the FNV-1a 128-bit hash of the canonical
//! URI; `.refused` remembers a download the API refused (its status, until when, why). Files are written to a unique temporary name in the same directory and renamed into place,
//! so readers in any process see either nothing or a complete file. No secret is ever written.
//!
//! A cached file is never written into, only replaced by a rename. So one file may have several
//! names (hard links: another state dir's download taken on a miss, [`crate::AssetConfig::shared`],
//! or identical files linked by `ror clean`), and replacing one name leaves the others as they were.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{Fetched, RefusalReason};

/// FNV-1a, 128 bits, as 32 hex digits.
pub(crate) fn hash_key(key: &str) -> String {
	const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
	const PRIME: u128 = 0x0000000001000000000000000000013b;
	let mut hash = OFFSET;
	for b in key.as_bytes() {
		hash ^= *b as u128;
		hash = hash.wrapping_mul(PRIME);
	}
	format!("{hash:032x}")
}

/// Metadata stored next to a cached file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Meta {
	pub uri: String,
	/// Unix seconds
	pub fetched_at: u64,
	pub source_host: String,
	pub size: usize,
	pub kind: String,
}

/// A refused download remembered on disk: its HTTP status, until when (Unix seconds) and why (none
/// for a 404, a 406 or a CDN's refusal, nor in files written before reasons were kept).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Refusal {
	pub status: u16,
	pub until: u64,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<RefusalReason>,
	/// A transcode answered "being made" (406): since when, unbroken (Unix seconds)
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub since: Option<u64>,
}

pub(crate) struct DiskCache {
	dir: PathBuf,
	/// The content store's `assets` folder ([`crate::store`]): looked in on a miss before the other
	/// state dirs, and given every download made here or taken from another state dir.
	store: Option<PathBuf>,
	/// Other state dirs' `assets` folders, looked in on a miss ([`DiskCache::read`]).
	shared: Vec<PathBuf>,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// (content, metadata) paths of a key's hash in an `assets` folder.
fn files(dir: &Path, hash: &str) -> (PathBuf, PathBuf) {
	let dir = dir.join(&hash[..2]);
	(dir.join(format!("{hash}.bin")), dir.join(format!("{hash}.json")))
}

impl DiskCache {
	/// The cache of `state_dir`, sharing `shared`'s downloads and the content store's (`store`: those of
	/// every state dir that used it too).
	pub(crate) fn new(state_dir: &Path, shared: &[PathBuf], store: Option<&Path>) -> DiskCache {
		let dir = state_dir.join("assets");
		// this state dir named among the shared ones would link a file onto itself
		let same = |other: &Path| matches!((std::fs::canonicalize(other), std::fs::canonicalize(&dir)), (Ok(a), Ok(b)) if a == b);
		let mut others: Vec<PathBuf> = shared.to_vec();
		if let Some(store) = store {
			crate::store::register(store, state_dir);
			others.extend(crate::store::state_dirs(store, state_dir));
		}
		let mut shared: Vec<PathBuf> = Vec::new();
		for other in others.iter().map(|other| other.join("assets")) {
			if other != dir && !shared.contains(&other) && !same(&other) {
				shared.push(other);
			}
		}
		let store = store.map(|store| store.join("assets")).filter(|store| *store != dir && !same(store));
		DiskCache { dir, store, shared }
	}

	/// (content, metadata) paths for a canonical URI.
	pub(crate) fn paths(&self, key: &str) -> (PathBuf, PathBuf) {
		files(&self.dir, &hash_key(key))
	}

	/// Whether `key` has bytes on disk (without reading them), here or in a shared state dir.
	pub(crate) fn contains(&self, key: &str) -> bool {
		let hash = hash_key(key);
		files(&self.dir, &hash).0.is_file() || self.store.iter().chain(&self.shared).any(|dir| files(dir, &hash).0.is_file())
	}

	/// The bytes cached under `key`. A miss here that a shared state dir has cached takes its file:
	/// hard-linked in (one file on disk for both state dirs; a copy where no link can be made), the
	/// metadata with it.
	///
	/// The content store is looked in first: its file is taken only when its metadata names `key` and
	/// its size (one damaged or misplaced is removed), and counts as used. Another state dir's is taken
	/// unless its metadata disagrees so, and goes to the store too.
	pub(crate) fn read(&self, key: &str) -> Option<Vec<u8>> {
		let hash = hash_key(key);
		let (bin, json) = files(&self.dir, &hash);
		if let Ok(bytes) = std::fs::read(&bin) {
			return Some(bytes);
		}
		let meta_of = |json: &Path| -> Option<Meta> { std::fs::read(json).ok().and_then(|text| serde_json::from_slice(&text).ok()) };
		if let Some(store) = &self.store {
			let (store_bin, store_json) = files(store, &hash);
			if let Ok(bytes) = std::fs::read(&store_bin) {
				if meta_of(&store_json).is_some_and(|meta| meta.uri == key && meta.size == bytes.len()) {
					crate::store::touch(&store_bin);
					// a failed link only costs the next process the same lookup
					if link_atomic(&store_bin, &bin).is_ok() {
						let _ = link_atomic(&store_json, &json);
					}
					return Some(bytes);
				}
				let _ = std::fs::remove_file(&store_bin);
				let _ = std::fs::remove_file(&store_json);
			}
		}
		for dir in &self.shared {
			let (shared_bin, shared_json) = files(dir, &hash);
			let Ok(bytes) = std::fs::read(&shared_bin) else {
				continue;
			};
			// one its metadata disagrees with (damaged, or written into through another name) is left
			if meta_of(&shared_json).is_some_and(|meta| meta.uri != key || meta.size != bytes.len()) {
				continue;
			}
			// a failed link only costs the next process the same lookup
			if link_atomic(&shared_bin, &bin).is_ok() {
				let _ = link_atomic(&shared_json, &json);
				self.publish(&hash);
			}
			return Some(bytes);
		}
		None
	}

	/// Gives the content store this state dir's download of a key's `hash` (a name of the same file).
	fn publish(&self, hash: &str) {
		let Some(store) = &self.store else { return };
		let (bin, json) = files(&self.dir, hash);
		let (store_bin, store_json) = files(store, hash);
		if json.is_file() && link_atomic(&bin, &store_bin).is_ok() {
			let _ = link_atomic(&json, &store_json);
			crate::store::maybe_prune(store.parent().unwrap_or(store));
		}
	}

	#[cfg(test)]
	pub(crate) fn read_meta(&self, key: &str) -> Option<Meta> {
		let (_, json) = self.paths(key);
		serde_json::from_slice(&std::fs::read(json).ok()?).ok()
	}

	/// Stores content then metadata, each atomically, and gives them to the content store.
	pub(crate) fn write(&self, key: &str, bytes: &[u8], meta: &Meta) -> std::io::Result<()> {
		let (bin, json) = self.paths(key);
		write_atomic(&bin, bytes)?;
		let text = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
		write_atomic(&json, &text)?;
		self.publish(&hash_key(key));
		Ok(())
	}

	/// Where a refusal of `key` is remembered: this state dir, then the content store (which every
	/// state dir's refusals go to: a fresh one need not ask again, nor wait out a transcode being made).
	fn refusal_paths(&self, key: &str) -> impl Iterator<Item = PathBuf> {
		let hash = hash_key(key);
		[Some(&self.dir), self.store.as_ref()].into_iter().flatten().map(move |dir| dir.join(&hash[..2]).join(format!("{hash}.refused"))).collect::<Vec<_>>().into_iter()
	}

	/// The refusal last written for `key` here or in the store (the one remembered longer).
	fn newest_refusal(&self, key: &str) -> Option<Refusal> {
		self.refusal_paths(key).filter_map(|path| serde_json::from_slice::<Refusal>(&std::fs::read(path).ok()?).ok()).max_by_key(|refusal| refusal.until)
	}

	/// The refusal of a download of `key`, while it is remembered (`now` in Unix seconds). An
	/// authorisation refusal (401, 403, 409) without a reason is not (written before reasons were
	/// kept, or a CDN's): it is asked again.
	pub(crate) fn read_refusal(&self, key: &str, now: u64) -> Option<Refusal> {
		let refusal = self.newest_refusal(key)?;
		let current = refusal.reason.is_some() || !matches!(refusal.status, 401 | 403 | 409);
		(refusal.until > now && current).then_some(refusal)
	}

	/// The refusal last written for `key`, remembered still or not.
	pub(crate) fn last_refusal(&self, key: &str) -> Option<Refusal> {
		self.newest_refusal(key)
	}

	pub(crate) fn write_refusal(&self, key: &str, refusal: &Refusal) -> std::io::Result<()> {
		let text = serde_json::to_vec(refusal).map_err(std::io::Error::other)?;
		let mut paths = self.refusal_paths(key);
		let own = paths.next().expect("this state dir's");
		for path in paths {
			let _ = write_atomic(&path, &text);
		}
		write_atomic(&own, &text)
	}
}

/// A unique temporary name next to `path` (its directory created).
fn temp_beside(path: &Path) -> std::io::Result<PathBuf> {
	let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
	std::fs::create_dir_all(dir)?;
	let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("entry");
	let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
	let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
	Ok(dir.join(format!(".{name}.{}.{unique}.{nanos}.tmp", std::process::id())))
}

/// Writes `bytes` to a unique temporary file next to `path` and renames it into place. When the
/// rename fails because another process holds or just replaced the target, an existing target
/// counts as success (same URI, same content).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let temp = temp_beside(path)?;
	{
		let mut file = std::fs::File::create(&temp)?;
		file.write_all(bytes)?;
	}
	place(&temp, path)
}

/// Makes `path` another name of the file `source` names (a hard link; a copy where no link can be
/// made: another volume, a file at the file system's link limit), through a temporary name and a
/// rename as [`write_atomic`].
pub(crate) fn link_atomic(source: &Path, path: &Path) -> std::io::Result<()> {
	let temp = temp_beside(path)?;
	if std::fs::hard_link(source, &temp).is_err()
		&& let Err(error) = std::fs::copy(source, &temp)
	{
		let _ = std::fs::remove_file(&temp);
		return Err(error);
	}
	place(&temp, path)
}

/// Renames `temp` to `path` (see [`write_atomic`]).
fn place(temp: &Path, path: &Path) -> std::io::Result<()> {
	let mut last_error = None;
	for attempt in 0..5u32 {
		match std::fs::rename(temp, path) {
			Ok(()) => return Ok(()),
			Err(e) => {
				if path.is_file() && attempt > 0 {
					let _ = std::fs::remove_file(temp);
					return Ok(());
				}
				last_error = Some(e);
				std::thread::sleep(Duration::from_millis(5 * (attempt as u64 + 1)));
			}
		}
	}
	let _ = std::fs::remove_file(temp);
	Err(last_error.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Identity of a local file's contents, to notice edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
	pub len: u64,
	pub modified: Option<SystemTime>,
}

impl FileStamp {
	pub(crate) fn of(path: &Path) -> Option<FileStamp> {
		let meta = std::fs::metadata(path).ok()?;
		Some(FileStamp { len: meta.len(), modified: meta.modified().ok() })
	}
}

struct Entry {
	value: Arc<Fetched>,
	stamp: Option<FileStamp>,
	tick: u64,
}

/// Least-recently-used cache bounded by content bytes.
pub(crate) struct MemoryCache {
	budget: usize,
	used: usize,
	tick: u64,
	entries: HashMap<String, Entry>,
	order: BTreeMap<u64, String>,
}

impl MemoryCache {
	pub(crate) fn new(budget: usize) -> MemoryCache {
		MemoryCache { budget, used: 0, tick: 0, entries: HashMap::new(), order: BTreeMap::new() }
	}

	/// The entry for `key`; `stamp` (local files) must match the stored one.
	pub(crate) fn get(&mut self, key: &str, stamp: Option<FileStamp>) -> Option<Arc<Fetched>> {
		self.tick += 1;
		let tick = self.tick;
		let entry = self.entries.get_mut(key)?;
		if stamp.is_some() && entry.stamp != stamp {
			return None;
		}
		self.order.remove(&entry.tick);
		entry.tick = tick;
		self.order.insert(tick, key.to_string());
		Some(entry.value.clone())
	}

	pub(crate) fn insert(&mut self, key: &str, value: Arc<Fetched>, stamp: Option<FileStamp>) {
		let size = value.bytes.len();
		if size > self.budget {
			return;
		}
		self.remove(key);
		while self.used + size > self.budget {
			let Some((_, oldest)) = self.order.pop_first() else {
				break;
			};
			if let Some(entry) = self.entries.remove(&oldest) {
				self.used -= entry.value.bytes.len();
			}
		}
		self.tick += 1;
		self.order.insert(self.tick, key.to_string());
		self.entries.insert(key.to_string(), Entry { value, stamp, tick: self.tick });
		self.used += size;
	}

	fn remove(&mut self, key: &str) {
		if let Some(entry) = self.entries.remove(key) {
			self.order.remove(&entry.tick);
			self.used -= entry.value.bytes.len();
		}
	}

	pub(crate) fn used(&self) -> usize {
		self.used
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Source;

	fn fetched(n: usize) -> Arc<Fetched> {
		Arc::new(Fetched { uri: format!("u{n}"), bytes: vec![0; n], source: Source::Network })
	}

	#[test]
	fn fnv_known_values() {
		// FNV-1a 128 reference values
		assert_eq!(hash_key(""), "6c62272e07bb014262b821756295c58d");
		assert_eq!(hash_key("a"), "d228cb696f1a8caf78912b704e4a8964");
	}

	#[test]
	fn lru_budget() {
		let mut cache = MemoryCache::new(100);
		cache.insert("a", fetched(40), None);
		cache.insert("b", fetched(40), None);
		assert!(cache.get("a", None).is_some());
		// "b" is now the least recently used
		cache.insert("c", fetched(40), None);
		assert!(cache.get("b", None).is_none());
		assert!(cache.get("a", None).is_some());
		assert!(cache.get("c", None).is_some());
		assert_eq!(cache.used(), 80);
		// Larger than the budget: not cached
		cache.insert("huge", fetched(101), None);
		assert!(cache.get("huge", None).is_none());
		// Replacing an entry keeps the accounting right
		cache.insert("a", fetched(10), None);
		assert_eq!(cache.used(), 50);
	}

	#[test]
	fn stamps_invalidate() {
		let mut cache = MemoryCache::new(100);
		let stamp = FileStamp { len: 3, modified: None };
		cache.insert("f", fetched(3), Some(stamp));
		assert!(cache.get("f", Some(stamp)).is_some());
		assert!(cache.get("f", Some(FileStamp { len: 4, modified: None })).is_none());
	}

	#[test]
	fn refusals_reach_other_state_dirs_through_the_store() {
		let root = std::env::temp_dir().join(format!("ror-assets-refusal-store-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		let store = root.join("store");
		let (first, fresh) = (DiskCache::new(&root.join("a"), &[], Some(&store)), DiskCache::new(&root.join("b"), &[], Some(&store)));
		first.write_refusal("rbxassetid://5#ktx2", &Refusal { status: 406, until: 600, reason: None, since: Some(0) }).unwrap();
		let read = fresh.read_refusal("rbxassetid://5#ktx2", 1).expect("the store's");
		assert_eq!((read.status, read.since), (406, Some(0)));
		// the one remembered longer counts
		fresh.write_refusal("rbxassetid://5#ktx2", &Refusal { status: 406, until: 900, reason: None, since: Some(0) }).unwrap();
		assert_eq!(first.read_refusal("rbxassetid://5#ktx2", 700).map(|refusal| refusal.until), Some(900));
		assert!(DiskCache::new(&root.join("c"), &[], None).read_refusal("rbxassetid://5#ktx2", 1).is_none(), "without the store");
		let _ = std::fs::remove_dir_all(&root);
	}

	#[test]
	fn refusals_keep_their_reason_and_read_older_files() {
		let state = std::env::temp_dir().join(format!("ror-assets-refusals-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		let disk = DiskCache::new(&state, &[], None);
		disk.write_refusal("rbxassetid://19", &Refusal { status: 403, until: 100, reason: Some(RefusalReason::Moderated), since: None }).unwrap();
		let read = disk.read_refusal("rbxassetid://19", 99).unwrap();
		assert_eq!((read.status, read.reason), (403, Some(RefusalReason::Moderated)));
		assert!(disk.read_refusal("rbxassetid://19", 100).is_none(), "expired");
		// files written before reasons were kept: a 404 still holds, a 403 is asked again for its reason
		write_atomic(&disk.refusal_paths("rbxassetid://404").next().unwrap(), br#"{"status":404,"until":100}"#).unwrap();
		let read = disk.read_refusal("rbxassetid://404", 0).unwrap();
		assert_eq!((read.status, read.reason), (404, None));
		write_atomic(&disk.refusal_paths("rbxassetid://9").next().unwrap(), br#"{"status":403,"until":100}"#).unwrap();
		assert!(disk.read_refusal("rbxassetid://9", 0).is_none());
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn atomic_writes_from_many_threads() {
		let dir = std::env::temp_dir().join(format!("ror-assets-atomic-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let path = dir.join("x").join("file.bin");
		std::thread::scope(|scope| {
			for i in 0..16u8 {
				let path = &path;
				scope.spawn(move || {
					let data = vec![i; 100_000];
					write_atomic(path, &data).unwrap();
				});
			}
		});
		let data = std::fs::read(&path).unwrap();
		assert_eq!(data.len(), 100_000);
		assert!(data.iter().all(|&b| b == data[0]), "a torn write");
		// No temporary files left behind
		let leftovers = std::fs::read_dir(dir.join("x")).unwrap().count();
		assert_eq!(leftovers, 1);
		let _ = std::fs::remove_dir_all(&dir);
	}
}
