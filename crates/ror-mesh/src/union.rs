//! Union / CSG graphics mesh data (`PartOperation.MeshData`, `PartOperationAsset.MeshData`).
//!
//! Layouts adapted from krakow10's `rbx_mesh` (MIT/Apache-2.0, used under MIT), see `notes/mesh.md`:
//!
//! - `CSGK` + 32 ASCII hex digits: a key naming server-side data, no geometry.
//! - `CSGMDL` + `u32 version`. The 10 header bytes are always XOR-obfuscated with a 31-byte key
//!   cycled by absolute file offset; versions 2 and 4 obfuscate the whole blob, version 5 only the
//!   header.
//!   - v2: 32-byte hash block, `u32 vertex count`, `u32 vertex size (84)`, vertices,
//!     `u32 index count`, `u32` indices.
//!   - v4: v2 followed by `u32 count` + `u32[count]` (unknown).
//!   - v5: per-attribute arrays with `u16` counts (positions f32x3, normals i16x3, colours u8x4,
//!     NormalIds u8, uvs f32x2, tangents i16x3), then a delta-coded index stream with range markers.

use crate::reader::{Reader, le_f32, le_u16, le_u32, le_vec3};
use crate::{Mesh, MeshError, MeshFormat, check_indices, face_count_u32};

/// The XOR key Roblox applies to CSGMDL data, indexed by `offset % 31`.
const KEY: [u8; 31] = [
	86, 46, 110, 88, 49, 32, 48, 4, 52, 105, 12, 119, 12, 1, 94, 0, 26, 96, 55, 105, 29, 82, 43, 7, 79, 36, 89, 101,
	83, 4, 122,
];
const HEADER_SIZE: usize = 10;

/// Whether `bytes` start like union mesh data (`CSGK`, a plain or obfuscated `CSGMDL`, or an
/// unpublished union's SolidMeshHolder data).
pub(crate) fn is_union_data(bytes: &[u8]) -> bool {
	crate::solid::is_solid(bytes)
		|| bytes.starts_with(b"CSGK")
		|| bytes.starts_with(b"CSGMDL")
		|| (bytes.len() >= 6 && deobfuscated::<6>(bytes, 0) == *b"CSGMDL")
}

