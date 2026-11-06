//! Immutable part preparation, shared by renders whose inputs agree byte for byte. Public scenes
//! can be edited directly, so a clone's identity or an extraction generation is not enough: every
//! lookup encodes the current inputs, including poses and material variants. Effects and Lighting
//! stay outside this cache and are prepared at their current moment.
//!
//! Three levels: a whole scene's preparation (its draws, instance buffer and light grid), each
//! part's own preparation (what a scene whose other parts changed reuses: a clip's map around a few
//! moving limbs), and the light grid by the parts that fill it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Affine3A, Vec3};
use parking_lot::Mutex;

use super::{DrawItem, GpuTexture, InstanceData};
use crate::scene::{Geometry, MaterialVariant, Materials, Part, Scene, SurfaceAppearance, Tiling};

const KEPT_FOR: Duration = Duration::from_secs(30);
const KEPT_SCENES: usize = 8;

/// One part's draws and instances, indexed from its own first instance and draw (a scene's
/// preparation lays them after the parts before it).
pub(super) struct PreparedPart {
	pub instances: Vec<InstanceData>,
	pub draws: Vec<DrawItem>,
	/// Its main draw (instance, index into `draws`), for its Highlights
	pub main: Option<(u32, usize)>,
	/// Its mesh's box as drawn, in its own space (`None`: the mesh did not load)
	pub drawn: Option<(Vec3, Vec3)>,
	/// Its material tiles with the Organic noise
	pub organic: bool,
	/// What its draws could not load (its material's own are reported before them)
	pub missing: Vec<String>,
}

impl PreparedPart {
	fn bytes(&self) -> u64 {
		(std::mem::size_of::<Self>() + std::mem::size_of_val(self.instances.as_slice()) + std::mem::size_of_val(self.draws.as_slice())) as u64
	}
}

/// A light grid's texture, kept by the parts that fill it (`lightgrid::key`).
pub(super) struct PreparedGrid {
	pub texture: wgpu::Texture,
	pub view: wgpu::TextureView,
	pub origin: Vec3,
	pub size: Vec3,
	pub bytes: u64,
}

struct KeptPart {
	/// Highlight membership and kept Glass, which depend on the part's place in its scene
	flags: u8,
	part: Arc<PreparedPart>,
	used: Instant,
	bytes: u64,
}

/// The parts prepared under one material configuration (`part_context`).
struct PartTable {
	context: Vec<u8>,
	/// By the part's key bytes, hashed by foldhash: SipHash over a 5,053-part map's 1.9 MB of keys took
	/// 1.35 ms of every frame's preparation
	parts: HashMap<Box<[u8]>, KeptPart, foldhash::fast::RandomState>,
	bytes: u64,
	used: Instant,
}

struct KeptGrid {
	key: Vec<u8>,
	grid: Arc<PreparedGrid>,
	used: Instant,
}

struct KeptState {
	state: super::lightgrid::GridState,
	used: Instant,
}

pub(super) struct PreparedGeometry {
	pub draws: Vec<DrawItem>,
	pub instances: wgpu::Buffer,
	pub no_lights: wgpu::Buffer,
	pub light_grid: Option<(wgpu::TextureView, Vec3, Vec3)>,
	pub light_grid_texture: Option<wgpu::Texture>,
	pub bounds: Option<(Vec3, Vec3)>,
	pub organic_noise: Option<Arc<GpuTexture>>,
	pub surface_maps: Option<(Arc<GpuTexture>, Arc<GpuTexture>)>,
	pub part_draws: Vec<Option<(u32, usize)>>,
	/// The prepared parts its draws came from, and each draw's among them (`parts[draw_parts[i]]`
	/// drew `draws[i]`): a clip's residency knows a part it has seen by its preparation
	/// (render::residency::ClipResidency)
	pub parts: Vec<Arc<PreparedPart>>,
	pub draw_parts: Vec<u32>,
	pub instance_count: usize,
	/// What it owns: its draw list, instance and light buffers and light grid
	pub bytes: u64,
	/// The meshes and textures its draws hold, the organic noise and surface maps: their caches'
	/// too, but kept alive by it past those caches' budgets
	pub held: u64,
}

impl PreparedGeometry {
	/// What keeping it holds on to
	fn kept_bytes(&self) -> u64 {
		self.bytes + self.held
	}

	/// The same preparation drawing `draws` instead (as many, in the same order: one view's levels of
	/// detail, render::levels); never kept, so nothing made for it (shadow maps) outlives the view.
	pub fn with_draws(&self, draws: Vec<DrawItem>) -> Self {
		debug_assert_eq!(draws.len(), self.draws.len());
		Self {
			draws,
			instances: self.instances.clone(),
			no_lights: self.no_lights.clone(),
			light_grid: self.light_grid.clone(),
			light_grid_texture: self.light_grid_texture.clone(),
			bounds: self.bounds,
			organic_noise: self.organic_noise.clone(),
			surface_maps: self.surface_maps.clone(),
			part_draws: self.part_draws.clone(),
			parts: self.parts.clone(),
			draw_parts: self.draw_parts.clone(),
			instance_count: self.instance_count,
			bytes: 0,
			held: 0,
		}
	}
}

struct Kept {
	key: Arc<SceneKey>,
	geometry: Arc<PreparedGeometry>,
	used: Instant,
}

pub(super) struct Preparations {
	kept: Mutex<Vec<Kept>>,
	parts: Mutex<Vec<PartTable>>,
	grids: Mutex<Vec<KeptGrid>>,
	/// Grids kept to be updated for a scene whose parts moved (`lightgrid::build_from`)
	states: Mutex<Vec<KeptState>>,
	budget: u64,
}

