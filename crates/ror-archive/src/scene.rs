//! The binary scene inside an archive, and how values stored outside it are put back.
//!
//! A binary Roblox file is a 32-byte header and a run of chunks (`[u8; 4]` name, `u32` compressed
//! size, `u32` size, `u32` reserved, data; an LZ4 block or a zstd frame when the compressed size is
//! not zero). `INST` gives each class id its instance count; a `PROP` chunk is `u32` class id, the
//! property's name, a type byte, then one value per instance; `SSTR` is the table shared strings
//! index into.
//!
//! In an archive a `PROP` chunk may start with the tag `0x82` instead of its type byte. A sparse
//! column of references follows, one slot per instance, then the real type byte and the values as
//! usual. Each slot is an `i32` length: negative skips that many instances, zero leaves one
//! instance's inline value in use, positive is that many bytes of reference (`u8` version, 16 digest
//! bytes, `u64` object size; version 2 appends a `u32`-prefixed identity). A referenced value's
//! inline form is an empty placeholder: an empty string, an empty shared string, or a `None` content.
//!
//! Flattening rewrites those chunks without the tag. Strings and shared strings get their bytes
//! back. A content reference (version 2) names an opaque object that no standalone file can carry,
//! so its value stays `None` and the [`Report`] lists it. Every other chunk is copied as it is.
//!
//! A reference is honoured only when everything about it checks out: the archive holds the object,
//! the object has the size and the BLAKE3 digest the reference gives, and the value it stands for is
//! the empty placeholder of its type. Anything else is an error, never a guess.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

use crate::{Archive, Error, Result};

pub(crate) const MAGIC: &[u8] = b"<roblox!\x89\xff\x0d\x0a\x1a\x0a";
pub(crate) const HEADER_LEN: usize = 32;
const CHUNK_HEADER_LEN: usize = 16;
const ZSTD_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
/// The most a zstd frame holds for each byte of its own: a run-length block is four bytes for 128 KiB.
const ZSTD_EXPANSION: usize = 32_768;

/// The tag that introduces a column of object references in a `PROP` chunk.
pub(crate) const TAG_EMBEDDED: u8 = 0x82;
pub(crate) const TYPE_STRING: u8 = 0x01;
pub(crate) const TYPE_SHARED_STRING: u8 = 0x1C;
pub(crate) const TYPE_CONTENT: u8 = 0x22;
/// The longest identity a version 2 reference may carry.
const MAX_IDENTITY: usize = 269;

/// What flattening an archive's scene did.
#[derive(Debug, Clone, Default)]
pub struct Report {
	/// Property values whose bytes were written back into the scene from `objects/`.
	pub restored_values: usize,
	/// The bytes those values hold.
	pub restored_bytes: u64,
	/// Content stored as opaque objects, which a standalone file cannot carry: these properties read
	/// as `None` in the flattened scene.
	pub embedded_content: Vec<EmbeddedContent>,
	/// Things a strict reader would refuse that this one read past.
	pub warnings: Vec<String>,
}

/// A content property whose values live in the archive as opaque objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedContent {
	/// The class of the instances.
	pub class: String,
	/// The property's name.
	pub property: String,
	/// How many instances have such a value.
	pub values: usize,
	/// The bytes of the objects they reference.
	pub bytes: u64,
}

/// One chunk of a binary scene, as stored.
#[derive(Debug, Clone)]
pub(crate) struct Chunk {
	pub(crate) name: [u8; 4],
	/// The chunk with its 16-byte header.
	pub(crate) whole: Range<usize>,
	/// The stored (maybe compressed) data.
	pub(crate) stored: Range<usize>,
	pub(crate) compressed: bool,
	pub(crate) size: usize,
}

fn invalid(message: impl Into<String>) -> Error {
	Error::Scene(message.into())
}

