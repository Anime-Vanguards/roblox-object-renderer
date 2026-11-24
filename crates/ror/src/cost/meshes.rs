//! What the cost model needs of a mesh: its triangles at each level of detail, a hash of its shape
//! (two uploads of one mesh never instance together: notes/cost.md "Instancing"), its triangles
//! facing each face (a Decal on a MeshPart draws those) and, for a cheap one, its triangles as an
//! occluder.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use rayon::prelude::*;

use crate::scene::{Geometry, Part};

/// Meshes of up to this many distinct vertex positions (normals and UVs merged away: a cube's 8) can
/// hide what is behind them, Roblox staff's "about 2000" (notes/occlusion.md "Occluders"; m18, b7:
/// a large place's meshes of 370 to 2,044 positions hid, of 2,429, 2,489 and 2,982 never, whatever their
/// triangles). Under it a mesh must also be large enough on screen ([`super::OCCLUDER_POSITIONS_PER_PIXEL`]).
pub const OCCLUDER_POSITIONS: u32 = 2048;

#[derive(Clone, Debug)]
pub struct MeshStats {
	/// Triangles at each level of detail, full detail first (one entry: the file has no chain).
	pub lods: Vec<u32>,
	/// FNV-1a of the full-detail positions and indices: equal for two uploads of one shape.
	pub shape: u64,
	/// The full-detail box in the mesh's own space (a MeshPart scales it to its Size).
	pub bounds: (Vec3, Vec3),
	/// Full-detail triangles assigned to each dominant box face, `Enum.NormalId` order (Right, Top,
	/// Back, Left, Bottom, Front): what a Decal or Texture on that face of a uniformly scaled MeshPart draws.
	pub facing: [u32; 6],
	/// Full-detail triangles with a positive normal component toward each face (`normal.dot(axis) > min`):
	/// kept for Unions and SpecialMesh FileMeshes, where the renderer does not use dominant-face clipping.
	pub facing_positive: [u32; 6],
	/// Distinct vertex positions at full detail: what Roblox's occlusion counts of a mesh.
	pub positions: u32,
	/// The full-detail triangles of a mesh cheap enough to occlude ([`OCCLUDER_POSITIONS`]).
	pub occluder: Option<Arc<Vec<[Vec3; 3]>>>,
	pub skinned: bool,
	/// The file's count of high-quality levels: what RenderFidelity Automatic takes of the chain
	/// (crate::lod)
	pub high_quality: u8,
	/// Encoded mesh bytes on disk / downloaded asset (inline MeshData length for inline unions).
	pub file_bytes: u64,
	/// Vertex count in the decoded mesh (`mesh.positions.len()`).
	pub vertices: u32,
	/// Index count across all LODs (`mesh.indices.len()`).
	pub indices: u32,
	/// Vertex buffer bytes estimated from Roblox's recorded FileMesh binary layout (`notes/mesh.md`:
	/// 12 B position, 12 B normal, 8 B UV, 4 B packed tangent, 4 B RGBA color; 40 B full vertex).
	pub vertex_bytes: u64,
	/// Assumed index buffer GPU bytes (u16 when `vertices <= 65535`, else u32; FileMesh stores u32).
	pub index_bytes: u64,
	/// Skinning bytes estimated from FileMesh packed envelopes (`4 B` joints + `4 B` weights per
	/// vertex) plus an assumed `64 B` per bone matrix counted once per unique mesh (a client poses
	/// each skinned instance separately).
	pub skinning_bytes: u64,
	/// Upper-bound estimated GPU geometry bytes (`vertex_bytes + index_bytes + skinning_bytes`;
	/// client GPU mesh layout is not recorded in `notes/mesh.md`).
	pub gpu_bytes: u64,
}

/// Every mesh, union and file mesh the scene's parts draw, decoded once (in parallel); failures
/// (refused or missing assets) are left out and listed.
pub struct Meshes {
	pub stats: HashMap<String, Arc<MeshStats>>,
	pub missing: Vec<String>,
}

/// The key a part's mesh has in [`Meshes::stats`]: its content id, or a union's asset or data hash.
pub fn mesh_key(geometry: &Geometry) -> Option<String> {
	match geometry {
		Geometry::MeshPart { mesh, .. } => Some(canonical(mesh)),
		Geometry::Special { mesh: Some(mesh), mesh_type, .. } if *mesh_type == crate::scene::extract::mesh_type::FILE_MESH => Some(canonical(mesh)),
		Geometry::Union { asset: Some(asset), mesh_data: None, .. } => Some(canonical(asset)),
		Geometry::Union { mesh_data: Some(data), .. } => Some(format!("union#{:016x}", fnv(data))),
		_ => None,
	}
}

