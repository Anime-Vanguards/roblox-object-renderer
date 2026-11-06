//! `ror service warm` (`POST /warm`): the GPU brought up to its working clocks ahead of a job.
//!
//! A GPU idle for a few seconds drops to its idle clocks (NVIDIA's P8: 210-270 MHz, against
//! 2,835 MHz), and the driver raises them only after it has seen load for a while: a clip's first
//! ~0.5 s ran at idle clocks. A caller that pauses between calls paid that nearly every time (a model's
//! clip 694 ms after an 8 s pause against 517 ms right after; a 240-frame 1440p clip's frames 898
//! against 576 ms). A caller that knows a job is coming (a tool, before its export or recording) asks
//! for a warm-up: one burst of compute
//! work, at most [`BURST`], ended as soon as a job starts. Nothing runs while the service is idle
//! otherwise; the burst's results are thrown away and touch nothing a job draws.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::render::gpu::Gpu;

/// The longest one warm-up keeps the GPU busy.
pub const BURST: Duration = Duration::from_millis(500);

/// Workgroups per dispatch: 2,048 x 64 invocations of 2,048 dependent multiply-adds; a batch of two
/// takes ~4 ms at idle clocks and ~0.3 ms at working ones, so a job that starts mid-burst waits
/// for little (16,384 took 31 and 2.4 ms).
const WORKGROUPS: u32 = 2_048;

const SHADER: &str = r"
@group(0) @binding(0) var<storage, read_write> sink: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
	var x = f32(id.x) * 1e-6;
	for (var i = 0u; i < 2048u; i = i + 1u) {
		x = x * 1.0000001 + 0.5;
	}
	sink[id.x & 65535u] = x;
}
";

/// The burst's pipeline and its scratch buffer, made on the first warm-up.
pub(crate) struct Warmer {
	pipeline: wgpu::ComputePipeline,
	group: wgpu::BindGroup,
	_sink: wgpu::Buffer,
}

impl Warmer {
	fn new(gpu: &Gpu) -> Self {
		let device = &gpu.device;
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ror warm"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
		let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
			label: Some("ror warm"),
			layout: None,
			module: &module,
			entry_point: Some("main"),
			compilation_options: Default::default(),
			cache: None,
		});
		let sink = device.create_buffer(&wgpu::BufferDescriptor { label: Some("ror warm sink"), size: 65_536 * 4, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
		let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("ror warm"),
			layout: &pipeline.get_bind_group_layout(0),
			entries: &[wgpu::BindGroupEntry { binding: 0, resource: sink.as_entire_binding() }],
		});
		Warmer { pipeline, group, _sink: sink }
	}
}

/// Keeps the GPU busy for up to [`BURST`] or until `stop` says a job began; returns each batch's
/// time and the whole burst's.
pub(crate) fn burst(gpu: &Gpu, warmer: &OnceLock<Warmer>, stop: impl Fn() -> bool) -> (Vec<Duration>, Duration) {
	let warmer = warmer.get_or_init(|| Warmer::new(gpu));
	let start = Instant::now();
	let mut batches = Vec::new();
	while start.elapsed() < BURST && !stop() {
		let batch = Instant::now();
		let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ror warm") });
		{
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ror warm"), timestamp_writes: None });
			pass.set_pipeline(&warmer.pipeline);
			pass.set_bind_group(0, &warmer.group, &[]);
			pass.dispatch_workgroups(WORKGROUPS, 1, 1);
			pass.dispatch_workgroups(WORKGROUPS, 1, 1);
		}
		let submitted = gpu.submit([encoder.finish()]);
		if gpu.wait_for(submitted).is_err() {
			break;
		}
		batches.push(batch.elapsed());
	}
	(batches, start.elapsed())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_burst_is_bounded_and_ends_when_a_job_starts() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let warmer = OnceLock::new();
		let (batches, took) = burst(&gpu, &warmer, || false);
		assert!(!batches.is_empty());
		// it stops between batches, so within one of them past BURST (a software adapter's takes seconds)
		let slowest = batches.iter().max().copied().unwrap_or_default();
		assert!(took < BURST + slowest + Duration::from_millis(250), "{took:?}");
		let (batches, _) = burst(&gpu, &warmer, || true);
		assert!(batches.is_empty());
	}
}
