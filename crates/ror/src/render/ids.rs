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

const LAYERS: &str = include_str!("../../shaders/layers.wgsl");

/// The see-through layers' pipelines (`shaders/layers.wgsl`), made the first time a view asks for
/// them ([`super::ViewRequest::layers`]).
pub struct LayerPipelines {
	layout: wgpu::BindGroupLayout,
	/// single-sided see-through parts and decals (back faces culled, as the main pass draws them)
	single: wgpu::RenderPipeline,
	/// DoubleSided ones
	double: wgpu::RenderPipeline,
}

impl LayerPipelines {
	pub fn new(device: &wgpu::Device, frame_layout: &wgpu::BindGroupLayout, cache: Option<&wgpu::PipelineCache>) -> Self {
		let storage = |binding: u32, read_only: bool, visibility: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
			binding,
			visibility,
			ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
			count: None,
		};
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("layers"),
			entries: &[
				storage(0, true, wgpu::ShaderStages::VERTEX),
				storage(1, false, wgpu::ShaderStages::FRAGMENT),
				storage(2, false, wgpu::ShaderStages::FRAGMENT),
				wgpu::BindGroupLayoutEntry {
					binding: 3,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
					count: None,
				},
			],
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("layers"), source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{LAYERS}").into()) });
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("layers"), bind_group_layouts: &[frame_layout, &layout], push_constant_ranges: &[] });
		let pipeline = |label: &str, cull_mode: Option<wgpu::Face>| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(label),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_layers"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode, ..Default::default() },
				depth_stencil: None,
				multisample: Default::default(),
				// the pass's one attachment, the opaque ids, left as it is: the counts are the output
				fragment: Some(wgpu::FragmentState {
					module: &module,
					entry_point: Some("fs_layers"),
					targets: &[Some(wgpu::ColorTargetState { format: PART_ID_FORMAT, blend: None, write_mask: wgpu::ColorWrites::empty() })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		Self { single: pipeline("layers single-sided", Some(wgpu::Face::Back)), double: pipeline("layers double-sided", None), layout }
	}
}

/// A view's layers being copied out ([`encode_layers`]).
pub struct LayersReadback {
	pub opaque: Readback,
	pub depth: Readback,
	pub pixels: Readback,
	pub parts: Readback,
	pub part_count: u32,
}

/// What lies at every pixel of a view, for the cost of drawing it ([`super::ViewRequest::layers`]).
#[derive(Clone, Debug, Default)]
pub struct Layers {
	pub width: u32,
	pub height: u32,
	/// Per pixel, row by row: the frontmost opaque part's index in `Scene::parts` + 1 (0: none, the sky).
	pub opaque: Vec<u32>,
	/// Per pixel: that surface's depth as drawn, reversed infinite Z (`near / distance along the view`,
	/// 1 at the near plane; 0 where no opaque part is).
	pub depth: Vec<f32>,
	/// Per pixel: fragments of see-through parts and of decals in front of the opaque surface (or of
	/// the sky), each shaded and blended once.
	pub blended: Vec<u32>,
	/// Per part (index in `Scene::parts`): the fragments counted in `blended` that it and its decals
	/// shade.
	pub part_blended: Vec<u32>,
}

/// Encodes a view's [`Layers`]: the opaque parts' ids and depth (decals and see-through parts left
/// out), then every see-through part and decal counted where it lies in front of them. `owners`
/// gives each instance's part index + 1 (0: none).
#[allow(clippy::too_many_arguments)]
pub fn encode_layers(
	device: &wgpu::Device,
	encoder: &mut wgpu::CommandEncoder,
	ids: &IdPipelines,
	layers: &LayerPipelines,
	owners: &[u32],
	part_count: usize,
	frame_group: &wgpu::BindGroup,
	draws: &[super::DrawItem],
	width: u32,
	height: u32,
	profile: &super::profile::Profile,
) -> LayersReadback {
	let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
	let (ids_texture, ids_view, _, opaque) = targets(device, "layers opaque ids", PART_ID_FORMAT, width, height);
	let depth = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("layers depth"),
		size,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: DEPTH_FORMAT,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
		view_formats: &[],
	});
	let depth_view = depth.create_view(&Default::default());
	let owners_group = ids.bind_group(device, owners);
	{
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("layers opaque"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: &ids_view,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
			})],
			depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
				view: &depth_view,
				depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
				stencil_ops: None,
			}),
			timestamp_writes: profile.pass("layers opaque"),
			occlusion_query_set: None,
		});
		pass.set_pipeline(&ids.part_ids);
		pass.set_bind_group(0, frame_group, &[]);
		pass.set_bind_group(1, &owners_group, &[]);
		for item in draws.iter().filter(|item| !item.decal && !item.highlight_only && !item.transparent) {
			pass.set_vertex_buffer(0, item.mesh.vertex_buffer.slice(..));
			pass.set_index_buffer(item.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
			pass.draw_indexed(item.indices.clone(), 0, item.instance..item.instance + 1);
		}
	}
	let counts = |label: &str, count: u64| {
		device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: count.max(1) * 4, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
	};
	let parts = counts("layers parts", part_count as u64);
	let pixels = counts("layers pixels", width as u64 * height as u64);
	encoder.clear_buffer(&parts, 0, None);
	encoder.clear_buffer(&pixels, 0, None);
	let owners_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
		label: Some("layers owners"),
		contents: bytemuck::cast_slice(if owners.is_empty() { &[0u32] } else { owners }),
		usage: wgpu::BufferUsages::STORAGE,
	});
	let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("layers"),
		layout: &layers.layout,
		entries: &[
			wgpu::BindGroupEntry { binding: 0, resource: owners_buffer.as_entire_binding() },
			wgpu::BindGroupEntry { binding: 1, resource: parts.as_entire_binding() },
			wgpu::BindGroupEntry { binding: 2, resource: pixels.as_entire_binding() },
			wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&depth_view) },
		],
	});
	{
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("layers see-through"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: &ids_view,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
			})],
			depth_stencil_attachment: None,
			timestamp_writes: profile.pass("layers see-through"),
			occlusion_query_set: None,
		});
		pass.set_bind_group(0, frame_group, &[]);
		pass.set_bind_group(1, &group, &[]);
		for item in draws.iter().filter(|item| !item.highlight_only && (item.transparent || item.decal)) {
			pass.set_pipeline(if item.double_sided { &layers.double } else { &layers.single });
			pass.set_vertex_buffer(0, item.mesh.vertex_buffer.slice(..));
			pass.set_index_buffer(item.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
			match &item.runs {
				Some(runs) => {
					for run in runs.iter() {
						pass.draw_indexed(run.clone(), 0, item.instance..item.instance + 1);
					}
				}
				None => pass.draw_indexed(item.indices.clone(), 0, item.instance..item.instance + 1),
			}
		}
	}
	copy_out(encoder, &ids_texture, &opaque, width, height);
	let depth_rows = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
	let depth_buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some("layers depth"), size: depth_rows as u64 * height as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
	encoder.copy_texture_to_buffer(
		wgpu::TexelCopyTextureInfo { texture: &depth, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::DepthOnly },
		wgpu::TexelCopyBufferInfo { buffer: &depth_buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(depth_rows), rows_per_image: Some(height) } },
		size,
	);
	let read = |label: &str, source: &wgpu::Buffer, bytes: u64, encoder: &mut wgpu::CommandEncoder| {
		let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
		encoder.copy_buffer_to_buffer(source, 0, &buffer, 0, bytes);
		buffer
	};
	let pixel_bytes = width as u64 * height as u64 * 4;
	let part_bytes = part_count.max(1) as u64 * 4;
	let pixels = Readback { buffer: read("layers pixels readback", &pixels, pixel_bytes, encoder), bytes_per_row: width * 4 };
	let parts = Readback { buffer: read("layers parts readback", &parts, part_bytes, encoder), bytes_per_row: part_bytes as u32 };
	LayersReadback { opaque, depth: Readback { buffer: depth_buffer, bytes_per_row: depth_rows }, pixels, parts, part_count: part_count as u32 }
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


