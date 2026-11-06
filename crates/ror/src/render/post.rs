//! Image-space post effects (shaders/post.wgsl, sun_rays.wgsl; notes/render.md "Post effects"):
//! BlurEffect blurs the shown image, DepthOfFieldEffect the scene's radiance before the bloom,
//! SunRaysEffect adds to it from a quarter of the resolution. Every blur runs on a level boxed down
//! until its sigma is at most [`MAX_SIGMA`] texels and is read back bilinearly, so no pass takes many
//! taps a pixel however large the render (long passes can reset the driver).

use bytemuck::{Pod, Zeroable};

use super::pipelines::{HDR_FORMAT, OUTPUT_FORMAT};
use super::reuse::Lease;
use crate::scene::environment::PostEffect;

/// The most texels a blur's sigma spans on its level: 2 · ceil(3.5 · 4) + 1 = 29 taps a pass.
const MAX_SIGMA: f32 = 4.0;

/// Studio's capture viewport height, where the effects' sizes were measured.
const REFERENCE_HEIGHT: f32 = 1116.0;

/// BlurEffect: a Gaussian of the shown values (post-blur: 0.5-1.8 of 255 mean error over the line,
/// square and checker, 3-7 in radiance) whose sigma follows Size: (Size, sigma px at 1116 px).
const BLUR_SIGMA: [(f32, f32); 8] = [(0.0, 0.0), (2.0, 0.45), (4.0, 0.64), (8.0, 2.45), (16.0, 5.98), (24.0, 8.45), (40.0, 13.73), (56.0, 19.38)];

/// DepthOfFieldEffect's largest blur and its middle level (sigma px at 1116 px; post-depth: the far
/// checkers stop growing at 6.1-6.6 px).
const DOF_SIGMA: f32 = 6.6;
const DOF_MID_SIGMA: f32 = 3.0;

/// SunRaysEffect in open sky: what it adds per unit Intensity by the angle from the sun
/// ([`SUN_RAYS_ANGLES`] degrees), relative to Spread 1 at 6 degrees, per Spread (post-sunrays-sky,
/// post-sunrays-time: the sun hidden or shown, over black, white and the default skies, at 25, 50
/// and 90 degree views alike; tools/analysis/post_fit.py). Past 40 degrees (61 at Spread 1) a power
/// law of the last measured ten. Several effects: their Intensities add, the strongest one's Spread.
const SUN_RAYS_ANGLES: [f32; 22] = [0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 25.0, 30.0, 35.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 120.0, 180.0];
const SUN_RAYS_PROFILE: [(f32, [f32; 22]); 8] = [
	(0.0, [2.045, 2.045, 2.072, 2.085, 1.666, 1.162, 0.735, 0.4643, 0.31, 0.217, 0.1579, 0.07991, 0.04542, 0.02832, 0.02118, 0.01108, 0.006528, 0.004173, 0.002832, 0.002012, 0.0008728, 0.000269]),
	(0.05, [5.608, 5.608, 5.882, 6.017, 4.731, 3.287, 2.082, 1.339, 0.9054, 0.6424, 0.4687, 0.2474, 0.1416, 0.1059, 0.07039, 0.03786, 0.02281, 0.01486, 0.01026, 0.007393, 0.003324, 0.001077]),
	(0.1, [10.22, 10.22, 10.15, 9.476, 7.263, 4.892, 3.094, 2.038, 1.4, 0.9976, 0.7297, 0.3712, 0.208, 0.1272, 0.08382, 0.041, 0.02286, 0.01395, 0.009093, 0.006235, 0.00248, 0.0006764]),
	(0.15, [13.69, 13.69, 13.82, 12.25, 8.574, 5.556, 3.673, 2.534, 1.797, 1.319, 0.9868, 0.5267, 0.3059, 0.1946, 0.1416, 0.07441, 0.04399, 0.02821, 0.01919, 0.01367, 0.005963, 0.001853]),
	(0.25, [12.91, 12.91, 12.72, 9.647, 7.31, 5.324, 3.927, 2.915, 2.193, 1.672, 1.297, 0.7292, 0.4335, 0.2749, 0.1886, 0.09724, 0.05661, 0.03583, 0.0241, 0.01699, 0.007236, 0.002172]),
	(0.5, [4.291, 4.291, 4.119, 3.838, 3.429, 3.003, 2.596, 2.234, 1.904, 1.62, 1.376, 0.9256, 0.6297, 0.4371, 0.3171, 0.1835, 0.1174, 0.08046, 0.058, 0.04346, 0.02147, 0.007949]),
	(0.75, [1.84, 1.84, 1.863, 1.824, 1.717, 1.615, 1.5, 1.388, 1.274, 1.163, 1.056, 0.8184, 0.6275, 0.4792, 0.3724, 0.246, 0.1753, 0.1317, 0.1027, 0.08253, 0.04836, 0.02276]),
	(1.0, [1.029, 1.024, 1.013, 1.012, 0.9848, 0.9555, 0.9183, 0.8777, 0.8362, 0.7923, 0.7466, 0.6371, 0.5339, 0.4414, 0.3639, 0.2359, 0.1529, 0.1067, 0.07738, 0.0583, 0.02919, 0.01101]),
];