impl Preparations {
	pub fn new(budget: u64) -> Self {
		Self { kept: Mutex::default(), parts: Mutex::default(), grids: Mutex::default(), states: Mutex::default(), budget }
	}

	/// Takes the most recently kept grid state with `fillers` fillers, to update for another scene.
	pub fn take_grid_state(&self, fillers: usize) -> Option<super::lightgrid::GridState> {
		let mut states = self.states.lock();
		states.retain(|entry| entry.used.elapsed() < KEPT_FOR);
		let index = states.iter().enumerate().filter(|(_, entry)| entry.state.fillers() == fillers).max_by_key(|(_, entry)| entry.used).map(|(index, _)| index)?;
		Some(states.swap_remove(index).state)
	}

	/// Keeps a grid state for the next scene's update, within the budget (the least recently used
	/// go first).
	pub fn keep_grid_state(&self, state: super::lightgrid::GridState) {
		if state.bytes() > self.budget {
			return;
		}
		let mut states = self.states.lock();
		states.retain(|entry| entry.used.elapsed() < KEPT_FOR);
		states.push(KeptState { state, used: Instant::now() });
		let mut total: u64 = states.iter().map(|entry| entry.state.bytes()).sum();
		while total > self.budget || states.len() > KEPT_SCENES {
			let oldest = states.iter().enumerate().min_by_key(|(_, entry)| entry.used).map(|(index, _)| index).unwrap();
			total -= states.swap_remove(oldest).state.bytes();
		}
	}

	/// Each part `wanted` names: its kept preparation under `context`, where its key (`key.parts`) and
	/// flags agree (`None` for the others).
	pub fn parts_where(&self, context: &[u8], key: &SceneKey, flags: &[u8], wanted: impl Fn(usize) -> bool) -> Vec<Option<Arc<PreparedPart>>> {
		let mut tables = self.parts.lock();
		let Some(table) = tables.iter_mut().find(|table| table.context == context) else {
			return vec![None; key.parts.len()];
		};
		let now = Instant::now();
		table.used = now;
		key.parts
			.iter()
			.zip(flags)
			.enumerate()
			.map(|(position, (bytes, &flags))| {
				if !wanted(position) {
					return None;
				}
				let kept = table.parts.get_mut(&bytes[..]).filter(|kept| kept.flags == flags)?;
				kept.used = now;
				Some(kept.part.clone())
			})
			.collect()
	}

