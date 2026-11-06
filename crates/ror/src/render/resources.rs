//! GPU resource caches shared by every render in the process: meshes, textures and material bind
//! groups. A resource is loaded once even when many renders ask for it at the same time (the
//! others wait for the first), and least-recently-used entries are dropped past a byte budget.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use glam::Vec3;
use parking_lot::Mutex;
use wgpu::util::DeviceExt;

use super::geometry::CpuMesh;
use super::gpu::Gpu;

pub struct GpuMesh {
	pub vertex_buffer: wgpu::Buffer,
	pub index_buffer: wgpu::Buffer,
	pub lods: Vec<Range<u32>>,
	/// The file's count of high-quality levels ([`super::geometry::CpuMesh::high_quality`])
	pub high_quality: u8,
	pub bounds: (Vec3, Vec3),
	/// The box of what it draws (mesh space): `bounds`, but a posed skinned copy's own (its `bounds`
	/// stay the bind pose's, which the part's mesh transform and its decals follow).
	pub drawn: (Vec3, Vec3),
	/// A posed skinned copy's bind mesh vertices (the same layout and order): its decals project from
	/// them (`vs_decal_bind`).
	pub bind: Option<wgpu::Buffer>,
	/// The triangles of its finest level a decal on each box face may draw on ([`DecalFaces`]; `None`
	/// on a posed copy, whose decals draw on every triangle)
	pub decal_faces: Option<Arc<DecalFaces>>,
	pub bytes: u64,
}

/// The triangles of a mesh's finest level (`lods[0]`) a decal may draw on, per box face (NormalId
/// order, mesh.wgsl `FACE_NORMAL`), as runs of index ranges in the mesh's own order: a decal draws
/// only those, the shader's discard (`decal_left_out`) still deciding each fragment. A triangle is
/// left out only where no fragment of it could pass that discard:
/// - `normal`, a decal projected through its part's box (blocks, wedges, unions, MeshParts): in the
///   part's space the mesh's normals are its own divided by a positive scale per axis (the part's
///   mesh transform is a positive scale, `Renderer::decal_runs` checks), so a face can face the
///   fragment, or be the one its normal points most nearly at, only where some vertex normal has a
///   positive component along it. Left out: every vertex normal's component along the face 0 or
///   against it, and the three normals' components of one sign or 0 on every axis (so no blend of
///   them, however the axes scale, comes near zero length, where its direction would be rounding);
/// - `code`, a Ball's or Cylinder's face-only decal: the face named in the vertex alpha, its
///   interpolation between the triangle's vertices' codes.
pub struct DecalFaces {
	pub normal: [Arc<[Range<u32>]>; 6],
	pub code: [Arc<[Range<u32>]>; 6],
}

/// The most runs a decal draws (each a draw call): runs closer than this merge first, the triangles
/// between them drawn too.
const DECAL_RUNS: usize = 16;

