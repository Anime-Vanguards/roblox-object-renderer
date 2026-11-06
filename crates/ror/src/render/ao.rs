//! Studio's screen-space ambient occlusion (notes/render.md "Ambient occlusion"; experiments
//! ao-distance, ao-gap, ao-scale, ao-corner, ao-fade, ao-depth, ao-dolly, ao-size, ao-layers,
//! ao-relief, ao-thin, ao-sees): the opaque parts' depths and face normals at the resolution
//! of a 1116 px tall view, taken with 4 samples a pixel and resolved to the farthest covered one, the
//! occlusion of each pixel by what its 16 disk samples see in front of it, held a depth tolerance below
//! the pixel's plane and faded with the view depth, then a blur among neighbours near its view depth;
//! the forward pass multiplies the flat ambient and the sky's diffuse light by it.

use bytemuck::{Pod, Zeroable};
use parking_lot::Mutex;
use wgpu::util::DeviceExt;

use super::camera::Camera;
use super::pipelines::{AO_BLURRED_FORMAT, AO_DEPTH_FORMAT, AO_FORMAT, AO_NORMAL_FORMAT, AO_SURFACE_FORMAT, AO_SAMPLES, DEPTH_FORMAT, Pipelines};
use super::reuse::Lease;

/// The disk's radius at a 1116 px tall view (it scales with the view's height: the same in pixels at
/// every distance and field of view, ao-distance from 12.5 to 100 studs, ao-depth through 88 to 3.3
/// degrees, ao-relief's strips from 15 to 80 studs).
const RADIUS: f32 = 34.3;
const REFERENCE_HEIGHT: f32 = 1116.0;
/// The AO runs at about this many pixels tall (an integer fraction of the render), whatever the
/// render's size: every pixel of the capture's view (ao-thin: Studio's lines along slots a fifth of
/// a pixel wide are continuous where a half-resolution prepass drew them in dashes on its own grid).
const WORKING_HEIGHT: f32 = 1116.0;
/// A = max(0, 1 - SIGMA mean(max(0, v^.n - BIAS) (1 - |P - C|^2 / RANGE^2)^2))^CONTRAST, v = P - n t - C
/// in studs, t the depth tolerance (fitted by rendering the ao-* experiments and two models,
/// tools/analysis/ao_fit.py: AO factor rms over 58 runs 0.0537 with the half-resolution model, 0.0440
/// with this one).
const SIGMA: f32 = 1.75;
const CONTRAST: f32 = 1.02;
const BIAS: f32 = 0.018;
const RANGE: f32 = 5.95;
/// The depth tolerance: a sample counts as t = TOLERANCE 50^(2 - POWER) depth^POWER studs lower than
/// it is over the pixel's plane (depth: the pixel's view depth; 0.19 studs at 50, 0.46 at 80, 0.69 at
/// 100), so a crease's occlusion depends on depth / tan(fov / 2): Studio's crease is as dark at
/// 0.75-12 studs through 40 degrees as at 3 studs through 88 (ao-size, ao-depth), lighter at 50 studs
/// through 40 (ao-dolly) and gone at 50 studs through 6.7 (ao-depth), where the disk spans 0.2 studs; a
/// bar 1 stud off a wall stays dark at 100 studs through 40 (ao-distance) and not through 3.3
/// (ao-depth); grooves and strips half a stud deep leave no occlusion at 80 studs (ao-relief).
const TOLERANCE: f32 = 7.55e-5;
const POWER: f32 = 1.88;
/// The fade with the view depth (ao-fade, ao-far: the same picture from further away through a
/// narrower field of view): 1 / (1 + e^((depth - FADE_DEPTH) / FADE_WIDTH)).
const FADE_DEPTH: f32 = 127.5;
const FADE_WIDTH: f32 = 4.1;
const SAMPLES: f32 = 16.0;
/// The blur: a (2 BLUR + 1)^2 box among the neighbours whose view depth is within BLUR_TOLERANCE
/// (depth / 50)^BLUR_POWER studs of the pixel's (ao-relief: Studio's occlusion runs 6-9 px onto a strip
/// standing 1-2 studs out at 80 studs, 1-4 px at 40 and none at 15; ao-thin: a slot's darkness spreads
/// 6-10 px onto the plate around it at 80 studs).
const BLUR: f32 = 4.0;
const BLUR_TOLERANCE: f32 = 3.38;
const BLUR_POWER: f32 = 2.0;
/// The widest blur the compute pass takes (ao.wgsl `BLUR_BORDER`); a wider one (`ROR_TUNE_AO` blur) or
/// the plane's measure (blur_depth=0) runs `fs_ao_blur`, as does `ROR_AO_BLUR=fragment`.
const TILE_BLUR_MAX: f32 = 4.0;
/// The compute blur's workgroup size (ao.wgsl `BLUR_TILE`)
const BLUR_TILE: u32 = 16;
/// The tiles a clip's kept occlusion is compared and made again in (ao.wgsl `HISTORY_TILE`)
const HISTORY_TILE: u32 = 16;
/// Where the disk's list of tiles starts in a kept occlusion's lists (ao.wgsl `DISK_LIST`, words)
const DISK_LIST: u64 = 64;