/// One spelling per asset (`rbxassetid://1`, `http://www.roblox.com/asset/?id=1` and `1` are one
/// mesh to Roblox).
pub fn canonical(uri: &str) -> String {
	match ror_assets::ContentId::parse(uri) {
		Ok(id) => id.canonical(),
		Err(_) => uri.trim().to_string(),
	}
}

pub fn fnv(bytes: &[u8]) -> u64 {
	let mut hash = 0xcbf29ce484222325u64;
	for &byte in bytes {
		hash ^= byte as u64;
		hash = hash.wrapping_mul(0x100000001b3);
	}
	hash
}

impl Meshes {
	pub fn load(parts: &[Arc<Part>], assets: &ror_assets::Assets) -> Self {
		let mut cache = HashMap::new();
		Self::load_cached(parts, assets, &mut cache)
	}

	/// Loads every distinct mesh referenced by `parts`, reusing any previously decoded entries in
	/// `cache` (used by multi-frame timeline costing to decode each mesh asset once).
	pub fn load_cached(parts: &[Arc<Part>], assets: &ror_assets::Assets, cache: &mut HashMap<String, Result<Arc<MeshStats>, String>>) -> Self {
		let mut wanted: HashMap<String, &Geometry> = HashMap::new();
		for part in parts {
			if let Some(key) = mesh_key(&part.geometry) {
				wanted.entry(key).or_insert(&part.geometry);
			}
		}
		let uncached: Vec<(String, &Geometry)> = wanted.iter().filter(|(key, _)| !cache.contains_key(*key)).map(|(key, geometry)| (key.clone(), *geometry)).collect();
		if !uncached.is_empty() {
			let decoded: Vec<(String, Result<Arc<MeshStats>, String>)> = uncached
				.into_par_iter()
				.filter_map(|(key, geometry)| {
					let result = match geometry {
						Geometry::Union { mesh_data: Some(data), .. } => ror_mesh::decode_union(data).map(|mesh| (mesh, data.len() as u64)).map_err(|error| error.to_string()),
						Geometry::Union { asset: Some(asset), .. } => assets.fetch(asset).map_err(|error| error.to_string()).and_then(|fetched| {
							let len = fetched.bytes.len() as u64;
							ror_mesh::decode_union(&fetched.bytes).map(|mesh| (mesh, len)).map_err(|error| error.to_string())
						}),
						Geometry::MeshPart { mesh, .. } | Geometry::Special { mesh: Some(mesh), .. } => assets.fetch(mesh).map_err(|error| error.to_string()).and_then(|fetched| {
							let len = fetched.bytes.len() as u64;
							ror_mesh::decode_mesh(&fetched.bytes).map(|mesh| (mesh, len)).map_err(|error| error.to_string())
						}),
						_ => return None,
					};
					Some((key, result.map(|(mesh, file_bytes)| Arc::new(stats_of(&mesh, file_bytes)))))
				})
				.collect();
			for (key, result) in decoded {
				cache.insert(key, result);
			}
		}
		let mut stats = HashMap::new();
		let mut missing = Vec::new();
		for key in wanted.into_keys() {
			match cache.get(&key) {
				Some(Ok(entry)) => {
					stats.insert(key, entry.clone());
				}
				Some(Err(error)) => {
					missing.push(format!("{key}: {error}"));
				}
				None => {}
			}
		}
		missing.sort();
		Self { stats, missing }
	}

	pub fn of(&self, geometry: &Geometry) -> Option<&Arc<MeshStats>> {
		self.stats.get(&mesh_key(geometry)?)
	}
}

