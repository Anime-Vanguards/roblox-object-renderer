//! FileMesh versions 1.00 to 5.00, and the version-line dispatch.
//!
//! Every file starts with a 12-byte version line (`version 2.00`) and a newline (`\r\n` in old text
//! meshes). Layouts are in `notes/mesh.md`.

use crate::reader::{Reader, le_f32, le_u32, le_vec3};
use crate::skinning::{self, BONE_SIZE, ENVELOPE_SIZE, RawSkinning, SUBSET_SIZE};
use crate::{Mesh, MeshError, MeshFormat, check_indices, chunked, decode_tangent, face_count_u32, lods_from_offsets};

/// Bytes of a full FileMesh vertex (position, normal, uv, tangent, colour).
pub(crate) const VERTEX_SIZE: usize = 40;
/// A 2.00/3.00 vertex without the colour.
const VERTEX_SIZE_NO_COLOR: usize = 36;
const FACE_SIZE: usize = 12;

pub(crate) fn decode(bytes: &[u8]) -> Result<Mesh, MeshError> {
	let Some(version) = bytes.get(8..12) else {
		return Err(MeshError::UnknownHeader(crate::header_text(bytes, 12)));
	};
	let version_text = String::from_utf8_lossy(version).into_owned();
	match version {
		b"1.00" | b"1.01" => return decode_v1(bytes, version[3] - b'0'),
		b"2.00" | b"3.00" | b"3.01" | b"4.00" | b"4.01" | b"5.00" | b"6.00" | b"7.00" => {}
		_ => return Err(MeshError::UnsupportedVersion(version_text)),
	}
	if bytes.get(12) != Some(&b'\n') {
		return Err(MeshError::Invalid(format!("version {version_text} line not followed by a newline")));
	}
	let body = &bytes[13..];
	match version {
		b"2.00" => decode_v2_v3(body, None),
		b"3.00" => decode_v2_v3(body, Some(0)),
		b"3.01" => decode_v2_v3(body, Some(1)),
		b"4.00" => decode_v4_v5(body, MeshFormat::V4 { minor: 0 }),
		b"4.01" => decode_v4_v5(body, MeshFormat::V4 { minor: 1 }),
		b"5.00" => decode_v4_v5(body, MeshFormat::V5),
		b"6.00" => chunked::decode(body, 6),
		_ => chunked::decode(body, 7),
	}
}

/// Appends `count` vertices of `stride` bytes (at least 36; colour present when at least 40).
pub(crate) fn read_vertices(data: &[u8], stride: usize, mesh: &mut Mesh) {
	let count = data.len() / stride;
	mesh.positions.reserve_exact(count);
	mesh.normals.reserve_exact(count);
	mesh.uvs.reserve_exact(count);
	mesh.tangents.reserve_exact(count);
	let has_color = stride >= VERTEX_SIZE;
	if has_color {
		mesh.colors.reserve_exact(count);
	}
	for vertex in data.chunks_exact(stride) {
		mesh.positions.push(le_vec3(vertex, 0));
		mesh.normals.push(le_vec3(vertex, 12));
		mesh.uvs.push([le_f32(vertex, 24), le_f32(vertex, 28)]);
		mesh.tangents.push(decode_tangent([vertex[32], vertex[33], vertex[34], vertex[35]]));
		if has_color {
			mesh.colors.push([vertex[36], vertex[37], vertex[38], vertex[39]]);
		}
	}
}

/// Appends the indices of `data.len() / stride` faces (three `u32` at the start of each record).
pub(crate) fn read_faces(data: &[u8], stride: usize, indices: &mut Vec<u32>) {
	indices.reserve_exact(data.len() / stride * 3);
	if stride == FACE_SIZE {
		indices.extend(data.chunks_exact(4).map(|word| le_u32(word, 0)));
		return;
	}
	for face in data.chunks_exact(stride) {
		indices.extend([le_u32(face, 0), le_u32(face, 4), le_u32(face, 8)]);
	}
}

