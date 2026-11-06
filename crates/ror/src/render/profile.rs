//! Optional per-view GPU timestamps. Normal renders allocate no queries and do not wait here.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::gpu::Gpu;

const QUERY_COUNT: u32 = 1024;

pub(super) struct Profile {
	queries: Option<wgpu::QuerySet>,
	names: RefCell<Vec<String>>,
	inside_passes: bool,
}

impl Profile {
	pub fn new(gpu: &Gpu) -> Self {
		let enabled = std::env::var("ROR_GPU_PROFILE").is_ok_and(|value| value == "1");
		let queries = (enabled && gpu.features.contains(wgpu::Features::TIMESTAMP_QUERY)).then(|| {
			gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("GPU profile"), ty: wgpu::QueryType::Timestamp, count: QUERY_COUNT })
		});
		Self { queries, names: RefCell::new(Vec::new()), inside_passes: gpu.features.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES) }
	}

	fn allocate(&self, name: &str) -> Option<(&wgpu::QuerySet, u32)> {
		let queries = self.queries.as_ref()?;
		let mut names = self.names.borrow_mut();
		let index = names.len() as u32 * 2;
		if index + 1 >= QUERY_COUNT {
			return None;
		}
		names.push(name.to_string());
		Some((queries, index))
	}

	pub fn pass(&self, name: &str) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
		let (query_set, index) = self.allocate(name)?;
		Some(wgpu::RenderPassTimestampWrites { query_set, beginning_of_pass_write_index: Some(index), end_of_pass_write_index: Some(index + 1) })
	}

	pub fn compute_pass(&self, name: &str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
		let (query_set, index) = self.allocate(name)?;
		Some(wgpu::ComputePassTimestampWrites { query_set, beginning_of_pass_write_index: Some(index), end_of_pass_write_index: Some(index + 1) })
	}

	pub fn start(&self, pass: &mut wgpu::RenderPass<'_>, name: &str) -> Option<u32> {
		if !self.inside_passes {
			return None;
		}
		let (queries, index) = self.allocate(name)?;
		pass.write_timestamp(queries, index);
		Some(index)
	}

	pub fn end(&self, pass: &mut wgpu::RenderPass<'_>, index: Option<u32>) {
		if let (Some(queries), Some(index)) = (&self.queries, index) {
			pass.write_timestamp(queries, index + 1);
		}
	}

	pub fn resolve(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> Option<wgpu::Buffer> {
		let queries = self.queries.as_ref()?;
		let count = self.names.borrow().len() as u32 * 2;
		if count == 0 {
			return None;
		}
		let size = count as u64 * 8;
		let buffer = |label, usage| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false });
		let resolved = buffer("GPU timestamps", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
		let readback = buffer("GPU timestamp readback", wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
		encoder.resolve_query_set(queries, 0..count, &resolved, 0);
		encoder.copy_buffer_to_buffer(&resolved, 0, &readback, 0, size);
		Some(readback)
	}

	pub fn report(&self, gpu: &Gpu, submission: super::gpu::Submitted, buffer: Option<wgpu::Buffer>, width: u32, height: u32) {
		let Some(buffer) = buffer else { return };
		let (sender, receiver) = std::sync::mpsc::channel();
		buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| { let _ = sender.send(result); });
		if !matches!(gpu.wait_mapped(submission, &receiver), Ok(Ok(()))) {
			eprintln!("ror GPU profile: timestamp readback failed");
			return;
		}
		let mapped = buffer.slice(..).get_mapped_range();
		let mut passes = BTreeMap::<&str, f64>::new();
		let (mut first, mut last) = (u64::MAX, 0);
		let names = self.names.borrow();
		for (name, pair) in names.iter().zip(mapped.chunks_exact(16)) {
			let start = u64::from_le_bytes(pair[..8].try_into().unwrap());
			let end = u64::from_le_bytes(pair[8..].try_into().unwrap());
			first = first.min(start);
			last = last.max(end);
			*passes.entry(name).or_default() += end.saturating_sub(start) as f64 * gpu.queue.get_timestamp_period() as f64 / 1e6;
		}
		let span = last.saturating_sub(first) as f64 * gpu.queue.get_timestamp_period() as f64 / 1e6;
		eprintln!("ror GPU profile {}", serde_json::json!({"width": width, "height": height, "backend": format!("{:?}", gpu.adapter_info.backend), "spanMs": span, "passesMs": passes}));
		drop(mapped);
		buffer.unmap();
	}
}
