//! Identity of a local file's contents across caches and renders.
//!
//! One rule for every local input (models, maps, lending places, images, meshes, packs, skins and
//! skies):
//! - On a filesystem with a kernel-maintained, unspoofable change token (Windows NTFS/ReFS with an
//!   active USN journal: length, modified time, `ChangeTime`, `FILE_ID_INFO` and
//!   `FSCTL_READ_FILE_USN_DATA` USN; Unix: length, mtime, `ctime`, device and inode), a file first
//!   seen older than the recent window is keyed by that OS stamp without reading its bytes.
//! - While a file is within the 2 s recent window (closing same-tick / open-handle rewrites), its
//!   content is hashed with streaming BLAKE3 and keyed by `(len, hash)` alone; that content key is
//!   kept as the file ages past 2 s (and across identical-byte rewrites) so unchanged files are
//!   never reloaded when the 2 s window passes.
//! - On any filesystem without a trustworthy stamp (FAT/exFAT, SMB/network shares where USN queries
//!   are unsupported, volumes without an active USN journal, or `ChangeTime == 0`), the content
//!   hash always decides regardless of age or size.

use std::collections::HashMap;
use std::fmt::Write;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Files modified or metadata-changed within this window are content-hashed so same-tick or
/// still-open-handle rewrites cannot collide.
const RECENT_WINDOW: Duration = Duration::from_secs(2);
/// Largest file hashed solely for the recent-window guard on a trusted filesystem. Untrusted
/// filesystems have no size cap: content always decides there.
const RECENT_HASH_MAX_BYTES: u64 = 64 << 20;
/// Maximum recently hashed file identities retained across the recent window.
const FILE_HASHES_KEPT: usize = 16_384;

/// Recently hashed files on trusted filesystems, keyed by `(volume, file_id)`:
/// `(os_stamp, content_hash, settled)` where `settled` becomes true after one re-hash once the file
/// ages past [`RECENT_WINDOW`] (closing any rewrite made in the last timer tick it was read in).
type FileHashes = Mutex<HashMap<(u64, [u8; 16]), (OsStamp, [u8; 16], bool)>>;
static FILE_HASHES: OnceLock<FileHashes> = OnceLock::new();

/// Raw OS metadata observed for a local file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OsStamp {
	pub len: u64,
	pub modified_nanos: u128,
	pub change_nanos: u128,
	/// Volume serial number (Windows `FILE_ID_INFO.VolumeSerialNumber`, Unix `st_dev`).
	pub volume: u64,
	/// 128-bit file identifier (Windows `FILE_ID_INFO.FileId`, Unix `st_ino` zero-extended).
	pub file_id: [u8; 16],
	/// Kernel-maintained update sequence number (Windows `FSCTL_READ_FILE_USN_DATA` `Usn > 0`;
	/// `0` on Unix where kernel-only `ctime` cannot be backdated by unprivileged writers).
	pub usn: u64,
	/// Whether this OS stamp can be trusted without hashing content once outside [`RECENT_WINDOW`].
	pub trusted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StampKind {
	Content { len: u64, hash: [u8; 16] },
	Os(OsStamp),
}

/// Identity of a local file's contents, to detect edits, renames, time-preserving copies and
/// tools that restore Windows `ChangeTime`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
	kind: StampKind,
}

/// A [`FileStamp`] paired with the raw [`OsStamp`] and recent-window state observed when it was
/// taken, so a reader can prove after reading the file's bytes that the bytes match the stamp it
/// plans to cache them under.
#[derive(Clone, Copy, Debug)]
pub struct FileSnapshot {
	pub stamp: FileStamp,
	pub os: OsStamp,
	recent: bool,
}