/// The model's parameters: `ROR_TUNE_AO="radius=34,depth=6e-5,..."` (keys radius, sigma, contrast,
/// bias, range, depth, power, fade, fade_width, blur, blur_plane, blur_power, blur_depth, samples)
/// overrides them; tools/analysis/ao_fit.py fits through it.
#[derive(Clone, Copy)]
struct Tune {
	radius: f32,
	sigma: f32,
	contrast: f32,
	bias: f32,
	range: f32,
	tolerance: f32,
	fade_depth: f32,
	fade_width: f32,
	/// the depth tolerance's power of the view depth (its value at 50 studs kept)
	power: f32,
	/// the blur's half size in AO pixels and its tolerance (studs at 50 studs), growing as this power
	/// of the view depth, measured along the view depth (1) or off the pixel's plane (0)
	blur: f32,
	blur_plane: f32,
	blur_power: f32,
	blur_depth: f32,
	/// the prepass's samples per pixel: AO_SAMPLES resolved to the farthest, or 1
	samples: f32,
}

fn tune() -> Tune {
	static TUNE: std::sync::OnceLock<Tune> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| {
		let mut tune = Tune {
			radius: RADIUS,
			sigma: SIGMA,
			contrast: CONTRAST,
			bias: BIAS,
			range: RANGE,
			tolerance: TOLERANCE,
			fade_depth: FADE_DEPTH,
			fade_width: FADE_WIDTH,
			power: POWER,
			blur: BLUR,
			blur_plane: BLUR_TOLERANCE,
			blur_power: BLUR_POWER,
			blur_depth: 1.0,
			samples: AO_SAMPLES as f32,
		};
		for pair in std::env::var("ROR_TUNE_AO").unwrap_or_default().split(',') {
			let Some((key, value)) = pair.split_once('=') else {
				continue;
			};
			let Ok(value) = value.trim().parse::<f32>() else {
				continue;
			};
			match key.trim() {
				"radius" => tune.radius = value,
				"sigma" => tune.sigma = value,
				"contrast" => tune.contrast = value,
				"bias" => tune.bias = value,
				"range" => tune.range = value,
				"depth" => tune.tolerance = value,
				"fade" => tune.fade_depth = value,
				"fade_width" => tune.fade_width = value,
				"power" => tune.power = value,
				"blur" => tune.blur = value,
				"blur_plane" => tune.blur_plane = value,
				"blur_power" => tune.blur_power = value,
				"blur_depth" => tune.blur_depth = value,
				"samples" => tune.samples = value,
				_ => {}
			}
		}
		tune
	})
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct AoUniform {
	/// xyz the camera's position, w the depth tolerance's factor
	eye: [f32; 4],
	/// xyz the camera's forward direction, w the depth tolerance's power
	forward: [f32; 4],
	params: [f32; 4],
	extra: [f32; 4],
	/// x the fade's depth, y its width, z the blur tolerance's power, w its measure
	fade: [f32; 4],
	/// the view's rotation and projection inverted (the surfaces' positions from their depths)
	unprojection: [[f32; 4]; 4],
}

/// A clip's occlusion kept from one frame to the next while its camera holds still (video.rs), so that
/// a frame makes again only what changed: the last kept frame's surface depths and normals, its final
/// occlusion and the AO's parameters it was made with (the camera's among them).
///
/// A pixel's occlusion is a function of those parameters, of the surface depths within the disk's
/// reach (its 16 samples, at most `radius` AO pixels off) and its own normal, then of the occlusion and
/// view depths within the blur's: where none of those changed, the kept occlusion is this frame's. A
/// frame made with the same parameters compares its depths and normals with the kept ones bit for bit,
/// in tiles of 16 x 16 AO pixels (ao.wgsl `cs_ao_changed`), and runs the disk on the tiles within the
/// disk's and the blur's reach of a changed one and the blur on those within the blur's, each listed on
/// the GPU (`cs_ao_tiles`) and drawn indirectly by the same shaders as the whole frame's (`fs_ao` on
/// tile quads, the blur's workgroups taking their tile from the list): the same arithmetic on the same
/// values, the rest of the occlusion as the last frame left it. A still camera's clip whose unit moves
/// makes the disk and the blur again round the unit alone.
///
/// Held in memory while a clip runs: the kept depths (R32Float), normals (Rgba16Float) and occlusion
/// (R8), 13 bytes an AO pixel (48 MB at 2560 x 1440), the inputs only once a frame keeps them (the next
/// frame's camera is this one's). `ROR_AO_HISTORY=off` makes every frame whole.
#[derive(Default)]
pub struct AoHistory {
	kept: Mutex<Option<Kept>>,
}

