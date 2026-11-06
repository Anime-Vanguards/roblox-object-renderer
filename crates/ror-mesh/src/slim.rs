//! `SLIM` / `SLMD` cache files: Roblox's SLIM ("Scalable Lightweight Interactive Models")
//! representations — cloud-generated, composited LOD versions of streamed-out models and platform
//! avatars. They are not what Studio renders (it shows the full models); this module exists so the
//! cache can be read and the embedded meshes inspected.
//!
//! A file is a sequence of records: `[u8; 4]` magic (`SLIM` v3 or `SLMD` v2), `u32 payload size`,
//! `u32 version`, `u32 0`, then a property-bag payload:
//!
//! - `u32 n`, `u16 class[n]` (2 mesh, 3 material, 4 root; 0 = SLMD LOD entry)
//! - property ids per object: `u32 n`, `u32 end[n]`, `u32 total`, `u16 id[total]`
//! - value index per property: `u32 total`, `u32 index[total]` (element / item index in the
//!   property's column; which column a property uses is implied by its id)
//! - nine value columns: 0 bytes, 1 (empty), 2 four-byte scalars (f32/u32), 3 (empty), 4 byte
//!   arrays (strings, blobs), 5 and 6 (empty), 7 four-byte arrays (f32), 8 eight-byte arrays.
//!   Scalar columns are `u32 count` + values; array columns `u32 count`, `u32 end[count]`,
//!   `u32 total units`, units.
//!
//! SLIM files are `(mesh, material)` record pairs (1 to 4) followed by one root record. Mesh
//! records hold a complete FileMesh (`version 7.00`, `COREMESH` + `LODS`; prop 15) and an AABB
//! (prop 17, six f32: the source bounds, not always the mesh's own); materials a name (prop 7), a
//! Color3uint8 (prop 18) and `rbxassetid://` texture references (props 3, 8, 9, 11, 12, 13; slot
//! meanings unknown); the root one 12 f32 per pair (rotation then translation, identity in every
//! observed file; prop 25). An `SLMD` file is one `SLMD` record (an LOD manifest) followed by the
//! finest LOD as a SLIM model; other LODs are separate SLIM files. See `notes/mesh.md`.

use crate::reader::{Reader, le_f32};
use crate::{Mesh, MeshError, MeshFormat};

pub(crate) const MAGIC: &[u8] = b"SLIM";
pub(crate) const MANIFEST_MAGIC: &[u8] = b"SLMD";

const CLASS_MESH: u16 = 2;
const CLASS_MATERIAL: u16 = 3;
const CLASS_ROOT: u16 = 4;
const PROP_MESH: u16 = 15;
const PROP_BOUNDS: u16 = 17;
const PROP_MATERIAL_NAME: u16 = 7;
const PROP_MATERIAL_COLOR: u16 = 18;
const PROP_MATERIAL_TEXTURES: [u16; 6] = [3, 8, 9, 11, 12, 13];
const PROP_TRANSFORMS: u16 = 25;