impl DecalFaces {
	fn of(mesh: &CpuMesh) -> Option<Arc<Self>> {
		let range = mesh.lods.first()?.clone();
		let indices = mesh.indices.get(range.start as usize..range.end as usize)?;
		if !indices.len().is_multiple_of(3) || indices.iter().any(|&index| index as usize >= mesh.vertices.len()) {
			return None;
		}
		// per triangle: bit f the normal test's face f, bit 6 + f the code's; a run opens where its bit
		// turns on and closes where it turns off (most triangles change no bit)
		let mut runs: [Vec<Range<u32>>; 12] = Default::default();
		let mut open = [0u32; 12];
		let mut previous = 0u32;
		let close = |runs: &mut [Vec<Range<u32>>; 12], open: &[u32; 12], changed: u32, mask: u32, at: u32| {
			let mut bits = changed;
			while bits != 0 {
				let bit = bits.trailing_zeros() as usize;
				bits &= bits - 1;
				if mask & (1 << bit) == 0 {
					runs[bit].push(open[bit]..at);
				}
			}
		};
		for (triangle, corners) in indices.chunks_exact(3).enumerate() {
			let vertices = [&mesh.vertices[corners[0] as usize], &mesh.vertices[corners[1] as usize], &mesh.vertices[corners[2] as usize]];
			let (mut positive, mut negative, mut short) = (0u32, 0u32, false);
			let (mut low, mut high) = (8u32, 0u32);
			for vertex in vertices {
				let normal = vertex.normal;
				short |= normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2] < 0.25;
				for (axis, component) in normal.iter().enumerate() {
					positive |= u32::from(*component > 0.0) << axis;
					negative |= u32::from(*component < 0.0) << axis;
				}
				let code = u32::from(vertex.color[3]).min(8);
				(low, high) = (low.min(code), high.max(code));
			}
			// a normal too short to point anywhere, or an axis the normals take both signs on: any face
			let unsure = short || positive & negative != 0;
			let normal_faces = if unsure { 0b11_1111 } else { positive | negative << 3 };
			// the faces from low to high, of the six
			let code_faces = if low <= high && low < 6 { ((1u32 << (high.min(5) + 1)) - 1) & !((1u32 << low) - 1) } else { 0 };
			let mask = normal_faces | code_faces << 6;
			let at = range.start + triangle as u32 * 3;
			let changed = mask ^ previous;
			if changed != 0 {
				close(&mut runs, &open, changed, mask, at);
				let mut opened = changed & mask;
				while opened != 0 {
					let bit = opened.trailing_zeros() as usize;
					opened &= opened - 1;
					open[bit] = at;
				}
				previous = mask;
			}
		}
		close(&mut runs, &open, previous, 0, range.end);
		let merged = |mut list: Vec<Range<u32>>| -> Arc<[Range<u32>]> {
			while list.len() > DECAL_RUNS {
				// the smallest gaps closed, at least half of those over the limit each round
				let mut gaps: Vec<u32> = list.windows(2).map(|pair| pair[1].start - pair[0].end).collect();
				gaps.sort_unstable();
				let close = gaps[(list.len() - DECAL_RUNS - 1).min(gaps.len() - 1)];
				let mut joined: Vec<Range<u32>> = Vec::with_capacity(list.len());
				for run in list {
					match joined.last_mut() {
						Some(last) if run.start - last.end <= close => last.end = run.end,
						_ => joined.push(run),
					}
				}
				list = joined;
			}
			list.into()
		};
		let mut runs = runs.into_iter().map(merged);
		let normal = std::array::from_fn(|_| runs.next().expect("six normal faces"));
		let code = std::array::from_fn(|_| runs.next().expect("six code faces"));
		Some(Arc::new(Self { normal, code }))
	}
}

impl GpuMesh {
	pub fn upload(gpu: &Gpu, label: &str, mesh: &CpuMesh) -> Self {
		let vertex_buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
			label: Some(label),
			contents: bytemuck::cast_slice(&mesh.vertices),
			usage: wgpu::BufferUsages::VERTEX,
		});
		let index_buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
			label: Some(label),
			contents: bytemuck::cast_slice(&mesh.indices),
			usage: wgpu::BufferUsages::INDEX,
		});
		let bytes = (std::mem::size_of_val(mesh.vertices.as_slice()) + std::mem::size_of_val(mesh.indices.as_slice())) as u64;
		// staged until a submission carries them, as queued writes are
		gpu.uploaded(bytes);
		Self { vertex_buffer, index_buffer, lods: mesh.lods.clone(), high_quality: mesh.high_quality, bounds: mesh.bounds, drawn: mesh.bounds, bind: None, decal_faces: DecalFaces::of(mesh), bytes }
	}
}

/// A texture on the GPU. One kept on disk as the GPU takes it (render::texture_files) goes up only
/// once something asks for its levels, and a material map then only from the finest level a view
/// samples (render::residency): [`GpuTexture::levels_from`].
pub struct GpuTexture {
	/// Its full size (its finest level's), whatever levels the GPU holds
	pub width: u32,
	pub height: u32,
	pub has_alpha: bool,
	/// A DXT5nm normal map: X in alpha, Y in green, Z reconstructed.
	pub dxt5nm: bool,
	/// A two-channel (BC5) normal map: X in red, Y in green, Z reconstructed.
	pub normal_rg: bool,
	/// Its levels' bytes, all of them
	pub full_bytes: u64,
	/// What the GPU holds of it (`None`: nothing yet, a texture on disk not asked for)
	held: parking_lot::RwLock<Option<Held>>,
	/// Where its levels are read from when the GPU may hold fewer than all
	source: Option<super::texture_files::Source>,
	/// Its slot in a view's residency feedback (render::residency), 0 when it is always held whole
	pub id: u32,
}