/// What a clip keeps of its last frame's occlusion ([`AoHistory`]).
struct Kept {
	size: (u32, u32),
	/// Tiles across and down
	tiles: (u32, u32),
	/// The parameters the kept occlusion was made with, while the kept inputs are that frame's
	uniform: Option<[u8; std::mem::size_of::<AoUniform>()]>,
	/// That frame's surface depths and normals (made the first time a frame keeps them)
	inputs: Option<(wgpu::Texture, wgpu::Texture)>,
	occlusion: wgpu::Texture,
	/// A word a tile: whether its inputs changed
	changed: wgpu::Buffer,
	/// The indirect arguments, then the disk's tiles, then (from `blur_start`, a word) the blur's
	lists: wgpu::Buffer,
	blur_start: u32,
	/// ao.wgsl `History`: keeping the changed tiles' inputs, and every tile's
	params: wgpu::Buffer,
	params_all: wgpu::Buffer,
}

impl AoHistory {
	pub fn new() -> Self {
		Self::default()
	}

	/// Whether clips keep their occlusion between frames (`ROR_AO_HISTORY=off`: not).
	pub fn enabled() -> bool {
		static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
		*ENABLED.get_or_init(|| std::env::var("ROR_AO_HISTORY").map_or(true, |value| value != "off"))
	}
}

/// A frame's use of its clip's [`AoHistory`]: `keep` when the next frame's camera is this one's (its
/// inputs are worth keeping).
#[derive(Clone, Copy)]
pub struct HistoryUse<'a> {
	pub history: &'a AoHistory,
	pub keep: bool,
}

/// The textures of one view's occlusion.
pub struct AoTargets<'a> {
	surfaces: wgpu::TextureView,
	normals: wgpu::TextureView,
	/// The single-sampled prepass's depth (without `samples`)
	depth: Option<wgpu::TextureView>,
	raw: wgpu::TextureView,
	/// Each pixel's view depth, the occlusion pass's for its blur
	view_depths: wgpu::TextureView,
	/// The blurred occlusion the forward pass reads.
	pub occlusion: wgpu::TextureView,
	/// The compute blur's 32-bit result, when it runs (`cs_ao_blur`)
	blurred: Option<wgpu::TextureView>,
	/// The multisampled prepass (normals, depth), resolved into `surfaces` and `normals`
	samples: Option<(wgpu::TextureView, wgpu::TextureView)>,
	/// That depth's texture ([`AoTargets::depth`])
	samples_depth: Option<wgpu::Texture>,
	/// Every texture but the occlusion's: no pass after [`AoTargets::encode`] reads them
	textures: Vec<wgpu::Texture>,
	/// The targets' size
	size: (u32, u32),
	/// The disk's radius in AO pixels, the view's aspect
	radius: f32,
	aspect: f32,
	/// The clip's kept occlusion, `occlusion` its texture
	history: Option<HistoryUse<'a>>,
}

fn color(view: &wgpu::TextureView) -> wgpu::RenderPassColorAttachment<'_> {
	wgpu::RenderPassColorAttachment {
		view,
		depth_slice: None,
		resolve_target: None,
		ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
	}
}

/// A target of the occlusion's, its texture kept in `textures`. Its uses are the main pass's of the
/// same kind (render::Renderer::render_view), so a view of the AO's size draws its scene into these
/// once the occlusion is encoded ([`AoTargets::release`]): the normals' 4 samples and depth's are the
/// main pass's colour and depth, the resolved normals its single-sampled colour (copy source too).
fn target(lease: &Lease, textures: &mut Vec<wgpu::Texture>, label: &str, width: u32, height: u32, format: wgpu::TextureFormat, samples: u32) -> wgpu::TextureView {
	let copy = if format == AO_NORMAL_FORMAT && samples == 1 { wgpu::TextureUsages::COPY_SRC } else { wgpu::TextureUsages::empty() };
	let texture = lease.scratch(&wgpu::TextureDescriptor {
		label: Some(label),
		size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
		mip_level_count: 1,
		sample_count: samples,
		dimension: wgpu::TextureDimension::D2,
		format,
		usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | copy,
		view_formats: &[],
	});
	let view = texture.create_view(&Default::default());
	textures.push(texture);
	view
}