fn stats_of(mesh: &ror_mesh::Mesh, file_bytes: u64) -> MeshStats {
	let lods: Vec<u32> = mesh.lods.iter().map(|range| (range.end - range.start) / 3).filter(|&count| count > 0).collect();
	let lods = if lods.is_empty() { vec![(mesh.indices.len() / 3) as u32] } else { lods };
	let indices = mesh.lod0();
	let position = |index: u32| -> Vec3 { Vec3::from(mesh.positions.get(index as usize).copied().unwrap_or_default()) };
	let mut hash = 0xcbf29ce484222325u64;
	let mut mix = |bits: u32| {
		for byte in bits.to_le_bytes() {
			hash ^= byte as u64;
			hash = hash.wrapping_mul(0x100000001b3);
		}
	};
	for &index in indices {
		let p = position(index);
		mix(p.x.to_bits());
		mix(p.y.to_bits());
		mix(p.z.to_bits());
	}
	let bounds = mesh.lod0_bounds().map(|(min, max)| (Vec3::from(min), Vec3::from(max))).unwrap_or((Vec3::ZERO, Vec3::ZERO));
	const AXES: [Vec3; 6] = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	let mut facing = [0u32; 6];
	let mut facing_positive = [0u32; 6];
	// Default: assign each MeshPart triangle to its single dominant box face (matching mesh.wgsl's
	// `box_face` for uniformly scaled MeshPart decals), so the six faces partition the mesh (~1x total,
	// ~1/6 per face on an isotropic mesh) and faces with no matching triangles stay at 0. Setting
	// `ROR_COST_FACING_MIN` overrides this with the legacy cosine threshold (`normal.dot(axis) > min`).
	// `facing_positive` keeps the positive-component rule for Unions and SpecialMesh FileMeshes.
	let facing_min: Option<f32> = std::env::var("ROR_COST_FACING_MIN").ok().and_then(|value| value.parse().ok());
	let pos_min = facing_min.unwrap_or(0.0);
	let positions = indices
		.iter()
		.map(|&index| {
			let p = position(index);
			(p.x.to_bits(), p.y.to_bits(), p.z.to_bits())
		})
		.collect::<std::collections::HashSet<_>>()
		.len() as u32;
	let limit: u32 = std::env::var("ROR_COST_OCCLUDER_POSITIONS").ok().and_then(|value| value.parse().ok()).unwrap_or(OCCLUDER_POSITIONS);
	let full = positions <= limit;
	let mut triangles = Vec::with_capacity(if full { indices.len() / 3 } else { 0 });
	for triangle in indices.chunks_exact(3) {
		let [a, b, c] = [position(triangle[0]), position(triangle[1]), position(triangle[2])];
		let normal = (b - a).cross(c - a);
		if normal.length_squared() > 0.0 {
			// NormalId order: Right +X, Top +Y, Back +Z, Left -X, Bottom -Y, Front -Z
			let normal = normal.normalize();
			for (face, axis) in AXES.iter().enumerate() {
				if normal.dot(*axis) > pos_min {
					facing_positive[face] += 1;
				}
			}
			if let Some(min) = facing_min {
				for (face, axis) in AXES.iter().enumerate() {
					if normal.dot(*axis) > min {
						facing[face] += 1;
					}
				}
			} else {
				facing[box_face(normal)] += 1;
			}
		}
		if full {
			triangles.push([a, b, c]);
		}
	}
	let vertices = mesh.positions.len() as u32;
	let indices_count = mesh.indices.len() as u32;
	// Roblox FileMesh binary vertex layout (notes/mesh.md): 12 B pos, 12 B normal, 8 B UV,
	// 4 B packed tangent, 4 B RGBA color (40 B full vertex). Client GPU layout is unrecorded.
	let mut stride = 12u64;
	if !mesh.normals.is_empty() {
		stride += 12;
	}
	if !mesh.uvs.is_empty() {
		stride += 8;
	}
	if !mesh.tangents.is_empty() {
		stride += 4;
	}
	if !mesh.colors.is_empty() {
		stride += 4;
	}
	let vertex_bytes = u64::from(vertices) * stride;
	let index_bytes = u64::from(indices_count) * if vertices > 65_535 { 4 } else { 2 };
	let skinning_bytes = mesh.skinning.as_ref().map_or(0, |skin| skin.joints.len() as u64 * 4 + skin.weights.len() as u64 * 4 + skin.bones.len() as u64 * 64);
	let gpu_bytes = vertex_bytes + index_bytes + skinning_bytes;
	MeshStats {
		lods,
		shape: hash,
		bounds,
		facing,
		facing_positive,
		positions,
		occluder: full.then(|| Arc::new(triangles)),
		skinned: mesh.skinning.is_some(),
		high_quality: mesh.high_quality_lods,
		file_bytes,
		vertices,
		indices: indices_count,
		vertex_bytes,
		index_bytes,
		skinning_bytes,
		gpu_bytes,
	}
}

