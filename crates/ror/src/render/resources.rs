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

/// Provenance of a cached resource with respect to local files on disk: what stamp its bytes were
/// proven to match at read time, so higher-level caches (materials, prepared parts/scenes, skies,
/// effect bindings) only keep what matches their own key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StampProof {
	/// Loaded from remote, built-in or embedded bytes (no local file read).
	#[default]
	Remote,
	/// Loaded from local file(s) and verified to match this exact stamped key (`content_key(uri)`).
	Proven(String),
	/// A local file moved while being read: valid for this draw only, never cached.
	Moved,
}

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
	/// Some vertex's alpha is under 1 (a FileMesh's or an EditableMesh's colours): a transparent MeshPart's
	/// shadows are cut by it (`fs_shadow_cutout`)
	pub vertex_alpha: bool,
	pub bytes: u64,
	/// Local-file stamp provenance for this mesh.
	pub proof: StampProof,
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
		Self {
			vertex_buffer,
			index_buffer,
			lods: mesh.lods.clone(),
			high_quality: mesh.high_quality,
			bounds: mesh.bounds,
			drawn: mesh.bounds,
			bind: None,
			decal_faces: DecalFaces::of(mesh),
			vertex_alpha: mesh.vertices.iter().any(|vertex| vertex.color[3] < 255),
			bytes,
			proof: StampProof::Remote,
		}
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
	/// Local-file stamp provenance for this texture.
	pub proof: StampProof,
	/// An image asset's upload drawn in place of its transcode for now (Roblox still making it, none
	/// cached offline, not fetched): why, as a render reports it. Kept as a failure is, for
	/// [`FAILURE_TTL`], and never written to disk.
	pub stand_in: Option<Arc<str>>,
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
		Self { width, height, has_alpha, dxt5nm, normal_rg, full_bytes: bytes, held: parking_lot::RwLock::new(Some(Held { texture, view, base: 0, bytes })), source: None, id: 0, proof: StampProof::Remote, stand_in: None }
	}

	/// A texture whose levels are read from `source` once asked for, with its slot in the residency
	/// feedback.
	pub(super) fn on_disk(source: super::texture_files::Source, width: u32, height: u32, [has_alpha, dxt5nm, normal_rg]: [bool; 3], full_bytes: u64) -> Self {
		Self { width, height, has_alpha, dxt5nm, normal_rg, full_bytes, held: parking_lot::RwLock::new(None), source: Some(source), id: super::residency::take_id(), proof: StampProof::Remote, stand_in: None }
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
	/// A stand-in for what should have loaded ([`GpuTexture::stand_in`]): kept as a failure is, for
	/// [`FAILURE_TTL`], then loaded again.
	fn stand_in(&self) -> bool {
		false
	}
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

	fn stand_in(&self) -> bool {
		self.stand_in.is_some()
	}
}

/// Result of a verified cache load ([`Cache::get_or_load_checked`]).
pub struct LoadOutcome<V> {
	pub value: V,
	/// When `false` (e.g. a local file moved while being read), `value` is returned to the caller
	/// for this draw alone without keeping it in the cache or handing it to concurrent waiters.
	pub keep: bool,
	/// True when an offline view fell back to a substitute (never promoted into the shared cache;
	/// increments `offline_errors`).
	pub offline_fallback: bool,
}

/// Failure from a verified cache load: `Cached` is remembered for [`FAILURE_TTL`], while
/// `Uncached` (e.g. a local file moved mid-read) is removed immediately so the next request retries.
pub enum LoadError {
	Cached(anyhow::Error),
	Uncached(anyhow::Error),
}

enum CellState<V> {
	Cached(Arc<V>),
	/// Leader returned an uncached value (moving local file or offline fallback); waiters retry.
	Uncached,
	CachedErr(String),
	/// Leader hit a transient uncached failure; waiters retry.
	UncachedErr,
}

impl<V> Clone for CellState<V> {
	fn clone(&self) -> Self {
		match self {
			Self::Cached(value) => Self::Cached(value.clone()),
			Self::Uncached => Self::Uncached,
			Self::CachedErr(error) => Self::CachedErr(error.clone()),
			Self::UncachedErr => Self::UncachedErr,
		}
	}
}

struct Entry<V> {
	cell: Arc<OnceLock<CellState<V>>>,
	last_used: u64,
	created: std::time::Instant,
}