/// The levels of a texture the GPU holds: its level `base` down to its coarsest, as a texture of
/// their own (its level 0 the texture's level `base`).
#[derive(Clone)]
pub struct Held {
	pub texture: wgpu::Texture,
	pub view: wgpu::TextureView,
	pub base: u32,
	pub bytes: u64,
}

impl Drop for GpuTexture {
	fn drop(&mut self) {
		if self.id != 0 {
			super::residency::give_back_id(self.id);
		}
	}
}

/// What a texture is sampled as. All are uploaded raw (UNORM): the shader decodes colour itself (a
/// square, or sRGB for material maps). Normal and single-channel maps from RGBA8 sources are
/// compressed on upload (BC5 / BC4: 4x / 8x smaller, error under one fourteenth of a block's range),
/// so large scenes' textures fit the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureUse {
	Color,
	Data,
	/// A normal map (BC5 of X and Y when compressible).
	Normal,
	/// A single-channel map read from red: roughness, metalness (BC4 when compressible).
	Scalar,
}

impl TextureUse {
	pub fn is_normal(self) -> bool {
		self == TextureUse::Normal
	}
}

impl GpuTexture {
	/// `image` as [`GpuTexture::upload`] puts it on the GPU: block-compressed as it is where the device
	/// takes BC and its size is whole blocks, else expanded to RGBA8.
	pub fn uploadable<'a>(gpu: &Gpu, image: &'a ror_image::Image) -> std::borrow::Cow<'a, ror_image::Image> {
		super::texels::uploadable(image, gpu.supports_bc())
	}

	pub fn upload(gpu: &Gpu, label: &str, image: &ror_image::Image, usage: TextureUse) -> Result<Self> {
		use ror_image::PixelFormat;
		let image = &*Self::uploadable(gpu, image);
		if image.format != PixelFormat::Rgba8 && image.format.block_bytes().is_none() {
			return Err(anyhow!("{label}: unsupported pixel format {:?}", image.format));
		}
		let format = texture_format(image.format);
		let levels = image.levels.len().max(1) as u32;
		let max_dimension = gpu.device.limits().max_texture_dimension_2d;
		if image.width == 0 || image.height == 0 || image.width > max_dimension || image.height > max_dimension {
			return Err(anyhow!("{label}: texture size {}x{} unsupported", image.width, image.height));
		}
		let size = wgpu::Extent3d { width: image.width, height: image.height, depth_or_array_layers: 1 };
		let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
			label: Some(label),
			size,
			mip_level_count: levels,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});
		let mut bytes = 0u64;
		for (level, data) in image.levels.iter().enumerate() {
			let width = (image.width >> level).max(1);
			let height = (image.height >> level).max(1);
			let (bytes_per_row, rows) = match image.format.block_bytes() {
				Some(block) => (width.div_ceil(4) * block as u32, height.div_ceil(4)),
				None => (width * 4, height),
			};
			if (data.len() as u64) < bytes_per_row as u64 * rows as u64 {
				break;
			}
			// Block-compressed levels below 4x4 still occupy whole blocks; the copy size is the
			// physical (block-rounded) size.
			let copy = match image.format.block_bytes() {
				Some(_) => wgpu::Extent3d { width: width.div_ceil(4) * 4, height: height.div_ceil(4) * 4, depth_or_array_layers: 1 },
				None => wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			};
			gpu.queue.write_texture(
				wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				data,
				wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(rows) },
				copy,
			);
			bytes += data.len() as u64;
		}
		gpu.uploaded(bytes);
		let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
		Ok(Self::whole(texture, view, image.width, image.height, [image.has_alpha, image.dxt5nm, usage.is_normal() && image.format == ror_image::PixelFormat::Bc5], bytes))
	}

	/// A texture the GPU holds whole: `[has_alpha, dxt5nm, normal_rg]`, its levels' bytes.
	pub fn whole(texture: wgpu::Texture, view: wgpu::TextureView, width: u32, height: u32, [has_alpha, dxt5nm, normal_rg]: [bool; 3], bytes: u64) -> Self {
		Self { width, height, has_alpha, dxt5nm, normal_rg, full_bytes: bytes, held: parking_lot::RwLock::new(Some(Held { texture, view, base: 0, bytes })), source: None, id: 0 }
	}

	/// A texture whose levels are read from `source` once asked for, with its slot in the residency
	/// feedback.
	pub(super) fn on_disk(source: super::texture_files::Source, width: u32, height: u32, [has_alpha, dxt5nm, normal_rg]: [bool; 3], full_bytes: u64) -> Self {
		Self { width, height, has_alpha, dxt5nm, normal_rg, full_bytes, held: parking_lot::RwLock::new(None), source: Some(source), id: super::residency::take_id() }
	}

	/// What the GPU holds of it now.
	pub fn held(&self) -> Option<Held> {
		self.held.read().clone()
	}

	/// Where its levels are read from, when the GPU may hold fewer than all.
	pub(super) fn source(&self) -> Option<&super::texture_files::Source> {
		self.source.as_ref()
	}

	/// `held` (read from its source, its copies submitted) is what it holds now, unless it holds finer
	/// levels already.
	pub(super) fn hold(&self, held: Held) {
		let mut current = self.held.write();
		if current.as_ref().is_none_or(|current| current.base > held.base) {
			*current = Some(held);
		}
	}

	/// Its levels as the GPU holds them (a texture asked for whole: [`super::Renderer::load_whole`],
	/// or made on the GPU).
	pub fn view(&self) -> wgpu::TextureView {
		self.held.read().as_ref().map(|held| held.view.clone()).expect("a texture's levels used before they were read")
	}

	/// The bytes the GPU holds of it.
	pub fn bytes(&self) -> u64 {
		self.held.read().as_ref().map_or(0, |held| held.bytes)
	}

	/// The coarsest level the GPU may hold it from: one whose texels sample as the full texture's at
	/// any level from it on (its sizes halve evenly down to it, in whole blocks when compressed); 0
	/// for a texture always held whole.
	pub fn max_base(&self) -> u32 {
		self.source.as_ref().map_or(0, |source| source.max_base)
	}

	/// Its levels from `base` on (at most [`GpuTexture::max_base`]) on the GPU, read now if it holds
	/// fewer: what it held finer stays (a texture's GPU copy only ever gains levels). `room` refuses a
	/// read the GPU has no room for.
	pub fn levels_from(&self, gpu: &Gpu, base: u32, room: &dyn Fn(u64) -> Result<()>) -> Result<Held> {
		if let Some(held) = self.held.read().as_ref().filter(|held| held.base <= base) {
			return Ok(held.clone());
		}
		let Some(source) = &self.source else {
			return self.held().ok_or_else(|| anyhow!("a texture without levels"));
		};
		let base = base.min(source.max_base);
		let mut file = source.file.lock();
		// read meanwhile by another view
		if let Some(held) = self.held.read().as_ref().filter(|held| held.base <= base) {
			return Ok(held.clone());
		}
		let held = source.read(gpu, &mut file, base, room)?;
		*self.held.write() = Some(held.clone());
		Ok(held)
	}

	/// A 1x1 texture of one RGBA8 value.
	pub fn solid(gpu: &Gpu, label: &str, rgba: [u8; 4], usage: TextureUse) -> Self {
		let image = ror_image::Image::from_rgba8(1, 1, rgba.to_vec());
		Self::upload(gpu, label, &image, usage).expect("1x1 texture")
	}
}

