//! The GPU memory a render draws with and lets go of (its targets, uniforms, vertices), kept for the
//! renders after it instead of allocated anew.
//!
//! wgpu's Vulkan allocator (gpu-alloc) gives a texture of 1 MiB or more that the driver would rather
//! have alone (a render target) an allocation of its own, and carves the rest out of chunks of
//! 128 MiB doubling up to 512 MiB, handing a chunk back to the driver as soon as nothing is left in
//! it. A render that allocated its memory and dropped it made the driver allocate (and Windows
//! zero) memory every frame, the GPU starting on the frame only once that was done: one model's
//! Knights)'s live preview took 3.2 ms a frame, 0.55 of them GPU work, and one idle 2 MB texture
//! allocated elsewhere took a frame from 1.6 ms to 3.2 (its memory pushed the frame's into a new
//! chunk). Kept, a render's memory is allocated once (notes/render.md "GPU memory").
//!
//! A render takes what it needs through a [`Lease`], which gives it all back when dropped, after the
//! render's submissions: a later render's use queues after them. What it takes holds what an earlier
//! render left in it: every pass clears or overwrites a target before reading it.
//!
//! The targets a render writes and reads within its one submission it takes as scratch
//! ([`Lease::scratch`]): renders recording at the same time draw into the same ones. The queue runs
//! submissions one after another, each a render's whole use of them (WebGPU orders a texture's uses
//! across submissions as they were submitted), so none sees another's: eight clips recording their
//! frames side by side take one set of 1080p targets (~0.4 GB), not eight.

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::gpu::Gpu;

/// How long a kept texture or buffer may go unused before it is let go: a live preview's settled
/// frame after a long drag of quick ones finds its targets (kept for 16 renders, they were gone after
/// a 40-frame drag, and the settled frame took 13 ms instead of 3), a batch of photos of one size
/// keeps its own between renders.
const KEPT_FOR: Duration = Duration::from_secs(30);
/// The most texture memory kept between renders: the least recently used goes first (a photo's
/// targets at 2048 px with 4x MSAA take ~0.5 GB; a 1080p view's, its shadow atlas included, 0.2 GB).
const KEPT_BYTES: u64 = 256 << 20;
/// What a render gave back this recently stays past [`KEPT_BYTES`]: a clip's frames and a batch's views
/// draw one after another into the same targets, and a 1080p frame's (its views, AO, MSAA and sun
/// cascades, ~0.5 GB) did not fit the budget: 27 targets and 250 MB of GPU memory were allocated
/// (and zeroed by Windows) and freed again every frame of a map's clip.
const IN_USE_FOR: Duration = Duration::from_millis(250);
/// The smallest buffer kept (buffers come in powers of two from here, so that one serves contents
/// that change a little in size).
const SMALLEST_BUFFER: u64 = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
struct TextureKind {
	size: wgpu::Extent3d,
	mips: u32,
	samples: u32,
	dimension: wgpu::TextureDimension,
	format: wgpu::TextureFormat,
	usage: wgpu::TextureUsages,
}

impl TextureKind {
	fn of(desc: &wgpu::TextureDescriptor) -> Self {
		Self { size: desc.size, mips: desc.mip_level_count, samples: desc.sample_count, dimension: desc.dimension, format: desc.format, usage: desc.usage }
	}

	fn of_texture(texture: &wgpu::Texture) -> Self {
		Self { size: texture.size(), mips: texture.mip_level_count(), samples: texture.sample_count(), dimension: texture.dimension(), format: texture.format(), usage: texture.usage() }
	}