	/// Keeps parts prepared under `context` (key bytes, flags, preparation), then lets go of those
	/// unused for a while and, past the budget, of the least recently used.
	pub fn keep_parts<'a>(&self, context: &[u8], parts: impl Iterator<Item = (&'a [u8], u8, Arc<PreparedPart>)>) {
		let now = Instant::now();
		let mut tables = self.parts.lock();
		let index = match tables.iter().position(|table| table.context == context) {
			Some(index) => index,
			None => {
				tables.push(PartTable { context: context.to_vec(), parts: HashMap::default(), bytes: 0, used: now });
				tables.len() - 1
			}
		};
		let table = &mut tables[index];
		table.used = now;
		for (key, flags, part) in parts {
			let bytes = part.bytes() + key.len() as u64 + std::mem::size_of::<KeptPart>() as u64;
			table.bytes += bytes;
			if let Some(old) = table.parts.insert(key.into(), KeptPart { flags, part, used: now, bytes }) {
				table.bytes -= old.bytes;
			}
		}
		Self::expire_parts(&mut tables, now);
		let mut total: u64 = tables.iter().map(|table| table.bytes + table.context.len() as u64).sum();
		if total > self.budget {
			let mut ages: Vec<(Instant, usize, Box<[u8]>)> = tables.iter().enumerate().flat_map(|(index, table)| table.parts.iter().map(move |(key, kept)| (kept.used, index, key.clone()))).collect();
			ages.sort_unstable_by_key(|(used, ..)| *used);
			for (_, index, key) in ages {
				if total <= self.budget {
					break;
				}
				let table = &mut tables[index];
				if let Some(kept) = table.parts.remove(&key) {
					table.bytes -= kept.bytes;
					total -= kept.bytes;
				}
			}
			tables.retain(|table| !table.parts.is_empty());
		}
	}

	fn expire_parts(tables: &mut Vec<PartTable>, now: Instant) {
		for table in tables.iter_mut() {
			let mut freed = 0;
			table.parts.retain(|_, kept| {
				let keep = now.duration_since(kept.used) < KEPT_FOR;
				if !keep {
					freed += kept.bytes;
				}
				keep
			});
			table.bytes -= freed;
		}
		tables.retain(|table| !table.parts.is_empty() && now.duration_since(table.used) < KEPT_FOR);
	}

	/// The light grid kept for `key`.
	pub fn grid(&self, key: &[u8]) -> Option<Arc<PreparedGrid>> {
		let mut grids = self.grids.lock();
		grids.retain(|entry| entry.used.elapsed() < KEPT_FOR);
		let entry = grids.iter_mut().find(|entry| entry.key == key)?;
		entry.used = Instant::now();
		Some(entry.grid.clone())
	}

	/// Whether the grid was kept: the caller then takes its texture out of its lease.
	pub fn keep_grid(&self, key: Vec<u8>, grid: Arc<PreparedGrid>) -> bool {
		let bytes = grid.bytes + key.len() as u64;
		if bytes > self.budget {
			return false;
		}
		let mut grids = self.grids.lock();
		grids.retain(|entry| entry.used.elapsed() < KEPT_FOR && entry.key != key);
		grids.push(KeptGrid { key, grid, used: Instant::now() });
		let mut total: u64 = grids.iter().map(|entry| entry.grid.bytes + entry.key.len() as u64).sum();
		while total > self.budget || grids.len() > KEPT_SCENES {
			let oldest = grids.iter().enumerate().min_by_key(|(_, entry)| entry.used).map(|(index, _)| index).unwrap();
			let entry = grids.swap_remove(oldest);
			total -= entry.grid.bytes + entry.key.len() as u64;
		}
		true
	}

	pub fn get(&self, key: &SceneKey) -> Option<Arc<PreparedGeometry>> {
		let mut kept = self.kept.lock();
		kept.retain(|entry| entry.used.elapsed() < KEPT_FOR);
		let entry = kept.iter_mut().find(|entry| entry.key.same(key))?;
		entry.used = Instant::now();
		Some(entry.geometry.clone())
	}

	/// Keeps a scene's preparation for its next frame or group of views when what it owns fits the
	/// budget. The others kept go, the least recently used first, while what they all own and hold
	/// (meshes and textures included) is past it: a map whose textures alone are past the budget
	/// (a large place's 1.6 GB) is kept for its clip's next frame, and nothing else with it.
	pub fn insert(&self, key: Arc<SceneKey>, geometry: Arc<PreparedGeometry>) -> bool {
		if geometry.bytes + key.bytes() > self.budget {
			return false;
		}
		let cost = |entry: &Kept| entry.geometry.kept_bytes() + entry.key.bytes();
		let mut kept = self.kept.lock();
		kept.retain(|entry| entry.used.elapsed() < KEPT_FOR && !entry.key.same(&key));
		kept.push(Kept { key, geometry, used: Instant::now() });
		let mut total: u64 = kept.iter().map(cost).sum();
		while (total > self.budget && kept.len() > 1) || kept.len() > KEPT_SCENES {
			// never the one just kept (the last)
			let others = kept.len() - 1;
			let oldest = kept[..others].iter().enumerate().min_by_key(|(_, entry)| entry.used).map(|(index, _)| index).unwrap();
			total -= cost(&kept.remove(oldest));
		}
		true
	}

	pub fn trim(&self) {
		self.kept.lock().retain(|entry| entry.used.elapsed() < KEPT_FOR);
		Self::expire_parts(&mut self.parts.lock(), Instant::now());
		self.grids.lock().retain(|entry| entry.used.elapsed() < KEPT_FOR);
		self.states.lock().retain(|entry| entry.used.elapsed() < KEPT_FOR);
	}

	/// Lets go of every kept preparation, part, grid and grid state; returns their bytes.
	pub fn clear(&self) -> u64 {
		let scenes: u64 = std::mem::take(&mut *self.kept.lock()).iter().map(|entry| entry.geometry.kept_bytes()).sum();
		let parts: u64 = std::mem::take(&mut *self.parts.lock()).iter().map(|table| table.bytes).sum();
		let grids: u64 = std::mem::take(&mut *self.grids.lock()).iter().map(|entry| entry.grid.bytes).sum();
		let states: u64 = std::mem::take(&mut *self.states.lock()).iter().map(|entry| entry.state.bytes()).sum();
		scenes + parts + grids + states
	}

	pub fn stats(&self) -> (usize, u64) {
		let kept = self.kept.lock();
		(kept.len(), kept.iter().map(|entry| entry.geometry.kept_bytes() + entry.key.bytes()).sum())
	}

	/// Kept parts and their bytes, kept grids and grid states and theirs.
	pub fn part_stats(&self) -> (usize, u64, usize, u64) {
		let tables = self.parts.lock();
		let grids = self.grids.lock();
		let states = self.states.lock();
		let grid_bytes: u64 = grids.iter().map(|entry| entry.grid.bytes).sum::<u64>() + states.iter().map(|entry| entry.state.bytes()).sum::<u64>();
		(tables.iter().map(|table| table.parts.len()).sum(), tables.iter().map(|table| table.bytes).sum(), grids.len() + states.len(), grid_bytes)
	}
}

/// Lengths and option tags make this an unambiguous encoding, compared in full rather than by a
/// hash. Floats keep their bits (including signed zero). Exhaustive destructuring makes a newly
/// added part or material field require an explicit decision here before the renderer compiles.
#[derive(Default)]
struct Key(Vec<u8>);

impl Key {
	fn number(&mut self, value: u64) {
		self.0.extend_from_slice(&value.to_le_bytes());
	}

	fn floats(&mut self, values: &[f32]) {
		self.0.extend_from_slice(bytemuck::cast_slice(values));
	}

	fn vector(&mut self, value: Vec3) {
		self.floats(&value.to_array());
	}

	fn frame(&mut self, value: Affine3A) {
		self.floats(&value.to_cols_array());
	}

	fn bytes(&mut self, value: &[u8]) {
		self.number(value.len() as u64);
		self.0.extend_from_slice(value);
	}

	fn text(&mut self, value: &str) {
		self.bytes(value.as_bytes());
	}

	fn optional<T>(&mut self, value: Option<T>, write: impl FnOnce(&mut Self, T)) {
		self.number(value.is_some() as u64);
		if let Some(value) = value {
			write(self, value);
		}
	}

	fn texture(&mut self, value: &Option<String>) {
		self.optional(value.as_deref(), Self::content);
	}

	/// A content URI as the resource caches key it: a local file's with its stamp (a file rewritten
	/// under the same path draws anew), other URIs as they are.
	fn content(&mut self, uri: &str) {
		self.text(&keyed(uri));
	}

	fn appearance(&mut self, appearance: &SurfaceAppearance) {
		let SurfaceAppearance { color_map, normal_map, metalness_map, roughness_map, texture_pack, alpha_mode, color, emissive_mask, emissive_strength, emissive_tint } =
			appearance;
		for texture in [color_map, normal_map, metalness_map, roughness_map, texture_pack, emissive_mask] {
			self.texture(texture);
		}
		self.number(*alpha_mode as u64);
		self.vector(*color);
		self.floats(&[*emissive_strength]);
		self.vector(*emissive_tint);
	}