/// SunRaysEffect's colour (radiance per unit Intensity at the profile's 1) follows the sun's elevation
/// alone (post-sunrays-clock: the same at 16:00 40 degrees south as at 17:00 on the equator; nothing
/// from Brightness, ColorShift_Top, the Atmosphere, the sky or the sun's texture and size): blue-grey
/// above 23.5 degrees, orange below 14.3, between the two linear in √ space; from 5.75 degrees down to
/// the horizon it fades linearly to nothing; no rays from the moon.
const SUN_RAYS_DAY: [f32; 3] = [6.17, 7.05, 10.78];
const SUN_RAYS_LOW: [f32; 3] = [11.01, 4.25, 0.0];
const SUN_RAYS_LOW_BELOW: f32 = 14.3;
const SUN_RAYS_DAY_ABOVE: f32 = 23.5;
const SUN_RAYS_FADE_BELOW: f32 = 5.75;

/// A sun away from the view's centre gives weaker rays, exp(-0.09 r²) with r its distance from the
/// centre in half heights (post-sunrays-time edge-in, edge-out, post-sunrays-sky beside: 0.88, 0.71
/// and 0.55 of them near the sun at r 1.18, 1.93 and 2.57; across the view they vary by ±15 % more,
/// stronger away from the sun when it is in view, not drawn).
const SUN_RAYS_OFF_CENTRE: f32 = 0.09;

#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct PostUniform {
	blur: [f32; 4],
	focus: [f32; 4],
	lens: [f32; 4],
	texels: [f32; 4],
}

/// shaders/sun_rays.wgsl's uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SunRaysUniform {
	rays: [[f32; 4]; 4],
	sun: [f32; 4],
	screen: [f32; 4],
	color: [f32; 4],
	angles: [[f32; 4]; 6],
	profile: [[f32; 4]; 6],
}

pub struct PostPipelines {
	layout: wgpu::BindGroupLayout,
	down: wgpu::RenderPipeline,
	blur: wgpu::RenderPipeline,
	radiance: wgpu::RenderPipeline,
	dof: wgpu::RenderPipeline,
	/// a premultiplied layer (the level binding) over the source
	over: wgpu::RenderPipeline,
	present: wgpu::RenderPipeline,
	sun_rays: wgpu::RenderPipeline,
	sampler: wgpu::Sampler,
}

impl PostPipelines {
	pub fn new(device: &wgpu::Device, common: &str, fullscreen: &str, cache: Option<&wgpu::PipelineCache>) -> Self {
		let float = wgpu::TextureSampleType::Float { filterable: true };
		let texture = |binding: u32, sample_type: wgpu::TextureSampleType| wgpu::BindGroupLayoutEntry {
			binding,
			visibility: wgpu::ShaderStages::FRAGMENT,
			ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
			count: None,
		};
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("post"),
			entries: &[
				texture(0, float),
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
					count: None,
				},
				wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
				texture(3, float),
				texture(4, float),
				texture(5, wgpu::TextureSampleType::Depth),
				texture(6, float),
			],
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("post"),
			source: wgpu::ShaderSource::Wgsl(format!("{common}\n{fullscreen}\n{}", include_str!("../../shaders/post.wgsl")).into()),
		});
		let rays_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("sun rays"),
			source: wgpu::ShaderSource::Wgsl(format!("{fullscreen}\n{}", include_str!("../../shaders/sun_rays.wgsl")).into()),
		});
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("post"), bind_group_layouts: &[&layout], push_constant_ranges: &[] });
		let pipeline_in = |module: &wgpu::ShaderModule, entry: &str, format: wgpu::TextureFormat| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(entry),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState { module, entry_point: Some("vs_fullscreen"), buffers: &[], compilation_options: Default::default() },
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let pipeline = |entry: &str, format: wgpu::TextureFormat| pipeline_in(&module, entry, format);
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("post"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});
		Self {
			down: pipeline("fs_post_down", HDR_FORMAT),
			blur: pipeline("fs_post_blur", HDR_FORMAT),
			radiance: pipeline("fs_post_radiance", HDR_FORMAT),
			dof: pipeline("fs_post_dof", HDR_FORMAT),
			over: pipeline("fs_post_over", HDR_FORMAT),
			present: pipeline("fs_post_present", OUTPUT_FORMAT),
			sun_rays: pipeline_in(&rays_module, "fs_sun_rays", HDR_FORMAT),
			layout,
			sampler,
		}
	}
}

