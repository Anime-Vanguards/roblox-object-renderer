//! CPU skinning: a MeshPart whose Bones pose its skinned mesh away from the bind pose draws a
//! deformed copy of the mesh (notes/mesh.md "Skinning in Studio").
//!
//! Studio skins in the part's space, after the mesh is scaled to the part (skin-pose "nonuniform":
//! a raised arm keeps the shape the scale gave it at rest). A mesh bone's bind CFrame, in the part,
//! sits at Size / InitialSize · (bind position - mesh centre) with the bind rotation; the scope's
//! Bone of the same name gives its CFrame relative to the part, and M = pose · bind⁻¹. A vertex
//! blends its four bones' matrices by weight (linear blend skinning, skin-pose "pose": the forearm
//! twist matches). A bone without a Bone moves with its nearest ancestor that has one ("missing-hand").
//! A Roblox client blends the bones' matrices in world space as stored, so a vertex without any weight
//! lands on the world origin (a client: a skinned mesh with 1,500 vertices' weights zeroed, doctor-unw),
//! posed or not. The result is stored back in mesh space, so the part's mesh transform (and the
//! shader) stay as for the bind mesh.
//!
//! Saved models mostly keep their Bones at the bind pose (the importer and Model:ScaleTo put them
//! there): those draw the cached bind mesh at no cost. Other poses are skinned once per (mesh, part
//! scale, pose) and cached; the copy shares the bind mesh's index buffer. A live scene's parts
//! (`Scene::live`) each keep one buffer instead, written over when their pose changes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use glam::{Affine3A, Mat3A, Vec3, Vec3A};
use parking_lot::Mutex;
use wgpu::util::DeviceExt;

use super::Renderer;
use super::geometry::{self, Vertex};
use super::resources::{Cache, GpuMesh, LoadError, LoadOutcome, StampProof, Weighed};
use crate::scene::{Geometry, Part, Skeleton};

/// A pose closer than this to the bind pose (matrix entries; studs for the translation) draws the
/// bind mesh: saved Bones sit within 1e-4 studs of it.
const BIND_TOLERANCE: f32 = 1e-3;

/// A mesh bone as skinning needs it.
struct SkinBone {
	name: String,
	/// Its bind CFrame (mesh space).
	bind: Affine3A,
	parent: Option<usize>,
}

/// A skinned mesh's bind-pose vertices (as `geometry::from_mesh` uploads them) and skinning data.
pub struct SkinSource {
	vertices: Vec<Vertex>,
	joints: Vec<[u16; 4]>,
	weights: Vec<[f32; 4]>,
	bones: Vec<SkinBone>,
	/// The vertices LOD0 draws (the only ones skinned).
	used: Vec<u32>,
	/// Whether any of them has no weight (it lands on the world origin, even in the bind pose).
	unweighted: bool,
	bytes: u64,
	pub(super) proof: StampProof,
}

impl Weighed for SkinSource {
	fn bytes(&self) -> u64 {
		self.bytes
	}
}

/// Live scenes' skinned parts kept at once (a few units' worth; more empties them all).
const LIVE_KEPT: usize = 64;

/// A live scene's skinned part (`Scene::live`): the pose last written and the buffer it is in.
struct LivePart {
	pose: u64,
	mesh: Arc<GpuMesh>,
}

/// Poses met once that [`Skins::posed`] remembers, to keep the next copy of one: past this many it
/// forgets them all.
const SEEN_POSES: usize = 1 << 16;

/// Skinning data per mesh and skinned copies per (mesh, scale, pose).
pub struct Skins {
	pub(super) sources: Cache<String, SkinSource>,
	/// Posed copies drawn more than once: a still's pose drawn again, a clip's character standing still.
	/// An animation's poses are each a frame's own (a clip of a unit's idle filled the cache's 1 GB
	/// of GPU memory with copies never drawn again; 2,484 of them in an aged service, hit 39 % of the
	/// time): the first copy of a pose is drawn and let go, the second kept.
	posed: Cache<String, GpuMesh>,
	seen: Mutex<std::collections::HashSet<String>>,
	/// By mesh and part index.
	live: Mutex<HashMap<(String, usize), LivePart>>,
	/// Since the start: meshes skinned, their CPU skinning and vertex upload (ns), the bytes uploaded
	/// (RenderStats takes each render's share).
	pub(super) totals: [AtomicU64; 4],
}

