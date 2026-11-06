//! Roblox mesh decoding.
//!
//! - FileMesh assets (`MeshPart.MeshId`, `SpecialMesh.MeshId`): versions 1.00, 1.01, 2.00, 3.00,
//!   3.01, 4.00, 4.01, 5.00, 6.00 and 7.00 (chunked; `COREMESH` v2 is Draco-compressed).
//! - Union / CSG mesh data (`PartOperation.MeshData` / `MeshData2`, the asset behind
//!   `PartOperation.Content` / `AssetId`, or an unpublished union's `SolidMeshHolder`).
//! - `SLIM` / `SLMD` cache files (Roblox's cloud-generated LOD representations of streamed-out
//!   models), which embed FileMeshes; see [`decode_slim`].
//!
//! Everything decodes into one [`Mesh`]: positions, normals, UVs, tangents and vertex colours, the
//! triangle list of every LOD, and skinning data when present. Decoding is pure (no I/O) and never
//! panics on malformed input; it returns a [`MeshError`].
//!
//! Conventions, verified on Roblox's own data (`notes/mesh.md` has the evidence):
//!
//! - Positions are in studs, object space, as stored, except that version 1.00 positions are
//!   halved: Roblox's own 7.00 re-encodes of 1.00 meshes match the halved geometry exactly.
//! - UVs are returned exactly as stored. For versions 2.00+ (and unions) the stored UVs have a
//!   top-left origin (V grows downwards in the image; Roblox's clothing compositing mesh maps the
//!   shirt template's torso front to pixels 231..359 x 74..202 from the top), which is what
//!   wgpu/D3D/Vulkan sample with: use them unchanged. Version 1.x text meshes store V from the
//!   bottom; Roblox uses `1 - v` (its 7.00 re-encode of a 1.00 mesh stores exactly `1 - v`) — see
//!   [`Mesh::uv_v_flipped`].
//! - Triangles are counter-clockwise seen from the front (the side the normals point to), in
//!   Roblox's right-handed, Y-up space (99.86% of 4.3M measured triangles).
//! - Normals are normalised on decode (old exporters, SLIM and Draco quantization leave some
//!   unnormalised); normals shorter than 0.01 (degenerate in the file) become zero vectors.
//! - Tangents come from the file's four bytes per vertex, each mapped `(b - 127) / 127` (not as
//!   `i8`); `w` = ±1. Measured on 3M triangles: `tangent` follows +dP/du and
//!   `w * cross(normal, tangent)` follows -dP/dv of the stored UVs, i.e. points up in the image
//!   (OpenGL-style, green-up normal maps).

use std::ops::Range;

mod chunked;
mod draco;
mod filemesh;
pub mod rbx_storage;
mod rbxm;
mod reader;
mod skinning;
mod slim;
mod solid;
mod union;
pub mod validate;

pub use chunked::Chunk;
pub use slim::{Slim, SlimMaterial, SlimPart, decode_slim};

/// Lists the chunks of a chunked (`version 6.00` / `version 7.00`) FileMesh, for diagnostics.
pub fn file_chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>, MeshError> {
	match bytes.get(..13) {
		Some(b"version 6.00\n") | Some(b"version 7.00\n") => chunked::chunks(&bytes[13..]),
		_ => Err(MeshError::UnknownHeader(header_text(bytes, 12))),
	}
}