	fn geometry(&mut self, geometry: &Geometry) {
		match geometry {
			Geometry::Block => self.number(0),
			Geometry::Ball => self.number(1),
			Geometry::Cylinder => self.number(2),
			Geometry::Wedge => self.number(3),
			Geometry::CornerWedge => self.number(4),
			Geometry::Truss { style } => {
				self.number(5);
				self.number(*style as u64);
			}
			Geometry::MeshPart { mesh, initial_size, fidelity } => {
				self.number(6);
				self.text(&keyed(mesh));
				self.vector(*initial_size);
				self.number(*fidelity as u64);
			}
			Geometry::Special { mesh_type, mesh, scale, offset } => {
				self.number(7);
				self.number(*mesh_type as u64);
				self.texture(mesh);
				self.vector(*scale);
				self.vector(*offset);
			}
			Geometry::Union { asset, mesh_data, initial_size, use_part_color } => {
				self.number(8);
				self.texture(asset);
				self.optional(mesh_data.as_ref(), |key, data| key.bytes(data));
				self.vector(*initial_size);
				self.number(*use_part_color as u64);
			}
			Geometry::Collision { key, initial_size, .. } => {
				self.number(9);
				self.number(*key);
				self.vector(*initial_size);
			}
		}
	}

	fn part(&mut self, part: &Part) {
		let Part {
			path,
			class,
			cframe,
			size,
			geometry,
			color,
			material,
			material_variant,
			transparency,
			reflectance,
			cast_shadow,
			double_sided,
			texture,
			vertex_color,
			surface_appearance,
			decals,
			surfaces,
			skeleton,
			body_part,
			composite,
			seed,
			// read by the collisions-only view, which makes parts of its own: no pixel of this part
			collision: _,
			// which instance it came from: no pixel either
			referent: _,
		} = part;
		self.text(path);
		self.text(class);
		self.frame(*cframe);
		self.vector(*size);
		self.geometry(geometry);
		self.vector(*color);
		self.number(*material as u64);
		self.text(material_variant);
		self.floats(&[*transparency, *reflectance]);
		self.number(*cast_shadow as u64);
		self.number(*double_sided as u64);
		self.texture(texture);
		self.vector(*vertex_color);
		self.optional(surface_appearance.as_ref(), Self::appearance);
		self.number(decals.len() as u64);
		for decal in decals {
			let crate::scene::Decal { face, texture, color, transparency, z_index, tiling, uv_offset, uv_scale } = decal;
			self.number(*face as u64);
			self.content(texture);
			self.vector(*color);
			self.floats(&[*transparency]);
			self.floats(uv_offset);
			self.floats(uv_scale);
			self.number(*z_index as u64);
			self.optional(tiling.as_ref(), |key, tiling| {
				let Tiling { studs_per_tile_u, studs_per_tile_v, offset_studs_u, offset_studs_v } = tiling;
				key.floats(&[*studs_per_tile_u, *studs_per_tile_v, *offset_studs_u, *offset_studs_v]);
			});
		}
		for surface in surfaces {
			self.number(*surface as u64);
		}
		self.optional(skeleton.as_ref(), |key, skeleton| {
			let crate::scene::Skeleton { bones } = skeleton.as_ref();
			let mut bones: Vec<_> = bones.iter().collect();
			bones.sort_by_key(|(name, _)| *name);
			key.number(bones.len() as u64);
			for (name, frame) in bones {
				key.text(name);
				key.frame(*frame);
			}
		});
		self.optional(*body_part, |key, part| key.number(part as u64));
		self.optional(composite.as_ref(), |key, composite| {
			let crate::scene::character::Composite { layout, colors, pants, shirt, graphic } = composite;
			key.number(*layout as u64);
			key.number(colors.len() as u64);
			for color in colors {
				key.floats(color);
			}
			for layer in [pants, shirt, graphic] {
				key.optional(layer.as_ref(), |key, layer| {
					let crate::scene::character::Layer { image, tint } = layer;
					key.text(image);
					key.floats(tint);
				});
			}
		});
		// the material offsets drawn from it
		self.number(*seed);
	}

	fn materials(&mut self, materials: &Materials) {
		let Materials { use_2022, variants, overrides } = materials;
		self.number(*use_2022 as u64);
		self.number(variants.len() as u64);
		for variant in variants {
			let MaterialVariant {
				name,
				base_material,
				color_map,
				normal_map,
				metalness_map,
				roughness_map,
				emissive_mask,
				texture_pack,
				studs_per_tile,
				pattern,
				emissive_strength,
				emissive_tint,
			} = variant;
			self.text(name);
			self.number(*base_material as u64);
			for texture in [color_map, normal_map, metalness_map, roughness_map, emissive_mask, texture_pack] {
				self.texture(texture);
			}
			self.floats(&[*studs_per_tile, *emissive_strength]);
			self.number(*pattern as u64);
			self.vector(*emissive_tint);
		}
		self.number(overrides.len() as u64);
		for (material, name) in overrides {
			self.number(*material as u64);
			self.text(name);
		}
	}
}

/// A scene's key, and where each part's own key lies in it.
pub(super) struct SceneKey {
	/// Each part's encoding (one a scene before held shared, [`key`])
	pub parts: Vec<Arc<[u8]>>,
	/// What the parts' preparation reads from the scene beyond them: its materials, Neon share, light
	/// grid, ViewportFrame, Highlight membership and kept Glass
	pub tail: Vec<u8>,
	/// Hashed from the parts' and the tail's: keys whose hashes differ are different keys
	pub hash: u64,
}

