//! Submitted images whose readbacks can finish while the caller prepares the next frame.

use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use super::gpu::Submitted;

use anyhow::{Context, Result, anyhow};

use super::{RenderStats, Renderer, ViewImage, guis, highlights, ids};
use crate::scene::Highlight;

struct MappedRows {
	readback: ids::Readback,
	height: u32,
	ready: Receiver<Result<(), wgpu::BufferAsyncError>>,
	mapping: bool,
}

impl MappedRows {
	fn new(readback: ids::Readback, height: u32) -> Self {
		let (sender, ready) = channel();
		readback.buffer.slice(..readback.bytes_per_row as u64 * height as u64).map_async(wgpu::MapMode::Read, move |result| {
			let _ = sender.send(result);
		});
		Self { readback, height, ready, mapping: true }
	}

	/// Waits for the mapping, once the GPU has done `submission` (no poll of the device here: the GPU's
	/// poller fires the mapping's callback, render::gpu).
	fn wait(&mut self, gpu: &super::gpu::Gpu, submission: Submitted) -> Result<()> {
		if let Err(error) = gpu.wait_mapped(submission, &self.ready)? {
			self.mapping = false;
			return Err(anyhow!("readback: {error}"));
		}
		Ok(())
	}

	fn read<T: bytemuck::Pod>(&mut self, gpu: &super::gpu::Gpu, submission: Submitted, width: u32) -> Result<Vec<T>> {
		self.wait(gpu, submission)?;
		let mapped = self.readback.buffer.slice(..self.readback.bytes_per_row as u64 * self.height as u64).get_mapped_range();
		let row_bytes = width as usize * std::mem::size_of::<T>();
		let mut texels = Vec::with_capacity(width as usize * self.height as usize);
		if row_bytes == self.readback.bytes_per_row as usize {
			texels.extend_from_slice(bytemuck::cast_slice(&mapped));
		} else {
			for row in mapped.chunks_exact(self.readback.bytes_per_row as usize) {
				texels.extend_from_slice(bytemuck::cast_slice(&row[..row_bytes]));
			}
		}
		drop(mapped);
		self.readback.buffer.unmap();
		self.mapping = false;
		Ok(texels)
	}
}

impl Drop for MappedRows {
	fn drop(&mut self) {
		// A cancelled frame cancels its mapping too; its buffer is never reused while mapped.
		if self.mapping && !matches!(self.ready.try_recv(), Ok(Err(_))) {
			self.readback.buffer.unmap();
		}
	}
}

/// A view with its passes submitted and mappings requested, keeping only its readback buffers.
pub struct PendingView<'a> {
	renderer: &'a Renderer,
	image: ViewImage,
	submission: Submitted,
	rgba: Option<MappedRows>,
	highlight: Option<(MappedRows, Vec<Highlight>)>,
	gui: Option<MappedRows>,
	ids: Option<MappedRows>,
	nv12: Option<MappedRows>,
	flatten: bool,
}

impl<'a> PendingView<'a> {
	pub(super) fn new(renderer: &'a Renderer, image: ViewImage, submission: Submitted, rgba: Option<ids::Readback>, highlight: Option<(ids::Readback, Vec<Highlight>)>, gui: Option<ids::Readback>, ids: Option<ids::Readback>) -> Self {
		let height = image.height;
		Self {
			renderer,
			image,
			submission,
			rgba: rgba.map(|readback| MappedRows::new(readback, height)),
			highlight: highlight.map(|(readback, styles)| (MappedRows::new(readback, height * 2), styles)),
			gui: gui.map(|readback| MappedRows::new(readback, height)),
			ids: ids.map(|readback| MappedRows::new(readback, height)),
			nv12: None,
			flatten: false,
		}
	}

	pub(super) fn with_video(mut self, nv12: Option<ids::Readback>, flatten: bool) -> Self {
		self.nv12 = nv12.map(|readback| MappedRows::new(readback, 1));
		self.flatten = flatten;
		self
	}