/// A decoded SLIM model.
#[derive(Clone, Debug, Default)]
pub struct Slim {
	/// Mesh parts in file order, each with the material record that follows it.
	pub parts: Vec<SlimPart>,
	/// Per part: rotation (row-major 3x3) then translation, as the root record stores them.
	pub transforms: Vec<[f32; 12]>,
	/// LOD entries listed by an `SLMD` manifest (0 for a plain SLIM file).
	pub manifest_lods: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SlimPart {
	pub mesh: Mesh,
	/// The mesh record's AABB (min, max).
	pub bounds: Option<([f32; 3], [f32; 3])>,
	pub material: Option<SlimMaterial>,
}

#[derive(Clone, Debug, Default)]
pub struct SlimMaterial {
	pub name: Option<String>,
	pub color: Option<[u8; 3]>,
	/// `rbxassetid://` texture references in property order.
	pub textures: Vec<String>,
}

/// Decodes a `SLIM` or `SLMD` file.
pub fn decode_slim(bytes: &[u8]) -> Result<Slim, MeshError> {
	let mut slim = Slim::default();
	let mut reader = Reader::new(bytes);
	while reader.remaining() > 0 {
		let magic = reader.fixed::<4>("SLIM record magic")?;
		let size = reader.u32("SLIM record size")? as usize;
		let _version = reader.u32("SLIM record version")?;
		reader.skip(4, "SLIM record header")?;
		let payload = reader.bytes(size, "SLIM record payload")?;
		let record = Record::parse(payload)?;
		match &magic[..] {
			MANIFEST_MAGIC => slim.manifest_lods += record.objects.len(),
			MAGIC => record.collect(&mut slim)?,
			_ => return Err(MeshError::Invalid(format!("unknown SLIM record {:?}", String::from_utf8_lossy(&magic)))),
		}
	}
	Ok(slim)
}

/// Decodes the parts of a `SLIM` / `SLMD` file into one mesh (part transforms applied, LOD0 of
/// each part concatenated).
pub(crate) fn decode(bytes: &[u8]) -> Result<Mesh, MeshError> {
	let slim = decode_slim(bytes)?;
	if slim.parts.is_empty() {
		return Err(MeshError::Invalid("SLIM file without meshes".into()));
	}
	let draco = slim.parts.iter().all(|part| matches!(part.mesh.format, MeshFormat::Chunked { draco: true, .. }));
	let mut merged = Mesh { format: MeshFormat::Slim { version: 3, draco }, ..Mesh::default() };
	for (index, part) in slim.parts.iter().enumerate() {
		append(&mut merged, &part.mesh, slim.transforms.get(index))?;
	}
	let count = merged.indices.len() as u32;
	merged.lods = crate::single_lod(count);
	Ok(merged)
}

fn append(merged: &mut Mesh, part: &Mesh, transform: Option<&[f32; 12]>) -> Result<(), MeshError> {
	let base = u32::try_from(merged.positions.len()).map_err(|_| MeshError::Invalid("SLIM model too large".into()))?;
	let count = part.positions.len();
	let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
	let transform = transform.filter(|transform| **transform != identity);
	let rotate = |v: [f32; 3]| match transform {
		Some(t) => [
			t[0] * v[0] + t[1] * v[1] + t[2] * v[2],
			t[3] * v[0] + t[4] * v[1] + t[5] * v[2],
			t[6] * v[0] + t[7] * v[1] + t[8] * v[2],
		],
		None => v,
	};
	merged.positions.extend(part.positions.iter().map(|&p| {
		let r = rotate(p);
		match transform {
			Some(t) => [r[0] + t[9], r[1] + t[10], r[2] + t[11]],
			None => r,
		}
	}));
	merged.normals.extend(part.normals.iter().map(|&n| rotate(n)));
	// Optional arrays stay aligned with the positions: fill parts that lack them.
	extend_or_fill(&mut merged.uvs, &part.uvs, base as usize, count, [0.0; 2]);
	let tangents: Vec<[f32; 4]> = part
		.tangents
		.iter()
		.map(|t| {
			let r = rotate([t[0], t[1], t[2]]);
			[r[0], r[1], r[2], t[3]]
		})
		.collect();
	extend_or_fill(&mut merged.tangents, &tangents, base as usize, count, [0.0, 0.0, 0.0, 1.0]);
	extend_or_fill(&mut merged.colors, &part.colors, base as usize, count, [255; 4]);
	merged.indices.extend(part.lod0().iter().map(|&index| index + base));
	Ok(())
}

fn extend_or_fill<T: Copy>(target: &mut Vec<T>, source: &[T], before: usize, count: usize, fill: T) {
	if source.len() == count && count > 0 {
		if target.len() < before {
			target.resize(before, fill);
		}
		target.extend_from_slice(source);
	} else if !target.is_empty() {
		target.resize(before + count, fill);
	}
}

struct Object {
	class: u16,
	/// (property id, value index)
	props: Vec<(u16, u32)>,
}

struct Record<'a> {
	objects: Vec<Object>,
	bytes: &'a [u8],
	floats: &'a [u8],
	blobs: Vec<&'a [u8]>,
	float_arrays: Vec<&'a [u8]>,
}

impl<'a> Record<'a> {
	fn parse(payload: &'a [u8]) -> Result<Self, MeshError> {
		let mut reader = Reader::new(payload);
		let object_count = reader.u32("SLIM object count")? as usize;
		let classes = reader.array(object_count, 2, "SLIM classes")?;
		let property_lists = array_column(&mut reader, 2, "SLIM property lists")?;
		if property_lists.len() != object_count {
			return Err(MeshError::Invalid("SLIM property lists do not match the objects".into()));
		}
		let index_count = reader.u32("SLIM value index count")? as usize;
		let indices = reader.array(index_count, 4, "SLIM value index")?;

		let mut objects = Vec::with_capacity(object_count);
		let mut next = 0usize;
		for (class, list) in classes.chunks_exact(2).zip(&property_lists) {
			let mut props = Vec::with_capacity(list.len() / 2);
			for id in list.chunks_exact(2) {
				let Some(index) = indices.get(next * 4..next * 4 + 4) else {
					return Err(MeshError::Invalid("SLIM value index shorter than the property lists".into()));
				};
				props.push((u16::from_le_bytes([id[0], id[1]]), crate::reader::le_u32(index, 0)));
				next += 1;
			}
			objects.push(Object { class: u16::from_le_bytes([class[0], class[1]]), props });
		}
		if next != index_count {
			return Err(MeshError::Invalid("SLIM value index longer than the property lists".into()));
		}

		let bytes = scalar_column(&mut reader, 1, "SLIM column 0")?;
		empty_column(&mut reader, "SLIM column 1")?;
		let floats = scalar_column(&mut reader, 4, "SLIM column 2")?;
		empty_column(&mut reader, "SLIM column 3")?;
		let blobs = array_column(&mut reader, 1, "SLIM column 4")?;
		empty_column(&mut reader, "SLIM column 5")?;
		empty_column(&mut reader, "SLIM column 6")?;
		let float_arrays = array_column(&mut reader, 4, "SLIM column 7")?;
		array_column(&mut reader, 8, "SLIM column 8")?;
		if reader.remaining() != 0 {
			return Err(MeshError::Invalid(format!("{} unparsed bytes in a SLIM record", reader.remaining())));
		}
		Ok(Self { objects, bytes, floats, blobs, float_arrays })
	}