/// One full-screen triangle into `outputs`
fn fullscreen(encoder: &mut wgpu::CommandEncoder, profile: &super::profile::Profile, label: &str, pipeline: &wgpu::RenderPipeline, bind_group: &wgpu::BindGroup, outputs: &[Option<wgpu::RenderPassColorAttachment<'_>>]) {
	let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
		label: Some(label),
		color_attachments: outputs,
		depth_stencil_attachment: None,
		timestamp_writes: profile.pass(label),
		occlusion_query_set: None,
	});
	pass.set_pipeline(pipeline);
	pass.set_bind_group(0, bind_group, &[]);
	pass.draw(0..3, 0..1);
}

/// Whether the blur runs as the compute pass (`cs_ao_blur`)
fn tile_blur(tune: &Tune) -> bool {
	tune.blur_depth > 0.5 && tune.blur.trunc() <= TILE_BLUR_MAX && std::env::var("ROR_AO_BLUR").map_or(true, |value| value != "fragment")
}

impl<'a> AoTargets<'a> {
	/// The prepass's size and samples for a view of `render_width` x `render_height`: a whole fraction
	/// of it, about [`WORKING_HEIGHT`] tall (or its own size when smaller).
	pub fn size(render_width: u32, render_height: u32) -> ((u32, u32), u32) {
		let working = std::env::var("ROR_AO_HEIGHT").ok().and_then(|value| value.parse::<f32>().ok()).unwrap_or(WORKING_HEIGHT);
		let factor = (render_height as f32 / working).round().max(1.0);
		let width = (render_width as f32 / factor).ceil() as u32;
		let height = (render_height as f32 / factor).ceil() as u32;
		((width, height), if tune().samples > 1.0 { AO_SAMPLES } else { 1 })
	}

	pub fn new(lease: &Lease, render_width: u32, render_height: u32, history: Option<HistoryUse<'a>>) -> Self {
		let working = std::env::var("ROR_AO_HEIGHT").ok().and_then(|value| value.parse::<f32>().ok()).unwrap_or(WORKING_HEIGHT);
		let factor = (render_height as f32 / working).round().max(1.0);
		let ((width, height), _) = Self::size(render_width, render_height);
		let tune = tune();
		let mut textures = Vec::new();
		let samples = (tune.samples > 1.0).then(|| (target(lease, &mut textures, "ao normal samples", width, height, AO_NORMAL_FORMAT, AO_SAMPLES), target(lease, &mut textures, "ao depth samples", width, height, DEPTH_FORMAT, AO_SAMPLES)));
		let samples_depth = samples.as_ref().and_then(|_| textures.last().cloned());
		let surfaces = target(lease, &mut textures, "ao surface depths", width, height, AO_SURFACE_FORMAT, 1);
		let normals = target(lease, &mut textures, "ao normals", width, height, AO_NORMAL_FORMAT, 1);
		let depth = samples.is_none().then(|| target(lease, &mut textures, "ao depth", width, height, DEPTH_FORMAT, 1));
		let raw = target(lease, &mut textures, "ao raw", width, height, AO_FORMAT, 1);
		let view_depths = target(lease, &mut textures, "ao view depths", width, height, AO_DEPTH_FORMAT, 1);
		let stored = |textures: &mut Vec<wgpu::Texture>, label: &str, format: wgpu::TextureFormat| {
			let texture = lease.scratch(&wgpu::TextureDescriptor {
				label: Some(label),
				size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format,
				usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			});
			let view = texture.create_view(&Default::default());
			textures.push(texture);
			view
		};
		let blurred = tile_blur(&tune).then(|| stored(&mut textures, "ao blurred", AO_BLURRED_FORMAT));
		let radius = tune.radius * render_height as f32 / REFERENCE_HEIGHT / factor;
		// read by the main pass: not released; a clip's kept one when it keeps its occlusion (only with
		// the compute blur, which its tiles run)
		let history = history.filter(|_| blurred.is_some() && AoHistory::enabled());
		let occlusion = match history {
			Some(HistoryUse { history, .. }) => history.occlusion(lease.device(), (width, height), radius, &tune),
			None => target(lease, &mut Vec::new(), "ao", width, height, AO_FORMAT, 1),
		};
		Self { surfaces, normals, depth, raw, view_depths, occlusion, blurred, size: (width, height), samples, samples_depth, textures, radius, aspect: render_width as f32 / render_height as f32, history }
	}

