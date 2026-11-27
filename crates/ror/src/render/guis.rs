//! SurfaceGuis and BillboardGuis (notes/render.md "GUIs"). rur draws their content (render::rur)
//! into textures cached by content, canvas size and scale, in memory and on disk
//! (`<state dir>/guis/<rur build>-<code>`, render::gui_files; the content store's too, which a state
//! dir lacking one takes it from). A SurfaceGui is a quad on its part's face, a BillboardGui a
//! quad facing the camera at its anchor's depth, sized in whole pixels; both blend with the
//! transparent geometry (`shaders/gui.wgsl`), and AlwaysOnTop ones are a layer laid over the
//! finished image, Highlights included.
//!
//! Sizes in GUI pixels follow Studio's capture viewport: SurfaceGui content is drawn at 1.5 device
//! pixels per canvas pixel (the scale Studio snaps and rasterises it at here), and a BillboardGui's
//! pixel offsets, text and lines scale with the image height, `height / 744` device pixels per GUI
//! pixel (744 GUI pixels is the capture viewport's height at DPI 144, so a 1116 px tall render is
//! Studio's 1.5), unless a view sets `guiScale`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use parking_lot::Mutex;
use rayon::prelude::*;
use wgpu::util::DeviceExt;

use super::camera::Camera;
use super::ids::{Readback, copy_out};
use super::pipelines::{DEPTH_FORMAT, HDR_FORMAT, LIGHTS, OUTPUT_FORMAT, SAMPLE_COUNTS, SHADOW, VOXEL_READ};
use super::gui_files::{GUI_CHUNK, GuiImage};
use super::resources::{GpuTexture, Weighed};
use super::{Prepared, Renderer, ViewRequest, rur};
use crate::scene::Scene;
use crate::scene::guis::{GRADIENT_KEYS, Gui, GuiKind, ScreenArea, ViewportKind};

const COMMON: &str = include_str!("../../shaders/common.wgsl");
const GUI: &str = include_str!("../../shaders/gui.wgsl");

/// Device pixels per canvas pixel of SurfaceGui content: Studio snaps its rects to 1/1.5 canvas pixel
/// and rasterises its glyphs at 1.5 x TextSize (gui-surface-basics: magnified, one-pixel frames
/// come out 0.67 or 1.33 canvas pixels).
pub const SURFACE_SCALE: f32 = 1.5;

/// Device pixels per canvas pixel a SurfaceGui's image is drawn at, from its canvas's corners projected into the view's
/// pixels (top-left, top-right, bottom-left, bottom-right; `None` behind the camera): SURFACE_SCALE, or where the view
/// magnifies the canvas past it, the doubling of it at or past the most a canvas pixel covers on screen, so that its
/// frames keep an edge a pixel wide as a client's do (fixtures/world-guis.luau "sampling" in a Roblox client: a 32 x 24
/// canvas seen at 32 device pixels a canvas pixel draws its frames as geometry, one-pixel frames whole canvas pixels,
/// where the image at 1.5 ramped over 21 pixels). The texture's size limit caps it ([`GuiKey::new`]).
fn surface_scale(canvas: [f32; 2], corners: [Option<glam::Vec2>; 4]) -> f32 {
	let [Some(a), Some(b), Some(c), Some(d)] = corners else {
		return SURFACE_SCALE;
	};
	let across = a.distance(b).max(c.distance(d)) / canvas[0].max(1e-3);
	let down = a.distance(c).max(b.distance(d)) / canvas[1].max(1e-3);
	let shown = across.max(down);
	if shown.is_nan() || shown <= SURFACE_SCALE {
		return SURFACE_SCALE;
	}
	// (at most 2^10 times: a canvas pixel over 1,500 device pixels wide)
	SURFACE_SCALE * 2f32.powf((shown / SURFACE_SCALE).log2().ceil().min(10.0))
}

/// The capture viewport's height in GUI pixels (1116 device pixels at DPI 144).
pub const REFERENCE_HEIGHT: f32 = 744.0;
/// The largest GUI texture side (device pixels, margins included); bigger canvases are drawn at a
/// smaller scale.
const MAX_TEXTURE: f32 = 2048.0;
/// A ScreenGui's: a whole 4K view at its own pixels, as a client draws it (fixtures/screen-guis.luau
/// "fine" at 3840 x 2160 in a Roblox client: one-pixel lines whole; drawn at 2048 and magnified they
/// were 27 % within 2 of 255), within one of rur's atlases (render::rur, 4096 a side).
const MAX_SCREEN_TEXTURE: f32 = 4088.0;
/// Device pixels of GUI content drawn by one rur call at most, and how many calls run at once: rur
/// draws a call's atlas on the GPU in one frame, kept about the size of a screen (a long GPU
/// submission can make the driver reset the device, every other app's included).
const BATCH_PIXELS: u64 = 2048 * 2048;
/// rur calls one resolution of GUIs makes at once (rur's service draws one atlas at a time; the
/// others run their Luau, are written and read meanwhile).
const RUR_WORKERS: usize = 3;
/// rur's revision while its native app has no build ([`rur::revision`]): nothing is cached on disk.
const UNBUILT: &str = "unbuilt";
/// How long a GUI whose content could not be drawn is not tried again.
const FAILURE_TTL: Duration = Duration::from_secs(60);

pub(crate) const RUR_NOT_FOUND_PREFIX: &str = "rur not found";
pub(crate) const ROR_RUR_INVALID_PREFIX: &str = "ROR_RUR points at ";

/// Whether a GUI skip reason is a permanent configuration/environment cause (missing `rur` or an
/// explicitly invalid `ROR_RUR`) that cannot recover on a draw-time retry within the same run.
pub(crate) fn is_unrecoverable_gui_skip(reason: &str) -> bool {
	reason.starts_with(RUR_NOT_FOUND_PREFIX) || reason.starts_with(ROR_RUR_INVALID_PREFIX)
}

/// Emits a `crate::say` warning at most once per run per distinct invalid-`ROR_RUR` cause found in
/// `report.skipped` (avoiding per-frame warning spam in multi-frame clips).
pub(crate) fn warn_invalid_rur_once(warned: &mut std::collections::HashSet<String>, report: &crate::scene::guis::GuiReport) {
	for skip in &report.skipped {
		if skip.reason.starts_with(ROR_RUR_INVALID_PREFIX) && warned.insert(skip.reason.clone()) {
			crate::say(&format!("warning: {}", skip.reason));
		}
	}
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct GuiVertex {
	position: [f32; 3],
	uv: [f32; 2],
	normal: [f32; 3],
	/// x LightInfluence, y Brightness, z and w unused
	params: [f32; 4],
}

impl GuiVertex {
	const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x3, 3 => Float32x4];

	fn layout() -> wgpu::VertexBufferLayout<'static> {
		wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<GuiVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &Self::ATTRIBUTES }
	}
}

/// What a GUI's texture holds: its content, canvas size (1/1024 GUI pixel), scale (1/1024) and
/// whether it clips (else the texture keeps margins for what overflows the canvas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GuiKey {
	content: u64,
	width: u32,
	height: u32,
	scale: u32,
	clips: bool,
}

impl GuiKey {
	fn new(content: u64, canvas: [f32; 2], scale: f32, clips: bool) -> Self {
		Self::within(content, canvas, scale, clips, MAX_TEXTURE)
	}

	/// [`GuiKey::new`] with a texture side of at most `largest` device pixels.
	fn within(content: u64, canvas: [f32; 2], scale: f32, clips: bool, largest: f32) -> Self {
		// a canvas bigger than a texture can hold is drawn at a smaller scale (the margins: a quarter of
		// the canvas and 8 GUI pixels on each side, up to a device pixel more each when rounded up)
		let side = canvas[0].max(canvas[1]);
		let extent = if clips { side } else { side * 1.5 + 16.0 };
		let scale = scale.min((largest - 3.0) / extent.max(1.0));
		let quantize = |value: f32, steps: f32| (value * steps).round().clamp(1.0, u32::MAX as f32) as u32;
		// the scale rounded down, so a capped one stays within the limit
		let scale = (scale * 1024.0).floor().max(1.0) as u32;
		Self { content, width: quantize(canvas[0], 1024.0), height: quantize(canvas[1], 1024.0), scale, clips }
	}

	fn canvas(&self) -> [f32; 2] {
		[self.width as f32 / 1024.0, self.height as f32 / 1024.0]
	}

	fn scale(&self) -> f32 {
		self.scale as f32 / 1024.0
	}

	/// Device pixels kept on each side of a canvas that does not clip: Studio draws what overflows it
	/// (gui-surface-misc: overflow), here up to a quarter of the canvas and 8 GUI pixels.
	fn margin(&self) -> [u32; 2] {
		if self.clips {
			return [0, 0];
		}
		self.canvas().map(|size| ((size * 0.25 + 8.0) * self.scale()).ceil() as u32)
	}

	/// The image's extent in canvas fractions: (left, top, right, bottom), beyond 0..1 by the margins.
	fn extent(&self) -> [f32; 4] {
		let [width, height] = self.canvas().map(|size| size * self.scale());
		let [x, y] = self.margin().map(|margin| margin as f32);
		[-x / width, -y / height, 1.0 + x / width, 1.0 + y / height]
	}

	/// Its file in the disk cache ([`GuiImage`]'s `.gui`).
	fn file_name(&self) -> String {
		format!("{}.gui", self.stem())
	}

	fn stem(&self) -> String {
		format!("{:016x}-{}x{}-{}{}", self.content, self.width, self.height, self.scale, if self.clips { "" } else { "-overflow" })
	}
}

pub struct GuiTexture {
	texture: GpuTexture,
	group: wgpu::BindGroup,
	/// The share of the texture the canvas and its margins cover (u, v): the texture is whole device
	/// pixels.
	uv: [f32; 2],
	/// Where its texture goes once dropped: a frame's own image's, after that frame is submitted
	/// ([`GuiUploads`])
	recycle: Option<Arc<GuiUploads>>,
}

impl Drop for GuiTexture {
	fn drop(&mut self) {
		if let Some(uploads) = self.recycle.take()
			&& let Some(held) = self.texture.held()
		{
			uploads.give_back(held.texture, held.view, self.group.clone());
		}
	}
}

/// What a clip's GUI uploads go through again frame after frame ([`Renderer::upload_guis`]): two
/// staging buffers taking turns, each mapped again once its last copies are done, and the textures
/// of images only one frame drew, back from that frame once it is submitted (dropped after its
/// render: a later copy into one queues after the draws that read it), for the next image of their
/// size. An animated GUI is a new image every frame, its size the same: 13 telegraphs made a ~25 MB
/// staging buffer and 13 textures with their bind groups every frame, 1.1 ms of each frame's upload
/// for the buffer alone.
#[derive(Default)]
pub struct GuiUploads {
	staging: Mutex<StagingPair>,
	textures: Mutex<Vec<Recycled>>,
}

/// A GUI texture given back ([`GuiUploads`]): its size and levels, its view and bind group, when.
struct Recycled {
	size: (u32, u32, u32),
	texture: wgpu::Texture,
	view: wgpu::TextureView,
	group: wgpu::BindGroup,
	given: Instant,
}

#[derive(Default)]
struct StagingPair {
	buffers: [Option<(wgpu::Buffer, super::gpu::Submitted)>; 2],
	turn: usize,
	used: Option<Instant>,
}

/// Recycled GUI textures kept past this go (a clip's frames take theirs back within a frame or two)
const GUI_TEXTURES_KEPT: usize = 64;
/// What the uploads keep goes after this long unused ([`GuiUploads::trim`])
const GUI_UPLOADS_KEPT_FOR: std::time::Duration = std::time::Duration::from_secs(2);

impl GuiUploads {
	fn give_back(&self, texture: wgpu::Texture, view: wgpu::TextureView, group: wgpu::BindGroup) {
		let mut textures = self.textures.lock();
		if textures.len() >= GUI_TEXTURES_KEPT {
			textures.remove(0);
		}
		textures.push(Recycled { size: (texture.width(), texture.height(), texture.mip_level_count()), texture, view, group, given: Instant::now() });
	}

	fn take(&self, width: u32, height: u32, levels: u32) -> Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)> {
		let mut textures = self.textures.lock();
		let index = textures.iter().rposition(|entry| entry.size == (width, height, levels))?;
		let Recycled { texture, view, group, .. } = textures.remove(index);
		Some((texture, view, group))
	}

	/// Lets go of the staging buffers and textures unused for a while; returns their bytes.
	pub fn trim(&self, all: bool) -> u64 {
		let mut freed = 0;
		{
			let mut staging = self.staging.lock();
			if all || staging.used.is_some_and(|used| used.elapsed() > GUI_UPLOADS_KEPT_FOR) {
				for (buffer, _) in staging.buffers.iter_mut().filter_map(Option::take) {
					freed += buffer.size();
				}
				staging.used = None;
			}
		}
		self.textures.lock().retain(|entry| {
			let keep = !all && entry.given.elapsed() <= GUI_UPLOADS_KEPT_FOR;
			if !keep {
				let (width, height, levels) = entry.size;
				freed += (0..levels).map(|level| ((width >> level).max(1) as u64) * ((height >> level).max(1) as u64) * 4).sum::<u64>();
			}
			keep
		});
		freed
	}
}

impl Weighed for GuiTexture {
	fn bytes(&self) -> u64 {
		self.texture.bytes()
	}
}

/// What a ViewportFrame's world adds to a GUI draw (`shaders/gui.wgsl` GuiWindow): the frame's rect in
/// the quad's uv (left, top, right, bottom) and the image's tint (ImageColor3, and its opacity in w; 0:
/// no world); for a framed one (`mode` x 1) its UIGradient: `plane` gives t over the frame, `mode` y
/// and z count `colors` (t, r, g, b) and `transparency` (t, value) keypoints; `corner` xy its UICorner's
/// radius in the quad's uv (0: square corners).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct WindowUniform {
	rect: [f32; 4],
	tint: [f32; 4],
	mode: [f32; 4],
	plane: [f32; 4],
	colors: [[f32; 4]; GRADIENT_KEYS],
	transparency: [[f32; 4]; GRADIENT_KEYS],
	corner: [f32; 4],
}

/// The GUI pipelines: in the scene (per MSAA sample count) and over the finished image.
pub struct GuiPipelines {
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
	/// A window's world as the view sees it (group 2), and the group for a GUI without one.
	window_layout: wgpu::BindGroupLayout,
	window_sampler: wgpu::Sampler,
	no_window: wgpu::BindGroup,
	/// A GUI texture of nothing (group 1): what draws over a framed ViewportFrame when nothing does
	no_texture: wgpu::BindGroup,
	/// 1 x 1, zero
	blank: wgpu::TextureView,
	scene: Vec<(u32, wgpu::RenderPipeline)>,
	overlay: wgpu::RenderPipeline,
}