/// The chunks of a binary scene up to and including `END`, and where the bytes after it start.
pub(crate) fn chunks(scene: &[u8]) -> Result<(Vec<Chunk>, usize)> {
	if !scene.starts_with(MAGIC) || scene.len() < HEADER_LEN {
		return Err(invalid("it does not start with the binary file header"));
	}
	let version = u16::from_le_bytes([scene[14], scene[15]]);
	if version != 0 {
		return Err(Error::Unsupported(format!("binary file version {version}")));
	}
	let mut list = Vec::new();
	let mut at = HEADER_LEN;
	loop {
		let header = scene.get(at..at + CHUNK_HEADER_LEN).ok_or_else(|| invalid("a chunk header is cut short, or END is missing"))?;
		let name = [header[0], header[1], header[2], header[3]];
		let compressed = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
		let size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
		let start = at + CHUNK_HEADER_LEN;
		let end = start
			.checked_add(if compressed == 0 { size } else { compressed })
			.filter(|end| *end <= scene.len())
			.ok_or_else(|| invalid(format!("chunk {} runs past the end of the file", String::from_utf8_lossy(&name))))?;
		list.push(Chunk { name, whole: at..end, stored: start..end, compressed: compressed != 0, size });
		at = end;
		if &name == b"END\0" {
			return Ok((list, at));
		}
	}
}

/// A chunk's data, decompressed into `scratch` when it is stored compressed.
pub(crate) fn data<'a>(scene: &'a [u8], chunk: &Chunk, scratch: &'a mut Vec<u8>, max: usize) -> Result<&'a [u8]> {
	let stored = &scene[chunk.stored.clone()];
	if !chunk.compressed {
		return Ok(stored);
	}
	let name = String::from_utf8_lossy(&chunk.name).into_owned();
	if chunk.size > max {
		return Err(Error::Limit("scene chunk bytes"));
	}
	scratch.clear();
	if stored.starts_with(ZSTD_MAGIC) {
		// A chunk holds one frame, and a frame says how much it holds when its writer knew, as the
		// writers of these files do. A chunk that disagrees with its frame, or claims more than a frame
		// of that length can hold, is corrupt, so do not allocate for it.
		if let Ok(frame) = zstd::zstd_safe::find_frame_compressed_size(stored)
			&& frame != stored.len()
		{
			return Err(invalid(format!("chunk {name} has {} bytes after its zstd frame", stored.len() - frame)));
		}
		if let Ok(Some(declared)) = zstd::zstd_safe::get_frame_content_size(stored)
			&& declared != chunk.size as u64
		{
			return Err(invalid(format!("chunk {name} claims {} bytes, and its zstd frame declares {declared}", chunk.size)));
		}
		if chunk.size > stored.len().saturating_mul(ZSTD_EXPANSION) {
			return Err(invalid(format!("chunk {name} claims {} bytes from {}", chunk.size, stored.len())));
		}
		*scratch = zstd::bulk::decompress(stored, chunk.size).map_err(|error| invalid(format!("chunk {name} zstd: {error}")))?;
	} else {
		// LZ4 expands at most ~255:1; a larger claim is corrupt, so do not allocate for it
		if chunk.size > stored.len().saturating_mul(255).saturating_add(16) {
			return Err(invalid(format!("chunk {name} claims {} bytes from {}", chunk.size, stored.len())));
		}
		scratch.resize(chunk.size, 0);
		let written = lz4_flex::block::decompress_into(stored, scratch).map_err(|error| invalid(format!("chunk {name} lz4: {error}")))?;
		scratch.truncate(written);
	}
	if scratch.len() != chunk.size {
		return Err(invalid(format!("chunk {name} decompressed to {} of {} bytes", scratch.len(), chunk.size)));
	}
	Ok(scratch)
}

/// Appends a chunk, its data as an LZ4 block: the compression every reader of the format has.
pub(crate) fn write_chunk(out: &mut Vec<u8>, name: &[u8; 4], payload: &[u8]) -> Result<()> {
	// an LZ4 block cannot hold more, and the chunk's size fields are 32 bits
	if payload.len() > 0x7E00_0000 {
		return Err(Error::Limit("scene chunk bytes"));
	}
	let compressed = lz4_flex::block::compress(payload);
	out.extend_from_slice(name);
	out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
	out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
	out.extend_from_slice(&[0; 4]);
	out.extend_from_slice(&compressed);
	Ok(())
}

/// A cursor over a chunk's data.
pub(crate) struct Reader<'a> {
	pub(crate) bytes: &'a [u8],
	pub(crate) at: usize,
}

impl<'a> Reader<'a> {
	pub(crate) fn new(bytes: &'a [u8]) -> Self {
		Self { bytes, at: 0 }
	}

	pub(crate) fn remaining(&self) -> usize {
		self.bytes.len() - self.at
	}

