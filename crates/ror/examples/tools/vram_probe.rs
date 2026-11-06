//! What GPU allocations cost the process in committed system memory: `cargo run -p ror --release
//! --example tools -- vram_probe` (ROR_BACKEND=dx12 for DX12). After each step (textures and buffers made,
//! written, dropped, made again) it prints the process's write-combined and read-write committed
//! memory and wgpu's counts. On a 16 GB GPU (Windows, 2026-10-11) every device-local
//! allocation committed as much write-combined system memory, given back when freed, short of a few
//! hundred MB the driver keeps: a service's private bytes follow what it holds on the GPU
//! (notes/render.md "Service memory").

use std::time::Duration;

#[cfg(windows)]
fn regions() -> (u64, u64, usize) {
	use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, VirtualQuery};
	let (mut address, mut wc, mut rw, mut count) = (0usize, 0u64, 0u64, 0usize);
	let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
	while unsafe { VirtualQuery(address as *const _, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) } != 0 {
		if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE {
			if info.Protect & 0x400 != 0 {
				wc += info.RegionSize as u64;
				count += 1;
			} else {
				rw += info.RegionSize as u64;
			}
		}
		address = info.BaseAddress as usize + info.RegionSize;
		if address >= 0x7FFF_FFFF_0000 {
			break;
		}
	}
	(wc, rw, count)
}

#[cfg(not(windows))]
fn regions() -> (u64, u64, usize) {
	(0, 0, 0)
}

fn report(gpu: &ror::render::gpu::Gpu, step: &str) {
	let _ = gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_secs(10)) });
	std::thread::sleep(Duration::from_millis(200));
	let (wc, rw, count) = regions();
	let hal = gpu.device.get_internal_counters().hal;
	println!(
		"{step:40} WC {:7.1} MB ({count:4} regions)  RW {:7.1} MB | textures {} {:.1} MB, buffers {} {:.1} MB, allocations {}",
		wc as f64 / 1048576.0,
		rw as f64 / 1048576.0,
		hal.textures.read(),
		hal.texture_memory.read() as f64 / 1048576.0,
		hal.buffers.read(),
		hal.buffer_memory.read() as f64 / 1048576.0,
		hal.memory_allocations.read()
	);
}

fn texture(gpu: &ror::render::gpu::Gpu, side: u32, format: wgpu::TextureFormat, mips: u32, usage: wgpu::TextureUsages) -> wgpu::Texture {
	gpu.device.create_texture(&wgpu::TextureDescriptor {
		label: None,
		size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
		mip_level_count: mips,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format,
		usage,
		view_formats: &[],
	})
}

pub fn main() {
	let gpu = ror::render::gpu::Gpu::new().expect("gpu");
	report(&gpu, "device");
	let sampled = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
	let target = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
	let mut kept = Vec::new();
	for _ in 0..16 {
		kept.push(texture(&gpu, 1024, wgpu::TextureFormat::Rgba8Unorm, 1, sampled));
	}
	report(&gpu, "16 x 1024^2 rgba8 sampled, unwritten");
	let data = vec![7u8; 1024 * 1024 * 4];
	for texture in &kept {
		gpu.queue.write_texture(texture.as_image_copy(), &data, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4096), rows_per_image: Some(1024) }, wgpu::Extent3d { width: 1024, height: 1024, depth_or_array_layers: 1 });
	}
	gpu.queue.submit([]);
	report(&gpu, "  written");
	for _ in 0..8 {
		kept.push(texture(&gpu, 2048, wgpu::TextureFormat::Rgba16Float, 1, target));
	}
	report(&gpu, "+8 x 2048^2 rgba16f targets");
	for _ in 0..64 {
		kept.push(texture(&gpu, 256, wgpu::TextureFormat::Rgba8Unorm, 1, sampled));
	}
	report(&gpu, "+64 x 256^2 sampled");
	let buffers: Vec<wgpu::Buffer> = (0..8).map(|_| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16 << 20, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })).collect();
	report(&gpu, "+8 x 16 MiB storage buffers");
	let readbacks: Vec<wgpu::Buffer> = (0..8).map(|_| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16 << 20, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })).collect();
	report(&gpu, "+8 x 16 MiB readbacks");
	drop(readbacks);
	report(&gpu, "-readbacks");
	drop(buffers);
	report(&gpu, "-storage buffers");
	kept.truncate(16);
	report(&gpu, "-targets and small textures");
	kept.clear();
	report(&gpu, "-all textures");
	for round in 0..3 {
		let again: Vec<wgpu::Texture> = (0..8).map(|_| texture(&gpu, 2048, wgpu::TextureFormat::Rgba16Float, 1, target)).collect();
		report(&gpu, &format!("round {round}: 8 x 2048^2 targets again"));
		drop(again);
		report(&gpu, &format!("round {round}:   dropped"));
	}
	let other: Vec<wgpu::Texture> = (0..8).map(|_| texture(&gpu, 1536, wgpu::TextureFormat::Rgba16Float, 1, target)).collect();
	report(&gpu, "8 x 1536^2 targets (another size)");
	drop(other);
	let other: Vec<wgpu::Texture> = (0..24).map(|_| texture(&gpu, 1024, wgpu::TextureFormat::Rgba8Unorm, 1, sampled)).collect();
	report(&gpu, "24 x 1024^2 rgba8 (another size)");
	drop(other);
	report(&gpu, "dropped");
	let other: Vec<wgpu::Texture> = (0..4).map(|_| texture(&gpu, 4096, wgpu::TextureFormat::Rgba16Float, 1, target)).collect();
	report(&gpu, "4 x 4096^2 rgba16f (bigger)");
	drop(other);
	report(&gpu, "dropped");
}
