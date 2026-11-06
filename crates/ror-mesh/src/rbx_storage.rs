//! Roblox's local content cache (`%LOCALAPPDATA%\Roblox\rbx-storage`), for tools and tests that
//! read real assets.
//!
//! Cache entries are named by a 16-byte id (hex) and live either in `rbx-storage/<hex[..2]>/<hex>`
//! or inline in the `files` table of `rbx-storage.db`. Most entries are wrapped in an `RBXH`
//! record:
//!
//! | type | field |
//! |---|---|
//! | `[u8; 4]` | `RBXH` |
//! | `u32` | record version (2) |
//! | `u32` + bytes | request URL (empty for assetdelivery downloads) |
//! | `u8` | 0 |
//! | `u32` | HTTP status |
//! | `u32` | header block size |
//! | `u32` | header block hash (0 without headers) |
//! | `u32` | content size |
//! | `u32` | content hash (not CRC-32) |
//! | `u32` | 0, or 3 when headers are present |
//! | bytes | HTTP response headers (`name: value\r\n`...) |
//! | bytes | content |
//!
//! Entries without the wrapper (union physics `CSGPHS` blobs, category 22 in the database) start
//! with `u16`, `u32 size - 10`, `u32 size - 20` and a zstd frame; they never hold render meshes.

/// A parsed `RBXH` record.
#[derive(Clone, Copy, Debug)]
pub struct CacheEntry<'a> {
	pub url: &'a [u8],
	pub status: u32,
	pub headers: &'a [u8],
	pub content: &'a [u8],
}

/// Parses an `RBXH` cache record; `None` when `bytes` are not one (or are truncated).
pub fn parse_rbxh(bytes: &[u8]) -> Option<CacheEntry<'_>> {
	let mut reader = crate::reader::Reader::new(bytes);
	if reader.bytes(4, "magic").ok()? != b"RBXH" {
		return None;
	}
	let _version = reader.u32("version").ok()?;
	let url = reader.sized("url").ok()?;
	let _flag = reader.u8("flag").ok()?;
	let status = reader.u32("status").ok()?;
	let headers_size = reader.u32("headers size").ok()? as usize;
	let _headers_hash = reader.u32("headers hash").ok()?;
	let content_size = reader.u32("content size").ok()? as usize;
	let _content_hash = reader.u32("content hash").ok()?;
	let _extra = reader.u32("extra").ok()?;
	let headers = reader.bytes(headers_size, "headers").ok()?;
	let content = reader.bytes(content_size, "content").ok()?;
	Some(CacheEntry { url, status, headers, content })
}

/// The cache directory (`%LOCALAPPDATA%\Roblox\rbx-storage`), if it exists.
pub fn default_dir() -> Option<std::path::PathBuf> {
	let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("rbx-storage");
	dir.is_dir().then_some(dir)
}
