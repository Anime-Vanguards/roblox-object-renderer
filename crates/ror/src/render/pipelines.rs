//! Bind group layouts and render pipelines, created once per process.

use super::geometry::Vertex;

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The sun's blurred shadow moments (shadow_blur.wgsl), read with textureLoad
pub const SUN_MOMENTS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

pub(super) const COMMON: &str = include_str!("../../shaders/common.wgsl");
const MESH: &str = include_str!("../../shaders/mesh.wgsl");
/// The lights' voxels read from the view's field (render::voxel_field), for particles, beams and trails
pub(super) const VOXEL_READ: &str = include_str!("../../shaders/voxel_read.wgsl");
pub(super) const SHADOW: &str = include_str!("../../shaders/shadow.wgsl");
const SHADOW_BLUR: &str = include_str!("../../shaders/shadow_blur.wgsl");
pub(crate) const FULLSCREEN: &str = include_str!("../../shaders/fullscreen.wgsl");
const SKY: &str = include_str!("../../shaders/sky.wgsl");
/// Terrain Clouds' generic model, which the sky pass draws (render::clouds)
const CLOUDS: &str = include_str!("../../shaders/clouds.wgsl");
const RESOLVE: &str = include_str!("../../shaders/resolve.wgsl");
const BLOOM: &str = include_str!("../../shaders/bloom.wgsl");
const AO: &str = include_str!("../../shaders/ao.wgsl");
const OPAQUE_SHARE: &str = include_str!("../../shaders/opaque_share.wgsl");
/// A window's world's opaque share (`ViewImage::opaque_share`)
pub const OPAQUE_SHARE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// The ambient occlusion's inputs: the opaque surfaces' depth (ao.wgsl `surface` makes their world positions
/// from it) and normal.
pub const AO_SURFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
pub const AO_NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// The compute blur's result before `fs_ao_blurred` writes it to the 8-bit occlusion (ao.wgsl `cs_ao_blur`)
pub const AO_BLURRED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// The occlusion pass's view depths, for the blur (ao.wgsl `fs_ao_blur`)
pub const AO_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// The ambient occlusion's multisampled prepass (render::ao, `samples` in ROR_TUNE_AO)
pub const AO_SAMPLES: u32 = 4;