	/// About what it takes on the GPU.
	fn bytes(&self) -> u64 {
		let (block_width, block_height) = self.format.block_dimensions();
		let block = self.format.block_copy_size(None).or_else(|| self.format.block_copy_size(Some(wgpu::TextureAspect::DepthOnly))).unwrap_or(4) as u64;
		let blocks = self.size.width.div_ceil(block_width) as u64 * self.size.height.div_ceil(block_height) as u64 * self.size.depth_or_array_layers as u64;
		// a full chain of mips adds a third
		blocks * block * self.samples as u64 * if self.mips > 1 { 4 } else { 3 } / 3
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BufferKind {
	size: u64,
	usage: wgpu::BufferUsages,
}

struct Kept<K, T> {
	kind: K,
	item: T,
	bytes: u64,
	/// When it was last given back.
	used: Instant,
}

/// What renders gave back, for later ones.
#[derive(Default)]
pub struct Reuse {
	textures: Mutex<Vec<Kept<TextureKind, wgpu::Texture>>>,
	buffers: Mutex<Vec<Kept<BufferKind, wgpu::Buffer>>>,
	/// The scratch targets ([`Lease::scratch`]): a render's n-th held at once of a kind is the n-th
	/// of that kind here, whichever renders draw into it too
	scratch: Mutex<Vec<Scratch>>,
}

/// A scratch target: its kind, its place among those of its kind, when a render last gave it back,
/// and how many renders hold it now.
struct Scratch {
	kind: TextureKind,
	index: usize,
	texture: wgpu::Texture,
	bytes: u64,
	used: Instant,
	holders: usize,
}

impl Reuse {
	/// What one render takes, given back when it drops.
	pub fn lease<'a>(&'a self, gpu: &'a Gpu) -> Lease<'a> {
		Lease { reuse: self, gpu, textures: Mutex::new(Vec::new()), released: Mutex::new(Vec::new()), buffers: Mutex::new(Vec::new()), scratch: Mutex::new(Vec::new()), held: Mutex::new(Vec::new()) }
	}

	/// The `index`-th scratch target of `kind`, made as `desc` describes when there is none.
	fn scratch(&self, gpu: &Gpu, kind: TextureKind, index: usize, desc: &wgpu::TextureDescriptor) -> wgpu::Texture {
		let mut scratch = self.scratch.lock();
		if let Some(found) = scratch.iter_mut().find(|entry| entry.kind == kind && entry.index == index) {
			found.holders += 1;
			return found.texture.clone();
		}
		let texture = gpu.device.create_texture(desc);
		scratch.push(Scratch { kind, index, texture: texture.clone(), bytes: kind.bytes(), used: Instant::now(), holders: 1 });
		texture
	}

	/// A texture a render handed out ([`Lease::hand_out`]) that its holder is done with, for a later
	/// render to draw into.
	pub fn give_back(&self, texture: wgpu::Texture) {
		let kind = TextureKind::of_texture(&texture);
		self.textures.lock().push(Kept { kind, item: texture, bytes: kind.bytes(), used: Instant::now() });
	}

	/// A staging buffer detached from its lease, after its mapping has finished and been unmapped.
	pub fn give_back_buffer(&self, buffer: wgpu::Buffer) {
		let kind = BufferKind { size: buffer.size(), usage: buffer.usage() };
		self.buffers.lock().push(Kept { kind, item: buffer, bytes: kind.size, used: Instant::now() });
	}

	/// What is kept: (textures, their bytes, buffers, their bytes), scratch targets among the textures.
	pub fn stats(&self) -> (usize, u64, usize, u64) {
		let textures = self.textures.lock();
		let scratch = self.scratch.lock();
		let buffers = self.buffers.lock();
		(textures.len() + scratch.len(), textures.iter().map(|kept| kept.bytes).sum::<u64>() + scratch.iter().map(|entry| entry.bytes).sum::<u64>(), buffers.len(), buffers.iter().map(|kept| kept.bytes).sum())
	}

	/// The kept textures by kind, largest first: "WxH format xSAMPLES mips" with their count and bytes.
	pub fn kinds(&self) -> Vec<(String, usize, u64)> {
		let mut kinds: Vec<(String, usize, u64)> = Vec::new();
		let scratch: Vec<(TextureKind, u64)> = self.scratch.lock().iter().map(|entry| (entry.kind, entry.bytes)).collect();
		let kept: Vec<(TextureKind, u64)> = self.textures.lock().iter().map(|kept| (kept.kind, kept.bytes)).collect();
		for (kind, bytes) in kept.into_iter().chain(scratch) {
			let kind = &kind;
			let name = format!("{}x{}x{} {:?} x{} mips {}", kind.size.width, kind.size.height, kind.size.depth_or_array_layers, kind.format, kind.samples, kind.mips);
			match kinds.iter_mut().find(|(known, _, _)| *known == name) {
				Some((_, count, total)) => {
					*count += 1;
					*total += bytes;
				}
				None => kinds.push((name, 1, bytes)),
			}
		}
		kinds.sort_by_key(|(_, _, bytes)| std::cmp::Reverse(*bytes));
		kinds
	}

	/// Lets go of what has gone unused for [`KEPT_FOR`], as each render does when it gives its memory
	/// back: for a caller gone idle (a window between previews).
	pub fn trim(&self) {
		self.evict(Instant::now());
	}

	/// Lets go of everything kept; returns its bytes.
	pub fn clear(&self) -> u64 {
		let textures: u64 = std::mem::take(&mut *self.textures.lock()).iter().map(|kept| kept.bytes).sum::<u64>() + std::mem::take(&mut *self.scratch.lock()).iter().map(|entry| entry.bytes).sum::<u64>();
		let buffers: u64 = std::mem::take(&mut *self.buffers.lock()).iter().map(|kept| kept.bytes).sum();
		textures + buffers
	}

	/// [`Reuse::clear`] but for the scratch targets renders draw into now: taken from the pool, the next
	/// render of their size would make a set of its own beside theirs. Returns the bytes let go of.
	pub fn release_unheld(&self) -> u64 {
		let textures: u64 = std::mem::take(&mut *self.textures.lock()).iter().map(|kept| kept.bytes).sum();
		let buffers: u64 = std::mem::take(&mut *self.buffers.lock()).iter().map(|kept| kept.bytes).sum();
		let mut scratch = self.scratch.lock();
		let before: u64 = scratch.iter().map(|entry| entry.bytes).sum();
		scratch.retain(|entry| entry.holders > 0);
		textures + buffers + before - scratch.iter().map(|entry| entry.bytes).sum::<u64>()
	}

	/// Whether a scratch target of `width` x `height`, `format` and `samples` is kept: the set a view of
	/// that size draws into, made together by the view before.
	pub fn holds_scratch(&self, width: u32, height: u32, format: wgpu::TextureFormat, samples: u32) -> bool {
		self.scratch.lock().iter().any(|entry| entry.kind.size.width == width && entry.kind.size.height == height && entry.kind.format == format && entry.kind.samples == samples)
	}

	/// Lets go of what has gone unused for [`KEPT_FOR`] by `now`, then of the least recently used
	/// textures (scratch targets among them) past [`KEPT_BYTES`], but not those given back within
	/// [`IN_USE_FOR`]. A scratch target let go of while a render draws into it stays that render's; the
	/// next to ask for its place gets a new one.
	fn evict(&self, now: Instant) {
		let fresh = |used: Instant| now.saturating_duration_since(used) < KEPT_FOR;
		self.buffers.lock().retain(|kept| fresh(kept.used));
		let mut textures = self.textures.lock();
		let mut scratch = self.scratch.lock();
		textures.retain(|kept| fresh(kept.used));
		scratch.retain(|entry| fresh(entry.used));
		let mut total: u64 = textures.iter().map(|kept| kept.bytes).sum::<u64>() + scratch.iter().map(|entry| entry.bytes).sum::<u64>();
		if total > KEPT_BYTES {
			textures.sort_by_key(|kept| std::cmp::Reverse(kept.used));
			scratch.sort_by_key(|entry| std::cmp::Reverse(entry.used));
			while total > KEPT_BYTES {
				// the least recently given back of either, unless that was within IN_USE_FOR
				let (kept, shared) = (textures.last().map(|oldest| oldest.used), scratch.last().map(|oldest| oldest.used));
				let (used, scratch_first) = match (kept, shared) {
					(Some(kept), Some(shared)) if shared < kept => (shared, true),
					(Some(kept), _) => (kept, false),
					(None, Some(shared)) => (shared, true),
					(None, None) => break,
				};
				if now.saturating_duration_since(used) < IN_USE_FOR {
					break;
				}
				total -= if scratch_first { scratch.pop().map_or(0, |entry| entry.bytes) } else { textures.pop().map_or(0, |kept| kept.bytes) };
			}
		}
	}
}

/// The textures and buffers one render took: given back to its [`Reuse`] when dropped, which must be
/// after the render's last submission.
pub struct Lease<'a> {
	reuse: &'a Reuse,
	gpu: &'a Gpu,
	textures: Mutex<Vec<(TextureKind, wgpu::Texture)>>,
	/// Textures this render is done with, for its later ones ([`Lease::release`])
	released: Mutex<Vec<(TextureKind, wgpu::Texture)>>,
	buffers: Mutex<Vec<(BufferKind, wgpu::Buffer)>>,
	/// The scratch targets it took ([`Lease::scratch`]): kind, place, texture and whether it still
	/// holds it (not [`Lease::release`]d)
	scratch: Mutex<Vec<(TextureKind, usize, wgpu::Texture, bool)>>,
	/// What its commands read that is not its own, held until it is given back ([`Lease::hold`])
	held: Mutex<Vec<std::sync::Arc<dyn std::any::Any + Send + Sync>>>,
}

impl Lease<'_> {
	/// The device its textures and buffers are of.
	pub fn device(&self) -> &wgpu::Device {
		&self.gpu.device
	}