impl GuiPipelines {
	pub fn new(device: &wgpu::Device, frame_layout: &wgpu::BindGroupLayout, cache: Option<&wgpu::PipelineCache>) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("gui texture"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
					count: None,
				},
				wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
			],
		});
		let window_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("gui window"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
					count: None,
				},
				wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
				wgpu::BindGroupLayoutEntry {
					binding: 2,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
					count: None,
				},
				// the share of each pixel an opaque surface of the world covers (ViewImage::opaque_share)
				wgpu::BindGroupLayoutEntry {
					binding: 3,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
					count: None,
				},
			],
		});
		// the world is drawn at the view's size and read pixel for pixel: no mips, no anisotropy
		let window_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("gui window"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
		// wgpu clears a new texture to zero: transparent, nothing opaque (a window drawn without its share)
		let blank = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("gui no window"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rgba8Unorm,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&Default::default());
		let no_window = {
			// the uniform's zero opacity draws none
			let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("gui no window"), contents: bytemuck::bytes_of(&WindowUniform::default()), usage: wgpu::BufferUsages::UNIFORM });
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("gui no window"),
				layout: &window_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank) },
					wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&window_sampler) },
					wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&blank) },
				],
			})
		};
		// (the GUI draws read the image's texels themselves, gui.wgsl `gui_sample`; the bind group keeps a sampler)
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("gui"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Linear,
			anisotropy_clamp: 16,
			..Default::default()
		});
		let no_texture = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("gui no texture"),
			layout: &layout,
			entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("gui"), source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{SHADOW}\n{LIGHTS}\n{GUI}\n{VOXEL_READ}").into()) });
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("gui"), bind_group_layouts: &[frame_layout, &layout, &window_layout], push_constant_ranges: &[] });
		let premultiplied = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
		let pipeline = |label: &str, entry: &str, samples: u32, format: wgpu::TextureFormat, depth: Option<wgpu::DepthStencilState>| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(label),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_gui"), buffers: &[GuiVertex::layout()], compilation_options: Default::default() },
				primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
				depth_stencil: depth,
				multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
				fragment: Some(wgpu::FragmentState {
					module: &module,
					entry_point: Some(entry),
					targets: &[Some(wgpu::ColorTargetState { format, blend: premultiplied, write_mask: wgpu::ColorWrites::ALL })],
					compilation_options: Default::default(),
				}),
				multiview: None,
				cache,
			})
		};
		let scene = SAMPLE_COUNTS
			.iter()
			.map(|&samples| {
				let depth = wgpu::DepthStencilState {
					format: DEPTH_FORMAT,
					depth_write_enabled: false,
					depth_compare: wgpu::CompareFunction::GreaterEqual,
					stencil: Default::default(),
					bias: Default::default(),
				};
				(samples, pipeline("gui", "fs_gui", samples, HDR_FORMAT, Some(depth)))
			})
			.collect();
		let overlay = pipeline("gui overlay", "fs_gui_overlay", 1, OUTPUT_FORMAT, None);
		Self { layout, sampler, window_layout, window_sampler, no_window, no_texture, blank, scene, overlay }
	}

	fn scene(&self, samples: u32) -> &wgpu::RenderPipeline {
		&self.scene.iter().find(|(count, _)| *count == samples).unwrap_or(&self.scene[0]).1
	}
}

/// GUI textures that could not be drawn recently (not tried again for a while).
#[derive(Default)]
pub struct GuiFailures {
	keys: Mutex<HashMap<GuiKey, (Instant, String)>>,
}

/// A GUI as a view draws it.
pub(super) struct GuiDraw {
	/// The GUI's index in `Scene::guis`.
	pub(super) gui: usize,
	pub(super) key: GuiKey,
	/// Live local-file snapshots taken when `key` was built, verified before keeping rur's output.
	local_snaps: Arc<[(PathBuf, Option<ror_assets::FileSnapshot>)]>,
	/// Top-left, top-right, bottom-left, bottom-right.
	corners: [Vec3; 4],
	normal: Vec3,
	params: [f32; 4],
	/// The point it sorts by among transparent geometry, and its order on a shared face (ZOffset).
	pub center: Vec3,
	pub z_offset: f32,
	/// AlwaysOnTop: in the layer over the finished image.
	pub overlay: bool,
	/// A ScreenGui's DisplayOrder: in that layer over the world's GUIs, by DisplayOrder.
	pub screen: Option<i32>,
	/// Its framed ViewportFrames as this view draws them, in the order the GUI draws them.
	framed: Vec<FramedDraw>,
}

/// A framed ViewportFrame of a GUI draw (scene::guis::Framed): its world drawn by its own camera at
/// about the size the view shows the frame, laid inside the frame's rect under what the GUI draws over
/// it (`over`'s image; none when nothing does).
struct FramedDraw {
	/// Its index in the GUI's viewports.
	viewport: usize,
	/// Its rect in canvas fractions (left, top, right, bottom).
	rect: [f32; 4],
	/// Its picture's size in pixels, at the frame's own proportions.
	size: (u32, u32),
	over: Option<GuiKey>,
	over_snaps: Arc<[(PathBuf, Option<ror_assets::FileSnapshot>)]>,
	/// Its UIGradient's t over it (scene::guis::Gradient::plane).
	plane: [f32; 3],
}

impl GuiDraw {
	/// The GUI images it draws with: the GUI's own, and what draws over each framed ViewportFrame.
	pub(super) fn keys(&self) -> impl Iterator<Item = GuiKey> + '_ {
		std::iter::once(self.key).chain(self.framed.iter().filter_map(|framed| framed.over))
	}

	/// Collects distinct local files referenced by this GUI draw that had to be hashed in full
	/// because their volume provided no trusted OS change token.
	pub(super) fn untrusted_paths(&self, out: &mut std::collections::HashSet<PathBuf>) {
		for (path, snap) in self.local_snaps.iter() {
			if snap.is_some_and(|snap| snap.is_untrusted_hash()) {
				out.insert(path.clone());
			}
		}
		for framed in &self.framed {
			for (path, snap) in framed.over_snaps.iter() {
				if snap.is_some_and(|snap| snap.is_untrusted_hash()) {
					out.insert(path.clone());
				}
			}
		}
	}
}

/// A framed ViewportFrame's picture is drawn no smaller than this (pixels), and its longer side no
/// longer than a GUI image's, at the frame's proportions.
const FRAMED_MIN: f32 = 8.0;

/// A GUI's framed ViewportFrames as a draw of it shows them: on a canvas of `canvas` GUI pixels, its
/// image keyed as `key`, the picture's height given by `height` (the frame's rect in canvas fractions
/// and in GUI pixels → pixels).
fn plan_framed(gui: &Gui, canvas: [f32; 2], key: &GuiKey, height: &dyn Fn([f32; 4], [f32; 4]) -> f32) -> Vec<FramedDraw> {
	gui.viewports
		.iter()
		.enumerate()
		.filter_map(|(index, viewport)| {
			let ViewportKind::Framed(framed) = &viewport.kind else {
				return None;
			};
			let rect = viewport.rect(canvas);
			let pixels = viewport.pixels(canvas);
			let (width, tall) = (pixels[2], pixels[3]);
			if !(width > 0.0 && tall > 0.0) {
				return None;
			}
			// at the frame's own proportions (its camera's aspect), as tall as it shows: a smaller picture
			// stretched across a wider frame would show its world squeezed
			let shown = height(rect, pixels);
			let fit = (MAX_TEXTURE / shown.max(shown * width / tall)).min(1.0);
			let rows = (shown * fit).round().clamp(FRAMED_MIN, MAX_TEXTURE);
			let columns = (rows * width / tall).round().clamp(FRAMED_MIN, MAX_TEXTURE);
			let (over, over_snaps) = match framed.over.as_ref() {
				Some((_, base_hash)) => {
					let (stamped, snaps) = crate::scene::guis::stamp_gui_hash(*base_hash, &framed.over_local_files);
					(Some(GuiKey { content: stamped, ..*key }), Arc::from(snaps))
				}
				None => (None, Arc::from([])),
			};
			let plane = framed.gradient.as_ref().map_or([0.0; 3], |gradient| gradient.plane([width, tall]));
			Some(FramedDraw { viewport: index, rect, size: (columns as u32, rows as u32), over, over_snaps, plane })
		})
		.collect()
}

/// Whether `scene` has GUIs to draw (ROR_NO_GUIS: the image without them, for before / after
/// comparisons).
fn drawn(scene: &Scene) -> bool {
	!scene.guis.is_empty() && std::env::var_os("ROR_NO_GUIS").is_none()
}

/// Where the scene's GUIs draw in a view and which textures they need, alongside any scene GUIs
/// skipped for this view and why.
pub(super) fn plan_with_skips(scene: &Scene, camera: &Camera, request: &ViewRequest) -> (Vec<GuiDraw>, Vec<crate::scene::guis::SkippedGui>) {
	let mut skipped = Vec::new();
	if scene.guis.is_empty() {
		return (Vec::new(), skipped);
	}
	if std::env::var_os("ROR_NO_GUIS").is_some() {
		for gui in &scene.guis {
			skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "ROR_NO_GUIS set".into() });
		}
		return (Vec::new(), skipped);
	}
	let supersample = request.supersample.clamp(1, 4);
	let (width, height) = (request.width.max(1), request.height.max(1));
	let right = Vec3::from(camera.world.x_axis).normalize_or(Vec3::X);
	let up = Vec3::from(camera.world.y_axis).normalize_or(Vec3::Y);
	let back = Vec3::from(camera.world.z_axis).normalize_or(Vec3::Z);
	let tan = (camera.fov.to_radians() * 0.5).tan().max(1e-4);
	let mut draws = Vec::new();
	for (index, gui) in scene.guis.iter().enumerate() {
		match &gui.kind {
			GuiKind::Surface(surface) => {
				let normal = surface.normal();
				let center = surface.point(0.5, 0.5);
				// one-sided: nothing shows from behind its face
				if (camera.eye - center).dot(normal) <= 0.0 {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "facing away from camera".into() });
					continue;
				}
				if gui.max_distance > 0.0 && camera.eye.distance(center) > gui.max_distance {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "beyond MaxDistance".into() });
					continue;
				}
				let (content_hash, local_snaps) = crate::scene::guis::stamp_gui_hash(gui.base_content_hash, &gui.local_files);
				let (across, upward) = (right, up);
				let focal = (height * if gui.always_on_top { 1 } else { supersample }) as f32 * 0.5 / tan;
				let project = |point: Vec3| {
					let relative = point - camera.eye;
					let depth = -relative.dot(back);
					(depth > camera.near).then(|| glam::Vec2::new(relative.dot(across) / depth, relative.dot(upward) / depth) * focal)
				};
				let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(u, v)| project(surface.point(u, v)));
				let key = GuiKey::new(content_hash, surface.canvas, surface_scale(surface.canvas, corners), gui.clips);
				let [left, top, right, bottom] = key.extent();
				// a framed ViewportFrame's picture as tall as the view shows the frame (its sides projected),
				// else as tall as the GUI's image draws it
				let shown = |rect: [f32; 4], pixels: [f32; 4]| match [(rect[0], rect[1]), (rect[2], rect[1]), (rect[0], rect[3]), (rect[2], rect[3])].map(|(u, v)| project(surface.point(u, v))) {
					[Some(a), Some(b), Some(c), Some(d)] => a.distance(c).max(b.distance(d)),
					_ => pixels[3] * key.scale(),
				};
				let framed = plan_framed(gui, surface.canvas, &key, &shown);
				draws.push(GuiDraw {
					gui: index,
					key,
					local_snaps: Arc::from(local_snaps),
					corners: [surface.point(left, top), surface.point(right, top), surface.point(left, bottom), surface.point(right, bottom)],
					normal,
					params: [gui.light_influence, gui.brightness, 0.0, 0.0],
					center,
					z_offset: surface.z_offset,
					overlay: gui.always_on_top,
					screen: None,
					framed,
				});
			}
			GuiKind::Billboard(billboard) => {
				// the pixels it is drawn in: the render target, or the image for AlwaysOnTop
				let overlay = gui.always_on_top;
				let grid = if overlay { 1 } else { supersample };
				let (grid_width, grid_height) = ((width * grid) as f32, (height * grid) as f32);
				// pixels per stud at one stud of depth
				let focal = grid_height * 0.5 / tan;
				// (in the key's 1/1024 steps, so the canvas at this scale is the whole pixels drawn)
				let gui_scale = (request.gui_scale.filter(|scale| *scale > 0.0).unwrap_or(height as f32 / REFERENCE_HEIGHT) * grid as f32 * 1024.0).floor().max(1.0) / 1024.0;
				// StudsOffset and ExtentsOffset in the camera's axes, z towards it (gui-billboard-basics:
				// studsoffset, extents; probes: StudsOffset z 5 sizes it as 5 studs nearer)
				let offset = billboard.camera_offset;
				let position = billboard.anchor + billboard.world_offset + right * offset.x + up * offset.y + back * offset.z;
				let relative = position - camera.eye;
				// sized by its view depth, not its distance (probes: 10 studs to the side reads as head-on)
				let depth = -relative.dot(back);
				if depth <= camera.near {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "behind near plane".into() });
					continue;
				}
				if gui.max_distance > 0.0 && relative.length() > gui.max_distance {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "beyond MaxDistance".into() });
					continue;
				}
				// whole device pixels: AbsoluteSize is round(1.5 · (studs · pixels per stud + offset)) / 1.5
				// (probes at depths 10-80 and fields of view 40 and 70)
				let [x_scale, x_offset, y_scale, y_offset] = billboard.size;
				let pixel_width = (x_scale * focal / depth + x_offset * gui_scale).round();
				let pixel_height = (y_scale * focal / depth + y_offset * gui_scale).round();
				if pixel_width < 1.0 || pixel_height < 1.0 {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "zero pixel size".into() });
					continue;
				}
				// centred on the projected anchor, SizeOffset moving it right and up by fractions of its
				// size, the corner on a whole pixel (gui-billboard-basics: sizeoffset, studs-12)
				let x = relative.dot(right) / depth * focal + grid_width * 0.5 + billboard.size_offset[0] * pixel_width;
				let y = -relative.dot(up) / depth * focal + grid_height * 0.5 - billboard.size_offset[1] * pixel_height;
				let (content_hash, local_snaps) = crate::scene::guis::stamp_gui_hash(gui.base_content_hash, &gui.local_files);
				let key = GuiKey::new(content_hash, [pixel_width / gui_scale, pixel_height / gui_scale], gui_scale, gui.clips);
				// the texture's margins in these pixels (wider than its texels when a big billboard's
				// texture is drawn at a smaller scale)
				let [margin_x, margin_y] = key.margin().map(|margin| margin as f32 * gui_scale / key.scale());
				// a half pixel rounds right and up (gui-billboard-basics: 566.5 -> 567 across, 532.5 -> 532
				// down the image, for stud, pixel and mixed sizes)
				let left = (x - pixel_width * 0.5 + 0.5).floor() - margin_x;
				let top = (y - pixel_height * 0.5 - 0.5).ceil() - margin_y;
				let (right_edge, bottom_edge) = (left + pixel_width + 2.0 * margin_x, top + pixel_height + 2.0 * margin_y);
				let corner = |px: f32, py: f32| camera.eye - back * depth + right * ((px - grid_width * 0.5) * depth / focal) + up * ((grid_height * 0.5 - py) * depth / focal);
				// a framed ViewportFrame's picture as tall as the frame is on the screen
				let framed = plan_framed(gui, key.canvas(), &key, &|rect: [f32; 4], _| (rect[3] - rect[1]) * pixel_height);
				draws.push(GuiDraw {
					gui: index,
					key,
					local_snaps: Arc::from(local_snaps),
					corners: [corner(left, top), corner(right_edge, top), corner(left, bottom_edge), corner(right_edge, bottom_edge)],
					// lit facing the camera (gui-light: bb-front, bb-behind)
					normal: back,
					params: [gui.light_influence, gui.brightness, 0.0, 0.0],
					center: position,
					z_offset: 0.0,
					overlay,
					screen: None,
					framed,
				});
			}
			GuiKind::Screen(screen) => {
				// the view in GUI pixels (whole 1/1024 steps of the scale), below the top bar when inset;
				// drawn 1:1 into the image's pixels over everything (screen-gui-post: no post effect touches it)
				let gui_scale = (request.gui_scale.filter(|scale| *scale > 0.0).unwrap_or(height as f32 / REFERENCE_HEIGHT) * 1024.0).floor().max(1.0) / 1024.0;
				// its canvas from `top` to `bottom`: the whole view, below the top bar, or inside it (in Studio's
				// edit viewport, which has no top bar, the whole view)
				let bar = (crate::scene::guis::top_bar_inset(scene.play) * gui_scale).min(height as f32);
				let (top, bottom) = match screen.area {
					ScreenArea::Whole => (0.0, height as f32),
					ScreenArea::BelowTopBar => (bar, height as f32),
					ScreenArea::TopBar if scene.play => (0.0, bar),
					ScreenArea::TopBar => (0.0, height as f32),
				};
				if (bottom - top) < 1.0 {
					skipped.push(crate::scene::guis::SkippedGui { path: gui.path.clone(), reason: "zero pixel size".into() });
					continue;
				}
				let (content_hash, local_snaps) = crate::scene::guis::stamp_gui_hash(gui.base_content_hash, &gui.local_files);
				let key = GuiKey::within(content_hash, [width as f32 / gui_scale, (bottom - top) / gui_scale], gui_scale, true, MAX_SCREEN_TEXTURE);
				// the canvas as the texture holds it (a canvas past MAX_SCREEN_TEXTURE is drawn smaller and magnified)
				let [canvas_width, canvas_height] = key.canvas().map(|size| size * gui_scale);
				let focal = height as f32 * 0.5 / tan;
				// (any depth past the near plane: the layer has no depth test; ten studs keep the corners'
				// pixels exact for cameras thousands of studs out)
				let depth = (camera.near * 4.0).max(10.0);
				let corner = |px: f32, py: f32| camera.eye - back * depth + right * ((px - width as f32 * 0.5) * depth / focal) + up * ((height as f32 * 0.5 - py) * depth / focal);
				// a framed ViewportFrame's picture at the frame's size in GUI pixels, stretched over it: Studio
				// draws a ScreenGui's ViewportFrame at its logical size whatever the device scale (rur's viewport
				// probes, frames in a ScreenGui in Studio 0.747 at 1.5x: a part's edge ramps over about three
				// device pixels); in an image smaller than Studio's view, at the pixels it covers (a picture
				// minified would alias)
				let framed = plan_framed(gui, key.canvas(), &key, &|_, pixels: [f32; 4]| pixels[3] * gui_scale.min(1.0));
				draws.push(GuiDraw {
					gui: index,
					key,
					local_snaps: Arc::from(local_snaps),
					corners: [corner(0.0, top), corner(canvas_width, top), corner(0.0, top + canvas_height), corner(canvas_width, top + canvas_height)],
					normal: back,
					params: [0.0, 1.0, 0.0, 0.0],
					center: camera.eye - back * depth,
					z_offset: 0.0,
					overlay: true,
					screen: Some(screen.display_order),
					framed,
				});
			}
		}
	}
	(draws, skipped)
}