fn read_lod_offsets(reader: &mut Reader<'_>, count: usize, size: usize) -> Result<Vec<u32>, MeshError> {
	if size < 4 {
		return Err(MeshError::Invalid(format!("LOD offset size {size}")));
	}
	let data = reader.array(count, size, "LOD offsets")?;
	Ok(data.chunks_exact(size).map(|offset| le_u32(offset, 0)).collect())
}

/// Versions 2.00 (`lod_minor == None`) and 3.00/3.01.
fn decode_v2_v3(body: &[u8], lod_minor: Option<u8>) -> Result<Mesh, MeshError> {
	let mut reader = Reader::new(body);
	let header_size = reader.u16("header size")? as usize;
	let vertex_size = reader.u8("vertex size")? as usize;
	let face_size = reader.u8("face size")? as usize;
	let (lod_offset_size, lod_count) = match lod_minor {
		Some(_) => (reader.u16("LOD offset size")? as usize, reader.u16("LOD count")? as usize),
		None => (4, 0),
	};
	let vertex_count = reader.u32("vertex count")? as usize;
	let face_count = reader.u32("face count")? as usize;
	if header_size < reader.pos() {
		return Err(MeshError::Invalid(format!("header size {header_size}")));
	}
	reader.skip(header_size - reader.pos(), "header")?;
	if vertex_size < VERTEX_SIZE_NO_COLOR || face_size < FACE_SIZE {
		return Err(MeshError::Invalid(format!("vertex size {vertex_size}, face size {face_size}")));
	}

	let mut mesh = Mesh {
		format: match lod_minor {
			Some(minor) => MeshFormat::V3 { minor },
			None => MeshFormat::V2,
		},
		..Mesh::default()
	};
	read_vertices(reader.array(vertex_count, vertex_size, "vertices")?, vertex_size, &mut mesh);
	read_faces(reader.array(face_count, face_size, "faces")?, face_size, &mut mesh.indices);
	let offsets = read_lod_offsets(&mut reader, lod_count, lod_offset_size)?;
	let face_count = face_count_u32(face_count)?;
	check_indices(&mesh.indices, vertex_count)?;
	mesh.lods = lods_from_offsets(&offsets, face_count)?;
	Ok(mesh)
}

/// Versions 4.00, 4.01 and 5.00 (4.01 differs from 4.00 only in the version line; the header's
/// last byte is padding that some 4.01 writers leave uninitialised).
fn decode_v4_v5(body: &[u8], format: MeshFormat) -> Result<Mesh, MeshError> {
	let mut reader = Reader::new(body);
	let header_size = reader.u16("header size")? as usize;
	let lod_type = reader.u16("LOD type")?;
	let vertex_count = reader.u32("vertex count")? as usize;
	let face_count = reader.u32("face count")? as usize;
	let lod_count = reader.u16("LOD count")? as usize;
	let bone_count = reader.u16("bone count")? as usize;
	let names_size = reader.u32("bone name table size")? as usize;
	let subset_count = reader.u16("subset count")? as usize;
	let high_quality_lods = reader.u8("high-quality LOD count")?;
	let _padding = reader.u8("header padding")?;
	let (facs_format, facs_size) = match format {
		MeshFormat::V5 => (reader.u32("FACS format")?, reader.u32("FACS size")? as usize),
		_ => (0, 0),
	};
	if header_size < reader.pos() {
		return Err(MeshError::Invalid(format!("header size {header_size}")));
	}
	reader.skip(header_size - reader.pos(), "header")?;

	let mut mesh = Mesh { format, lod_type, high_quality_lods, ..Mesh::default() };
	read_vertices(reader.array(vertex_count, VERTEX_SIZE, "vertices")?, VERTEX_SIZE, &mut mesh);
	let envelopes = if bone_count > 0 { reader.array(vertex_count, ENVELOPE_SIZE, "skinning envelopes")? } else { &[] };
	read_faces(reader.array(face_count, FACE_SIZE, "faces")?, FACE_SIZE, &mut mesh.indices);
	let offsets = read_lod_offsets(&mut reader, lod_count, 4)?;
	let bones = reader.array(bone_count, BONE_SIZE, "bones")?;
	let names = reader.bytes(names_size, "bone names")?;
	let subsets = reader.array(subset_count, SUBSET_SIZE, "subsets")?;
	if facs_size > 0 {
		let facs = reader.bytes(facs_size, "FACS data")?;
		if facs_format != 0 {
			mesh.facs = Some(facs.to_vec());
		}
	}

	let face_count = face_count_u32(face_count)?;
	check_indices(&mesh.indices, vertex_count)?;
	mesh.lods = lods_from_offsets(&offsets, face_count)?;
	mesh.skinning = skinning::build(RawSkinning { envelopes, bones, names, subsets }, vertex_count)?;
	Ok(mesh)
}

