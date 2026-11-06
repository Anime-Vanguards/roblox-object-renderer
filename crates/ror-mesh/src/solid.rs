//! Unpublished union meshes: `PartOperation.SolidMeshHolder`, a NetAssetRef whose shared string
//! holds the mesh inline until the place is published (the mesh then moves to an asset named by
//! `PartOperation.Content`). Worked out from unions built in a Studio session (a two-colour
//! box, the same box minus a cylinder: notes/mesh.md "SolidMesh"):
//!
//! - header: `u8 3, u8 1, u32 size, u32 0, u32 size` (size = the bytes after the header),
//!   `"SolidMesh"`, `u32 0`;
//! - four attribute arrays, each a `u64` count and the values: positions `f32x3` (studs, relative to
//!   the union at InitialSize), normals `f32x3`, one array never seen non-empty, colours `f32x3`
//!   (0..1, the source parts' colours);
//! - four index streams, one per attribute and one entry per triangle corner: a `u64` count and
//!   indices delta-coded as in CSGMDL 5 (`union::delta_prefix`).

use std::collections::HashMap;

use crate::reader::{Reader, le_vec3};
use crate::{Mesh, MeshError, MeshFormat, check_indices, face_count_u32};

const HEADER_SIZE: usize = 14;
const NAME: &[u8; 9] = b"SolidMesh";

/// Whether `bytes` start like SolidMeshHolder data.
pub(crate) fn is_solid(bytes: &[u8]) -> bool {
	bytes.len() >= HEADER_SIZE + NAME.len() && bytes[0] == 3 && bytes[1] == 1 && &bytes[HEADER_SIZE..HEADER_SIZE + NAME.len()] == NAME
}

fn count(reader: &mut Reader<'_>, what: &'static str) -> Result<usize, MeshError> {
	let value = u64::from_le_bytes(reader.fixed(what)?);
	usize::try_from(value).ok().filter(|&count| count <= reader.remaining()).ok_or(MeshError::Truncated(what))
}

fn vectors(reader: &mut Reader<'_>, what: &'static str) -> Result<Vec<[f32; 3]>, MeshError> {
	let count = count(reader, what)?;
	Ok(reader.array(count, 12, what)?.chunks_exact(12).map(|value| le_vec3(value, 0)).collect())
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Mesh, MeshError> {
	let mut reader = Reader::new(bytes);
	let version = reader.u8("SolidMesh version")?;
	reader.u8("SolidMesh kind")?;
	let size = reader.u32("SolidMesh size")? as usize;
	let flags = reader.u32("SolidMesh flags")?;
	let stored = reader.u32("SolidMesh stored size")? as usize;
	if flags != 0 || size != stored {
		return Err(MeshError::UnsupportedVersion(format!("SolidMesh with flags {flags} ({size} / {stored} bytes)")));
	}
	if size != reader.remaining() {
		return Err(MeshError::Invalid(format!("SolidMesh size {size} for {} bytes", reader.remaining())));
	}
	reader.skip(NAME.len(), "SolidMesh name")?;
	reader.u32("SolidMesh format")?;
	let positions = vectors(&mut reader, "SolidMesh positions")?;
	let normals = vectors(&mut reader, "SolidMesh normals")?;
	if count(&mut reader, "SolidMesh third array")? != 0 {
		return Err(MeshError::UnsupportedVersion("SolidMesh with a third attribute array".into()));
	}
	let colors = vectors(&mut reader, "SolidMesh colours")?;
	let mut streams = Vec::with_capacity(4);
	for what in ["SolidMesh position indices", "SolidMesh normal indices", "SolidMesh third indices", "SolidMesh colour indices"] {
		let entries = count(&mut reader, what)?;
		let rest = reader.bytes(reader.remaining(), what)?;
		let (indices, used) = crate::union::delta_prefix(rest, entries)?;
		reader = Reader::new(&rest[used..]);
		streams.push(indices);
	}
	if reader.remaining() != 0 {
		return Err(MeshError::Invalid(format!("{} bytes after the SolidMesh indices", reader.remaining())));
	}
	let corners = streams[0].len();
	if !corners.is_multiple_of(3) || streams[1].len() != corners || (streams[3].len() != corners && !streams[3].is_empty()) {
		return Err(MeshError::Invalid(format!("SolidMesh index streams of {:?} entries", streams.iter().map(Vec::len).collect::<Vec<_>>())));
	}
	face_count_u32(corners / 3)?;

	// one vertex per distinct (position, normal, colour) corner
	let mut mesh = Mesh { format: MeshFormat::Solid { version }, ..Mesh::default() };
	let mut seen: HashMap<(u32, u32, u32), u32> = HashMap::new();
	mesh.indices.reserve(corners);
	for corner in 0..corners {
		let key = (streams[0][corner], streams[1][corner], streams[3].get(corner).copied().unwrap_or(u32::MAX));
		let position = *positions.get(key.0 as usize).ok_or_else(|| MeshError::Invalid(format!("SolidMesh position {} of {}", key.0, positions.len())))?;
		let normal = *normals.get(key.1 as usize).ok_or_else(|| MeshError::Invalid(format!("SolidMesh normal {} of {}", key.1, normals.len())))?;
		let color = match colors.get(key.2 as usize) {
			Some(color) => color.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8),
			None if key.2 == u32::MAX => [255; 3],
			None => return Err(MeshError::Invalid(format!("SolidMesh colour {} of {}", key.2, colors.len()))),
		};
		let next = mesh.positions.len() as u32;
		let index = *seen.entry(key).or_insert_with(|| {
			mesh.positions.push(position);
			mesh.normals.push(normal);
			mesh.colors.push([color[0], color[1], color[2], 255]);
			next
		});
		mesh.indices.push(index);
	}
	check_indices(&mesh.indices, mesh.positions.len())?;
	mesh.lods = crate::single_lod(mesh.indices.len() as u32);
	Ok(mesh)
}
