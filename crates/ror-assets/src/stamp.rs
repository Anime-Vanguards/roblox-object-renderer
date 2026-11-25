//! Identity of a local file's contents across caches and renders.
//!
//! One rule for every local input (models, maps, lending places, images, meshes, packs, skins and
//! skies): byte length, modified time and metadata change time (`FILE_BASIC_INFO.ChangeTime` on
//! Windows, `ctime` on Unix), plus a content hash while either timestamp is within the recent
//! window (closing same-tick rewrites) or when `modified` is absent.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Files modified or metadata-changed within this window are content-hashed so same-tick rewrites
/// (~15.6 ms Windows timer ticks) cannot collide.
const RECENT_WINDOW: Duration = Duration::from_secs(2);
/// Largest file hashed inside the recent window.
const RECENT_HASH_MAX_BYTES: u64 = 64 << 20;
/// Most recently hashed files retained across the recent window so unchanged files keep one key as
/// they age; past this cap the table clears and starts fresh.
const FILE_HASHES_KEPT: usize = 4096;

/// Hashes taken for local files while recent, keyed by `(path, len, modified_nanos, change_nanos)`:
/// `(hash, settled)` where `settled` becomes true after one re-read once the file ages past
/// [`RECENT_WINDOW`] (catching any rewrite in the last timer tick it was read in).
type FileHashes = Mutex<HashMap<(PathBuf, u64, u128, u128), ([u8; 8], bool)>>;
static FILE_HASHES: OnceLock<FileHashes> = OnceLock::new();

/// Identity of a local file's contents, to detect edits, time-preserving copies and renames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
	len: u64,
	modified_nanos: u128,
	change_nanos: u128,
	hash: Option<[u8; 8]>,
}

impl FileStamp {
	/// Reads `path`'s metadata and builds its stamp.
	pub fn try_of(path: &Path) -> std::io::Result<FileStamp> {
		let meta = std::fs::metadata(path)?;
		Ok(Self::from_parts(path, meta.len(), meta.modified().ok(), Self::change_time(path, &meta)))
	}

	/// Reads `path`'s metadata and builds its stamp, or `None` if it cannot be read.
	pub fn of(path: &Path) -> Option<FileStamp> {
		Self::try_of(path).ok()
	}

	/// Like [`FileStamp::of`], returning `None` unless `path` is a regular file.
	pub fn of_file(path: &Path) -> Option<FileStamp> {
		let meta = std::fs::metadata(path).ok().filter(|meta| meta.is_file())?;
		Some(Self::from_parts(path, meta.len(), meta.modified().ok(), Self::change_time(path, &meta)))
	}

	/// Builds a stamp from already-read metadata components (exposed for deterministic collision tests).
	pub fn from_parts(path: &Path, len: u64, modified_time: Option<SystemTime>, change_time: Option<SystemTime>) -> FileStamp {
		let unix_nanos = |time: Option<SystemTime>| time.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |time| time.as_nanos());
		let modified_nanos = unix_nanos(modified_time);
		let change_nanos = unix_nanos(change_time);
		let is_recent = |time: Option<SystemTime>| time.is_some_and(|time| time.elapsed().map_or(true, |age| age < RECENT_WINDOW));
		let recent = modified_time.is_none() || is_recent(modified_time) || is_recent(change_time);
		if len > RECENT_HASH_MAX_BYTES {
			return FileStamp { len, modified_nanos, change_nanos, hash: None };
		}
		let hashes = FILE_HASHES.get_or_init(Default::default);
		let cache_key = (path.to_path_buf(), len, modified_nanos, change_nanos);
		if !recent {
			match hashes.lock().get(&cache_key).copied() {
				None => return FileStamp { len, modified_nanos, change_nanos, hash: None },
				Some((hash, true)) => return FileStamp { len, modified_nanos, change_nanos, hash: Some(hash) },
				Some((_, false)) => {}
			}
		}
		let hash = if let Ok(bytes) = std::fs::read(path) {
			let digest = blake3::hash(&bytes);
			let mut short = [0u8; 8];
			short.copy_from_slice(&digest.as_bytes()[..8]);
			let mut table = hashes.lock();
			if table.len() >= FILE_HASHES_KEPT {
				table.clear();
			}
			table.insert(cache_key, (short, !recent));
			Some(short)
		} else {
			None
		};
		FileStamp { len, modified_nanos, change_nanos, hash }
	}

	/// Byte length observed when this stamp was taken.
	pub fn len(&self) -> u64 {
		self.len
	}

	/// Whether the file had zero length when this stamp was taken.
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Stable string encoding of this stamp (for string-keyed caches).
	pub fn key_suffix(&self) -> String {
		match self.hash {
			Some(hash) => {
				let mut out = format!("{}:{}:{}:", self.len, self.modified_nanos, self.change_nanos);
				for byte in hash {
					let _ = write!(out, "{byte:02x}");
				}
				out
			}
			None => format!("{}:{}:{}", self.len, self.modified_nanos, self.change_nanos),
		}
	}

	/// Stamps a URI or path string with this file identity.
	pub fn stamp_uri(&self, uri: &str) -> String {
		format!("{uri}@{}", self.key_suffix())
	}

	/// The inode/metadata change time of `path` (NTFS `FILE_BASIC_INFO.ChangeTime` on Windows,
	/// `ctime` on Unix), which advances on rewrites, renames and time-preserving copies even when
	/// `mtime` is restored to an older timestamp.
	#[cfg(windows)]
	pub fn change_time(path: &Path, _meta: &std::fs::Metadata) -> Option<SystemTime> {
		use std::os::windows::ffi::OsStrExt;
		use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
		use windows_sys::Win32::Storage::FileSystem::{
			CreateFileW, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
		};
		let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
		// SAFETY: `wide` is null-terminated UTF-16; `handle` is closed before returning.
		unsafe {
			let handle = CreateFileW(
				wide.as_ptr(),
				FILE_READ_ATTRIBUTES,
				FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
				std::ptr::null(),
				OPEN_EXISTING,
				FILE_FLAG_BACKUP_SEMANTICS,
				std::ptr::null_mut(),
			);
			if handle == INVALID_HANDLE_VALUE {
				return None;
			}
			let mut info: FILE_BASIC_INFO = std::mem::zeroed();
			let ok = GetFileInformationByHandleEx(handle, FileBasicInfo, (&raw mut info).cast(), std::mem::size_of::<FILE_BASIC_INFO>() as u32);
			CloseHandle(handle);
			if ok == 0 || info.ChangeTime <= 0 {
				return None;
			}
			// Windows FILETIME: 100 ns ticks since 1601-01-01; Unix epoch is 116_444_736_000_000_000 ticks later.
			const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
			let ticks = (info.ChangeTime as u64).checked_sub(EPOCH_DIFF)?;
			Some(UNIX_EPOCH + Duration::new(ticks / 10_000_000, ((ticks % 10_000_000) * 100) as u32))
		}
	}

	/// The inode/metadata change time of `path` (`ctime` on Unix).
	#[cfg(unix)]
	pub fn change_time(_path: &Path, meta: &std::fs::Metadata) -> Option<SystemTime> {
		use std::os::unix::fs::MetadataExt;
		let secs = u64::try_from(meta.ctime()).ok()?;
		let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
		Some(UNIX_EPOCH + Duration::new(secs, nanos))
	}

	#[cfg(not(any(windows, unix)))]
	pub fn change_time(_path: &Path, _meta: &std::fs::Metadata) -> Option<SystemTime> {
		None
	}
}