impl SceneKey {
	/// The same key: every part's bytes and the tail the same.
	pub fn same(&self, other: &SceneKey) -> bool {
		self.hash == other.hash && self.parts.len() == other.parts.len() && self.tail == other.tail && self.parts.iter().zip(&other.parts).all(|(a, b)| Arc::ptr_eq(a, b) || a[..] == b[..])
	}

	/// Its bytes: what a kept scene's preparation costs to keep its key.
	pub fn bytes(&self) -> u64 {
		(self.tail.len() + self.parts.iter().map(|part| part.len()).sum::<usize>()) as u64
	}

	/// All of it in one (the part count, each part's bytes, the tail), for comparisons in tests.
	#[cfg(test)]
	pub fn flat(&self) -> Vec<u8> {
		let mut out = (self.parts.len() as u64).to_le_bytes().to_vec();
		for part in &self.parts {
			out.extend_from_slice(part);
		}
		out.extend_from_slice(&self.tail);
		out
	}
}

/// A clip's last frame's preparation, which its next frame's starts from (render::ClipKept): a part
/// whose key bytes and flags are the last frame's, under the same material configuration, is the
/// last frame's preparation of it, as the kept parts' table would give (the same bytes prepare the
/// same), found by comparing its bytes instead of hashing and looking up all of a map's; and where
/// the parts that changed draw as many instances and draws as before, the frame's draws and
/// instances are the last frame's with theirs written over, laid out exactly as anew.
#[derive(Default)]
pub(super) struct ClipGeometry {
	pub last: Mutex<Option<LastFrame>>,
}

pub(super) struct LastFrame {
	pub key: Arc<SceneKey>,
	pub context: Vec<u8>,
	pub flags: Vec<u8>,
	/// Each part's preparation may stand for a part with its key and flags: all its assets loaded and
	/// no skeleton, as the kept parts' table takes them
	pub reusable: Vec<bool>,
	pub geometry: Arc<PreparedGeometry>,
	/// The instances `geometry`'s buffer holds
	pub instances: Vec<InstanceData>,
	/// Each part's mesh box as drawn, and its first instance and draw
	pub drawn: Vec<Option<(Vec3, Vec3)>>,
	pub offsets: Vec<(u32, u32)>,
	/// Some of its parts were kept from an earlier scene and others changed
	pub moving: bool,
}

/// What a frame's preparation leaves for its clip's next frame besides its geometry ([`LastFrame`]).
pub(super) struct FramePieces {
	pub context: Vec<u8>,
	pub flags: Vec<u8>,
	pub reusable: Vec<bool>,
	pub instances: Vec<InstanceData>,
	pub drawn: Vec<Option<(Vec3, Vec3)>>,
	pub offsets: Vec<(u32, u32)>,
}

impl FramePieces {
	pub fn finish(self, key: Arc<SceneKey>, geometry: Arc<PreparedGeometry>, moving: bool) -> LastFrame {
		let FramePieces { context, flags, reusable, instances, drawn, offsets } = self;
		LastFrame { key, context, flags, reusable, geometry, instances, drawn, offsets, moving }
	}
}

/// `ROR_CLIP_GEOMETRY`: `off` prepares every clip frame as a scene of its own, `check` also lays each
/// patched frame out anew and panics where they differ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipGeometryMode {
	On,
	Off,
	Check,
}

pub(super) fn clip_geometry() -> ClipGeometryMode {
	static MODE: std::sync::OnceLock<ClipGeometryMode> = std::sync::OnceLock::new();
	*MODE.get_or_init(|| match std::env::var("ROR_CLIP_GEOMETRY").ok().as_deref().map(str::trim) {
		Some("off" | "0") => ClipGeometryMode::Off,
		Some("check") => ClipGeometryMode::Check,
		_ => ClipGeometryMode::On,
	})
}

/// A frame's parts laid out one after another: their instances, draws (each draw's instance and a
/// decal's order moved by where its part starts), each part's main draw, each draw's part, each
/// part's drawn box, and where each part's instances and draws start.
pub(super) struct Layout {
	pub instances: Vec<InstanceData>,
	pub draws: Vec<DrawItem>,
	pub part_draws: Vec<Option<(u32, usize)>>,
	pub draw_parts: Vec<u32>,
	pub drawn: Vec<Option<(Vec3, Vec3)>>,
	pub offsets: Vec<(u32, u32)>,
}

impl Layout {
	pub fn of(parts: &[Arc<PreparedPart>]) -> Self {
		let mut instances = Vec::with_capacity(parts.iter().map(|part| part.instances.len()).sum());
		let mut draws: Vec<DrawItem> = Vec::with_capacity(parts.iter().map(|part| part.draws.len()).sum());
		// each part's main draw (instance, index into draws), for its Highlights
		let mut part_draws: Vec<Option<(u32, usize)>> = Vec::with_capacity(parts.len());
		let mut draw_parts: Vec<u32> = Vec::with_capacity(draws.capacity());
		let mut drawn = Vec::with_capacity(parts.len());
		let mut offsets = Vec::with_capacity(parts.len());
		for (index, part) in parts.iter().enumerate() {
			let (instance, draw) = (instances.len() as u32, draws.len());
			offsets.push((instance, draw as u32));
			instances.extend_from_slice(&part.instances);
			draw_parts.extend(std::iter::repeat_n(index as u32, part.draws.len()));
			draws.extend(part.draws.iter().map(|item| placed(item, instance, draw)));
			part_draws.push(part.main.map(|(main, index)| (main + instance, index + draw)));
			drawn.push(part.drawn);
		}
		Self { instances, draws, part_draws, draw_parts, drawn, offsets }
	}