	pub(crate) fn rest(&self) -> &'a [u8] {
		&self.bytes[self.at..]
	}

	pub(crate) fn take(&mut self, len: usize, what: &str) -> Result<&'a [u8]> {
		let end = self.at.checked_add(len).filter(|end| *end <= self.bytes.len()).ok_or_else(|| invalid(format!("{what} is cut short")))?;
		let slice = &self.bytes[self.at..end];
		self.at = end;
		Ok(slice)
	}

	pub(crate) fn u8(&mut self, what: &str) -> Result<u8> {
		Ok(self.take(1, what)?[0])
	}

	pub(crate) fn u32(&mut self, what: &str) -> Result<u32> {
		Ok(u32::from_le_bytes(self.take(4, what)?.try_into().expect("four bytes")))
	}

	pub(crate) fn u64(&mut self, what: &str) -> Result<u64> {
		Ok(u64::from_le_bytes(self.take(8, what)?.try_into().expect("eight bytes")))
	}

	/// A `u32` length and that many bytes.
	pub(crate) fn sized(&mut self, what: &str) -> Result<&'a [u8]> {
		let len = self.u32(what)? as usize;
		self.take(len, what)
	}
}

/// `count` big-endian `u32`s stored byte-interleaved: every value's first byte, then every second...
pub(crate) fn read_interleaved(bytes: &[u8], count: usize) -> Vec<u32> {
	(0..count)
		.map(|index| {
			u32::from_be_bytes([bytes[index], bytes[count + index], bytes[2 * count + index], bytes[3 * count + index]])
		})
		.collect()
}

pub(crate) fn write_interleaved(out: &mut Vec<u8>, values: &[u32]) {
	for byte in 0..4 {
		out.extend(values.iter().map(|value| value.to_be_bytes()[byte]));
	}
}

pub(crate) fn push_sized(out: &mut Vec<u8>, bytes: &[u8]) {
	out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
	out.extend_from_slice(bytes);
}

/// The class a chunk belongs to: its id, name and instance count, from its `INST` chunk.
pub(crate) struct Class {
	pub(crate) name: String,
	pub(crate) count: usize,
}

pub(crate) fn read_inst(payload: &[u8]) -> Result<(u32, Class)> {
	let mut chunk = Reader::new(payload);
	let id = chunk.u32("an INST chunk")?;
	let name = String::from_utf8_lossy(chunk.sized("an INST chunk's class name")?).into_owned();
	chunk.u8("an INST chunk")?;
	let count = chunk.u32("an INST chunk")? as usize;
	// each instance has a four-byte referent, so a count the chunk cannot hold is corrupt
	if count > chunk.remaining() / 4 {
		return Err(invalid(format!("class {name} claims {count} instances")));
	}
	Ok((id, Class { name, count }))
}

/// A reference from a property value to an object of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reference {
	pub(crate) digest: [u8; 16],
	pub(crate) size: u64,
	/// Version 2's identity: present for opaque content, absent for plain bytes.
	pub(crate) identity: Option<String>,
}

impl Reference {
	fn decode(bytes: &[u8]) -> Result<Self> {
		let mut reader = Reader::new(bytes);
		let version = reader.u8("an object reference")?;
		if version != 1 && version != 2 {
			return Err(Error::Unsupported(format!("object reference version {version} in a property")));
		}
		let digest: [u8; 16] = reader.take(16, "an object reference")?.try_into().expect("sixteen bytes");
		let size = reader.u64("an object reference")?;
		let identity = if version == 2 {
			let identity = reader.sized("an object reference's identity")?;
			if identity.len() > MAX_IDENTITY {
				return Err(invalid("an object reference's identity is too long"));
			}
			Some(std::str::from_utf8(identity).map_err(|_| invalid("an object reference's identity is not UTF-8"))?.to_owned())
		} else {
			None
		};
		if size == 0 || reader.remaining() != 0 {
			return Err(invalid("an object reference has a wrong length"));
		}
		Ok(Self { digest, size, identity })
	}