pub struct Pipelines {
	pub frame_layout: wgpu::BindGroupLayout,
	pub material_layout: wgpu::BindGroupLayout,
	pub sky_layout: wgpu::BindGroupLayout,
	pub resolve_layout: wgpu::BindGroupLayout,
	pub bloom_layout: wgpu::BindGroupLayout,
	/// fs_blur_add: the input and the level it is added to
	pub bloom_pair_layout: wgpu::BindGroupLayout,
	/// The ambient occlusion: its inputs drawn from the opaque parts (single-sided, double-sided),
	/// the occlusion and its blur (render::ao)
	pub ao_layout: wgpu::BindGroupLayout,
	pub ao_prepass: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	/// The same for the parts drawn by coverage, where they cover every sample
	/// (`fs_ao_prepass_coverage`)
	pub ao_prepass_coverage: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	/// The same into multisampled targets (normals and depth; `fs_ao_prepass_samples`), resolved to
	/// one sample per pixel by `ao_resolve` (render::ao)
	pub ao_prepass_samples: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	pub ao_prepass_coverage_samples: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	pub ao_resolve_layout: wgpu::BindGroupLayout,
	pub ao_resolve: wgpu::RenderPipeline,
	pub ao: wgpu::RenderPipeline,
	pub ao_blur: wgpu::RenderPipeline,
	/// The blur as a compute pass over 16 x 16 tiles kept in workgroup memory, into an
	/// [`AO_BLURRED_FORMAT`] target `ao_blurred` writes to the occlusion as `ao_blur` would (render::ao)
	pub ao_blur_tile_layout: wgpu::BindGroupLayout,
	pub ao_blur_tile: wgpu::ComputePipeline,
	pub ao_blurred: wgpu::RenderPipeline,
	ao_module: wgpu::ShaderModule,
	/// A clip's occlusion kept between frames (render::ao::AoHistory), made the first time one is kept
	ao_history: std::sync::OnceLock<AoHistoryPipelines>,
	pub bloom_extract: wgpu::RenderPipeline,
	pub bloom_down: wgpu::RenderPipeline,
	pub bloom_blur: wgpu::RenderPipeline,
	pub bloom_up: wgpu::RenderPipeline,
	pub bloom_blur_add: wgpu::RenderPipeline,
	pub shadow: wgpu::RenderPipeline,
	/// The sun's cascades: depth from the sun (no bias; both faces of double-sided casters, the faces
	/// turned towards the sun of the rest: `sun_depth_front`), its warped moments blurred along rows,
	/// then along columns into the atlas (shadow_blur.wgsl)
	pub sun_depth: wgpu::RenderPipeline,
	pub sun_depth_front: wgpu::RenderPipeline,
	pub sun_moments_layout: wgpu::BindGroupLayout,
	pub sun_moments: wgpu::RenderPipeline,
	pub sun_blur_layout: wgpu::BindGroupLayout,
	pub sun_blur: wgpu::RenderPipeline,
	/// The shadow pass culling back faces (local lights)
	pub light_shadow: wgpu::RenderPipeline,
	/// The opaque parts' depth from the camera, single-sampled, for ForceField's contact glow and soft
	/// particles: (single-sided, double-sided)
	pub scene_depth: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	/// The same for parts that write depth where their texels are opaque (`fs_cutout_prepass`)
	pub scene_depth_cutout: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	/// The same for parts drawn by coverage, where they cover 2 or more samples (`fs_coverage_prepass`)
	pub scene_depth_coverage: (wgpu::RenderPipeline, wgpu::RenderPipeline),
	/// Opaque pipelines per MSAA sample count: (single-sided, double-sided)
	pub opaque: Vec<(u32, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
	pub transparent: Vec<(u32, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
	/// Transparent Glass (`fs_glass`, replacing what is behind it, writing its depth): (single-sided,
	/// double-sided)
	pub glass: Vec<(u32, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
	/// Depth alone where a transparent part is opaque (`fs_cutout_depth`), after its colour:
	/// (single-sided, double-sided)
	pub cutout_depth: Vec<(u32, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
	/// A part at Transparency 0 whose SurfaceAppearance is in AlphaMode Transparency covering its
	/// alpha's share of the samples (`fs_coverage`, multisampled targets): (single-sided, double-sided)
	pub coverage: Vec<(u32, wgpu::RenderPipeline, wgpu::RenderPipeline)>,
	/// Decals over their (already drawn) part: equal-or-nearer depth, blended, no depth writes
	pub decal: Vec<(u32, wgpu::RenderPipeline)>,
	/// The same on a posed skinned mesh, projected from its bind pose (a second vertex buffer, the bind
	/// mesh's: `vs_decal_bind`)
	pub decal_bind: Vec<(u32, wgpu::RenderPipeline)>,
	pub sky: Vec<(u32, wgpu::RenderPipeline)>,
	/// Texture residency's feedback (render::residency): a material's sampler and maps' sizes and
	/// slots, and the buffer the fragments record into
	pub residency_material_layout: wgpu::BindGroupLayout,
	pub residency_layout: wgpu::BindGroupLayout,
	residency_pipeline_layout: wgpu::PipelineLayout,
	mesh_module: wgpu::ShaderModule,
	cache: Option<wgpu::PipelineCache>,
	/// Its pipelines per sample count (`SAMPLE_COUNTS`), made the first time a view reads levels:
	/// surfaces, decals, decals on posed skinned meshes (`fs_residency`, `fs_residency_decal`)
	residency: [std::sync::OnceLock<[wgpu::RenderPipeline; 3]>; 2],
	pub resolve: wgpu::RenderPipeline,
	/// A window's world (ViewRequest::opaque_share): the share of each pixel's samples an opaque
	/// surface covers, from its depth; (layout, pipeline) single-sampled and multisampled
	pub opaque_share: [(wgpu::BindGroupLayout, wgpu::RenderPipeline); 2],
}

// WebGPU guarantees 1 and 4 for every render format; 8 is adapter-specific
pub const SAMPLE_COUNTS: [u32; 2] = [1, 4];

fn texture_entry(binding: u32, view_dimension: wgpu::TextureViewDimension, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
	wgpu::BindGroupLayoutEntry {
		binding,
		visibility: wgpu::ShaderStages::FRAGMENT,
		ty: wgpu::BindingType::Texture { sample_type, view_dimension, multisampled: false },
		count: None,
	}
}

fn sampler_entry(binding: u32, kind: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
	wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(kind), count: None }
}

fn buffer_entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
	wgpu::BindGroupLayoutEntry {
		binding,
		visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
		ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
		count: None,
	}
}

impl Pipelines {
	pub fn new(device: &wgpu::Device, cache: Option<&wgpu::PipelineCache>) -> Self {
		let float = wgpu::TextureSampleType::Float { filterable: true };
		let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("frame"),
			entries: &[
				buffer_entry(0, wgpu::BufferBindingType::Uniform),
				buffer_entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
				// the sun's blurred shadow moments (shadow.wgsl)
				texture_entry(2, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Float { filterable: false }),
				sampler_entry(3, wgpu::SamplerBindingType::Comparison),
				texture_entry(4, wgpu::TextureViewDimension::Cube, float),
				sampler_entry(5, wgpu::SamplerBindingType::Filtering),
				texture_entry(6, wgpu::TextureViewDimension::D2, float),
				buffer_entry(7, wgpu::BufferBindingType::Storage { read_only: true }),
				// the opaque scene's depth (ForceField contact glow)
				texture_entry(8, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Depth),
				// the Organic pattern's noise
				texture_entry(9, wgpu::TextureViewDimension::D2, float),
				// the surface atlas (Studs, Inlet, ...): albedo and normal
				texture_entry(10, wgpu::TextureViewDimension::D2, float),
				texture_entry(11, wgpu::TextureViewDimension::D2, float),
				// Studio's wang index (pre-2022 Wang-tiled materials)
				texture_entry(12, wgpu::TextureViewDimension::D2, float),
				// local lights' shadow maps (six faces per light with Shadows) and their view-projections
				texture_entry(13, wgpu::TextureViewDimension::D2Array, wgpu::TextureSampleType::Depth),
				buffer_entry(14, wgpu::BufferBindingType::Storage { read_only: true }),
				// the light grid's sky visibility (notes/render.md "Light grid")
				texture_entry(15, wgpu::TextureViewDimension::D3, float),
				// the ambient occlusion (notes/render.md "Ambient occlusion")
				texture_entry(16, wgpu::TextureViewDimension::D2, float),
				// the opaque scene's colour that Glass shows through it
				texture_entry(17, wgpu::TextureViewDimension::D2, float),
				// the lights' voxels baked for the view (render::voxel_field)
				texture_entry(18, wgpu::TextureViewDimension::D3, float),
				// the sky chain distant surfaces fade into through the Atmosphere (common.wgsl `atmosphere_fog`)
				texture_entry(19, wgpu::TextureViewDimension::Cube, float),
				// the view's lights by cluster (render::light_clusters)
				buffer_entry(20, wgpu::BufferBindingType::Storage { read_only: true }),
			],
		});
		let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("material"),
			entries: &[
				texture_entry(0, wgpu::TextureViewDimension::D2, float),
				texture_entry(1, wgpu::TextureViewDimension::D2, float),
				texture_entry(2, wgpu::TextureViewDimension::D2, float),
				texture_entry(3, wgpu::TextureViewDimension::D2, float),
				sampler_entry(4, wgpu::SamplerBindingType::Filtering),
				texture_entry(5, wgpu::TextureViewDimension::D2, float),
				// each map's full size (mesh.wgsl MaterialInfo)
				buffer_entry(6, wgpu::BufferBindingType::Uniform),
			],
		});
		let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("sky"),
			entries: &[
				buffer_entry(0, wgpu::BufferBindingType::Uniform),
				texture_entry(1, wgpu::TextureViewDimension::Cube, float),
				sampler_entry(2, wgpu::SamplerBindingType::Filtering),
				buffer_entry(3, wgpu::BufferBindingType::Uniform),
				// the sun's and the moon's textures
				texture_entry(4, wgpu::TextureViewDimension::D2, float),
				texture_entry(5, wgpu::TextureViewDimension::D2, float),
				// the skybox's six images
				texture_entry(6, wgpu::TextureViewDimension::D2Array, float),
				// Terrain Clouds' shape volume and detail volume (render::clouds)
				texture_entry(7, wgpu::TextureViewDimension::D3, float),
				sampler_entry(8, wgpu::SamplerBindingType::Filtering),
				texture_entry(9, wgpu::TextureViewDimension::D3, float),
			],
		});
		let unfiltered = wgpu::TextureSampleType::Float { filterable: false };
		let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("resolve"),
			entries: &[
				texture_entry(0, wgpu::TextureViewDimension::D2, unfiltered),
				buffer_entry(1, wgpu::BufferBindingType::Uniform),
				// the bloom at half the working resolution, sampled bilinearly
				texture_entry(2, wgpu::TextureViewDimension::D2, float),
				sampler_entry(4, wgpu::SamplerBindingType::Filtering),
				// SunRaysEffect's rays, a quarter of the resolution
				texture_entry(6, wgpu::TextureViewDimension::D2, float),
			],
		});
		let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("bloom"),
			entries: &[texture_entry(0, wgpu::TextureViewDimension::D2, unfiltered), buffer_entry(1, wgpu::BufferBindingType::Uniform)],
		});
		let bloom_pair_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("bloom pair"),
			entries: &[
				texture_entry(0, wgpu::TextureViewDimension::D2, unfiltered),
				buffer_entry(1, wgpu::BufferBindingType::Uniform),
				texture_entry(2, wgpu::TextureViewDimension::D2, unfiltered),
			],
		});

		let mesh_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("mesh"),
			source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{SHADOW}\n{MESH}").into()),
		});
		let sky_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("sky"),
			source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{FULLSCREEN}\n{CLOUDS}\n{SKY}").into()),
		});
		let resolve_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("resolve"),
			source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{FULLSCREEN}\n{RESOLVE}").into()),
		});
		let bloom_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("bloom"),
			source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{FULLSCREEN}\n{BLOOM}").into()),
		});
		let ao_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("ao"),
			source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{FULLSCREEN}\n{AO}").into()),
		});
		let ao_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("ao"),
			entries: &[
				texture_entry(0, wgpu::TextureViewDimension::D2, unfiltered),
				texture_entry(1, wgpu::TextureViewDimension::D2, unfiltered),
				buffer_entry(2, wgpu::BufferBindingType::Uniform),
				texture_entry(3, wgpu::TextureViewDimension::D2, unfiltered),
				texture_entry(7, wgpu::TextureViewDimension::D2, unfiltered),
			],
		});
		let computed = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
		let ao_blur_tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("ao blur tile"),
			entries: &[
				computed(2, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
				computed(3, wgpu::BindingType::Texture { sample_type: unfiltered, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }),
				computed(7, wgpu::BindingType::Texture { sample_type: unfiltered, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }),
				computed(8, wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: AO_BLURRED_FORMAT, view_dimension: wgpu::TextureViewDimension::D2 }),
			],
		});
		let multisampled = |binding: u32, sample_type: wgpu::TextureSampleType| wgpu::BindGroupLayoutEntry {
			binding,
			visibility: wgpu::ShaderStages::FRAGMENT,
			ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: true },
			count: None,
		};
		let ao_resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("ao resolve"),
			entries: &[buffer_entry(2, wgpu::BufferBindingType::Uniform), multisampled(5, wgpu::TextureSampleType::Depth), multisampled(6, unfiltered)],
		});

		let sun_moments_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("sun moments"),
			entries: &[buffer_entry(0, wgpu::BufferBindingType::Uniform), texture_entry(1, wgpu::TextureViewDimension::D2, wgpu::TextureSampleType::Depth)],
		});
		let sun_blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("sun blur"),
			entries: &[buffer_entry(0, wgpu::BufferBindingType::Uniform), texture_entry(2, wgpu::TextureViewDimension::D2, unfiltered)],
		});
		let sun_blur_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("sun blur"),
			source: wgpu::ShaderSource::Wgsl(format!("{FULLSCREEN}\n{SHADOW_BLUR}").into()),
		});

		let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("mesh"),
			bind_group_layouts: &[&frame_layout, &material_layout],
			push_constant_ranges: &[],
		});
		let residency_material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("residency material"),
			entries: &[sampler_entry(4, wgpu::SamplerBindingType::Filtering), buffer_entry(6, wgpu::BufferBindingType::Uniform)],
		});
		let residency_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("residency"),
			entries: &[wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: wgpu::ShaderStages::FRAGMENT,
				ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None },
				count: None,
			}],
		});
		let residency_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("residency"),
			bind_group_layouts: &[&frame_layout, &residency_material_layout, &residency_layout],
			push_constant_ranges: &[],
		});
		let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("shadow"),
			bind_group_layouts: &[&frame_layout],
			push_constant_ranges: &[],
		});
		let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("sky"),
			bind_group_layouts: &[&sky_layout],
			push_constant_ranges: &[],
		});
		let resolve_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("resolve"),
			bind_group_layouts: &[&resolve_layout],
			push_constant_ranges: &[],
		});

		let shadow_pipeline = |cull: Option<wgpu::Face>| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("shadow"),
				layout: Some(&shadow_layout),
				vertex: wgpu::VertexState {
					module: &mesh_module,
					entry_point: Some("vs_shadow"),
					buffers: &[Vertex::layout()],
					compilation_options: Default::default(),
				},
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: SHADOW_FORMAT,
					depth_write_enabled: true,
					depth_compare: wgpu::CompareFunction::LessEqual,
					stencil: Default::default(),
					bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
				}),
				multisample: Default::default(),
				fragment: None,
				multiview: None,
				cache,
			})
		};
		let sun_depth_pipeline = |cull: Option<wgpu::Face>| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("sun depth"),
				layout: Some(&shadow_layout),
				vertex: wgpu::VertexState { module: &mesh_module, entry_point: Some("vs_shadow"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: SHADOW_FORMAT,
					depth_write_enabled: true,
					depth_compare: wgpu::CompareFunction::LessEqual,
					stencil: Default::default(),
					bias: Default::default(),
				}),
				multisample: Default::default(),
				fragment: None,
				multiview: None,
				cache,
			})
		};
		let sun_pass = |label: &str, layout: &wgpu::BindGroupLayout, entry: &str| {
			let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[layout], push_constant_ranges: &[] });
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(label),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState { module: &sun_blur_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &sun_blur_module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format: SUN_MOMENTS_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let scene_depth_pipeline = |cull: Option<wgpu::Face>| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("scene depth"),
				layout: Some(&shadow_layout),
				vertex: wgpu::VertexState { module: &mesh_module, entry_point: Some("vs_main"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: DEPTH_FORMAT,
					depth_write_enabled: true,
					depth_compare: wgpu::CompareFunction::GreaterEqual,
					stencil: Default::default(),
					bias: Default::default(),
				}),
				multisample: Default::default(),
				fragment: None,
				multiview: None,
				cache,
			})
		};
		let scene_depth_cutout_pipeline = |cull: Option<wgpu::Face>, entry: &str| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("scene depth cutout"),
				layout: Some(&mesh_layout),
				vertex: wgpu::VertexState { module: &mesh_module, entry_point: Some("vs_main"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: DEPTH_FORMAT,
					depth_write_enabled: true,
					depth_compare: wgpu::CompareFunction::GreaterEqual,
					stencil: Default::default(),
					bias: Default::default(),
				}),
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState { module: &mesh_module, entry_point: Some(entry), targets: &[], compilation_options: Default::default() }),
				multiview: None,
				cache,
			})
		};

		let bind_buffers = [Vertex::layout(), Vertex::bind_layout()];
		let mesh_pipeline_from = |vertex: &str, buffers: &[wgpu::VertexBufferLayout], samples: u32, entry: &str, cull: Option<wgpu::Face>, blend: Option<wgpu::BlendState>, write_depth: bool, write_mask: wgpu::ColorWrites| {
			let depth_compare = wgpu::CompareFunction::GreaterEqual;
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(&mesh_layout),
				vertex: wgpu::VertexState { module: &mesh_module, entry_point: Some(vertex), buffers, compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: DEPTH_FORMAT,
					depth_write_enabled: write_depth,
					depth_compare,
					stencil: Default::default(),
					bias: Default::default(),
				}),
				multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
				fragment: Some(wgpu::FragmentState {
					module: &mesh_module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend, write_mask })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let mesh_pipeline = |samples: u32, entry: &str, cull: Option<wgpu::Face>, blend: Option<wgpu::BlendState>, write_depth: bool, write_mask: wgpu::ColorWrites| {
			mesh_pipeline_from("vs_main", &bind_buffers[..1], samples, entry, cull, blend, write_depth, write_mask)
		};
		let all = wgpu::ColorWrites::ALL;
		let premultiplied = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;
		// one sample count's mesh pipelines in two halves: decals and the opaque and transparent parts;
		// Glass, cutouts, coverage and the sky
		let mesh_set_a = |samples: u32| {
			(
				(samples, mesh_pipeline(samples, "fs_decal", Some(wgpu::Face::Back), Some(premultiplied), false, all)),
				(samples, mesh_pipeline_from("vs_decal_bind", &bind_buffers, samples, "fs_decal", Some(wgpu::Face::Back), Some(premultiplied), false, all)),
				(samples, mesh_pipeline(samples, "fs_opaque", Some(wgpu::Face::Back), None, true, all), mesh_pipeline(samples, "fs_opaque", None, None, true, all)),
				(samples, mesh_pipeline(samples, "fs_transparent", Some(wgpu::Face::Back), Some(premultiplied), false, all), mesh_pipeline(samples, "fs_transparent", None, Some(premultiplied), false, all)),
			)
		};
		let mesh_set_b = |samples: u32| {
			(
				(samples, mesh_pipeline(samples, "fs_glass", Some(wgpu::Face::Back), None, true, all), mesh_pipeline(samples, "fs_glass", None, None, true, all)),
				(samples, mesh_pipeline(samples, "fs_cutout_depth", Some(wgpu::Face::Back), None, true, wgpu::ColorWrites::empty()), mesh_pipeline(samples, "fs_cutout_depth", None, None, true, wgpu::ColorWrites::empty())),
				(samples, mesh_pipeline(samples, "fs_coverage", Some(wgpu::Face::Back), None, true, all), mesh_pipeline(samples, "fs_coverage", None, None, true, all)),
				(
					samples,
					device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
					label: Some("sky"),
					layout: Some(&sky_pipeline_layout),
					vertex: wgpu::VertexState { module: &sky_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
					primitive: Default::default(),
					depth_stencil: Some(wgpu::DepthStencilState {
						format: DEPTH_FORMAT,
						depth_write_enabled: false,
						depth_compare: wgpu::CompareFunction::Equal,
						stencil: Default::default(),
						bias: Default::default(),
					}),
					multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
					fragment: Some(wgpu::FragmentState {
						module: &sky_module,
						entry_point: Some("fs_sky"),
						targets: &[Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
						compilation_options: Default::default(),
					}),
					multiview: None,
					cache,
				}),
				),
			)
		};
		let resolve_pipeline = || device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("resolve"),
			layout: Some(&resolve_pipeline_layout),
			vertex: wgpu::VertexState { module: &resolve_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
			primitive: Default::default(),
			depth_stencil: None,
			multisample: Default::default(),
			fragment: Some(wgpu::FragmentState {
				module: &resolve_module,
				entry_point: Some("fs_resolve"),
				targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
				compilation_options: Default::default(),
			}),
			multiview: None,
			cache,
		});
		let opaque_share_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("opaque share"),
			source: wgpu::ShaderSource::Wgsl(format!("{FULLSCREEN}\n{OPAQUE_SHARE}").into()),
		});
		let opaque_share_pipelines = || [(false, "fs_opaque_share"), (true, "fs_opaque_share_samples")].map(|(multisampled, entry)| {
			let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some("opaque share"),
				entries: &[wgpu::BindGroupLayoutEntry {
					binding: multisampled as u32,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled },
					count: None,
				}],
			});
			let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("opaque share"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
			let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState { module: &opaque_share_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &opaque_share_module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format: OPAQUE_SHARE_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			});
			(layout, pipeline)
		});
		let bloom_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("bloom"),
			bind_group_layouts: &[&bloom_layout],
			push_constant_ranges: &[],
		});
		let bloom_pair_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("bloom pair"),
			bind_group_layouts: &[&bloom_pair_layout],
			push_constant_ranges: &[],
		});
		let bloom_pipeline = |entry: &str, layout: &wgpu::PipelineLayout| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(layout),
				vertex: wgpu::VertexState { module: &bloom_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &bloom_module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};

		// the ambient occlusion: depths and normals of the opaque parts, the occlusion, its blur
		let ao_prepass_pipeline = |cull: Option<wgpu::Face>, layout: &wgpu::PipelineLayout, entry: &str, samples: u32| {
			let both = [
				Some(wgpu::ColorTargetState { format: AO_SURFACE_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
				Some(wgpu::ColorTargetState { format: AO_NORMAL_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
			];
			let normal = [Some(wgpu::ColorTargetState { format: AO_NORMAL_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })];
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(layout),
				vertex: wgpu::VertexState { module: &mesh_module, entry_point: Some("vs_main"), buffers: &[Vertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
				depth_stencil: Some(wgpu::DepthStencilState {
					format: DEPTH_FORMAT,
					depth_write_enabled: true,
					depth_compare: wgpu::CompareFunction::GreaterEqual,
					stencil: Default::default(),
					bias: Default::default(),
				}),
				multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
				fragment: Some(wgpu::FragmentState {
					module: &mesh_module,
					entry_point: Some(entry),
					targets: if samples > 1 { &normal[..] } else { &both[..] },
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let ao_resolve_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ao resolve"), bind_group_layouts: &[&ao_resolve_layout], push_constant_ranges: &[] });
		let ao_resolve_pipeline = || device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("fs_ao_resolve"),
			layout: Some(&ao_resolve_pipeline_layout),
			vertex: wgpu::VertexState { module: &ao_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
			primitive: Default::default(),
			depth_stencil: None,
			multisample: Default::default(),
			fragment: Some(wgpu::FragmentState {
				module: &ao_module,
				entry_point: Some("fs_ao_resolve"),
				targets: &[
					Some(wgpu::ColorTargetState { format: AO_SURFACE_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
					Some(wgpu::ColorTargetState { format: AO_NORMAL_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL }),
				],
				compilation_options: Default::default(),
			}),
			multiview: None,
			cache,
		});
		let ao_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ao"), bind_group_layouts: &[&ao_layout], push_constant_ranges: &[] });
		let ao_pipeline = |entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(&ao_pipeline_layout),
				vertex: wgpu::VertexState { module: &ao_module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &ao_module,
					entry_point: Some(entry),
					targets,
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let ao_target = |format: wgpu::TextureFormat| Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL });
		let ao_blur_tile_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ao blur tile"), bind_group_layouts: &[&ao_blur_tile_layout], push_constant_ranges: &[] });
		let ao_blur_tile_pipeline = || {
			device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
				label: Some("cs_ao_blur"),
				layout: Some(&ao_blur_tile_pipeline_layout),
				module: &ao_module,
				entry_point: Some("cs_ao_blur"),
				compilation_options: Default::default(),
				cache,
			})
		};

		// Each pipeline is its entry points translated (naga) and the driver's compile, or its pipeline
		// cache's copy: 40 ms one after another at every start (3.7-5.1 s where the driver compiles them
		// anew); made in groups on threads of their own (the driver keeps its cache thread-safe)
		let (mut shadows, mut depths, mut ao_prepasses, mut ao_passes, mut post) = (None, None, None, None, None);
		let mut sets_a = [None, None];
		let mut sets_b = [None, None];
		std::thread::scope(|scope| {
			scope.spawn(|| {
				// a single-sided caster shadows from the faces turned towards the sun, as local lights' do:
				// an inside-out sphere does not darken its own inside (glass-zero sphere-t0-shadow,
				// sphere-plastic-shadow: an effect's bubble, Glass and SmoothPlastic, lit whole); local
				// lights' shadows come from the faces turned towards the light: a part around the light
				// (its own or another) does not darken the room (light-shadows "own-ball", "other-ball")
				shadows = Some((
					shadow_pipeline(None),
					sun_depth_pipeline(None),
					sun_depth_pipeline(Some(wgpu::Face::Back)),
					sun_pass("sun moments", &sun_moments_layout, "fs_sun_moments"),
					sun_pass("sun blur", &sun_blur_layout, "fs_sun_blur"),
					shadow_pipeline(Some(wgpu::Face::Back)),
				));
			});
			scope.spawn(|| {
				depths = Some((
					(scene_depth_pipeline(Some(wgpu::Face::Back)), scene_depth_pipeline(None)),
					(scene_depth_cutout_pipeline(Some(wgpu::Face::Back), "fs_cutout_prepass"), scene_depth_cutout_pipeline(None, "fs_cutout_prepass")),
					(scene_depth_cutout_pipeline(Some(wgpu::Face::Back), "fs_coverage_prepass"), scene_depth_cutout_pipeline(None, "fs_coverage_prepass")),
				));
			});
			let (mesh_set_a, mesh_set_b) = (&mesh_set_a, &mesh_set_b);
			for ((set_a, set_b), samples) in sets_a.iter_mut().zip(sets_b.iter_mut()).zip(SAMPLE_COUNTS) {
				scope.spawn(move || *set_a = Some(mesh_set_a(samples)));
				scope.spawn(move || *set_b = Some(mesh_set_b(samples)));
			}
			scope.spawn(|| {
				ao_prepasses = Some((
					(ao_prepass_pipeline(Some(wgpu::Face::Back), &shadow_layout, "fs_ao_prepass", 1), ao_prepass_pipeline(None, &shadow_layout, "fs_ao_prepass", 1)),
					(ao_prepass_pipeline(Some(wgpu::Face::Back), &mesh_layout, "fs_ao_prepass_coverage", 1), ao_prepass_pipeline(None, &mesh_layout, "fs_ao_prepass_coverage", 1)),
					(ao_prepass_pipeline(Some(wgpu::Face::Back), &shadow_layout, "fs_ao_prepass_samples", AO_SAMPLES), ao_prepass_pipeline(None, &shadow_layout, "fs_ao_prepass_samples", AO_SAMPLES)),
					(
						ao_prepass_pipeline(Some(wgpu::Face::Back), &mesh_layout, "fs_ao_prepass_coverage_samples", AO_SAMPLES),
						ao_prepass_pipeline(None, &mesh_layout, "fs_ao_prepass_coverage_samples", AO_SAMPLES),
					),
				));
			});
			scope.spawn(|| {
				ao_passes = Some((
					ao_resolve_pipeline(),
					ao_pipeline("fs_ao", &[ao_target(AO_FORMAT), ao_target(AO_DEPTH_FORMAT)]),
					ao_pipeline("fs_ao_blur", &[ao_target(AO_FORMAT)]),
					ao_blur_tile_pipeline(),
					ao_pipeline("fs_ao_blurred", &[ao_target(AO_FORMAT)]),
				))
			});
			post = Some((
				resolve_pipeline(),
				opaque_share_pipelines(),
				bloom_pipeline("fs_extract", &bloom_pipeline_layout),
				bloom_pipeline("fs_down", &bloom_pipeline_layout),
				bloom_pipeline("fs_blur", &bloom_pipeline_layout),
				bloom_pipeline("fs_up", &bloom_pipeline_layout),
				bloom_pipeline("fs_blur_add", &bloom_pair_pipeline_layout),
			));
		});
		let made = "the pipelines' threads";
		let (shadow, sun_depth, sun_depth_front, sun_moments, sun_blur, light_shadow) = shadows.expect(made);
		let (scene_depth, scene_depth_cutout, scene_depth_coverage) = depths.expect(made);
		let (ao_prepass, ao_prepass_coverage, ao_prepass_samples, ao_prepass_coverage_samples) = ao_prepasses.expect(made);
		let (ao_resolve, ao, ao_blur, ao_blur_tile, ao_blurred) = ao_passes.expect(made);
		let (resolve, opaque_share, bloom_extract, bloom_down, bloom_blur, bloom_up, bloom_blur_add) = post.expect(made);
		let (mut decal, mut decal_bind, mut opaque, mut transparent) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
		for set in sets_a {
			let (one_decal, one_decal_bind, one_opaque, one_transparent) = set.expect(made);
			decal.push(one_decal);
			decal_bind.push(one_decal_bind);
			opaque.push(one_opaque);
			transparent.push(one_transparent);
		}
		let (mut glass, mut cutout_depth, mut coverage, mut sky) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
		for set in sets_b {
			let (one_glass, one_cutout_depth, one_coverage, one_sky) = set.expect(made);
			glass.push(one_glass);
			cutout_depth.push(one_cutout_depth);
			coverage.push(one_coverage);
			sky.push(one_sky);
		}
		Self {
			frame_layout,
			material_layout,
			sky_layout,
			resolve_layout,
			bloom_layout,
			bloom_pair_layout,
			ao_layout,
			ao_prepass,
			ao_prepass_coverage,
			ao_prepass_samples,
			ao_prepass_coverage_samples,
			ao_resolve_layout,
			ao_resolve,
			ao,
			ao_blur,
			ao_blur_tile_layout,
			ao_blur_tile,
			ao_blurred,
			ao_module,
			ao_history: Default::default(),
			bloom_extract,
			bloom_down,
			bloom_blur,
			bloom_up,
			bloom_blur_add,
			shadow,
			sun_depth,
			sun_depth_front,
			sun_moments_layout,
			sun_moments,
			sun_blur_layout,
			sun_blur,
			light_shadow,
			scene_depth,
			scene_depth_cutout,
			scene_depth_coverage,
			opaque,
			transparent,
			glass,
			cutout_depth,
			coverage,
			decal,
			decal_bind,
			sky,
			residency_material_layout,
			residency_layout,
			residency_pipeline_layout,
			mesh_module,
			cache: cache.cloned(),
			residency: Default::default(),
			resolve,
			opaque_share,
		}
	}

	/// Whether [`Pipelines::residency`] made its pipelines for `samples` already.
	/// The pipelines of a clip's occlusion kept between frames (render::ao::AoHistory, ao.wgsl
	/// `cs_ao_changed` ...): made the first time a clip keeps one.
	pub fn ao_history(&self, device: &wgpu::Device) -> &AoHistoryPipelines {
		self.ao_history.get_or_init(|| AoHistoryPipelines::new(device, &self.ao_module, self.cache.as_ref()))
	}

	pub fn residency_made(&self, samples: u32) -> bool {
		self.residency[SAMPLE_COUNTS.iter().position(|&count| count == samples).unwrap_or(0)].get().is_some()
	}

	/// The residency feedback's pipelines for `samples` (render::residency): surfaces, decals, decals
	/// on posed skinned meshes. Every fragment of every face (no culling), whatever is in front (no
	/// depth test): a superset of what the view's passes sample.
	pub fn residency(&self, device: &wgpu::Device, samples: u32) -> &[wgpu::RenderPipeline; 3] {
		let index = SAMPLE_COUNTS.iter().position(|&count| count == samples).unwrap_or(0);
		if let Some(made) = self.residency[index].get() {
			return made;
		}
		self.residency[index].get_or_init(|| {
			let bind_buffers = [Vertex::layout(), Vertex::bind_layout()];
			let pipeline = |vertex: &str, buffers: &[wgpu::VertexBufferLayout], entry: &str| {
				device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
					label: Some(entry),
					layout: Some(&self.residency_pipeline_layout),
					vertex: wgpu::VertexState { module: &self.mesh_module, entry_point: Some(vertex), buffers, compilation_options: Default::default() },
					primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
					depth_stencil: Some(wgpu::DepthStencilState {
						format: DEPTH_FORMAT,
						depth_write_enabled: false,
						depth_compare: wgpu::CompareFunction::Always,
						stencil: Default::default(),
						bias: Default::default(),
					}),
					multisample: wgpu::MultisampleState { count: SAMPLE_COUNTS[index], mask: !0, alpha_to_coverage_enabled: false },
					fragment: Some(wgpu::FragmentState { module: &self.mesh_module, entry_point: Some(entry), targets: &[], compilation_options: Default::default() }),
					multiview: None,
					cache: self.cache.as_ref(),
				})
			};
			[
				pipeline("vs_main", &bind_buffers[..1], "fs_residency"),
				pipeline("vs_main", &bind_buffers[..1], "fs_residency_decal"),
				pipeline("vs_decal_bind", &bind_buffers, "fs_residency_decal"),
			]
		})
	}

	/// Transparent Glass (`fs_glass`), written over what is behind it with its depth.
	pub fn glass(&self, samples: u32, double_sided: bool) -> &wgpu::RenderPipeline {
		let (_, single, double) = self.glass.iter().find(|(count, ..)| *count == samples).unwrap_or(&self.glass[0]);
		if double_sided { double } else { single }
	}

	pub fn opaque(&self, samples: u32, double_sided: bool) -> &wgpu::RenderPipeline {
		let (_, single, double) = self.opaque.iter().find(|(count, ..)| *count == samples).unwrap_or(&self.opaque[0]);
		if double_sided { double } else { single }
	}

	pub fn transparent(&self, samples: u32, double_sided: bool) -> &wgpu::RenderPipeline {
		let (_, single, double) = self.transparent.iter().find(|(count, ..)| *count == samples).unwrap_or(&self.transparent[0]);
		if double_sided { double } else { single }
	}

	pub fn cutout_depth(&self, samples: u32, double_sided: bool) -> &wgpu::RenderPipeline {
		let (_, single, double) = self.cutout_depth.iter().find(|(count, ..)| *count == samples).unwrap_or(&self.cutout_depth[0]);
		if double_sided { double } else { single }
	}

	pub fn coverage(&self, samples: u32, double_sided: bool) -> &wgpu::RenderPipeline {
		let (_, single, double) = self.coverage.iter().find(|(count, ..)| *count == samples).unwrap_or(&self.coverage[0]);
		if double_sided { double } else { single }
	}

	/// `bind_pose`: on a posed skinned mesh, projected from its bind pose (its bind vertices in slot 1).
	pub fn decal(&self, samples: u32, bind_pose: bool) -> &wgpu::RenderPipeline {
		let pipelines = if bind_pose { &self.decal_bind } else { &self.decal };
		&pipelines.iter().find(|(count, _)| *count == samples).unwrap_or(&pipelines[0]).1
	}

	pub fn sky(&self, samples: u32) -> &wgpu::RenderPipeline {
		&self.sky.iter().find(|(count, _)| *count == samples).unwrap_or(&self.sky[0]).1
	}
}

/// The passes of a clip's occlusion kept between frames (render::ao::AoHistory; ao.wgsl).
pub struct AoHistoryPipelines {
	/// Which tiles' depths or normals changed (`cs_ao_changed`)
	pub changed_layout: wgpu::BindGroupLayout,
	pub changed: wgpu::ComputePipeline,
	/// This frame's depths and normals kept for the next (`cs_ao_keep`)
	pub keep_layout: wgpu::BindGroupLayout,
	pub keep: wgpu::ComputePipeline,
	/// The lists of tiles the disk and the blur run on, and their indirect arguments (`cs_ao_tiles`)
	pub tiles_layout: wgpu::BindGroupLayout,
	pub tiles: wgpu::ComputePipeline,
	/// `fs_ao` and `fs_ao_blurred` over the listed tiles (`vs_ao_tile`), the blur over its list
	/// (`cs_ao_blur_list`)
	pub tile_layout: wgpu::BindGroupLayout,
	pub disk: wgpu::RenderPipeline,
	pub blurred: wgpu::RenderPipeline,
	pub blur_list_layout: wgpu::BindGroupLayout,
	pub blur_list: wgpu::ComputePipeline,
}

impl AoHistoryPipelines {
	fn new(device: &wgpu::Device, module: &wgpu::ShaderModule, cache: Option<&wgpu::PipelineCache>) -> Self {
		let unfiltered = wgpu::TextureSampleType::Float { filterable: false };
		let entry = |binding: u32, visibility: wgpu::ShaderStages, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None };
		let texture = |binding: u32, visibility: wgpu::ShaderStages| entry(binding, visibility, wgpu::BindingType::Texture { sample_type: unfiltered, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false });
		let storage = |binding: u32, read_only: bool| entry(binding, wgpu::ShaderStages::COMPUTE, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None });
		let uniform = |binding: u32, visibility: wgpu::ShaderStages| entry(binding, visibility, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None });
		let written = |binding: u32, format: wgpu::TextureFormat| entry(binding, wgpu::ShaderStages::COMPUTE, wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D2 });
		let layout = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
		let compute = wgpu::ShaderStages::COMPUTE;
		let fragment = wgpu::ShaderStages::FRAGMENT;
		let changed_layout = layout("ao changed", &[texture(0, compute), texture(1, compute), texture(9, compute), texture(10, compute), storage(11, false)]);
		let keep_layout = layout("ao keep", &[texture(0, compute), texture(1, compute), uniform(12, compute), written(13, AO_SURFACE_FORMAT), written(14, AO_NORMAL_FORMAT), storage(15, true)]);
		let tiles_layout = layout("ao tiles", &[uniform(12, compute), storage(15, true), storage(16, false)]);
		// ao_layout's, the history's sizes for the tiles' quads besides
		let tile_layout = layout("ao tile", &[texture(0, fragment), texture(1, fragment), uniform(2, wgpu::ShaderStages::VERTEX | fragment), texture(3, fragment), texture(7, fragment), uniform(12, wgpu::ShaderStages::VERTEX)]);
		let blur_list_layout = layout(
			"ao blur list",
			&[uniform(2, compute), texture(3, compute), texture(7, compute), written(8, AO_BLURRED_FORMAT), storage(17, true)],
		);
		let compute_pipeline = |label: &str, layout: &wgpu::BindGroupLayout| {
			let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[layout], push_constant_ranges: &[] });
			device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(label), layout: Some(&pipeline_layout), module, entry_point: Some(label), compilation_options: Default::default(), cache })
		};
		let tile_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("ao tile"), bind_group_layouts: &[&tile_layout], push_constant_ranges: &[] });
		// one u32 a tile, an instance each
		let tile_buffer = wgpu::VertexBufferLayout { array_stride: 4, step_mode: wgpu::VertexStepMode::Instance, attributes: &[wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 0, shader_location: 0 }] };
		let target = |format: wgpu::TextureFormat| Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL });
		let tile_pipeline = |entry: &str, targets: &[Option<wgpu::ColorTargetState>]| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(&tile_pipeline_layout),
				vertex: wgpu::VertexState { module, entry_point: Some("vs_ao_tile"), buffers: std::slice::from_ref(&tile_buffer), compilation_options: Default::default() },
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState { module, entry_point: Some(entry), targets, compilation_options: Default::default() }),
				multiview: None,
				cache,
			})
		};
		Self {
			changed: compute_pipeline("cs_ao_changed", &changed_layout),
			keep: compute_pipeline("cs_ao_keep", &keep_layout),
			tiles: compute_pipeline("cs_ao_tiles", &tiles_layout),
			blur_list: compute_pipeline("cs_ao_blur_list", &blur_list_layout),
			disk: tile_pipeline("fs_ao", &[target(AO_FORMAT), target(AO_DEPTH_FORMAT)]),
			blurred: tile_pipeline("fs_ao_blurred", &[target(AO_FORMAT)]),
			changed_layout,
			keep_layout,
			tiles_layout,
			tile_layout,
			blur_list_layout,
		}
	}
}