/// The scene's SunRaysEffects as one: (Intensity, Spread), None without any.
pub fn sun_rays(effects: &[PostEffect]) -> Option<(f32, f32)> {
	let mut intensity = 0.0f32;
	let mut strongest = (0.0f32, 1.0f32);
	for effect in effects {
		if let PostEffect::SunRays { intensity: own, spread } = effect {
			intensity += own.max(0.0);
			if *own > strongest.0 {
				strongest = (*own, *spread);
			}
		}
	}
	(intensity > 0.0).then_some((intensity, strongest.1.clamp(0.0, 1.0)))
}

/// The open-sky profile at `spread`, between the measured Spreads.
fn sun_rays_profile(spread: f32) -> [f32; 22] {
	let mut profile = SUN_RAYS_PROFILE[SUN_RAYS_PROFILE.len() - 1].1;
	for pair in SUN_RAYS_PROFILE.windows(2) {
		let ((s0, p0), (s1, p1)) = (pair[0], pair[1]);
		if spread <= s1 {
			let t = ((spread - s0) / (s1 - s0)).clamp(0.0, 1.0);
			profile = std::array::from_fn(|index| p0[index] + (p1[index] - p0[index]) * t);
			break;
		}
	}
	profile
}

/// The rays' colour per unit Intensity with the sun `elevation` degrees up.
fn sun_rays_color(elevation: f32) -> glam::Vec3 {
	if elevation <= 0.0 {
		return glam::Vec3::ZERO;
	}
	let day = glam::Vec3::from(SUN_RAYS_DAY).map(f32::sqrt);
	let low = glam::Vec3::from(SUN_RAYS_LOW).map(f32::sqrt);
	let shown = low.lerp(day, ((elevation - SUN_RAYS_LOW_BELOW) / (SUN_RAYS_DAY_ABOVE - SUN_RAYS_LOW_BELOW)).clamp(0.0, 1.0));
	shown * shown * (elevation / SUN_RAYS_FADE_BELOW).min(1.0)
}

/// SunRaysEffect's uniform for a view, None when it adds nothing (the sun down or behind the camera).
/// `view_proj` is the camera's, `rays` its [`crate::render::camera::Camera::ray_unprojection`],
/// `aspect` width / height.
pub fn sun_rays_uniform((intensity, spread): (f32, f32), sun: glam::Vec3, view_proj: glam::Mat4, rays: glam::Mat4, aspect: f32) -> Option<SunRaysUniform> {
	let color = sun_rays_color(sun.y.clamp(-1.0, 1.0).asin().to_degrees()) * intensity;
	let clip = view_proj * sun.extend(0.0);
	if color.max_element() <= 0.0 || clip.w <= 1e-6 {
		return None;
	}
	let ndc = glam::Vec2::new(clip.x, clip.y) / clip.w;
	// how far from the view's centre the sun is, in half heights
	let fade = (-SUN_RAYS_OFF_CENTRE * glam::Vec2::new(ndc.x * aspect, ndc.y).length_squared()).exp();
	let profile = sun_rays_profile(spread);
	// 22 values in 6 vec4s, the last two repeating the last
	let rows = |values: &[f32; 22]| std::array::from_fn(|row| std::array::from_fn(|column| values[(row * 4 + column).min(21)]));
	Some(SunRaysUniform {
		rays: rays.to_cols_array_2d(),
		sun: sun.extend(0.0).to_array(),
		screen: [ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5, fade, 0.0],
		color: color.extend(1.0).to_array(),
		angles: rows(&SUN_RAYS_ANGLES),
		profile: rows(&profile),
	})
}