	pub(crate) fn encode(&self, out: &mut Vec<u8>) {
		let start = out.len();
		out.extend_from_slice(&[0; 4]);
		out.push(if self.identity.is_some() { 2 } else { 1 });
		out.extend_from_slice(&self.digest);
		out.extend_from_slice(&self.size.to_le_bytes());
		if let Some(identity) = &self.identity {
			push_sized(out, identity.as_bytes());
		}
		let len = (out.len() - start - 4) as i32;
		out[start..start + 4].copy_from_slice(&len.to_le_bytes());
	}

	fn hex(&self) -> String {
		self.digest.iter().map(|byte| format!("{byte:02x}")).collect()
	}
}

/// The reference column after a `0x82` tag: one slot per instance of the class.
pub(crate) fn read_references(chunk: &mut Reader, count: usize) -> Result<Vec<Option<Reference>>> {
	let mut references = Vec::with_capacity(count);
	while references.len() < count {
		let length = chunk.u32("a property's object references")? as i32;
		if length < 0 {
			let skip = length.unsigned_abs() as usize;
			if skip > count - references.len() {
				return Err(invalid("a property's object references skip past its instances"));
			}
			references.resize(references.len() + skip, None);
		} else if length == 0 {
			references.push(None);
		} else {
			references.push(Some(Reference::decode(chunk.take(length as usize, "an object reference")?)?));
		}
	}
	Ok(references)
}

/// Writes a reference column, runs of instances without a reference as one skip each.
pub(crate) fn write_references(out: &mut Vec<u8>, references: &[Option<Reference>]) {
	let mut index = 0;
	while index < references.len() {
		if let Some(reference) = &references[index] {
			reference.encode(out);
			index += 1;
			continue;
		}
		let run = references[index..].iter().take_while(|reference| reference.is_none()).count();
		out.extend_from_slice(&(-(run as i32)).to_le_bytes());
		index += run;
	}
}

/// The shared string table: its entries as stored, and the ones flattening adds.
struct SharedStrings<'a> {
	/// Each entry's data.
	entries: Vec<&'a [u8]>,
	/// The stored entries, hash and data, as they are in the chunk.
	stored: &'a [u8],
	added: Vec<u8>,
	added_count: usize,
	/// The index given to each object already added.
	by_digest: BTreeMap<[u8; 16], u32>,
}

impl<'a> SharedStrings<'a> {
	fn parse(payload: &'a [u8]) -> Result<Self> {
		let mut chunk = Reader::new(payload);
		let version = chunk.u32("the shared string table")?;
		if version != 0 {
			return Err(Error::Unsupported(format!("shared string table version {version}")));
		}
		let count = chunk.u32("the shared string table")? as usize;
		if count > chunk.remaining() / 20 {
			return Err(invalid(format!("the shared string table claims {count} entries")));
		}
		let start = chunk.at;
		let mut entries = Vec::with_capacity(count);
		for _ in 0..count {
			chunk.take(16, "a shared string's hash")?;
			entries.push(chunk.sized("a shared string")?);
		}
		// Bytes after the last entry belong to no entry, and a parser stops before them. They are left
		// out, so that the entries flattening adds follow the stored ones directly.
		let stored = &payload[start..chunk.at];
		Ok(Self { entries, stored, added: Vec::new(), added_count: 0, by_digest: BTreeMap::new() })
	}

	/// The index of the entry added for this object, when there is one.
	fn index_of(&self, digest: &[u8; 16]) -> Option<u32> {
		self.by_digest.get(digest).copied()
	}

	/// Adds an entry for an object that has none yet (see [`SharedStrings::index_of`]).
	fn add(&mut self, digest: [u8; 16], bytes: &[u8]) -> u32 {
		let index = (self.entries.len() + self.added_count) as u32;
		// the table's hash field is the first 16 bytes of the string's BLAKE3, which is the object's digest
		self.added.extend_from_slice(&digest);
		push_sized(&mut self.added, bytes);
		self.added_count += 1;
		self.by_digest.insert(digest, index);
		index
	}

	fn payload(&self) -> Vec<u8> {
		let mut out = Vec::with_capacity(8 + self.stored.len() + self.added.len());
		out.extend_from_slice(&0u32.to_le_bytes());
		out.extend_from_slice(&((self.entries.len() + self.added_count) as u32).to_le_bytes());
		out.extend_from_slice(self.stored);
		out.extend_from_slice(&self.added);
		out
	}
}

enum Out {
	Verbatim(Range<usize>),
	Rewritten([u8; 4], Vec<u8>),
}

