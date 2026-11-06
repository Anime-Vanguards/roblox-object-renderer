//! MP4's compact readback: finished display RGB to BT.709 NV12, after all GPU post effects.
//! CPU overlays keep their RGBA readback and use the same matrix after their composition.

use super::{ids, profile::Profile, reuse::Lease};

#[derive(Clone, Copy)]
pub(crate) struct VideoReadback {
	/// Contact sheets retain the exact lossless image, rather than reconstructing it from YUV.
	pub rgba: bool,
	pub flatten: bool,
}

pub(super) struct Conversion {
	layout: wgpu::BindGroupLayout,
	pipeline: wgpu::ComputePipeline,
}

impl Conversion {
	pub fn new(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("video NV12"),
			entries: &[
				wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
				wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None },
				wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
			],
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("video NV12"), source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/nv12.wgsl").into()) });
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("video NV12"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
		let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("video NV12"), layout: Some(&pipeline_layout), module: &module, entry_point: Some("convert"), compilation_options: Default::default(), cache });
		Self { layout, pipeline }
	}

	pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, lease: &Lease, image: &wgpu::TextureView, (width, height): (u32, u32), flatten: bool, profile: &Profile) -> ids::Readback {
		let size = (width as u64 * height as u64 * 3 / 2).next_multiple_of(4);
		let packed = lease.empty_buffer("video NV12", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, size);
		let readback = lease.empty_buffer("video NV12 readback", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, size);
		let settings = lease.buffer("video NV12 settings", wgpu::BufferUsages::UNIFORM, bytemuck::cast_slice(&[width, height, u32::from(flatten), 0]));
		let group = lease.device().create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("video NV12"), layout: &self.layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(image) },
				wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &packed, offset: 0, size: std::num::NonZeroU64::new(size) }) },
				wgpu::BindGroupEntry { binding: 2, resource: settings.as_entire_binding() },
			],
		});
		{
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("video NV12"), timestamp_writes: profile.compute_pass("video NV12") });
			pass.set_pipeline(&self.pipeline);
			pass.set_bind_group(0, &group, &[]);
			let groups = (size / 4).div_ceil(64) as u32;
			pass.dispatch_workgroups(groups.min(256), groups.div_ceil(256), 1);
		}
		encoder.copy_buffer_to_buffer(&packed, 0, &readback, 0, size);
		ids::Readback { buffer: readback, bytes_per_row: size as u32 }
	}
}

/// The fallback for frames with CPU-composited Highlights or AlwaysOnTop GUIs. The matrix,
/// 2x2 chroma average and alpha rounding match the compute pass; no transfer curve is applied.
pub(crate) fn from_rgba(rgba: &[u8], width: u32, height: u32, flatten: bool) -> Vec<u8> {
	let pixels = width as usize * height as usize;
	let mut output = vec![0u8; pixels * 3 / 2];
	let rgb = |index: usize| -> [f32; 3] {
		let pixel = &rgba[index * 4..index * 4 + 4];
		std::array::from_fn(|channel| if flatten { ((pixel[channel] as u32 * pixel[3] as u32 + 127) / 255) as f32 } else { pixel[channel] as f32 })
	};
	let luma = |value: [f32; 3]| value[0] * 0.2126 + value[1] * 0.7152 + value[2] * 0.0722;
	let byte = |value: f32| value.round().clamp(0.0, 255.0) as u8;
	// rows on every core (a 1080p frame took ~3 ms on one)
	use rayon::prelude::*;
	let width_px = width as usize;
	let (luma_plane, chroma_plane) = output.split_at_mut(pixels);
	luma_plane.par_chunks_mut(width_px.max(1)).enumerate().for_each(|(row, line)| {
		for (column, output) in line.iter_mut().enumerate() {
			*output = byte(16.0 + luma(rgb(row * width_px + column)) * (219.0 / 255.0));
		}
	});
	chroma_plane.par_chunks_mut(width_px.max(1)).enumerate().for_each(|(pair, line)| {
		let row = pair * 2;
		for column in (0..width_px).step_by(2) {
			let index = row * width_px + column;
			let values = [rgb(index), rgb(index + 1), rgb(index + width_px), rgb(index + width_px + 1)];
			let average: [f32; 3] = std::array::from_fn(|channel| values.iter().map(|value| value[channel]).sum::<f32>() * 0.25);
			let y = luma(average);
			line[column] = byte(128.0 + (average[2] - y) * (224.0 / (255.0 * 1.8556)));
			line[column + 1] = byte(128.0 + (average[0] - y) * (224.0 / (255.0 * 1.5748)));
		}
	});
	output
}

#[cfg(test)]
mod tests {
	use super::*;
	use super::super::{gpu::Gpu, reuse::Reuse};

	#[test]
	fn bt709_primary_colours_and_integer_alpha() {
		for (rgba, expected) in [
			([0, 0, 0, 255], [16, 128, 128]),
			([255, 255, 255, 255], [235, 128, 128]),
			([255, 0, 0, 255], [63, 102, 240]),
			([0, 255, 0, 255], [173, 42, 26]),
			([0, 0, 255, 255], [32, 240, 118]),
			([255, 0, 0, 128], [39, 115, 184]),
		] {
			let output = from_rgba(&rgba.repeat(4), 2, 2, true);
			assert_eq!(&output[..4], &[expected[0]; 4]);
			assert_eq!(&output[4..], &expected[1..]);
		}
	}

	#[test]
	fn gpu_conversion_matches_cpu_with_partial_words_and_multiple_dispatch_rows() {
		let gpu = Gpu::new().expect("a GPU (or the software adapter)");
		let conversion = Conversion::new(&gpu.device, None);
		let reuse = Reuse::default();
		for (width, height) in [(2, 2), (6, 2), (258, 258)] {
			for flatten in [false, true] {
				let rgba: Vec<u8> = (0..width * height * 4).map(|index| ((index * 73 + index / 19 * 11) % 256) as u8).collect();
				let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
					label: Some("NV12 test"), size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
					mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
					format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
				});
				gpu.queue.write_texture(wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, &rgba,
					wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
					wgpu::Extent3d { width, height, depth_or_array_layers: 1 });
				let lease = reuse.lease(&gpu);
				let mut encoder = gpu.device.create_command_encoder(&Default::default());
				let readback = conversion.encode(&mut encoder, &lease, &texture.create_view(&Default::default()), (width, height), flatten, &Profile::new(&gpu));
				let submission = gpu.queue.submit([encoder.finish()]);
				let (sender, ready) = std::sync::mpsc::channel();
				let slice = readback.buffer.slice(..readback.bytes_per_row as u64);
				slice.map_async(wgpu::MapMode::Read, move |result| { sender.send(result).unwrap(); });
				gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(std::time::Duration::from_secs(60)) }).unwrap();
				ready.recv().unwrap().unwrap();
				let mapped = slice.get_mapped_range();
				let expected = from_rgba(&rgba, width, height, flatten);
				assert!(mapped[..expected.len()].iter().zip(&expected).all(|(&actual, &expected)| actual.abs_diff(expected) <= 1), "{width}x{height}, flatten={flatten}");
				drop(mapped);
				readback.buffer.unmap();
			}
		}
	}
}
