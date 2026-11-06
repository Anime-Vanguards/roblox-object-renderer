//! Per-part ids drawn into integer targets (`shaders/ids.wgsl`): each instance writes the id a
//! storage buffer gives it. Highlights use 8-bit ids at twice the output resolution
//! ([`super::highlights`]); `ViewRequest::ids` reads back which part is in front at every pixel.

use wgpu::util::DeviceExt;

use super::geometry::Vertex;
use super::pipelines::DEPTH_FORMAT;

const COMMON: &str = include_str!("../../shaders/common.wgsl");
const IDS: &str = include_str!("../../shaders/ids.wgsl");
pub const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Uint;
pub const PART_ID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

pub struct IdPipelines {
	ids_layout: wgpu::BindGroupLayout,
	/// every opaque part's depth (for Highlights' DepthMode Occluded), no ids
	pub highlight_depth: wgpu::RenderPipeline,
	/// Highlight ids where nothing nearer covers them
	pub highlight_occluded: wgpu::RenderPipeline,
	/// Highlight ids over everything
	pub highlight_on_top: wgpu::RenderPipeline,
	/// the frontmost part's id
	pub part_ids: wgpu::RenderPipeline,
}

impl IdPipelines {
	pub fn new(device: &wgpu::Device, frame_layout: &wgpu::BindGroupLayout, cache: Option<&wgpu::PipelineCache>) -> Self {
		let ids_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("ids"),
			entries: &[wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: wgpu::ShaderStages::VERTEX,
				ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
				count: None,
			}],
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ids"), source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{IDS}").into()) });
		let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ids"), bind_group_layouts: &[frame_layout, &ids_layout], push_constant_ranges: &[] });
		let pipeline = |label: &str, format: wgpu::TextureFormat, write_depth: bool, compare: wgpu::CompareFunction, write_ids: bool| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(label),
				layout: Some(&layout),
				vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_ids"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: write_depth, depth_compare: compare, stencil: Default::default(), bias: Default::default() }),
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &module,
					entry_point: Some("fs_ids"),
					targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: if write_ids { wgpu::ColorWrites::ALL } else { wgpu::ColorWrites::empty() } })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		// reversed depth (cleared to 0, nearer is greater), as the main pass
		Self {
			highlight_depth: pipeline("highlight depth", MASK_FORMAT, true, wgpu::CompareFunction::GreaterEqual, false),
			highlight_occluded: pipeline("highlight occluded", MASK_FORMAT, false, wgpu::CompareFunction::GreaterEqual, true),
			highlight_on_top: pipeline("highlight on top", MASK_FORMAT, false, wgpu::CompareFunction::Always, true),
			part_ids: pipeline("part ids", PART_ID_FORMAT, true, wgpu::CompareFunction::GreaterEqual, true),
			ids_layout,
		}
	}

	/// A bind group of per-instance ids.
	pub fn bind_group(&self, device: &wgpu::Device, ids: &[u32]) -> wgpu::BindGroup {
		let contents: &[u32] = if ids.is_empty() { &[0] } else { ids };
		let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("ids"), contents: bytemuck::cast_slice(contents), usage: wgpu::BufferUsages::STORAGE });
		device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("ids"), layout: &self.ids_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }] })
	}
}

/// An id target being copied out: its buffer and row pitch in bytes.
pub struct Readback {
	pub buffer: wgpu::Buffer,
	pub bytes_per_row: u32,
}

/// An integer target and a depth buffer of the given size, and a readback buffer for the target.
pub fn targets(device: &wgpu::Device, label: &str, format: wgpu::TextureFormat, width: u32, height: u32) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView, Readback) {
	let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
	let texture = device.create_texture(&wgpu::TextureDescriptor {
		label: Some(label),
		size,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
		view_formats: &[],
	});
	let depth = device.create_texture(&wgpu::TextureDescriptor {
		label: Some(label),
		size,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: DEPTH_FORMAT,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
		view_formats: &[],
	});
	let bytes_per_texel = format.block_copy_size(None).unwrap_or(4);
	let bytes_per_row = (width * bytes_per_texel).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
	let buffer = device.create_buffer(&wgpu::BufferDescriptor {
		label: Some(label),
		size: bytes_per_row as u64 * height as u64,
		usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
		mapped_at_creation: false,
	});
	let view = texture.create_view(&Default::default());
	let depth_view = depth.create_view(&Default::default());
	(texture, view, depth_view, Readback { buffer, bytes_per_row })
}

/// Copies an id target into its readback buffer.
pub fn copy_out(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, readback: &Readback, width: u32, height: u32) {
	encoder.copy_texture_to_buffer(
		wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
		wgpu::TexelCopyBufferInfo { buffer: &readback.buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(readback.bytes_per_row), rows_per_image: Some(height) } },
		wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
	);
}

/// Encodes which part is in front at every pixel (`part ids`: each drawn part's index + 1, 0 for none)
/// at `width` x `height`, opaque and transparent parts alike, decals not.
pub fn encode_part_ids(
	device: &wgpu::Device,
	encoder: &mut wgpu::CommandEncoder,
	pipelines: &IdPipelines,
	ids: &wgpu::BindGroup,
	frame_group: &wgpu::BindGroup,
	draws: &[super::DrawItem],
	width: u32,
	height: u32,
	profile: &super::profile::Profile,
) -> Readback {
	let (texture, view, depth_view, readback) = targets(device, "part ids", PART_ID_FORMAT, width, height);
	{
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("part ids"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: &view,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
			})],
			depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
				view: &depth_view,
				depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
				stencil_ops: None,
			}),
			timestamp_writes: profile.pass("part ids"),
			occlusion_query_set: None,
		});
		pass.set_pipeline(&pipelines.part_ids);
		pass.set_bind_group(0, frame_group, &[]);
		pass.set_bind_group(1, ids, &[]);
		for item in draws.iter().filter(|item| !item.decal && !item.highlight_only) {
			pass.set_vertex_buffer(0, item.mesh.vertex_buffer.slice(..));
			pass.set_index_buffer(item.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
			pass.draw_indexed(item.indices.clone(), 0, item.instance..item.instance + 1);
		}
	}
	copy_out(encoder, &texture, &readback, width, height);
	readback
}