	/// A texture as `desc` describes: one this render is done with ([`Lease::release`]), else one an
	/// earlier render gave back, holding what it left there, or a new one.
	pub fn texture(&self, desc: &wgpu::TextureDescriptor) -> wgpu::Texture {
		let kind = TextureKind::of(desc);
		let own = {
			let mut released = self.released.lock();
			released.iter().position(|(released, _)| *released == kind).map(|index| released.swap_remove(index).1)
		};
		if let Some(texture) = own {
			self.textures.lock().push((kind, texture.clone()));
			return texture;
		}
		let kept = {
			let mut kept = self.reuse.textures.lock();
			kept.iter().position(|kept| kept.kind == kind).map(|index| kept.swap_remove(index).item)
		};
		let texture = kept.unwrap_or_else(|| self.gpu.device.create_texture(desc));
		self.textures.lock().push((kind, texture.clone()));
		texture
	}

	/// A texture as `desc` describes for the shadow cache to keep for good ([`Lease::texture`]); the
	/// scratch targets of its kind are let go of (each goes once no render holds it): the maps of a
	/// projection seen a second time are kept from then on, so the scratch one its first frame drew
	/// into would stay in the pool unused (a still camera's 1080p clip: 80 MB of sun atlas and 24 MB
	/// of a light's cube, until the pool's time limit). One that no render holds becomes this
	/// render's own, the kept map: made anew, it stood beside the scratch one until that went (the
	/// still camera's clip peaked 64 MB higher on the GPU and 120 MB in commit). Every pass writes a
	/// target before it reads it, as with any texture the pool gives.
	pub fn kept(&self, desc: &wgpu::TextureDescriptor) -> wgpu::Texture {
		let kind = TextureKind::of(desc);
		let free = {
			let mut scratch = self.reuse.scratch.lock();
			let free = scratch.iter().position(|entry| entry.kind == kind && entry.holders == 0).map(|index| scratch.swap_remove(index).texture);
			scratch.retain(|entry| entry.kind != kind);
			free
		};
		match free {
			Some(texture) => {
				self.textures.lock().push((kind, texture.clone()));
				texture
			}
			None => self.texture(desc),
		}
	}