	/// Where `self` and `other` differ, if they do (`ROR_CLIP_GEOMETRY=check`).
	pub fn difference(&self, other: &Layout) -> Option<String> {
		if bytemuck::cast_slice::<InstanceData, u8>(&self.instances) != bytemuck::cast_slice::<InstanceData, u8>(&other.instances) {
			return Some("instances".into());
		}
		if self.part_draws != other.part_draws || self.draw_parts != other.draw_parts || self.drawn != other.drawn || self.offsets != other.offsets || self.draws.len() != other.draws.len() {
			return Some("parts' draws, boxes or places".into());
		}
		self.draws.iter().zip(&other.draws).position(|(a, b)| !a.same(b)).map(|index| format!("draw {index}"))
	}
}

/// A part's draw placed after the instances and draws before its part.
fn placed(item: &DrawItem, instance: u32, draw: usize) -> DrawItem {
	let mut item = item.clone();
	item.instance += instance;
	if item.decal {
		item.decal_order.0 += draw as u32;
	}
	item
}

impl LastFrame {
	/// This frame's layout, the last frame's with the parts not `reused` written over: `None` when
	/// one of those draws a number of instances or draws other than the last frame's part there.
	pub fn patched(&self, parts: &[Arc<PreparedPart>], reused: &[bool]) -> Option<Layout> {
		let last = &self.geometry.parts;
		if last.len() != parts.len() || self.offsets.len() != parts.len() {
			return None;
		}
		let changed: Vec<usize> = (0..parts.len()).filter(|&position| !reused[position]).collect();
		if changed.iter().any(|&position| parts[position].instances.len() != last[position].instances.len() || parts[position].draws.len() != last[position].draws.len()) {
			return None;
		}
		let mut instances = self.instances.clone();
		let mut draws = self.geometry.draws.clone();
		let mut part_draws = self.geometry.part_draws.clone();
		let mut drawn = self.drawn.clone();
		for position in changed {
			let part = &parts[position];
			let (instance, draw) = self.offsets[position];
			let (first, start) = (instance as usize, draw as usize);
			instances[first..first + part.instances.len()].copy_from_slice(&part.instances);
			for (index, item) in part.draws.iter().enumerate() {
				draws[start + index] = placed(item, instance, start);
			}
			part_draws[position] = part.main.map(|(main, index)| (main + instance, index + start));
			drawn[position] = part.drawn;
		}
		Some(Layout { instances, draws, part_draws, draw_parts: self.geometry.draw_parts.clone(), drawn, offsets: self.offsets.clone() })
	}

	/// Whether part `position` of `key` (with `flags`, under the same material configuration) is this
	/// frame's part there.
	pub fn same_part(&self, position: usize, key: &SceneKey, flags: &[u8]) -> bool {
		self.reusable.get(position).copied().unwrap_or(false)
			&& self.flags.get(position) == flags.get(position)
			&& self.key.parts.get(position).is_some_and(|bytes| Arc::ptr_eq(bytes, &key.parts[position]) || bytes[..] == key.parts[position][..])
	}
}

/// What a part's preparation reads beyond the part itself (the rest of its key is the part's own
/// encoding and its flags in the scene).
pub(super) fn part_context(materials: &Materials, neon_share: f32, viewport: bool, surface_maps: bool, play: bool) -> Vec<u8> {
	let mut key = Key::default();
	key.materials(materials);
	key.floats(&[neon_share]);
	key.number(viewport as u64);
	key.number(surface_maps as u64);
	key.number(play as u64);
	key.0
}