/// Versions 1.00 and 1.01: text. Line 2 is the face count, line 3 holds `[x,y,z]` triples, three
/// (position, normal, uvw) per vertex and three vertices per face. 1.00 positions are halved.
fn decode_v1(bytes: &[u8], minor: u8) -> Result<Mesh, MeshError> {
	let text = std::str::from_utf8(bytes).map_err(|_| MeshError::Invalid("version 1 mesh is not text".into()))?;
	let mut lines = text.splitn(3, '\n');
	let _version = lines.next();
	let face_count: usize = lines
		.next()
		.and_then(|line| line.trim().parse().ok())
		.ok_or_else(|| MeshError::Invalid("version 1 face count line".into()))?;
	let data = lines.next().unwrap_or("");
	let Some(vertex_count) = face_count.checked_mul(3) else {
		return Err(MeshError::Invalid(format!("face count {face_count}")));
	};
	// Each vertex needs at least 21 bytes of text ("[0,0,0]" three times).
	if vertex_count > data.len() / 21 + 1 {
		return Err(MeshError::Truncated("version 1 vertices"));
	}

	let scale = if minor == 0 { 0.5 } else { 1.0 };
	let mut mesh = Mesh { format: MeshFormat::V1 { minor }, ..Mesh::default() };
	mesh.positions.reserve_exact(vertex_count);
	mesh.normals.reserve_exact(vertex_count);
	mesh.uvs.reserve_exact(vertex_count);
	let mut vectors = Vec3Iter { rest: data };
	for _ in 0..vertex_count {
		let position = vectors.next_vec3()?;
		let normal = vectors.next_vec3()?;
		let uvw = vectors.next_vec3()?;
		mesh.positions.push(position.map(|value| value * scale));
		mesh.normals.push(normal);
		mesh.uvs.push([uvw[0], uvw[1]]);
	}
	if vectors.has_more() {
		return Err(MeshError::Invalid(format!("version 1 mesh has more than {face_count} faces of data")));
	}
	let face_count = face_count_u32(face_count)?;
	mesh.indices = (0..face_count * 3).collect();
	mesh.lods = lods_from_offsets(&[], face_count)?;
	Ok(mesh)
}

/// Iterates `[a,b,c]` groups of a version 1 data line.
struct Vec3Iter<'a> {
	rest: &'a str,
}

impl Vec3Iter<'_> {
	fn next_vec3(&mut self) -> Result<[f32; 3], MeshError> {
		let start = self.rest.find('[').ok_or(MeshError::Truncated("version 1 vertices"))?;
		let after = &self.rest[start + 1..];
		let end = after.find(']').ok_or(MeshError::Truncated("version 1 vertices"))?;
		let group = &after[..end];
		self.rest = &after[end + 1..];
		let mut out = [0f32; 3];
		let mut parts = group.split(',');
		for value in &mut out {
			let part = parts.next().ok_or_else(|| MeshError::Invalid(format!("version 1 vector [{group}]")))?;
			*value = part.trim().parse().map_err(|_| MeshError::Invalid(format!("version 1 number {part:?}")))?;
		}
		if parts.next().is_some() {
			return Err(MeshError::Invalid(format!("version 1 vector [{group}]")));
		}
		Ok(out)
	}

	fn has_more(&self) -> bool {
		self.rest.contains('[')
	}
}