fn deobfuscated<const N: usize>(bytes: &[u8], offset: usize) -> [u8; N] {
	std::array::from_fn(|index| bytes[offset + index] ^ KEY[(offset + index) % KEY.len()])
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Mesh, MeshError> {
	if crate::solid::is_solid(bytes) {
		return crate::solid::decode(bytes);
	}
	if bytes.starts_with(b"CSGK") {
		let key = bytes.get(4..36).unwrap_or(&bytes[4..]);
		return Err(MeshError::CsgReference(String::from_utf8_lossy(key).into_owned()));
	}
	if bytes.len() < HEADER_SIZE {
		return Err(MeshError::UnknownHeader(crate::header_text(bytes, 8)));
	}
	let (header, obfuscated) = if bytes.starts_with(b"CSGMDL") {
		(<[u8; HEADER_SIZE]>::try_from(&bytes[..HEADER_SIZE]).unwrap_or_default(), false)
	} else {
		(deobfuscated::<HEADER_SIZE>(bytes, 0), true)
	};
	if &header[..6] != b"CSGMDL" {
		return Err(MeshError::UnknownHeader(crate::header_text(bytes, 8)));
	}
	let version = le_u32(&header, 6);
	let mut mesh = match version {
		2 | 4 if obfuscated => {
			let mut plain = bytes.to_vec();
			for (offset, byte) in plain.iter_mut().enumerate() {
				*byte ^= KEY[offset % KEY.len()];
			}
			decode_v2_v4(&plain[HEADER_SIZE..], version)?
		}
		2 | 4 => decode_v2_v4(&bytes[HEADER_SIZE..], version)?,
		5 => decode_v5(&bytes[HEADER_SIZE..])?,
		_ => return Err(MeshError::UnsupportedVersion(format!("CSGMDL {version}"))),
	};
	mesh.format = MeshFormat::Union { version };
	Ok(mesh)
}

/// Versions 2 and 4 (already deobfuscated, header removed).
///
/// Vertex (84 bytes): position f32x3, normal f32x3, colour u8x4 (RGBA), NormalId u32, uv f32x2,
/// 16 zero bytes, tangent f32x3, 16 zero bytes.
fn decode_v2_v4(body: &[u8], version: u32) -> Result<Mesh, MeshError> {
	let mut reader = Reader::new(body);
	reader.skip(32, "union hash")?;
	let vertex_count = reader.u32("union vertex count")? as usize;
	let vertex_size = reader.u32("union vertex size")? as usize;
	if vertex_size < 68 {
		return Err(MeshError::Invalid(format!("union vertex size {vertex_size}")));
	}
	let vertices = reader.array(vertex_count, vertex_size, "union vertices")?;
	let index_count = reader.u32("union index count")? as usize;
	let indices = reader.array(index_count, 4, "union indices")?;
	if version == 4 {
		let count = reader.u32("union v4 trailer count")? as usize;
		reader.array(count, 4, "union v4 trailer")?;
	}
	if !index_count.is_multiple_of(3) {
		return Err(MeshError::Invalid(format!("union index count {index_count} is not a multiple of 3")));
	}

	let mut mesh = Mesh::default();
	mesh.positions.reserve_exact(vertex_count);
	mesh.normals.reserve_exact(vertex_count);
	mesh.colors.reserve_exact(vertex_count);
	mesh.normal_ids.reserve_exact(vertex_count);
	mesh.uvs.reserve_exact(vertex_count);
	mesh.tangents.reserve_exact(vertex_count);
	for vertex in vertices.chunks_exact(vertex_size) {
		mesh.positions.push(le_vec3(vertex, 0));
		mesh.normals.push(le_vec3(vertex, 12));
		mesh.colors.push([vertex[24], vertex[25], vertex[26], vertex[27]]);
		mesh.normal_ids.push(le_u32(vertex, 28).min(255) as u8);
		mesh.uvs.push([le_f32(vertex, 32), le_f32(vertex, 36)]);
		let [x, y, z] = le_vec3(vertex, 56);
		mesh.tangents.push([x, y, z, 1.0]);
	}
	mesh.indices = indices.chunks_exact(4).map(|index| le_u32(index, 0)).collect();
	finish(mesh, &[])
}

/// Version 5 (header removed, not obfuscated).
fn decode_v5(body: &[u8]) -> Result<Mesh, MeshError> {
	let mut reader = Reader::new(body);
	let position_count = reader.u16("union position count")? as usize;
	let positions = reader.array(position_count, 12, "union positions")?;
	let normal_count = reader.u16("union normal count")? as usize;
	let normals_size = reader.u32("union normals size")? as usize;
	let normals = reader.bytes(normals_size, "union normals")?;
	let color_count = reader.u16("union colour count")? as usize;
	let colors = reader.array(color_count, 4, "union colours")?;
	let normal_id_count = reader.u16("union NormalId count")? as usize;
	let normal_ids = reader.array(normal_id_count, 1, "union NormalIds")?;
	let uv_count = reader.u16("union uv count")? as usize;
	let uvs = reader.array(uv_count, 8, "union uvs")?;
	let tangent_count = reader.u16("union tangent count")? as usize;
	let tangents_size = reader.u32("union tangents size")? as usize;
	let tangents = reader.bytes(tangents_size, "union tangents")?;
	let index_count = reader.u32("union index count")? as usize;
	let index_data = reader.sized("union index data")?;
	let marker_count = reader.u8("union range marker count")? as usize;
	let markers: Vec<u32> =
		reader.array(marker_count, 4, "union range markers")?.chunks_exact(4).map(|marker| le_u32(marker, 0)).collect();
	if reader.remaining() != 0 {
		return Err(MeshError::Invalid(format!("{} bytes after the union index data", reader.remaining())));
	}
	if normal_count * 6 != normals_size || tangent_count * 6 != tangents_size {
		return Err(MeshError::Invalid(format!(
			"union normal/tangent sizes {normals_size}/{tangents_size} for {normal_count}/{tangent_count} values"
		)));
	}

	let vertex_count = position_count;
	for (count, what) in [
		(normal_count, "normals"),
		(color_count, "colours"),
		(normal_id_count, "NormalIds"),
		(uv_count, "uvs"),
		(tangent_count, "tangents"),
	] {
		if count != vertex_count && count != 0 {
			return Err(MeshError::Invalid(format!("{count} union {what} for {vertex_count} positions")));
		}
	}

	let mesh = Mesh {
		positions: positions.chunks_exact(12).map(|value| le_vec3(value, 0)).collect(),
		normals: if normal_count == 0 {
			vec![[0.0; 3]; vertex_count]
		} else {
			normals.chunks_exact(6).map(quantized_unit).collect()
		},
		colors: colors.chunks_exact(4).map(|color| [color[0], color[1], color[2], color[3]]).collect(),
		normal_ids: normal_ids.to_vec(),
		uvs: uvs.chunks_exact(8).map(|uv| [le_f32(uv, 0), le_f32(uv, 4)]).collect(),
		tangents: tangents
			.chunks_exact(6)
			.map(|tangent| {
				let [x, y, z] = quantized_unit(tangent);
				[x, y, z, 1.0]
			})
			.collect(),
		indices: delta_indices(index_data, index_count)?,
		..Mesh::default()
	};
	finish(mesh, &markers)
}

/// A unit vector stored as three `u16` with 32767 meaning zero: `(q - 32767) / 32767`.
fn quantized_unit(bytes: &[u8]) -> [f32; 3] {
	std::array::from_fn(|axis| ((le_u16(bytes, axis * 2) as f32 - 32767.0) / 32767.0).clamp(-1.0, 1.0))
}

/// Decodes the version 5 index stream: each index is the previous one plus a delta coded as
/// - `0xxxxxxx` with bit 6 clear: `+x` (0..=63),
/// - `01xxxxxx`: `x - 128` (-64..=-1),
/// - `1xxxxxxx yyyyyyyy zzzzzzzz`: `+((x << 16) | (y << 8) | z)` modulo 2^23,
///
/// and every index is kept to 23 bits.
fn delta_indices(data: &[u8], count: usize) -> Result<Vec<u32>, MeshError> {
	let (indices, used) = delta_prefix(data, count)?;
	if used != data.len() {
		return Err(MeshError::Invalid("unused bytes after the union index data".into()));
	}
	Ok(indices)
}

/// [`delta_indices`] over the start of `data`, also returning how many bytes the `count` indices
/// took (SolidMesh streams carry no byte size).
pub(crate) fn delta_prefix(data: &[u8], count: usize) -> Result<(Vec<u32>, usize), MeshError> {
	if count > data.len() {
		return Err(MeshError::Truncated("union index data"));
	}
	let mut indices = Vec::with_capacity(count);
	let mut bytes = data.iter().copied();
	let mut index: u32 = 0;
	while indices.len() < count {
		let first = bytes.next().ok_or(MeshError::Truncated("union index data"))?;
		if first & 0x80 == 0 {
			if first & 0x40 == 0 {
				index = index.wrapping_add(first as u32);
			} else {
				index = index.wrapping_sub(128 - first as u32);
			}
		} else {
			let second = bytes.next().ok_or(MeshError::Truncated("union index data"))?;
			let third = bytes.next().ok_or(MeshError::Truncated("union index data"))?;
			index = index.wrapping_add(((first as u32 & 0x7F) << 16) | ((second as u32) << 8) | third as u32);
		}
		index &= 0x7F_FFFF;
		indices.push(index);
	}
	let used = data.len() - bytes.len();
	Ok((indices, used))
}

/// Checks the triangles and builds the LODs from version 5 range markers (index offsets:
/// `markers[i]..markers[i + 1]` is level `i`; no markers means one level).
fn finish(mut mesh: Mesh, markers: &[u32]) -> Result<Mesh, MeshError> {
	check_indices(&mesh.indices, mesh.positions.len())?;
	let index_count = mesh.indices.len() as u32;
	face_count_u32(mesh.indices.len() / 3)?;
	if markers.len() < 2 {
		mesh.lods = crate::single_lod(index_count);
		return Ok(mesh);
	}
	for pair in markers.windows(2) {
		if pair[1] < pair[0] || pair[1] > index_count || !pair[0].is_multiple_of(3) || !pair[1].is_multiple_of(3) {
			return Err(MeshError::Invalid(format!("union range markers {markers:?} for {index_count} indices")));
		}
	}
	mesh.lods = markers.windows(2).map(|pair| pair[0]..pair[1]).filter(|range| !range.is_empty()).collect();
	if mesh.lods.is_empty() && index_count > 0 {
		mesh.lods = crate::single_lod(index_count);
	}
	Ok(mesh)
}