thread_local! {
	/// Whether the part being encoded names a local file (its key carries the file's stamp then,
	/// which can change while the part does not: [`key`] encodes such a part every time).
	static LOCAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A content URI as [`Key::content`] writes it, noting a local file's.
fn keyed(uri: &str) -> String {
	if super::names_local_file(uri) {
		LOCAL.set(true);
	}
	super::content_key(uri)
}

/// The key bytes of parts encoded lately, by the part's `Arc` ([`key`]): a clip's frames take its
/// map's parts again and again (scene::extract::RootReuse, each producer document its own `Arc`s).
/// Each entry holds a `Weak` of its part, so its allocation is never another part's while the entry
/// lives, and a part whose `Arc` is the entry's is the part encoded (`Arc::make_mut` of a part held
/// only once moves it elsewhere when weak references exist; shared, it copies it). Equal bytes are
/// held once (`interned`), so the documents' copies of a map share them.
#[derive(Default)]
struct PartKeys {
	at: HashMap<usize, PartKey, foldhash::fast::RandomState>,
	interned: HashMap<u64, Vec<Arc<[u8]>>, foldhash::fast::RandomState>,
	/// The entries past which the dead ones (their part dropped) go
	sweep_at: usize,
}

/// A part held weakly, its key bytes and their hash.
type PartKey = (std::sync::Weak<Part>, Arc<[u8]>, u64);

static PART_KEYS: parking_lot::Mutex<Option<PartKeys>> = parking_lot::Mutex::new(None);

/// Lets go of the parts' keys kept for later scenes (a quiet service's); returns their bytes.
pub(super) fn release_part_keys() -> u64 {
	PART_KEYS.lock().take().map_or(0, |keys| keys.interned.values().flatten().map(|bytes| bytes.len() as u64).sum::<u64>() + (keys.at.len() * std::mem::size_of::<Part>()) as u64)
}

pub(super) fn key(scene: &Scene, neon_share: f32, light_grid: bool) -> Arc<SceneKey> {
	let kept = scene.parts.len() >= KEYS_KEPT_FROM;
	// the parts encoded lately (the same `Arc`s) have the bytes they had then
	let found: Vec<Option<(Arc<[u8]>, u64)>> = match kept {
		true => {
			let cache = PART_KEYS.lock();
			match cache.as_ref() {
				Some(cache) => scene.parts.iter().map(|part| cache.at.get(&(Arc::as_ptr(part) as usize)).map(|(_, bytes, hash)| (bytes.clone(), *hash))).collect(),
				None => vec![None; scene.parts.len()],
			}
		}
		false => vec![None; scene.parts.len()],
	};
	let mut parts: Vec<Arc<[u8]>> = Vec::with_capacity(scene.parts.len());
	let mut hashes = Vec::with_capacity(scene.parts.len());
	// the parts encoded now that can be found by their `Arc` later (no local file)
	let mut fresh: Vec<usize> = Vec::new();
	let mut encoded = Key(Vec::with_capacity(512));
	for (index, (part, found)) in scene.parts.iter().zip(found).enumerate() {
		match found {
			Some((bytes, hash)) => {
				parts.push(bytes);
				hashes.push(hash);
			}
			None => {
				LOCAL.set(false);
				encoded.0.clear();
				encoded.part(part);
				hashes.push(hash_bytes(&encoded.0));
				parts.push(Arc::from(&encoded.0[..]));
				if kept && !LOCAL.get() {
					fresh.push(index);
				}
			}
		}
	}
	if !fresh.is_empty() {
		let mut cache = PART_KEYS.lock();
		let cache = cache.get_or_insert_with(PartKeys::default);
		for index in fresh {
			let (bytes, hash) = (&mut parts[index], hashes[index]);
			let same = cache.interned.entry(hash).or_default();
			match same.iter().find(|known| known[..] == bytes[..]) {
				Some(known) => *bytes = known.clone(),
				None => same.push(bytes.clone()),
			}
			cache.at.insert(Arc::as_ptr(&scene.parts[index]) as usize, (Arc::downgrade(&scene.parts[index]), bytes.clone(), hash));
		}
		if cache.at.len() > cache.sweep_at {
			cache.at.retain(|_, (part, _, _)| part.strong_count() > 0);
			cache.interned.retain(|_, same| {
				same.retain(|bytes| Arc::strong_count(bytes) > 1);
				!same.is_empty()
			});
			cache.sweep_at = (cache.at.len() * 2).max(65_536);
		}
	}
	if clip_geometry() == ClipGeometryMode::Check {
		for (part, bytes) in scene.parts.iter().zip(&parts) {
			let mut anew = Key::default();
			anew.part(part);
			assert!(anew.0[..] == bytes[..], "a part's key bytes found by its Arc differ from those made anew");
		}
	}
	let mut tail = Key::default();
	tail.materials(&scene.materials);
	tail.floats(&[neon_share]);
	tail.number(light_grid as u64);
	// a ViewportFrame's MeshParts draw their vertex alpha (TEX_VERTEX_ALPHA)
	tail.number(scene.environment.viewport.is_some() as u64);
	// Highlight membership can keep otherwise invisible parts drawing. Its style is per frame.
	tail.number(scene.highlights.len() as u64);
	for highlight in &scene.highlights {
		tail.number(highlight.parts.len() as u64);
		for part in &highlight.parts {
			tail.number(*part as u64);
		}
	}
	tail.number(scene.kept_glass.len() as u64);
	for part in &scene.kept_glass {
		tail.number(*part as u64);
	}
	// the whole key's hash from its parts' (and their count) and the tail's
	let mut whole = Vec::with_capacity(8 * (hashes.len() + 2));
	whole.extend_from_slice(&(parts.len() as u64).to_le_bytes());
	for hash in &hashes {
		whole.extend_from_slice(&hash.to_le_bytes());
	}
	whole.extend_from_slice(&hash_bytes(&tail.0).to_le_bytes());
	let hash = hash_bytes(&whole);
	Arc::new(SceneKey { parts, tail: tail.0, hash })
}

/// Scenes this many parts or more keep their parts' keys for later scenes (a clip's frames; a small
/// scene encodes in no time).
const KEYS_KEPT_FROM: usize = 256;

/// Bytes hashed (foldhash with a fixed seed), to tell keys apart before comparing them whole.
fn hash_bytes(bytes: &[u8]) -> u64 {
	use std::hash::{BuildHasher, Hasher};
	let mut hasher = foldhash::quality::FixedState::with_seed(0x726f_725f_6b65_7973).build_hasher();
	hasher.write(bytes);
	hasher.finish()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::{Decal, Highlight, Skeleton};

	fn key(scene: &Scene, neon_share: f32, light_grid: bool) -> Vec<u8> {
		super::key(scene, neon_share, light_grid).flat()
	}

	fn scene() -> Scene {
		let document =
			crate::scene::load::read_bytes(br#"<roblox version="4"><Item class="Part" referent="RBX0"><Properties><string name="Name">Box</string></Properties></Item></roblox>"#)
				.unwrap();
		crate::scene::extract::extract(&document)
	}

	fn changed(base: &Scene, change: impl FnOnce(&mut Scene)) {
		let mut other = base.clone();
		change(&mut other);
		assert_ne!(key(base, 1.0, true), key(&other, 1.0, true));
	}

	#[test]
	fn edits_to_parts_and_materials_invalidate_preparation() {
		let base = scene();
		assert_eq!(base.parts.len(), 1);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).cframe.translation.x += 1.0);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).size.y += 1.0);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).transparency = 0.6);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).cast_shadow = false);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).geometry = Geometry::Ball);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).color.x = 0.2);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).texture = Some("texture".into()));
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).surfaces[0] = 3);
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).seed ^= 1);
		let decal = Decal { face: 0, texture: "decal".into(), color: Vec3::ONE, transparency: 0.5, z_index: 1, tiling: None, uv_offset: [0.0; 2], uv_scale: [1.0; 2] };
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).decals.push(decal.clone()));
		let mut with_decal = base.clone();
		Arc::make_mut(&mut with_decal.parts[0]).decals.push(decal);
		changed(&with_decal, |scene| Arc::make_mut(&mut scene.parts[0]).decals[0].uv_offset[1] = 0.5);
		changed(&with_decal, |scene| Arc::make_mut(&mut scene.parts[0]).decals[0].uv_scale[1] = 0.75);
		changed(&base, |scene| scene.materials.use_2022 = false);
		changed(&base, |scene| scene.materials.overrides.push((256, "Variant".into())));
		changed(&base, |scene| scene.kept_glass.push(0));
		// a ViewportFrame's MeshParts draw their vertex alpha
		changed(&base, |scene| scene.in_viewport_frame(crate::scene::environment::ViewportLighting { ambient: Vec3::ONE, light_color: Vec3::ZERO, light_direction: Vec3::NEG_Y, sky: None }));
		changed(&base, |scene| {
			scene.highlights.push(Highlight { parts: vec![0], fill_color: Vec3::ONE, fill_transparency: 0.0, outline_color: Vec3::ONE, outline_transparency: 0.0, depth_mode: 0 })
		});
		assert_ne!(key(&base, 1.0, true), key(&base, 0.5, true));
		assert_ne!(key(&base, 1.0, true), key(&base, 1.0, false));
	}

	#[test]
	fn key_preserves_float_bits_and_skeleton_content() {
		let mut base = scene();
		Arc::make_mut(&mut base.parts[0]).reflectance = 0.0;
		changed(&base, |scene| Arc::make_mut(&mut scene.parts[0]).reflectance = -0.0);
		Arc::make_mut(&mut base.parts[0]).skeleton = Some(Arc::new(Skeleton { bones: [("Bone".into(), Affine3A::IDENTITY)].into() }));
		changed(&base, |scene| Arc::make_mut(Arc::make_mut(&mut scene.parts[0]).skeleton.as_mut().unwrap()).bones.get_mut("Bone").unwrap().translation.x += 1.0);
		assert_eq!(key(&base, 1.0, true), key(&base.clone(), 1.0, true));
	}

	#[test]
	fn lighting_and_highlight_style_are_prepared_per_frame() {
		let mut base = scene();
		base.highlights.push(Highlight { parts: vec![0], fill_color: Vec3::ONE, fill_transparency: 0.0, outline_color: Vec3::ONE, outline_transparency: 0.0, depth_mode: 0 });
		let mut other = base.clone();
		other.environment.clock_time = 18.0;
		other.highlights[0].fill_color = Vec3::ZERO;
		assert_eq!(key(&base, 1.0, true), key(&other, 1.0, true));
	}
}