	/// A target as `desc` describes that this render writes before it reads it and uses only within the
	/// command encoder it records now (no queue writes into it, no copy out of it after another
	/// submission, never handed out): one other renders may draw into while this one does. Its n-th held
	/// at once of a kind is the scratch targets' n-th of that kind (one it released first).
	pub fn scratch(&self, desc: &wgpu::TextureDescriptor) -> wgpu::Texture {
		let kind = TextureKind::of(desc);
		let mut taken = self.scratch.lock();
		if let Some(entry) = taken.iter_mut().find(|entry| entry.0 == kind && !entry.3) {
			entry.3 = true;
			return entry.2.clone();
		}
		let index = taken.iter().filter(|entry| entry.0 == kind).count();
		let texture = self.reuse.scratch(self.gpu, kind, index, desc);
		taken.push((kind, index, texture.clone(), true));
		texture
	}

	/// Holds `item` (a kept shadow map the render draws with) until the lease is given back, after the
	/// render's submissions: its owner lets it go to the pool only when nothing holds it.
	pub fn hold(&self, item: std::sync::Arc<dyn std::any::Any + Send + Sync>) {
		self.held.lock().push(item);
	}

	/// [`Lease::scratch`]'s default view.
	pub fn scratch_view(&self, desc: &wgpu::TextureDescriptor) -> wgpu::TextureView {
		self.scratch(desc).create_view(&Default::default())
	}