/// The texture format a pixel format uploads as (raw UNORM: the shader decodes colour itself).
pub fn texture_format(format: ror_image::PixelFormat) -> wgpu::TextureFormat {
	use ror_image::PixelFormat;
	match format {
		PixelFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
		PixelFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
		PixelFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
		PixelFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
		PixelFormat::Bc4 => wgpu::TextureFormat::Bc4RUnorm,
		PixelFormat::Bc5 => wgpu::TextureFormat::Bc5RgUnorm,
		PixelFormat::Bc7 => wgpu::TextureFormat::Bc7RgbaUnorm,
	}
}

/// Something a [`Cache`] can account for.
pub trait Weighed {
	fn bytes(&self) -> u64;
}

impl Weighed for GpuMesh {
	fn bytes(&self) -> u64 {
		self.bytes
	}
}

impl Weighed for GpuTexture {
	fn bytes(&self) -> u64 {
		// one on disk the GPU holds nothing of yet still holds its file, a little
		GpuTexture::bytes(self).max(if self.source.is_some() { 4096 } else { 0 })
	}
}

struct Entry<V> {
	cell: Arc<OnceLock<std::result::Result<Arc<V>, String>>>,
	last_used: u64,
	created: std::time::Instant,
}

/// How long a failed load is remembered before the next request retries it.
const FAILURE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A keyed cache with load de-duplication and an LRU byte budget.
pub struct Cache<K, V> {
	entries: Mutex<HashMap<K, Entry<V>>>,
	tick: AtomicU64,
	budget: u64,
	pub hits: AtomicU64,
	pub misses: AtomicU64,
}