impl<V: Weighed> Entry<V> {
	/// A stand-in ([`Weighed::stand_in`]): retried after [`FAILURE_TTL`], as a failure is.
	fn holds_stand_in(&self) -> bool {
		matches!(self.cell.get(), Some(CellState::Cached(value)) if value.stand_in())
	}

	fn stale_stand_in(&self) -> bool {
		self.holds_stand_in() && self.created.elapsed() > FAILURE_TTL
	}
}

/// How long a failed load is remembered before the next request retries it.
const FAILURE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// A keyed cache with load de-duplication and an LRU byte budget.
pub struct Cache<K, V> {
	entries: Mutex<HashMap<K, Entry<V>>>,
	/// In-flight loads and cached failures from offline views on an online service (`offline_view`).
	/// Kept separate from `entries` so offline misses deduplicate across concurrent threads and
	/// repeat frames (with [`FAILURE_TTL`]) without blocking or poisoning online loads; offline
	/// successes are promoted into `entries` and removed here so this map holds zero GPU bytes at rest.
	offline: Mutex<HashMap<K, Entry<V>>>,
	tick: AtomicU64,
	budget: u64,
	pub hits: AtomicU64,
	pub misses: AtomicU64,
	/// Failures let go of before [`FAILURE_TTL`] ([`Cache::shrink_to`]): their next request retries them
	dropped_failures: AtomicU64,
	/// Offline-view failures let go of or superseded by a later online success
	dropped_offline_failures: AtomicU64,
	/// Offline-view misses or isolated fallbacks seen so far, so callers can tell when an offline
	/// prepare took a silent fallback path even if no user-visible `missing` entry was recorded.
	offline_errors: AtomicU64,
}

impl<K: Eq + Hash + Clone, V: Weighed> Cache<K, V> {
	pub fn new(budget: u64) -> Self {
		Self {
			entries: Mutex::new(HashMap::new()),
			offline: Mutex::new(HashMap::new()),
			tick: AtomicU64::new(0),
			budget,
			hits: AtomicU64::new(0),
			misses: AtomicU64::new(0),
			dropped_failures: AtomicU64::new(0),
			dropped_offline_failures: AtomicU64::new(0),
			offline_errors: AtomicU64::new(0),
		}
	}

	/// Monotonic count of offline-view misses and isolated fallbacks seen by this cache.
	pub fn offline_errors(&self) -> u64 {
		self.offline_errors.load(Ordering::Relaxed)
	}

	/// When the first failure it remembers now is retried (`None`: it remembers none), and
	/// [`Cache::dropped_failures`]: what a preparation shaped by its failures may stand for a fresh one
	/// until (render::prepared::Failures).
	pub fn failures(&self) -> (Option<std::time::Instant>, u64) {
		self.failures_for(false)
	}