#[cfg(test)]
thread_local! {
	static BEFORE_HASH_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn run_before_hash_hook(path: &Path) {
	BEFORE_HASH_HOOK.with_borrow_mut(|hook| {
		if let Some(f) = hook.as_mut() {
			f(path);
		}
	});
}

#[cfg(not(test))]
#[inline]
fn run_before_hash_hook(_path: &Path) {}

impl FileSnapshot {
	/// Proves that `bytes` read from `path` match the stamp this snapshot carries.
	///
	/// - For a content-hashed stamp, verifies the exact bytes read against `(len, hash)`.
	/// - For a trusted OS stamp outside the recent window, re-reads the OS stamp after the read and
	///   requires it to match `self.os` (including its monotonic USN on Windows or kernel `ctime`
	///   and inode on Unix) and `bytes.len()`.
	/// - Untrusted or still-recent OS stamps cannot prove that a file did not move from A to B and
	///   back to A during the read, so they return `false`.
	pub fn verify_read(&self, path: &Path, bytes: &[u8]) -> bool {
		match self.stamp.kind {
			StampKind::Content { len, hash } => bytes.len() as u64 == len && hash_bytes(bytes) == hash,
			StampKind::Os(os_before) => {
				if !os_before.trusted || self.recent {
					return false;
				}
				let Ok((os_after, _, _, _)) = read_os_stamp(path) else {
					return false;
				};
				os_after == os_before && os_after.len == bytes.len() as u64
			}
		}
	}

	/// Proves that `path` has not been modified or replaced since this snapshot was taken, for
	/// external readers whose raw bytes are not returned to us (e.g. a subprocess rendering a GUI).
	///
	/// - On Windows NTFS/ReFS (`trusted && usn > 0`), the kernel USN advances on every write and
	///   `file_id` / USN changes on replacement, so `os_after == self.os` proves no write happened.
	/// - On Unix (`trusted && usn == 0`), kernel `ctime` and `ino` prove no write once outside the
	///   recent window; within the recent window, same-tick A -> B -> A cannot be ruled out.
	/// - On untrusted filesystems (`!trusted`), no monotonic OS change token exists, so this
	///   conservatively returns `false`.
	pub fn still_unchanged(&self, path: &Path) -> bool {
		if !self.os.trusted {
			return false;
		}
		if self.os.usn == 0 && self.recent {
			return false;
		}
		let Ok((os_after, _, _, _)) = read_os_stamp(path) else {
			return false;
		};
		os_after == self.os
	}
}

impl FileStamp {
	/// Reads `path`'s metadata (and content hash when recent or untrusted) and builds its snapshot.
	pub fn snapshot(path: &Path) -> std::io::Result<FileSnapshot> {
		let (os, _is_file, modified_time, change_time) = read_os_stamp(path)?;
		let recent = is_os_recent(&os, modified_time, change_time);
		let stamp = Self::from_os_stamp(path, os, modified_time, change_time);
		Ok(FileSnapshot { stamp, os, recent })
	}

	/// Like [`FileStamp::snapshot`], returning `None` unless `path` is a regular file.
	pub fn snapshot_file(path: &Path) -> Option<FileSnapshot> {
		let (os, is_file, modified_time, change_time) = read_os_stamp(path).ok()?;
		if !is_file {
			return None;
		}
		let recent = is_os_recent(&os, modified_time, change_time);
		let stamp = Self::from_os_stamp(path, os, modified_time, change_time);
		Some(FileSnapshot { stamp, os, recent })
	}

	/// Content-hashed stamp of an in-memory byte slice, matching what [`FileStamp`] produces when
	/// hashing a file on disk.
	pub fn of_bytes(bytes: &[u8]) -> FileStamp {
		FileStamp { kind: StampKind::Content { len: bytes.len() as u64, hash: hash_bytes(bytes) } }
	}

	/// Reads `path`'s metadata (and content hash when recent or untrusted) and builds its stamp.
	pub fn try_of(path: &Path) -> std::io::Result<FileStamp> {
		Self::snapshot(path).map(|snapshot| snapshot.stamp)
	}

	/// Reads `path`'s metadata and builds its stamp, or `None` if it cannot be read.
	pub fn of(path: &Path) -> Option<FileStamp> {
		Self::try_of(path).ok()
	}

	/// Like [`FileStamp::of`], returning `None` unless `path` is a regular file.
	pub fn of_file(path: &Path) -> Option<FileStamp> {
		Self::snapshot_file(path).map(|snapshot| snapshot.stamp)
	}

	/// Reads only the OS-level metadata stamp for `path` (exposed for diagnostics and tests).
	pub fn os_stamp(path: &Path) -> std::io::Result<OsStamp> {
		read_os_stamp(path).map(|(os, _, _, _)| os)
	}

	/// Builds a stamp from explicit `(len, modified, change)` parts (synthetic/untrusted fallback
	/// used in unit tests when no real OS file ID / USN is supplied).
	pub fn from_parts(path: &Path, len: u64, modified_time: Option<SystemTime>, change_time: Option<SystemTime>) -> FileStamp {
		let os = OsStamp {
			len,
			modified_nanos: unix_nanos(modified_time),
			change_nanos: unix_nanos(change_time),
			volume: 0,
			file_id: [0; 16],
			usn: 0,
			trusted: false,
		};
		Self::from_os_stamp(path, os, modified_time, change_time)
	}

	/// Builds a stamp from an already-read [`OsStamp`] and its timestamps (exposed for deterministic
	/// untrusted-filesystem and aging tests).
	pub fn from_os_stamp(path: &Path, os: OsStamp, modified_time: Option<SystemTime>, change_time: Option<SystemTime>) -> FileStamp {
		if !os.trusted {
			run_before_hash_hook(path);
			return match hash_file(path) {
				Some((len, hash)) => FileStamp { kind: StampKind::Content { len, hash } },
				None => FileStamp { kind: StampKind::Os(os) },
			};
		}
		let recent = is_os_recent(&os, modified_time, change_time);
		let id_key = (os.volume, os.file_id);
		let hashes = FILE_HASHES.get_or_init(Default::default);
		if !recent {
			let cached = hashes.lock().get(&id_key).copied();
			match cached {
				None => return FileStamp { kind: StampKind::Os(os) },
				Some((kept_os, hash, true)) if kept_os == os => {
					return FileStamp { kind: StampKind::Content { len: os.len, hash } };
				}
				Some((kept_os, _, false)) if kept_os == os => {
					// First look after aging past the recent window: hash once more to close any
					// rewrite in the last timer tick or open handle, then settle only if the OS
					// stamp still matches after hashing (what is kept under `os` is what was read).
					run_before_hash_hook(path);
					if let Some((len, hash)) = hash_file(path) {
						if len == os.len && read_os_stamp(path).is_ok_and(|(after, _, _, _)| after == os) {
							let mut table = hashes.lock();
							table.insert(id_key, (os, hash, true));
						}
						return FileStamp { kind: StampKind::Content { len, hash } };
					}
					return FileStamp { kind: StampKind::Os(os) };
				}
				Some(_) => {
					// File changed on disk and is already outside the recent window: drop the old
					// entry and key by the new trusted OS stamp.
					hashes.lock().remove(&id_key);
					return FileStamp { kind: StampKind::Os(os) };
				}
			}
		}
		if os.len > RECENT_HASH_MAX_BYTES {
			return FileStamp { kind: StampKind::Os(os) };
		}
		run_before_hash_hook(path);
		match hash_file(path) {
			Some((len, hash)) => {
				if len == os.len && read_os_stamp(path).is_ok_and(|(after, _, _, _)| after == os) {
					let mut table = hashes.lock();
					if table.len() >= FILE_HASHES_KEPT {
						table.clear();
					}
					table.insert(id_key, (os, hash, false));
				}
				FileStamp { kind: StampKind::Content { len, hash } }
			}
			None => FileStamp { kind: StampKind::Os(os) },
		}
	}

	/// Byte length observed when this stamp was taken.
	pub fn len(&self) -> u64 {
		match self.kind {
			StampKind::Content { len, .. } => len,
			StampKind::Os(os) => os.len,
		}
	}

	/// Whether the file had zero length when this stamp was taken.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// Whether this stamp is keyed by a content hash rather than OS metadata alone.
	pub fn is_content_hashed(&self) -> bool {
		matches!(self.kind, StampKind::Content { .. })
	}

	/// Stable string encoding of this stamp (for string-keyed caches).
	pub fn key_suffix(&self) -> String {
		match self.kind {
			StampKind::Content { len, hash } => {
				let mut out = format!("c:{len}:");
				for byte in hash {
					let _ = write!(out, "{byte:02x}");
				}
				out
			}
			StampKind::Os(os) => {
				let mut out = format!("m:{}:{}:{}:{:x}:", os.len, os.modified_nanos, os.change_nanos, os.volume);
				for byte in os.file_id {
					let _ = write!(out, "{byte:02x}");
				}
				let _ = write!(out, ":{:x}", os.usn);
				out
			}
		}
	}

	/// Stamps a URI or path string with this file identity.
	pub fn stamp_uri(&self, uri: &str) -> String {
		format!("{uri}@{}", self.key_suffix())
	}

	/// The inode/metadata change time of `path` (NTFS/ReFS `FILE_BASIC_INFO.ChangeTime` on Windows,
	/// `ctime` on Unix).
	pub fn change_time(path: &Path, _meta: &std::fs::Metadata) -> Option<SystemTime> {
		read_os_stamp(path).ok().and_then(|(_, _, _, change_time)| change_time)
	}
}

fn is_os_recent(os: &OsStamp, modified_time: Option<SystemTime>, change_time: Option<SystemTime>) -> bool {
	if !os.trusted {
		return true;
	}
	let is_recent = |time: Option<SystemTime>| time.is_some_and(|time| time.elapsed().map_or(true, |age| age < RECENT_WINDOW));
	modified_time.is_none() || is_recent(modified_time) || is_recent(change_time)
}

fn unix_nanos(time: Option<SystemTime>) -> u128 {
	time.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |time| time.as_nanos())
}