	/// The multisampled prepass's depth once [`AoTargets::encode`] drew it: each sample's frontmost
	/// of the opaque draws and of the coverage draws on all 4 samples, as `vs_main` places them. A
	/// main pass given this texture as its depth (a view of the AO's size and samples) loads it
	/// instead of clearing it: its own opaque and coverage draws reach each of those depths with the
	/// same vertex shader, so each sample still ends with the last fragment at its frontmost depth,
	/// and the fragments behind are rejected before they shade (notes/render.md "Ambient occlusion").
	pub fn depth(&self) -> Option<&wgpu::Texture> {
		self.samples_depth.as_ref()
	}

	/// Gives `lease` back every target but the occlusion, for the passes it records after
	/// [`AoTargets::encode`] (`Lease::release`).
	pub fn release(&self, lease: &Lease) {
		for texture in &self.textures {
			lease.release(texture);
		}
	}

	/// The prepass of the opaque draws `(vertex buffer, index buffer, indices, instance, double-sided)`
	/// and of the draws by coverage (the same and their material) through `frame_group`, then the
	/// occlusion of the view from `camera` and its blur.
	#[allow(clippy::too_many_arguments)]
	pub fn encode<'d>(
		&self,
		lease: &Lease,
		encoder: &mut wgpu::CommandEncoder,
		pipelines: &Pipelines,
		frame_group: &wgpu::BindGroup,
		camera: &Camera,
		draws: impl Iterator<Item = (&'d wgpu::Buffer, &'d wgpu::Buffer, std::ops::Range<u32>, u32, bool)> + Clone,
		coverage: impl Iterator<Item = (&'d wgpu::Buffer, &'d wgpu::Buffer, std::ops::Range<u32>, u32, bool, &'d wgpu::BindGroup)> + Clone,
		profile: &super::profile::Profile,
	) {
		let device = lease.device();
		let tune = tune();
		let forward = -camera.world.matrix3.z_axis;
		let uniform = AoUniform {
			eye: [camera.eye.x, camera.eye.y, camera.eye.z, tune.tolerance * 50f32.powf(2.0 - tune.power)],
			forward: [forward.x, forward.y, forward.z, tune.power],
			params: [self.radius, tune.sigma, tune.contrast, tune.bias],
			extra: [tune.range, SAMPLES, tune.blur_plane, tune.blur],
			fade: [tune.fade_depth, tune.fade_width, tune.blur_power, tune.blur_depth],
			unprojection: camera.ray_unprojection(self.aspect).to_cols_array_2d(),
		};
		let buffer = lease.buffer("ao params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&uniform));
		{
			// one sample per pixel: depths and normals; several: normals and depth, resolved below
			let (colors, depth, prepass, prepass_coverage) = match &self.samples {
				Some((normals, depth)) => (vec![Some(color(normals))], depth, &pipelines.ao_prepass_samples, &pipelines.ao_prepass_coverage_samples),
				None => (vec![Some(color(&self.surfaces)), Some(color(&self.normals))], self.depth.as_ref().expect("the single-sampled prepass's depth"), &pipelines.ao_prepass, &pipelines.ao_prepass_coverage),
			};
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("ao prepass"),
				color_attachments: &colors,
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view: depth,
					depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: if self.samples.is_some() { wgpu::StoreOp::Store } else { wgpu::StoreOp::Discard } }),
					stencil_ops: None,
				}),
				timestamp_writes: profile.pass("ao prepass"),
				occlusion_query_set: None,
			});
			pass.set_bind_group(0, frame_group, &[]);
			for (double_sided, pipeline) in [(false, &prepass.0), (true, &prepass.1)] {
				pass.set_pipeline(pipeline);
				let mut runs = super::runs::Runs::new(1);
				for (vertices, indices, range, instance, two_sided) in draws.clone() {
					if two_sided != double_sided {
						continue;
					}
					runs.draw(&mut pass, vertices, indices, range, instance, None);
				}
				runs.finish(&mut pass);
			}
			for (double_sided, pipeline) in [(false, &prepass_coverage.0), (true, &prepass_coverage.1)] {
				pass.set_pipeline(pipeline);
				let mut runs = super::runs::Runs::new(1);
				for (vertices, indices, range, instance, two_sided, material) in coverage.clone() {
					if two_sided != double_sided {
						continue;
					}
					runs.draw(&mut pass, vertices, indices, range, instance, Some(material));
				}
				runs.finish(&mut pass);
			}
		}
		if let Some((normals, depth)) = &self.samples {
			let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao resolve"),
				layout: &pipelines.ao_resolve_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(depth) },
					wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(normals) },
				],
			});
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("ao resolve"),
				color_attachments: &[Some(color(&self.surfaces)), Some(color(&self.normals))],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("ao resolve"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&pipelines.ao_resolve);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		if let Some(use_) = self.history {
			let mut kept = use_.history.kept.lock();
			let kept = kept.as_mut().expect("a kept occlusion (AoHistory::occlusion)");
			let bytes: [u8; std::mem::size_of::<AoUniform>()] = bytemuck::cast(uniform);
			if kept.uniform == Some(bytes) && kept.inputs.is_some() {
				self.encode_changed(device, encoder, pipelines, &buffer, kept, use_.keep, profile);
				kept.uniform = use_.keep.then_some(bytes);
				return;
			}
			kept.uniform = None;
			if use_.keep {
				kept.uniform = Some(bytes);
				// after the whole frame's passes, which the inputs' copies do not change
				self.encode_whole(device, encoder, pipelines, &buffer, profile);
				self.keep_inputs(device, encoder, pipelines, kept, true, profile);
				return;
			}
		}
		self.encode_whole(device, encoder, pipelines, &buffer, profile);
	}

	/// The occlusion and its blur of every pixel.
	fn encode_whole(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, pipelines: &Pipelines, buffer: &wgpu::Buffer, profile: &super::profile::Profile) {
		let group = |occlusion: &wgpu::TextureView, depths: &wgpu::TextureView| {
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao"),
				layout: &pipelines.ao_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.surfaces) },
					wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.normals) },
					wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(occlusion) },
					wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(depths) },
				],
			})
		};
		// the occlusion pass reads neither occlusion nor view depths (it writes the view depths the blur
		// reads): the blurred target stands in for bindings 3 and 7
		fullscreen(encoder, profile, "ao disk", &pipelines.ao, &group(&self.occlusion, &self.occlusion), &[white(&self.raw), white(&self.view_depths)]);
		match &self.blurred {
			Some(blurred) => {
				let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
					label: Some("ao blur tile"),
					layout: &pipelines.ao_blur_tile_layout,
					entries: &[
						wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
						wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.raw) },
						wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&self.view_depths) },
						wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(blurred) },
					],
				});
				{
					let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ao blur"), timestamp_writes: profile.compute_pass("ao blur") });
					pass.set_pipeline(&pipelines.ao_blur_tile);
					pass.set_bind_group(0, &bind_group, &[]);
					pass.dispatch_workgroups(self.size.0.div_ceil(BLUR_TILE), self.size.1.div_ceil(BLUR_TILE), 1);
				}
				fullscreen(encoder, profile, "ao blur copy", &pipelines.ao_blurred, &group(blurred, &self.view_depths), &[white(&self.occlusion)]);
			}
			None => fullscreen(encoder, profile, "ao blur", &pipelines.ao_blur, &group(&self.raw, &self.view_depths), &[white(&self.occlusion)]),
		}
	}

	/// The kept occlusion made again where this frame's inputs changed ([`AoHistory`]); this frame's
	/// inputs kept for the next where they changed, when `keep`.
	#[allow(clippy::too_many_arguments)]
	fn encode_changed(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, pipelines: &Pipelines, buffer: &wgpu::Buffer, kept: &mut Kept, keep: bool, profile: &super::profile::Profile) {
		let history = pipelines.ao_history(device);
		let view = |texture: &wgpu::Texture| texture.create_view(&Default::default());
		let (kept_depths, kept_normals) = kept.inputs.as_ref().expect("kept inputs");
		let (kept_depths_view, kept_normals_view) = (view(kept_depths), view(kept_normals));
		{
			let changed = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao changed"),
				layout: &history.changed_layout,
				entries: &[
					texture(0, &self.surfaces),
					texture(1, &self.normals),
					texture(9, &kept_depths_view),
					texture(10, &kept_normals_view),
					wgpu::BindGroupEntry { binding: 11, resource: kept.changed.as_entire_binding() },
				],
			});
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ao changed"), timestamp_writes: profile.compute_pass("ao changed") });
			pass.set_pipeline(&history.changed);
			pass.set_bind_group(0, &changed, &[]);
			pass.dispatch_workgroups(kept.tiles.0, kept.tiles.1, 1);
		}
		if keep {
			self.keep_inputs(device, encoder, pipelines, kept, false, profile);
		}
		encoder.clear_buffer(&kept.lists, 0, Some(64));
		{
			let tiles = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao tiles"),
				layout: &history.tiles_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 12, resource: kept.params.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 15, resource: kept.changed.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 16, resource: kept.lists.as_entire_binding() },
				],
			});
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ao tiles"), timestamp_writes: profile.compute_pass("ao tiles") });
			pass.set_pipeline(&history.tiles);
			pass.set_bind_group(0, &tiles, &[]);
			pass.dispatch_workgroups((kept.tiles.0 * kept.tiles.1).div_ceil(64), 1, 1);
		}
		let count = (kept.tiles.0 * kept.tiles.1) as u64;
		let tile_group = |occlusion: &wgpu::TextureView, depths: &wgpu::TextureView| {
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao tile"),
				layout: &history.tile_layout,
				entries: &[
					texture(0, &self.surfaces),
					texture(1, &self.normals),
					wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
					texture(3, occlusion),
					texture(7, depths),
					wgpu::BindGroupEntry { binding: 12, resource: kept.params.as_entire_binding() },
				],
			})
		};
		// the disk on its tiles (the rest of its targets is read by no tile the blur makes again)
		{
			let group = tile_group(&self.occlusion, &self.occlusion);
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("ao disk"),
				color_attachments: &[white(&self.raw), white(&self.view_depths)],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("ao disk"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&history.disk);
			pass.set_bind_group(0, &group, &[]);
			pass.set_vertex_buffer(0, kept.lists.slice(DISK_LIST * 4..DISK_LIST * 4 + count * 4));
			pass.draw_indirect(&kept.lists, 0);
		}
		let blurred = self.blurred.as_ref().expect("the compute blur (AoTargets::new)");
		{
			let start = kept.blur_start as u64 * 4;
			let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("ao blur list"),
				layout: &history.blur_list_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
					texture(3, &self.raw),
					texture(7, &self.view_depths),
					texture(8, blurred),
					wgpu::BindGroupEntry { binding: 17, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &kept.lists, offset: start, size: std::num::NonZeroU64::new(count * 4) }) },
				],
			});
			let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ao blur"), timestamp_writes: profile.compute_pass("ao blur") });
			pass.set_pipeline(&history.blur_list);
			pass.set_bind_group(0, &group, &[]);
			pass.dispatch_workgroups_indirect(&kept.lists, 32);
		}
		// the blurred tiles into the kept occlusion, the rest as it was
		{
			let group = tile_group(blurred, &self.view_depths);
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("ao blur copy"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &self.occlusion, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("ao blur copy"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&history.blurred);
			pass.set_bind_group(0, &group, &[]);
			let start = kept.blur_start as u64 * 4;
			pass.set_vertex_buffer(0, kept.lists.slice(start..start + count * 4));
			pass.draw_indirect(&kept.lists, 16);
		}
	}

	/// This frame's depths and normals kept for the next: every tile's (`all`) or the changed ones'.
	fn keep_inputs(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, pipelines: &Pipelines, kept: &mut Kept, all: bool, profile: &super::profile::Profile) {
		let history = pipelines.ao_history(device);
		let (width, height) = kept.size;
		let (depths, normals) = kept.inputs.get_or_insert_with(|| {
			let texture = |label: &str, format: wgpu::TextureFormat| {
				device.create_texture(&wgpu::TextureDescriptor {
					label: Some(label),
					size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
					mip_level_count: 1,
					sample_count: 1,
					dimension: wgpu::TextureDimension::D2,
					format,
					usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
					view_formats: &[],
				})
			};
			(texture("ao kept depths", AO_SURFACE_FORMAT), texture("ao kept normals", AO_NORMAL_FORMAT))
		});
		let (depths_view, normals_view) = (depths.create_view(&Default::default()), normals.create_view(&Default::default()));
		let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("ao keep"),
			layout: &history.keep_layout,
			entries: &[
				texture(0, &self.surfaces),
				texture(1, &self.normals),
				wgpu::BindGroupEntry { binding: 12, resource: if all { kept.params_all.as_entire_binding() } else { kept.params.as_entire_binding() } },
				texture(13, &depths_view),
				texture(14, &normals_view),
				wgpu::BindGroupEntry { binding: 15, resource: kept.changed.as_entire_binding() },
			],
		});
		let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("ao keep"), timestamp_writes: profile.compute_pass("ao keep") });
		pass.set_pipeline(&history.keep);
		pass.set_bind_group(0, &group, &[]);
		pass.dispatch_workgroups(kept.tiles.0, kept.tiles.1, 1);
	}
}