/// Where the scene's GUIs draw in a view and which textures they need: SurfaceGuis on their part's
/// face; BillboardGuis sized for this camera and image.
pub(super) fn plan(scene: &Scene, camera: &Camera, request: &ViewRequest) -> Vec<GuiDraw> {
	plan_with_skips(scene, camera, request).0
}

/// The GUI draws of a view, uploaded: each one's texture and vertex range.
pub(super) struct ViewGuis {
	buffer: Option<wgpu::Buffer>,
	pub draws: Vec<ViewGui>,
}

pub(super) struct ViewGui {
	texture: Arc<GuiTexture>,
	/// Its window's world as this view sees it (group 2), if it is one.
	window: Option<wgpu::BindGroup>,
	/// Its framed ViewportFrames, each drawn over the GUI's image in turn: what draws over the frame
	/// (group 1; none: nothing) and the frame's picture (group 2).
	layers: Vec<(Option<Arc<GuiTexture>>, wgpu::BindGroup)>,
	vertices: std::ops::Range<u32>,
	pub center: Vec3,
	pub z_offset: f32,
	pub overlay: bool,
	pub screen: Option<i32>,
}

/// A window's world drawn by a view's camera (scene::guis::Viewport): the texture and its opaque share
/// (given back once the view is drawn), and the frame's rect on the canvas and its tint.
pub(super) struct Window {
	pub texture: wgpu::Texture,
	view: wgpu::TextureView,
	pub share: Option<wgpu::Texture>,
	share_view: Option<wgpu::TextureView>,
	rect: [f32; 4],
	tint: [f32; 4],
	/// Its corners' radius in canvas fractions (scene::guis::Viewport::corner).
	corner: [f32; 2],
}

/// A framed ViewportFrame's picture, drawn by its own camera (given back once the view is drawn), and
/// which of its draw's framed ViewportFrames it is.
pub(super) struct Picture {
	pub texture: wgpu::Texture,
	view: wgpu::TextureView,
	framed: usize,
}

/// The worlds a GUI draw shows: its window's, and its framed ViewportFrames' pictures.
#[derive(Default)]
pub(super) struct Worlds {
	pub window: Option<Window>,
	pub pictures: Vec<Picture>,
}

/// Live local-file snapshots carried with a GUI request.
type GuiSnaps = Arc<[(PathBuf, Option<ror_assets::FileSnapshot>)]>;

/// GUI images a lookup or a rur call is about: each one's key, its GUI, the content drawn (the GUI's
/// own, or what draws over one of its framed ViewportFrames), and the local-file snapshots captured
/// when `key` was built.
type Members<'a> = Vec<(GuiKey, &'a Gui, &'a Arc<Vec<u8>>, &'a GuiSnaps)>;

/// The GUI images `draws` of `scene` need ([`GuiDraw::keys`]).
fn wanted<'a>(scene: &'a Scene, draws: &[&'a GuiDraw]) -> Members<'a> {
	let mut wanted = Vec::new();
	for draw in draws {
		let Some(gui) = scene.guis.get(draw.gui) else {
			continue;
		};
		wanted.push((draw.key, gui, &gui.content, &draw.local_snaps));
		for framed in &draw.framed {
			if let (Some(key), Some(ViewportKind::Framed(crate::scene::guis::Framed { over: Some((content, _)), .. }))) = (framed.over, gui.viewports.get(framed.viewport).map(|viewport| &viewport.kind)) {
				wanted.push((key, gui, content, &framed.over_snaps));
			}
		}
	}
	wanted
}

/// Whether every local-file snapshot in `snaps` still reflects the file on disk (used to avoid
/// caching transient failures when a local file moved).
fn snaps_still_unchanged(snaps: &[(PathBuf, Option<ror_assets::FileSnapshot>)]) -> bool {
	snaps.iter().all(|(path, snap)| match snap {
		Some(snap) => snap.still_unchanged(path),
		None => !path.exists(),
	})
}

/// GUI images kept on disk; past that the least recently used go (a billboard has an image per size,
/// so per camera distance).
const DISK_IMAGES: usize = 8192;
/// How long the images of another rur build are kept after it last drew.
const OLD_BUILDS: Duration = Duration::from_secs(24 * 3600);
/// The disk cache is pruned at most this often: its listing (thousands of files) was taken after
/// every rur call, every frame of an animated GUI.
const PRUNE_EVERY: Duration = Duration::from_secs(60);
/// Other processes' rur work directories (render::rur keeps its own between calls) left this long go.
const OLD_WORK: Duration = Duration::from_secs(3600);

static LAST_PRUNE: Mutex<Option<Instant>> = Mutex::new(None);

/// The cache files this process marked used lately (their modification time, which the pruning goes
/// by), and when: marked again only after [`MARK_EVERY`]. Opening each file a second time to mark it
/// on every read (13 images a frame: two opens, a metadata write and a close each, scanned by the
/// antivirus) took a fifth of a GUI clip's CPU.
static MARKED: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);
const MARK_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

fn mark_used(file: &std::path::Path) {
	{
		let mut marked = MARKED.lock();
		let marked = marked.get_or_insert_with(HashMap::new);
		if marked.get(file).is_some_and(|when| when.elapsed() < MARK_EVERY) {
			return;
		}
		if marked.len() > 65_536 {
			marked.retain(|_, when| when.elapsed() < MARK_EVERY);
		}
		marked.insert(file.to_path_buf(), Instant::now());
	}
	let _ = std::fs::File::options().write(true).open(file).and_then(|handle| handle.set_modified(std::time::SystemTime::now()));
}

/// The folder of `revision`'s GUI images under `guis/`: rur's build and the code that makes the files
/// (render::gui_files, `ROR_GUI_CODE`), so no build reads images another build's code made otherwise.
fn images_folder(revision: &str) -> String {
	format!("{revision}-{}", &env!("ROR_GUI_CODE")[..8])
}

/// A GUI image kept as `name` in `folder` elsewhere: the content store's, else another state dir's that
/// used it, taken whole (a `.gui` file this build reads, all its bytes there) and made this state dir's
/// too at `own` (hard-linked), and the store's.
fn kept_elsewhere(folder: &str, name: &str, own: &std::path::Path) -> Option<GuiImage> {
	let store = crate::output::store_dir().map(|store| (store.join("guis").join(folder).join(name), true));
	let others = crate::output::other_state_dirs().iter().map(|dir| (dir.join("guis").join(folder).join(name), false));
	for (candidate, in_store) in store.into_iter().chain(others) {
		let Ok(bytes) = std::fs::read(&candidate) else { continue };
		let Some(image) = GuiImage::from_file(&bytes) else {
			if in_store {
				let _ = std::fs::remove_file(&candidate);
			}
			continue;
		};
		if ror_assets::store::link(&candidate, own).is_ok() {
			mark_used(own);
			if !in_store {
				publish(folder, own);
			}
		}
		return Some(image);
	}
	None
}

/// Gives the content store the GUI image kept at `file` (a name of the same file).
fn publish(folder: &str, file: &std::path::Path) {
	if let Some(root) = crate::output::store_dir()
		&& let Some(name) = file.file_name()
		&& ror_assets::store::link(file, &root.join("guis").join(folder).join(name)).is_ok()
	{
		ror_assets::store::maybe_prune(&root);
	}
}

fn prune_disk_cache(directory: &std::path::Path) {
	{
		let mut last = LAST_PRUNE.lock();
		if last.is_some_and(|when| when.elapsed() < PRUNE_EVERY) {
			return;
		}
		*last = Some(Instant::now());
	}
	let idle = |entry: &std::fs::DirEntry| entry.metadata().and_then(|metadata| metadata.modified()).ok().and_then(|modified| modified.elapsed().ok());
	// other builds' images (rur's project directory stays)
	if let Some(parent) = directory.parent()
		&& let Ok(entries) = std::fs::read_dir(parent)
	{
		for entry in entries.flatten() {
			let path = entry.path();
			if path == directory || entry.file_name() == "project" || !path.is_dir() {
				continue;
			}
			if idle(&entry).is_some_and(|idle| idle > OLD_BUILDS) {
				let _ = std::fs::remove_dir_all(&path);
			}
		}
	}
	// work directories of processes gone (a running one draws into its own every call)
	let own = format!("{}-", std::process::id());
	if let Ok(entries) = std::fs::read_dir(directory.join("work")) {
		for entry in entries.flatten() {
			if !entry.file_name().to_string_lossy().starts_with(&own) && idle(&entry).is_some_and(|idle| idle > OLD_WORK) {
				let _ = std::fs::remove_dir_all(entry.path());
			}
		}
	}
	let Ok(entries) = std::fs::read_dir(directory) else {
		return;
	};
	let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
		.flatten()
		.filter(|entry| entry.path().extension().is_some_and(|extension| extension == "gui" || extension == "png"))
		.filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
		.collect();
	if files.len() <= DISK_IMAGES {
		return;
	}
	files.sort();
	let excess = files.len() - DISK_IMAGES * 3 / 4;
	for (_, path) in files.into_iter().take(excess) {
		let _ = std::fs::remove_file(path);
	}
}


/// rur's batches for GUI images (frame, key, GUI, content) in frame order, made frame by frame as each
/// frame alone would make them: one scale each, at most BATCH_PIXELS of content, an image into the first
/// of its frame's batches it fits (scale, members, device pixels). A GUI's images (its own and what draws
/// over its framed ViewportFrames, nearly the same content) go into one batch while they fit twice that:
/// a rur call that finds no warm VM waiting runs in a new one, ~1 s, far more than drawing the second
/// image (an emote's portrait, two images of 2.1 M pixels a frame: a 3 s clip made 63 more calls, 14 ->
/// 65 s).
fn frame_batches<'a>(guis: Vec<(usize, GuiKey, &'a Gui, &'a Arc<Vec<u8>>, &'a GuiSnaps)>) -> Vec<(u32, Members<'a>, u64)> {
	let mut batches: Vec<(u32, Members, u64)> = Vec::new();
	// the frame being batched and where its batches start
	let mut current = (usize::MAX, 0);
	for (frame, key, gui, content, snaps) in guis {
		if frame != current.0 {
			current = (frame, batches.len());
		}
		let (width, height) = rur::image_size(key.canvas(), key.scale(), key.margin());
		let pixels = width as u64 * height as u64;
		let fits = |scale: &u32, members: &Members, total: &u64| *scale == key.scale && (*total + pixels <= BATCH_PIXELS || (*total + pixels <= 2 * BATCH_PIXELS && members.iter().all(|(_, other, _, _)| std::ptr::eq(*other, gui))));
		match batches[current.1..].iter_mut().find(|(scale, members, total)| fits(scale, members, total)) {
			Some((_, members, total)) => {
				members.push((key, gui, content, snaps));
				*total += pixels;
			}
			None => batches.push((key.scale, vec![(key, gui, content, snaps)], pixels)),
		}
	}
	batches
}

/// The GUI textures a render draws with, by key: resolved ahead of it so that its GPU slot never
/// waits on rur ([`Renderer::resolve_frame_guis`], [`Renderer::upload_frame_guis`]).
pub type GuiTextures = HashMap<GuiKey, Arc<GuiTexture>>;

/// What a frame's GUIs need, found before it draws: the textures the GPU cache already holds (kept
/// here, so the cache may let them go meanwhile) and the images, from disk or rur, to upload when it
/// draws, each marked whether the GPU cache keeps it (an image other frames draw too); and what went
/// wrong finding them.
#[derive(Default)]
pub struct FrameGuis {
	textures: GuiTextures,
	images: Vec<(GuiKey, Arc<GuiImage>, bool)>,
	skipped: HashMap<GuiKey, String>,
	pub warnings: Vec<String>,
}