/// The box face (`Enum.NormalId` order: Right +X, Top +Y, Back +Z, Left -X, Bottom -Y, Front -Z)
/// that `direction` points most nearly toward, matching `mesh.wgsl`'s `box_face`.
fn box_face(direction: Vec3) -> usize {
	let a = direction.abs();
	if a.x >= a.y && a.x >= a.z {
		if direction.x > 0.0 { 0 } else { 3 }
	} else if a.y >= a.z {
		if direction.y > 0.0 { 1 } else { 4 }
	} else if direction.z > 0.0 {
		2
	} else {
		5
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn mesh_from_tris(tris: &[[[f32; 3]; 3]]) -> ror_mesh::Mesh {
		let mut mesh = ror_mesh::Mesh::default();
		for tri in tris {
			let base = mesh.positions.len() as u32;
			mesh.positions.extend_from_slice(tri);
			mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
		}
		mesh
	}

	fn tri_facing(dir: Vec3) -> [[f32; 3]; 3] {
		let n = dir.normalize();
		let ref_up = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
		let u = ref_up.cross(n).normalize();
		let v = n.cross(u);
		[Vec3::ZERO.into(), u.into(), v.into()]
	}

	#[test]
	fn dominant_axis_assigns_each_triangle_to_one_face() {
		// Flat +Y quad (2 triangles strictly facing Top / +Y)
		let flat = mesh_from_tris(&[
			[[-1.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 0.0, -1.0]],
			[[-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0]],
		]);
		let stats = stats_of(&flat, 0);
		assert_eq!(stats.facing, [0, 2, 0, 0, 0, 0]);
		assert_eq!(stats.facing_positive, [0, 2, 0, 0, 0, 0]);
		assert_eq!((stats.vertices, stats.indices, stats.vertex_bytes, stats.index_bytes, stats.gpu_bytes), (6, 6, 72, 12, 84));

		// Asymmetric tilted mesh: distinct triangle count per NormalId face (Right=1, Top=2, Back=3,
		// Left=4, Bottom=5, Front=6) so swapping any two faces fails, and tilted normals ensure
		// facing_positive exceeds facing.
		let dirs = [
			(Vec3::new(1.0, 0.25, 0.1), 1usize),  // Right (+X)
			(Vec3::new(0.1, 1.0, 0.25), 2),       // Top (+Y)
			(Vec3::new(0.25, 0.1, 1.0), 3),       // Back (+Z)
			(Vec3::new(-1.0, 0.25, -0.1), 4),     // Left (-X)
			(Vec3::new(-0.1, -1.0, 0.25), 5),     // Bottom (-Y)
			(Vec3::new(0.25, -0.1, -1.0), 6),     // Front (-Z)
		];
		let mut tris = Vec::new();
		for (dir, count) in dirs {
			for _ in 0..count {
				tris.push(tri_facing(dir));
			}
		}
		let asym = mesh_from_tris(&tris);
		let asym_stats = stats_of(&asym, 0);
		assert_eq!(asym_stats.facing, [1, 2, 3, 4, 5, 6]);
		assert_eq!(asym_stats.facing.iter().sum::<u32>(), 21);
		assert!(asym_stats.facing_positive.iter().sum::<u32>() > 21, "tilted faces have positive dot on multiple axes: {:?}", asym_stats.facing_positive);

		// Octahedron (8 tilted faces, none axis-aligned): dominant axis partitions all 8 triangles
		let (px, nx) = ([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]);
		let (py, ny) = ([0.0, 1.0, 0.0], [0.0, -1.0, 0.0]);
		let (pz, nz) = ([0.0, 0.0, 1.0], [0.0, 0.0, -1.0]);
		let oct = mesh_from_tris(&[
			[py, pz, px], [py, px, nz], [py, nz, nx], [py, nx, pz],
			[ny, px, pz], [ny, nz, px], [ny, nx, nz], [ny, pz, nx],
		]);
		let oct_stats = stats_of(&oct, 0);
		assert_eq!(oct_stats.facing.iter().sum::<u32>(), 8);
	}

	#[test]
	fn full_vertex_layout_matches_recorded_40_byte_filemesh_stride() {
		let mut mesh = mesh_from_tris(&[[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]);
		mesh.normals = vec![[0.0, 0.0, 1.0]; 3];
		mesh.uvs = vec![[0.0, 0.0]; 3];
		mesh.tangents = vec![[1.0, 0.0, 0.0, 1.0]; 3];
		mesh.colors = vec![[255, 255, 255, 255]; 3];
		let stats = stats_of(&mesh, 200);
		// 12 + 12 + 8 + 4 + 4 = 40 B/vertex (FileMesh packed tangent), 3*2 = 6 B u16 indices
		assert_eq!(stats.vertex_bytes, 3 * 40);
		assert_eq!(stats.index_bytes, 6);
		assert_eq!(stats.gpu_bytes, 126);
	}
}