	/// Streams tightly packed NV12 and, at sheet moments, the exact lossless RGBA. Frames with
	/// CPU overlays convert after composition instead, so their colour and ordering stay intact.
	pub(crate) fn with_nv12<T>(mut self, stats: &mut RenderStats, consume: impl FnOnce(&[u8], Option<&[u8]>) -> Result<T>) -> Result<T> {
		let Some(mut rows) = self.nv12.take() else {
			let flatten = self.flatten;
			let mut conversion_ms = 0.0;
			let result = self.with_rgba(stats, |image, rgba| {
				let started = Instant::now();
				let nv12 = super::nv12::from_rgba(rgba, image.width, image.height, flatten);
				conversion_ms = started.elapsed().as_secs_f64() * 1000.0;
				consume(&nv12, Some(rgba))
			});
			stats.readback_ms += conversion_ms;
			return result;
		};
		let started = Instant::now();
		rows.wait(&self.renderer.gpu, self.submission)?;
		let mapped = rows.readback.buffer.slice(..rows.readback.bytes_per_row as u64).get_mapped_range();
		stats.readback_ms += started.elapsed().as_secs_f64() * 1000.0;
		let size = self.image.width as usize * self.image.height as usize * 3 / 2;
		let renderer = self.renderer;
		let result = if self.rgba.is_some() {
			// Sheet moments can use the original mapped RGBA too, without copying a full frame.
			self.with_rgba(stats, |_, rgba| consume(&mapped[..size], Some(rgba)))
		} else {
			consume(&mapped[..size], None)
		};
		drop(mapped);
		rows.readback.buffer.unmap();
		rows.mapping = false;
		renderer.reuse.give_back_buffer(rows.readback.buffer.clone());
		result
	}

	/// [`PendingView::with_nv12`], the frame's NV12 kept after it ([`Held`]: its staging buffer, unmapped
	/// and not given back, or the bytes converted on the CPU) for the encoder to take again when the next
	/// frame is the same one (video.rs).
	pub(crate) fn with_nv12_held<T>(mut self, stats: &mut RenderStats, consume: impl FnOnce(&[u8], Option<&[u8]>) -> Result<T>) -> Result<(T, Held<'a>)> {
		let (renderer, submission) = (self.renderer, self.submission);
		let Some(mut rows) = self.nv12.take() else {
			let flatten = self.flatten;
			let mut conversion_ms = 0.0;
			let mut kept = Vec::new();
			let result = self.with_rgba(stats, |image, rgba| {
				let started = Instant::now();
				kept = super::nv12::from_rgba(rgba, image.width, image.height, flatten);
				conversion_ms = started.elapsed().as_secs_f64() * 1000.0;
				consume(&kept, Some(rgba))
			});
			stats.readback_ms += conversion_ms;
			return result.map(|result| (result, Held { renderer, submission, source: HeldSource::Owned(kept) }));
		};
		let started = Instant::now();
		rows.wait(&renderer.gpu, submission)?;
		let length = rows.readback.bytes_per_row as u64;
		let mapped = rows.readback.buffer.slice(..length).get_mapped_range();
		stats.readback_ms += started.elapsed().as_secs_f64() * 1000.0;
		let size = self.image.width as usize * self.image.height as usize * 3 / 2;
		let result = if self.rgba.is_some() { self.with_rgba(stats, |_, rgba| consume(&mapped[..size], Some(rgba))) } else { consume(&mapped[..size], None) };
		drop(mapped);
		rows.readback.buffer.unmap();
		rows.mapping = false;
		let buffer = rows.readback.buffer.clone();
		drop(rows);
		result.map(|result| (result, Held { renderer, submission, source: HeldSource::Buffer { buffer, length, size } }))
	}