/// The objects a scene's properties refer to, each one found and checked once however many values
/// name it.
struct Objects<'a, 'b> {
	archive: &'b Archive<'a>,
	scene_digest: &'b str,
	found: BTreeMap<[u8; 16], &'a [u8]>,
}

impl<'a> Objects<'a, '_> {
	fn resolve(&mut self, reference: &Reference) -> Result<&'a [u8]> {
		let bytes = match self.found.get(&reference.digest) {
			Some(bytes) => *bytes,
			None => {
				let hex = reference.hex();
				if self.scene_digest == hex {
					return Err(invalid("a property refers to the scene itself"));
				}
				let Some(bytes) = self.archive.object(&hex) else {
					let algorithm = &self.archive.manifest().digest_algorithm;
					return Err(invalid(if self.archive.verified() {
						format!("a property refers to object {hex}, which the archive does not hold")
					} else {
						format!(
							"a property refers to object {hex}, which the archive does not hold under that name (its objects are named by digest algorithm '{algorithm}', which this reader does not know)"
						)
					}));
				};
				// A reference names its object by the first 16 bytes of the object's BLAKE3, whatever
				// algorithm the manifest names the members by. Opening the archive checked exactly that
				// when the two are the same algorithm.
				if !self.archive.verified() && blake3::hash(bytes).as_bytes()[..16] != reference.digest {
					return Err(Error::Digest { digest: hex });
				}
				self.found.insert(reference.digest, bytes);
				bytes
			}
		};
		if bytes.len() as u64 != reference.size {
			return Err(invalid(format!(
				"object {} is {} bytes where a property expects {}",
				reference.hex(),
				bytes.len(),
				reference.size
			)));
		}
		Ok(bytes)
	}
}

/// Counts bytes against the limit on what is written back into a scene, before they are copied.
fn charge(written: &mut u64, bytes: &[u8], limit: u64) -> Result<()> {
	*written = written.saturating_add(bytes.len() as u64);
	if *written > limit {
		return Err(Error::Limit("restored bytes"));
	}
	Ok(())
}

