//! `COREMESH` v2 (FileMesh 7.00): `u32 size` + a Google Draco mesh bitstream.
//!
//! Every Roblox stream measured (1,892 of them) is bitstream 2.2, sequential connectivity (so
//! vertex and face order match the file's `LODS` and `SKINNING` chunks), no metadata, and five
//! attributes with identity point mapping:
//!
//! | unique id | Draco type | data | Roblox meaning |
//! |---|---|---|---|
//! | 0 | POSITION | f32 x3 | position |
//! | 1 | GENERIC | f32 x3 | normal |
//! | 2 | TEX_COORD | f32 x2 | uv |
//! | 3 | GENERIC | u8 x4 | tangent, bytes as in the raw vertex: `(b - 127) / 127` |
//! | 4 | COLOR | u8 x4 | RGBA colour |
//!
//! Attributes are matched by type and shape rather than by id, so a stream that uses Draco's
//! `NORMAL` type for normals decodes too.

use draco_core::{DataType, DecoderBuffer, GeometryAttributeType, MeshDecoder, PointAttribute, PointIndex};

use crate::reader::{Reader, le_f32, le_vec3};
use crate::{Mesh, MeshError, decode_tangent};

pub(crate) fn decode_core_v2(data: &[u8], mesh: &mut Mesh) -> Result<(), MeshError> {
	let stream = Reader::new(data).sized("Draco bitstream")?;
	let mut decoded = draco_core::Mesh::new();
	MeshDecoder::new()
		.decode(&mut DecoderBuffer::new(stream), &mut decoded)
		.map_err(|error| MeshError::Draco(error.to_string()))?;

	let point_count = decoded.num_points();
	let mut position = None;
	let mut normal = None;
	let mut uv = None;
	let mut tangent = None;
	let mut color = None;
	for id in 0..decoded.num_attributes() {
		let attribute = decoded.attribute(id);
		let shape = (attribute.attribute_type(), attribute.data_type(), attribute.num_components());
		let slot = match shape {
			(GeometryAttributeType::Position, DataType::Float32, 3) => &mut position,
			(GeometryAttributeType::Normal | GeometryAttributeType::Generic, DataType::Float32, 3) => &mut normal,
			(GeometryAttributeType::TexCoord, DataType::Float32, 2) => &mut uv,
			(GeometryAttributeType::Generic, DataType::Uint8, 4) => &mut tangent,
			(GeometryAttributeType::Color, DataType::Uint8, 4) => &mut color,
			_ => continue,
		};
		if slot.is_none() {
			*slot = Some(attribute);
		}
	}

	let position = position.ok_or_else(|| MeshError::Draco("no position attribute".into()))?;
	mesh.positions = values(position, point_count, |bytes: &[u8; 12]| le_vec3(bytes, 0))?;
	mesh.normals = match normal {
		Some(normal) => values(normal, point_count, |bytes: &[u8; 12]| le_vec3(bytes, 0))?,
		None => vec![[0.0; 3]; point_count],
	};
	if let Some(uv) = uv {
		mesh.uvs = values(uv, point_count, |bytes: &[u8; 8]| [le_f32(bytes, 0), le_f32(bytes, 4)])?;
	}
	if let Some(tangent) = tangent {
		mesh.tangents = values(tangent, point_count, |bytes: &[u8; 4]| decode_tangent(*bytes))?;
	}
	if let Some(color) = color {
		mesh.colors = values(color, point_count, |bytes: &[u8; 4]| *bytes)?;
	}

	let faces = decoded.faces();
	mesh.indices = vec![0; faces.len() * 3];
	for (out, face) in mesh.indices.chunks_exact_mut(3).zip(faces) {
		out[0] = face[0].0;
		out[1] = face[1].0;
		out[2] = face[2].0;
	}
	Ok(())
}

/// Converts the first `S` bytes of each point's value, following the attribute's point-to-value
/// map (a straight pass over tightly packed values in the usual identity-mapped case).
fn values<const S: usize, T>(
	attribute: &PointAttribute,
	point_count: usize,
	convert: impl Fn(&[u8; S]) -> T,
) -> Result<Vec<T>, MeshError> {
	let stride = attribute.byte_stride() as usize;
	let data = attribute.buffer().data();
	if stride < S {
		return Err(MeshError::Draco(format!("attribute stride {stride} for {S}-byte values")));
	}
	let value_count = attribute.size().min(data.len() / stride);
	let short = || MeshError::Draco(format!("{value_count} attribute values for {point_count} points"));
	if attribute.is_mapping_identity() {
		if value_count < point_count {
			return Err(short());
		}
		if stride == S {
			return Ok(data[..point_count * S].as_chunks::<S>().0.iter().map(convert).collect());
		}
		return data
			.chunks_exact(stride)
			.take(point_count)
			.map(|value| value.first_chunk::<S>().map(&convert))
			.collect::<Option<_>>()
			.ok_or_else(short);
	}
	(0..point_count)
		.map(|point| {
			let index = attribute.mapped_index(PointIndex(point as u32)).0 as usize;
			if index >= value_count {
				return Err(MeshError::Draco(format!(
					"point {point} maps to attribute value {index} of {value_count}"
				)));
			}
			data[index * stride..].first_chunk::<S>().map(&convert).ok_or_else(short)
		})
		.collect()
}