/// A decoded mesh. All per-vertex arrays have `vertex_count()` entries, except that `tangents`,
/// `colors`, `uvs` and `normal_ids` may be empty when the format does not store them.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
	/// The format the data was decoded from.
	pub format: MeshFormat,
	/// Object-space positions, in studs, as the file stores them (version 1.00 halved, see notes).
	/// Union meshes are centred on the union's origin; scale them by `Size / InitialSize`.
	pub positions: Vec<[f32; 3]>,
	/// Unit normals (normalised on decode); zero-length where the file's normal is degenerate.
	pub normals: Vec<[f32; 3]>,
	/// Texture coordinates as stored in the file (no flipping applied). Top-left origin except for
	/// version 1.x meshes, see [`Mesh::uv_v_flipped`].
	pub uvs: Vec<[f32; 2]>,
	/// Tangent xyz + bitangent sign `w` (±1), decoded from the file's four bytes per vertex as
	/// `(b - 127) / 127`; empty when absent. Pre-PBR meshes (mostly 2.00) store zero bytes, which
	/// decode to `(-1, -1, -1, -1)`: check [`validate::tangents_usable`] before relying on them.
	/// Union meshes store no sign; `w` is `1.0` there.
	pub tangents: Vec<[f32; 4]>,
	/// RGBA vertex colours as stored; empty when absent (2.00 files with 36-byte vertices, version
	/// 1.x). Unions store the source parts' colours (BrickColor RGB values).
	pub colors: Vec<[u8; 4]>,
	/// Triangle indices of every LOD, concatenated in file order.
	pub indices: Vec<u32>,
	/// Index ranges into `indices` per level of detail; `lods[0]` is the highest detail. Always at
	/// least one entry when the mesh has triangles. A file LOD table whose first level is empty
	/// (`[0, 0]`, written by Roblox for meshes without LODs) yields one level with every triangle.
	pub lods: Vec<Range<u32>>,
	/// The file's LOD generator id (versions 4.00+): 0 none, 1 unknown, 2 RbxSimplifier, 3 Zeux
	/// meshoptimizer; 4 (most current assets) and 5 (SLIM meshes) are unnamed in public sources.
	/// Informational.
	pub lod_type: u16,
	/// The file's `numHighQualityLODs` (versions 4.00+). Informational.
	pub high_quality_lods: u8,
	/// Bones, per-vertex joints and weights, and subsets (versions 4.00+ with bones).
	pub skinning: Option<Skinning>,
	/// Raw FACS (dynamic head) data, uninterpreted (versions 5.00+): the `FileMeshFacsData` blob
	/// (five size fields, bone and control name blobs, quantized transforms, correctives).
	pub facs: Option<Vec<u8>>,
	/// `HSRAVIS` chunk (versions 6.00+): the stored bit flags, one per triangle, marking triangles
	/// hidden-surface removal must never cull. Bit order unverified (no cached file has the chunk;
	/// the community spec calls it disabled).
	pub hsr_always_visible: Option<Vec<u8>>,
	/// Union meshes only: per-vertex face of the source part the triangle came from, as stored
	/// (Roblox `Enum.NormalId` value + 1: 1 Right/+X, 2 Top/+Y, 3 Back/+Z, 4 Left/-X, 5 Bottom/-Y,
	/// 6 Front/-Z). Empty for FileMeshes.
	pub normal_ids: Vec<u8>,
}

impl Mesh {
	pub fn vertex_count(&self) -> usize {
		self.positions.len()
	}

	/// The highest-detail triangle list.
	pub fn lod0(&self) -> &[u32] {
		match self.lods.first() {
			Some(range) => self.indices.get(range.start as usize..range.end as usize).unwrap_or(&[]),
			None => &self.indices,
		}
	}

	/// Axis-aligned bounds of the positions referenced by the highest-detail LOD.
	pub fn lod0_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
		let mut min = [f32::INFINITY; 3];
		let mut max = [f32::NEG_INFINITY; 3];
		let mut any = false;
		for &index in self.lod0() {
			let Some(position) = self.positions.get(index as usize) else {
				continue;
			};
			any = true;
			for axis in 0..3 {
				min[axis] = min[axis].min(position[axis]);
				max[axis] = max[axis].max(position[axis]);
			}
		}
		any.then_some((min, max))
	}

	/// Whether the stored V coordinates have a bottom-left origin (version 1.x text meshes), i.e. the
	/// renderer must sample with `1 - v` to match what Roblox shows. False for every other format.
	pub fn uv_v_flipped(&self) -> bool {
		matches!(self.format, MeshFormat::V1 { .. })
	}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MeshFormat {
	#[default]
	Unknown,
	/// `version 1.00` / `version 1.01` text meshes
	V1 {
		minor: u8,
	},
	V2,
	V3 {
		minor: u8,
	},
	V4 {
		minor: u8,
	},
	V5,
	/// Chunked `version 6.00` / `version 7.00`; `draco` when COREMESH is the compressed (v2) chunk
	Chunked {
		major: u8,
		draco: bool,
	},
	/// Union / CSG graphics mesh data (`CSGMDL` versions 2, 4, 5)
	Union {
		version: u32,
	},
	/// An unpublished union's `SolidMeshHolder` data (`SolidMesh`, header version 3)
	Solid {
		version: u8,
	},
	/// The parts of a `SLIM` / `SLMD` file merged into one mesh (`version` is the SLIM record
	/// version, 3); `draco` when every part's COREMESH is Draco. [`decode_slim`] keeps the parts and
	/// their materials apart.
	Slim {
		version: u32,
		draco: bool,
	},
}