impl AoHistory {
	/// The kept occlusion's texture for AO targets of `size` (made, or made again for another size),
	/// with the tiles' reaches for a disk of `radius` AO pixels.
	fn occlusion(&self, device: &wgpu::Device, size: (u32, u32), radius: f32, tune: &Tune) -> wgpu::TextureView {
		let mut kept = self.kept.lock();
		if kept.as_ref().is_none_or(|kept| kept.size != size) {
			let (width, height) = size;
			let tiles = (width.div_ceil(HISTORY_TILE), height.div_ceil(HISTORY_TILE));
			let count = tiles.0 * tiles.1;
			// a pixel's occlusion reads the depths within the disk's reach (its samples' offsets round to
			// at most ceil(radius)) and the blur's; a tile k tiles off another is at least (k - 1) 16 + 1
			// pixels off it
			let reach = radius.max(0.0).ceil() as u32 + 1 + tune.blur.max(0.0) as u32;
			let blur_reach = (reach - 1) / HISTORY_TILE + 1;
			// the disk's tiles: those the blur reads round its own (its reach under a tile)
			let disk_reach = blur_reach + 1;
			// the blur's list where a storage binding may start (256 bytes), after the disk's
			let blur_start = (DISK_LIST as u32 + count).next_multiple_of(64);
			let params = |all: u32| {
				let words: [u32; 12] = [all, tiles.0, disk_reach, blur_reach, (width as f32).to_bits(), (height as f32).to_bits(), 0, 0, blur_start, count, 0, 0];
				device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("ao history"), contents: bytemuck::cast_slice(&words), usage: wgpu::BufferUsages::UNIFORM })
			};
			let occlusion = device.create_texture(&wgpu::TextureDescriptor {
				label: Some("ao kept"),
				size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: AO_FORMAT,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			});
			let buffer = |label: &str, words: u32, usage: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: words as u64 * 4, usage, mapped_at_creation: false });
			*kept = Some(Kept {
				size,
				tiles,
				uniform: None,
				inputs: None,
				occlusion,
				changed: buffer("ao changed tiles", count, wgpu::BufferUsages::STORAGE),
				lists: buffer("ao tile lists", blur_start + count, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST),
				blur_start,
				params: params(0),
				params_all: params(1),
			});
		}
		kept.as_ref().map(|kept| kept.occlusion.create_view(&Default::default())).expect("made above")
	}
}