/// First 128 bits of the BLAKE3 digest of `bytes`.
fn hash_bytes(bytes: &[u8]) -> [u8; 16] {
	let digest = blake3::hash(bytes);
	let mut out = [0u8; 16];
	out.copy_from_slice(&digest.as_bytes()[..16]);
	out
}

/// Streams `path` through BLAKE3 using a fixed 64 KiB buffer (no whole-file allocation) and returns
/// `(bytes_read, first 128 bits of the digest)`.
fn hash_file(path: &Path) -> Option<(u64, [u8; 16])> {
	let mut file = std::fs::File::open(path).ok()?;
	let mut hasher = blake3::Hasher::new();
	let mut buf = [0u8; 64 * 1024];
	let mut total = 0u64;
	loop {
		let read = file.read(&mut buf).ok()?;
		if read == 0 {
			break;
		}
		total += read as u64;
		hasher.update(&buf[..read]);
	}
	let digest = hasher.finalize();
	let mut out = [0u8; 16];
	out.copy_from_slice(&digest.as_bytes()[..16]);
	Some((total, out))
}

#[cfg(windows)]
fn filetime_to_system_time(ticks: i64) -> Option<SystemTime> {
	if ticks <= 0 {
		return None;
	}
	// Windows FILETIME: 100 ns ticks since 1601-01-01; Unix epoch is 116_444_736_000_000_000 ticks later.
	const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
	let unix_ticks = (ticks as u64).checked_sub(EPOCH_DIFF)?;
	Some(UNIX_EPOCH + Duration::new(unix_ticks / 10_000_000, ((unix_ticks % 10_000_000) * 100) as u32))
}