#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use glam::{Affine3A, Vec3};

	use crate::render::camera::CameraSpec;
	use crate::render::{Background, Renderer, ViewRequest};
	use crate::scene::build::{ModelBuilder, PartSpec};

	/// An opaque wall with two see-through sheets in front of it (one over its left half, one over all
	/// of it) and one behind it: the wall is the opaque part where it is, the sheets in front count once
	/// per pixel they cover, the one behind never, and the sky past the wall's edges has no opaque part.
	#[test]
	fn layers_count_the_see_through_fragments_in_front_of_the_opaque_part() {
		let state = std::env::temp_dir().join(format!("ror-layers-test-{}", std::process::id()));
		let gpu = Arc::new(crate::render::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = ModelBuilder::new("Layers");
		let at = |x: f32, z: f32| Affine3A::from_translation(Vec3::new(x, 0.0, z));
		let wall_ref = model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(8.0, 8.0, 1.0), ..Default::default() });
		// a decal on the wall's face toward the camera (Back, +Z): a blended layer of the wall's own, at
		// its depth; one on the face away from it (Front) is hidden
		std::fs::create_dir_all(&state).unwrap();
		let image = state.join("decal.png");
		crate::output::write_png(&image, 4, 4, &[255u8; 64], true).unwrap();
		for face in [2, 5] {
			model.child(wall_ref, rbx_dom_weak::InstanceBuilder::new("Decal").with_property("Texture", rbx_dom_weak::types::ContentId::from(image.display().to_string())).with_property("Face", rbx_dom_weak::types::Enum::from_u32(face)));
		}
		model.part(&PartSpec { name: "Left".into(), cframe: at(-2.0, 2.0), size: Vec3::new(4.0, 8.0, 0.2), transparency: 0.5, ..Default::default() });
		model.part(&PartSpec { name: "All".into(), cframe: at(0.0, 3.0), size: Vec3::new(8.0, 8.0, 0.2), transparency: 0.8, ..Default::default() });
		model.part(&PartSpec { name: "Behind".into(), cframe: at(0.0, -3.0), size: Vec3::new(8.0, 8.0, 0.2), transparency: 0.5, ..Default::default() });
		let scene = crate::scene::extract::extract(&model.into_document());
		let index = |name: &str| scene.parts.iter().position(|part| part.path.ends_with(name)).unwrap_or_else(|| panic!("{name} in the scene"));
		let (wall, left, all, behind) = (index("Wall"), index("Left"), index("All"), index("Behind"));
		let view = ViewRequest { width: 96, height: 64, msaa: 1, bloom: false, layers: true, background: Background::Sky, camera: CameraSpec::LookAt { eye: [0.0, 0.0, 30.0], target: [0.0; 3], fov: 30.0 }, ..Default::default() };
		let (images, _) = renderer.render(&scene, std::slice::from_ref(&view)).expect("a render");
		let layers = images[0].layers.as_ref().expect("the layers");
		assert_eq!((layers.width, layers.height, layers.opaque.len(), layers.depth.len(), layers.blended.len()), (96, 64, 96 * 64, 96 * 64, 96 * 64));
		let at_pixel = |x: u32, y: u32| (y * 96 + x) as usize;
		// the wall's left half (behind both sheets), right half (behind one), and the sky beside it
		let (left_half, right_half, sky) = (at_pixel(40, 32), at_pixel(56, 32), at_pixel(2, 32));
		assert_eq!(layers.opaque[left_half], wall as u32 + 1);
		assert_eq!(layers.opaque[right_half], wall as u32 + 1);
		assert_eq!(layers.opaque[sky], 0);
		assert!(layers.depth[left_half] > 0.0 && layers.depth[sky] == 0.0, "depth {} at the wall, {} at the sky", layers.depth[left_half], layers.depth[sky]);
		// the decal on the wall's face, one sheet or two
		assert_eq!((layers.blended[left_half], layers.blended[right_half], layers.blended[sky]), (3, 2, 0));
		assert_eq!(layers.part_blended[behind], 0, "the sheet behind the wall is hidden");
		let wall_pixels = layers.opaque.iter().filter(|&&id| id == wall as u32 + 1).count() as u32;
		assert_eq!(layers.part_blended[wall], wall_pixels, "the wall's decal over its whole face toward the camera, the other hidden");
		let total: u32 = layers.blended.iter().sum();
		assert_eq!(layers.part_blended[left] + layers.part_blended[all] + layers.part_blended[wall], total, "each part's fragments add up to the pixels' layers");
		assert!(layers.part_blended[all] > layers.part_blended[left] * 3 / 2, "the full sheet covers about twice the half one");
		let _ = std::fs::remove_dir_all(&state);
	}
}