	fn collect(&self, slim: &mut Slim) -> Result<(), MeshError> {
		for object in &self.objects {
			match object.class {
				CLASS_MESH => {
					let mut part = SlimPart::default();
					let mut mesh = None;
					for &(id, index) in &object.props {
						match id {
							PROP_MESH => {
								let mut decoded = crate::filemesh::decode(self.blob(index)?)?;
								crate::finalize(&mut decoded);
								mesh = Some(decoded);
							}
							PROP_BOUNDS => {
								let value = |k: u32| self.float(index + k);
								part.bounds =
									Some(([value(0)?, value(1)?, value(2)?], [value(3)?, value(4)?, value(5)?]));
							}
							_ => {}
						}
					}
					part.mesh = mesh.ok_or_else(|| MeshError::Invalid("SLIM mesh record without a mesh".into()))?;
					slim.parts.push(part);
				}
				CLASS_MATERIAL => {
					let mut material = SlimMaterial::default();
					for &(id, index) in &object.props {
						if id == PROP_MATERIAL_NAME {
							material.name = Some(String::from_utf8_lossy(self.blob(index)?).into_owned());
						} else if id == PROP_MATERIAL_COLOR {
							let start = index as usize;
							let Some(rgb) = self.bytes.get(start..start + 3) else {
								return Err(MeshError::Invalid("SLIM colour index out of range".into()));
							};
							material.color = Some([rgb[0], rgb[1], rgb[2]]);
						} else if PROP_MATERIAL_TEXTURES.contains(&id) {
							material.textures.push(String::from_utf8_lossy(self.blob(index)?).into_owned());
						}
					}
					if let Some(part) = slim.parts.iter_mut().rev().find(|part| part.material.is_none()) {
						part.material = Some(material);
					}
				}
				CLASS_ROOT => {
					for &(id, index) in &object.props {
						if id != PROP_TRANSFORMS {
							continue;
						}
						let Some(values) = self.float_arrays.get(index as usize) else {
							return Err(MeshError::Invalid("SLIM transform index out of range".into()));
						};
						for transform in values.chunks_exact(48) {
							slim.transforms.push(std::array::from_fn(|k| le_f32(transform, k * 4)));
						}
					}
				}
				_ => {}
			}
		}
		Ok(())
	}

	fn blob(&self, index: u32) -> Result<&'a [u8], MeshError> {
		self.blobs
			.get(index as usize)
			.copied()
			.ok_or_else(|| MeshError::Invalid(format!("SLIM blob index {index} out of range")))
	}

	fn float(&self, index: u32) -> Result<f32, MeshError> {
		let at = index as usize * 4;
		if at + 4 > self.floats.len() {
			return Err(MeshError::Invalid(format!("SLIM float index {index} out of range")));
		}
		Ok(le_f32(self.floats, at))
	}
}

fn scalar_column<'a>(reader: &mut Reader<'a>, size: usize, what: &'static str) -> Result<&'a [u8], MeshError> {
	let count = reader.u32(what)? as usize;
	reader.array(count, size, what)
}

fn empty_column(reader: &mut Reader<'_>, what: &'static str) -> Result<(), MeshError> {
	match reader.u32(what)? {
		0 => Ok(()),
		count => Err(MeshError::Invalid(format!("{what}: {count} values of unknown size"))),
	}
}

/// `u32 count`, `u32 end[count]` (exclusive end of each item, in units), `u32 total`, units.
fn array_column<'a>(reader: &mut Reader<'a>, unit: usize, what: &'static str) -> Result<Vec<&'a [u8]>, MeshError> {
	let count = reader.u32(what)? as usize;
	let ends = reader.array(count, 4, what)?;
	let total = reader.u32(what)? as usize;
	let data = reader.array(total, unit, what)?;
	let mut items = Vec::with_capacity(count);
	let mut start = 0usize;
	for end in ends.chunks_exact(4) {
		let end = crate::reader::le_u32(end, 0) as usize;
		if end < start || end > total {
			return Err(MeshError::Invalid(format!("{what}: item end {end} outside {start}..={total}")));
		}
		items.push(&data[start * unit..end * unit]);
		start = end;
	}
	Ok(items)
}
