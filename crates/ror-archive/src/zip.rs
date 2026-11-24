//! The ZIP container of an archive: every member stored (method 0), no encryption, ZIP64 allowed.
//! Stored members are slices of the file, so reading one copies nothing and an archive cannot
//! expand past its own size.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::{Error, Limits, Result};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;
const EXTRA_ZIP64: u16 = 0x0001;
/// Member paths longer than this are refused, as the format requires.
const MAX_NAME: usize = 128;
const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_MASKED: u16 = 1 << 13;

/// A stored member: its name and where its bytes sit in the archive.
#[derive(Debug)]
pub(crate) struct Entry {
	pub(crate) name: String,
	pub(crate) data: Range<usize>,
}

struct Cursor<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl<'a> Cursor<'a> {
	fn new(bytes: &'a [u8], at: usize) -> Self {
		Self { bytes, at }
	}

	fn take(&mut self, len: usize) -> Result<&'a [u8]> {
		let end = self.at.checked_add(len).filter(|end| *end <= self.bytes.len()).ok_or_else(truncated)?;
		let slice = &self.bytes[self.at..end];
		self.at = end;
		Ok(slice)
	}

	fn u16(&mut self) -> Result<u16> {
		Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("two bytes")))
	}

	fn u32(&mut self) -> Result<u32> {
		Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
	}

	fn u64(&mut self) -> Result<u64> {
		Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
	}
}

fn truncated() -> Error {
	Error::Container("the ZIP directory runs past the end of the file".into())
}

fn invalid(message: impl Into<String>) -> Error {
	Error::Container(message.into())
}

fn to_usize(value: u64) -> Result<usize> {
	usize::try_from(value).map_err(|_| truncated())
}

/// Where the central directory is and how many members it lists.
struct Directory {
	offset: usize,
	size: usize,
	entries: u64,
}

fn directory(bytes: &[u8]) -> Result<Directory> {
	// the end record is the last thing in the file but for its comment (at most 65,535 bytes)
	let fixed = 22;
	if bytes.len() < fixed {
		return Err(invalid("the file is too short to be a ZIP"));
	}
	let lowest = bytes.len().saturating_sub(fixed + u16::MAX as usize);
	let end = (lowest..=bytes.len() - fixed)
		.rev()
		.find(|&at| {
			bytes[at..at + 4] == END.to_le_bytes()
				&& usize::from(u16::from_le_bytes([bytes[at + 20], bytes[at + 21]])) == bytes.len() - at - fixed
		})
		.ok_or_else(|| invalid("the ZIP has no end-of-central-directory record"))?;
	let mut record = Cursor::new(bytes, end + 4);
	let disk = record.u16()?;
	let directory_disk = record.u16()?;
	let entries_here = record.u16()?;
	let entries = record.u16()?;
	let size = record.u32()?;
	let offset = record.u32()?;
	// a ZIP64 locator sits right before the end record and points at the 64-bit one
	let locator = end.checked_sub(20).filter(|&at| bytes[at..at + 4] == LOCATOR64.to_le_bytes());
	let directory = if let Some(locator) = locator {
		let mut locator = Cursor::new(bytes, locator + 4);
		let locator_disk = locator.u32()?;
		let end64 = to_usize(locator.u64()?)?;
		let disks = locator.u32()?;
		let mut record = Cursor::new(bytes, end64);
		if record.u32()? != END64 || locator_disk != 0 || disks > 1 {
			return Err(invalid("the ZIP64 end-of-central-directory record is missing or spans disks"));
		}
		record.take(8 + 2 + 2)?; // record size, version made by, version needed
		let disk = record.u32()?;
		let directory_disk = record.u32()?;
		let entries_here = record.u64()?;
		let entries = record.u64()?;
		let size = to_usize(record.u64()?)?;
		let offset = to_usize(record.u64()?)?;
		if disk != 0 || directory_disk != 0 || entries_here != entries {
			return Err(invalid("the ZIP spans several disks"));
		}
		if offset.checked_add(size).is_none_or(|directory_end| directory_end > end64) {
			return Err(truncated());
		}
		Directory { offset, size, entries }
	} else {
		if disk != 0 || directory_disk != 0 || entries_here != entries {
			return Err(invalid("the ZIP spans several disks"));
		}
		if entries == u16::MAX || size == u32::MAX || offset == u32::MAX {
			return Err(invalid("the ZIP needs ZIP64 records it does not have"));
		}
		let (offset, size) = (offset as usize, size as usize);
		if offset.checked_add(size).is_none_or(|directory_end| directory_end > end) {
			return Err(truncated());
		}
		Directory { offset, size, entries: u64::from(entries) }
	};
	Ok(directory)
}