#[derive(Clone, Debug, Default)]
pub struct Skinning {
	pub bones: Vec<Bone>,
	/// Per vertex: up to four bone indices into `bones` (already resolved through subsets). Unused
	/// slots (zero weight) hold 0.
	pub joints: Vec<[u16; 4]>,
	/// Per vertex: the four weights, normalised to sum to 1 (0 for unused slots). A vertex the file
	/// gives no weight at all keeps four zeros: it is not deformed by any bone (render it rigidly
	/// with the part).
	pub weights: Vec<[f32; 4]>,
	pub subsets: Vec<Subset>,
}

#[derive(Clone, Debug, Default)]
pub struct Bone {
	pub name: String,
	/// Index of the parent bone, `None` for a root (stored as 0xFFFF). Not range-checked; see
	/// [`validate`].
	pub parent: Option<u16>,
	pub lod_parent: Option<u16>,
	/// Distance to the farthest vertex the bone influences (used by Roblox for culling bounds).
	pub culling: f32,
	/// 3x3 rotation of the bone's bind CFrame, as stored: `rotation[r][c]` is `R{r}{c}` of
	/// `CFrame.new(x, y, z, R00, R01, R02, R10, R11, R12, R20, R21, R22)`, so the columns are the
	/// bone's right, up and back vectors in mesh space.
	pub rotation: [[f32; 3]; 3],
	/// Position of the bone's bind CFrame, in mesh space (studs).
	pub position: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct Subset {
	/// Triangle range (in faces, not indices) the subset covers.
	pub faces: Range<u32>,
	/// Vertex range whose joints index this subset's `bone_indices`.
	pub vertices: Range<u32>,
	/// Up to 26 bone indices this subset's vertices refer to.
	pub bone_indices: Vec<u16>,
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
	#[error("not a Roblox mesh (header {0:?})")]
	UnknownHeader(String),
	#[error("unsupported mesh version {0}")]
	UnsupportedVersion(String),
	#[error("truncated mesh data: {0}")]
	Truncated(&'static str),
	#[error("invalid mesh data: {0}")]
	Invalid(String),
	#[error("draco: {0}")]
	Draco(String),
	/// Union mesh data that is only a `CSGK` key (32 hex digits) naming server-side geometry. The
	/// mesh is in the asset behind `PartOperation.AssetId` (a model whose `PartOperationAsset` holds
	/// the `CSGMDL` data); pass that asset to [`decode_union`].
	#[error("union mesh data is a CSGK reference ({0}); decode the asset behind PartOperation.AssetId instead")]
	CsgReference(String),
	/// A model passed to [`decode_union`] has no non-empty `MeshData` property (for example a model
	/// whose unions keep their geometry behind `AssetId`).
	#[error("model holds no union mesh data")]
	NoUnionData,
}

/// Normalises the normals (zeroing degenerate ones). Every public decoder ends with this.
pub(crate) fn finalize(mesh: &mut Mesh) {
	for normal in &mut mesh.normals {
		let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
		if length >= 0.01 && length.is_finite() {
			let scale = 1.0 / length;
			*normal = [normal[0] * scale, normal[1] * scale, normal[2] * scale];
		} else if length.is_finite() {
			*normal = [0.0; 3];
		}
	}
}

/// Decodes a FileMesh asset (any version). Also accepts `SLIM` / `SLMD` cache files (their parts
/// merged, reported as [`MeshFormat::Slim`]).
pub fn decode_mesh(bytes: &[u8]) -> Result<Mesh, MeshError> {
	let mut mesh = if bytes.starts_with(b"version ") {
		filemesh::decode(bytes)?
	} else if bytes.starts_with(slim::MAGIC) || bytes.starts_with(slim::MANIFEST_MAGIC) {
		slim::decode(bytes)?
	} else {
		return Err(MeshError::UnknownHeader(header_text(bytes, 12)));
	};
	finalize(&mut mesh);
	Ok(mesh)
}

/// Decodes union / CSG graphics mesh data (`PartOperation.MeshData` or its `AssetId` asset).
///
/// Accepts the `CSGMDL` blob itself (versions 2 and 4, XOR-obfuscated, and 5), an unpublished
/// union's `SolidMeshHolder` data (`SolidMesh`), or the model the `Content` / `AssetId` URL delivers
/// (a binary or XML model whose `PartOperationAsset` carries `MeshData`).
/// A `CSGK` key returns [`MeshError::CsgReference`]; a model without union mesh data returns
/// [`MeshError::NoUnionData`].
pub fn decode_union(bytes: &[u8]) -> Result<Mesh, MeshError> {
	let mut mesh = if bytes.starts_with(b"<roblox") {
		let mesh_data = rbxm::find_mesh_data(bytes)?.ok_or(MeshError::NoUnionData)?;
		union::decode(&mesh_data)?
	} else {
		union::decode(bytes)?
	};
	finalize(&mut mesh);
	Ok(mesh)
}

/// Decodes whatever mesh-like asset `bytes` holds: a FileMesh, a `SLIM` / `SLMD` file, union mesh
/// data or a union asset model.
pub fn decode_any(bytes: &[u8]) -> Result<Mesh, MeshError> {
	match sniff(bytes) {
		Kind::FileMesh | Kind::Slim => decode_mesh(bytes),
		Kind::Union | Kind::Model => decode_union(bytes),
		Kind::Unknown => Err(MeshError::UnknownHeader(header_text(bytes, 12))),
	}
}

/// What kind of mesh data a byte buffer starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
	/// `version x.xx`
	FileMesh,
	/// `SLIM` or `SLMD` file
	Slim,
	/// `CSGMDL` (plain or obfuscated header) or `CSGK`
	Union,
	/// A Roblox model (`<roblox!` binary or `<roblox` XML); may hold union mesh data
	Model,
	Unknown,
}

/// Classifies `bytes` by their magic.
pub fn sniff(bytes: &[u8]) -> Kind {
	if bytes.starts_with(b"version ") {
		Kind::FileMesh
	} else if bytes.starts_with(slim::MAGIC) || bytes.starts_with(slim::MANIFEST_MAGIC) {
		Kind::Slim
	} else if union::is_union_data(bytes) {
		Kind::Union
	} else if bytes.starts_with(b"<roblox") {
		Kind::Model
	} else {
		Kind::Unknown
	}
}

fn header_text(bytes: &[u8], len: usize) -> String {
	bytes
		.iter()
		.take(len)
		.map(|&byte| if byte.is_ascii_graphic() || byte == b' ' { byte as char } else { '.' })
		.collect()
}

/// Builds the LOD ranges (in indices) from a file's LOD offset table (in faces).
///
/// `offsets[i]..offsets[i + 1]` is level `i`. Fewer than two offsets, or a first level that is empty
/// (`[0, 0]`), mean "no LODs": one level with every face. Offsets must be non-decreasing and within
/// the face count.
pub(crate) fn lods_from_offsets(offsets: &[u32], face_count: u32) -> Result<Vec<Range<u32>>, MeshError> {
	let all = || single_lod(face_count * 3);
	if offsets.len() < 2 {
		return Ok(all());
	}
	for pair in offsets.windows(2) {
		if pair[1] < pair[0] {
			return Err(MeshError::Invalid(format!("LOD offsets decrease: {offsets:?}")));
		}
	}
	if let Some(&last) = offsets.last()
		&& last > face_count
	{
		return Err(MeshError::Invalid(format!("LOD offset {last} beyond the face count {face_count}")));
	}
	if offsets[0] == offsets[1] {
		return Ok(all());
	}
	Ok(offsets.windows(2).map(|pair| pair[0] * 3..pair[1] * 3).collect())
}

/// One level covering `index_count` indices (none when there are no triangles).
#[allow(clippy::single_range_in_vec_init)]
pub(crate) fn single_lod(index_count: u32) -> Vec<Range<u32>> {
	if index_count == 0 { Vec::new() } else { vec![0..index_count] }
}

/// Checks that every index addresses a vertex.
pub(crate) fn check_indices(indices: &[u32], vertex_count: usize) -> Result<(), MeshError> {
	let max = indices.iter().copied().max().unwrap_or(0);
	if !indices.is_empty() && max as usize >= vertex_count {
		return Err(MeshError::Invalid(format!("vertex index {max} out of range ({vertex_count} vertices)")));
	}
	Ok(())
}

/// The face count as `u32`, or an error when it cannot be one.
pub(crate) fn face_count_u32(count: usize) -> Result<u32, MeshError> {
	u32::try_from(count)
		.ok()
		.filter(|count| count.checked_mul(3).is_some())
		.ok_or_else(|| MeshError::Invalid(format!("face count {count} too large")))
}

/// Decodes one tangent byte (`(b - 127) / 127`).
#[inline]
pub(crate) fn tangent_component(byte: u8) -> f32 {
	(byte as f32 - 127.0) * (1.0 / 127.0)
}

#[inline]
pub(crate) fn decode_tangent(bytes: [u8; 4]) -> [f32; 4] {
	[tangent_component(bytes[0]), tangent_component(bytes[1]), tangent_component(bytes[2]), tangent_component(bytes[3])]
}