	/// Like [`Cache::failures`], including isolated offline-view failures when `offline_view` is `true`.
	pub fn failures_for(&self, offline_view: bool) -> (Option<std::time::Instant>, u64) {
		let now = std::time::Instant::now();
		let mut first = {
			let entries = self.entries.lock();
			entries.values().filter(|entry| matches!(entry.cell.get(), Some(CellState::CachedErr(_))) || entry.holds_stand_in()).map(|entry| entry.created + FAILURE_TTL).filter(|retried| *retried > now).min()
		};
		if offline_view {
			let offline = self.offline.lock();
			let offline_first = offline.values().filter(|entry| matches!(entry.cell.get(), Some(CellState::CachedErr(_)))).map(|entry| entry.created + FAILURE_TTL).filter(|retried| *retried > now).min();
			first = match (first, offline_first) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(a, b) => a.or(b),
			};
		}
		(first, self.dropped_failures_for(offline_view))
	}

	/// How many failures it has let go of before their time ([`Cache::shrink_to`]).
	pub fn dropped_failures(&self) -> u64 {
		self.dropped_failures.load(Ordering::Relaxed)
	}

	/// Like [`Cache::dropped_failures`], including isolated offline-view failures when `offline_view` is `true`.
	pub fn dropped_failures_for(&self, offline_view: bool) -> u64 {
		self.dropped_failures() + if offline_view { self.dropped_offline_failures.load(Ordering::Relaxed) } else { 0 }
	}

	/// The cached value for `key`, loading it with `load` if absent. Concurrent callers with the
	/// same key share one load; a failure is remembered for [`FAILURE_TTL`], then retried.
	pub fn get_or_load(&self, key: &K, load: impl FnOnce() -> Result<V>) -> Result<Arc<V>> {
		self.get_or_load_for(key, false, load)
	}

	/// Like [`Cache::get_or_load`], isolating misses when `offline_view` is `true` (an offline view
	/// on an online service): successes share the main cache in both directions, while offline
	/// in-flight loads and [`FAILURE_TTL`] failures stay in `offline` so they deduplicate across
	/// concurrent offline threads and repeat frames without poisoning online loads.
	pub fn get_or_load_for(&self, key: &K, offline_view: bool, load: impl FnOnce() -> Result<V>) -> Result<Arc<V>> {
		self.get_or_load_checked(key, offline_view, || load().map(|value| LoadOutcome { value, keep: true, offline_fallback: false }).map_err(LoadError::Cached))
	}

	/// Like [`Cache::get_or_load_for`], where `load` also signals `offline_fallback` (`true` when an
	/// offline view fell back to a substitute such as an uploaded PNG in place of an uncached
	/// transcode). An `offline_fallback` value is returned to the caller without promoting it into the
	/// shared cache or keeping GPU bytes in `offline`, and increments [`Cache::offline_errors`].
	#[cfg(test)]
	pub fn get_or_load_isolated(&self, key: &K, offline_view: bool, load: impl FnOnce() -> Result<(V, bool)>) -> Result<Arc<V>> {
		self.get_or_load_checked(key, offline_view, || {
			load()
				.map(|(value, offline_fallback)| LoadOutcome { value, keep: !offline_fallback, offline_fallback })
				.map_err(LoadError::Cached)
		})
	}

	/// Like [`Cache::get_or_load_for`], where `load` proves whether its result may be kept under
	/// `key`. When `keep` is `false` (e.g. a local file moved while being read) or `LoadError::Uncached`
	/// is returned, the entry is removed immediately and any concurrent waiter retries rather than
	/// inheriting the moving read.
	pub fn get_or_load_checked(&self, key: &K, offline_view: bool, load: impl FnOnce() -> std::result::Result<LoadOutcome<V>, LoadError>) -> Result<Arc<V>> {
		let mut load = Some(load);
		loop {
			let tick = self.tick.fetch_add(1, Ordering::Relaxed);
			if offline_view {
				{
					let mut entries = self.entries.lock();
					if let Some(entry) = entries.get_mut(key) {
						let stale = entry.stale_stand_in();
						match entry.cell.get() {
							Some(CellState::Cached(value)) if !stale => {
								entry.last_used = tick;
								self.hits.fetch_add(1, Ordering::Relaxed);
								return Ok(value.clone());
							}
							Some(CellState::CachedErr(error)) if entry.created.elapsed() <= FAILURE_TTL => {
								entry.last_used = tick;
								self.hits.fetch_add(1, Ordering::Relaxed);
								return Err(anyhow!(error.clone()));
							}
							_ => {}
						}
					}
				}
				let cell = {
					let mut offline = self.offline.lock();
					match offline.get_mut(key) {
						Some(entry) if !matches!(entry.cell.get(), Some(CellState::Uncached | CellState::UncachedErr)) && !(matches!(entry.cell.get(), Some(CellState::CachedErr(_))) && entry.created.elapsed() > FAILURE_TTL) => {
							entry.last_used = tick;
							entry.cell.clone()
						}
						_ => {
							let cell = Arc::new(OnceLock::new());
							offline.insert(key.clone(), Entry { cell: cell.clone(), last_used: tick, created: std::time::Instant::now() });
							cell
						}
					}
				};
				let mut loaded = false;
				let mut offline_fallback = false;
				let mut uncached_value: Option<Arc<V>> = None;
				let mut uncached_error: Option<anyhow::Error> = None;
				let state = cell
					.get_or_init(|| {
						loaded = true;
						match (load.take().expect("leader runs load once"))() {
							Ok(outcome) => {
								offline_fallback = outcome.offline_fallback;
								let value = Arc::new(outcome.value);
								if outcome.keep && !outcome.offline_fallback {
									CellState::Cached(value)
								} else {
									uncached_value = Some(value);
									CellState::Uncached
								}
							}
							Err(LoadError::Cached(error)) => CellState::CachedErr(format!("{error:#}")),
							Err(LoadError::Uncached(error)) => {
								uncached_error = Some(error);
								CellState::UncachedErr
							}
						}
					})
					.clone();
				if loaded {
					self.misses.fetch_add(1, Ordering::Relaxed);
				} else {
					self.hits.fetch_add(1, Ordering::Relaxed);
				}
				match state {
					CellState::Cached(value) => {
						if loaded {
							let shared = Arc::new(OnceLock::new());
							let _ = shared.set(CellState::Cached(value.clone()));
							let keep_in_offline = {
								let mut entries = self.entries.lock();
								match entries.get(key) {
									None => {
										entries.insert(key.clone(), Entry { cell: shared, last_used: tick, created: std::time::Instant::now() });
										false
									}
									Some(entry) => match entry.cell.get() {
										Some(CellState::CachedErr(_) | CellState::Uncached | CellState::UncachedErr) => {
											entries.insert(key.clone(), Entry { cell: shared, last_used: tick, created: std::time::Instant::now() });
											false
										}
										// An online load is in flight: keep our Ok cell in `offline` until online
										// settles so other concurrent offline callers share it instead of loading again.
										None => true,
										Some(CellState::Cached(_)) => false,
									},
								}
							};
							if !keep_in_offline {
								let mut offline = self.offline.lock();
								if offline.get(key).is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell)) {
									offline.remove(key);
								}
							}
							self.evict();
						}
						return Ok(value);
					}
					CellState::Uncached => {
						let mut offline = self.offline.lock();
						if offline.get(key).is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell)) {
							offline.remove(key);
						}
						drop(offline);
						if loaded {
							if offline_fallback {
								self.offline_errors.fetch_add(1, Ordering::Relaxed);
							}
							return Ok(uncached_value.expect("leader produced uncached value"));
						}
						// Waiter: retry with our own `load` instead of taking a moving/fallback value.
						continue;
					}
					CellState::CachedErr(error) => {
						self.offline_errors.fetch_add(1, Ordering::Relaxed);
						return Err(anyhow!(error));
					}
					CellState::UncachedErr => {
						let mut offline = self.offline.lock();
						if offline.get(key).is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell)) {
							offline.remove(key);
						}
						drop(offline);
						if loaded {
							self.offline_errors.fetch_add(1, Ordering::Relaxed);
							return Err(uncached_error.expect("leader produced uncached error"));
						}
						continue;
					}
				}
			}
			let cell = {
				let mut entries = self.entries.lock();
				// a hit neither clones the key nor allocates while the lock is held
				match entries.get_mut(key) {
					Some(entry) if !matches!(entry.cell.get(), Some(CellState::Uncached | CellState::UncachedErr)) && !(matches!(entry.cell.get(), Some(CellState::CachedErr(_))) && entry.created.elapsed() > FAILURE_TTL) && !entry.stale_stand_in() => {
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
			let mut uncached_value: Option<Arc<V>> = None;
			let mut uncached_error: Option<anyhow::Error> = None;
			let state = cell
				.get_or_init(|| {
					loaded = true;
					match (load.take().expect("leader runs load once"))() {
						Ok(outcome) => {
							let value = Arc::new(outcome.value);
							if outcome.keep {
								CellState::Cached(value)
							} else {
								uncached_value = Some(value);
								CellState::Uncached
							}
						}
						Err(LoadError::Cached(error)) => CellState::CachedErr(format!("{error:#}")),
						Err(LoadError::Uncached(error)) => {
							uncached_error = Some(error);
							CellState::UncachedErr
						}
					}
				})
				.clone();
			if loaded {
				self.misses.fetch_add(1, Ordering::Relaxed);
			} else {
				self.hits.fetch_add(1, Ordering::Relaxed);
			}
			match state {
				CellState::Cached(value) => {
					if loaded {
						if let Some(removed) = self.offline.lock().remove(key)
							&& matches!(removed.cell.get(), Some(CellState::CachedErr(_)))
						{
							self.dropped_offline_failures.fetch_add(1, Ordering::Relaxed);
						}
						self.evict();
					}
					return Ok(value);
				}
				CellState::Uncached => {
					let mut entries = self.entries.lock();
					if entries.get(key).is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell)) {
						entries.remove(key);
					}
					drop(entries);
					if loaded {
						return Ok(uncached_value.expect("leader produced uncached value"));
					}
					continue;
				}
				CellState::CachedErr(error) => {
					if loaded {
						// Clear any temporary offline Ok parked while this online load was in flight so
						// `offline` retains no GPU bytes at rest.
						let mut offline = self.offline.lock();
						if offline.get(key).is_some_and(|entry| matches!(entry.cell.get(), Some(CellState::Cached(_)))) {
							offline.remove(key);
						}
					}
					return Err(anyhow!(error));
				}
				CellState::UncachedErr => {
					let mut entries = self.entries.lock();
					if entries.get(key).is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell)) {
						entries.remove(key);
					}
					drop(entries);
					if loaded {
						return Err(uncached_error.expect("leader produced uncached error"));
					}
					continue;
				}
			}
		}
	}

	/// The loaded value for `key`, without loading it (callers that load many at once).
	pub fn get(&self, key: &K) -> Option<Arc<V>> {
		let tick = self.tick.fetch_add(1, Ordering::Relaxed);
		let mut entries = self.entries.lock();
		let entry = entries.get_mut(key)?;
		if entry.stale_stand_in() {
			return None;
		}
		let CellState::Cached(value) = entry.cell.get()? else {
			return None;
		};
		let value = value.clone();
		entry.last_used = tick;
		self.hits.fetch_add(1, Ordering::Relaxed);
		Some(value)
	}

	pub fn bytes(&self) -> u64 {
		self.entries
			.lock()
			.values()
			.filter_map(|entry| match entry.cell.get() {
				Some(CellState::Cached(value)) => Some(value.bytes()),
				_ => None,
			})
			.sum()
	}

	/// The bytes of what no render holds: what the budget bounds, and what dropping frees.
	pub fn idle_bytes(&self) -> u64 {
		self.entries.lock().values().filter_map(Self::idle).sum()
	}

	/// An entry's bytes when only the cache holds its value.
	fn idle(entry: &Entry<V>) -> Option<u64> {
		match entry.cell.get() {
			Some(CellState::Cached(value)) if Arc::strong_count(value) == 1 => Some(value.bytes()),
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
		{
			let mut offline = self.offline.lock();
			let mut dropped = 0u64;
			offline.retain(|_, entry| match entry.cell.get() {
				Some(CellState::CachedErr(_)) if budget == 0 || entry.created.elapsed() > FAILURE_TTL => {
					if entry.created.elapsed() <= FAILURE_TTL {
						dropped += 1;
					}
					false
				}
				_ => true,
			});
			if dropped > 0 {
				self.dropped_offline_failures.fetch_add(dropped, Ordering::Relaxed);
			}
		}
		let mut entries = self.entries.lock();
		let mut total: u64 = entries.values().filter_map(Self::idle).sum();
		let before = total;
		if total <= budget {
			return 0;
		}
		let mut order: Vec<(u64, K)> = entries.iter().filter(|(_, entry)| matches!(entry.cell.get(), Some(CellState::CachedErr(_))) || Self::idle(entry).is_some()).map(|(key, entry)| (entry.last_used, key.clone())).collect();
		order.sort_by_key(|(tick, _)| *tick);
		for (_, key) in order {
			if total <= budget {
				break;
			}
			match entries.remove(&key).as_ref().and_then(|entry| entry.cell.get()) {
				Some(CellState::Cached(value)) => total = total.saturating_sub(value.bytes()),
				Some(CellState::CachedErr(_)) => {
					self.dropped_failures.fetch_add(1, Ordering::Relaxed);
				}
				_ => {}
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

	/// A failure is remembered until FAILURE_TTL after its load began (`failures`), and one the budget
	/// lets go of before then is counted.
	#[test]
	fn failures_are_remembered_until_retried_and_counted_when_let_go_of() {
		let cache: Cache<u32, Blob> = Cache::new(100);
		assert_eq!(cache.failures(), (None, 0));
		let before = std::time::Instant::now();
		assert!(cache.get_or_load(&1, || Err(anyhow!("refused"))).is_err());
		let (retried, dropped) = cache.failures();
		let retried = retried.expect("the failure remembered");
		assert!(retried >= before + FAILURE_TTL && retried <= std::time::Instant::now() + FAILURE_TTL);
		assert_eq!(dropped, 0);
		assert!(cache.get_or_load(&1, || panic!("retried while remembered")).is_err());
		// past the budget, the least recently used go first: the failure, then the older blob
		cache.get_or_load(&2, || Ok(Blob(80))).unwrap();
		cache.get_or_load(&3, || Ok(Blob(40))).unwrap();
		cache.fit();
		assert_eq!(cache.failures(), (None, 1), "let go of, and counted");
		assert!(cache.get(&2).is_none() && cache.get(&3).is_some());
	}

	#[test]
	fn offline_failures_deduplicate_without_poisoning_online_cache() {
		let cache: Cache<u32, Blob> = Cache::new(100);
		assert!(cache.get_or_load_for(&1, true, || Err(anyhow!("offline and not cached"))).is_err());
		// Online sees no cached failure, while repeated offline lookups hit the offline failure cache.
		assert_eq!(cache.failures(), (None, 0), "offline-view failure must not appear in online failures");
		assert!(cache.failures_for(true).0.is_some(), "offline-view failure is remembered for offline callers");
		assert!(cache.get_or_load_for(&1, true, || panic!("offline failure should be cached")).is_err());
		// Online fetch succeeds, shares the value with offline, and clears the stale offline failure.
		let loaded = cache.get_or_load(&1, || Ok(Blob(40))).unwrap();
		assert_eq!(loaded.0, 40);
		assert_eq!(cache.dropped_failures_for(true), 1, "superseded offline failure counted");
		let hit = cache.get_or_load_for(&1, true, || panic!("should hit cached success")).unwrap();
		assert!(Arc::ptr_eq(&loaded, &hit));
		// An offline-view success is promoted into the shared cache and removed from `offline`.
		let from_disk = cache.get_or_load_for(&2, true, || Ok(Blob(20))).unwrap();
		let shared = cache.get_or_load(&2, || panic!("should hit cached offline success")).unwrap();
		assert!(Arc::ptr_eq(&from_disk, &shared));
		assert!(cache.offline.lock().is_empty(), "offline map holds zero GPU values at rest");
	}

	#[test]
	fn offline_load_beside_inflight_online_deduplicates_and_does_not_miscount_dropped_failures() {
		let cache: Cache<u32, Blob> = Cache::new(100);
		let entered = std::sync::Barrier::new(2);
		let release = std::sync::Barrier::new(2);
		let offline_loads = std::sync::atomic::AtomicUsize::new(0);
		std::thread::scope(|scope| {
			let online = scope.spawn(|| {
				cache
					.get_or_load(&1, || {
						entered.wait();
						release.wait();
						Ok(Blob(50))
					})
					.unwrap()
			});
			entered.wait();
			// While online is in flight, two offline callers share one offline load instead of loading twice.
			let first = cache
				.get_or_load_for(&1, true, || {
					offline_loads.fetch_add(1, Ordering::SeqCst);
					Ok(Blob(50))
				})
				.unwrap();
			let second = cache
				.get_or_load_for(&1, true, || {
					offline_loads.fetch_add(1, Ordering::SeqCst);
					Ok(Blob(50))
				})
				.unwrap();
			let same = Arc::ptr_eq(&first, &second);
			let loads = offline_loads.load(Ordering::SeqCst);
			release.wait();
			let settled = online.join().unwrap();
			assert!(same, "concurrent offline callers beside in-flight online load should share one value");
			assert_eq!(loads, 1, "concurrent offline callers beside in-flight online load should deduplicate");
			assert_eq!(settled.0, 50);
		});
		assert!(cache.offline.lock().is_empty(), "online completion clears temporary offline entry");
		assert_eq!(cache.dropped_failures_for(true), 0, "clearing a temporary offline success must not count as a dropped offline failure");
	}

	/// When a leader's checked load returns `keep = false` (a local file moved while being read),
	/// the leader receives its own value without keeping it in the cache, and a concurrent waiter
	/// retries with its own loader instead of inheriting the leader's moving value.
	#[test]
	fn uncached_moving_load_is_not_kept_and_waiter_retries() {
		let cache: Cache<u32, Blob> = Cache::new(100);
		let entered = std::sync::Barrier::new(2);
		let release = std::sync::Barrier::new(2);
		std::thread::scope(|scope| {
			let leader = scope.spawn(|| {
				cache
					.get_or_load_checked(&1, false, || {
						entered.wait();
						release.wait();
						Ok(LoadOutcome { value: Blob(99), keep: false, offline_fallback: false })
					})
					.unwrap()
			});
			entered.wait();
			let waiter = scope.spawn(|| {
				cache
					.get_or_load_checked(&1, false, || Ok(LoadOutcome { value: Blob(42), keep: true, offline_fallback: false }))
					.unwrap()
			});
			// Let the waiter attach to the leader's in-flight cell before releasing the leader.
			while cache.entries.lock().get(&1).is_none_or(|e| Arc::strong_count(&e.cell) < 3) {
				std::thread::yield_now();
			}
			release.wait();
			let a = leader.join().unwrap();
			let b = waiter.join().unwrap();
			assert_eq!(a.0, 99, "leader gets its own uncached read");
			assert_eq!(b.0, 42, "concurrent waiter must retry and get settled 42, not moving 99");
		});
		assert_eq!(cache.get(&1).unwrap().0, 42, "settled waiter's value is what stays cached");
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