/// A texture's entry in a bind group.
fn texture(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
	wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(view) }
}

/// A colour attachment cleared to white.
fn white(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
	Some(wgpu::RenderPassColorAttachment {
		view,
		depth_slice: None,
		resolve_target: None,
		ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store },
	})
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use glam::{Affine3A, Vec3};

	use super::super::camera::CameraSpec;
	use super::super::{Renderer, ViewRequest};

	#[test]
	fn a_clip_s_kept_occlusion_is_its_frames_drawn_whole() {
		let state = std::env::temp_dir().join(format!("ror-ao-history-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		// a floor and a wall, and a block moving along the crease between them, from a still camera
		let scene_at = |x: f32| {
			let mut model = crate::scene::build::ModelBuilder::new("Crease");
			let part = |name: &str, at: Vec3, size: Vec3| crate::scene::build::PartSpec { name: name.into(), cframe: Affine3A::from_translation(at), size, smooth: true, ..Default::default() };
			model.part(&part("Floor", Vec3::new(0.0, -0.5, 0.0), Vec3::new(40.0, 1.0, 40.0)));
			model.part(&part("Wall", Vec3::new(0.0, 5.0, 4.5), Vec3::new(40.0, 10.0, 1.0)));
			model.part(&part("Block", Vec3::new(x, 1.0, 2.0), Vec3::new(2.0, 2.0, 2.0)));
			crate::scene::extract::extract(&model.into_document())
		};
		let request = ViewRequest { width: 320, height: 180, camera: CameraSpec::LookAt { eye: [0.0, 6.0, -14.0], target: [0.0, 1.0, 2.0], fov: 70.0 }, ..Default::default() };
		let history = crate::render::ClipKept::new();
		let frame = |x: f32, kept: Option<&crate::render::ClipKept>| {
			let scene = scene_at(x);
			let sky = renderer.prepare_environment(&scene);
			let (mut views, mut stats) = renderer.submit_frame(&scene, std::slice::from_ref(&request), sky, None, Default::default(), None, kept.map(|history| (history, true))).unwrap();
			views.pop().unwrap().finish(&mut stats).unwrap().rgba
		};
		let mut last = None;
		for x in [-6.0, -6.0, -3.5, 0.0, 0.0, 4.0] {
			let kept = frame(x, Some(&history));
			assert!(kept == frame(x, None), "the kept occlusion's frame at {x} is the one drawn whole");
			if let Some(last) = last.replace(kept.clone())
				&& x != -6.0
				&& x != 0.0
			{
				assert!(last != kept, "the block moved");
			}
		}
		assert!(history.ao.kept.lock().as_ref().is_some_and(|kept| kept.uniform.is_some() && kept.inputs.is_some()), "frames were made from the kept occlusion");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
