//! How long the GPU device takes to start, step by step, per backend set: `cargo run -p ror --release
//! --example tools -- gpu_start [primary|vulkan|dx12]`.

use std::time::Instant;

pub fn main() {
	let which = std::env::args().nth(1).unwrap_or_else(|| "primary".into());
	let backends = match which.as_str() {
		"vulkan" => wgpu::Backends::VULKAN,
		"dx12" => wgpu::Backends::DX12,
		_ => wgpu::Backends::PRIMARY,
	};
	let started = Instant::now();
	let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, ..Default::default() });
	let instance_ms = started.elapsed().as_secs_f64() * 1000.0;
	let step = Instant::now();
	let adapters = instance.enumerate_adapters(backends);
	let enumerate_ms = step.elapsed().as_secs_f64() * 1000.0;
	let names: Vec<String> = adapters.iter().map(|adapter| format!("{} ({:?})", adapter.get_info().name, adapter.get_info().backend)).collect();
	let step = Instant::now();
	let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: None, force_fallback_adapter: false })).expect("adapter");
	let adapter_ms = step.elapsed().as_secs_f64() * 1000.0;
	let step = Instant::now();
	let (_device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
	let device_ms = step.elapsed().as_secs_f64() * 1000.0;
	println!("{which}: instance {instance_ms:.0} ms, enumerate {enumerate_ms:.0} ms ({names:?}), adapter {adapter_ms:.0} ms ({:?}), device {device_ms:.0} ms, total {:.0} ms", adapter.get_info().name, started.elapsed().as_secs_f64() * 1000.0);
}