	/// `texture` (from this lease) is used by no command recorded after now: a later [`Lease::texture`]
	/// (or [`Lease::scratch`], for a scratch target) of its kind in this render draws into it (a frame
	/// graph's memory aliasing, within one kind). Its last use and the next must go into one command
	/// encoder, the next recorded after (wgpu orders them with a barrier); only this lease takes it, as
	/// another render's submissions could run between this one's.
	pub fn release(&self, texture: &wgpu::Texture) {
		if let Some(entry) = self.scratch.lock().iter_mut().find(|entry| entry.2 == *texture) {
			entry.3 = false;
			return;
		}
		let mut taken = self.textures.lock();
		if let Some(index) = taken.iter().position(|(_, held)| held == texture) {
			let entry = taken.swap_remove(index);
			self.released.lock().push(entry);
		}
	}

	/// A buffer for `usage` holding `contents` (written by the queue, before the next submission's
	/// commands), perhaps larger than them.
	pub fn buffer(&self, label: &str, usage: wgpu::BufferUsages, contents: &[u8]) -> wgpu::Buffer {
		let buffer = self.empty_buffer(label, usage | wgpu::BufferUsages::COPY_DST, contents.len() as u64);
		if !contents.is_empty() {
			if contents.len().is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize) {
				self.gpu.queue.write_buffer(&buffer, 0, contents);
			} else {
				let mut padded = contents.to_vec();
				padded.resize(contents.len().next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT as usize), 0);
				self.gpu.queue.write_buffer(&buffer, 0, &padded);
			}
		}
		buffer
	}

	/// A buffer for `usage` of at least `size` bytes, holding what an earlier render left in it.
	pub fn empty_buffer(&self, label: &str, usage: wgpu::BufferUsages, size: u64) -> wgpu::Buffer {
		let kind = BufferKind { size: size.max(SMALLEST_BUFFER).next_power_of_two(), usage };
		let kept = {
			let mut kept = self.reuse.buffers.lock();
			kept.iter().position(|kept| kept.kind == kind).map(|index| kept.swap_remove(index).item)
		};
		let buffer = kept.unwrap_or_else(|| self.gpu.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: kind.size, usage, mapped_at_creation: false }));
		self.buffers.lock().push((kind, buffer.clone()));
		buffer
	}

	/// `texture` (from this lease) leaves with the render's caller: not given back with the rest
	/// ([`Reuse::give_back`] once the caller is done with it).
	pub fn hand_out(&self, texture: &wgpu::Texture) {
		debug_assert!(!self.scratch.lock().iter().any(|entry| entry.2 == *texture), "a scratch target is never handed out");
		self.textures.lock().retain(|(_, taken)| taken != texture);
	}

	/// `buffer` (from this lease) is not given back with the lease: immutable cached scene data, or a
	/// pending or failed readback (a successful reader returns it through [`Reuse::give_back_buffer`]
	/// after unmapping it).
	pub fn forget_buffer(&self, buffer: &wgpu::Buffer) {
		self.buffers.lock().retain(|(_, taken)| taken != buffer);
	}
}