fn valid_name(name: &str) -> bool {
	name.chars().count() <= MAX_NAME
		&& !name.contains(['\\', '\0'])
		&& !name.starts_with('/')
		&& name.trim_end_matches('/').split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Lists a ZIP's members and checks each one's CRC-32. Refuses anything an archive may not hold:
/// compressed or encrypted members, repeated names, names that leave the archive, members that
/// overlap or run into the directory.
pub(crate) fn read(bytes: &[u8], limits: &Limits) -> Result<Vec<Entry>> {
	let directory = directory(bytes)?;
	if directory.entries > limits.max_entries as u64 {
		return Err(Error::Limit("ZIP members"));
	}
	let mut entries = Vec::with_capacity(directory.entries as usize);
	let mut names = BTreeSet::new();
	let mut ranges = Vec::with_capacity(directory.entries as usize);
	let mut central = Cursor::new(&bytes[..directory.offset + directory.size], directory.offset);
	for _ in 0..directory.entries {
		if central.u32()? != CENTRAL {
			return Err(invalid("the ZIP directory lists fewer members than it declares"));
		}
		central.take(2 + 2)?; // version made by, version needed
		let flags = central.u16()?;
		let method = central.u16()?;
		central.take(2 + 2)?; // modified time and date
		let crc = central.u32()?;
		let mut stored = u64::from(central.u32()?);
		let mut size = u64::from(central.u32()?);
		let name_len = usize::from(central.u16()?);
		let extra_len = usize::from(central.u16()?);
		let comment_len = usize::from(central.u16()?);
		let disk = central.u16()?;
		central.take(2 + 4)?; // internal and external attributes
		let mut offset = u64::from(central.u32()?);
		let name = std::str::from_utf8(central.take(name_len)?)
			.map_err(|_| invalid("a member's name is not UTF-8"))?
			.to_owned();
		let mut extra = Cursor::new(central.take(extra_len)?, 0);
		central.take(comment_len)?;
		while extra.bytes.len() - extra.at >= 4 {
			let id = extra.u16()?;
			let len = usize::from(extra.u16()?);
			let mut field = Cursor::new(extra.take(len)?, 0);
			if id != EXTRA_ZIP64 {
				continue;
			}
			// the 64-bit values are present only for the 32-bit fields that are saturated, in this order
			if size == u64::from(u32::MAX) {
				size = field.u64()?;
			}
			if stored == u64::from(u32::MAX) {
				stored = field.u64()?;
			}
			if offset == u64::from(u32::MAX) {
				offset = field.u64()?;
			}
		}
		if !valid_name(&name) {
			return Err(invalid(format!("a member's name is not allowed in an archive: {name:?}")));
		}
		if disk != 0 && disk != u16::MAX {
			return Err(invalid("the ZIP spans several disks"));
		}
		if flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION | FLAG_MASKED) != 0 {
			return Err(invalid(format!("member {name} is encrypted")));
		}
		if method != 0 || stored != size {
			return Err(invalid(format!(
				"member {name} is compressed (method {method}): archive members are stored uncompressed"
			)));
		}
		if !names.insert(name.clone()) {
			return Err(invalid(format!("member {name} appears twice")));
		}
		// the local header repeats the name; its own extra field may differ in length from the directory's
		let mut local = Cursor::new(&bytes[..directory.offset], to_usize(offset)?);
		if local.u32()? != LOCAL {
			return Err(invalid(format!("member {name} has no local header where the directory points")));
		}
		local.take(2 + 2 + 2 + 2 + 2 + 4 + 4 + 4)?;
		let local_name_len = usize::from(local.u16()?);
		let local_extra_len = usize::from(local.u16()?);
		if local.take(local_name_len)? != name.as_bytes() {
			return Err(invalid(format!("member {name} is named differently in its local header")));
		}
		local.take(local_extra_len)?;
		let start = local.at;
		let data = start..start.checked_add(to_usize(size)?).filter(|end| *end <= directory.offset).ok_or_else(truncated)?;
		if crc32fast::hash(&bytes[data.clone()]) != crc {
			return Err(invalid(format!("member {name} fails its CRC-32 check")));
		}
		ranges.push(to_usize(offset)?..data.end);
		entries.push(Entry { name, data });
	}
	ranges.sort_by_key(|range| range.start);
	if ranges.windows(2).any(|pair| pair[1].start < pair[0].end) {
		return Err(invalid("two members overlap"));
	}
	Ok(entries)
}