/// The BlurEffect's sigma in pixels at `height` (the largest Size of the scene's BlurEffects: two of
/// Size 8 blur as one, post-blur "two"), None without one.
pub fn blur_sigma(effects: &[PostEffect], height: u32) -> Option<f32> {
	let size = effects.iter().filter_map(|effect| if let PostEffect::Blur { size } = effect { Some(*size) } else { None }).reduce(f32::max)?;
	let mut sigma = BLUR_SIGMA[BLUR_SIGMA.len() - 1].1 + (size - BLUR_SIGMA[BLUR_SIGMA.len() - 1].0) * 0.353;
	for pair in BLUR_SIGMA.windows(2) {
		let ((s0, g0), (s1, g1)) = (pair[0], pair[1]);
		if size <= s1 {
			sigma = g0 + (g1 - g0) * ((size.max(0.0) - s0) / (s1 - s0));
			break;
		}
	}
	(sigma > 0.05).then_some(sigma * height as f32 / REFERENCE_HEIGHT)
}

/// The steps what blends takes its far blur in (render::mod: one layer each that a frame uses).
pub const FAR_STEPS: u32 = 6;

/// The far blur at a view depth as a share of the largest, as post.wgsl's dof_sigmas has it for the
/// opaque scene (focus: FocusDistance, InFocusRadius, FarIntensity, NearIntensity).
pub fn far_blur_share(focus: [f32; 4], depth: f32) -> f32 {
	let c = ((depth - (focus[0] + focus[1])) / depth.max(1e-3)).clamp(0.0, 1.0);
	let t = ((c - 0.08) / 0.92).clamp(0.0, 1.0);
	t * t * (3.0 - 2.0 * t) * focus[2].sqrt()
}

/// The scene's DepthOfFieldEffect (the first one).
pub fn depth_of_field(effects: &[PostEffect]) -> Option<[f32; 4]> {
	effects.iter().find_map(|effect| match effect {
		PostEffect::DepthOfField { far_intensity, focus_distance, in_focus_radius, near_intensity } => Some([*focus_distance, in_focus_radius.max(0.0), far_intensity.clamp(0.0, 1.0), near_intensity.clamp(0.0, 1.0)]),
		_ => None,
	})
}

/// Render targets and passes of the post effects, encoded in order.
pub struct PostPasses<'a> {
	pipelines: &'a PostPipelines,
	views: Vec<wgpu::TextureView>,
	passes: Vec<(wgpu::BindGroup, &'a wgpu::RenderPipeline, usize)>,
}

impl<'a> PostPasses<'a> {
	pub fn new(pipelines: &'a PostPipelines) -> Self {
		Self { pipelines, views: Vec::new(), passes: Vec::new() }
	}

	fn target(&mut self, lease: &Lease, label: &str, (width, height): (u32, u32), format: wgpu::TextureFormat) -> usize {
		let view = lease.scratch_view(&wgpu::TextureDescriptor {
			label: Some(label),
			size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		});
		self.views.push(view);
		self.views.len() - 1
	}

	pub fn view(&self, index: usize) -> &wgpu::TextureView {
		&self.views[index]
	}

	#[allow(clippy::too_many_arguments)]
	fn pass(&mut self, lease: &Lease, pipeline: &'a wgpu::RenderPipeline, source: &wgpu::TextureView, uniform: PostUniform, mid: &wgpu::TextureView, wide: &wgpu::TextureView, depth: &wgpu::TextureView, target: usize) {
		self.pass_near(lease, pipeline, source, uniform, [mid, wide, mid], depth, target);
	}