impl Default for Skins {
	fn default() -> Self {
		Self { sources: Cache::new(512 << 20), posed: Cache::new(1 << 30), seen: Mutex::default(), live: Mutex::default(), totals: Default::default() }
	}
}

impl Skins {
	/// [`Skins::totals`] now.
	pub(super) fn totals(&self) -> [u64; 4] {
		self.totals.each_ref().map(|total| total.load(Ordering::Relaxed))
	}

	/// Lets go of every posed copy; returns their bytes.
	pub(super) fn release_posed(&self) -> u64 {
		self.seen.lock().clear();
		self.posed.shrink_to(0)
	}

	pub fn stats(&self) -> serde_json::Value {
		serde_json::json!({
			"sources": {"entries": self.sources.len(), "bytes": self.sources.bytes()},
			"posed": {"entries": self.posed.len(), "bytes": self.posed.bytes(), "hits": self.posed.hits.load(Ordering::Relaxed), "misses": self.posed.misses.load(Ordering::Relaxed)},
		})
	}
}

/// A mesh bone's bind CFrame (mesh space): columns right, up, back (notes/mesh.md "Bones").
fn bind_cframe(bone: &ror_mesh::Bone) -> Affine3A {
	let r = bone.rotation;
	let matrix = Mat3A::from_cols(Vec3A::new(r[0][0], r[1][0], r[2][0]), Vec3A::new(r[0][1], r[1][1], r[2][1]), Vec3A::new(r[0][2], r[1][2], r[2][2]));
	Affine3A::from_mat3_translation(matrix.into(), Vec3::from(bone.position))
}

fn is_identity(matrix: &Affine3A) -> bool {
	let rotation = matrix.matrix3 - Mat3A::IDENTITY;
	let largest = rotation.x_axis.abs().max(rotation.y_axis.abs()).max(rotation.z_axis.abs()).max(matrix.translation.abs()).max_element();
	largest < BIND_TOLERANCE
}

fn hash_values(hash: &mut u64, values: impl IntoIterator<Item = f32>) {
	for value in values {
		// 1/65536 resolution: finer than any visible difference
		for byte in ((value as f64 * 65536.0).round() as i64).to_le_bytes() {
			*hash ^= byte as u64;
			*hash = hash.wrapping_mul(0x100000001b3);
		}
	}
}

/// How a MeshPart maps its mesh into the part: scale per axis about the mesh's centre.
#[derive(Clone, Copy)]
struct MeshScale {
	scale: Vec3A,
	centre: Vec3A,
}

impl MeshScale {
	/// From `part_mesh`'s transform (`scale(Size / InitialSize) · translate(-centre)`); `None` for
	/// anything else.
	fn of(local: &Affine3A) -> Option<Self> {
		let m = local.matrix3;
		let scale = Vec3A::new(m.x_axis.x, m.y_axis.y, m.z_axis.z);
		let off_diagonal = Vec3A::new(m.x_axis.y, m.x_axis.z, m.y_axis.x).abs().max(Vec3A::new(m.y_axis.z, m.z_axis.x, m.z_axis.y).abs()).max_element();
		if off_diagonal > 1e-6 || scale.abs().min_element() < 1e-9 || !scale.is_finite() {
			return None;
		}
		Some(Self { scale, centre: -local.translation / scale })
	}
}

/// Each mesh bone's skinning matrix in the part's space: the pose of the skeleton's Bone of its name
/// (relative to the part) times the inverse of its bind CFrame in the part. A bone without a Bone
/// moves with its nearest ancestor that has one; a root without one keeps its bind pose (skin-pose
/// "missing-hand").
fn bone_matrices(bones: &[SkinBone], skeleton: &Skeleton, part: &Affine3A, mesh_scale: MeshScale) -> Vec<Affine3A> {
	let to_part = part.inverse();
	let found: Vec<Option<Affine3A>> = bones
		.iter()
		.map(|bone| {
			skeleton.bones.get(&bone.name).map(|world| {
				// the bind CFrame in the part: scaled position, bind rotation
				let position = mesh_scale.scale * (bone.bind.translation - mesh_scale.centre);
				let bind = Affine3A::from_mat3_translation(bone.bind.matrix3.into(), position.into());
				to_part * *world * bind.inverse()
			})
		})
		.collect();
	(0..bones.len())
		.map(|index| {
			let mut current = Some(index);
			// bounded: a malformed parent cycle ends in the bind pose
			for _ in 0..=bones.len() {
				let Some(bone) = current else {
					break;
				};
				if let Some(matrix) = found[bone] {
					return matrix;
				}
				current = bones[bone].parent;
			}
			Affine3A::IDENTITY
		})
		.collect()
}