impl<K: Eq + Hash + Clone, V: Weighed> Cache<K, V> {
	pub fn new(budget: u64) -> Self {
		Self { entries: Mutex::new(HashMap::new()), tick: AtomicU64::new(0), budget, hits: AtomicU64::new(0), misses: AtomicU64::new(0) }
	}

	/// The cached value for `key`, loading it with `load` if absent. Concurrent callers with the
	/// same key share one load; a failure is remembered for [`FAILURE_TTL`], then retried.
	pub fn get_or_load(&self, key: &K, load: impl FnOnce() -> Result<V>) -> Result<Arc<V>> {
		let tick = self.tick.fetch_add(1, Ordering::Relaxed);
		let cell = {
			let mut entries = self.entries.lock();
			// a hit neither clones the key nor allocates while the lock is held
			match entries.get_mut(key) {
				Some(entry) if !(matches!(entry.cell.get(), Some(Err(_))) && entry.created.elapsed() > FAILURE_TTL) => {
					entry.last_used = tick;
					entry.cell.clone()
				}
				_ => {
					let cell = Arc::new(OnceLock::new());
					entries.insert(key.clone(), Entry { cell: cell.clone(), last_used: tick, created: std::time::Instant::now() });
					cell
				}
			}
		};
		let mut loaded = false;
		let result = cell
			.get_or_init(|| {
				loaded = true;
				load().map(Arc::new).map_err(|error| format!("{error:#}"))
			})
			.clone();
		if loaded {
			self.misses.fetch_add(1, Ordering::Relaxed);
		} else {
			self.hits.fetch_add(1, Ordering::Relaxed);
		}
		match result {
			Ok(value) => {
				if loaded {
					self.evict();
				}
				Ok(value)
			}
			Err(error) => Err(anyhow!(error)),
		}
	}

	/// The loaded value for `key`, without loading it (callers that load many at once).
	pub fn get(&self, key: &K) -> Option<Arc<V>> {
		let tick = self.tick.fetch_add(1, Ordering::Relaxed);
		let mut entries = self.entries.lock();
		let entry = entries.get_mut(key)?;
		let value = entry.cell.get()?.as_ref().ok()?.clone();
		entry.last_used = tick;
		self.hits.fetch_add(1, Ordering::Relaxed);
		Some(value)
	}

	pub fn bytes(&self) -> u64 {
		self.entries.lock().values().filter_map(|entry| entry.cell.get()).filter_map(|result| result.as_ref().ok()).map(|value| value.bytes()).sum()
	}

	/// The bytes of what no render holds: what the budget bounds, and what dropping frees.
	pub fn idle_bytes(&self) -> u64 {
		self.entries.lock().values().filter_map(Self::idle).sum()
	}

	/// An entry's bytes when only the cache holds its value.
	fn idle(entry: &Entry<V>) -> Option<u64> {
		match entry.cell.get() {
			Some(Ok(value)) if Arc::strong_count(value) == 1 => Some(value.bytes()),
			_ => None,
		}
	}

	pub fn len(&self) -> usize {
		self.entries.lock().len()
	}

	fn evict(&self) {
		self.shrink_to(self.budget);
	}

	/// Back within the budget: what renders held past it and have let go of since.
	pub fn fit(&self) -> u64 {
		self.shrink_to(self.budget)
	}