impl Drop for Lease<'_> {
	fn drop(&mut self) {
		let used = Instant::now();
		let taken = std::mem::take(self.scratch.get_mut());
		if !taken.is_empty() {
			let mut scratch = self.reuse.scratch.lock();
			for (kind, index, texture, _) in taken {
				if let Some(entry) = scratch.iter_mut().find(|entry| entry.kind == kind && entry.index == index && entry.texture == texture) {
					entry.used = used;
					entry.holders = entry.holders.saturating_sub(1);
				}
			}
		}
		let released = std::mem::take(self.released.get_mut());
		self.reuse.textures.lock().extend(self.textures.get_mut().drain(..).chain(released).map(|(kind, item)| Kept { kind, item, bytes: kind.bytes(), used }));
		self.reuse.buffers.lock().extend(self.buffers.get_mut().drain(..).map(|(kind, item)| Kept { kind, item, bytes: kind.size, used }));
		self.reuse.evict(used);
	}
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use super::{KEPT_FOR, Reuse};
	use crate::render::gpu::Gpu;

	fn target(width: u32) -> wgpu::TextureDescriptor<'static> {
		wgpu::TextureDescriptor {
			label: Some("target"),
			size: wgpu::Extent3d { width, height: 16, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::Rgba16Float,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		}
	}

	#[test]
	fn a_render_takes_what_the_last_gave_back() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let reuse = Reuse::default();
		let (texture, buffer, handed) = {
			let lease = reuse.lease(&gpu);
			let handed = lease.texture(&target(32));
			lease.hand_out(&handed);
			(lease.texture(&target(32)), lease.buffer("uniform", wgpu::BufferUsages::UNIFORM, &[7; 80]), handed)
		};
		{
			let lease = reuse.lease(&gpu);
			assert!(lease.texture(&target(32)) == texture, "the same kind: the texture given back");
			assert!(lease.texture(&target(32)) != handed, "a texture handed out stays out");
			assert!(lease.texture(&target(64)) != texture, "another size: a new one");
			// a little larger: the same power of two
			assert!(lease.buffer("uniform", wgpu::BufferUsages::UNIFORM, &[7; 96]) == buffer);
			assert!(lease.buffer("vertices", wgpu::BufferUsages::VERTEX, &[7; 80]) != buffer, "another use: a new one");
		}
		reuse.give_back(handed.clone());
		let lease = reuse.lease(&gpu);
		let (first, second) = (lease.texture(&target(32)), lease.texture(&target(32)));
		assert!(first == handed || second == handed, "given back by its holder");
	}

	#[test]
	fn a_released_texture_serves_its_own_render_alone() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let reuse = Reuse::default();
		let lease = reuse.lease(&gpu);
		let first = lease.texture(&target(32));
		lease.release(&first);
		let other = reuse.lease(&gpu);
		assert!(other.texture(&target(32)) != first, "another render does not take it");
		assert!(lease.texture(&target(32)) == first, "this render does");
		drop((lease, other));
		assert_eq!(reuse.stats().0, 2, "both given back once");
	}

	#[test]
	fn what_goes_unused_is_let_go() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let reuse = Reuse::default();
		drop(reuse.lease(&gpu).texture(&target(32)));
		assert_eq!(reuse.stats().0, 1);
		reuse.evict(Instant::now() + KEPT_FOR - Duration::from_secs(1));
		assert_eq!(reuse.stats().0, 1, "kept a while");
		reuse.evict(Instant::now() + KEPT_FOR + Duration::from_secs(1));
		assert_eq!(reuse.stats().0, 0);
	}

	#[test]
	fn light_grid_voxels_arrive_in_order() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let reuse = Reuse::default();
		let lease = reuse.lease(&gpu);
		let dims = [5u32, 3, 4];
		let values: Vec<u8> = (0..60u8).map(|index| index * 4 + 1).collect();
		// readable here
		fn copyable<'a>(desc: &wgpu::TextureDescriptor<'a>) -> wgpu::TextureDescriptor<'a> {
			wgpu::TextureDescriptor { usage: desc.usage | wgpu::TextureUsages::COPY_SRC, ..desc.clone() }
		}
		// twice: a kept texture written over
		crate::render::light_grid_texture(&gpu, |desc| lease.texture(&copyable(desc)), dims, &[0; 60]);
		drop(lease);
		let lease = reuse.lease(&gpu);
		let mut taken = None;
		crate::render::light_grid_texture(
			&gpu,
			|desc| {
				let texture = lease.texture(&copyable(desc));
				taken = Some(texture.clone());
				texture
			},
			dims,
			&values,
		);
		let texture = taken.expect("the grid's texture");
		let row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
		let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * dims[1] * dims[2]) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
		let mut encoder = gpu.device.create_command_encoder(&Default::default());
		encoder.copy_texture_to_buffer(
			texture.as_image_copy(),
			wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(dims[1]) } },
			texture.size(),
		);
		gpu.queue.submit([encoder.finish()]);
		readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
		gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("GPU wait");
		let mapped = readback.slice(..).get_mapped_range();
		// x fastest, then y, then z
		let read: Vec<u8> = (0..dims[1] * dims[2]).flat_map(|line| mapped[(line * row) as usize..(line * row + dims[0]) as usize].to_vec()).collect();
		assert_eq!(read, values);
	}
}
