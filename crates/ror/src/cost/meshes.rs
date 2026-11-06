//! What the cost model needs of a mesh: its triangles at each level of detail, a hash of its shape
//! (two uploads of one mesh never instance together: notes/cost.md "Instancing"), its triangles
//! facing each face (a Decal on a MeshPart draws those) and, for a cheap one, its triangles as an
//! occluder.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use parking_lot::Mutex;
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
	/// Full-detail triangles facing each face, `Enum.NormalId` order (Right, Top, Back, Left, Bottom,
	/// Front): what a Decal on that face of a MeshPart draws.
	pub facing: [u32; 6],
	/// Distinct vertex positions at full detail: what Roblox's occlusion counts of a mesh.
	pub positions: u32,
	/// The full-detail triangles of a mesh cheap enough to occlude ([`OCCLUDER_POSITIONS`]).
	pub occluder: Option<Arc<Vec<[Vec3; 3]>>>,
	pub skinned: bool,
	/// The file's count of high-quality levels: what RenderFidelity Automatic takes of the chain
	/// (crate::lod)
	pub high_quality: u8,
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
		let mut wanted: HashMap<String, &Geometry> = HashMap::new();
		for part in parts {
			if let Some(key) = mesh_key(&part.geometry) {
				wanted.entry(key).or_insert(&part.geometry);
			}
		}
		let missing = Mutex::new(Vec::new());
		let stats: HashMap<String, Arc<MeshStats>> = wanted
			.into_par_iter()
			.filter_map(|(key, geometry)| {
				let decoded = match geometry {
					Geometry::Union { mesh_data: Some(data), .. } => ror_mesh::decode_union(data).map_err(|error| error.to_string()),
					Geometry::Union { asset: Some(asset), .. } => assets.fetch(asset).map_err(|error| error.to_string()).and_then(|fetched| ror_mesh::decode_union(&fetched.bytes).map_err(|error| error.to_string())),
					Geometry::MeshPart { mesh, .. } | Geometry::Special { mesh: Some(mesh), .. } => {
						assets.fetch(mesh).map_err(|error| error.to_string()).and_then(|fetched| ror_mesh::decode_mesh(&fetched.bytes).map_err(|error| error.to_string()))
					}
					_ => return None,
				};
				match decoded {
					Ok(mesh) => Some((key, Arc::new(stats_of(&mesh)))),
					Err(error) => {
						missing.lock().push(format!("{key}: {error}"));
						None
					}
				}
			})
			.collect();
		let mut missing = missing.into_inner();
		missing.sort();
		Self { stats, missing }
	}

	pub fn of(&self, geometry: &Geometry) -> Option<&Arc<MeshStats>> {
		self.stats.get(&mesh_key(geometry)?)
	}
}

fn stats_of(mesh: &ror_mesh::Mesh) -> MeshStats {
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
	// ROR_COST_FACING_MIN: the cosine a triangle's normal must exceed toward a face to count as facing it (default 0: any
	// positive component, about three faces per triangle). Studio drew a quarter of that for 108 Textures on 18 meshes
	// (notes/cost.md "Decals and Textures on meshes"); no value fits both its triangles and its draws, so it stays 0.
	let facing_min: f32 = std::env::var("ROR_COST_FACING_MIN").ok().and_then(|value| value.parse().ok()).unwrap_or(0.0);
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
				if normal.dot(*axis) > facing_min {
					facing[face] += 1;
				}
			}
		}
		if full {
			triangles.push([a, b, c]);
		}
	}
	MeshStats { lods, shape: hash, bounds, facing, positions, occluder: full.then(|| Arc::new(triangles)), skinned: mesh.skinning.is_some(), high_quality: mesh.high_quality_lods }
}