	/// [`PendingView::with_rgba`], the frame's RGBA kept after it as [`PendingView::with_nv12_held`] keeps
	/// NV12.
	pub(crate) fn with_rgba_held<T>(mut self, stats: &mut RenderStats, consume: impl FnOnce(ViewImage, &[u8]) -> Result<T>) -> Result<(T, Held<'a>)> {
		let (renderer, submission) = (self.renderer, self.submission);
		let contiguous = self.rgba.as_ref().is_some_and(|rows| rows.readback.bytes_per_row == self.image.width * 4);
		if !contiguous || self.highlight.is_some() || self.gui.is_some() || self.ids.is_some() {
			let mut image = self.finish(stats)?;
			let rgba = std::mem::take(&mut image.rgba);
			let result = consume(image, &rgba)?;
			return Ok((result, Held { renderer, submission, source: HeldSource::Owned(rgba) }));
		}
		let started = Instant::now();
		let mut rows = self.rgba.take().context("RGBA readback")?;
		rows.wait(&renderer.gpu, submission)?;
		let length = rows.readback.bytes_per_row as u64 * rows.height as u64;
		let mapped = rows.readback.buffer.slice(..length).get_mapped_range();
		stats.readback_ms += started.elapsed().as_secs_f64() * 1000.0;
		let size = mapped.len();
		let result = consume(self.image, &mapped);
		drop(mapped);
		rows.readback.buffer.unmap();
		rows.mapping = false;
		let buffer = rows.readback.buffer.clone();
		drop(rows);
		result.map(|result| (result, Held { renderer, submission, source: HeldSource::Buffer { buffer, length, size } }))
	}

	/// With the levels of detail the view drew at (render::levels).
	pub(super) fn with_lod(mut self, report: super::LodView) -> Self {
		self.image.lod = Some(report);
		self
	}

	/// The submitted view's dimensions and camera, available before its pixels finish.
	pub fn image(&self) -> &ViewImage {
		&self.image
	}

	/// Consumes tightly packed RGBA directly from mapped staging memory. Padded rows and CPU
	/// overlays use the same owned image as a still render; the buffer is returned after consumption.
	pub fn with_rgba<T>(mut self, stats: &mut RenderStats, consume: impl FnOnce(ViewImage, &[u8]) -> Result<T>) -> Result<T> {
		let contiguous = self.rgba.as_ref().is_some_and(|rows| rows.readback.bytes_per_row == self.image.width * 4);
		if !contiguous || self.highlight.is_some() || self.gui.is_some() || self.ids.is_some() {
			let mut image = self.finish(stats)?;
			let rgba = std::mem::take(&mut image.rgba);
			return consume(image, &rgba);
		}
		let started = Instant::now();
		let mut rows = self.rgba.take().context("RGBA readback")?;
		rows.wait(&self.renderer.gpu, self.submission)?;
		let mapped = rows.readback.buffer.slice(..rows.readback.bytes_per_row as u64 * rows.height as u64).get_mapped_range();
		stats.readback_ms += started.elapsed().as_secs_f64() * 1000.0;
		let result = consume(self.image, &mapped);
		drop(mapped);
		rows.readback.buffer.unmap();
		rows.mapping = false;
		self.renderer.reuse.give_back_buffer(rows.readback.buffer.clone());
		result
	}

	/// Waits only for this view's submission, copies its rows and applies the CPU overlays.
	pub fn finish(mut self, stats: &mut RenderStats) -> Result<ViewImage> {
		let Some(mut readback) = self.rgba.take() else {
			return Ok(self.image);
		};
		let started = Instant::now();
		let (gpu, submission) = (&self.renderer.gpu, self.submission);
		self.image.rgba = readback.read(gpu, submission, self.image.width * 4)?;
		if let Some((mask, styles)) = &mut self.highlight {
			let mask = mask.read(gpu, submission, self.image.width * 2)?;
			highlights::composite(&mut self.image.rgba, self.image.width, self.image.height, &mask, styles);
		}
		if let Some(layer) = &mut self.gui {
			guis::composite_overlays(&mut self.image.rgba, &layer.read(gpu, submission, self.image.width * 4)?);
		}
		if let Some(ids) = &mut self.ids {
			self.image.ids = Some(ids.read(gpu, submission, self.image.width)?);
		}
		// The staging buffer was detached from the render's lease until its mapping finished.
		let buffer = readback.readback.buffer.clone();
		drop(readback);
		self.renderer.reuse.give_back_buffer(buffer);
		stats.readback_ms += started.elapsed().as_secs_f64() * 1000.0;
		Ok(self.image)
	}
}