impl Renderer {
	/// The mesh to draw for a part (`drawn` from `part_mesh`): a posed copy when its Bones deform its
	/// skinned mesh, else `drawn` unchanged. `live`: the part's index in a live scene (`Scene::live`).
	pub(super) fn skinned(&self, part: &Part, drawn: (Arc<GpuMesh>, Affine3A, bool), live: Option<usize>) -> (Arc<GpuMesh>, Affine3A, bool) {
		self.skinned_checked(part, drawn, live).0
	}

	/// Like [`Renderer::skinned`], also returning `false` when a local skin source moved while
	/// being read (even if the resulting bone matrices were identity or the mesh had no bones).
	pub(super) fn skinned_checked(&self, part: &Part, drawn: (Arc<GpuMesh>, Affine3A, bool), live: Option<usize>) -> ((Arc<GpuMesh>, Affine3A, bool), bool) {
		let (Some(skeleton), Geometry::MeshPart { mesh: uri, .. }) = (&part.skeleton, &part.geometry) else {
			return (drawn, true);
		};
		match self.posed_mesh(uri, skeleton, part, &drawn.0, &drawn.1, live) {
			Ok((Some(mesh), stable)) => ((mesh, drawn.1, drawn.2), stable),
			Ok((None, stable)) => (drawn, stable),
			Err(_) => (drawn, !super::names_local_file(uri)),
		}
	}