/// Writes a ZIP of stored members, in the order given, with fixed timestamps so the same members
/// always make the same bytes. ZIP64 records are written when a size, an offset or the member count
/// needs them, or when `force_zip64` asks (for tests).
pub(crate) fn write(members: &[(String, &[u8])], force_zip64: bool) -> Vec<u8> {
	// 1980-01-01 00:00:00, the earliest time the format can express
	const DOS_TIME: u16 = 0;
	const DOS_DATE: u16 = 0x0021;
	let total: usize = members.iter().map(|(name, data)| 30 + name.len() + data.len() + 20).sum();
	let mut out = Vec::with_capacity(total + members.len() * 80 + 128);
	let mut directory = Vec::new();
	for (name, data) in members {
		let offset = out.len() as u64;
		let size = data.len() as u64;
		let wide = force_zip64 || size >= u64::from(u32::MAX);
		let wide_offset = force_zip64 || offset >= u64::from(u32::MAX);
		let crc = crc32fast::hash(data);
		let version: u16 = if wide || wide_offset { 45 } else { 10 };
		let size32 = if wide { u32::MAX } else { size as u32 };
		out.extend_from_slice(&LOCAL.to_le_bytes());
		out.extend_from_slice(&version.to_le_bytes());
		out.extend_from_slice(&0u16.to_le_bytes()); // flags
		out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
		out.extend_from_slice(&DOS_TIME.to_le_bytes());
		out.extend_from_slice(&DOS_DATE.to_le_bytes());
		out.extend_from_slice(&crc.to_le_bytes());
		out.extend_from_slice(&size32.to_le_bytes());
		out.extend_from_slice(&size32.to_le_bytes());
		out.extend_from_slice(&(name.len() as u16).to_le_bytes());
		out.extend_from_slice(&(if wide { 20u16 } else { 0 }).to_le_bytes());
		out.extend_from_slice(name.as_bytes());
		if wide {
			out.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
			out.extend_from_slice(&16u16.to_le_bytes());
			out.extend_from_slice(&size.to_le_bytes());
			out.extend_from_slice(&size.to_le_bytes());
		}
		out.extend_from_slice(data);

		let mut extra = Vec::new();
		if wide {
			extra.extend_from_slice(&size.to_le_bytes());
			extra.extend_from_slice(&size.to_le_bytes());
		}
		if wide_offset {
			extra.extend_from_slice(&offset.to_le_bytes());
		}
		directory.extend_from_slice(&CENTRAL.to_le_bytes());
		directory.extend_from_slice(&version.to_le_bytes()); // version made by
		directory.extend_from_slice(&version.to_le_bytes()); // version needed
		directory.extend_from_slice(&0u16.to_le_bytes());
		directory.extend_from_slice(&0u16.to_le_bytes());
		directory.extend_from_slice(&DOS_TIME.to_le_bytes());
		directory.extend_from_slice(&DOS_DATE.to_le_bytes());
		directory.extend_from_slice(&crc.to_le_bytes());
		directory.extend_from_slice(&size32.to_le_bytes());
		directory.extend_from_slice(&size32.to_le_bytes());
		directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
		directory.extend_from_slice(&(if extra.is_empty() { 0 } else { extra.len() as u16 + 4 }).to_le_bytes());
		directory.extend_from_slice(&0u16.to_le_bytes()); // comment
		directory.extend_from_slice(&0u16.to_le_bytes()); // disk
		directory.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
		directory.extend_from_slice(&0u32.to_le_bytes()); // external attributes
		directory.extend_from_slice(&(if wide_offset { u32::MAX } else { offset as u32 }).to_le_bytes());
		directory.extend_from_slice(name.as_bytes());
		if !extra.is_empty() {
			directory.extend_from_slice(&EXTRA_ZIP64.to_le_bytes());
			directory.extend_from_slice(&(extra.len() as u16).to_le_bytes());
			directory.extend_from_slice(&extra);
		}
	}
	let offset = out.len() as u64;
	let size = directory.len() as u64;
	let count = members.len() as u64;
	out.extend_from_slice(&directory);
	let zip64 = force_zip64 || count >= u64::from(u16::MAX) || size >= u64::from(u32::MAX) || offset >= u64::from(u32::MAX);
	if zip64 {
		let end64 = out.len() as u64;
		out.extend_from_slice(&END64.to_le_bytes());
		out.extend_from_slice(&44u64.to_le_bytes()); // the record's size after this field
		out.extend_from_slice(&45u16.to_le_bytes());
		out.extend_from_slice(&45u16.to_le_bytes());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(&count.to_le_bytes());
		out.extend_from_slice(&count.to_le_bytes());
		out.extend_from_slice(&size.to_le_bytes());
		out.extend_from_slice(&offset.to_le_bytes());
		out.extend_from_slice(&LOCATOR64.to_le_bytes());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(&end64.to_le_bytes());
		out.extend_from_slice(&1u32.to_le_bytes());
	}
	out.extend_from_slice(&END.to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	out.extend_from_slice(&(if zip64 { u16::MAX } else { count as u16 }).to_le_bytes());
	out.extend_from_slice(&(if zip64 { u16::MAX } else { count as u16 }).to_le_bytes());
	out.extend_from_slice(&(if zip64 { u32::MAX } else { size as u32 }).to_le_bytes());
	out.extend_from_slice(&(if zip64 { u32::MAX } else { offset as u32 }).to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn members<'a>(list: &[(&str, &'a [u8])]) -> Vec<(String, &'a [u8])> {
		list.iter().map(|(name, data)| ((*name).to_owned(), *data)).collect()
	}

	fn names(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
		read(bytes, &Limits::default())
			.map(|entries| entries.into_iter().map(|entry| (entry.name, bytes[entry.data].to_vec())).collect())
	}

	#[test]
	fn what_is_written_reads_back() {
		let list = members(&[("manifest.jsonl", b"{}\n"), ("objects/aa", b"first"), ("objects/bb", b"")]);
		for force_zip64 in [false, true] {
			let bytes = write(&list, force_zip64);
			assert!(bytes.starts_with(b"PK\x03\x04"));
			let read = names(&bytes).unwrap();
			assert_eq!(read.len(), 3);
			assert_eq!(read[0], ("manifest.jsonl".to_owned(), b"{}\n".to_vec()));
			assert_eq!(read[1], ("objects/aa".to_owned(), b"first".to_vec()));
			assert_eq!(read[2].1, b"");
		}
	}

	#[test]
	fn the_same_members_make_the_same_bytes() {
		let list = members(&[("a", b"one"), ("b", b"two")]);
		assert_eq!(write(&list, false), write(&list, false));
	}

	#[test]
	fn an_empty_zip_lists_nothing() {
		let bytes = write(&[], false);
		assert!(bytes.starts_with(b"PK\x05\x06"));
		assert!(names(&bytes).unwrap().is_empty());
	}

	#[test]
	fn a_trailing_comment_is_allowed() {
		let mut bytes = write(&members(&[("a", b"one")]), false);
		let comment = b"made by a general ZIP tool";
		let at = bytes.len() - 2;
		bytes[at..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
		bytes.extend_from_slice(comment);
		assert_eq!(names(&bytes).unwrap().len(), 1);
	}

	#[test]
	fn a_changed_byte_fails_the_crc() {
		let mut bytes = write(&members(&[("a", b"payload")]), false);
		let at = bytes.windows(7).position(|window| window == b"payload").unwrap();
		bytes[at] ^= 1;
		let error = names(&bytes).unwrap_err();
		assert!(error.to_string().contains("CRC-32"), "{error}");
	}

	#[test]
	fn compressed_and_encrypted_members_are_refused() {
		let clean = write(&members(&[("a", b"payload")]), false);
		let central = clean.windows(4).rposition(|window| window == CENTRAL.to_le_bytes()).unwrap();
		let mut deflated = clean.clone();
		deflated[central + 10] = 8;
		assert!(names(&deflated).unwrap_err().to_string().contains("compressed"));
		let mut encrypted = clean.clone();
		encrypted[central + 8] |= 1;
		assert!(names(&encrypted).unwrap_err().to_string().contains("encrypted"));
	}

	#[test]
	fn names_that_leave_the_archive_or_repeat_are_refused() {
		for name in ["../a", "a/../b", "/a", "a\\b", "a//b", "./a"] {
			let bytes = write(&members(&[(name, b"x")]), false);
			assert!(names(&bytes).is_err(), "{name}");
		}
		let long = "a".repeat(129);
		assert!(names(&write(&members(&[(&long, b"x")]), false)).is_err());
		assert!(names(&write(&members(&[(&long[..128], b"x")]), false)).is_ok());
		assert!(names(&write(&members(&[("a", b"x"), ("a", b"y")]), false)).unwrap_err().to_string().contains("twice"));
	}

	#[test]
	fn a_cut_file_is_refused_not_read_past() {
		let bytes = write(&members(&[("a", b"payload"), ("b", b"more")]), false);
		for len in 0..bytes.len() {
			assert!(names(&bytes[..len]).is_err(), "cut at {len}");
		}
	}

	#[test]
	fn the_member_limit_holds() {
		let bytes = write(&members(&[("a", b"1"), ("b", b"2")]), false);
		let limits = Limits { max_entries: 1, ..Limits::default() };
		assert!(matches!(read(&bytes, &limits), Err(Error::Limit(_))));
	}
}