/// Rewrites a binary scene so that no property refers to an object outside it. Returns the scene
/// itself when it was standalone already.
pub(crate) fn flatten<'a>(scene: &'a [u8], scene_digest: &str, archive: &Archive<'a>) -> Result<(Cow<'a, [u8]>, Report)> {
	let limits = archive.limits();
	let (list, end) = chunks(scene)?;
	// A chunk is decompressed once at most, so what the chunks claim together bounds the work. Each one
	// has its own limit, but a scene can hold a great many of them.
	let decompressed = list.iter().fold(0u64, |total, chunk| total.saturating_add(chunk.size as u64));
	if decompressed > limits.max_scene_bytes {
		return Err(Error::Limit("scene bytes"));
	}
	let mut report = Report::default();
	let mut classes: BTreeMap<u32, Class> = BTreeMap::new();
	let mut outputs: Vec<Out> = Vec::with_capacity(list.len());
	let mut changed = false;
	let mut scratch = Vec::new();
	// the table comes before the properties that index it; it is read only if a shared string is restored
	let tables: Vec<usize> = list.iter().enumerate().filter(|(_, chunk)| &chunk.name == b"SSTR").map(|(index, _)| index).collect();
	let sstr_payload: std::cell::OnceCell<Vec<u8>> = std::cell::OnceCell::new();
	let mut shared: Option<(usize, SharedStrings)> = None;
	let mut objects = Objects { archive, scene_digest, found: BTreeMap::new() };
	// what the rewritten chunks gain: a string once per value that holds it, a shared string once
	let mut written = 0u64;

	for chunk in &list {
		let verbatim = Out::Verbatim(chunk.whole.clone());
		if &chunk.name == b"INST" {
			let (id, class) = read_inst(data(scene, chunk, &mut scratch, limits.max_chunk_bytes)?)?;
			classes.insert(id, class);
			outputs.push(verbatim);
			continue;
		}
		if &chunk.name != b"PROP" {
			outputs.push(verbatim);
			continue;
		}
		let payload = data(scene, chunk, &mut scratch, limits.max_chunk_bytes)?;
		let mut reader = Reader::new(payload);
		let class_id = reader.u32("a PROP chunk")?;
		let property = reader.sized("a PROP chunk's property name")?;
		// a chunk that ends after the name carries no values; readers skip it
		if reader.remaining() == 0 {
			outputs.push(verbatim);
			continue;
		}
		let prefix_len = reader.at;
		let tag = reader.u8("a PROP chunk")?;
		if tag < 0x80 {
			outputs.push(verbatim);
			continue;
		}
		let class = classes.get(&class_id).ok_or_else(|| invalid(format!("a PROP chunk names class id {class_id}, which has no INST chunk")))?;
		let property_name = String::from_utf8_lossy(property).into_owned();
		if tag != TAG_EMBEDDED {
			return Err(Error::Unsupported(format!(
				"property {}.{property_name} carries metadata tag {tag:#04x}, which this reader cannot restore",
				class.name
			)));
		}
		let references = read_references(&mut reader, class.count)?;
		let wire = reader.u8("a PROP chunk's type")?;
		if wire >= 0x80 {
			return Err(Error::Unsupported(format!("property {}.{property_name} carries more than one metadata block", class.name)));
		}
		let mut rewritten = Vec::with_capacity(payload.len());
		rewritten.extend_from_slice(&payload[..prefix_len]);
		rewritten.push(wire);
		let referenced = references.iter().flatten().count();
		if referenced == 0 {
			rewritten.extend_from_slice(reader.rest());
		} else {
			let mut restored = 0u64;
			match wire {
				TYPE_STRING => {
					for reference in &references {
						let inline = reader.sized("a string property")?;
						let Some(reference) = reference else {
							push_sized(&mut rewritten, inline);
							continue;
						};
						if reference.identity.is_some() || !inline.is_empty() {
							return Err(invalid(format!(
								"{}.{property_name} has a stored value that is not an empty placeholder for plain bytes",
								class.name
							)));
						}
						let bytes = objects.resolve(reference)?;
						charge(&mut written, bytes, limits.max_restored_bytes)?;
						restored += bytes.len() as u64;
						push_sized(&mut rewritten, bytes);
					}
				}
				TYPE_SHARED_STRING => {
					let raw = reader.take(class.count.checked_mul(4).ok_or_else(|| invalid("a shared string property is too long"))?, "a shared string property")?;
					let mut indices = read_interleaved(raw, class.count);
					if shared.is_none() {
						// Indices count through one table. With more than one, where the entries flattening
						// adds would land is each parser's own reading, so no value is put anywhere.
						let index = match tables[..] {
							[index] => index,
							[] => return Err(invalid("a shared string property has no shared string table to index")),
							_ => {
								return Err(Error::Unsupported(format!(
									"the scene has {} shared string tables, and {}.{property_name} keeps values outside it that go into one",
									tables.len(),
									class.name
								)));
							}
						};
						if sstr_payload.get().is_none() {
							let mut scratch = Vec::new();
							let payload = data(scene, &list[index], &mut scratch, limits.max_chunk_bytes)?.to_vec();
							let _ = sstr_payload.set(payload);
						}
						shared = Some((index, SharedStrings::parse(sstr_payload.get().expect("just set"))?));
					}
					let (_, table) = shared.as_mut().expect("just set");
					for (index, reference) in indices.iter_mut().zip(&references) {
						let Some(reference) = reference else {
							continue;
						};
						let placeholder = table.entries.get(*index as usize).ok_or_else(|| invalid("a shared string property indexes past its table"))?;
						if reference.identity.is_some() || !placeholder.is_empty() {
							return Err(invalid(format!(
								"{}.{property_name} has a stored value that is not an empty placeholder for plain bytes",
								class.name
							)));
						}
						let bytes = objects.resolve(reference)?;
						restored += bytes.len() as u64;
						*index = match table.index_of(&reference.digest) {
							Some(added) => added,
							None => {
								charge(&mut written, bytes, limits.max_restored_bytes)?;
								table.add(reference.digest, bytes)
							}
						};
					}
					write_interleaved(&mut rewritten, &indices);
				}
				TYPE_CONTENT => {
					// opaque objects have no form in a standalone file: the `None` placeholders stay
					let raw = reader.take(class.count.checked_mul(4).ok_or_else(|| invalid("a content property is too long"))?, "a content property")?;
					// each value's source type, stored transformed; `None` is 0 either way
					let sources = read_interleaved(raw, class.count);
					let mut bytes = 0u64;
					for (source, reference) in sources.iter().zip(&references) {
						let Some(reference) = reference else {
							continue;
						};
						if reference.identity.is_none() {
							return Err(invalid(format!("{}.{property_name} refers to plain bytes from a content value", class.name)));
						}
						if *source != 0 {
							return Err(invalid(format!(
								"{}.{property_name} has a stored value that is not a `None` placeholder for a content object",
								class.name
							)));
						}
						bytes += objects.resolve(reference)?.len() as u64;
					}
					report.embedded_content.push(EmbeddedContent {
						class: class.name.clone(),
						property: property_name.clone(),
						values: referenced,
						bytes,
					});
					rewritten.extend_from_slice(raw);
					rewritten.extend_from_slice(reader.rest());
					reader.at = reader.bytes.len();
				}
				other => {
					return Err(Error::Unsupported(format!(
						"property {}.{property_name} stores values of type {other:#04x} outside the scene",
						class.name
					)));
				}
			}
			if reader.remaining() != 0 {
				return Err(invalid(format!("{}.{property_name} has {} bytes after its values", class.name, reader.remaining())));
			}
			if wire != TYPE_CONTENT {
				report.restored_values += referenced;
				report.restored_bytes += restored;
			}
		}
		changed = true;
		outputs.push(Out::Rewritten(*b"PROP", rewritten));
	}

	if !changed {
		return Ok((Cow::Borrowed(scene), report));
	}
	if let Some((index, table)) = &shared
		&& table.added_count > 0
	{
		outputs[*index] = Out::Rewritten(*b"SSTR", table.payload());
	}
	let mut out = Vec::with_capacity(scene.len() + written as usize / 2);
	out.extend_from_slice(&scene[..HEADER_LEN]);
	for output in &outputs {
		match output {
			Out::Verbatim(range) => out.extend_from_slice(&scene[range.clone()]),
			// a chunk is written back within the limit it was read under, so the result reads under the
			// same limits
			Out::Rewritten(_, payload) if payload.len() > limits.max_chunk_bytes => return Err(Error::Limit("scene chunk bytes")),
			Out::Rewritten(name, payload) => write_chunk(&mut out, name, payload)?,
		}
	}
	out.extend_from_slice(&scene[end..]);
	Ok((Cow::Owned(out), report))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn interleaved_values_round_trip() {
		let values = [0u32, 1, 0x0102_0304, u32::MAX, 77];
		let mut bytes = Vec::new();
		write_interleaved(&mut bytes, &values);
		assert_eq!(bytes.len(), 20);
		// every value's most significant byte comes first
		assert_eq!(&bytes[..5], &[0, 0, 1, 255, 0]);
		assert_eq!(read_interleaved(&bytes, 5), values);
	}

	#[test]
	fn a_reference_column_round_trips_with_skips() {
		let plain = Reference { digest: [7; 16], size: 12, identity: None };
		let opaque = Reference { digest: [9; 16], size: 3, identity: Some("content-1".into()) };
		let column = vec![None, None, Some(plain.clone()), None, Some(opaque.clone()), None, None, None];
		let mut bytes = Vec::new();
		write_references(&mut bytes, &column);
		// two skips of four bytes, two references, one more skip
		assert_eq!(&bytes[..4], &(-2i32).to_le_bytes());
		let mut reader = Reader::new(&bytes);
		assert_eq!(read_references(&mut reader, column.len()).unwrap(), column);
		assert_eq!(reader.remaining(), 0);
	}

	#[test]
	fn a_reference_column_that_overruns_its_instances_is_refused() {
		let mut bytes = Vec::new();
		bytes.extend_from_slice(&(-3i32).to_le_bytes());
		assert!(read_references(&mut Reader::new(&bytes), 2).is_err());
		// a zero-size object and an unknown version are refused too
		let mut zero = Vec::new();
		Reference { digest: [1; 16], size: 0, identity: None }.encode(&mut zero);
		assert!(read_references(&mut Reader::new(&zero), 1).is_err());
		let mut newer = Vec::new();
		Reference { digest: [1; 16], size: 4, identity: None }.encode(&mut newer);
		newer[4] = 3;
		assert!(matches!(read_references(&mut Reader::new(&newer), 1), Err(Error::Unsupported(_))));
	}
}