	/// Drops the least recently used entries no render holds until those left weigh at most `budget`
	/// bytes; returns the bytes dropped. What renders hold is neither counted nor dropped: dropped, it
	/// would free nothing until they let go, and the next request for it would load a second copy (a
	/// scene with more textures than the budget loaded some again for each part that drew them);
	/// counted, a scene held past the budget would leave no room for what its frames load and let go.
	pub fn shrink_to(&self, budget: u64) -> u64 {
		let mut entries = self.entries.lock();
		let mut total: u64 = entries.values().filter_map(Self::idle).sum();
		let before = total;
		if total <= budget {
			return 0;
		}
		let mut order: Vec<(u64, K)> = entries.iter().filter(|(_, entry)| matches!(entry.cell.get(), Some(Err(_))) || Self::idle(entry).is_some()).map(|(key, entry)| (entry.last_used, key.clone())).collect();
		order.sort_by_key(|(tick, _)| *tick);
		for (_, key) in order {
			if total <= budget {
				break;
			}
			if let Some(entry) = entries.remove(&key)
				&& let Some(Ok(value)) = entry.cell.get()
			{
				total = total.saturating_sub(value.bytes());
			}
		}
		before - total
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	struct Blob(u64);

	impl Weighed for Blob {
		fn bytes(&self) -> u64 {
			self.0
		}
	}

	#[test]
	fn the_budget_counts_and_drops_only_what_no_render_holds() {
		let cache: Cache<u32, Blob> = Cache::new(100);
		let held = cache.get_or_load(&1, || Ok(Blob(80))).unwrap();
		// held past the budget, it leaves room for 100 bytes nothing holds
		for key in 2..=4 {
			cache.get_or_load(&key, || Ok(Blob(40))).unwrap();
		}
		// (the one just loaded counts from the next load or fit: its caller held it during its own)
		cache.fit();
		assert_eq!(cache.idle_bytes(), 80, "the two most recent idle ones");
		assert!(cache.get(&2).is_none() && cache.get(&3).is_some() && cache.get(&4).is_some());
		// dropping everything leaves what is held, found again rather than loaded twice
		assert_eq!(cache.shrink_to(0), 80);
		let again = cache.get_or_load(&1, || panic!("loaded a second copy")).unwrap();
		assert!(Arc::ptr_eq(&held, &again));
		drop((held, again));
		assert_eq!(cache.fit(), 0, "within the budget once let go");
		assert_eq!(cache.shrink_to(0), 80);
		assert_eq!(cache.len(), 0);
	}
}

#[cfg(test)]
mod decal_tests {
	use super::*;

	fn triangles(runs: &[Range<u32>]) -> Vec<u32> {
		runs.iter().flat_map(|run| (run.start..run.end).step_by(3).map(|at| at / 3)).collect()
	}

	/// A block's decal draws on its own face's two triangles; a ball's face-only decal on the
	/// triangles its vertex alpha names.
	#[test]
	fn decals_draw_on_the_triangles_that_can_take_them() {
		let block = super::super::geometry::block();
		let faces = DecalFaces::of(&block).expect("runs");
		for face in 0..6usize {
			let (axis, sign) = (face % 3, if face < 3 { 1.0 } else { -1.0 });
			let kept = triangles(&faces.normal[face]);
			assert_eq!(kept.len(), 2, "face {face}");
			for triangle in kept {
				for corner in 0..3 {
					let normal = block.vertices[block.indices[triangle as usize * 3 + corner] as usize].normal;
					assert!(normal[axis] * sign > 0.0, "face {face} triangle {triangle}");
				}
			}
		}
		let ball = super::super::geometry::sphere(6);
		let faces = DecalFaces::of(&ball).expect("runs");
		let all = ball.lods[0].len() as u32 / 3;
		let mut total = 0;
		for face in 0..6usize {
			let kept = triangles(&faces.code[face]);
			assert!(!kept.is_empty() && (kept.len() as u32) < all, "face {face}: {} of {all}", kept.len());
			// every triangle whose vertices name the face is kept
			for triangle in 0..all {
				let named = (0..3).any(|corner| u32::from(ball.vertices[ball.indices[triangle as usize * 3 + corner] as usize].color[3]) == face as u32);
				assert!(!named || kept.contains(&triangle), "face {face} triangle {triangle}");
			}
			total += kept.len();
		}
		assert!(total as u32 >= all);
	}
}