impl Renderer {
	/// A GUI's texture, its levels copied by `encoder` from `staged` (its decompressed bytes at `base`):
	/// one a frame gave back when `recycle` (a frame's own image's, given back in turn).
	fn staged_gui(&self, key: &GuiKey, image: &GuiImage, staged: &wgpu::Buffer, base: u64, encoder: &mut wgpu::CommandEncoder, recycle: Option<&Arc<GuiUploads>>) -> Result<GuiTexture> {
		let (width, height) = (image.width, image.height);
		let max_dimension = self.gpu.device.limits().max_texture_dimension_2d;
		if width > max_dimension || height > max_dimension {
			return Err(anyhow::anyhow!("gui: texture size {width}x{height} unsupported"));
		}
		let reused = recycle.and_then(|uploads| uploads.take(width, height, image.levels));
		let raw = match &reused {
			Some((texture, _, _)) => texture.clone(),
			None => self.gpu.device.create_texture(&wgpu::TextureDescriptor {
				label: Some("gui"),
				size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
				mip_level_count: image.levels,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rgba8Unorm,
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			}),
		};
		let (layout, size) = GuiImage::layout(width, height, image.levels);
		for (level, (w, h, row, offset)) in layout.into_iter().enumerate() {
			encoder.copy_buffer_to_texture(
				wgpu::TexelCopyBufferInfo { buffer: staged, layout: wgpu::TexelCopyBufferLayout { offset: base + offset, bytes_per_row: Some(row), rows_per_image: Some(h) } },
				wgpu::TexelCopyTextureInfo { texture: &raw, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
			);
		}
		// what its levels hold, rows unpadded
		let bytes = (0..image.levels).map(|level| ((width >> level).max(1) as u64) * ((height >> level).max(1) as u64) * 4).sum::<u64>().min(size);
		let (texture, group) = match reused {
			Some((_, view, group)) => (GpuTexture::whole(raw, view, width, height, [true, false, false], bytes), group),
			None => {
				let view = raw.create_view(&wgpu::TextureViewDescriptor::default());
				let texture = GpuTexture::whole(raw, view, width, height, [true, false, false], bytes);
				let group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
					label: Some("gui"),
					layout: &self.guis.layout,
					entries: &[
						wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&texture.view()) },
						wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.guis.sampler) },
					],
				});
				(texture, group)
			}
		};
		// the canvas and its margins cover the texture from its top-left corner (whole device pixels
		// rounded up)
		let [margin_x, margin_y] = key.margin().map(|margin| 2.0 * margin as f32);
		let [canvas_width, canvas_height] = key.canvas().map(|size| size * key.scale());
		let uv = [((canvas_width + margin_x) / width as f32).min(1.0), ((canvas_height + margin_y) / height as f32).min(1.0)];
		Ok(GuiTexture { texture, group, uv, recycle: recycle.cloned() })
	}

	/// What the GUIs of `frames` (each frame's GUIs in its draws' order) draw with: the textures the GPU
	/// cache holds, else their images from disk (read on every core), else drawn by rur. rur's batches
	/// are made frame by frame as each frame alone would have made them (one scale each, at most
	/// BATCH_PIXELS; a GUI with the first frame that needs it), so an image comes out of the same atlas
	/// as when every frame asked for its own: rur draws a GUI a little differently by what shares its
	/// atlas (render::rur). The batches of all the frames are drawn RUR_WORKERS at a time, each written
	/// to disk and mip-mapped as it arrives. Problems go into `warnings`.
	fn resolve_guis(&self, frames: &[Members], warnings: &mut Vec<String>, skipped: &mut HashMap<GuiKey, String>) -> (GuiTextures, HashMap<GuiKey, (Arc<GuiImage>, bool)>) {
		let mut textures = HashMap::new();
		let mut images: HashMap<GuiKey, (Arc<GuiImage>, bool)> = HashMap::new();
		let mut seen = std::collections::HashSet::new();
		// (frame, key, GUI, content, snaps) of what the GPU cache misses, in frame order
		let mut missing: Vec<(usize, GuiKey, &Gui, &Arc<Vec<u8>>, &GuiSnaps)> = Vec::new();
		for (frame, members) in frames.iter().enumerate() {
			for &(key, gui, content, snaps) in members {
				if !seen.insert(key) {
					continue;
				}
				match self.gui_textures.get(&key) {
					Some(texture) => {
						textures.insert(key, texture);
					}
					None => missing.push((frame, key, gui, content, snaps)),
				}
			}
		}
		if missing.is_empty() {
			return (textures, images);
		}
		let rur = match rur::locate_diagnosed() {
			Ok(Some(rur)) => rur,
			Ok(None) => {
				warnings.push(format!("{} GUIs not drawn: rur not found (set ROR_RUR or RUR_HOME to a roblox-ui-renderer checkout, put one beside this one, or put rur on PATH)", missing.len()));
				for (_, key, ..) in &missing {
					skipped.insert(*key, "rur not found (set ROR_RUR or RUR_HOME)".into());
				}
				return (textures, images);
			}
			Err(problem) => {
				warnings.push(format!("GUIs not drawn: {problem}"));
				for (_, key, ..) in &missing {
					skipped.insert(*key, problem.clone());
				}
				return (textures, images);
			}
		};
		let profile = std::env::var_os("ROR_PROFILE").is_some();
		let started = Instant::now();
		// drawn before, by this or another process, with this rur: on disk (marked used, the cache
		// drops the least recently used); not while rur is between builds (no fingerprint to file
		// them under)
		let revision = rur::revision(&rur);
		let folder = images_folder(&revision);
		let guis_dir = crate::output::state_dir().join("guis");
		let directory = guis_dir.join(&folder);
		let from_disk: Vec<Option<GuiImage>> = missing
			.par_iter()
			.map(|(_, key, _, _, _)| {
				if revision == UNBUILT {
					return None;
				}
				let file = directory.join(key.file_name());
				if let Some(image) = std::fs::read(&file).ok().and_then(|bytes| GuiImage::from_file(&bytes)) {
					mark_used(&file);
					return Some(image);
				}
				kept_elsewhere(&folder, &key.file_name(), &file)
			})
			.collect();
		let mut left = Vec::new();
		for ((frame, key, gui, content, snaps), image) in missing.into_iter().zip(from_disk) {
			match image {
				Some(image) => {
					images.insert(key, (Arc::new(image), true));
				}
				None => left.push((frame, key, gui, content, snaps)),
			}
		}
		if profile && !images.is_empty() {
			eprintln!("profile: {} GUI images from disk {:.1} ms", images.len(), started.elapsed().as_secs_f64() * 1000.0);
		}
		// recent failures are not tried again
		{
			let mut failures = self.gui_failures.keys.lock();
			failures.retain(|_, (when, _)| when.elapsed() < FAILURE_TTL);
			left.retain(|(_, key, gui, _, _)| match failures.get(key) {
				Some((_, reason)) => {
					warnings.push(format!("{}: not drawn ({reason})", gui.path));
					skipped.insert(*key, format!("rur failed: {reason}"));
					false
				}
				None => true,
			});
		}
		if left.is_empty() {
			return (textures, images);
		}
		let rur_start = Instant::now();
		let batches = frame_batches(left);
		let offline = self.assets.offline();
		let calls = batches.len();
		let next = std::sync::atomic::AtomicUsize::new(0);
		let finished: Mutex<Vec<(GuiKey, GuiImage, bool)>> = Mutex::new(Vec::new());
		let problems: Mutex<Vec<String>> = Mutex::new(Vec::new());
		let key_errors: Mutex<Vec<(GuiKey, String)>> = Mutex::new(Vec::new());
		std::thread::scope(|scope| {
			for _ in 0..RUR_WORKERS.min(calls) {
				scope.spawn(|| {
					loop {
						let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						let Some((scale, members, _)) = batches.get(index) else {
							break;
						};
						let requests: Vec<rur::Request> = members
							.iter()
							.map(|(key, gui, content, snaps)| rur::Request { content: (*content).clone(), canvas: key.canvas(), margin: key.margin(), z_index_behavior: gui.z_index_behavior, local_snaps: (*snaps).clone() })
							.collect();
						match rur::render(&rur, &requests, *scale as f32 / 1024.0, &directory, offline) {
							Ok((pieces, report)) => {
								// rur may have rebuilt itself for these: its images go with its new build's (a folder
								// that may not be there yet)
								let revision = rur::revision(&rur);
								let directory = guis_dir.join(images_folder(&revision));
								let mut found = report.warnings;
								if let Err(error) = std::fs::create_dir_all(&directory) {
									found.push(format!("caching GUI textures in {}: {error}", directory.display()));
								}
								// made ready for the GPU and written to the disk cache on every core;
								// images whose local inputs moved between key-stamp and read are drawn
								// once without writing to disk, store, or the GPU cache.
								let done: Vec<(GuiKey, GuiImage, bool, Option<String>)> = members
									.par_iter()
									.zip(pieces.into_par_iter())
									.filter_map(|((key, gui, _, _), piece)| {
										let stable = piece.stable;
										match GuiImage::from_rgba(piece.width, piece.height, piece.rgba) {
											Ok(image) => {
												let file = directory.join(key.file_name());
												let written = if !stable || revision == UNBUILT { Ok(()) } else { crate::output::write_atomic(&file, &image.file_bytes()).inspect(|_| publish(&images_folder(&revision), &file)) };
												let problem = written.err().map(|error| format!("{}: caching its GUI texture: {error:#}", gui.path));
												Some((*key, image, stable, problem))
											}
											Err(error) => {
												let reason = format!("texture encode failed: {error:#}");
												problems.lock().push(format!("{}: encoding its GUI texture: {error:#}", gui.path));
												key_errors.lock().push((*key, reason));
												None
											}
										}
									})
									.collect();
								let mut finished = finished.lock();
								for (key, image, stable, problem) in done {
									found.extend(problem);
									finished.push((key, image, stable));
								}
								problems.lock().extend(found);
							}
							Err(error) => {
								let reason = format!("{error:#}");
								let mut failures = self.gui_failures.keys.lock();
								let mut problems = problems.lock();
								let mut errs = key_errors.lock();
								let transient = self.assets.is_offline_view();
								for (key, gui, _, snaps) in members {
									problems.push(format!("{}: not drawn ({reason})", gui.path));
									errs.push((*key, format!("rur failed: {reason}")));
									if !transient && snaps_still_unchanged(snaps) {
										failures.insert(*key, (Instant::now(), reason.clone()));
									}
								}
							}
						}
					}
				});
			}
		});
		warnings.extend(problems.into_inner());
		for (key, reason) in key_errors.into_inner() {
			skipped.entry(key).or_insert(reason);
		}
		let finished = finished.into_inner();
		let drawn = finished.len();
		for (key, image, stable) in finished {
			images.insert(key, (Arc::new(image), stable));
		}
		prune_disk_cache(&guis_dir.join(images_folder(&rur::revision(&rur))));
		if profile {
			eprintln!("profile: {drawn} GUI images from rur in {calls} calls {:.1} ms", rur_start.elapsed().as_secs_f64() * 1000.0);
		}
		(textures, images)
	}

	/// Uploads GUI images, decoded on every core, adding their textures to `textures`: into the GPU
	/// cache those marked so (each once, however many renders ask for it), the rest for the render that
	/// asks alone.
	fn upload_guis(&self, images: Vec<(GuiKey, Arc<GuiImage>, bool)>, textures: &mut GuiTextures, warnings: &mut Vec<String>, skipped: &mut HashMap<GuiKey, String>) {
		let mut wanted = Vec::new();
		for (key, image, cached) in images {
			match self.gui_textures.get(&key).filter(|_| cached) {
				Some(texture) => {
					textures.insert(key, texture);
				}
				None => wanted.push((key, image, cached)),
			}
		}
		if wanted.is_empty() {
			return;
		}
		// one staging buffer for them all, every image's pieces decompressed into their places on every
		// core; then one submission copies them into their textures
		let device = &self.gpu.device;
		let mut offsets = Vec::with_capacity(wanted.len());
		let mut total = 0u64;
		for (_, image, _) in &wanted {
			offsets.push(total);
			total += image.size().next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
		}
		// the next of the two staging buffers kept between frames, mapped again once its last copies are
		// done (made larger when this frame's images need more); another upload using them meanwhile
		// (two clips with GUIs) takes a buffer of its own
		let mut pair = self.gui_uploads.staging.try_lock();
		let staging = match pair.as_deref_mut() {
			Some(pair) => {
				let slot = pair.turn % 2;
				pair.turn += 1;
				pair.used = Some(Instant::now());
				match pair.buffers[slot].take().filter(|(buffer, _)| buffer.size() >= total.max(4)) {
					Some((buffer, copied)) => {
						let (sender, mapped) = std::sync::mpsc::channel();
						buffer.slice(..).map_async(wgpu::MapMode::Write, move |result| {
							let _ = sender.send(result);
						});
						match self.gpu.wait_mapped(copied, &mapped) {
							Ok(Ok(())) => buffer,
							_ => device.create_buffer(&wgpu::BufferDescriptor { label: Some("gui staging"), size: total.max(4).next_power_of_two(), usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: true }),
						}
					}
					None => device.create_buffer(&wgpu::BufferDescriptor { label: Some("gui staging"), size: total.max(4).next_power_of_two(), usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: true }),
				}
			}
			None => device.create_buffer(&wgpu::BufferDescriptor { label: Some("gui staging"), size: total.max(4), usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: true }),
		};
		let unpacked: Vec<Result<()>> = {
			let mut mapped = staging.slice(..total.max(4)).get_mapped_range_mut();
			// every piece's place: the mapped bytes split in order
			let mut places: Vec<(usize, &[u8], &mut [u8])> = Vec::new();
			let mut rest: &mut [u8] = &mut mapped[..];
			let mut at = 0u64;
			for (index, ((_, image, _), offset)) in wanted.iter().zip(&offsets).enumerate() {
				let skip = (offset - at) as usize;
				rest = &mut std::mem::take(&mut rest)[skip..];
				at = *offset;
				let size = image.size() as usize;
				let (mut target, after) = std::mem::take(&mut rest).split_at_mut(size);
				rest = after;
				at += size as u64;
				for (piece, start) in image.pieces() {
					let length = GUI_CHUNK.min(size - start);
					let (place, more) = std::mem::take(&mut target).split_at_mut(length);
					target = more;
					places.push((index, piece, place));
				}
			}
			let failed: Vec<(usize, anyhow::Error)> = places.into_par_iter().filter_map(|(index, piece, place)| GuiImage::unpack(piece, place).err().map(|error| (index, error))).collect();
			let mut results: Vec<Result<()>> = (0..wanted.len()).map(|_| Ok(())).collect();
			for (index, error) in failed {
				results[index] = Err(error);
			}
			results
		};
		staging.unmap();
		let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gui uploads") });
		let mut made = Vec::new();
		for (((key, image, cached), offset), unpacked) in wanted.into_iter().zip(offsets).zip(unpacked) {
			// a frame's own image's texture goes back to the uploads after its frame (the GPU cache's stay)
			let recycle = (!cached).then_some(&self.gui_uploads);
			match unpacked.and_then(|()| self.staged_gui(&key, &image, &staging, offset, &mut encoder, recycle)) {
				Ok(texture) => made.push((key, cached, texture)),
				Err(error) => {
					warnings.push(format!("GUI texture: {error:#}"));
					skipped.entry(key).or_insert_with(|| format!("texture upload failed: {error:#}"));
				}
			}
		}
		let copied = self.gpu.submit([encoder.finish()]);
		// the staging buffer back in its place, mapped again once these copies are done
		if let Some(pair) = pair.as_deref_mut() {
			let slot = (pair.turn + 1) % 2;
			pair.buffers[slot] = Some((staging, copied));
		}
		drop(pair);
		// into the GPU cache only once their copies are submitted: another render may draw with what it
		// finds there (one that got there first is kept)
		for (key, cached, texture) in made {
			let texture = if cached { self.gui_textures.get_or_load(&key, || Ok(texture)) } else { Ok(Arc::new(texture)) };
			match texture {
				Ok(texture) => {
					textures.insert(key, texture);
				}
				Err(error) => {
					warnings.push(format!("GUI texture: {error:#}"));
					skipped.entry(key).or_insert_with(|| format!("texture upload failed: {error:#}"));
				}
			}
		}
	}

	/// The textures `draws` need: from memory, from disk, else drawn by rur. Problems go into
	/// `warnings`, and per-key failure reasons into `skipped`.
	pub(super) fn gui_textures_reported(&self, scene: &Scene, draws: &[&GuiDraw], warnings: &mut Vec<String>, skipped: &mut HashMap<GuiKey, String>) -> GuiTextures {
		let (mut textures, images) = self.resolve_guis(&[wanted(scene, draws)], warnings, skipped);
		// a still's stable images are kept: the render after the service's prefetch finds them in the GPU cache
		self.upload_guis(images.into_iter().map(|(key, (image, stable))| (key, image, stable)).collect(), &mut textures, warnings, skipped);
		textures
	}

	/// Loads (or has rur draw) every GUI texture `views` of `scene` need, so the render that follows
	/// finds them cached; the render service does this before taking a GPU slot. Returns the warnings.
	pub fn prefetch_guis(&self, scene: &Scene, views: &[ViewRequest]) -> Vec<String> {
		let mut warnings = Vec::new();
		if !drawn(scene) {
			return warnings;
		}
		let bounds = self.framing_bounds(scene);
		let plans: Vec<Vec<GuiDraw>> = views.iter().map(|view| plan(scene, &view.camera.place(bounds, view.width.max(1), view.height.max(1)), view)).collect();
		let draws: Vec<&GuiDraw> = plans.iter().flatten().collect();
		let mut skipped = HashMap::new();
		self.gui_textures_reported(scene, &draws, &mut warnings, &mut skipped);
		warnings
	}

	/// The GUIs of many frames, each a scene and the view drawing it, found together: one set of rur
	/// calls for every GUI image they miss (an animated GUI is a new image every frame: 13 telegraphs
	/// cost two rur calls a frame), before any of them takes a GPU slot. One [`FrameGuis`] per frame,
	/// the problems in the first's.
	pub fn resolve_frame_guis(&self, frames: &[(&Scene, &ViewRequest)]) -> Vec<FrameGuis> {
		let plans: Vec<Vec<GuiDraw>> = frames
			.par_iter()
			.map(|(scene, view)| {
				if !drawn(scene) {
					return Vec::new();
				}
				let (width, height) = (view.width.max(1), view.height.max(1));
				// an orbit with its target and distance fixed (a clip's) is placed without the scene's bounds
				let bounds = match view.camera {
					super::camera::CameraSpec::Orbit { distance: Some(_), target: Some(_), .. } | super::camera::CameraSpec::LookAt { .. } | super::camera::CameraSpec::CFrame { .. } => None,
					_ => self.framing_bounds(scene),
				};
				plan(scene, &view.camera.place(bounds, width, height), view)
			})
			.collect();
		let wanted: Vec<Members> = frames.iter().zip(&plans).map(|((scene, _), plan)| wanted(scene, &plan.iter().collect::<Vec<_>>())).collect();
		let mut warnings = Vec::new();
		let mut skipped = HashMap::new();
		let (textures, images) = if wanted.iter().all(Vec::is_empty) { Default::default() } else { self.resolve_guis(&wanted, &mut warnings, &mut skipped) };
		// the GPU cache keeps an image other frames of these draw too; an animated GUI's image, a
		// frame's own, leaves the GPU once its frame has drawn
		let mut frames_drawing: HashMap<GuiKey, usize> = HashMap::new();
		for plan in &plans {
			let keys: std::collections::HashSet<GuiKey> = plan.iter().flat_map(GuiDraw::keys).collect();
			for key in keys {
				*frames_drawing.entry(key).or_default() += 1;
			}
		}
		let mut resolved: Vec<FrameGuis> = plans
			.iter()
			.map(|plan| {
				let mut frame = FrameGuis::default();
				for key in plan.iter().flat_map(GuiDraw::keys) {
					if let Some(texture) = textures.get(&key) {
						frame.textures.insert(key, texture.clone());
					} else if let Some((image, stable)) = images.get(&key)
						&& !frame.images.iter().any(|(kept, _, _)| *kept == key)
					{
						frame.images.push((key, image.clone(), *stable && frames_drawing.get(&key).is_some_and(|&frames| frames > 1)));
					} else if let Some(reason) = skipped.get(&key) {
						frame.skipped.entry(key).or_insert_with(|| reason.clone());
					}
				}
				frame
			})
			.collect();
		if let Some(first) = resolved.first_mut() {
			first.warnings = warnings;
		}
		resolved
	}

	/// A frame's GUI images uploaded (into the GPU cache, each once), with the textures it already had:
	/// what its render draws with ([`Renderer::submit_video_with`]). Problems go into `warnings`.
	pub fn upload_frame_guis(&self, guis: FrameGuis, warnings: &mut Vec<String>) -> GuiTextures {
		self.upload_frame_guis_reported(guis, warnings).0
	}

	pub(crate) fn upload_frame_guis_reported(&self, guis: FrameGuis, warnings: &mut Vec<String>) -> (GuiTextures, HashMap<GuiKey, String>) {
		let FrameGuis { mut textures, images, mut skipped, warnings: found } = guis;
		warnings.extend(found);
		self.upload_guis(images, &mut textures, warnings, &mut skipped);
		(textures, skipped)
	}

	/// The worlds a view's GUIs show (scene::guis::Viewport), by the draw's index: a window's drawn by
	/// this view's camera, at the view's size, transparent where it draws nothing; each framed
	/// ViewportFrame's by its own camera, at the size the view shows it. Problems go into `warnings`.
	pub(super) fn render_worlds(&self, scene: &Scene, draws: &[GuiDraw], request: &ViewRequest, bounds: Option<(Vec3, Vec3)>, warnings: &mut Vec<String>) -> HashMap<usize, Worlds> {
		let mut worlds: HashMap<usize, Worlds> = HashMap::new();
		for (index, draw) in draws.iter().enumerate() {
			let Some(gui) = scene.guis.get(draw.gui) else {
				continue;
			};
			for (at, framed) in draw.framed.iter().enumerate() {
				let Some(viewport) = gui.viewports.get(framed.viewport) else {
					continue;
				};
				let ViewportKind::Framed(frame) = &viewport.kind else {
					continue;
				};
				let picture = ViewRequest {
					width: framed.size.0,
					height: framed.size.1,
					camera: super::camera::CameraSpec::CFrame { components: frame.camera, fov: frame.fov, near: None },
					background: super::Background::Transparent,
					msaa: request.msaa,
					supersample: 1,
					bloom: false,
					ids: false,
					layers: false,
					gui_scale: None,
					linear_scale: None,
					texture: true,
					opaque_share: false,
					// a ViewportFrame's world at full detail (not measured)
					lod: None,
					quality: None,
				};
				match self.render(&viewport.scene, std::slice::from_ref(&picture)) {
					Ok((mut images, stats)) => {
						warnings.extend(stats.warnings);
						if let Some(texture) = images.pop().and_then(|image| image.texture) {
							let view = texture.create_view(&Default::default());
							worlds.entry(index).or_default().pictures.push(Picture { texture, view, framed: at });
						}
					}
					Err(error) => warnings.push(format!("{}: a ViewportFrame's world not drawn ({error:#})", gui.path)),
				}
			}
			let windows = gui.viewports.iter().filter(|viewport| viewport.kind == ViewportKind::Window).count();
			let (Some(viewport), GuiKind::Surface(surface)) = (gui.viewports.iter().find(|viewport| viewport.kind == ViewportKind::Window), &gui.kind) else {
				continue;
			};
			if draw.overlay {
				warnings.push(format!("{}: an AlwaysOnTop window (its world not drawn)", gui.path));
				continue;
			}
			if windows > 1 {
				warnings.push(format!("{}: {windows} window ViewportFrames (the first drawn)", gui.path));
			}
			let (width, height) = (request.width.max(1), request.height.max(1));
			let camera = request.camera.place(bounds, width, height);
			let window = ViewRequest {
				width,
				height,
				camera: camera.spec(),
				background: super::Background::Transparent,
				msaa: request.msaa,
				supersample: 1,
				bloom: false,
				ids: false,
				layers: false,
				gui_scale: None,
				linear_scale: None,
				texture: true,
				opaque_share: true,
				// a ViewportFrame's world at full detail (not measured)
				lod: None,
				quality: None,
			};
			// ROR_WINDOWS_DEBUG=<dir>: each window's world as an image too
			if let Some(directory) = std::env::var_os("ROR_WINDOWS_DEBUG") {
				let readback = ViewRequest { texture: false, ..window.clone() };
				if let Ok((images, _)) = self.render(&viewport.scene, std::slice::from_ref(&readback))
					&& let Some(image) = images.first()
				{
					let name = gui.path.replace(['.', '/', '\\', ' ', '"', '[', ']'], "_");
					let _ = crate::output::write_png(&std::path::Path::new(&directory).join(format!("{name}.png")), image.width, image.height, &image.rgba, true);
				}
			}
			match self.render(&viewport.scene, std::slice::from_ref(&window)) {
				Ok((mut images, stats)) => {
					warnings.extend(stats.warnings);
					if let Some(image) = images.pop()
						&& let Some(texture) = image.texture
					{
						let view = texture.create_view(&Default::default());
						let share_view = image.opaque_share.as_ref().map(|share| share.create_view(&Default::default()));
						worlds.entry(index).or_default().window = Some(Window { texture, view, share: image.opaque_share, share_view, rect: viewport.rect(surface.canvas), tint: viewport.tint, corner: viewport.corner(surface.canvas) });
					}
				}
				Err(error) => warnings.push(format!("{}: its window's world not drawn ({error:#})", gui.path)),
			}
		}
		worlds
	}

	/// A view's GUI quads with their textures (draws without a texture are left out), and the worlds they
	/// show ([`Renderer::render_worlds`], by the draw's index).
	pub(super) fn gui_geometry(&self, scene: &Scene, draws: &[GuiDraw], textures: &HashMap<GuiKey, Arc<GuiTexture>>, worlds: &HashMap<usize, Worlds>, lease: &super::reuse::Lease) -> ViewGuis {
		let mut vertices = Vec::new();
		let mut out = Vec::new();
		for (index, draw) in draws.iter().enumerate() {
			let Some(texture) = textures.get(&draw.key) else {
				continue;
			};
			let [u, v] = texture.uv;
			let uvs = [[0.0, 0.0], [u, 0.0], [0.0, v], [u, v]];
			let vertex = |index: usize| GuiVertex { position: draw.corners[index].into(), uv: uvs[index], normal: draw.normal.into(), params: draw.params };
			let start = vertices.len() as u32;
			vertices.extend_from_slice(&[vertex(0), vertex(2), vertex(1), vertex(1), vertex(2), vertex(3)]);
			// a frame's rect from canvas fractions to the quad's uv (the quad spans the texture's extent,
			// margins included, over 0..u and 0..v)
			let [left, top, right, bottom] = draw.key.extent();
			let to_uv = |[x0, y0, x1, y1]: [f32; 4]| {
				let to_u = |fraction: f32| (fraction - left) / (right - left).max(1e-6) * u;
				let to_v = |fraction: f32| (fraction - top) / (bottom - top).max(1e-6) * v;
				[to_u(x0), to_v(y0), to_u(x1), to_v(y1)]
			};
			let corner_uv = |[across, down]: [f32; 2]| [across / (right - left).max(1e-6) * u, down / (bottom - top).max(1e-6) * v, 0.0, 0.0];
			let world_group = |label: &str, uniform: &WindowUniform, view: &wgpu::TextureView, share: Option<&wgpu::TextureView>| {
				let buffer = lease.buffer("gui window", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(uniform));
				self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
					label: Some(label),
					layout: &self.guis.window_layout,
					entries: &[
						wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
						wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.guis.window_sampler) },
						wgpu::BindGroupEntry { binding: 2, resource: buffer.as_entire_binding() },
						wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(share.unwrap_or(&self.guis.blank)) },
					],
				})
			};
			let shown = worlds.get(&index);
			let window = shown.and_then(|worlds| worlds.window.as_ref()).map(|window| {
				let uniform = WindowUniform { rect: to_uv(window.rect), tint: window.tint, corner: corner_uv(window.corner), ..Default::default() };				world_group("gui window", &uniform, &window.view, window.share_view.as_ref())
			});
			// each framed ViewportFrame's picture with what draws over it, in the order the GUI draws them
			let mut layers = Vec::new();
			for picture in shown.map(|worlds| worlds.pictures.as_slice()).unwrap_or_default() {
				let framed = &draw.framed[picture.framed];
				let Some(viewport) = scene.guis.get(draw.gui).and_then(|gui| gui.viewports.get(framed.viewport)) else {
					continue;
				};
				let corner = corner_uv(viewport.corner(draw.key.canvas()));
				let mut uniform = WindowUniform { rect: to_uv(framed.rect), tint: viewport.tint, plane: [framed.plane[0], framed.plane[1], framed.plane[2], 0.0], mode: [1.0, 0.0, 0.0, 0.0], corner, ..Default::default() };
				if let ViewportKind::Framed(crate::scene::guis::Framed { gradient: Some(gradient), .. }) = &viewport.kind {
					uniform.mode = [1.0, gradient.colors.len() as f32, gradient.transparency.len() as f32, 0.0];
					for (slot, (time, color)) in uniform.colors.iter_mut().zip(&gradient.colors) {
						*slot = [*time, color[0], color[1], color[2]];
					}
					for (slot, (time, value)) in uniform.transparency.iter_mut().zip(&gradient.transparency) {
						*slot = [*time, *value, 0.0, 0.0];
					}
				}
				// what draws over it: its own image (a missing one, rur having failed, as nothing)
				layers.push((framed.over.and_then(|key| textures.get(&key)).cloned(), world_group("gui framed", &uniform, &picture.view, None)));
			}
			out.push(ViewGui { texture: texture.clone(), window, layers, vertices: start..vertices.len() as u32, center: draw.center, z_offset: draw.z_offset, overlay: draw.overlay, screen: draw.screen });
		}
		let buffer = (!vertices.is_empty()).then(|| lease.buffer("guis", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&vertices)));
		ViewGuis { buffer, draws: out }
	}

	/// Draws one GUI of `view` into the scene pass (the frame bind group is set).
	pub(super) fn draw_gui(&self, pass: &mut wgpu::RenderPass<'_>, view: &ViewGuis, draw: &ViewGui, samples: u32) {
		let Some(buffer) = &view.buffer else {
			return;
		};
		pass.set_pipeline(self.guis.scene(samples));
		pass.set_vertex_buffer(0, buffer.slice(..));
		pass.set_bind_group(1, &draw.texture.group, &[]);
		pass.set_bind_group(2, draw.window.as_ref().unwrap_or(&self.guis.no_window), &[]);
		pass.draw(draw.vertices.clone(), 0..1);
		self.draw_layers(pass, draw);
	}

	/// A GUI draw's framed ViewportFrames, each over what is drawn of it so far (the same quad again).
	fn draw_layers(&self, pass: &mut wgpu::RenderPass<'_>, draw: &ViewGui) {
		for (over, picture) in &draw.layers {
			pass.set_bind_group(1, over.as_ref().map_or(&self.guis.no_texture, |over| &over.group), &[]);
			pass.set_bind_group(2, picture, &[]);
			pass.draw(draw.vertices.clone(), 0..1);
		}
	}

	/// Encodes the AlwaysOnTop GUIs and ScreenGuis of `view` into a layer of their own (premultiplied,
	/// the image's size, [`overlay_order`]) and its copy into a readback buffer: [`composite_overlays`]
	/// lays it over the finished image after the Highlights. `None` without such GUIs.
	#[allow(clippy::too_many_arguments)]
	pub(super) fn encode_gui_overlays(&self, encoder: &mut wgpu::CommandEncoder, prepared: &Prepared, view: &ViewGuis, camera: &Camera, width: u32, height: u32, profile: &super::profile::Profile) -> Option<Readback> {
		let buffer = view.buffer.as_ref()?;
		let mut overlays: Vec<&ViewGui> = view.draws.iter().filter(|draw| draw.overlay).collect();
		if overlays.is_empty() {
			return None;
		}
		overlays.sort_by(|a, b| overlay_order((a.screen, camera.depth(a.center)), (b.screen, camera.depth(b.center))));
		let device = &self.gpu.device;
		let frame = self.frame_uniform(Some(camera), width, height, 0);
		let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("gui overlay frame"), contents: bytemuck::bytes_of(&frame), usage: wgpu::BufferUsages::UNIFORM });
		let frame_group = self.frame_bind_group(prepared, &frame_buffer, &self.no_light_bindings(prepared), &self.defaults.empty_moments, &self.defaults.empty_shadow, &self.defaults.white.view, &self.defaults.black.view);
		let texture = device.create_texture(&wgpu::TextureDescriptor {
			label: Some("gui overlay"),
			size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: OUTPUT_FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
			view_formats: &[],
		});
		{
			let target = texture.create_view(&Default::default());
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("gui overlay"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &target,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("gui overlay"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&self.guis.overlay);
			pass.set_bind_group(0, &frame_group, &[]);
			pass.set_bind_group(2, &self.guis.no_window, &[]);
			pass.set_vertex_buffer(0, buffer.slice(..));
			for draw in overlays {
				pass.set_bind_group(1, &draw.texture.group, &[]);
				pass.set_bind_group(2, &self.guis.no_window, &[]);
				pass.draw(draw.vertices.clone(), 0..1);
				self.draw_layers(&mut pass, draw);
			}
		}
		let bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
		let readback = Readback {
			buffer: device.create_buffer(&wgpu::BufferDescriptor {
				label: Some("gui overlay"),
				size: bytes_per_row as u64 * height as u64,
				usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
				mapped_at_creation: false,
			}),
			bytes_per_row,
		};
		copy_out(encoder, &texture, &readback, width, height);
		Some(readback)
	}
}