	fn posed_mesh(&self, uri: &str, skeleton: &Skeleton, part: &Part, bind: &Arc<GpuMesh>, local: &Affine3A, live: Option<usize>) -> Result<(Option<Arc<GpuMesh>>, bool)> {
		let local_snap = super::local_snapshot_for(uri);
		let stamped = match &local_snap {
			Some((_, snap)) => snap.stamp.stamp_uri(uri),
			None => super::content_key(uri),
		};
		let source = self.skins.sources.get_or_load_checked(&stamped, self.assets.is_offline_view(), || self.skin_source(uri, local_snap.as_ref(), &stamped))?;
		// Only cache a posed/live mesh when both the skin source and bind mesh are proven to match
		// the same local-file stamp (or are both remote).
		let proof = match (&source.proof, &bind.proof) {
			(StampProof::Remote, StampProof::Remote) => StampProof::Remote,
			(StampProof::Proven(a), StampProof::Proven(b)) if a == b && *a == stamped => StampProof::Proven(stamped.clone()),
			_ => StampProof::Moved,
		};
		let stable = !matches!(proof, StampProof::Moved);
		let Some(mesh_scale) = MeshScale::of(local).filter(|_| !source.bones.is_empty()) else {
			return Ok((None, stable));
		};
		let matrices = bone_matrices(&source.bones, skeleton, &part.cframe, mesh_scale);
		if matrices.iter().all(is_identity) && !source.unweighted {
			return Ok((None, stable));
		}
		// where the world origin is in the part (unweighted vertices land there)
		let origin = part.cframe.inverse().translation;
		let mut hash = 0xcbf29ce484222325u64;
		hash_values(&mut hash, mesh_scale.scale.to_array().into_iter().chain(mesh_scale.centre.to_array()));
		if source.unweighted {
			hash_values(&mut hash, origin.to_array());
		}
		for matrix in &matrices {
			hash_values(&mut hash, matrix.to_cols_array());
		}
		if let Some(index) = live {
			return Ok((Some(self.live_mesh(&stamped, index, hash, &source, &matrices, mesh_scale, origin, bind, proof)), stable));
		}
		let key = format!("{stamped}#{hash:016x}");
		let pose = || {
			let start = std::time::Instant::now();
			let vertices = skin(&source, &matrices, mesh_scale, origin);
			let skinned = start.elapsed();
			let vertex_buffer = self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
				label: Some("skinned mesh"),
				contents: bytemuck::cast_slice(&vertices),
				usage: wgpu::BufferUsages::VERTEX,
			});
			let totals = &self.skins.totals;
			totals[0].fetch_add(1, Ordering::Relaxed);
			totals[1].fetch_add(skinned.as_nanos() as u64, Ordering::Relaxed);
			totals[2].fetch_add((start.elapsed() - skinned).as_nanos() as u64, Ordering::Relaxed);
			totals[3].fetch_add(std::mem::size_of_val(vertices.as_slice()) as u64, Ordering::Relaxed);
			Ok(GpuMesh {
				vertex_buffer,
				index_buffer: bind.index_buffer.clone(),
				lods: bind.lods.clone(),
				high_quality: bind.high_quality,
				// the bind pose's bounds: the part's mesh transform stays the bind mesh's
				bounds: bind.bounds,
				drawn: drawn_bounds(&source, &vertices).unwrap_or(bind.drawn),
				bind: Some(bind.vertex_buffer.clone()),
				decal_faces: None,
				vertex_alpha: bind.vertex_alpha,
				bytes: std::mem::size_of_val(vertices.as_slice()) as u64,
				proof: proof.clone(),
			})
		};
		if !stable {
			self.skins.posed.misses.fetch_add(1, Ordering::Relaxed);
			return Ok((Some(Arc::new(pose()?)), false));
		}
		if let Some(mesh) = self.skins.posed.get(&key) {
			return Ok((Some(mesh), true));
		}
		let first = {
			let mut seen = self.skins.seen.lock();
			if seen.len() >= SEEN_POSES {
				seen.clear();
			}
			seen.insert(key.clone())
		};
		let mesh = if first {
			self.skins.posed.misses.fetch_add(1, Ordering::Relaxed);
			Arc::new(pose()?)
		} else {
			self.skins.posed.get_or_load(&key, pose)?
		};
		Ok((Some(mesh), true))
	}

	/// A live scene's skinned part: its one buffer, written over when the pose is not the one in it.
	/// A new buffer is allocated and uploaded through a staging copy at the next submit: for one character
	/// (Ruler) scrubbing its idle (two skinned parts, 1.17 MB a pose) the submit took 1.40 ms a frame
	/// against 0.34 with the camera turning; written over, the vertices are the only upload.
	#[allow(clippy::too_many_arguments)]
	fn live_mesh(&self, uri: &str, index: usize, pose: u64, source: &SkinSource, matrices: &[Affine3A], mesh_scale: MeshScale, origin: Vec3A, bind: &Arc<GpuMesh>, proof: StampProof) -> Arc<GpuMesh> {
		let stable = !matches!(proof, StampProof::Moved);
		let slot = (uri.to_string(), index);
		if stable
			&& let Some(part) = self.skins.live.lock().get(&slot)
			&& part.pose == pose
		{
			return part.mesh.clone();
		}
		let start = std::time::Instant::now();
		let vertices = skin(source, matrices, mesh_scale, origin);
		let skinned = start.elapsed();
		let bytes: &[u8] = bytemuck::cast_slice(&vertices);
		let mut live = self.skins.live.lock();
		let vertex_buffer = match stable.then(|| live.get(&slot).filter(|part| part.mesh.vertex_buffer.size() == bytes.len() as u64)).flatten() {
			Some(part) => part.mesh.vertex_buffer.clone(),
			None => self.gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some("live skinned mesh"), size: bytes.len() as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
		};
		// ordered after the draws already submitted with it, before the next submit's
		self.gpu.queue.write_buffer(&vertex_buffer, 0, bytes);
		// the part's one buffer, with this pose's box
		let mesh = Arc::new(GpuMesh {
			vertex_buffer,
			index_buffer: bind.index_buffer.clone(),
			lods: bind.lods.clone(),
			high_quality: bind.high_quality,
			bounds: bind.bounds,
			drawn: drawn_bounds(source, &vertices).unwrap_or(bind.drawn),
			bind: Some(bind.vertex_buffer.clone()),
			decal_faces: None,
			vertex_alpha: bind.vertex_alpha,
			bytes: bytes.len() as u64,
			proof,
		});
		if stable {
			if live.len() >= LIVE_KEPT && !live.contains_key(&slot) {
				live.clear();
			}
			live.insert(slot, LivePart { pose, mesh: mesh.clone() });
		}
		let totals = &self.skins.totals;
		totals[0].fetch_add(1, Ordering::Relaxed);
		totals[1].fetch_add(skinned.as_nanos() as u64, Ordering::Relaxed);
		totals[2].fetch_add((start.elapsed() - skinned).as_nanos() as u64, Ordering::Relaxed);
		totals[3].fetch_add(bytes.len() as u64, Ordering::Relaxed);
		mesh
	}

	fn skin_source(&self, uri: &str, local_snap: Option<&(std::path::PathBuf, ror_assets::FileSnapshot)>, stamped: &str) -> std::result::Result<LoadOutcome<SkinSource>, LoadError> {
		let (fetched, proof, keep) = match local_snap {
			Some((path, snap)) => {
				super::run_test_read_hook(path);
				match self.assets.fetch_local_with_snapshot(uri, path, *snap) {
					Ok((fetched, verified)) => (fetched, if verified { StampProof::Proven(stamped.to_string()) } else { StampProof::Moved }, verified),
					Err(error) => {
						return Err(if snap.still_unchanged(path) { LoadError::Cached(anyhow!("{error}")) } else { LoadError::Uncached(anyhow!("{error}")) });
					}
				}
			}
			None => match self.assets.fetch(uri) {
				Ok(fetched) => (fetched, StampProof::Remote, true),
				Err(error) => return Err(LoadError::Cached(anyhow!("{error}"))),
			},
		};
		let mesh = match ror_mesh::decode_mesh(&fetched.bytes) {
			Ok(mesh) => mesh,
			Err(error) => {
				let err = anyhow!("{uri}: {error}");
				return Err(if keep { LoadError::Cached(err) } else { LoadError::Uncached(err) });
			}
		};
		let Some(skinning) = mesh.skinning.as_ref().filter(|skinning| !skinning.bones.is_empty() && skinning.joints.len() == mesh.vertex_count()) else {
			return Ok(LoadOutcome {
				value: SkinSource { vertices: Vec::new(), joints: Vec::new(), weights: Vec::new(), bones: Vec::new(), used: Vec::new(), unweighted: false, bytes: 64, proof },
				keep,
				offline_fallback: false,
			});
		};
		let cpu = geometry::from_mesh(&mesh);
		let lod0 = cpu.lods.first().cloned().unwrap_or(0..cpu.indices.len() as u32);
		let mut used: Vec<u32> = cpu.indices[lod0.start as usize..lod0.end as usize].to_vec();
		used.sort_unstable();
		used.dedup();
		let count = skinning.bones.len();
		let bones: Vec<SkinBone> =
			skinning.bones.iter().map(|bone| SkinBone { name: bone.name.clone(), bind: bind_cframe(bone), parent: bone.parent.map(usize::from).filter(|&parent| parent < count) }).collect();
		let bytes = (std::mem::size_of_val(cpu.vertices.as_slice()) + skinning.joints.len() * 24 + used.len() * 4) as u64;
		let unweighted = used.iter().any(|&index| skinning.weights.get(index as usize).is_some_and(|weights| weights.iter().sum::<f32>() <= 0.0));
		Ok(LoadOutcome {
			value: SkinSource { vertices: cpu.vertices, joints: skinning.joints.clone(), weights: skinning.weights.clone(), bones, used, unweighted, bytes, proof },
			keep,
			offline_fallback: false,
		})
	}
}