#[cfg(test)]
mod clip_tests {
	use std::sync::Arc;

	use glam::{Affine3A, Vec3};

	use super::super::camera::CameraSpec;
	use super::super::{ClipKept, Renderer, ViewRequest};

	/// A clip's frames prepared from its last frame (its unchanged parts taken by their bytes, the
	/// layout patched) draw what each frame drawn alone does: parts moving, one turning see-through,
	/// one more part (laid out anew), and back.
	#[test]
	fn a_clip_frame_from_the_last_one_draws_as_it_alone() {
		let state = std::env::temp_dir().join(format!("ror-clip-geometry-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let scene_at = |x: f32, see_through: bool, extra: bool| {
			let mut model = crate::scene::build::ModelBuilder::new("Row");
			let part = |name: &str, at: Vec3, size: Vec3| crate::scene::build::PartSpec { name: name.into(), cframe: Affine3A::from_translation(at), size, ..Default::default() };
			model.part(&part("Floor", Vec3::new(0.0, -0.5, 0.0), Vec3::new(40.0, 1.0, 40.0)));
			for index in 0..12 {
				model.part(&part(&format!("Post{index}"), Vec3::new(index as f32 * 3.0 - 16.0, 2.0, 6.0), Vec3::new(1.0, 4.0, 1.0)));
			}
			model.part(&crate::scene::build::PartSpec { transparency: if see_through { 0.5 } else { 0.0 }, ..part("Block", Vec3::new(x, 1.0, 2.0), Vec3::new(2.0, 2.0, 2.0)) });
			model.part(&part("Ball", Vec3::new(-x, 3.0, 0.0), Vec3::new(1.5, 1.5, 1.5)));
			if extra {
				model.part(&part("Extra", Vec3::new(3.0, 1.0, -2.0), Vec3::new(1.0, 1.0, 1.0)));
			}
			crate::scene::extract::extract(&model.into_document())
		};
		let request = ViewRequest { width: 256, height: 144, camera: CameraSpec::LookAt { eye: [0.0, 8.0, -16.0], target: [0.0, 1.0, 2.0], fov: 70.0 }, ..Default::default() };
		let kept = ClipKept::new();
		let frame = |scene: &crate::scene::Scene, kept: Option<&ClipKept>| {
			let sky = renderer.prepare_environment(scene);
			let (mut views, mut stats) = renderer.submit_frame(scene, std::slice::from_ref(&request), sky, None, Default::default(), None, kept.map(|kept| (kept, false))).unwrap();
			views.pop().unwrap().finish(&mut stats).unwrap().rgba
		};
		let mut patched = 0;
		for (x, see_through, extra) in [(-6.0, false, false), (-4.0, false, false), (-2.0, false, false), (-2.0, true, false), (0.0, true, false), (1.0, false, true), (2.0, false, true), (3.0, false, false)] {
			let scene = scene_at(x, see_through, extra);
			let from_last = frame(&scene, Some(&kept));
			assert!(from_last == frame(&scene, None), "the frame at {x} (see-through {see_through}, extra {extra}) from the last frame is the one drawn alone");
			patched += usize::from(kept.geometry.last.lock().as_ref().is_some_and(|last| last.moving));
		}
		assert!(patched >= 4, "frames were made from the last frame ({patched})");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