/// The order of the overlay layer's GUIs (each its ScreenGui DisplayOrder, its depth along the
/// camera's forward axis, Camera::depth), first drawn first: the world's AlwaysOnTop GUIs far to near, then the ScreenGuis by
/// DisplayOrder, the higher over (screen-gui-probes: DisplayOrder 2 over 1); a tie keeps the scene's order.
fn overlay_order(a: (Option<i32>, f32), b: (Option<i32>, f32)) -> std::cmp::Ordering {
	match (a.0, b.0) {
		(None, None) => b.1.total_cmp(&a.1),
		(None, Some(_)) => std::cmp::Ordering::Less,
		(Some(_), None) => std::cmp::Ordering::Greater,
		(Some(a), Some(b)) => a.cmp(&b),
	}
}

/// Lays the AlwaysOnTop GUIs' and ScreenGuis' layer (premultiplied RGBA8) over the finished
/// straight-alpha image, in display values as the GPU would blend them (screen-gui-probes: black at
/// BackgroundTransparency 0.5 over 255 / 200, 100, 50 / 128 reads 127 / 100, 50, 25 / 64, over the post
/// effects).
pub(super) fn composite_overlays(rgba: &mut [u8], layer: &[u8]) {
	// in pieces on every core (each pixel its own)
	const PIECE: usize = 64 << 10;
	rgba.par_chunks_mut(PIECE).zip(layer.par_chunks(PIECE)).for_each(|(rgba, layer)| composite_overlay_piece(rgba, layer));
}