/// The box of the vertices LOD0 draws (mesh space): a posed copy's own, as `geometry::from_mesh`
/// measures the bind mesh's (`None` without any).
fn drawn_bounds(source: &SkinSource, vertices: &[Vertex]) -> Option<(Vec3, Vec3)> {
	let (low, high) = source.used.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(low, high), &index| {
		let position = Vec3::from(vertices[index as usize].position);
		(low.min(position), high.max(position))
	});
	(low.x <= high.x).then_some((low, high))
}

/// Linear blend skinning in the part's space of the vertices LOD0 draws (the rest keep their bind
/// pose), stored back in mesh space: positions through the inverse of the mesh scale, normals and
/// tangents so that the shader's transforms of the part's matrix give the skinned directions. A vertex
/// without weights goes to `origin`, the world origin in the part's space.
fn skin(source: &SkinSource, matrices: &[Affine3A], mesh_scale: MeshScale, origin: Vec3A) -> Vec<Vertex> {
	let MeshScale { scale, centre } = mesh_scale;
	let mut vertices = source.vertices.clone();
	for &index in &source.used {
		let index = index as usize;
		let weights = source.weights[index];
		let joints = source.joints[index];
		let mut linear = Mat3A::ZERO;
		let mut translation = Vec3A::ZERO;
		let mut total = 0.0;
		for slot in 0..4 {
			let weight = weights[slot];
			if weight > 0.0 {
				let matrix = &matrices[joints[slot] as usize];
				linear += matrix.matrix3 * weight;
				translation += matrix.translation * weight;
				total += weight;
			}
		}
		// unweighted: a client blends in world space, so nothing moves the vertex off the world origin
		if total <= 0.0 {
			vertices[index].position = (origin / scale + centre).to_array();
			continue;
		}
		let vertex = &mut vertices[index];
		let position = linear * (scale * (Vec3A::from(vertex.position) - centre)) + translation;
		vertex.position = (position / scale + centre).to_array();
		// normals transform with the inverse scale, tangents with the scale
		vertex.normal = (scale * (linear * (Vec3A::from(vertex.normal) / scale))).normalize_or_zero().to_array();
		let tangent = Vec3A::new(vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]);
		let tangent = ((linear * (tangent * scale)) / scale).normalize_or_zero();
		vertex.tangent = [tangent.x, tangent.y, tangent.z, vertex.tangent[3]];
	}
	vertices
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use glam::{Affine3A, Quat, Vec3, Vec3A};

	use super::{MeshScale, SkinBone, SkinSource, Vertex, bone_matrices, is_identity, skin};
	use crate::scene::Skeleton;

	fn vertex(position: [f32; 3], normal: [f32; 3]) -> Vertex {
		Vertex { position, normal, uv: [0.0; 2], tangent: [1.0, 0.0, 0.0, 1.0], color: [255; 4] }
	}

	/// A two-bone chain up +Y in mesh space (a root at the origin, a tip at y = 1) and vertices on
	/// the root, on the tip, half on each, and on none.
	fn chain(vertices: Vec<Vertex>, joints: Vec<[u16; 4]>, weights: Vec<[f32; 4]>) -> SkinSource {
		let used = (0..vertices.len() as u32).collect();
		SkinSource {
			vertices,
			joints,
			weights,
			bones: vec![
				SkinBone { name: "Root".into(), bind: Affine3A::IDENTITY, parent: None },
				SkinBone { name: "Tip".into(), bind: Affine3A::from_translation(Vec3::Y), parent: Some(0) },
			],
			used,
			unweighted: false,
			bytes: 0,
			proof: super::StampProof::Remote,
		}
	}

	fn skeleton(bones: &[(&str, Affine3A)]) -> Skeleton {
		Skeleton { bones: bones.iter().map(|(name, world)| (name.to_string(), *world)).collect::<HashMap<_, _>>() }
	}

	fn close(a: [f32; 3], b: [f32; 3]) -> bool {
		(Vec3::from(a) - Vec3::from(b)).length() < 1e-5
	}

	const UNSCALED: MeshScale = MeshScale { scale: Vec3A::ONE, centre: Vec3A::ZERO };

	#[test]
	fn bind_pose_is_identity() {
		let part = Affine3A::from_rotation_translation(Quat::from_rotation_y(0.7), Vec3::new(10.0, 2.0, -3.0));
		let source = chain(Vec::new(), Vec::new(), Vec::new());
		let bones = skeleton(&[("Root", part), ("Tip", part * Affine3A::from_translation(Vec3::Y))]);
		assert!(bone_matrices(&source.bones, &bones, &part, UNSCALED).iter().all(is_identity));
	}

	#[test]
	fn rotated_tip_bends_its_vertices_about_the_joint() {
		let part = Affine3A::from_translation(Vec3::new(5.0, 0.0, 0.0));
		let source = chain(
			vec![vertex([0.0, 0.5, 0.0], [1.0, 0.0, 0.0]), vertex([0.0, 2.0, 0.0], [1.0, 0.0, 0.0]), vertex([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]), vertex([1.0, 1.0, 0.0], [0.0, 0.0, 1.0])],
			vec![[0, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0], [0; 4]],
			vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.0, 0.0], [0.0; 4]],
		);
		let tip = part * Affine3A::from_translation(Vec3::Y) * Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
		let matrices = bone_matrices(&source.bones, &skeleton(&[("Root", part), ("Tip", tip)]), &part, UNSCALED);
		let skinned = skin(&source, &matrices, UNSCALED, Vec3A::new(-5.0, 0.0, 0.0));
		assert!(close(skinned[0].position, [0.0, 0.5, 0.0]));
		// one stud above the joint, turned a quarter about Z
		assert!(close(skinned[1].position, [-1.0, 1.0, 0.0]));
		assert!(close(skinned[1].normal, [0.0, 1.0, 0.0]));
		// the joint itself stays put whatever the blend
		assert!(close(skinned[2].position, [0.0, 1.0, 0.0]));
		// unweighted: on the world origin (doctor-unw), five studs left of the part
		assert!(close(skinned[3].position, [-5.0, 0.0, 0.0]));
	}

	#[test]
	fn missing_bone_follows_its_parent() {
		let part = Affine3A::IDENTITY;
		let source = chain(vec![vertex([0.0, 2.0, 0.0], [1.0, 0.0, 0.0])], vec![[1, 0, 0, 0]], vec![[1.0, 0.0, 0.0, 0.0]]);
		let root = Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
		let matrices = bone_matrices(&source.bones, &skeleton(&[("Root", root)]), &part, UNSCALED);
		assert!(close(skin(&source, &matrices, UNSCALED, Vec3A::ZERO)[0].position, [-2.0, 0.0, 0.0]));
		// nothing found: the bind pose
		assert!(bone_matrices(&source.bones, &skeleton(&[]), &part, UNSCALED).iter().all(is_identity));
	}

	#[test]
	fn skins_after_a_non_uniform_scale() {
		// Size / InitialSize (2, 1, 1): the tip's bind sits at (0, 1, 0) in the part, and a vertex one
		// mesh unit to its right two studs to its right; a quarter turn puts it two studs above
		let scale = MeshScale { scale: Vec3A::new(2.0, 1.0, 1.0), centre: Vec3A::ZERO };
		let part = Affine3A::IDENTITY;
		let source = chain(vec![vertex([1.0, 1.0, 0.0], [1.0, 0.0, 0.0])], vec![[1, 0, 0, 0]], vec![[1.0, 0.0, 0.0, 0.0]]);
		let tip = Affine3A::from_translation(Vec3::Y) * Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
		let matrices = bone_matrices(&source.bones, &skeleton(&[("Root", part), ("Tip", tip)]), &part, scale);
		let skinned = skin(&source, &matrices, scale, Vec3A::ZERO);
		// part space (0, 3, 0), stored in mesh space
		assert!(close(skinned[0].position, [0.0, 3.0, 0.0]));
		// the part's normal (1, 0, 0) turned to +Y; in mesh space a normal scales by S (still +Y)
		assert!(close(skinned[0].normal, [0.0, 1.0, 0.0]));
	}

	/// Guards stamping the local mesh URI in `Renderer::posed_mesh` (`self.skins.sources`): rewriting a
	/// local skinned `.mesh` file under the same path must reload its `SkinSource` instead of reusing
	/// the cached bind vertices / bone weights from the earlier version.
	#[test]
	fn rewriting_local_skinned_mesh_reloads_skin_source() {
		use std::sync::Arc;
		use super::Renderer;
		use crate::scene::Geometry;

		fn skinned_v4(tip_x: f32) -> Vec<u8> {
			let mut b = Vec::new();
			let u16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
			let u32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
			let f32 = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
			let vec3 = |b: &mut Vec<u8>, v: [f32; 3]| {
				f32(b, v[0]);
				f32(b, v[1]);
				f32(b, v[2]);
			};
			let names = b"Root\0Child\0";
			b.extend_from_slice(b"version 4.01\n");
			u16(&mut b, 24);
			u16(&mut b, 3);
			u32(&mut b, 4); // num_verts
			u32(&mut b, 2); // num_faces
			u16(&mut b, 0); // num_lods
			u16(&mut b, 2); // num_bones
			u32(&mut b, names.len() as u32);
			u16(&mut b, 1); // num_subsets
			b.push(1); // num_hq_lods
			b.push(0); // unused
			// 4 vertices (40 bytes each: pos[3], norm[3], uv[2], tangent[4], color[4])
			let verts = [[0.0, 0.0, 0.0], [tip_x, 1.0, 0.0], [tip_x, 2.0, 0.0], [0.0, 2.0, 0.0]];
			for pos in verts {
				vec3(&mut b, pos);
				vec3(&mut b, [0.0, 0.0, 1.0]);
				f32(&mut b, 0.0);
				f32(&mut b, 0.0);
				b.extend_from_slice(&[254, 127, 127, 254]);
				b.extend_from_slice(&[255, 255, 255, 255]);
			}
			// 4 envelopes (8 bytes each: 4 bone slots, 4 weights): all weighted 100% to subset slot 0 (bone 1 = Child)
			for _ in 0..4 {
				b.extend_from_slice(&[0, 0, 0, 0, 255, 0, 0, 0]);
			}
			// 2 faces
			for [i0, i1, i2] in [[0u32, 1, 2], [0, 2, 3]] {
				u32(&mut b, i0);
				u32(&mut b, i1);
				u32(&mut b, i2);
			}
			// 2 bones: Root (parent 0xFFFF, pos 0), Child (parent 0, pos (0,1,0)), identity rotation
			for (name_off, parent, pos) in [(0u32, 0xFFFFu16, [0.0, 0.0, 0.0]), (5, 0, [0.0, 1.0, 0.0])] {
				u32(&mut b, name_off);
				u16(&mut b, parent);
				u16(&mut b, 0xFFFF);
				f32(&mut b, 2.0);
				vec3(&mut b, [1.0, 0.0, 0.0]);
				vec3(&mut b, [0.0, 1.0, 0.0]);
				vec3(&mut b, [0.0, 0.0, 1.0]);
				vec3(&mut b, pos);
			}
			b.extend_from_slice(names);
			// 1 subset: faces_begin=0, faces_len=2, verts_begin=0, verts_len=4, num_bones=2, table=[1, 0, 0xFFFF..]
			u32(&mut b, 0);
			u32(&mut b, 2);
			u32(&mut b, 0);
			u32(&mut b, 4);
			u32(&mut b, 2);
			let mut table = [0xFFFFu16; 26];
			table[0] = 1;
			table[1] = 0;
			for entry in table {
				u16(&mut b, entry);
			}
			b
		}

		let state = std::env::temp_dir().join(format!("ror-skin-source-stamp-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let mesh_path = state.join("rig.mesh");
		let uri = format!("file:///{}", mesh_path.display().to_string().replace('\\', "/").replace(' ', "%20"));

		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("renderer");

		let mut model = crate::scene::build::ModelBuilder::new("Rig");
		model.part(&crate::scene::build::PartSpec { name: "Body".into(), class: "MeshPart", size: Vec3::splat(2.0), ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let part = Arc::make_mut(&mut scene.parts[0]);
		part.geometry = Geometry::MeshPart { mesh: uri.clone(), initial_size: Vec3::splat(2.0), fidelity: 0 };
		// Rotate Child bone 90 degrees about Z (moves vertex (tip_x, 1, 0) to (0, 1 + tip_x, 0)).
		part.skeleton = Some(Arc::new(skeleton(&[
			("Root", Affine3A::IDENTITY),
			("Child", Affine3A::from_translation(Vec3::Y) * Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2)),
		])));

		std::fs::write(&mesh_path, skinned_v4(1.0)).unwrap();
		let bind_a = renderer.load_mesh(&uri).unwrap();
		let (posed_a, _, _) = renderer.skinned(part, (bind_a, Affine3A::IDENTITY, false), None);

		std::fs::write(&mesh_path, skinned_v4(5.0)).unwrap();
		let bind_b = renderer.load_mesh(&uri).unwrap();
		let (posed_b, _, _) = renderer.skinned(part, (bind_b, Affine3A::IDENTITY, false), None);

		assert!(
			(posed_b.drawn.1.y - posed_a.drawn.1.y).abs() > 2.0,
			"rewriting local skinned mesh must reload SkinSource (drawn_a={:?}, drawn_b={:?})",
			posed_a.drawn,
			posed_b.drawn
		);
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