	/// [`Self::pass`] with the near level too: `levels` = middle, widest, near.
	#[allow(clippy::too_many_arguments)]
	fn pass_near(&mut self, lease: &Lease, pipeline: &'a wgpu::RenderPipeline, source: &wgpu::TextureView, uniform: PostUniform, levels: [&wgpu::TextureView; 3], depth: &wgpu::TextureView, target: usize) {
		let [mid, wide, near] = levels;
		let buffer = lease.buffer("post params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&uniform));
		let group = lease.device().create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("post"),
			layout: &self.pipelines.layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
				wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.pipelines.sampler) },
				wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(mid) },
				wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(wide) },
				wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(depth) },
				wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(near) },
			],
		});
		self.passes.push((group, pipeline, target));
	}

	/// A Gaussian of `sigma` texels of `source` (`size` texels), boxed down while sigma exceeds
	/// MAX_SIGMA; returns the blurred level and its texel size in source texels.
	#[allow(clippy::too_many_arguments)]
	fn gaussian(&mut self, lease: &Lease, source: &wgpu::TextureView, size: (u32, u32), sigma: f32, filler: &wgpu::TextureView, depth: &wgpu::TextureView, label: &str) -> (usize, f32) {
		let mut level = source.clone();
		let mut level_size = size;
		let mut texels = 1.0f32;
		while sigma / texels > MAX_SIGMA && level_size.0.min(level_size.1) > 8 {
			let next = (level_size.0.div_ceil(2), level_size.1.div_ceil(2));
			let target = self.target(lease, label, next, HDR_FORMAT);
			self.pass(lease, &self.pipelines.down, &level, PostUniform::default(), filler, filler, depth, target);
			level = self.views[target].clone();
			level_size = next;
			texels *= 2.0;
		}
		// the boxes and the bilinear read back of a coarser level blur already: their variance comes off
		let upsampling = if texels > 1.0 { texels * texels / 6.0 } else { 0.0 };
		let remaining = ((sigma * sigma - (texels * texels - 1.0) / 12.0 - upsampling).max(0.0)).sqrt() / texels;
		let uniform = |direction: f32| PostUniform { blur: [remaining.max(0.3), direction, (remaining * 3.5).ceil(), 0.0], ..Default::default() };
		let horizontal = self.target(lease, label, level_size, HDR_FORMAT);
		self.pass(lease, &self.pipelines.blur, &level, uniform(0.0), filler, filler, depth, horizontal);
		let vertical = self.target(lease, label, level_size, HDR_FORMAT);
		let input = self.views[horizontal].clone();
		self.pass(lease, &self.pipelines.blur, &input, uniform(1.0), filler, filler, depth, vertical);
		(vertical, texels)
	}

	/// BlurEffect: `shown` (the resolved image, `size`) blurred by `sigma` px into the frame's output
	/// ([`OUTPUT`] at encoding).
	pub fn blur(&mut self, lease: &Lease, shown: &wgpu::TextureView, size: (u32, u32), sigma: f32, filler: &wgpu::TextureView, depth: &wgpu::TextureView) {
		let (blurred, texels) = self.gaussian(lease, shown, size, sigma, filler, depth, "blur level");
		let input = self.views[blurred].clone();
		self.pass(lease, &self.pipelines.present, &input, PostUniform { texels: [texels, 0.0, 0.0, 0.0], ..Default::default() }, filler, filler, depth, OUTPUT);
	}

	/// The field alone: the scene target `color` (premultiplied √x, `size` render pixels) blurred by its
	/// `depth`, into a new target of the same kind; returns its index.
	#[allow(clippy::too_many_arguments)]
	fn field(&mut self, lease: &Lease, color: &wgpu::TextureView, depth: &wgpu::TextureView, size: (u32, u32), focus: [f32; 4], near_plane: f32, filler: &wgpu::TextureView) -> usize {
		let scale = size.1 as f32 / REFERENCE_HEIGHT;
		let factor = (scale.floor() as u32).clamp(1, 8);
		let working = (size.0.div_ceil(factor), size.1.div_ceil(factor));
		let lens = [near_plane, DOF_SIGMA * scale, DOF_MID_SIGMA * scale, DOF_SIGMA * scale];
		// the layers: each pixel's membership of the middle, widest and near layer with its radiance (the
		// middle one's plain, the others' times the membership)
		let layer = |which: f32| PostUniform { blur: [0.0, 0.0, 0.0, which], focus, lens, texels: [0.0, 0.0, factor as f32, 0.0] };
		let mut layers = Vec::new();
		for (which, label) in [(0.0, "dof middle"), (1.0, "dof widest"), (2.0, "dof near")] {
			let target = self.target(lease, label, working, HDR_FORMAT);
			self.pass(lease, &self.pipelines.radiance, color, layer(which), filler, filler, depth, target);
			layers.push(self.views[target].clone());
		}
		let (mid, mid_texels) = self.gaussian(lease, &layers[0], working, DOF_MID_SIGMA * scale / factor as f32, filler, depth, "dof level");
		let (wide, wide_texels) = self.gaussian(lease, &layers[1], working, DOF_SIGMA * scale / factor as f32, filler, depth, "dof level");
		let (near, near_texels) = self.gaussian(lease, &layers[2], working, DOF_SIGMA * scale / factor as f32, filler, depth, "dof level");
		let output = self.target(lease, "dof", size, HDR_FORMAT);
		let uniform = PostUniform { focus, lens, texels: [mid_texels * factor as f32, wide_texels * factor as f32, 0.0, near_texels * factor as f32], ..Default::default() };
		let (mid, wide, near) = (self.views[mid].clone(), self.views[wide].clone(), self.views[near].clone());
		self.pass_near(lease, &self.pipelines.dof, color, uniform, [&mid, &wide, &near], depth, output);
		output
	}

	/// DepthOfFieldEffect on `color` (the scene's premultiplied √x, opaque) by the opaque scene's
	/// `depth`, into a new target of the same kind; `late` (what blends: layers of the same kind over
	/// transparent black, each with its far blur as a share of the largest, farthest first) is blurred
	/// by that share and laid over the field's far image in turn, the field's near blur taken after over
	/// all of it; returns its index.
	#[allow(clippy::too_many_arguments)]
	pub fn depth_of_field(&mut self, lease: &Lease, color: &wgpu::TextureView, depth: &wgpu::TextureView, size: (u32, u32), focus: [f32; 4], near_plane: f32, filler: &wgpu::TextureView, late: &[(f32, &wgpu::TextureView)]) -> usize {
		if late.is_empty() {
			return self.field(lease, color, depth, size, focus, near_plane, filler);
		}
		let scale = size.1 as f32 / REFERENCE_HEIGHT;
		let mut current = self.field(lease, color, depth, size, [focus[0], focus[1], focus[2], 0.0], near_plane, filler);
		for (share, layer) in late {
			let (level, texels) = if *share > 0.0 {
				let (index, texels) = self.gaussian(lease, layer, size, share * DOF_SIGMA * scale, filler, depth, "late level");
				(self.views[index].clone(), texels)
			} else {
				((*layer).clone(), 1.0)
			};
			let below = self.views[current].clone();
			let over = self.target(lease, "dof and what blends", size, HDR_FORMAT);
			self.pass(lease, &self.pipelines.over, &below, PostUniform { texels: [texels, 0.0, 0.0, 0.0], ..Default::default() }, &level, filler, depth, over);
			current = over;
		}
		if focus[3] > 0.0 {
			let combined = self.views[current].clone();
			current = self.field(lease, &combined, depth, size, [focus[0], focus[1], 0.0, focus[3]], near_plane, filler);
		}
		current
	}

	/// SunRaysEffect: what the rays add to the scene's radiance, at a quarter of the view's `size`
	/// (`depth`: the opaque parts', single-sampled); returns its index.
	pub fn sun_rays(&mut self, lease: &Lease, uniform: &SunRaysUniform, depth: &wgpu::TextureView, size: (u32, u32), filler: &wgpu::TextureView) -> usize {
		let target = self.target(lease, "sun rays", (size.0.div_ceil(4), size.1.div_ceil(4)), HDR_FORMAT);
		let buffer = lease.buffer("sun rays params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(uniform));
		let group = lease.device().create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("sun rays"),
			layout: &self.pipelines.layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(filler) },
				wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.pipelines.sampler) },
				wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(filler) },
				wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(filler) },
				wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(depth) },
				wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(filler) },
			],
		});
		self.passes.push((group, &self.pipelines.sun_rays, target));
		target
	}

	/// Encodes every pass in order.
	pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, output: Option<&wgpu::TextureView>, profile: &super::profile::Profile) {
		for (group, pipeline, target) in &self.passes {
			let view = if *target == usize::MAX { output.expect("post output") } else { &self.views[*target] };
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("post"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("post"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(pipeline);
			pass.set_bind_group(0, group, &[]);
			pass.draw(0..3, 0..1);
		}
	}
}

/// The target index meaning "the frame's output" in [`PostPasses::blur`].
pub const OUTPUT: usize = usize::MAX;

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use glam::{Affine3A, Vec3};
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	use super::super::camera::CameraSpec;
	use super::super::{Renderer, ViewRequest};
	use crate::scene::build::{ModelBuilder, PartSpec};
	use crate::scene::environment::PostEffect;

	#[test]
	fn far_blur_share_follows_the_field() {
		let cutscene = [85.0, 30.0, 0.79, 0.0];
		assert_eq!(super::far_blur_share(cutscene, 100.0), 0.0, "in focus");
		assert!(super::far_blur_share(cutscene, 160.0) < 0.15, "just past the field's far edge");
		let default = [0.05, 10.0, 0.75, 0.75];
		assert!((super::far_blur_share(default, 1e6) - 0.75f32.sqrt()).abs() < 1e-3, "the largest, times √FarIntensity");
		assert!(super::far_blur_share(default, 50.0) > super::far_blur_share(default, 20.0));
	}

	/// A white Beam 60 studs out before a black backdrop 600 out: Roblox's default field blurs it by its own
	/// depth, a near-only field leaves it sharp (the backdrop behind it is far) (post-depth-blended).
	#[test]
	fn what_blends_takes_the_far_blur_of_its_own_depth() {
		let state = std::env::temp_dir().join(format!("ror-dof-blended-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = ModelBuilder::new("BlendedField");
		model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 600.0)), size: Vec3::new(2000.0, 2000.0, 1.0), color: [0, 0, 0], material: 288, ..Default::default() });
		let anchor = model.part(&PartSpec { name: "Anchor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 60.0)), size: Vec3::splat(0.2), transparency: 1.0, ..Default::default() });
		let half = Vec3::new(0.0, 2.0, 0.0);
		let a0 = model.child(anchor, InstanceBuilder::new("Attachment").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(-half))));
		let a1 = model.child(anchor, InstanceBuilder::new("Attachment").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(half))));
		let white = Color3::new(1.0, 1.0, 1.0);
		model.child(
			anchor,
			InstanceBuilder::new("Beam")
				.with_property("Attachment0", Variant::Ref(a0))
				.with_property("Attachment1", Variant::Ref(a1))
				.with_property("Color", ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, white), ColorSequenceKeypoint::new(1.0, white)] })
				.with_property("Transparency", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 0.0, 0.0)] })
				.with_property("Width0", 4.0f32)
				.with_property("Width1", 4.0f32)
				.with_property("FaceCamera", true)
				.with_property("LightInfluence", 0.0f32)
				.with_property("TextureSpeed", 0.0f32),
		);
		let document = model.into_document();
		// at the height the field was measured at, so its largest blur is 6.6 px
		let (width, height) = (600usize, 1116usize);
		let request = ViewRequest { width: width as u32, height: height as u32, camera: CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 20.0 }, ..Default::default() };
		let draw = |effects: Vec<PostEffect>| {
			let mut scene = crate::scene::extract::extract(&document);
			scene.environment.effects = effects;
			let sky = renderer.prepare_environment(&scene);
			let (mut views, mut stats) = renderer.submit_frame(&scene, std::slice::from_ref(&request), sky, None, Default::default(), None, None).unwrap();
			views.pop().unwrap().finish(&mut stats).unwrap().rgba
		};
		// the pixels of the middle row on the beam's edges, between dark and the beam's own light
		let spill = |rgba: &[u8]| {
			let row = height / 2 * width * 4;
			(0..width).map(|x| rgba[row + x * 4]).filter(|value| (16..200).contains(value)).count()
		};
		let field = |far: f32, focus: f32, radius: f32, near: f32| vec![PostEffect::DepthOfField { far_intensity: far, focus_distance: focus, in_focus_radius: radius, near_intensity: near }];
		let sharp = spill(&draw(Vec::new()));
		let default = spill(&draw(field(0.75, 0.05, 10.0, 0.75)));
		let near = spill(&draw(field(0.0, 300.0, 10.0, 1.0)));
		assert!(default >= sharp + 12, "the default field blurs the beam's edges: {default} ramp pixels against {sharp}");
		assert!(near.abs_diff(sharp) <= 1, "a near-only field leaves it sharp before the far backdrop: {near} ramp pixels against {sharp}");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