fn composite_overlay_piece(rgba: &mut [u8], layer: &[u8]) {
	for (pixel, over) in rgba.chunks_exact_mut(4).zip(layer.chunks_exact(4)) {
		if over[3] == 0 {
			continue;
		}
		let coverage = over[3] as f32 / 255.0;
		let below = pixel[3] as f32 / 255.0 * (1.0 - coverage);
		let alpha = coverage + below;
		for channel in 0..3 {
			let premultiplied = over[channel] as f32 / 255.0 + pixel[channel] as f32 / 255.0 * below;
			pixel[channel] = (premultiplied / alpha * 255.0).round().clamp(0.0, 255.0) as u8;
		}
		pixel[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::gui_files::gui_image;

	#[test]
	fn gui_images_keep_their_levels_through_files_and_pieces() {
		// odd sides and several pieces
		let (width, height) = (601u32, 377u32);
		let rgba: Vec<u8> = (0..width * height * 4).map(|index| (index.wrapping_mul(2654435761) >> 13) as u8).collect();
		let expected = gui_image(width, height, rgba.clone());
		assert_eq!(expected.levels.len(), 1, "no mips: a GUI is read at its 4x MSAA sample positions, level 0 alone");
		let image = GuiImage::from_rgba(width, height, rgba).unwrap();
		assert!(image.ends.len() > 1);
		let read = GuiImage::from_file(&image.file_bytes()).unwrap();
		assert!(GuiImage::from_file(&image.file_bytes()[..30]).is_none(), "a cut file is no image");
		let whole = image.file_bytes();
		assert!(GuiImage::from_file(&whole[..whole.len() - 1]).is_none(), "nor one a byte short");
		assert!(GuiImage::from_file(&[&whole[..], &[0]].concat()).is_none(), "nor one a byte long");
		let mut unpacked = vec![0u8; read.size() as usize];
		let mut rest = &mut unpacked[..];
		for (piece, start) in read.pieces() {
			let length = GUI_CHUNK.min(read.size() as usize - start);
			let (place, more) = rest.split_at_mut(length);
			GuiImage::unpack(piece, place).unwrap();
			rest = more;
		}
		let (layout, _) = GuiImage::layout(width, height, read.levels);
		assert_eq!(layout.len(), expected.levels.len());
		for ((w, h, row, offset), level) in layout.into_iter().zip(&expected.levels) {
			for y in 0..h as usize {
				let start = offset as usize + y * row as usize;
				assert_eq!(&unpacked[start..start + w as usize * 4], &level[y * w as usize * 4..(y + 1) * w as usize * 4], "level {w}x{h} row {y}");
			}
		}
	}

	#[test]
	fn keys_quantize_and_cap() {
		let key = GuiKey::new(7, [800.0, 600.0], 1.5, true);
		assert_eq!(key.canvas(), [800.0, 600.0]);
		assert_eq!(key.scale(), 1.5);
		assert_eq!(key.margin(), [0, 0]);
		assert_eq!(key.extent(), [0.0, 0.0, 1.0, 1.0]);
		// big canvases fit the largest texture, margins included
		for (canvas, clips) in [([4000.0, 4000.0], true), ([3000.0, 500.0], false), ([1400.0, 1400.0], false)] {
			let key = GuiKey::new(7, canvas, 1.5, clips);
			let (width, height) = rur::image_size(key.canvas(), key.scale(), key.margin());
			assert!(width.max(height) as f32 <= MAX_TEXTURE, "{canvas:?}: {width} x {height}");
		}
		assert_ne!(GuiKey::new(7, [100.0, 50.0], 1.5, true), GuiKey::new(7, [100.5, 50.0], 1.5, true));
		// a ScreenGui over a 4K view at GUI scale 1 keeps its own pixels, within one of rur's atlases
		let key = GuiKey::within(7, [3840.0, 2160.0], 1.0, true, MAX_SCREEN_TEXTURE);
		assert_eq!(key.scale(), 1.0);
		let (width, height) = rur::image_size(key.canvas(), key.scale(), key.margin());
		assert!(width as f32 <= MAX_SCREEN_TEXTURE && height == 2160, "{width} x {height}");
	}

	#[test]
	fn batches_are_made_frame_by_frame() {
		let gui = |path: &str| Gui {
			path: path.into(),
			kind: GuiKind::Billboard(crate::scene::guis::BillboardGui { anchor: Vec3::ZERO, extents: Vec3::ZERO, size: [0.0; 4], size_offset: [0.0; 2], camera_offset: Vec3::ZERO, world_offset: Vec3::ZERO }),
			content: Arc::new(Vec::new()),
			base_content_hash: 0,
			content_hash: 0,
			local_files: Vec::new(),
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			max_distance: 0.0,
			clips: true,
			z_index_behavior: 1,
			viewports: Vec::new(),
		};
		let guis: Vec<Gui> = (0..9).map(|index| gui(&format!("Gui{index}"))).collect();
		let content = Arc::new(Vec::new());
		let snaps: GuiSnaps = Arc::from([]);
		// 1000 x 1000 canvases at 1.5: 2.25 M device pixels, one to a batch of BATCH_PIXELS (4.2 M)
		let big = |content| GuiKey::new(content, [1000.0, 1000.0], 1.5, true);
		let small = |content| GuiKey::new(content, [100.0, 100.0], 1.5, true);
		let other_scale = |content| GuiKey::new(content, [100.0, 100.0], 2.0, true);
		let images = vec![
			(0, small(1), &guis[0], &content, &snaps),
			(0, other_scale(2), &guis[1], &content, &snaps),
			(0, small(3), &guis[2], &content, &snaps),
			(1, small(4), &guis[3], &content, &snaps),
			(1, big(5), &guis[4], &content, &snaps),
			(1, big(6), &guis[5], &content, &snaps),
			(3, small(7), &guis[6], &content, &snaps),
			// a GUI's own image and what draws over its ViewportFrame: one batch while they fit two
			(4, big(8), &guis[7], &content, &snaps),
			(4, big(9), &guis[7], &content, &snaps),
			(4, big(10), &guis[8], &content, &snaps),
		];
		let batches = frame_batches(images);
		let contents: Vec<Vec<u64>> = batches.iter().map(|(_, members, _)| members.iter().map(|(key, _, _, _)| key.content).collect()).collect();
		// a frame's GUIs never share a batch with another frame's, however much room is left
		assert_eq!(contents, vec![vec![1, 3], vec![2], vec![4, 5], vec![6], vec![7], vec![8, 9], vec![10]]);
		assert!(batches.iter().all(|(_, members, pixels)| *pixels <= BATCH_PIXELS || (*pixels <= 2 * BATCH_PIXELS && members.iter().all(|(_, gui, _, _)| std::ptr::eq(*gui, members[0].1)))));
	}

	#[test]
	fn a_framed_viewport_frame_is_planned_at_its_size_on_the_screen_under_what_draws_over_it() {
		// an emote's portrait: a BillboardGui 8 x 6 studs whose ViewportFrame fills 0.9 x 0.8 of it,
		// centred, a label over it
		use crate::render::camera::CameraSpec;
		use crate::scene::guis::{BillboardGui, Framed, Placement, Viewport};

		let billboard = BillboardGui { anchor: Vec3::ZERO, extents: Vec3::ZERO, size: [8.0, 0.0, 6.0, 0.0], size_offset: [0.0; 2], camera_offset: Vec3::ZERO, world_offset: Vec3::ZERO };
		let frame = Placement { size: [0.9, 0.0, 0.8, 0.0], position: [0.5, 0.0, 0.5, 0.0], anchor: [0.5, 0.5] };
		let framed = Framed { camera: [0.0, 0.0, 8.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], fov: 40.0, over: Some((Arc::new(vec![1]), 99)), over_local_files: Vec::new(), gradient: None };
		let gui = Gui {
			path: "Billboard".into(),
			kind: GuiKind::Billboard(billboard),
			content: Arc::new(Vec::new()),
			base_content_hash: 7,
			content_hash: 7,
			local_files: Vec::new(),
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			max_distance: 0.0,
			clips: false,
			z_index_behavior: 1,
			viewports: vec![Viewport { placement: vec![frame], tint: [1.0; 4], scene: Arc::new(Scene::default()), kind: ViewportKind::Framed(framed), corner_radius: None }],
		};
		let scene = Scene { guis: vec![gui], ..Default::default() };
		let request = ViewRequest { width: 1280, height: 720, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -20.0], target: [0.0; 3], fov: 40.0 }, ..Default::default() };
		let camera = request.camera.place(None, request.width, request.height);
		let draws = plan(&scene, &camera, &request);
		let [framed] = draws[0].framed.as_slice() else {
			panic!("one framed ViewportFrame");
		};
		assert!(framed.rect.iter().zip([0.05, 0.1, 0.95, 0.9]).all(|(a, b)| (a - b).abs() < 1e-5), "{:?}", framed.rect);
		// as tall as the frame shows (0.8 of 6 studs 20 studs off at a 40-degree field: 0.8 of 297 pixels),
		// at its own proportions (0.9 x 8 by 0.8 x 6 studs)
		let tall = 0.8 * (6.0 * 720.0 * 0.5 / 20f32.to_radians().tan() / 20.0).round();
		assert_eq!(framed.size, ((tall.round() * 7.2 / 4.8).round() as u32, tall.round() as u32));
		// what draws over it: an image of its own, the GUI's canvas and scale
		assert_eq!(framed.over, Some(GuiKey { content: 99, ..draws[0].key }));
		assert_eq!(draws[0].keys().collect::<Vec<_>>(), [draws[0].key, GuiKey { content: 99, ..draws[0].key }]);
	}

	#[test]
	fn a_screen_guis_viewport_frame_is_planned_at_its_logical_size() {
		// a ScreenGui's ViewportFrame over the whole view and one over its right half, below the top bar in
		// a recording of play: each picture the frame's size in GUI pixels (its logical size, stretched over
		// the frame), at its proportions however wide
		use crate::render::camera::CameraSpec;
		use crate::scene::guis::{Framed, Placement, ScreenGui, Viewport};

		let framed = |size: [f32; 4], position: [f32; 4]| Viewport {
			placement: vec![Placement { size, position, anchor: [0.0, 0.0] }],
			tint: [1.0; 4],
			scene: Arc::new(Scene::default()),
			kind: ViewportKind::Framed(Framed { camera: [0.0, 0.0, 8.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], fov: 40.0, over: None, over_local_files: Vec::new(), gradient: None }),
			corner_radius: None,
		};
		let gui = |inset: bool| Gui {
			path: "PlayerGui.View".into(),
			kind: GuiKind::Screen(ScreenGui { display_order: -5, area: if inset { ScreenArea::BelowTopBar } else { ScreenArea::Whole } }),
			content: Arc::new(Vec::new()),
			base_content_hash: 7,
			content_hash: 7,
			local_files: Vec::new(),
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			max_distance: 0.0,
			clips: true,
			z_index_behavior: 1,
			viewports: vec![framed([1.0, 0.0, 1.0, 0.0], [0.0; 4]), framed([0.5, 0.0, 1.0, 0.0], [0.5, 0.0, 0.0, 0.0])],
		};
		// (744 GUI pixels tall: 1440 device pixels are 1.93 a GUI pixel, 112 of them the top bar's in play)
		for (width, height, inset, whole, half) in [(2560, 1440, false, (1323, 744), (661, 744)), (2560, 1440, true, (1323, 686), (661, 686)), (5120, 1440, false, (2048, 576), (1323, 744))] {
			let scene = Scene { guis: vec![gui(inset)], play: true, ..Default::default() };
			let request = ViewRequest { width, height, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -20.0], target: [0.0; 3], fov: 40.0 }, ..Default::default() };
			let camera = request.camera.place(None, request.width, request.height);
			let draws = plan(&scene, &camera, &request);
			let [draw] = draws.as_slice() else {
				panic!("one ScreenGui");
			};
			let sizes: Vec<(u32, u32)> = draw.framed.iter().map(|framed| framed.size).collect();
			// (5120 x 1440: the whole frame's 2646 x 744 GUI pixels drawn 2048 x 576, its proportions kept)
			assert_eq!(sizes, [whole, half], "{width} x {height}, inset {inset}");
			assert!(draw.framed[1].rect.iter().zip([0.5, 0.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-5), "{:?}", draw.framed[1].rect);
			assert!(draw.overlay && draw.screen == Some(-5));
		}
	}

	/// A ScreenGui's ViewportFrame over the whole screen, as a portal arm is drawn: its world through its
	/// own camera, laid over the finished image at its ScreenGui's place among the others by DisplayOrder;
	/// a mesh at vertex alpha 0 hides what lies behind it in that world and is a hole through the picture
	/// to what is under the ScreenGui. Before, the frame drew its background alone (nothing here).
	#[test]
	fn a_screen_guis_viewport_frame_draws_its_world_over_the_screen() {
		use crate::render::camera::CameraSpec;
		use crate::scene::build::{ModelBuilder, PartSpec};
		use rbx_dom_weak::InstanceBuilder;
		use rbx_dom_weak::types::{CFrame, Color3, Content, Enum, Matrix3, UDim, UDim2, Variant, Vector3};

		let state = std::env::temp_dir().join(format!("ror-screen-viewport-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mask_mesh = state.join("mask.mesh");
		std::fs::write(&mask_mesh, super::super::vertex_colour_tests::quad_mesh([0, 0, 0, 0])).unwrap();
		let mask_uri = format!("file:///{}", mask_mesh.display().to_string().replace('\\', "/"));
		let whole = UDim2::new(UDim::new(1.0, 0), UDim::new(1.0, 0));
		let (width, height) = (128u32, 64u32);
		// a cover over the screen's top half in a ScreenGui at `cover_order`; the frame's at -5
		let draw = |cover_order: i32| -> Vec<u8> {
			let mut model = ModelBuilder::new("Scene");
			let player_gui = model.child(model.model, InstanceBuilder::new("PlayerGui").with_name("PlayerGui"));
			let view = model.child(player_gui, InstanceBuilder::new("ScreenGui").with_name("View").with_property("ScreenInsets", Enum::from_u32(1)).with_property("DisplayOrder", -5i32));
			let frame = model.child(
				view,
				InstanceBuilder::new("ViewportFrame").with_property("Size", whole).with_property("BackgroundTransparency", 1.0f32).with_property("Ambient", Color3::new(1.0, 1.0, 1.0)).with_property("LightColor", Color3::new(0.0, 0.0, 0.0)),
			);
			let world = model.child(frame, InstanceBuilder::new("WorldModel"));
			// a red plate 4 studs square at the origin, the mask's quad over its +X half a stud nearer
			let plate = model.part(&PartSpec { name: "Plate".into(), size: glam::Vec3::new(4.0, 4.0, 0.2), color: [255, 0, 0], material: 272, cast_shadow: false, ..Default::default() });
			let mask = model.part(&PartSpec { class: "MeshPart", name: "Mask".into(), cframe: glam::Affine3A::from_translation(glam::Vec3::new(2.0, 0.0, 1.0)), size: glam::Vec3::new(4.0, 4.0, 0.05), color: [0, 0, 0], material: 272, cast_shadow: false, ..Default::default() });
			model.set(mask, "MeshContent", Content::from_uri(mask_uri.as_str()));
			model.set(mask, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
			for part in [plate, mask] {
				model.dom.transfer_within(part, world);
			}
			let camera = model.child(frame, InstanceBuilder::new("Camera").with_property("CFrame", CFrame::new(Vector3::new(0.0, 0.0, 10.0), Matrix3::identity())).with_property("FieldOfView", 40.0f32));
			model.set(frame, "CurrentCamera", Variant::Ref(camera));
			model.child(player_gui, InstanceBuilder::new("ScreenGui").with_name("Cover").with_property("ScreenInsets", Enum::from_u32(1)).with_property("DisplayOrder", cover_order));
			let scene = crate::scene::extract::extract(&model.into_document());
			assert_eq!(scene.guis.len(), 2, "{:?}", scene.warnings);
			// (a GUI pixel a pixel: the frame's picture the view's size)
			let request = ViewRequest { width, height, camera: CameraSpec::LookAt { eye: [0.0, 0.0, 10.0], target: [0.0; 3], fov: 40.0 }, background: super::super::Background::Color([0.0, 0.0, 1.0]), bloom: false, gui_scale: Some(1.0), ..Default::default() };
			let camera = request.camera.place(None, width, height);
			// rur's images of the two ScreenGuis: the frame's without a background, transparent; the cover's
			// top half opaque white
			let mut images = Vec::new();
			for draw in plan(&scene, &camera, &request) {
				let (columns, rows) = rur::image_size(draw.key.canvas(), draw.key.scale(), draw.key.margin());
				let cover = scene.guis[draw.gui].path.ends_with("Cover");
				let rgba: Vec<u8> = (0..rows).flat_map(|row| (0..columns).flat_map(move |_| if cover && row < rows / 2 { [255u8; 4] } else { [0u8; 4] })).collect();
				images.push((draw.key, Arc::new(GuiImage::from_rgba(columns, rows, rgba).unwrap()), false));
			}
			let (mut textures, mut warnings, mut skipped) = (HashMap::new(), Vec::new(), HashMap::new());
			renderer.upload_guis(images, &mut textures, &mut warnings, &mut skipped);
			assert!(warnings.is_empty(), "{warnings:?}");
			let (pending, mut stats) = renderer.submit_views(&scene, std::slice::from_ref(&request), None, None, Some((textures, skipped)), None, None).unwrap();
			let image = pending.into_iter().next().unwrap().finish(&mut stats).unwrap();
			// (the world itself is empty: the screen is the background's blue)
			assert!(stats.warnings.iter().all(|warning| warning.starts_with("nothing to frame")), "{:?}", stats.warnings);
			image.rgba
		};
		// a point of the frame's world on the screen (its camera is the view's): one stud is 8.8 pixels
		let focal = height as f32 * 0.5 / 20f32.to_radians().tan();
		let at = |image: &[u8], x: f32, y: f32| -> [u8; 3] {
			let (column, row) = ((width as f32 * 0.5 + x * focal / 10.0) as u32, (height as f32 * 0.5 - y * focal / 10.0) as u32);
			let index = ((row * width + column) * 4) as usize;
			[image[index], image[index + 1], image[index + 2]]
		};
		let red = |pixel: [u8; 3]| pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40;
		// the background's blue through the tone curve (184 of 255)
		let blue = |pixel: [u8; 3]| pixel[0] < 40 && pixel[1] < 40 && pixel[2] > 150;
		let white = |pixel: [u8; 3]| pixel.iter().all(|value| *value > 215);
		for (cover_order, cover_over) in [(0, true), (-10, false)] {
			let image = draw(cover_order);
			let case = format!("cover at DisplayOrder {cover_order}");
			// the plate where no mask hides it, over the screen; beside it and where the mask hides it the
			// screen shows through
			assert!(red(at(&image, -1.0, -1.0)), "{case}: the plate {:?}", at(&image, -1.0, -1.0));
			assert!(blue(at(&image, 1.0, -1.0)), "{case}: the mask's hole {:?}", at(&image, 1.0, -1.0));
			assert!(blue(at(&image, -3.0, -1.0)), "{case}: beside the plate {:?}", at(&image, -3.0, -1.0));
			// the top half: the cover over the picture, or under it and seen through the mask's hole
			let plate = at(&image, -1.0, 1.0);
			assert!(if cover_over { white(plate) } else { red(plate) }, "{case}: the plate under the cover {plate:?}");
			assert!(white(at(&image, 1.0, 1.0)), "{case}: the mask's hole over the cover {:?}", at(&image, 1.0, 1.0));
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn overflow_margins() {
		// a quarter of the canvas and 8 GUI pixels on each side, in whole device pixels
		let key = GuiKey::new(7, [200.0, 100.0], 1.5, false);
		assert_eq!(key.margin(), [87, 50]);
		let [left, top, right, bottom] = key.extent();
		assert!((left + 87.0 / 300.0).abs() < 1e-6 && (top + 50.0 / 150.0).abs() < 1e-6);
		assert!((right - 1.0 - 87.0 / 300.0).abs() < 1e-6 && (bottom - 1.0 - 50.0 / 150.0).abs() < 1e-6);
		assert_eq!(rur::image_size(key.canvas(), key.scale(), key.margin()), (300 + 174, 150 + 100));
	}

	#[test]
	fn big_billboards_keep_their_proportions() {
		// a billboard too big for a texture at the image's scale: its texture is drawn at a smaller
		// scale, and the quad still gives the canvas its whole pixels and the margins their share
		use crate::render::camera::CameraSpec;
		use crate::scene::guis::BillboardGui;

		let billboard = BillboardGui { anchor: Vec3::ZERO, extents: Vec3::ZERO, size: [24.0, 0.0, 12.0, 0.0], size_offset: [0.0; 2], camera_offset: Vec3::ZERO, world_offset: Vec3::ZERO };
		let gui = Gui {
			path: "Billboard".into(),
			kind: GuiKind::Billboard(billboard),
			content: Arc::new(Vec::new()),
			base_content_hash: 7,
			content_hash: 7,
			local_files: Vec::new(),
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			max_distance: 0.0,
			clips: false,
			z_index_behavior: 1,
			viewports: Vec::new(),
		};
		let scene = Scene { guis: vec![gui], ..Default::default() };
		let request = ViewRequest { width: 1644, height: 1116, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -8.0], target: [0.0; 3], fov: 70.0 }, ..Default::default() };
		let camera = request.camera.place(None, request.width, request.height);
		let draws = plan(&scene, &camera, &request);
		let key = draws[0].key;
		assert!(key.scale() < 1.5, "{}", key.scale());
		let focal = 1116.0 * 0.5 / 35f32.to_radians().tan();
		let quad = (draws[0].corners[1] - draws[0].corners[0]).length() * focal / 8.0;
		let canvas = (24.0 * focal / 8.0).round();
		let texture_canvas = key.canvas()[0] * key.scale();
		let texture = texture_canvas + 2.0 * key.margin()[0] as f32;
		assert!((canvas / quad - texture_canvas / texture).abs() < 1e-3, "{canvas} of {quad} on screen, {texture_canvas} of {texture} in the texture");
	}

	#[test]
	fn screen_guis_cover_the_view_below_the_top_bar() {
		// a fade over the whole view and a HUD below the top bar, in a recording of play (58 GUI pixels)
		// and in Studio's edit viewport (none); drawn 1:1 into the image's pixels, in the overlay layer,
		// at the view's GUI scale (744 GUI pixels tall, as billboards)
		use crate::render::camera::CameraSpec;
		use crate::scene::guis::ScreenGui;

		let screen = |name: &str, display_order: i32, area: ScreenArea| Gui {
			path: format!("PlayerGui.{name}"),
			kind: GuiKind::Screen(ScreenGui { display_order, area }),
			content: Arc::new(Vec::new()),
			base_content_hash: display_order as u64,
			content_hash: display_order as u64,
			local_files: Vec::new(),
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			max_distance: 0.0,
			clips: false,
			z_index_behavior: 1,
			viewports: Vec::new(),
		};
		let request = ViewRequest { width: 1572, height: 862, supersample: 2, camera: CameraSpec::LookAt { eye: [2040.0, 2050.0, 2000.0], target: [2048.0; 3], fov: 70.0 }, ..Default::default() };
		let camera = request.camera.place(None, request.width, request.height);
		let pixel = |point: Vec3| {
			let clip = camera.view_projection(request.width as f32 / request.height as f32) * point.extend(1.0);
			((clip.x / clip.w + 1.0) * 0.5 * request.width as f32, (1.0 - clip.y / clip.w) * 0.5 * request.height as f32)
		};
		// and a bar inside the top bar (TopbarSafeInsets: in a client the top 58 GUI pixels; in Studio's edit
		// viewport, which has none, the whole view)
		for play in [true, false] {
			let scene = Scene { guis: vec![screen("Transition", 3, ScreenArea::Whole), screen("Hud", 0, ScreenArea::BelowTopBar), screen("Bar", 1, ScreenArea::TopBar)], play, ..Default::default() };
			let draws = plan(&scene, &camera, &request);
			assert_eq!(draws.len(), 3);
			let scale = draws[0].key.scale();
			assert!((scale - 862.0 / 744.0).abs() < 1e-3 && draws.iter().all(|draw| draw.overlay && draw.key.clips), "the view's scale, whatever the supersampling");
			assert_eq!((draws[0].screen, draws[1].screen, draws[2].screen), (Some(3), Some(0), Some(1)));
			let bar = if play { 58.0 * scale } else { 0.0 };
			for (draw, top, bottom) in [(&draws[0], 0.0, 862.0), (&draws[1], bar, 862.0), (&draws[2], 0.0, if play { bar } else { 862.0 })] {
				let [width, height] = draw.key.canvas().map(|size| size * scale);
				assert!((width - 1572.0).abs() < 0.01 && (height - (bottom - top)).abs() < 0.01, "canvas {width} x {height} at play {play}");
				let (left, upper) = pixel(draw.corners[0]);
				let (right, lower) = pixel(draw.corners[3]);
				assert!(left.abs() < 0.02 && (upper - top).abs() < 0.02 && (right - 1572.0).abs() < 0.02 && (lower - bottom).abs() < 0.02, "{left},{upper} - {right},{lower}");
			}
		}
		// the world's AlwaysOnTop GUIs far to near, then the ScreenGuis by DisplayOrder (a tie keeps the scene's order)
		let mut layer = vec![(Some(2), 0.0), (None, 4.0), (Some(1), 0.0), (None, 9.0), (Some(1), 1.0)];
		layer.sort_by(|a, b| overlay_order(*a, *b));
		assert_eq!(layer, vec![(None, 9.0), (None, 4.0), (Some(1), 0.0), (Some(1), 1.0), (Some(2), 0.0)]);
	}

	/// A Scene kept warm in SceneCache is not re-extracted when a local PNG referenced by one of its
	/// SurfaceGuis or BillboardGuis is rewritten on disk: `plan()` must re-stamp local files live so
	/// the GuiKey changes, and `rur::stage_local_files` must mark a moving file (A -> B -> A between
	/// plan and read) unstable so it is never kept under A's key.
	#[test]
	fn plan_stamps_local_gui_images_live_and_stage_rejects_moving_file() {
		use crate::render::camera::CameraSpec;
		use crate::scene::build::{ModelBuilder, PartSpec};
		use rbx_dom_weak::InstanceBuilder;
		use rbx_dom_weak::types::{Content, Enum, UDim, UDim2};

		let dir = std::env::temp_dir().join(format!("ror-gui-live-plan-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let sign = dir.join("sign.png");
		let png_a = b"PNG_BYTES_VERSION_A_1234";
		let png_b = b"PNG_BYTES_VERSION_B_9876";
		std::fs::write(&sign, png_a).unwrap();
		let sign_uri = format!("file:///{}", sign.display().to_string().replace('\\', "/"));

		let mut model = ModelBuilder::new("SignModel");
		let part = model.part(&PartSpec { size: Vec3::new(4.0, 4.0, 1.0), ..Default::default() });
		let surface = model.child(part, InstanceBuilder::new("SurfaceGui").with_name("FrontSign").with_property("Face", Enum::from_u32(5)));
		model.child(surface, InstanceBuilder::new("ImageLabel").with_property("ImageContent", Content::from_uri(sign_uri.as_str())));
		let billboard = model.child(part, InstanceBuilder::new("BillboardGui").with_name("HeadSign").with_property("Size", UDim2::new(UDim::new(4.0, 0), UDim::new(2.0, 0))));
		model.child(billboard, InstanceBuilder::new("ImageLabel").with_property("ImageContent", Content::from_uri(sign_uri.as_str())));
		let scene = crate::scene::extract::extract(&model.into_document());
		assert_eq!(scene.guis.len(), 2);

		let request = ViewRequest { width: 512, height: 512, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 40.0 }, ..Default::default() };
		let camera = request.camera.place(None, request.width, request.height);

		let draws_a = plan(&scene, &camera, &request);
		assert_eq!(draws_a.len(), 2);

		// Rewrite sign.png without re-extracting `scene` (as when SceneCache reuses it).
		std::fs::write(&sign, png_b).unwrap();
		let draws_b = plan(&scene, &camera, &request);
		assert_eq!(draws_b.len(), 2);
		for i in 0..2 {
			assert_ne!(draws_a[i].key, draws_b[i].key, "GUI {i} key must change live on same cached Scene");
		}

		// Deterministic A -> B -> A hook during staging: plan sees A, hook writes B before read,
		// so stage_local_files reads B while snapshot expects A -> stable must be false.
		std::fs::write(&sign, png_a).unwrap();
		let draws_for_race = plan(&scene, &camera, &request);
		let sign_for_hook = sign.clone();
		crate::render::set_test_read_hook(sign.clone(), move || {
			let _ = std::fs::write(&sign_for_hook, png_b);
		});
		let work = dir.join("work");
		let reqs: Vec<rur::Request> = draws_for_race
			.iter()
			.map(|d| rur::Request { content: scene.guis[d.gui].content.clone(), canvas: d.key.canvas(), margin: d.key.margin(), z_index_behavior: scene.guis[d.gui].z_index_behavior, local_snaps: d.local_snaps.clone() })
			.collect();
		let (staged_race, stable_race) = rur::stage_local_files(&reqs, &work);
		assert_eq!(stable_race, vec![false, false], "moving file between plan() and rur staging must be marked unstable");

		// Return to A without hook: now stable must be true, and the staged URI must differ from B's.
		std::fs::write(&sign, png_a).unwrap();
		let draws_settled = plan(&scene, &camera, &request);
		let reqs_settled: Vec<rur::Request> = draws_settled
			.iter()
			.map(|d| rur::Request { content: scene.guis[d.gui].content.clone(), canvas: d.key.canvas(), margin: d.key.margin(), z_index_behavior: scene.guis[d.gui].z_index_behavior, local_snaps: d.local_snaps.clone() })
			.collect();
		let (staged_a, stable_a) = rur::stage_local_files(&reqs_settled, &work);
		assert_eq!(stable_a, vec![true, true], "unchanged file must be marked stable");
		assert_ne!(staged_race.get(&sign), staged_a.get(&sign), "staged content-addressed path must change between B and A");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// A GUI that failed transiently during video prefetch (reported in `guis.1` without being
	/// pinned in `self.gui_failures`) must still be retried at draw time and clear its stale
	/// skip reason once recovered.
	#[test]
	fn draw_time_retries_gui_that_failed_transiently_during_prefetch() {
		use std::sync::Arc;

		use crate::render::camera::CameraSpec;
		use crate::render::{Background, Renderer, ViewRequest};
		use crate::scene::build::{ModelBuilder, PartSpec};
		use rbx_dom_weak::InstanceBuilder;
		use rbx_dom_weak::types::Enum;

		let dir = std::env::temp_dir().join(format!("ror-gui-retry-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let gpu = Arc::new(crate::render::gpu::Gpu::new().expect("a GPU (or software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&dir) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");

		let mut model = ModelBuilder::new("RetryGui");
		let part = model.part(&PartSpec { name: "Board".into(), size: Vec3::new(4.0, 4.0, 1.0), ..Default::default() });
		let gui = model.child(part, InstanceBuilder::new("SurfaceGui").with_name("Sign").with_property("Face", Enum::from_u32(5)));
		model.child(gui, InstanceBuilder::new("Frame").with_property("BackgroundTransparency", 0.0f32));
		let scene = crate::scene::extract::extract(&model.into_document());
		let view = ViewRequest {
			width: 64,
			height: 64,
			msaa: 1,
			bloom: false,
			background: Background::Color([0.0, 0.0, 0.0]),
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 50.0 },
			..Default::default()
		};
		let camera = view.camera.place(scene.bounds(), view.width, view.height);
		let draws = plan(&scene, &camera, &view);
		assert_eq!(draws.len(), 1);
		let key = draws[0].key;

		// Populate the renderer's GPU GUI cache as if the transient failure resolved before draw time.
		let image = Arc::new(GuiImage::from_rgba(4, 4, vec![255u8; 4 * 4 * 4]).unwrap());
		let mut textures = GuiTextures::new();
		let mut warn = Vec::new();
		let mut skipped = HashMap::new();
		renderer.upload_guis(vec![(key, image, true)], &mut textures, &mut warn, &mut skipped);
		assert!(renderer.gui_textures.get(&key).is_some());

		// Pass an empty ready map + a prefetch failure entry for `key`: draw-time retry must
		// pick up the recovered texture, draw the GUI, and leave `gui_report.skipped` empty.
		let prefetch_failed = (GuiTextures::new(), HashMap::from([(key, "transient prefetch error".to_string())]));
		let (_pending, stats) = renderer
			.submit_views(&scene, std::slice::from_ref(&view), None, None, Some(prefetch_failed), None, None)
			.expect("render with draw-time GUI retry");
		assert_eq!(stats.guis, 1, "GUI must be drawn after draw-time retry recovers it");
		assert_eq!(stats.gui_report.drawn.len(), 1, "recovered GUI must appear in gui_report.drawn: {:?}", stats.gui_report);
		assert!(stats.gui_report.skipped.is_empty(), "recovered GUI must not remain in gui_report.skipped: {:?}", stats.gui_report);

		// Permanent/unrecoverable causes (missing rur or stale ROR_RUR) must NOT be retried at
		// draw time even if the key is missing from `ready`.
		for cause in [
			"rur not found (set ROR_RUR or RUR_HOME)",
			"ROR_RUR points at /no/such/rur, which is not a rur checkout (expected cli/rur.mjs)",
		] {
			assert!(is_unrecoverable_gui_skip(cause));
			let unrecoverable = (GuiTextures::new(), HashMap::from([(key, cause.to_string())]));
			let (_pending, stats) = renderer
				.submit_views(&scene, std::slice::from_ref(&view), None, None, Some(unrecoverable), None, None)
				.expect("render skipping unrecoverable GUI retry");
			assert_eq!(stats.guis, 0, "unrecoverable cause {cause:?} must not retry at draw time");
			assert!(stats.gui_report.drawn.is_empty());
			assert_eq!(stats.gui_report.skipped.len(), 1);
			assert_eq!(stats.gui_report.skipped[0].reason, cause);
		}

		// `warn_invalid_rur_once` emits one warning per distinct invalid-ROR_RUR cause across many frames.
		struct NoteSink(parking_lot::Mutex<Vec<String>>);
		impl crate::Caller for NoteSink {
			fn hear(&self, text: &str) {
				self.0.lock().push(text.to_string());
			}
			fn gone(&self) -> bool {
				false
			}
			fn setting(&self, _name: &str) -> Option<String> {
				None
			}
		}
		let sink = std::rc::Rc::new(NoteSink(parking_lot::Mutex::new(Vec::new())));
		let mut report = crate::scene::guis::GuiReport::default();
		report.record_skipped("Board.Sign", "ROR_RUR points at /stale/rur, which is not a rur checkout (expected cli/rur.mjs)");
		let mut warned = std::collections::HashSet::new();
		crate::for_caller(sink.clone(), || {
			for _ in 0..10 {
				warn_invalid_rur_once(&mut warned, &report);
			}
		});
		assert_eq!(sink.0.lock().len(), 1, "invalid ROR_RUR must warn once per run per cause, got: {:?}", *sink.0.lock());

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_magnified_surface_gui_is_drawn_at_a_doubling_of_its_scale_past_the_screens() {
		let corners = |width: f32, height: f32| [Some(glam::Vec2::ZERO), Some(glam::Vec2::new(width, 0.0)), Some(glam::Vec2::new(0.0, height)), Some(glam::Vec2::new(width, height))];
		// a 32 x 24 canvas 1,024 pixels across: 32 device pixels a canvas pixel, drawn at 48
		assert_eq!(surface_scale([32.0, 24.0], corners(1024.0, 768.0)), 48.0);
		// at its own size or smaller, at 1.5
		assert_eq!(surface_scale([32.0, 24.0], corners(48.0, 36.0)), SURFACE_SCALE);
		assert_eq!(surface_scale([300.0, 300.0], corners(40.0, 40.0)), SURFACE_SCALE);
		// a hair past it, the next doubling; a corner behind the camera, as before
		assert_eq!(surface_scale([100.0, 100.0], corners(151.0, 100.0)), 3.0);
		assert_eq!(surface_scale([100.0, 100.0], [None, Some(glam::Vec2::ZERO), Some(glam::Vec2::ZERO), Some(glam::Vec2::ZERO)]), SURFACE_SCALE);
	}

	/// What a Roblox client shows of GUIs in the world (fixtures/world-guis.luau; notes/render.md "GUIs"): a SurfaceGui
	/// facing the sun takes it with GlobalShadows on as without them, a PointLight lights a SurfaceGui beside it, a
	/// billboard seen against the sun takes 0.41 of it from behind with GlobalShadows on, and a SurfaceGui seen smaller
	/// than its image shows the coverage of 4x MSAA samples, not filtered texels. Before, the first and third were
	/// black (GUIs took no sun with GlobalShadows, none from behind), the second unlit and the last filtered.
	#[test]
	fn guis_in_the_world_are_lit_and_sampled_as_a_client_draws_them() {
		use crate::render::camera::CameraSpec;
		use crate::scene::build::{ModelBuilder, PartSpec, facing};
		use rbx_dom_weak::InstanceBuilder;
		use rbx_dom_weak::types::{Color3, Enum, UDim, UDim2, Vector2};

		let state = std::env::temp_dir().join(format!("ror-world-gui-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let sun = crate::render::environment::sun_direction(14.5, 0.0);
		// the sun alone (Brightness 2) or nothing at all, no fog
		let lighting = |scene: &mut Scene, brightness: f32, global_shadows: bool| {
			let environment = &mut scene.environment;
			(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
			(environment.clock_time, environment.geographic_latitude, environment.brightness, environment.global_shadows) = (14.5, 0.0, brightness, global_shadows);
			(environment.fog_color, environment.fog_start, environment.fog_end) = (Vec3::ZERO, 0.0, 100000.0);
		};
		// the view of `scene` with each GUI's image filled by `fill(column, row)`
		let draw = |scene: &Scene, request: &ViewRequest, fill: &dyn Fn(u32, u32) -> [u8; 4]| -> super::super::ViewImage {
			let camera = request.camera.place(None, request.width, request.height);
			let mut images = Vec::new();
			for draw in plan(scene, &camera, request) {
				let (columns, rows) = rur::image_size(draw.key.canvas(), draw.key.scale(), draw.key.margin());
				let rgba: Vec<u8> = (0..rows).flat_map(|row| (0..columns).flat_map(move |column| fill(column, row))).collect();
				images.push((draw.key, Arc::new(GuiImage::from_rgba(columns, rows, rgba).unwrap()), false));
			}
			assert!(!images.is_empty(), "the GUI is planned");
			let (mut textures, mut warnings, mut skipped) = (HashMap::new(), Vec::new(), HashMap::new());
			renderer.upload_guis(images, &mut textures, &mut warnings, &mut skipped);
			assert!(warnings.is_empty(), "{warnings:?}");
			let (pending, mut stats) = renderer.submit_views(scene, std::slice::from_ref(request), None, None, Some((textures, skipped)), None, None).unwrap();
			pending.into_iter().next().unwrap().finish(&mut stats).unwrap()
		};
		// the mean red level of the 3 x 3 pixels round where `at` lands in the view
		let level = |image: &super::super::ViewImage, request: &ViewRequest, at: Vec3| -> f32 {
			let camera = request.camera.place(None, request.width, request.height);
			let clip = camera.view_projection(request.width as f32 / request.height as f32) * at.extend(1.0);
			let (x, y) = ((((clip.x / clip.w) * 0.5 + 0.5) * request.width as f32) as i32, ((0.5 - (clip.y / clip.w) * 0.5) * request.height as f32) as i32);
			let mut sum = 0.0;
			for dy in -1..=1 {
				for dx in -1..=1 {
					sum += image.rgba[(((y + dy) as u32 * request.width + (x + dx) as u32) * 4) as usize] as f32;
				}
			}
			sum / 9.0
		};
		let surface = |model: &mut ModelBuilder, part: rbx_dom_weak::types::Ref, canvas: [f32; 2], light_influence: f32| {
			model.child(
				part,
				InstanceBuilder::new("SurfaceGui")
					.with_property("Face", Enum::from_u32(5))
					.with_property("CanvasSize", Vector2::new(canvas[0], canvas[1]))
					.with_property("LightInfluence", light_influence)
					.with_property("Brightness", 1.0f32),
			);
		};
		let white = |_: u32, _: u32| [255u8; 4];
		let view = |eye: Vec3, target: Vec3, fov: f32, width: u32| ViewRequest { width, height: 128, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov }, background: super::super::Background::Color([0.0, 0.0, 0.0]), bloom: false, ..Default::default() };

		// a white SurfaceGui (LightInfluence 1) on a slab facing the sun, seen from near the sun
		let slab = |global_shadows: bool| {
			let mut model = ModelBuilder::new("Sun");
			let part = model.part(&PartSpec { name: "Slab".into(), cframe: facing(Vec3::ZERO, sun), size: Vec3::new(8.0, 8.0, 0.5), color: [0, 0, 0], material: 272, ..Default::default() });
			surface(&mut model, part, [100.0, 100.0], 1.0);
			let mut scene = crate::scene::extract::extract(&model.into_document());
			lighting(&mut scene, 2.0, global_shadows);
			scene
		};
		let request = view(sun * 30.0 + Vec3::new(0.0, 0.0, 0.5), Vec3::ZERO, 30.0, 128);
		let front = |global_shadows: bool| level(&draw(&slab(global_shadows), &request, &white), &request, sun * 0.25);
		let (open, shadows) = (front(false), front(true));
		assert!(open > 150.0, "a SurfaceGui facing the sun is lit ({open})");
		assert!((open - shadows).abs() <= 1.0, "with GlobalShadows a SurfaceGui facing the sun takes it as without them ({shadows} against {open})");

		// in the dark, a PointLight (Brightness 2, Range 16) 3 studs before a white SurfaceGui
		let mut model = ModelBuilder::new("Lamp");
		let part = model.part(&PartSpec { name: "Board".into(), size: Vec3::new(12.0, 6.0, 0.5), color: [0, 0, 0], material: 272, ..Default::default() });
		surface(&mut model, part, [200.0, 100.0], 1.0);
		let lamp = model.part(&PartSpec { name: "Lamp".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, -3.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		model.child(lamp, InstanceBuilder::new("PointLight").with_property("Brightness", 2.0f32).with_property("Range", 16.0f32).with_property("Color", Color3::new(1.0, 1.0, 1.0)));
		let mut scene = crate::scene::extract::extract(&model.into_document());
		lighting(&mut scene, 0.0, true);
		let request = view(Vec3::new(3.0, 2.0, -30.0), Vec3::ZERO, 30.0, 128);
		let lit = level(&draw(&scene, &request, &white), &request, Vec3::new(0.0, 0.0, -0.25));
		assert!(lit > 150.0, "a PointLight before a SurfaceGui lights it ({lit})");

		// a white billboard (LightInfluence 1) seen against the sun: its normal faces the camera, away from the sun
		let across = Vec3::Y.cross(sun).normalize();
		let against = |global_shadows: bool| {
			let mut model = ModelBuilder::new("Behind");
			let anchor = model.part(&PartSpec { name: "Anchor".into(), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			let size = UDim2::new(UDim::new(4.0, 0), UDim::new(4.0, 0));
			model.child(anchor, InstanceBuilder::new("BillboardGui").with_property("Size", size).with_property("LightInfluence", 1.0f32).with_property("Brightness", 1.0f32));
			// (a block below it, so that GlobalShadows draw the sun's shadow maps)
			model.part(&PartSpec { name: "Caster".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -12.0, 0.0)), size: Vec3::splat(4.0), color: [0, 0, 0], material: 272, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			lighting(&mut scene, 2.0, global_shadows);
			let request = view((across - sun * 0.8).normalize() * 25.0, Vec3::ZERO, 30.0, 128);
			level(&draw(&scene, &request, &white), &request, Vec3::ZERO)
		};
		let (open, shadows) = (against(false), against(true));
		assert!(open > 60.0, "a billboard seen against the sun is lit from behind ({open})");
		assert!((open - shadows).abs() <= 1.0, "with GlobalShadows a billboard takes the sun from behind as without them ({shadows} against {open})");

		// an unlit SurfaceGui of one-texel black lines every third texel on white, seen 2.7 times smaller than its image
		let mut model = ModelBuilder::new("Lines");
		let part = model.part(&PartSpec { name: "Board".into(), size: Vec3::new(12.0, 4.0, 0.5), color: [0, 0, 0], material: 272, ..Default::default() });
		surface(&mut model, part, [300.0, 100.0], 0.0);
		let mut scene = crate::scene::extract::extract(&model.into_document());
		lighting(&mut scene, 0.0, false);
		// 300 canvas pixels over 12 studs, 1.5 / 2.7 device pixels each: the camera this far off
		let focal = 64.0 / 20f32.to_radians().tan();
		let request = view(Vec3::new(0.0, 0.0, -focal / (25.0 * 1.5 / 2.7)), Vec3::ZERO, 40.0, 256);
		let image = draw(&scene, &request, &|column, _| if column % 3 == 0 { [0, 0, 0, 255] } else { [255; 4] });
		let mut levels: Vec<u8> = (48..80u32).flat_map(|row| (98..158u32).map(move |column| (row, column))).map(|(row, column)| image.rgba[((row * 256 + column) * 4) as usize]).collect();
		levels.sort_unstable();
		levels.dedup();
		assert!(levels.len() <= 5, "each pixel shows 0-4 quarters of a line, as 4x MSAA samples of geometry do: {levels:?}");
		// (a pixel 2.7 texels wide holds the four samples over two of them: one or two quarters of a line)
		assert!(levels.len() >= 2, "lines of the image show: {levels:?}");

		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