/// A frame's pixels as the encoder took them, kept to be taken again ([`PendingView::with_nv12_held`]):
/// its staging buffer, mapped again when they are (its GPU work long done), given back to the pool when
/// this goes; or bytes made on the CPU.
pub(crate) struct Held<'a> {
	renderer: &'a Renderer,
	submission: Submitted,
	source: HeldSource,
}

enum HeldSource {
	Buffer { buffer: wgpu::Buffer, length: u64, size: usize },
	Owned(Vec<u8>),
}

impl Held<'_> {
	/// The kept pixels, the same bytes as when first taken.
	pub(crate) fn with<T>(&self, consume: impl FnOnce(&[u8]) -> T) -> Result<T> {
		match &self.source {
			HeldSource::Owned(bytes) => Ok(consume(bytes)),
			HeldSource::Buffer { buffer, length, size } => {
				let (sender, ready) = channel();
				buffer.slice(..*length).map_async(wgpu::MapMode::Read, move |result| {
					let _ = sender.send(result);
				});
				self.renderer.gpu.wait_mapped(self.submission, &ready)?.map_err(|error| anyhow!("readback again: {error}"))?;
				let mapped = buffer.slice(..*length).get_mapped_range();
				let result = consume(&mapped[..*size]);
				drop(mapped);
				buffer.unmap();
				Ok(result)
			}
		}
	}
}

impl Drop for Held<'_> {
	fn drop(&mut self) {
		if let HeldSource::Buffer { buffer, .. } = &self.source {
			self.renderer.reuse.give_back_buffer(buffer.clone());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::gpu::Gpu;

	#[test]
	fn submitted_readbacks_keep_distinct_frames_and_strip_padding() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let mut pending = Vec::new();
		for (value, width) in [(17, 12), (93, 256)] {
			let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 512, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
			let mut bytes = vec![255u8; 512];
			bytes[..width].fill(value);
			bytes[256..256 + width].fill(value + 1);
			gpu.queue.write_buffer(&buffer, 0, &bytes);
			let submission = gpu.submit([]);
			pending.push((MappedRows::new(ids::Readback { buffer, bytes_per_row: 256 }, 2), submission, value, width));
		}
		for (mut rows, submission, value, width) in pending {
			let pixels: Vec<u8> = rows.read(&gpu, submission, width as u32).unwrap();
			assert_eq!(pixels, [vec![value; width], vec![value + 1; width]].concat());
		}
	}

	#[test]
	fn dropping_a_pending_readback_cancels_its_mapping() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 256, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
		drop(MappedRows::new(ids::Readback { buffer: buffer.clone(), bytes_per_row: 256 }, 1));
		gpu.queue.write_buffer(&buffer, 0, &[7; 256]);
		let submission = gpu.submit([]);
		let mut rows = MappedRows::new(ids::Readback { buffer, bytes_per_row: 256 }, 1);
		assert_eq!(rows.read::<u8>(&gpu, submission, 256).unwrap(), vec![7; 256]);
	}

	#[test]
	fn compact_readbacks_detach_until_consumed_and_cancel_without_pooling() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let reuse = super::super::reuse::Reuse::default();
		let mut pending = Vec::new();
		for value in [17u8, 93, 201] {
			let lease = reuse.lease(&gpu);
			let buffer = lease.buffer("compact test", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, &[value; 8]);
			lease.forget_buffer(&buffer);
			let submission = gpu.submit([]);
			pending.push((MappedRows::new(ids::Readback { buffer, bytes_per_row: 8 }, 1), submission, value));
		}
		// Cancelling one pending mapping must leave the other submitted frames intact.
		drop(pending.remove(1));
		for (mut rows, submission, value) in pending {
			assert_eq!(rows.read::<u8>(&gpu, submission, 6).unwrap(), vec![value; 6]);
			reuse.give_back_buffer(rows.readback.buffer.clone());
		}
		let lease = reuse.lease(&gpu);
		let buffer = lease.buffer("compact test", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, &[42u8; 8]);
		let submission = gpu.submit([]);
		let mut rows = MappedRows::new(ids::Readback { buffer, bytes_per_row: 8 }, 1);
		assert_eq!(rows.read::<u8>(&gpu, submission, 6).unwrap(), vec![42; 6]);
	}
}
