//! Chunked FileMesh versions 6.00 and 7.00.
//!
//! After the version line the file is a sequence of chunks until the end of the data:
//! `[u8; 8] type` (NUL-padded), `u32 version`, `u32 size`, `size` bytes. Known types: `COREMESH`
//! (v1 raw vertices/faces, v2 Draco), `LODS` v1, `SKINNING` v1, `FACS` v1, `HSRAVIS` v1. Unknown
//! chunk types are skipped; known types with an unknown version are an error.

use crate::filemesh::{VERTEX_SIZE, read_faces, read_vertices};
use crate::reader::Reader;
use crate::skinning::{self, BONE_SIZE, ENVELOPE_SIZE, RawSkinning, SUBSET_SIZE};
use crate::{Mesh, MeshError, MeshFormat, check_indices, draco, face_count_u32, lods_from_offsets};

/// One chunk of a chunked FileMesh.
#[derive(Clone, Copy, Debug)]
pub struct Chunk<'a> {
	/// The type name with its NUL padding removed (`COREMESH`, `LODS`, ...).
	pub name: &'a [u8],
	pub version: u32,
	pub data: &'a [u8],
}

/// Splits the body of a chunked FileMesh (the bytes after `version 7.00\n`) into chunks.
pub fn chunks(body: &[u8]) -> Result<Vec<Chunk<'_>>, MeshError> {
	let mut reader = Reader::new(body);
	let mut chunks = Vec::new();
	while reader.remaining() > 0 {
		let name = reader.bytes(8, "chunk type")?;
		let version = reader.u32("chunk version")?;
		let data = reader.sized("chunk data")?;
		let trimmed = name.iter().position(|&byte| byte == 0).map_or(name, |end| &name[..end]);
		chunks.push(Chunk { name: trimmed, version, data });
	}
	Ok(chunks)
}

pub(crate) fn decode(body: &[u8], major: u8) -> Result<Mesh, MeshError> {
	let chunks = chunks(body)?;
	let mut core = None;
	let mut lods = None;
	let mut skin = None;
	let mut facs = None;
	let mut hsr = None;
	for chunk in &chunks {
		let slot = match chunk.name {
			b"COREMESH" => &mut core,
			b"LODS" => &mut lods,
			b"SKINNING" => &mut skin,
			b"FACS" => &mut facs,
			b"HSRAVIS" => &mut hsr,
			_ => continue,
		};
		if slot.is_some() {
			return Err(MeshError::Invalid(format!("duplicate {} chunk", String::from_utf8_lossy(chunk.name))));
		}
		*slot = Some(*chunk);
	}

	let core = core.ok_or_else(|| MeshError::Invalid("no COREMESH chunk".into()))?;
	let mut mesh = Mesh::default();
	match core.version {
		1 => decode_core_v1(core.data, &mut mesh)?,
		2 => draco::decode_core_v2(core.data, &mut mesh)?,
		version => return Err(MeshError::UnsupportedVersion(format!("COREMESH chunk v{version}"))),
	}
	mesh.format = MeshFormat::Chunked { major, draco: core.version == 2 };
	let vertex_count = mesh.positions.len();
	check_indices(&mesh.indices, vertex_count)?;
	let face_count = face_count_u32(mesh.indices.len() / 3)?;

	let mut offsets = Vec::new();
	if let Some(chunk) = lods {
		expect_version(&chunk, 1)?;
		let mut reader = Reader::new(chunk.data);
		mesh.lod_type = reader.u16("LOD type")?;
		mesh.high_quality_lods = reader.u8("high-quality LOD count")?;
		let count = reader.u32("LOD count")? as usize;
		offsets = reader
			.array(count, 4, "LOD offsets")?
			.chunks_exact(4)
			.map(|offset| crate::reader::le_u32(offset, 0))
			.collect();
	}
	mesh.lods = lods_from_offsets(&offsets, face_count)?;

	if let Some(chunk) = skin {
		expect_version(&chunk, 1)?;
		let mut reader = Reader::new(chunk.data);
		let envelope_count = reader.u32("skinning envelope count")? as usize;
		let envelopes = reader.array(envelope_count, ENVELOPE_SIZE, "skinning envelopes")?;
		if envelope_count != 0 && envelope_count != vertex_count {
			return Err(MeshError::Invalid(format!("{envelope_count} skinning envelopes for {vertex_count} vertices")));
		}
		let bone_count = reader.u32("bone count")? as usize;
		let bones = reader.array(bone_count, BONE_SIZE, "bones")?;
		let names = reader.sized("bone names")?;
		let subset_count = reader.u32("subset count")? as usize;
		let subsets = reader.array(subset_count, SUBSET_SIZE, "subsets")?;
		mesh.skinning = skinning::build(RawSkinning { envelopes, bones, names, subsets }, vertex_count)?;
	}

	if let Some(chunk) = facs {
		expect_version(&chunk, 1)?;
		let data = Reader::new(chunk.data).sized("FACS data")?;
		if !data.is_empty() {
			mesh.facs = Some(data.to_vec());
		}
	}

	if let Some(chunk) = hsr {
		expect_version(&chunk, 1)?;
		let mut reader = Reader::new(chunk.data);
		let bits = reader.u32("HSRAVIS bit count")? as usize;
		mesh.hsr_always_visible = Some(reader.bytes(bits.div_ceil(8), "HSRAVIS bits")?.to_vec());
	}
	Ok(mesh)
}

fn expect_version(chunk: &Chunk<'_>, version: u32) -> Result<(), MeshError> {
	if chunk.version != version {
		return Err(MeshError::UnsupportedVersion(format!(
			"{} chunk v{}",
			String::from_utf8_lossy(chunk.name),
			chunk.version
		)));
	}
	Ok(())
}

/// `COREMESH` v1: `u32 vertex count`, 40-byte vertices, `u32 face count`, 12-byte faces.
fn decode_core_v1(data: &[u8], mesh: &mut Mesh) -> Result<(), MeshError> {
	let mut reader = Reader::new(data);
	let vertex_count = reader.u32("vertex count")? as usize;
	read_vertices(reader.array(vertex_count, VERTEX_SIZE, "vertices")?, VERTEX_SIZE, mesh);
	let face_count = reader.u32("face count")? as usize;
	read_faces(reader.array(face_count, 12, "faces")?, 12, &mut mesh.indices);
	Ok(())
}