#[cfg(windows)]
fn parse_usn_record(bytes: &[u8]) -> Option<u64> {
	if bytes.len() < 8 {
		return None;
	}
	let major = u16::from_le_bytes([bytes[4], bytes[5]]);
	let usn = match major {
		// USN_RECORD_V2: Usn (i64) is at byte offset 24
		2 if bytes.len() >= 32 => i64::from_le_bytes(bytes[24..32].try_into().ok()?),
		// USN_RECORD_V3: Usn (i64) is at byte offset 40 (128-bit file reference numbers)
		3 if bytes.len() >= 48 => i64::from_le_bytes(bytes[40..48].try_into().ok()?),
		_ => return None,
	};
	(usn > 0).then_some(usn as u64)
}

#[cfg(windows)]
fn read_os_stamp(path: &Path) -> std::io::Result<(OsStamp, bool, Option<SystemTime>, Option<SystemTime>)> {
	use std::os::windows::ffi::OsStrExt;
	use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
	use windows_sys::Win32::Storage::FileSystem::{
		CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
		FILE_STANDARD_INFO, FileBasicInfo, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
	};
	use windows_sys::Win32::System::IO::DeviceIoControl;
	use windows_sys::Win32::System::Ioctl::{FSCTL_READ_FILE_USN_DATA, READ_FILE_USN_DATA};

	struct OwnedHandle(HANDLE);
	impl Drop for OwnedHandle {
		fn drop(&mut self) {
			// SAFETY: `self.0` is a valid open handle returned by `CreateFileW`.
			unsafe {
				CloseHandle(self.0);
			}
		}
	}

	let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
	// SAFETY: `wide` is null-terminated UTF-16; all output buffers are properly aligned and sized.
	unsafe {
		let raw = CreateFileW(
			wide.as_ptr(),
			FILE_READ_ATTRIBUTES,
			FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
			std::ptr::null(),
			OPEN_EXISTING,
			FILE_FLAG_BACKUP_SEMANTICS,
			std::ptr::null_mut(),
		);
		if raw == INVALID_HANDLE_VALUE {
			return Err(std::io::Error::last_os_error());
		}
		let handle = OwnedHandle(raw);

		let mut basic: FILE_BASIC_INFO = std::mem::zeroed();
		if GetFileInformationByHandleEx(handle.0, FileBasicInfo, (&raw mut basic).cast(), std::mem::size_of::<FILE_BASIC_INFO>() as u32) == 0 {
			return Err(std::io::Error::last_os_error());
		}
		let mut standard: FILE_STANDARD_INFO = std::mem::zeroed();
		if GetFileInformationByHandleEx(handle.0, FileStandardInfo, (&raw mut standard).cast(), std::mem::size_of::<FILE_STANDARD_INFO>() as u32) == 0 {
			return Err(std::io::Error::last_os_error());
		}
		let mut id_info: FILE_ID_INFO = std::mem::zeroed();
		let has_file_id = GetFileInformationByHandleEx(handle.0, FileIdInfo, (&raw mut id_info).cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) != 0;

		let input = READ_FILE_USN_DATA { MinMajorVersion: 2, MaxMajorVersion: 3 };
		let mut usn_words = [0u64; 128]; // 1024 bytes, 8-byte aligned for USN_RECORD_V2 / V3
		let mut returned: u32 = 0;
		let usn = if DeviceIoControl(
			handle.0,
			FSCTL_READ_FILE_USN_DATA,
			(&raw const input).cast(),
			std::mem::size_of::<READ_FILE_USN_DATA>() as u32,
			usn_words.as_mut_ptr().cast(),
			std::mem::size_of_val(&usn_words) as u32,
			&raw mut returned,
			std::ptr::null_mut(),
		) != 0
		{
			let bytes = std::slice::from_raw_parts(usn_words.as_ptr().cast::<u8>(), (returned as usize).min(std::mem::size_of_val(&usn_words)));
			parse_usn_record(bytes)
		} else {
			None
		};

		let modified_time = filetime_to_system_time(basic.LastWriteTime);
		let change_time = filetime_to_system_time(basic.ChangeTime);
		let len = u64::try_from(standard.EndOfFile).unwrap_or(0);
		let is_file = !standard.Directory && (basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0;
		let (volume, file_id) = if has_file_id { (id_info.VolumeSerialNumber, id_info.FileId.Identifier) } else { (0, [0; 16]) };
		let trusted = has_file_id && usn.is_some() && modified_time.is_some() && change_time.is_some();
		let os = OsStamp {
			len,
			modified_nanos: unix_nanos(modified_time),
			change_nanos: unix_nanos(change_time),
			volume,
			file_id,
			usn: usn.unwrap_or(0),
			trusted,
		};
		Ok((os, is_file, modified_time, change_time))
	}
}

#[cfg(unix)]
fn read_os_stamp(path: &Path) -> std::io::Result<(OsStamp, bool, Option<SystemTime>, Option<SystemTime>)> {
	use std::os::unix::fs::MetadataExt;
	let meta = std::fs::metadata(path)?;
	let modified_time = meta.modified().ok();
	let change_time = (meta.ctime() > 0)
		.then(|| {
			let secs = u64::try_from(meta.ctime()).ok()?;
			let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
			Some(UNIX_EPOCH + Duration::new(secs, nanos))
		})
		.flatten();
	let mut file_id = [0u8; 16];
	file_id[..8].copy_from_slice(&meta.ino().to_le_bytes());
	let trusted = meta.ino() != 0 && modified_time.is_some() && change_time.is_some();
	let os = OsStamp {
		len: meta.len(),
		modified_nanos: unix_nanos(modified_time),
		change_nanos: unix_nanos(change_time),
		volume: meta.dev(),
		file_id,
		usn: 0,
		trusted,
	};
	Ok((os, meta.is_file(), modified_time, change_time))
}

#[cfg(not(any(windows, unix)))]
fn read_os_stamp(path: &Path) -> std::io::Result<(OsStamp, bool, Option<SystemTime>, Option<SystemTime>)> {
	let meta = std::fs::metadata(path)?;
	let modified_time = meta.modified().ok();
	let os = OsStamp {
		len: meta.len(),
		modified_nanos: unix_nanos(modified_time),
		change_nanos: 0,
		volume: 0,
		file_id: [0; 16],
		usn: 0,
		trusted: false,
	};
	Ok((os, meta.is_file(), modified_time, None))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// On Windows NTFS/ReFS with an active USN journal, even a tool that overwrites a file at the
	/// same byte length and restores ALL FOUR `FILE_BASIC_INFO` timestamps (`CreationTime`,
	/// `LastAccessTime`, `LastWriteTime`, `ChangeTime`) advances the kernel USN, so `FileStamp`
	/// still invalidates after aging past the 2 s recent window.
	#[cfg(windows)]
	#[test]
	fn restoring_all_windows_timestamps_including_change_time_still_invalidates_stamp() {
		use std::os::windows::ffi::OsStrExt;
		use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
		use windows_sys::Win32::Storage::FileSystem::{
			CreateFileW, FILE_BASIC_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileBasicInfo, GetFileInformationByHandleEx,
			OPEN_EXISTING, SetFileInformationByHandle,
		};

		let dir = std::env::temp_dir().join(format!("ror-stamp-all-times-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("payload.bin");
		let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

		let get_basic = || unsafe {
			let h = CreateFileW(wide.as_ptr(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
			assert_ne!(h, INVALID_HANDLE_VALUE);
			let mut info: FILE_BASIC_INFO = std::mem::zeroed();
			assert_ne!(GetFileInformationByHandleEx(h, FileBasicInfo, (&raw mut info).cast(), std::mem::size_of::<FILE_BASIC_INFO>() as u32), 0);
			CloseHandle(h);
			info
		};
		let set_basic = |info: &FILE_BASIC_INFO| unsafe {
			let h = CreateFileW(wide.as_ptr(), FILE_WRITE_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
			assert_ne!(h, INVALID_HANDLE_VALUE);
			assert_ne!(SetFileInformationByHandle(h, FileBasicInfo, (info as *const FILE_BASIC_INFO).cast(), std::mem::size_of::<FILE_BASIC_INFO>() as u32), 0);
			CloseHandle(h);
		};

		std::fs::write(&path, b"version-alpha-01").unwrap();
		let old = SystemTime::now() - Duration::from_secs(3600);
		std::fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
		let saved = get_basic();
		std::thread::sleep(Duration::from_millis(2200));

		let os_before = FileStamp::os_stamp(&path).unwrap();
		let stamp_before = FileStamp::of(&path).unwrap();

		// Overwrite with different bytes of the same length and restore ALL four timestamps,
		// including ChangeTime, so FILE_BASIC_INFO is bit-identical to before the write.
		std::fs::write(&path, b"version-bravo-02").unwrap();
		set_basic(&saved);
		let after_basic = get_basic();
		assert_eq!(
			(after_basic.CreationTime, after_basic.LastAccessTime, after_basic.LastWriteTime, after_basic.ChangeTime),
			(saved.CreationTime, saved.LastAccessTime, saved.LastWriteTime, saved.ChangeTime),
			"test setup must restore all four FILE_BASIC_INFO timestamps"
		);
		std::thread::sleep(Duration::from_millis(2200));

		let os_after = FileStamp::os_stamp(&path).unwrap();
		let stamp_after = FileStamp::of(&path).unwrap();
		if os_before.trusted && os_after.trusted {
			assert_ne!(os_before.usn, os_after.usn, "USN must advance on overwrite even when all 4 FILE_BASIC_INFO timestamps are restored");
		}
		assert_ne!(stamp_before, stamp_after, "FileStamp must invalidate even when all 4 Windows timestamps are restored");
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// On an untrusted filesystem (FAT/exFAT, SMB/network share, volume without USN journal),
	/// content hashing decides even when every OS metadata field matches and the file is old.
	#[test]
	fn untrusted_filesystem_falls_back_to_content_hash() {
		let dir = std::env::temp_dir().join(format!("ror-stamp-untrusted-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("network.bin");
		let old = SystemTime::now() - Duration::from_secs(3600);
		let fixed_os = OsStamp {
			len: 16,
			modified_nanos: unix_nanos(Some(old)),
			change_nanos: 0,
			volume: 7,
			file_id: [1; 16],
			usn: 0,
			trusted: false,
		};
		std::fs::write(&path, b"version-alpha-01").unwrap();
		let a = FileStamp::from_os_stamp(&path, fixed_os, Some(old), None);
		assert!(a.is_content_hashed(), "untrusted filesystem must hash content");
		std::fs::write(&path, b"version-bravo-02").unwrap();
		let b = FileStamp::from_os_stamp(&path, fixed_os, Some(old), None);
		assert!(b.is_content_hashed());
		assert_ne!(a, b, "untrusted filesystem must detect same-size same-metadata overwrite via content hash");
		std::fs::write(&path, b"version-alpha-01").unwrap();
		let a2 = FileStamp::from_os_stamp(&path, fixed_os, Some(old), None);
		assert_eq!(a, a2, "identical content on untrusted filesystem must produce the same stamp");
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// A file hashed while recent keeps the exact same content stamp as it ages past 2 s and even
	/// when rewritten with identical bytes, while a real byte change still invalidates immediately.
	#[test]
	fn fresh_file_keeps_content_stamp_past_recent_window_and_identical_rewrite() {
		let dir = std::env::temp_dir().join(format!("ror-stamp-stable-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("tile.png");
		std::fs::write(&path, b"identical-payload-01").unwrap();
		let fresh = FileStamp::of(&path).unwrap();
		assert!(fresh.is_content_hashed());
		std::thread::sleep(Duration::from_millis(2200));
		let aged_first = FileStamp::of(&path).unwrap();
		let aged_second = FileStamp::of(&path).unwrap();
		assert_eq!(aged_first, fresh, "aging past 2 s must not change stamp");
		assert_eq!(aged_second, fresh, "settled stamp must stay equal");
		// Identical-byte rewrite (e.g. a rebuild that changes nothing) keeps the exact same stamp.
		std::fs::write(&path, b"identical-payload-01").unwrap();
		let rewritten_same = FileStamp::of(&path).unwrap();
		assert_eq!(rewritten_same, fresh, "identical-byte rewrite must keep the same content stamp");
		// A real content change of the same size invalidates immediately.
		std::fs::write(&path, b"different-payload-02").unwrap();
		let changed = FileStamp::of(&path).unwrap();
		assert_ne!(changed, fresh, "real content change must invalidate");
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// Deterministic test (no sleeps): if a file changes between `read_os_stamp` and `hash_file`,
	/// `FILE_HASHES` must NOT record `(os_A, hash_B)`, so a subsequent look after the file returns
	/// to `A` (or stays at `os_A`) never inherits `hash_B`.
	#[test]
	fn file_hashes_not_poisoned_when_file_moves_between_os_stamp_and_hash() {
		let dir = std::env::temp_dir().join(format!("ror-stamp-race-hash-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("moving.bin");
		let bytes_a = b"payload-alpha-01";
		let bytes_b = b"payload-bravo-02";
		std::fs::write(&path, bytes_a).unwrap();
		let (os_a, _, mod_a, chg_a) = read_os_stamp(&path).unwrap();
		assert!(os_a.trusted, "temp dir should be on a trusted filesystem for this test");

		let moved_path = path.clone();
		BEFORE_HASH_HOOK.set(Some(Box::new(move |p| {
			if p == moved_path {
				std::fs::write(p, bytes_b).unwrap();
			}
		})));
		let stamp_during = FileStamp::from_os_stamp(&path, os_a, mod_a, chg_a);
		BEFORE_HASH_HOOK.set(None);
		// The stamp returned matches the bytes actually hashed (B), and `FILE_HASHES` was NOT
		// poisoned with `(os_a, hash_b)` because `read_os_stamp` after hashing saw `os_b != os_a`.
		assert_eq!(stamp_during, FileStamp::of_bytes(bytes_b));
		let cached = FILE_HASHES.get_or_init(Default::default).lock().get(&(os_a.volume, os_a.file_id)).copied();
		assert_ne!(cached.map(|(os, _, _)| os), Some(os_a), "FILE_HASHES must not cache a hash read after OS stamp moved");
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// Deterministic test (no sleeps): `FileSnapshot::verify_read` rejects B read under snapshot A,
	/// even if the file was rewritten back to A before `verify_read` runs.
	#[test]
	fn verify_read_rejects_a_to_b_to_a_content_and_os() {
		let dir = std::env::temp_dir().join(format!("ror-stamp-verify-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("flip.bin");
		let bytes_a = b"alpha-contents-1";
		let bytes_b = b"bravo-contents-2";

		// 1. Content-stamped (fresh) file: A -> B -> A while reading B.
		std::fs::write(&path, bytes_a).unwrap();
		let snap_content = FileStamp::snapshot(&path).unwrap();
		assert!(snap_content.stamp.is_content_hashed());
		assert!(snap_content.verify_read(&path, bytes_a), "matching bytes verify");
		std::fs::write(&path, bytes_b).unwrap();
		let read_b = std::fs::read(&path).unwrap();
		std::fs::write(&path, bytes_a).unwrap();
		assert!(!snap_content.verify_read(&path, &read_b), "content snapshot must reject B even after file returns to A");

		// 2. OS-stamped (aged, trusted) file: construct an aged OS snapshot directly (zero sleeps).
		let (os_a, _, _, _) = read_os_stamp(&path).unwrap();
		if os_a.trusted {
			let snap_os = FileSnapshot { stamp: FileStamp { kind: StampKind::Os(os_a) }, os: os_a, recent: false };
			assert!(snap_os.verify_read(&path, bytes_a), "unchanged trusted OS stamp verifies");
			assert!(snap_os.still_unchanged(&path), "unchanged trusted OS stamp reports still_unchanged");
			// File moves A -> B -> A: monotonic USN (Windows) / ctime (Unix) cannot return to os_a.
			std::fs::write(&path, bytes_b).unwrap();
			let read_b = std::fs::read(&path).unwrap();
			std::fs::write(&path, bytes_a).unwrap();
			assert!(!snap_os.verify_read(&path, &read_b), "OS snapshot must reject B even after file returns to A");
			assert!(!snap_os.still_unchanged(&path), "still_unchanged must reject A -> B -> A");
		}
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// Guards the `if kept_os == os` check on settled `FILE_HASHES` entries: once a file is settled
	/// under `os_a`, a subsequent look with a different OS stamp `os_b` (same volume/file_id, new
	/// USN/change_nanos, outside the recent window) must drop the stale settled entry instead of
	/// returning `os_a`'s content hash.
	#[test]
	fn settled_entry_is_rejected_when_os_stamp_changes() {
		let dir = std::env::temp_dir().join(format!("ror-stamp-settled-guard-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("settled.bin");
		let bytes_a = b"settled-alpha-01";
		let bytes_b = b"settled-bravo-02";
		std::fs::write(&path, bytes_a).unwrap();
		let (os_a, _, mod_a, chg_a) = read_os_stamp(&path).unwrap();
		assert!(os_a.trusted, "temp dir should be on a trusted filesystem");
		// 1. Fresh look inserts `(os_a, hash_a, settled = false)`.
		let fresh = FileStamp::from_os_stamp(&path, os_a, mod_a, chg_a);
		assert_eq!(fresh, FileStamp::of_bytes(bytes_a));
		// 2. First aged look settles `(os_a, hash_a, settled = true)`.
		let old = SystemTime::now() - Duration::from_secs(3600);
		let settled = FileStamp::from_os_stamp(&path, os_a, Some(old), Some(old));
		assert_eq!(settled, FileStamp::of_bytes(bytes_a));
		// 3. File changes on disk (new USN / change_nanos, same `(volume, file_id)`), viewed aged.
		std::fs::write(&path, bytes_b).unwrap();
		let (os_b, _, _, _) = read_os_stamp(&path).unwrap();
		assert_eq!((os_b.volume, os_b.file_id), (os_a.volume, os_a.file_id));
		assert_ne!(os_b, os_a, "rewriting file must advance OS stamp");
		let after = FileStamp::from_os_stamp(&path, os_b, Some(old), Some(old));
		assert_ne!(after, settled, "changed OS stamp must not reuse settled hash of earlier bytes");
		assert!(!after.is_content_hashed(), "aged changed file should drop stale settled entry and return Os(os_b)");
		let _ = std::fs::remove_dir_all(&dir);
	}
}
