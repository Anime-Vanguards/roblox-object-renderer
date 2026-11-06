//! Just enough of Roblox's model formats to pull union mesh data out of the asset behind
//! `PartOperation.AssetId`: a model holding one `PartOperationAsset` whose `MeshData` (a
//! `BinaryString`) is the `CSGMDL` blob.
//!
//! Binary models: `<roblox!` + 6 signature bytes, `u16` version, `u32` class count, `u32` instance
//! count, 8 reserved bytes, then chunks (`[u8; 4]` name, `u32` compressed size, `u32` size,
//! `u32` reserved, data; LZ4 block or zstd when the compressed size is non-zero). `INST` gives each
//! class id's instance count; `PROP` chunks of type `0x01` (string) hold `u32` length-prefixed values,
//! one per instance.

use crate::MeshError;
use crate::reader::Reader;

/// Chunks larger than this are refused instead of allocated.
const MAX_CHUNK: usize = 256 << 20;

/// Returns the first non-empty `MeshData` property found in a binary or XML model.
pub(crate) fn find_mesh_data(model: &[u8]) -> Result<Option<Vec<u8>>, MeshError> {
	if model.starts_with(b"<roblox!") {
		binary_string_property(model, b"MeshData")
	} else {
		xml_string_property(model, "MeshData")
	}
}

fn binary_string_property(model: &[u8], wanted: &[u8]) -> Result<Option<Vec<u8>>, MeshError> {
	let mut reader = Reader::new(model);
	reader.skip(32, "model header")?;
	let mut instance_counts: Vec<(u32, u32)> = Vec::new();
	let mut scratch = Vec::new();
	while reader.remaining() > 0 {
		let name = reader.fixed::<4>("model chunk name")?;
		let compressed = reader.u32("model chunk size")? as usize;
		let size = reader.u32("model chunk size")? as usize;
		reader.skip(4, "model chunk")?;
		if &name == b"END\0" {
			break;
		}
		let stored = reader.bytes(if compressed == 0 { size } else { compressed }, "model chunk data")?;
		if &name != b"INST" && &name != b"PROP" {
			continue;
		}
		let data = chunk_data(stored, compressed, size, &mut scratch)?;
		let mut chunk = Reader::new(data);
		let class_id = chunk.u32("model chunk class")?;
		let chunk_name = chunk.sized("model chunk class name")?;
		if &name == b"INST" {
			chunk.skip(1, "INST format")?;
			instance_counts.push((class_id, chunk.u32("INST count")?));
			continue;
		}
		if chunk_name != wanted || chunk.u8("PROP type")? != 0x01 {
			continue;
		}
		let count = instance_counts.iter().find(|(id, _)| *id == class_id).map_or(0, |(_, count)| *count);
		for _ in 0..count {
			let value = chunk.sized("string property")?;
			if !value.is_empty() {
				return Ok(Some(value.to_vec()));
			}
		}
	}
	Ok(None)
}

fn chunk_data<'a>(
	stored: &'a [u8],
	compressed: usize,
	size: usize,
	scratch: &'a mut Vec<u8>,
) -> Result<&'a [u8], MeshError> {
	if compressed == 0 {
		return Ok(stored);
	}
	if size > MAX_CHUNK {
		return Err(MeshError::Invalid(format!("model chunk of {size} bytes")));
	}
	scratch.clear();
	if stored.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
		*scratch = zstd::bulk::decompress(stored, size)
			.map_err(|error| MeshError::Invalid(format!("model chunk zstd: {error}")))?;
	} else {
		// LZ4 expands at most ~255:1; a larger claim is corrupt, so do not allocate for it.
		if size > stored.len().saturating_mul(255).saturating_add(16) {
			return Err(MeshError::Invalid(format!("model chunk claims {size} bytes from {}", stored.len())));
		}
		scratch.resize(size, 0);
		let written = lz4_flex::block::decompress_into(stored, scratch)
			.map_err(|error| MeshError::Invalid(format!("model chunk lz4: {error}")))?;
		scratch.truncate(written);
	}
	if scratch.len() != size {
		return Err(MeshError::Invalid(format!("model chunk decompressed to {} of {size} bytes", scratch.len())));
	}
	Ok(scratch)
}

/// XML models: `<BinaryString name="MeshData"><![CDATA[base64]]></BinaryString>` (CDATA optional).
fn xml_string_property(model: &[u8], wanted: &str) -> Result<Option<Vec<u8>>, MeshError> {
	let text = String::from_utf8_lossy(model);
	let needle = format!("name=\"{wanted}\"");
	let mut rest = text.as_ref();
	while let Some(at) = rest.find(&needle) {
		let after = &rest[at + needle.len()..];
		let Some(open_end) = after.find('>') else {
			break;
		};
		let body = &after[open_end + 1..];
		let Some(close) = body.find("</") else {
			break;
		};
		let mut value = body[..close].trim();
		if let Some(inner) = value.strip_prefix("<![CDATA[").and_then(|inner| inner.strip_suffix("]]>")) {
			value = inner;
		}
		let decoded = base64(value)?;
		if !decoded.is_empty() {
			return Ok(Some(decoded));
		}
		rest = &body[close..];
	}
	Ok(None)
}

fn base64(text: &str) -> Result<Vec<u8>, MeshError> {
	let mut out = Vec::with_capacity(text.len() / 4 * 3);
	let mut buffer = 0u32;
	let mut bits = 0;
	for byte in text.bytes() {
		let value = match byte {
			b'A'..=b'Z' => byte - b'A',
			b'a'..=b'z' => byte - b'a' + 26,
			b'0'..=b'9' => byte - b'0' + 52,
			b'+' => 62,
			b'/' => 63,
			b'=' => break,
			b' ' | b'\t' | b'\r' | b'\n' => continue,
			_ => return Err(MeshError::Invalid("invalid base64 in model".into())),
		};
		buffer = (buffer << 6) | value as u32;
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((buffer >> bits) as u8);
		}
	}
	Ok(out)
}
