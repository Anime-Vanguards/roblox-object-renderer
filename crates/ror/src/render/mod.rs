//! The wgpu renderer.
//!
//! One [`Renderer`] per process holds the GPU device, the pipelines and the resource caches;
//! [`Renderer::render`] may be called from many threads at once. A render prepares the scene once
//! (geometry and textures from the caches, the instance buffer, the sun's shadow map, the sky
//! lighting) and then draws every requested view of it.

mod ao;
pub use ao::AoHistory;

/// What a clip keeps from frame to frame while its camera holds still: its occlusion
/// ([`AoHistory`]) and the parts its residency feedback has seen (`residency::ClipResidency`).
pub struct ClipKept {
	pub ao: AoHistory,
	residency: residency::ClipResidency,
	/// Its last frame's preparation, which the next frame's starts from (prepared::ClipGeometry)
	geometry: prepared::ClipGeometry,
}

impl ClipKept {
	pub fn new() -> Self {
		Self { ao: AoHistory::new(), residency: Default::default(), geometry: Default::default() }
	}
}

impl Default for ClipKept {
	fn default() -> Self {
		Self::new()
	}
}
pub mod beams;
mod bloom;
pub mod camera;
pub mod clouds;
pub mod composite;
mod culling;
#[cfg(test)]
mod cutout_depth_tests;
#[cfg(test)]
mod glass_tests;
#[cfg(test)]
mod sun_shadow_tests;
#[cfg(test)]
mod texture_alpha_tests;
#[cfg(test)]
mod post_tests;
#[cfg(test)]
mod quality_tests;
#[cfg(test)]
mod tone_tests;
pub mod environment;
pub mod geometry;
pub mod gpu;
pub mod guis;
mod gui_files;
mod highlights;
mod ids;
mod levels;
mod light_clusters;
pub(crate) mod nv12;
pub(crate) use nv12::VideoReadback;
pub mod lightgrid;
pub mod materials;
pub mod particles;
mod pipelines;
mod post;
mod prepared;
mod profile;
mod readback;
mod renderdoc;
mod residency;
pub(crate) use readback::Held;
pub use readback::PendingView;
pub mod resources;
mod reuse;
mod runs;
mod shadow_reuse;
pub mod rur;
mod skinning;
mod texels;
mod texture_files;
pub mod texture_pack;
pub mod trails;
mod transcode;
mod voxel_field;
pub use transcode::{downloads, transcode_packs, transcode_target};

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use glam::{Affine3A, Mat4, Vec2, Vec3, Vec4};
use parking_lot::Mutex;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

use crate::scene::{Geometry, LightKind, Materials, Part, Scene};
use camera::{Camera, CameraSpec};
use environment::CubeImage;
use gpu::Gpu;
use materials::MaterialParams;
use pipelines::{DEPTH_FORMAT, HDR_FORMAT, OPAQUE_SHARE_FORMAT, OUTPUT_FORMAT, Pipelines, SHADOW_FORMAT, SUN_MOMENTS_FORMAT};
use resources::{Cache, GpuMesh, GpuTexture, TextureUse, Weighed};
use texture_pack::TexturePack;

/// What fills the pixels no part covers.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Background {
	/// The scene's sky (Roblox's default skybox when the scene has no Sky).
	#[default]
	Sky,
	/// Alpha 0: a PNG with straight alpha, over black the image of a black background (light that
	/// covers nothing tints what is behind it; notes/render.md "Transparent backgrounds").
	Transparent,
	/// A flat sRGB colour (0..1).
	Color([f32; 3]),
	/// A ViewportFrame's target: cleared to its background, an sRGB colour (0..1) at an opacity (1 -
	/// BackgroundTransparency), and written as a PNG with straight alpha. Studio averages its samples'
	/// colours as they are, so a part's edge mixes the clear colour in at full strength whatever its
	/// opacity, and clips the mean at 1 (notes/render.md "ViewportFrames").
	Frame { color: [f32; 3], alpha: f32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRequest {
	pub width: u32,
	pub height: u32,
	#[serde(default)]
	pub camera: CameraSpec,
	#[serde(default)]
	pub background: Background,
	/// MSAA samples (1 or 4).
	#[serde(default = "default_msaa")]
	pub msaa: u32,
	/// Render at `supersample` times the size and average down (1-4).
	#[serde(default = "default_supersample")]
	pub supersample: u32,
	/// Studio's built-in bloom (the glow around Neon and very bright pixels).
	#[serde(default = "default_bloom")]
	pub bloom: bool,
	/// Also report which part is in front at every pixel ([`ViewImage::ids`]).
	#[serde(default)]
	pub ids: bool,
	/// Also report what lies at every pixel for the cost of drawing it: the frontmost opaque part, its
	/// depth, and the see-through parts' and decals' fragments in front of it ([`ViewImage::layers`]).
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub layers: bool,
	/// Device pixels per GUI pixel for BillboardGuis (their pixel offsets, text, lines); default the
	/// image height / 744, Studio's 1.5 at its 1116 px tall capture viewport (render::guis).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub gui_scale: Option<f32>,
	/// Calibration: the image is √(exposed radiance · this) without the tone curve, the bloom or
	/// colour correction, what the bloom reads (`ROR_CALIBRATE_SOURCE`, tools/analysis/bloom_fit.py).
	#[serde(skip)]
	pub linear_scale: Option<f32>,
	/// Keep the image on the GPU ([`ViewImage::texture`]) instead of reading it back: a window draws
	/// it on this device, no copy and no wait. Highlights, AlwaysOnTop GUIs and part ids (composited
	/// after the readback) are left out.
	#[serde(skip)]
	pub texture: bool,
	/// With `texture`: also keep the share of each pixel's samples an opaque surface covers
	/// ([`ViewImage::opaque_share`]; a SurfaceGui window's frame background shows only in the rest,
	/// render::guis). Not with supersampling.
	#[serde(skip)]
	pub opaque_share: bool,
	/// Draw each MeshPart at the level of detail Roblox draws for this view's camera and its
	/// RenderFidelity, and leave out the parts the graphics quality's view distance and size cull leave
	/// out (crate::lod; `"roblox:<quality>"`); `None`: every mesh at full detail, every part drawn.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub lod: Option<crate::lod::Lod>,
	/// Draw what a Roblox client draws at this graphics quality level (1-21, crate::quality, under the
	/// scene's LightingStyle and PrioritizeLightingQuality): the sun's shadow maps to the level's reach and
	/// the light grid's voxel sun past it, the share of the emitters' Rate it makes, and (without `lod`)
	/// each MeshPart at its level of detail and none of the parts the level's view distance and size cull
	/// leave out. `None`: level 21, every mesh at full detail and every part drawn.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quality: Option<u32>,
}

fn default_bloom() -> bool {
	true
}

fn default_msaa() -> u32 {
	4
}

fn default_supersample() -> u32 {
	1
}

impl Default for ViewRequest {
	fn default() -> Self {
		Self { width: 1024, height: 1024, camera: CameraSpec::default(), background: Background::Sky, msaa: 4, supersample: 1, bloom: true, ids: false, layers: false, gui_scale: None, linear_scale: None, texture: false, opaque_share: false, lod: None, quality: None }
	}
}

pub struct ViewImage {
	pub width: u32,
	pub height: u32,
	/// RGBA8, sRGB-encoded, straight alpha (empty with [`ViewRequest::texture`]).
	pub rgba: Vec<u8>,
	/// With [`ViewRequest::texture`]: the image on the GPU, `Rgba8Unorm` holding sRGB-encoded values
	/// (as egui-wgpu takes a native texture), straight alpha, its passes submitted. Given back
	/// ([`Renderer::give_back`]) once no longer shown, a later view draws into it.
	pub texture: Option<wgpu::Texture>,
	/// With [`ViewRequest::opaque_share`]: per pixel the share of its samples an opaque surface covers
	/// (`R8Unorm`, the image's size), given back like `texture`.
	pub opaque_share: Option<wgpu::Texture>,
	pub camera: Camera,
	/// With [`ViewRequest::ids`]: per pixel, the frontmost part's index in `Scene::parts` + 1 (0 for
	/// none; decals count as their part).
	pub ids: Option<Vec<u32>>,
	/// With [`ViewRequest::layers`]: the opaque part and its depth at every pixel and the see-through
	/// layers in front of it.
	pub layers: Option<ids::Layers>,
	/// With [`ViewRequest::lod`]: the levels its meshes drew at and the parts left out.
	pub lod: Option<levels::LodView>,
}

pub use ids::Layers;
pub use levels::LodView;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderStats {
	pub prepare_ms: f64,
	/// The immutable part preparation matched an earlier render's current inputs exactly.
	pub geometry_reused: bool,
	/// CPU time obtaining shadow maps, including encoding and submitting them on a cache miss.
	pub sun_shadow_ms: f64,
	pub light_shadow_ms: f64,
	pub assets_ms: f64,
	/// Building the light grid (part of prepare)
	pub light_grid_ms: f64,
	pub draw_ms: f64,
	pub readback_ms: f64,
	/// Skinned meshes posed this render (not found posed in the cache, or a live part's pose changed):
	/// their CPU skinning, and their vertices' upload to the GPU (part of prepare).
	pub skinned: usize,
	pub skin_ms: f64,
	pub skin_upload_ms: f64,
	pub skin_upload_bytes: u64,
	pub parts: usize,
	pub draws: usize,
	pub triangles: u64,
	/// Assets that could not be loaded, with the reason (the part draws without them), each reason once
	/// however many views met it.
	pub missing: Vec<String>,
	/// Local files hashed in full because their volume provided no trusted OS change token
	/// (docs/local-files.md).
	pub untrusted_stamp_hashes: usize,
	/// GUI quads drawn (over every view) and the time spent getting their textures (rur's renders
	/// included).
	pub guis: usize,
	pub gui_ms: f64,
	/// The effects: particles drawn at the moment drawn, and the scene's ParticleEmitters (the legacy
	/// effects' among them), beams and trails.
	pub particles: usize,
	pub emitters: usize,
	pub beams: usize,
	pub trails: usize,
	/// The box orbit views framed: the parts as they draw or, no part drawn, what the effects draw
	/// (None: nothing to frame, the views look at the origin).
	pub framed: Option<[[f32; 3]; 2]>,
	/// What the job should report: GUIs not drawn and why, rur's diagnostics.
	#[serde(skip)]
	pub warnings: Vec<String>,
	/// Which GUIs drew and which were skipped (with reasons) across the views rendered.
	#[serde(skip)]
	pub gui_report: crate::scene::guis::GuiReport,
	/// Texture residency (render::residency): its feedback passes' time and the whole step's, and the
	/// material maps whose levels it read and the bytes they added.
	#[serde(skip)]
	pub residency_ms: f64,
	#[serde(skip)]
	pub residency_total_ms: f64,
	#[serde(skip)]
	pub residency_reads: usize,
	#[serde(skip)]
	pub residency_read_bytes: u64,
	/// Waiting for the GPU memory the draw's targets add ([`crate::budget::GpuGate`])
	#[serde(skip)]
	pub gpu_wait_ms: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
	view_proj: [[f32; 4]; 4],
	inv_view_proj: [[f32; 4]; 4],
	shadow_view_proj: [[f32; 4]; 4],
	camera_position: [f32; 4],
	sun_direction: [f32; 4],
	sun_color: [f32; 4],
	ambient: [f32; 4],
	outdoor_ambient: [f32; 4],
	sky_sh: [[f32; 4]; 9],
	sky_cube: [[f32; 4]; 6],
	environment: [f32; 4],
	shadow: [f32; 4],
	fog_color: [f32; 4],
	fog: [f32; 4],
	counts: [u32; 4],
	viewport: [f32; 4],
	/// The light grid: xyz its minimum corner, w 1 when there is one
	light_grid: [f32; 4],
	/// xyz 1 / its size in studs, w OutdoorAmbient's factor where the sky is open
	light_grid_scale: [f32; 4],
	/// The sun's shadow cascades: each one's depth pass view-projection, sphere (centre, radius; 0
	/// unused) and square in the moment atlas (column, side in texels)
	shadow_cascades: [[[f32; 4]; 4]; 4],
	shadow_spheres: [[f32; 4]; 4],
	shadow_regions: [[f32; 4]; 4],
	atmosphere: [f32; 4],
	/// The direct specular's lobe ([`materials::DIRECT_SPECULAR`]): x the GGX width at roughness 0, y the
	/// width's scale, z and w the visibility's k at roughness 0 and its growth with the roughness
	direct_specular: [f32; 4],
	/// The metalness and the sky specular fading with the view depth ([`materials::SPECULAR_FADE`])
	specular_fade: [f32; 4],
	/// The direct specular's reflectance ([`materials::DIRECT_REFLECTANCE`])
	direct_reflectance: [f32; 4],
	/// The sky specular's colour ([`materials::SKY_SPECULAR_COLOUR`])
	sky_specular_colour: [f32; 4],
	/// The direct specular's metallic share ([`materials::DIRECT_METAL`])
	direct_metal: [f32; 4],
	/// What the haze surfaces fade into adds to the sky chain's ([`EnvironmentMaps`]'s `haze`): per
	/// cube face (rgb), and w of `haze_shape` 1 when there is any
	haze_light: [[f32; 4]; 6],
	haze_shape: [f32; 4],
	/// The lights' voxels baked for the view ([`voxel_field`]): xyz its first voxel's index, w 1 when
	/// there is one; xyz its voxels along each axis
	voxel_field: [f32; 4],
	voxel_field_size: [f32; 4],
	/// Terrain Clouds' dimming of the sun ([`clouds::sun_cover`]): x the share they take off, yz the
	/// camera's X and Z, w unused
	cloud_sun: [f32; 4],
	/// The view's lights by cluster ([`light_clusters`]): the tile side, tiles across and down, u32s a
	/// cluster (0: none); the camera's forward axis and the slices per unit of ln(depth)
	light_clusters: [f32; 4],
	light_cluster_view: [f32; 4],
	/// Where surfaces start taking the local lights' voxels instead of per pixel and where they take
	/// them alone, from the camera, at the view's graphics quality ([`local_light_far`]); both 0: level
	/// 21's 580 and 600 studs (common.wgsl LOCAL_LIGHT_FAR_START, LOCAL_LIGHT_FAR_END)
	local_light_far: [f32; 4],
	/// The view's graphics quality shading ([`quality_shading`]): where the high-quality shading starts fading and
	/// has gone from the camera, how far a part's box may be from the focus to be lit per pixel, the terms left out
	quality_shading: [f32; 4],
	/// The view's focus ([`view_focus`]), and how far a part's cluster may be from it with faces turned from the sun
	/// unlit (crate::quality::Quality::back_light_radius)
	quality_focus: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SunBlurUniform {
	params: [f32; 4],
	depth: [f32; 4],
	region: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InstanceData {
	model: [[f32; 4]; 4],
	normal_matrix: [[f32; 4]; 4],
	color: [f32; 4],
	params: [f32; 4],
	size: [f32; 4],
	flags: [u32; 4],
	tiling: [f32; 4],
	part_inverse: [[f32; 4]; 4],
	specular: [f32; 4],
	material: [f32; 4],
	texture: [f32; 4],
	tint: [f32; 4],
	lobe: [f32; 4],
	/// The box round the part's cluster (xyz, w unused; [`cluster_boxes`]), for faces turned from the sun
	cluster_low: [f32; 4],
	cluster_high: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LightData {
	position: [f32; 4],
	direction: [f32; 4],
	color: [f32; 4],
	right: [f32; 4],
	up: [f32; 4],
	/// x: the first of its six shadow map layers (-1 without Shadows), y 1 with Shadows, z how far
	/// behind a SurfaceLight's face its shadow is cast from, w its faces' texels a side
	shadow: [f32; 4],
	/// What its voxels give particles, beams, trails and far surfaces (common.wgsl `voxel_light`):
	/// rgb 1.98 · Brightness · Color², w how far from the light they reach
	voxel_color: [f32; 4],
	/// x: 1 when far surfaces take its voxels, 0 for a light placed with a playback (light-latency);
	/// y: the Range its shadow's grazing darkening goes by ([`shared_graze_ranges`]); z: a
	/// SurfaceLight's shadow frustum's half-angle tangent when its map is one aimed face (0: six faces)
	extra: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkyUniform {
	params: [f32; 4],
	background: [f32; 4],
	sun: [f32; 4],
	moon: [f32; 4],
	bodies: [f32; 4],
	/// The skybox's images: x 1 when drawn from them, z SKY_RADIANCE
	images: [f32; 4],
	/// SkyboxOrientation's rotation, by columns
	rotation: [[f32; 4]; 3],
	/// The time of day's display tints above and below (w 1 when tinted)
	tint_above: [f32; 4],
	tint_below: [f32; 4],
	/// The Atmosphere ([`environment::AtmosphereParams`])
	haze: [f32; 4],
	haze_color: [f32; 4],
	haze_light: [[f32; 4]; 6],
	glare: [f32; 4],
	decay: [f32; 4],
	glare_sun: [f32; 4],
	/// The camera's rays ([`Camera::ray_unprojection`])
	rays: [[f32; 4]; 4],
	/// Terrain Clouds ([`clouds::uniform`])
	clouds: [[f32; 4]; 9],
	/// The stars ([`star_uniform`])
	stars: [f32; 4],
}

/// The stars for the sky pass (shaders/sky.wgsl `stars`): x how many (StarCount up to
/// [`environment::STARS_MAX`], 0 without), y their light as a share of midnight's
/// ([`environment::star_light`]), z the side of a star in the target's pixels (the supersampling
/// factor: Studio draws each star one pixel across, whatever the field of view), w unused. A Sky
/// draws them only with CelestialBodiesShown, and the default sky without a Sky draws 3,000 (sim
/// client: the same count as a Sky at StarCount 3,000); a ViewportFrame's sky draws none (not measured).
fn star_uniform(environment: &crate::scene::Environment, supersample: u32) -> [f32; 4] {
	let sky = environment.sky.clone().unwrap_or_default();
	if !sky.celestial_bodies_shown || environment.viewport.is_some() || sky.star_count <= 0 {
		return [0.0; 4];
	}
	let light = environment::star_light(environment.clock_time, environment.environment_diffuse_scale);
	if light <= 0.0 {
		return [0.0; 4];
	}
	[sky.star_count.min(environment::STARS_MAX) as f32, light, supersample as f32, 0.0]
}

/// The sun's and the moon's discs (sky-sun, sky-elements): their texture, a square of
/// Sun / MoonAngularSize facing the viewer, adds its value times a weight to the scene's √ radiance;
/// the sun 1.11 (its centre (√sky + 1.11)² over skyboxes of gray 0 to 255, at -3 to 0 EV, whatever the
/// Brightness), the moon 1.30. With an Atmosphere the sun is far brighter and follows Brightness:
/// 2.82 per unit (Brightness 1 and 2: centres 7.96 and 34 over black), 0.61 of that behind full haze
/// (Haze 5).
const SUN_WEIGHT: f32 = 1.109;
const MOON_WEIGHT: f32 = 1.30;
const SUN_ATMOSPHERE_WEIGHT: f32 = 2.82;
const SUN_HAZE_LOSS: f32 = 0.39;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ResolveUniform {
	params: [f32; 4],
	correction: [f32; 4],
	tint: [f32; 4],
	bloom: [f32; 4],
	/// x 1 with SunRaysEffect's rays to add, y 1 to show non-finite light in magenta (`ROR_NAN_CHECK`)
	rays: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BloomUniform {
	source: [f32; 4],
	footprint: [f32; 4],
	blur: [f32; 4],
}

/// Roblox holds Lighting.FogEnd to 100000: a client draws FogEnd 200000, 500000, 10⁶, 10⁷ and 10⁸ as
/// 100000, to the pixel (fixtures/tone-bloom.luau: black panels 60-2,000 studs away under each, and
/// FogStart 1000 with FogEnd 10⁸ fogging as (d - 1000) / (100000 - 1000)), so a FogEnd meant to switch
/// the fog off still leaves the default fog: x 0.0009 60 studs away, 2 of 255 on black at +2 EV.
const FOG_END_LIMIT: f32 = 100000.0;

/// How the sun's shadow maps are drawn at a graphics quality (notes/render.md "Shadows"): what of them
/// changes with the level. `SUN_SHADOWS` is quality 21's; a view at a lower level (`ViewRequest::quality`)
/// draws 21's cascades to its own reach (crate::quality::Quality::sun_shadows: [`view_sun_cascades`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SunShadowQuality {
	/// Slices of the view's depth ending at these distances (studs), one cascade each: each covered by
	/// a sphere centred on the view axis at the slice's far end, through its far corners or its near
	/// ones (whichever are further); a point takes the first sphere holding it, and has no shadow beyond
	/// the last
	pub splits: &'static [f32],
	/// Texels across each cascade's diameter: the filter follows the sphere's radius by these
	pub texels: &'static [u32],
	/// The filter in texels: a Gaussian of sigma `filter.0` · √(ShadowSoftness / texel in studs) between
	/// `filter.1` and `filter.2`
	pub filter: (f32, f32, f32),
	/// How far from the camera a surface takes the sun's shadow maps at all (studs); past it the light
	/// grid's voxel sun (lightgrid::propagate_sun)
	pub reach: f32,
}

/// Quality 21's sun shadows. Cascades: shadow-cascade-map, shadow-cascade-down (the first three; the
/// fourth's 400 studs is assumed: moving it changed little against the engine). Texels: 39 edges of
/// shadow-cascades, -cascade-map, -cascade-down and -softness over 3 cascades and fields of view 30-70
/// (rms 4 %). Filter: shadow-softness (0.5, 0.75 and 1 alike in the finest cascade, 0 and 0.05 alike in
/// a coarse one), with Studio's 4x MSAA resolve (SUN_MSAA_VARIANCE) and bilinear sampling's 1/6 on top;
/// the edge profiles alone fit 8 % narrower under a darkening of 0.17, the images (shadow-gap, -contact,
/// -softness, -cascades, -edge, -steps, R15's sun alone) prefer this with SUN_DARKEN. Reach: the Roblox
/// client (0.747, quality 21, fixtures/sun-shadows.luau "plates") shadows the floor under 6-stud plates
/// to 587.4 studs from the camera and none from 587.7, the same in every view (fields of view 40-70,
/// 1600 x 900, 900 x 1600 and 1644 x 1116, the camera 120-600 studs up): a distance from the camera,
/// not a depth along its axis or a cascade's sphere. ror's spheres shadowed to 700-1,000 studs.
pub(crate) const SUN_SHADOWS: SunShadowQuality = SunShadowQuality {
	splits: &[19.6, 45.3, 131.8, 400.0],
	texels: &[512, 1024, 1280, 1280],
	filter: (1.605, 0.832, 4.560),
	reach: 587.5,
};
const SUN_MSAA_VARIANCE: f32 = 0.091;
/// The moments' warp over a cascade's depth (-1..1 across its sphere) and their least variance (of
/// the warped depth squared): an occluder Δ studs in front of a receiver lets 1 / (1 + (Δ / L)²)
/// through, L = √variance · radius / warp, 0.37 studs in the cascade round the camera (shadow-gap:
/// plates 0.3, 0.6 and 1.2 studs in front of boards turned 45-75° from the sun leave 0.56-0.68,
/// 0.11-0.14 and 0 of the light, whatever the turn). A weak warp: where the filter mixes depths
/// (wider filters over the plates' narrow bands) Studio lets more through than a strong one does.
const SUN_WARP: f32 = 1.0;
const SUN_LEAST_VARIANCE: f32 = 0.000356;
/// L grows with the cascade's radius up to this many studs (SunCascade::least_variance): the Roblox
/// client (0.747, quality 21, fixtures/sun-shadows.luau "leaks" and "plates") lets nothing under plates
/// 3 to 8 studs over a floor (Δ 6.8-18.2) in any cascade, out to its shadows' reach, at fields of view
/// 40-70, 1600 x 900 and 900 x 1600, so L is at most 3.6 there; growing on, it was 5.6 to 11 in the
/// last cascade and let 0.2-0.6 of the light through (the near cascades' patterns still prefer the
/// growth below the cap).
const SUN_LEAK_MOST: f32 = 3.6;
/// Lit shares up to this go black, the rest rescaled (Studio's leak reduction: shadow-cascades' edge
/// profiles are a Gaussian's CDF rescaled from 0.16-0.18 up; see SUN_SHADOWS' filter)
const SUN_DARKEN: f32 = 0.22;

/// One cascade of a view's sun shadow.
struct SunCascade {
	/// Its place in the view's order (the shader picks the first sphere holding a point)
	index: usize,
	center: Vec3,
	radius: f32,
	texels: u32,
	/// World → the depth pass's clip space (the sphere's square, from beyond every caster)
	view_proj: Mat4,
	/// The depth pass's far plane and its eye's distance from the centre, towards the sun
	far: f32,
	reach: f32,
	/// The blur's Gaussian sigma in texels
	sigma: f32,
}

impl SunCascade {
	/// Its texel's side in studs.
	fn texel(&self) -> f32 {
		2.0 * self.radius / self.texels as f32
	}

	/// The moments' least variance in it (of the warped depth squared): the leak length L =
	/// √variance · radius / warp grows with the radius from the finest cascade's 0.37 studs at 19.6, up
	/// to SUN_LEAK_MOST studs.
	fn least_variance(&self) -> f32 {
		let leak = SUN_LEAST_VARIANCE.sqrt() * self.radius / SUN_WARP;
		if leak > SUN_LEAK_MOST { SUN_LEAST_VARIANCE * (SUN_LEAK_MOST / leak).powi(2) } else { SUN_LEAST_VARIANCE }
	}

	/// Whether a caster's sphere reaches its square, from beyond the sun side of its sphere (twice the
	/// sphere: a posed skinned mesh keeps its bind pose's bounds): what its depth pass draws.
	fn reaches(&self, draw: &DrawItem, sun: Vec3) -> bool {
		let (center, radius) = (draw.bound.0, draw.bound.1 * 2.0);
		let offset = center - self.center;
		let along = offset.dot(sun);
		along >= -(self.radius + radius) && (offset - sun * along).length() <= self.radius * std::f32::consts::SQRT_2 + radius
	}
}

/// A view's sun cascades at its graphics quality (crate::quality): none where the level draws no shadow maps
/// (or `RorNoSunShadowMaps` says so), else 21's (splits, texels and filter below 21 not measured: the client's
/// edges within the reach read as 21's) less those whose sphere lies wholly past the level's reach, which no
/// point takes.
fn view_sun_cascades(camera: &Camera, aspect: f32, prepared: &Prepared, environment: &crate::scene::Environment, quality: &crate::quality::Quality) -> Vec<SunCascade> {
	let (Some(reach), true, Some(bounds)) = (sun_reach(quality, environment), prepared.sun_shadows, prepared.bounds) else {
		return Vec::new();
	};
	let mut cascades = sun_cascades(camera, aspect, prepared.sun_direction, bounds, environment.shadow_softness, &SUN_SHADOWS);
	cascades.retain(|cascade| cascade.center.distance(camera.eye) - cascade.radius < reach.gone);
	cascades
}

/// Where a view's sun shadow maps reach ([`view_sun_cascades`]): its quality's, none under `RorNoSunShadowMaps`.
fn sun_reach(quality: &crate::quality::Quality, environment: &crate::scene::Environment) -> Option<crate::quality::SunReach> {
	quality.sun_shadows.filter(|_| environment.sun_shadow_maps)
}

/// The sun the light grid carries for `views` (lightgrid::grid_sun): only when one of them can see past its sun
/// shadow maps (a level without them, or a camera farther than their reach from a corner of the scene's `bounds`),
/// where it shades the sun, or when the scene has GUIs in the world the sun lights, whose sides turned from it take
/// their back light through it (gui.wgsl `gui_light`). Carrying it doubled the grid's build on 30,000 blocks over
/// 1,536 x 324 x 1,536 studs, for views that never read it.
fn grid_sun_for(scene: &Scene, views: &[ViewRequest], bounds: Option<(Vec3, Vec3)>) -> Option<Vec3> {
	let sun = lightgrid::grid_sun(scene)?;
	let lit_guis = scene.guis.iter().any(|gui| matches!(gui.kind, crate::scene::guis::GuiKind::Surface(_) | crate::scene::guis::GuiKind::Billboard(_)) && gui.light_influence > 0.0);
	if lit_guis {
		return Some(sun);
	}
	let (min, max) = bounds?;
	let environment = &scene.environment;
	let past = |view: &ViewRequest| match sun_reach(&view_quality(view, environment), environment) {
		None => true,
		Some(reach) => {
			let eye = view.camera.place(bounds, view.width.max(1), view.height.max(1)).eye;
			(0..8).any(|corner| {
				let point = Vec3::new(if corner & 1 == 0 { min.x } else { max.x }, if corner & 2 == 0 { min.y } else { max.y }, if corner & 4 == 0 { min.z } else { max.z });
				point.distance(eye) > reach.full
			})
		}
	};
	views.iter().any(past).then_some(sun)
}

/// The graphics quality a view draws at: its `quality` level under the scene's Lighting, level 21 without one.
pub(crate) fn view_quality(request: &ViewRequest, environment: &crate::scene::Environment) -> crate::quality::Quality {
	request.quality.map_or_else(crate::quality::Quality::top, |level| crate::quality::Quality::for_environment(level, environment))
}

/// Which terms the shadings below the high-quality one leave out ([`quality_shading`], mesh.wgsl `shading_tier`):
/// bit 1 the low-quality shading's sky specular, 2 its normal maps, 4 its sky diffuse light giving up what the sky
/// specular reflects (kept when set), 8 its Part.Reflectance; 16 the lowest shading's sky light the same from every
/// direction, 32 its direct specular; 64 metalness below the high-quality shading (metals drawn as any surface); 128 the
/// lowest shading's sky light giving up what the sky specular reflects; 256 the lowest shading's sun glint lit by vertex.
/// `ROR_TUNE_SHADING_TERMS` replaces them, for fits.
const SHADING_TERMS: u32 = 1 | 2 | 16 | 32 | 64 | 256;

fn shading_terms() -> u32 {
	static TERMS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
	*TERMS.get_or_init(|| std::env::var("ROR_TUNE_SHADING_TERMS").ok().and_then(|text| text.trim().parse().ok()).unwrap_or(SHADING_TERMS))
}

/// A graphics quality level's shading for the shader (`Frame.quality_shading`): the high-quality shading fades
/// linearly from 3/4 of its radius from the camera to 5/4 of it (crate::quality::Quality::shading_radius), a part is
/// lit per pixel within its focus radius of the view's focus (crate::quality::Quality::lit_radius), and the terms the
/// lower shadings leave out. Infinite radii are held to 1e9 studs.
fn quality_shading(quality: &crate::quality::Quality) -> [f32; 4] {
	let finite = |studs: f32| studs.min(1e9);
	[finite(quality.shading_radius * 0.75), finite(quality.shading_radius * 1.25), finite(quality.lit_radius), shading_terms() as f32]
}

/// The cells a Roblox client groups parts into by their middles, whose joint box decides how they take the sun near the
/// focus (crate::quality::Quality::back_light_radius; the `cluster` station: a ball's far side follows a part of any
/// material, transparency or kind 68 studs off in its 128 x 64 x 128-stud cell, not one across X = 128 or Y = 64, and a
/// bar whose middle is in the cell while it reaches past it)
const SUN_CLUSTER_CELL: Vec3 = Vec3::new(128.0, 64.0, 128.0);

/// Each instance's cluster box (`InstanceData::cluster_low`, `cluster_high`; mesh.wgsl `near_sun`): the box round every
/// part's box whose middle shares its part's cell ([`SUN_CLUSTER_CELL`]); decals take their part's. A box that is not
/// finite stands alone, as wide as the world (always near the focus).
fn cluster_boxes(instances: &mut [InstanceData]) {
	let boxes: Vec<Option<(glam::IVec3, Vec3, Vec3)>> = instances
		.iter()
		.map(|instance| {
			let part = Mat4::from_cols_array_2d(&instance.part_inverse).inverse();
			let centre = part.w_axis.truncate();
			let half = (part.x_axis.truncate().abs() + part.y_axis.truncate().abs() + part.z_axis.truncate().abs()) * 0.5;
			(centre.is_finite() && half.is_finite()).then(|| ((centre / SUN_CLUSTER_CELL).floor().as_ivec3(), centre - half, centre + half))
		})
		.collect();
	let mut clusters: std::collections::HashMap<glam::IVec3, (Vec3, Vec3)> = std::collections::HashMap::new();
	for &(cell, low, high) in boxes.iter().flatten() {
		let cluster = clusters.entry(cell).or_insert((low, high));
		*cluster = (cluster.0.min(low), cluster.1.max(high));
	}
	for (instance, part) in instances.iter_mut().zip(&boxes) {
		let (low, high) = part.map_or((Vec3::splat(-1e30), Vec3::splat(1e30)), |(cell, ..)| clusters[&cell]);
		instance.cluster_low = [low.x, low.y, low.z, 0.0];
		instance.cluster_high = [high.x, high.y, high.z, 0.0];
	}
}

/// The point a view's focus is at, which the client's per-pixel lighting follows (Camera.Focus; crate::quality::
/// Quality::lit_radius): what a look-at or orbit camera looks at, else the camera itself.
fn view_focus(spec: &CameraSpec, camera: &camera::Camera, bounds: Option<(Vec3, Vec3)>) -> Vec3 {
	match spec {
		CameraSpec::LookAt { target, .. } => Vec3::from(*target),
		CameraSpec::Orbit { target: Some(target), .. } => Vec3::from(*target),
		CameraSpec::Orbit { target: None, .. } => bounds.map_or(camera.eye, |(low, high)| (low + high) * 0.5),
		CameraSpec::CFrame { .. } => camera.eye,
	}
}

/// The samples a view draws with: 4 with `msaa` 4 or more, else 1, and none past its graphics quality level's
/// anti-aliasing (crate::quality::Quality::msaa: none at levels 1-16, 2 samples at 17, which draws 4 here, 4 from 18)
pub(crate) fn view_samples(request: &ViewRequest) -> u32 {
	let level = request.quality.map_or(4, |level| crate::quality::Quality::new(level, false, true).msaa);
	if request.msaa >= 4 && level > 1 { 4 } else { 1 }
}

/// The sun's cascades at `quality` for a camera whose spheres reach the scene's bounds.
fn sun_cascades(camera: &Camera, aspect: f32, sun: Vec3, bounds: (Vec3, Vec3), softness: f32, quality: &SunShadowQuality) -> Vec<SunCascade> {
	let forward = -Vec3::from(camera.world.matrix3.z_axis).normalize();
	// the frustum's corner: tan of the half field of view (vertical) times the diagonal
	let corner = (camera.fov.to_radians() * 0.5).tan() * (1.0 + aspect * aspect).sqrt();
	let (min, max) = bounds;
	let corners: Vec<Vec3> = (0..8)
		.map(|index| Vec3::new(if index & 1 == 0 { min.x } else { max.x }, if index & 2 == 0 { min.y } else { max.y }, if index & 4 == 0 { min.z } else { max.z }))
		.collect();
	let up = if sun.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
	let mut near = 0.0;
	let mut cascades: Vec<SunCascade> = Vec::new();
	for (index, &far) in quality.splits.iter().enumerate() {
		let radius = (far * corner).max(Vec2::new(far - near, near * corner).length());
		let center = camera.eye + forward * far;
		near = far;
		// a sphere that misses the scene holds nothing, and none is picked after one holding it all
		if center.clamp(min, max).distance(center) > radius {
			continue;
		}
		if cascades.iter().any(|earlier| corners.iter().all(|point| point.distance(earlier.center) < earlier.radius)) {
			break;
		}
		let texels = quality.texels[index];
		let texel = 2.0 * radius / texels as f32;
		// the depth pass's square moves over the scene a whole texel at a time (centred on the sphere's
		// centre snapped across the sun's view), so a caster lands in the same texels from frame to
		// frame: sliding with the camera by fractions of one, a thin caster's shadow drew a little
		// darker and lighter in turn (a map's tree trunks, their long shadows across the track
		// flickering as a camera turned). The sphere, which picks the cascade, stays where Studio's is
		let axes = Mat4::look_at_rh(Vec3::ZERO, -sun, up);
		let across = axes.transform_vector3(center);
		let snapped = axes.inverse().transform_vector3(Vec3::new((across.x / texel).round() * texel, (across.y / texel).round() * texel, across.z));
		let (scale, least, most) = quality.filter;
		let filter = (scale * (softness.max(0.0) / texel).sqrt()).clamp(least, most);
		// the depth pass starts beyond every caster towards the sun
		let reach = corners.iter().map(|point| (*point - center).dot(sun)).fold(radius, f32::max) + 1.0;
		let far_plane = reach + radius + 1.0;
		let view = Mat4::look_at_rh(snapped + sun * reach, snapped, up);
		let projection = Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.0, far_plane);
		cascades.push(SunCascade {
			index,
			center,
			radius,
			texels,
			view_proj: projection * view,
			far: far_plane,
			reach,
			sigma: (filter * filter + SUN_MSAA_VARIANCE).sqrt(),
		});
	}
	cascades
}

/// The Atmosphere's extinction per stud at Density 1: a surface d studs away shows through by
/// exp(-0.1 · Density⁴ · d) (atmosphere-distance: Density 0.2-1 fit 0.0995-0.0999).
const ATMOSPHERE_EXTINCTION: f32 = 0.0997;
/// The specular chain level a surface fades into: ATMOSPHERE_TOP_LEVEL · transmission^ATMOSPHERE_LEVEL_POWER,
/// at least the top level times Offset^ATMOSPHERE_OFFSET_POWER, i.e. 5 · max(transmission, Offset).
/// Fitted on the tile rows of atmosphere-objects2, -mips and -distance (black tiles' mean error in
/// levels of 255: 6.3 / 8.4 / 3.4 with 5 · √transmission and Offset^0.6, 4.3 / 3.8 / 2.9 linear; the
/// powers 0.85-1.2 and the top 4.75-5 are within 0.2 of it).
const ATMOSPHERE_LEVEL_POWER: f32 = 1.0;
const ATMOSPHERE_TOP_LEVEL: f32 = 5.0;
const ATMOSPHERE_OFFSET_POWER: f32 = 1.0;

/// (level power, top level, offset power) of the Atmosphere's fog; `ROR_TUNE_ATMOSPHERE="p,top,q"`
/// overrides them for fits.
fn atmosphere_levels() -> (f32, f32, f32) {
	std::env::var("ROR_TUNE_ATMOSPHERE")
		.ok()
		.and_then(|text| {
			let values: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
			(values.len() == 3).then(|| (values[0], values[1], values[2]))
		})
		.unwrap_or((ATMOSPHERE_LEVEL_POWER, ATMOSPHERE_TOP_LEVEL, ATMOSPHERE_OFFSET_POWER))
}

/// Radiance of a skybox texel c: SKY_RADIANCE · c² (notes/render.md "Sky").
const SKY_RADIANCE: f32 = 1.935;

/// The sky light's radiance over the sky's own (notes/render.md "Sky").
const SKY_LIGHT_SCALE: f32 = 1.044;

/// The sky light's cube resolution, texels a face side (sky-kernel: a white image over dark ones
/// lights the axis it faces with 0.5438 of a white sky and each neighbour with 0.1140, as 4 x 4
/// texels give, 0.5437 / 0.1141; finer cubes 0.554 / 0.112).
const SKY_LIGHT_TEXELS: u32 = 4;

/// The sky cube the reflections and the sky's irradiance start from, texels a face side.
const SKY_CUBE_TEXELS: u32 = 256;

/// The sky pass draws the skybox's images themselves, up to this many texels a side (a place's
/// 1024² skybox drawn from the 256 cube lost its clouds' edges: the lobby-sky experiment's "sky" runs 64-85 % within
/// 2 of 255, 98.8-99.9 % from the images).
const SKY_IMAGE_TEXELS: u32 = 1024;

/// The mesh of SpecialMesh MeshType Head (the classic rounded-cylinder head).
const HEAD_MESH: &str = "rbxasset://avatar/heads/head.mesh";

/// Neon emits 25.75 · c⁴ per channel (c the colour in 0..1), unlit (notes/render.md "Neon"); the
/// shader squares the instance colour (c²), this is the multiplier. 3 % over the first 25 fit:
/// next to beams and lit SmoothPlastic of known radiance Neon reads 1.028-1.031 times its model at
/// EV -2 and -1, and its bloom then starts at the same threshold (bloom-threshold).
fn neon_emission(_color: Vec3) -> f32 {
	25.75
}

/// Studio's level of detail for effects far from the camera: 1 within 200 studs, falling linearly to
/// 0 at 1,000. A beam keeps that share of its segments (at least a tenth; `beams::lod_segments`), an
/// emitter that share of what it emits by Rate (`particles::quads`; bursts keep all).
pub(crate) fn distance_lod(distance: f32) -> f32 {
	((1000.0 - distance) / 800.0).clamp(0.0, 1.0)
}

const TEX_COLOR: u32 = 1;
const TEX_NORMAL: u32 = 2;
const TEX_METALNESS: u32 = 4;
const TEX_ROUGHNESS: u32 = 8;
const TEX_MATERIAL: u32 = 16;
const TEX_ALPHA_TRANSPARENCY: u32 = 32;
const TEX_ALPHA_OVERLAY: u32 = 64;
const TEX_ALPHA_TINTMASK: u32 = 128;
const TEX_VERTEX_COLOR: u32 = 256;
const TEX_NORMAL_DXT5NM: u32 = 512;
const TEX_DECAL: u32 = 1024;
const TEX_DECAL_TILED: u32 = 2048;
const TEX_ORGANIC: u32 = 4096;
const TEX_EMISSIVE: u32 = 8192;
const TEX_SRGB: u32 = 16384;

/// A `file://` snapshot written for an `EditableImage` (which the client decodes with the square
/// rather than the sRGB curve on a `SurfaceAppearance` colour map).
fn is_editable_image_uri(uri: &str) -> bool {
	uri.starts_with("file://") && (uri.contains("/editable-images/") || uri.contains("\\editable-images\\"))
}
const TEX_DECAL_MESH_UV: u32 = 32768;
const TEX_DECAL_CENTERED: u32 = 65536;
const TEX_NORMAL_RG: u32 = 131072;
const TEX_WANG: u32 = 262144;
const TEX_VERTEX_TINT: u32 = 524288;
const TEX_ROUGHNESS_GREEN: u32 = 1048576;
const TEX_EMISSIVE_BLUE: u32 = 2097152;
const TEX_DECAL_FACE_ONLY: u32 = 4194304;
const TEX_VERTEX_ALPHA: u32 = 8388608;
const TEX_DECAL_BIND: u32 = 16777216;
const TEX_DECAL_UV: u32 = 33554432;
const TEX_DECAL_DOMINANT: u32 = 67108864;
const TEX_EMISSIVE_RED: u32 = 134217728;
const TEX_ALPHA_IGNORED: u32 = 268435456;
const TEX_GLASS_VERTEX_SKY: u32 = 1073741824;

/// Colour textures land this much darker than their decode (texture-decode; the shader's
/// TEXTURE_ALBEDO_SCALE), and so do Humanoid body parts' colours.
const COMPOSITED_ALBEDO: f32 = 0.971;

/// The Organic pattern's noise: Studio's 32 x 32 gray noise (notes/materials.md "Organic pattern").
const ORGANIC_NOISE: &str = "rbxasset://textures/noise.dds";
/// Studio's wang index: per cell of a Wang-tiled material (pre-2022), the atlas cell to show.
const WANG_INDEX: &str = "rbxasset://textures/wangIndex.dds";
/// Roblox's surface atlas (Studs, Glue / Weld, Inlet, Universal), albedo and DXT5nm normal
/// (notes/render.md "Surfaces").
const SURFACE_ALBEDO: &str = "rbxasset://textures/plastic/diffuse.dds";
const SURFACE_NORMAL: &str = "rbxasset://textures/plastic/normal.dds";

#[cfg(test)]
mod surface_tests {
	use super::*;

	/// A local mesh rewritten under the same path gets a new cache key (the render service lives on
	/// between jobs: a map tool may rewrite its planned meshes in place); asset ids keep theirs.
	#[test]
	fn a_rewritten_local_file_gets_a_new_cache_key() {
		let file = std::env::temp_dir().join(format!("ror-content-key-{}.mesh", std::process::id()));
		std::fs::write(&file, b"one").unwrap();
		let uri = format!("file:///{}", file.display().to_string().replace('\\', "/"));
		let first = content_key(&uri);
		std::fs::write(&file, b"three").unwrap();
		assert_ne!(first, content_key(&uri));
		assert_eq!(content_key(&format!("{uri}#lod=1")), format!("{}#lod=1", uri) + &content_key(&uri)[uri.len()..]);
		// a path without a scheme, and a drive's, are local files too
		let plain = file.display().to_string();
		std::fs::write(&file, b"one").unwrap();
		assert_ne!(content_key(&plain), plain);
		assert_ne!(content_key(&plain.replace('\\', "/")), plain.replace('\\', "/"));
		// A time-preserving copy whose mtime is older than 2 s and whose size stays equal still gets a new key.
		let old_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
		std::fs::write(&file, b"alpha").unwrap();
		std::fs::File::options().write(true).open(&file).unwrap().set_modified(old_mtime).unwrap();
		let old_before = content_key(&uri);
		std::fs::write(&file, b"bravo").unwrap();
		std::fs::File::options().write(true).open(&file).unwrap().set_modified(old_mtime).unwrap();
		assert_ne!(old_before, content_key(&uri), "same-size time-preserving copy with >2 s old mtime must change cache key");
		std::fs::remove_file(&file).unwrap();
		assert_eq!(content_key("rbxassetid://123"), "rbxassetid://123");
		assert_eq!(content_key(&transcode::image_key("rbxassetid://123")), transcode::image_key("rbxassetid://123"));
	}

	/// A character composite whose clothing template is a local file gets a stamped cache key (both
	/// in `content_key` and in prepared geometry keys) so replacing that local template redraws it.
	#[test]
	fn a_rewritten_local_clothing_template_changes_composite_key() {
		let file = std::env::temp_dir().join(format!("ror-composite-stamp-{}.png", std::process::id()));
		let old_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
		std::fs::write(&file, b"shirt-a").unwrap();
		std::fs::File::options().write(true).open(&file).unwrap().set_modified(old_mtime).unwrap();
		let uri = format!("file:///{}", file.display().to_string().replace('\\', "/"));
		let local_comp = crate::scene::character::Composite {
			layout: crate::scene::character::Layout::R6,
			colors: vec![[1.0, 1.0, 1.0]; 5],
			pants: None,
			shirt: Some(crate::scene::character::Layer { image: uri.clone(), tint: [1.0, 1.0, 1.0] }),
			graphic: None,
			overlay: None,
		};
		let local_key = composite::key(&local_comp);
		assert!(names_local_file(&local_key));
		let before = content_key(&local_key);
		std::fs::write(&file, b"shirt-b").unwrap();
		std::fs::File::options().write(true).open(&file).unwrap().set_modified(old_mtime).unwrap();
		assert_ne!(before, content_key(&local_key), "replaced local clothing template must invalidate composite key");
		let _ = std::fs::remove_file(&file);

		let remote_comp = crate::scene::character::Composite {
			layout: crate::scene::character::Layout::R6,
			colors: vec![[1.0, 1.0, 1.0]; 5],
			pants: None,
			shirt: Some(crate::scene::character::Layer { image: "rbxassetid://123".into(), tint: [1.0, 1.0, 1.0] }),
			graphic: None,
			overlay: None,
		};
		let remote_key = composite::key(&remote_comp);
		assert!(!names_local_file(&remote_key));
		assert_eq!(content_key(&remote_key), remote_key);
	}

	/// An unchanged local file keeps its key once its times pass the 2 s it is read and hashed in: a scene
	/// drawn from a file written just before was prepared again when drawn after that. A file first seen
	/// old is keyed by its stamp alone, unread.
	#[test]
	fn a_local_file_keeps_its_key_as_it_ages() {
		let dir = std::env::temp_dir().join(format!("ror-aging-key-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let uri = |file: &std::path::Path| format!("file:///{}", file.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let (seen, unseen) = (dir.join("seen.png"), dir.join("unseen.png"));
		std::fs::write(&seen, b"paint").unwrap();
		std::fs::write(&unseen, b"paint").unwrap();
		let fresh = content_key(&uri(&seen));
		let fresh_stamp = ror_assets::FileStamp::of(&seen).unwrap();
		assert!(fresh_stamp.is_content_hashed(), "fresh file should be content-hashed: {fresh}");
		assert_eq!(fresh, fresh_stamp.stamp_uri(&uri(&seen)));
		let age = |file: &std::path::Path| {
			let meta = std::fs::metadata(file).unwrap();
			let since = |time: Option<std::time::SystemTime>| time.and_then(|time| time.elapsed().ok()).unwrap_or_default();
			since(meta.modified().ok()).min(since(ror_assets::FileStamp::change_time(file, &meta)))
		};
		while age(&seen).min(age(&unseen)) < std::time::Duration::from_millis(2100) {
			std::thread::sleep(std::time::Duration::from_millis(100));
		}
		assert_eq!(content_key(&uri(&seen)), fresh, "the file's key changed as it aged");
		// Rewriting the same bytes keeps the exact same content key (zero reload on no-op rebuilds).
		std::fs::write(&seen, b"paint").unwrap();
		assert_eq!(content_key(&uri(&seen)), fresh, "identical-byte rewrite changed content key");
		let unseen_stamp = ror_assets::FileStamp::of(&unseen).unwrap();
		let unseen_os = ror_assets::FileStamp::os_stamp(&unseen).unwrap();
		if unseen_os.trusted {
			assert!(!unseen_stamp.is_content_hashed(), "file first seen old on a trusted filesystem should not be hashed");
		}
		assert_eq!(content_key(&uri(&unseen)), unseen_stamp.stamp_uri(&uri(&unseen)));
		let _ = std::fs::remove_dir_all(&dir);
	}

	/// A local image rewritten under its path is drawn anew while geometry prepared with its first
	/// version still holds that version's material: the service drew a repainted sheet's old paint for as
	/// long as renders followed one another.
	#[test]
	fn a_rewritten_local_image_gets_a_new_material() {
		let state = std::env::temp_dir().join(format!("ror-material-stamp-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let file = state.join("paint.png");
		let paint = |side: u32, rgba: [u8; 4]| crate::output::write_png(&file, side, side, &rgba.repeat((side * side) as usize), true).unwrap();
		let set_mtime = |time: std::time::SystemTime| {
			std::fs::File::options().write(true).open(&file).unwrap().set_modified(time).unwrap();
		};
		paint(4, [255, 0, 0, 255]);
		let uri = format!("file:///{}", file.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let key = MaterialKey { color: Some(uri.clone()), ..MaterialKey::default() };
		let mut missing = Vec::new();
		let first = renderer.material_bind_group(&key, &mut missing);
		assert!(Arc::ptr_eq(&first, &renderer.material_bind_group(&key, &mut missing)), "an unchanged image keeps its material");
		paint(8, [0, 255, 0, 255]);
		let meta_second = std::fs::metadata(&file).unwrap();
		let mtime_second = meta_second.modified().unwrap();
		let change_second = ror_assets::FileStamp::change_time(&file, &meta_second);
		let stamped_second = ror_assets::FileStamp::from_parts(&file, meta_second.len(), Some(mtime_second), change_second).stamp_uri(&uri);
		// `first` is still held, as the geometry prepared with it holds it
		let second = renderer.material_bind_group(&key, &mut missing);
		assert!(missing.is_empty(), "{missing:?}");
		assert!(!Arc::ptr_eq(&first, &second), "the rewritten image drew with its first version's material");
		assert_eq!(second.maps[0].as_ref().map(|map| map.width), Some(8));
		assert_eq!(first.maps[0].as_ref().map(|map| map.width), Some(4));
		// Rewriting again at the exact same dimensions, byte length, and mtime (and even when OS
		// timestamp ticks collide so len/mtime/ctime are all identical) still gets a new key & material.
		paint(8, [0, 0, 255, 255]);
		set_mtime(mtime_second);
		let meta_third = std::fs::metadata(&file).unwrap();
		assert_eq!(meta_third.len(), meta_second.len(), "test setup needs identical byte length");
		assert_eq!(meta_third.modified().unwrap(), mtime_second, "test setup needs identical mtime");
		assert_ne!(
			stamped_second,
			ror_assets::FileStamp::from_parts(&file, meta_third.len(), Some(mtime_second), change_second).stamp_uri(&uri),
			"same-tick recent rewrite with identical len/mtime/change_time must hash contents"
		);
		let third = renderer.material_bind_group(&key, &mut missing);
		assert!(missing.is_empty(), "{missing:?}");
		assert!(!Arc::ptr_eq(&second, &third), "same-size same-mtime rewrite kept its previous material");
		// A time-preserving copy whose mtime is older than the 2 s recent window still gets a new material
		// because NTFS ChangeTime / Unix ctime advances on the overwrite.
		let old_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
		set_mtime(old_mtime);
		let meta_old_before = std::fs::metadata(&file).unwrap();
		let fourth = renderer.material_bind_group(&key, &mut missing);
		paint(8, [255, 0, 0, 255]);
		set_mtime(old_mtime);
		let meta_old_after = std::fs::metadata(&file).unwrap();
		assert_eq!(meta_old_after.len(), meta_old_before.len(), "old-mtime rewrite needs identical byte length");
		assert_eq!(meta_old_after.modified().unwrap(), meta_old_before.modified().unwrap(), "old-mtime rewrite needs identical mtime");
		let fifth = renderer.material_bind_group(&key, &mut missing);
		assert!(missing.is_empty(), "{missing:?}");
		assert!(!Arc::ptr_eq(&fourth, &fifth), "same-size time-preserving copy with >2 s old mtime kept its previous material");
		// an asset id's key is kept as it is
		let asset = MaterialKey { color: Some("rbxassetid://123".into()), ..MaterialKey::default() };
		assert!(matches!(asset.stamped(), std::borrow::Cow::Borrowed(_)));
		drop((first, second, third, fourth, fifth, renderer));
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn surface_groups_pack_per_face() {
		let block = |material: u32, geometry: Geometry| crate::scene::Part {
			path: "Part".into(),
			class: "Part".into(),
			cframe: Affine3A::IDENTITY,
			size: Vec3::ONE,
			geometry,
			color: Vec3::ONE,
			material,
			material_variant: String::new(),
			transparency: 0.0,
			reflectance: 0.0,
			cast_shadow: true,
			shadow_only: false,
			double_sided: false,
			texture: None,
			vertex_color: Vec3::ONE,
			surface_appearance: None,
			decals: Vec::new(),
			// Right Weld, Top Studs, Back Glue, Left Inlet, Bottom Universal, Front Hinge
			surfaces: [2, 3, 1, 4, 5, 6],
			skeleton: None,
			body_part: None,
			composite: None,
			seed: 0,
			collision: None,
			referent: None,
		};
		assert_eq!(surface_groups(&block(256, Geometry::Block)), 2 | 1 << 4 | 2 << 8 | 3 << 12 | 4 << 16);
		// other materials and meshes draw none; a cylinder's caps (Right, Left) none
		assert_eq!(surface_groups(&block(272, Geometry::Block)), 0);
		assert_eq!(surface_groups(&block(256, Geometry::MeshPart { mesh: String::new(), initial_size: Vec3::ONE, fidelity: 0 })), 0);
		assert_eq!(surface_groups(&block(256, Geometry::Cylinder)), 1 << 4 | 2 << 8 | 4 << 16);
	}
}

/// A MeshPart without a mesh draws its box at this share of its albedo, as a vertex colour of 0.8
/// would (a Roblox client 0.747 and Studio alike: blue (60, 160, 230) 15, 82, 133 against a block's
/// 22, 112, 167 under flat Ambient, white 149 against 182, red 112 / 146, gray 58 / 82, Plastic and
/// SmoothPlastic; a FileMesh without a MeshId draws its box as bright as a block).
const MESHLESS_BOX_ALBEDO: f32 = 0.64;

/// `Enum.SurfaceType` values drawn as geometry of their own ([`Renderer::push_joints`]).
const SURFACE_HINGE: u32 = 6;
const SURFACE_MOTOR: u32 = 7;
const SURFACE_STEPPING_MOTOR: u32 = 8;
/// A joint's pieces: (length along the face normal, diameter, linear colour). The client's pin reads
/// 127, 127, 0 and its disc 44 where a white face reads 182 under flat Ambient 0.7: SmoothPlastic of
/// (0.7, 0.7, 0) and 0.345 gray (geo1 top, side and oblique views, within 1 of 255; under a low sun
/// the client's read 5 % darker, 119 and 41).
const JOINT_PIN: (f32, f32, Vec3) = (1.0, 0.4, Vec3::new(0.49, 0.49, 0.0));
const JOINT_DISC: (f32, f32, Vec3) = (0.25, 0.8, Vec3::splat(0.119));

/// The surface types a part shows (`InstanceData.flags.z`): per NormalId face 4 bits, 0 for none or
/// 1 + the atlas group (Studs, Glue / Weld, Inlet, Universal). Only Plastic primitives draw them, on
/// every face but a cylinder's caps (surfaces, surfaces-shapes: other materials, meshes and caps
/// show nothing); Hinge, Motor and SteppingMotor are geometry of their own.
fn surface_groups(part: &Part) -> u32 {
	if part.material != 256 || !matches!(part.geometry, Geometry::Block | Geometry::Wedge | Geometry::CornerWedge | Geometry::Cylinder | Geometry::Ball) {
		return 0;
	}
	let mut packed = 0;
	for (face, surface) in part.surfaces.iter().enumerate() {
		if matches!(part.geometry, Geometry::Cylinder) && (face == 0 || face == 3) {
			continue;
		}
		let group = match surface {
			3 => 1,
			1 | 2 => 2,
			4 => 3,
			5 => 4,
			_ => 0,
		};
		packed |= group << (4 * face);
	}
	packed
}

/// How a part's material textures and surface types map onto the drawn shape: planar per face on
/// primitives, projected on balls and meshes, the mesh's own uv without either. Surface types tile the
/// face's frame also on a Plastic without a texture pack (the pre-2022 Plastic: Studio 0.747 puts its studs
/// where the 2022 Plastic's are, rur's viewport-models in a place without Use2022Materials; with the mesh's
/// uv a cylinder's and a ball's studs came out larger and shifted).
fn uv_mode(flags: u32, is_mesh: bool, geometry: &Geometry, surfaces: u32) -> u32 {
	if flags & TEX_MATERIAL == 0 && surfaces == 0 {
		UV_MESH
	} else if is_mesh {
		UV_BOX
	} else if matches!(geometry, Geometry::Ball) {
		UV_BALL
	} else {
		UV_PLANAR
	}
}

/// How material textures map onto the drawn shape (`UV_*` in shaders/common.wgsl).
const UV_MESH: u32 = 0;
const UV_PLANAR: u32 = 1;
const UV_BALL: u32 = 2;
const UV_BOX: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Primitive {
	Block,
	Wedge,
	CornerWedge,
	Sphere,
	Cylinder,
}

#[derive(Clone, Default, PartialEq, Eq, Hash)]
struct MaterialKey {
	color: Option<String>,
	normal: Option<String>,
	metalness: Option<String>,
	roughness: Option<String>,
	emissive: Option<String>,
	/// Sampled clamped at the image's edges (a Decal's: Studio's does not repeat, decal-shapes),
	/// else repeating
	clamped: bool,
}

impl MaterialKey {
	/// Its textures, each with the use it loads for.
	fn textures(&self) -> impl Iterator<Item = (&str, TextureUse)> {
		[(&self.color, TextureUse::Color), (&self.normal, TextureUse::Normal), (&self.metalness, TextureUse::Scalar), (&self.roughness, TextureUse::Scalar), (&self.emissive, TextureUse::Color)]
			.into_iter()
			.filter_map(|(uri, usage)| uri.as_deref().map(|uri| (uri, usage)))
	}

	/// The key its binding is kept by: each local file's URI with its file's stamp ([`content_key`]),
	/// so an image rewritten under its path is another material's.
	fn stamped(&self) -> std::borrow::Cow<'_, MaterialKey> {
		if !self.textures().any(|(uri, _)| names_local_file(uri)) {
			return std::borrow::Cow::Borrowed(self);
		}
		let stamp = |uri: &Option<String>| uri.as_deref().map(content_key);
		std::borrow::Cow::Owned(MaterialKey { color: stamp(&self.color), normal: stamp(&self.normal), metalness: stamp(&self.metalness), roughness: stamp(&self.roughness), emissive: stamp(&self.emissive), clamped: self.clamped })
	}
}

/// A part's textures and how they apply.
struct PartMaterial {
	key: MaterialKey,
	flags: u32,
	params: MaterialParams,
	/// Material textures: studs per tile; their emission (linear) at full mask.
	studs_per_tile: f32,
	emissive: Vec3,
	/// Material textures: an offset in tiles (the random per-part shift of some materials).
	offset: [f32; 2],
	/// The Organic pattern: its noise's offset (noise uv) and frequency (noise uv per tile).
	noise_offset: [f32; 2],
	noise_frequency: f32,
	/// How strongly the material normal map tilts the surface (built-in packs; 1 as stored), and its
	/// falloff: (the view depth from which it holds, the power of depth / that depth it follows nearer).
	normal_strength: f32,
	normal_falloff: (f32, f32),
	/// A lobe widening with the view depth (built-in packs: render::materials::BuiltInTextures::lobe)
	lobe: (f32, f32),
	/// The part colour's level (render::materials::untextured_level: 2022 DiamondPlate's, drawn without its
	/// colour map); 1 elsewhere.
	albedo: f32,
	/// Material textures: the colour map's far tiling (far tiles per tile, 0: none) and 1 / the view
	/// depth where it has taken over (render::materials::FarTiling).
	far: [f32; 2],
	/// Provenance of any local TexturePack files read while resolving this material (empty when no
	/// local pack was read).
	pack_proofs: Vec<(String, resources::StampProof)>,
}

/// A stable pseudo-random offset in tiles for a part, standing in for the random one Studio picks
/// for some materials: from its seed, never from where it is now, so that it travels with the part
/// as it moves (a moment's position, rounded to its last bit by a lerp between equal keys, drew new
/// cracks on still CrackedLava rocks frame after frame).
fn part_offset(part: &Part) -> [f32; 2] {
	let hash = part.seed;
	[(hash & 0xffff) as f32 / 65536.0, ((hash >> 16) & 0xffff) as f32 / 65536.0]
}

/// A stable pseudo-random Organic noise offset (noise uv) for a built-in part, standing in for the
/// one Studio draws per part and load, independent of the texture's (organic-reset): from its seed,
/// as `part_offset`.
fn part_noise_offset(part: &Part) -> [f32; 2] {
	let hash = part.seed;
	[((hash >> 32) & 0xffff) as f32 / 65536.0, ((hash >> 48) & 0xffff) as f32 / 65536.0]
}

#[cfg(test)]
mod offset_tests {
	use super::*;

	#[test]
	fn random_offsets_travel_with_the_part() {
		// CrackedLava as loaded, and moved, turned, resized and a bit off where it was loaded: the same
		// texture and noise offsets as long as its seed (from where it was loaded) is the same
		let mut model = crate::scene::build::ModelBuilder::new("Rocks");
		model.part(&crate::scene::build::PartSpec { name: "Rock".into(), material: 804, ..Default::default() });
		let rock = Part::clone(&crate::scene::extract::extract(&model.into_document()).parts[0]);
		let mut moved = rock.clone();
		moved.cframe = Affine3A::from_rotation_y(0.4) * Affine3A::from_translation(Vec3::new(198.40268, -3.4036407, 202.14102));
		moved.size = Vec3::new(2.0, 3.0, 4.0);
		assert_eq!((part_offset(&moved), part_noise_offset(&moved)), (part_offset(&rock), part_noise_offset(&rock)));
		let mut nudged = rock.clone();
		nudged.cframe.translation.x = f32::from_bits(nudged.cframe.translation.x.to_bits() + 1);
		assert_eq!(part_offset(&nudged), part_offset(&rock));
		// another part draws its own
		let other = Part { seed: crate::scene::part_seed(Vec3::X, Vec3::ONE), ..rock.clone() };
		assert_ne!(part_offset(&other), part_offset(&rock));
	}
}

/// Lighting derived from a sky, cached per sky.
pub struct EnvironmentMaps {
	sky: wgpu::TextureView,
	/// The skybox's own images, drawn in their detail by the sky pass (else it draws `sky`)
	images: Option<SkyImages>,
	specular: wgpu::TextureView,
	/// The chain distant surfaces fade into through the Atmosphere: `specular` unless SkyboxOrientation
	/// turns the skybox, which the fade does not take
	faded: wgpu::TextureView,
	specular_levels: u32,
	sh: [Vec3; 9],
	cube: [Vec3; 6],
	/// What the haze drawn adds to the reflected one's, per cube face (frame `haze_light`; the
	/// specular chain's alpha holds the haze's share, blurred level by level as the colour)
	haze: Option<[Vec3; 6]>,
	bytes: u64,
}

/// One skybox image: its texels as stored (RGBA8) read as radiance, the same at the sky light's
/// resolution, and its size; a BC1 image's own blocks and mips too.
struct SkyFaceImages {
	radiance: environment::SkyFace,
	light: environment::SkyFace,
	blocks: Option<Vec<Vec<u8>>>,
	width: u32,
	height: u32,
	/// True when the face is remote or its local file bytes were proven to match its snapshot.
	verified: bool,
}

/// The skybox's six images for the sky pass (shaders/sky.wgsl `skybox`): a 2D array in Bk, Dn, Ft,
/// Lf, Rt, Up order with mips, its size in bytes, and what the pass does with them as the cube had.
struct SkyImages {
	view: wgpu::TextureView,
	bytes: u64,
	rotation: glam::Mat3,
	tint: Option<(Vec3, Vec3)>,
	atmosphere: Option<environment::AtmosphereParams>,
}

impl Weighed for EnvironmentMaps {
	fn bytes(&self) -> u64 {
		self.bytes
	}
}

/// The sky's maps for a scene's Lighting, made ahead of its render ([`Renderer::prepare_environment`]),
/// what was missing making them, and the scene's preparation key.
pub struct PreparedSky {
	maps: Arc<EnvironmentMaps>,
	missing: Vec<String>,
	key: Option<Arc<prepared::SceneKey>>,
	/// Every emitter's particles, made with the rest ahead of the draw thread (`particles::simulate_all`)
	particles: Option<Vec<Vec<particles::Particle>>>,
	/// Local skybox faces hashed in full because their volume provided no trusted OS stamp.
	untrusted_hashes: usize,
	/// CPU time building environment maps and the scene preparation key.
	pub(crate) env_ms: f64,
	/// CPU time running [`particles::simulate_all`] for this frame.
	pub(crate) particle_sim_ms: f64,
}

/// The effects' textures and bind groups a frame drew with ([`Renderer::effect_bindings`]): the
/// next frame whose emitters, beams and trails name the same textures in the same order takes them as
/// they are. A recorded fight's 368 emitters and 71 beams (145 textures) looked their textures up
/// and made their bind groups again every frame, 1.1 ms of each frame's draw thread.
struct EffectBindings {
	/// Of the scene's effects' textures in order
	hash: u64,
	/// The textures, and the views their groups bind
	held: Vec<(Arc<GpuTexture>, wgpu::TextureView)>,
	particles: Vec<(Arc<wgpu::BindGroup>, bool)>,
	beams: Vec<Option<Arc<wgpu::BindGroup>>>,
	trails: Vec<Option<Arc<wgpu::BindGroup>>>,
}

/// Effect bindings kept (a clip's, a few clips' at once)
const EFFECT_BINDINGS_KEPT: usize = 4;

/// The effects' textures in order (`EffectBindings::hash`), stamping any local files and returning
/// the observed `(uri -> stamped_uri)` map so `effect_bindings` only caches bindings whose loaded
/// textures match the stamps hashed into `hash`.
fn effect_hash(scene: &Scene) -> (u64, HashMap<String, String>, usize) {
	use std::hash::{Hash, Hasher};
	let mut hasher = std::collections::hash_map::DefaultHasher::new();
	let mut local_keys: HashMap<String, String> = HashMap::new();
	let mut untrusted = 0usize;
	let mut hash_uri = |uri: Option<&str>, hasher: &mut std::collections::hash_map::DefaultHasher| match uri {
		None => false.hash(hasher),
		Some(uri) => {
			true.hash(hasher);
			let key = transcode::image_key(uri);
			if names_local_file(&key) {
				let stamped = local_keys.entry(key.clone()).or_insert_with(|| {
					let (stamped, is_untrusted) = content_key_checked(&key);
					untrusted += usize::from(is_untrusted);
					stamped
				});
				stamped.hash(hasher);
			} else {
				key.hash(hasher);
			}
		}
	};
	scene.particles.len().hash(&mut hasher);
	for emitter in &scene.particles {
		hash_uri(emitter.texture.as_deref(), &mut hasher);
	}
	scene.beams.len().hash(&mut hasher);
	for beam in &scene.beams {
		hash_uri(beam.texture.as_deref(), &mut hasher);
	}
	scene.trails.len().hash(&mut hasher);
	for trail in &scene.trails {
		hash_uri(trail.texture.as_deref(), &mut hasher);
	}
	(hasher.finish(), local_keys, untrusted)
}

/// The preparation key of frames whose parts are all the same ([`Renderer::prepare_environment_sharing`]).
#[derive(Default)]
pub struct PartsKey(std::sync::OnceLock<Option<Arc<prepared::SceneKey>>>);

/// What a sky's maps start from, decoded once per skybox and orientation: the skybox as a cube at
/// the sky's resolution and at the sky light's, and its own images for the sky pass. A clip whose
/// Lighting changes (the time of day, the Atmosphere) makes its maps again every frame from these.
struct SkyBase {
	cube: environment::CubeImage,
	light: environment::CubeImage,
	/// The same two cubes without SkyboxOrientation's turn, when it turns the skybox: what the sky
	/// light and the Atmosphere's fade read (sky-orientation)
	upright: Option<(environment::CubeImage, environment::CubeImage)>,
	images: Option<(wgpu::TextureView, u64)>,
	bytes: u64,
	/// True when every local skybox face was proven to match its snapshot (`false` if any moved).
	stable: bool,
}

impl Weighed for SkyBase {
	fn bytes(&self) -> u64 {
		self.bytes
	}
}

/// A texture made once, always held whole, and its view.
struct Solid {
	view: wgpu::TextureView,
	_texture: GpuTexture,
}

impl From<GpuTexture> for Solid {
	fn from(texture: GpuTexture) -> Self {
		Self { view: texture.view(), _texture: texture }
	}
}

struct Defaults {
	no_bloom: wgpu::TextureView,
	/// A 1 x 1 array of six: no skybox images (the sky pass draws its cube)
	no_sky_images: wgpu::TextureView,
	white: Solid,
	normal: Solid,
	black: Solid,
	material_sampler: wgpu::Sampler,
	/// The material sampler clamped at the edges (Decals)
	clamped_sampler: wgpu::Sampler,
	linear_sampler: wgpu::Sampler,
	shadow_sampler: wgpu::Sampler,
	brdf_lut: wgpu::TextureView,
	empty_shadow: wgpu::TextureView,
	/// A one-texel moment atlas: no sun shadow
	empty_moments: wgpu::TextureView,
	/// A one-layer array: no local light shadow maps
	empty_light_shadows: wgpu::TextureView,
	/// One identity matrix: no local light shadow faces
	no_light_matrices: wgpu::Buffer,
	/// A view's light clusters when it sorts none ([`light_clusters`])
	no_light_clusters: wgpu::Buffer,
	/// One unlit voxel: no baked voxel field
	no_voxel_field: wgpu::TextureView,
	/// A 1 x 1 x 1 open sky: no light grid
	no_light_grid: wgpu::TextureView,
	/// A 1 x 1 x 1 cloud volume: no clouds
	no_clouds: wgpu::TextureView,
	/// The cloud volume's sampler, repeating every way
	cloud_sampler: wgpu::Sampler,
	/// Studio's wang index (a 1x1 stand-in without a Studio install: Wang materials then tile
	/// regularly).
	wang_index: Solid,
	wang: bool,
}

pub struct Renderer {
	pub gpu: Arc<Gpu>,
	pub assets: ror_assets::Assets,
	inner: Arc<RendererInner>,
}

/// Shared GPU pipelines and caches behind [`Renderer`], so online and offline handles on one service
/// share every compiled pipeline and cached GPU resource while differing only in [`Renderer::assets`].
#[doc(hidden)]
pub struct RendererInner {
	pipelines: Arc<Pipelines>,
	defaults: Defaults,
	primitives: HashMap<Primitive, Arc<GpuMesh>>,
	meshes: Cache<String, GpuMesh>,
	textures: Cache<(String, TextureUse), GpuTexture>,
	packs: Cache<String, TexturePack>,
	environments: Cache<String, EnvironmentMaps>,
	skies: Cache<String, SkyBase>,
	materials: Mutex<HashMap<MaterialKey, std::sync::Weak<MaterialBinding>>>,
	/// Skinned meshes posed by their Bones (skinning.rs)
	skins: skinning::Skins,
	beams: Arc<beams::BeamPipelines>,
	/// Per-part id and Highlight mask pipelines, built lazily the first time a scene draws
	/// Highlights, `--ids`, or `--layers` so plain renders neither compile nor cache them.
	ids: std::sync::OnceLock<ids::IdPipelines>,
	particles: Arc<particles::ParticlePipelines>,
	/// Image-space post effects (post.rs)
	post: Arc<post::PostPipelines>,
	guis: Arc<guis::GuiPipelines>,
	/// SurfaceGui / BillboardGui content drawn by rur (guis.rs)
	gui_textures: Cache<guis::GuiKey, guis::GuiTexture>,
	gui_failures: guis::GuiFailures,
	/// A clip's GUI uploads' staging and its frames' own GUI textures, kept between frames
	gui_uploads: Arc<guis::GuiUploads>,
	/// The targets and buffers of finished renders, for the renders after them (reuse.rs)
	reuse: reuse::Reuse,
	preparations: prepared::Preparations,
	shadows: shadow_reuse::Shadows,
	nv12: std::sync::OnceLock<nv12::Conversion>,
	/// The see-through layers' pipelines ([`ViewRequest::layers`]), made when first asked for
	layers: std::sync::OnceLock<ids::LayerPipelines>,
	/// The lights' voxels' bake, made the first time a view has lit effects ([`voxel_field`])
	voxel_field: std::sync::OnceLock<voxel_field::Baker>,
	/// Studio's cloud volume, loaded the first time a view draws Terrain Clouds ([`clouds`]), or why it
	/// could not be
	clouds: std::sync::OnceLock<Result<clouds::Volumes, String>>,
	/// The cloud volume's 2D stand-in the lowest looks lay flat ([`clouds::load_flat`]), loaded the first
	/// time a view draws them, or why it could not be
	clouds_flat: std::sync::OnceLock<Result<(wgpu::TextureView, wgpu::TextureView), String>>,
	/// What kept textures' levels go up through (texture_files)
	staging: texture_files::Staging,
	/// The bytes of material maps a render's views must be able to save for their levels to be read
	/// by the views' feedback (render::residency), not whole
	residency_from: std::sync::atomic::AtomicU64,
	/// The effects' bindings of the last frames drawn ([`EffectBindings`])
	effect_bindings: Mutex<Vec<Arc<EffectBindings>>>,
	/// Scene keys frames made ahead still hold ([`Renderer::prepare_environment`]), shared by equal ones
	recent_keys: Mutex<Vec<std::sync::Weak<prepared::SceneKey>>>,
}

impl std::ops::Deref for Renderer {
	type Target = RendererInner;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

/// The share of the GPU's memory budget a process's renders may fill (`ROR_GPU_ROOM_PERCENT`, default 90).
fn gpu_room_percent() -> u64 {
	static PERCENT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
	*PERCENT.get_or_init(|| std::env::var("ROR_GPU_ROOM_PERCENT").ok().and_then(|value| value.trim().parse().ok()).unwrap_or(90).clamp(1, 100))
}

/// The start of a load's error when the GPU has no room left for it ([`Renderer::gpu_room`]).
pub const GPU_FULL: &str = "GPU memory nearly full: this scene needs more than this GPU holds; render fewer parts at a time";

/// What [`Renderer::release_idle`] let go of, in bytes.
pub struct Released {
	/// Targets, buffers, preparations and shadow maps kept for later renders
	pub kept: u64,
	pub guis: u64,
	pub skins: u64,
	/// Downloaded files' bytes kept in memory (what the GPU holds came from them; the disk cache has
	/// them again)
	pub assets: u64,
}

/// A material's bind group with the textures it binds. Draws hold it and so the textures, which the
/// texture cache keeps while they are held (`Cache::shrink_to`): a part prepared after a scene's
/// textures passed the cache's budget finds its material and textures rather than loading second
/// copies (a map under a 32 MB texture cache held 3.3 GB of GPU memory so). The material map
/// only points at it, so nothing outlives the draws that use it (kept for good, materials held
/// dropped textures past the cache's budget: a sweep of 180 batches of effects in one process
/// reached 6.8 GB).
///
/// Its maps may hold only their coarser levels (render::residency): each view reads what it needs of
/// them first and takes its bind group then ([`Renderer::material_group`]), made again when they hold
/// more than when it was made.
struct MaterialBinding {
	/// Colour, normal, metalness, roughness, emissive (`None`: none, or one that did not load)
	maps: [Option<Arc<GpuTexture>>; 5],
	clamped: bool,
	/// The maps' full sizes and residency slots (mesh.wgsl MaterialInfo)
	info: wgpu::Buffer,
	/// The sampler and `info`, for the residency feedback
	feedback: wgpu::BindGroup,
	/// The bind group last made, and the levels each map held then
	group: Mutex<Option<([u32; 5], wgpu::BindGroup)>>,
}

/// A resource cache's byte budget: `ROR_<NAME>_CACHE_MB` megabytes when set, else `default_mb`.
fn cache_budget(name: &str, default_mb: u64) -> u64 {
	std::env::var(format!("ROR_{name}_CACHE_MB")).ok().and_then(|value| value.trim().parse::<u64>().ok()).unwrap_or(default_mb) << 20
}

/// A part's draw: which mesh and index range, its instance slot and material bind group.
#[derive(Clone)]
struct DrawItem {
	mesh: Arc<GpuMesh>,
	indices: Range<u32>,
	/// A decal's: the runs of `indices` it can draw on, drawn instead of them all
	/// ([`Renderer::decal_runs`]; `None`: all of them)
	runs: Option<Arc<[Range<u32>]>>,
	instance: u32,
	material: Arc<MaterialBinding>,
	transparent: bool,
	double_sided: bool,
	casts_shadow: bool,
	/// Shadows local lights with Shadows (as the sun: `scene::casts_shadow`)
	casts_light_shadow: bool,
	/// Its shadows (the sun's and local lights') only where its colour map's alpha times its opacity is
	/// over the cut (a SurfaceAppearance in AlphaMode Transparency: `fs_shadow_cutout`)
	shadow_cutout: bool,
	center: Vec3,
	/// A sphere round what it draws, in world space (which sun cascades it can shadow)
	bound: (Vec3, f32),
	/// The posed mesh vertices in world space, for clip-volume culling.
	drawn_bounds: culling::Bounds,
	/// A Decal / Texture over its part (drawn after the opaque parts, in `decal_order`)
	decal: bool,
	/// ForceField: reads the opaque scene's depth (its contact glow)
	forcefield: bool,
	/// A transparent part that also writes depth where it is opaque (`fs_cutout_depth`)
	write_depth: bool,
	/// Glass with Transparency: tints what is behind it (`fs_glass_multiply`, then `fs_glass_add`)
	glass: bool,
	/// (part index, ZIndex, decal index): decals of one part in ZIndex order
	decal_order: (u32, i32, u32),
	/// An invisible part a Highlight covers, drawn only into the Highlights' mask, or a shadow-only
	/// part, drawn only into the shadow maps
	highlight_only: bool,
	/// Drawn into the ambient occlusion's depth and normals when opaque (not a joint's pin and disc:
	/// [`Renderer::push_joints`])
	occludes: bool,
}

impl DrawItem {
	/// The same draw: the same mesh, material and decal runs, and every other field the same bits
	/// (prepared::Layout::difference).
	fn same(&self, other: &DrawItem) -> bool {
		let runs = match (&self.runs, &other.runs) {
			(None, None) => true,
			(Some(a), Some(b)) => a[..] == b[..],
			_ => false,
		};
		Arc::ptr_eq(&self.mesh, &other.mesh)
			&& Arc::ptr_eq(&self.material, &other.material)
			&& runs
			&& self.indices == other.indices
			&& self.instance == other.instance
			&& (self.transparent, self.double_sided, self.casts_shadow, self.casts_light_shadow, self.shadow_cutout, self.decal, self.forcefield, self.write_depth, self.glass, self.highlight_only, self.occludes)
				== (other.transparent, other.double_sided, other.casts_shadow, other.casts_light_shadow, other.shadow_cutout, other.decal, other.forcefield, other.write_depth, other.glass, other.highlight_only, other.occludes)
			&& self.center.to_array().map(f32::to_bits) == other.center.to_array().map(f32::to_bits)
			&& self.bound.0.to_array().map(f32::to_bits) == other.bound.0.to_array().map(f32::to_bits)
			&& self.bound.1.to_bits() == other.bound.1.to_bits()
			&& self.drawn_bounds == other.drawn_bounds
			&& self.decal_order == other.decal_order
	}

	/// Its triangles for `instance` on a pass whose pipeline, buffers and groups are set: a decal's
	/// runs, else its whole range.
	fn draw_indexed(&self, pass: &mut wgpu::RenderPass<'_>) {
		match &self.runs {
			Some(runs) => {
				for run in runs.iter() {
					pass.draw_indexed(run.clone(), 0, self.instance..self.instance + 1);
				}
			}
			None => pass.draw_indexed(self.indices.clone(), 0, self.instance..self.instance + 1),
		}
	}

	fn in_frustum(&self, frustum: &culling::Frustum) -> bool {
		// a part a view's levels of detail leave out (render::levels) draws nothing anywhere
		if self.indices.is_empty() && self.mesh.lods.first().is_some_and(|indices| !indices.is_empty()) {
			return false;
		}
		// Posed bounds describe LOD0, whose vertices the chain's lower levels draw a subset of (a view's
		// levels of detail, render::levels; `ROR_TUNE_LOD`). Anything else stays conservative.
		self.mesh.lods.iter().all(|indices| *indices != self.indices) || frustum.intersects(self.drawn_bounds)
	}
}

/// A blended draw: a transparent part, a beam, a trail, an emitter's particles, a legacy effect's or a GUI.
enum Blended<'a> {
	Part(&'a DrawItem),
	/// a see-through part's decal, after its part
	Decal(&'a DrawItem),
	Beam(&'a beams::BeamDraw),
	Trail(&'a trails::TrailDraw),
	Particles(&'a particles::ParticleDraw),
	/// a legacy effect's particles, blended in linear light in a layer of their own composited at their
	/// place in the order (render::particles)
	Legacy(&'a particles::ParticleDraw),
	Gui(&'a guis::ViewGui),
}

/// A run of the blend order drawn together: what blends as the scene takes it, or legacy effects in their
/// linear-light layer.
#[derive(Clone, Debug, PartialEq)]
enum BlendRun {
	Blended(std::ops::Range<usize>),
	Legacy(std::ops::Range<usize>),
}

/// The box a Roblox client's occlusion culling tests an effect by, where it stands before its ZOffset (as
/// `ror cost` boxes it): an emitter's particles, each a square of twice its size round its place.
fn particles_box(particles: &[particles::Particle]) -> Option<(Vec3, Vec3)> {
	particles.iter().fold(None, |bounds, particle| {
		let half = Vec3::splat(particle.size.max(0.0));
		let (low, high) = (particle.position - half, particle.position + half);
		Some(bounds.map_or((low, high), |(min, max): (Vec3, Vec3)| (min.min(low), max.max(high))))
	})
}

/// A beam's box for the occlusion culling: its ends, widened by half its width and half its CurveSize
/// either way (cost::Analysis's).
fn beam_box(beam: &crate::scene::Beam) -> (Vec3, Vec3) {
	let (a, b) = (Vec3::from(beam.attachment0.translation), Vec3::from(beam.attachment1.translation));
	let reach = (beam.width0.abs().max(beam.width1.abs()) + beam.curve_size0.abs().max(beam.curve_size1.abs())) * 0.5;
	(a.min(b) - Vec3::splat(reach), a.max(b) + Vec3::splat(reach))
}

/// Of `effects` (an emitter's or a beam's index and its box), those whose box lies wholly behind what a
/// Roblox client's occlusion culling rasterizes from `camera` (cost::occluding, of the scene's parts that
/// stand in the view nearer than the farthest effect; `meshes` loads their meshes).
fn hidden_effects(scene: &Scene, effects: &[(usize, (Vec3, Vec3))], camera: &Camera, aspect: f32, meshes: impl FnOnce(&[Arc<Part>]) -> crate::cost::meshes::Meshes) -> Vec<usize> {
	if effects.is_empty() {
		return Vec::new();
	}
	let forward = -Vec3::from(camera.world.matrix3.z_axis);
	let depths = |(low, high): (Vec3, Vec3)| {
		(0..8).map(|corner| Vec3::new(if corner & 1 == 0 { low.x } else { high.x }, if corner & 2 == 0 { low.y } else { high.y }, if corner & 4 == 0 { low.z } else { high.z })).map(|point| (point - camera.eye).dot(forward)).fold((f32::INFINITY, f32::NEG_INFINITY), |(near, far), depth| (near.min(depth), far.max(depth)))
	};
	let farthest = effects.iter().map(|(_, bounds)| depths(*bounds).1).fold(f32::NEG_INFINITY, f32::max);
	let view = crate::cost::cull::Culler::new(camera, aspect);
	let near: Vec<&Arc<Part>> = scene
		.parts
		.iter()
		.filter(|part| {
			let bounds = crate::cost::cull::world_box(&part.cframe, part.size);
			view.in_frustum(bounds) && depths(bounds).0 < farthest
		})
		.collect();
	let meshes = meshes(&near.iter().filter(|part| matches!(part.geometry, Geometry::MeshPart { .. })).map(|part| Arc::clone(part)).collect::<Vec<_>>());
	let occluders: Vec<crate::cost::Occluder> = near.iter().filter_map(|part| crate::cost::part_occluder(part, &meshes)).collect();
	if occluders.is_empty() {
		return Vec::new();
	}
	let culler = crate::cost::occluding(&occluders, camera, aspect);
	effects.iter().filter(|(_, bounds)| !culler.visible(*bounds)).map(|(index, _)| *index).collect()
}

/// The blend order cut into runs (`legacy[i]`: whether its i-th draw is a legacy effect's): what blends up
/// to the first legacy effect (drawn with the scene, perhaps nothing), then each run of legacy effects and
/// each run of what blends after one, in order.
fn blend_runs(legacy: &[bool]) -> Vec<BlendRun> {
	let first = legacy.iter().position(|&is| is).unwrap_or(legacy.len());
	let mut runs = vec![BlendRun::Blended(0..first)];
	let mut start = first;
	while start < legacy.len() {
		let kind = legacy[start];
		let end = legacy[start..].iter().position(|&is| is != kind).map_or(legacy.len(), |offset| start + offset);
		runs.push(if kind { BlendRun::Legacy(start..end) } else { BlendRun::Blended(start..end) });
		start = end;
	}
	runs
}

/// Shadow maps a frame draws that the shadow cache keeps, once the frame is submitted.
struct PendingShadows {
	key: Vec<u8>,
	maps: shadow_reuse::Maps,
	texture: wgpu::Texture,
}

/// What a frame bind group gives the shader of local lights: a view's lights, their shadow maps
/// (six layers each) and those maps' view-projections.
struct LightBindings<'a> {
	lights: &'a wgpu::Buffer,
	shadow_maps: &'a wgpu::TextureView,
	shadow_matrices: &'a wgpu::Buffer,
	/// Their voxels baked for the view ([`voxel_field`])
	voxel_field: &'a wgpu::TextureView,
	/// Their clusters of the view ([`light_clusters`])
	clusters: &'a wgpu::Buffer,
}

/// A scene ready to draw from any camera.
struct Prepared {
	geometry: Arc<prepared::PreparedGeometry>,
	/// While a view draws under levels of detail (`geometry` its copy, render::levels): the shared
	/// preparation, which its shadow maps draw from, at full detail as Studio draws shadows (a
	/// Performance mesh at its third level cast its full shadow, notes/cost.md "Levels of detail")
	casters: Option<Arc<prepared::PreparedGeometry>>,
	immutable: bool,
	/// Every local light (each view binds those it can see, `view_lights`)
	lights: Vec<LightData>,
	environment: Arc<EnvironmentMaps>,
	/// GlobalShadows with the sun or moon lighting: each view draws its cascades (`sun_cascades`)
	sun_shadows: bool,
	bounds: Option<(Vec3, Vec3)>,
	/// Lighting's directional light (environment::direct_light): towards it, and its strength as a
	/// share of Brightness
	sun_direction: Vec3,
	sun_strength: Vec3,
	/// Each beam's texture bind group (`None`: its texture cannot be loaded, it draws nothing)
	beam_textures: Vec<Option<Arc<wgpu::BindGroup>>>,
	/// Each trail's texture bind group (`None` as beams')
	trail_textures: Vec<Option<Arc<wgpu::BindGroup>>>,
	/// The scene's Highlights, when any covers a drawn part.
	highlights: Option<highlights::PreparedHighlights>,
	/// Every emitter's particles and texture
	particles: particles::PreparedParticles,
}

impl std::ops::Deref for Prepared {
	type Target = prepared::PreparedGeometry;

	fn deref(&self) -> &Self::Target {
		&self.geometry
	}
}

impl Prepared {
	/// What shadow maps draw (and the shadow cache keys on): the shared preparation, at full detail.
	fn casters(&self) -> &Arc<prepared::PreparedGeometry> {
		self.casters.as_ref().unwrap_or(&self.geometry)
	}
}

/// A thread pool for parallel work inside cache loaders (see [`Renderer::environment`]).
pub(crate) fn inner_pool() -> &'static rayon::ThreadPool {
	static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
	POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(crate::worker_threads()).thread_name(|index| format!("ror-inner-{index}")).build().expect("inner thread pool"))
}

/// Roblox's colour decode: a square (gamma 2.0), measured against Studio (notes/render.md).
fn linear(color: Vec3) -> Vec3 {
	color * color
}

/// Studio's ambient occlusion (render::ao); `ROR_NO_AO` turns it off (decomposing renders).
fn ambient_occlusion_enabled() -> bool {
	static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ENABLED.get_or_init(|| std::env::var("ROR_NO_AO").is_err())
}

/// `ROR_NAN_CHECK`: a pixel whose light is not a finite number shows magenta (resolve.wgsl), for
/// finding what makes one (examples/vfx_fuzz.rs).
fn nan_check() -> bool {
	static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ENABLED.get_or_init(|| std::env::var_os("ROR_NAN_CHECK").is_some())
}

/// Studio's light grid (render::lightgrid); `ROR_NO_LIGHT_GRID` leaves it open everywhere
/// (decomposing renders).
fn light_grid_enabled() -> bool {
	static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ENABLED.get_or_init(|| std::env::var("ROR_NO_LIGHT_GRID").is_err())
}

/// The key a scene's preparation is kept under (`Preparations`), none for a live scene: live skinning
/// writes its vertex buffers in place, and such meshes must never enter an immutable preparation or
/// shadow cache, even if a frame's pose matches an older one.
fn scene_key(scene: &Scene) -> Option<Arc<prepared::SceneKey>> {
	let neon_share = scene.environment.viewport.as_ref().map_or(1.0, |_| environment::viewport_neon());
	(!scene.live).then(|| prepared::key(scene, neon_share, scene.environment.global_shadows && light_grid_enabled()))
}

/// OutdoorAmbient where the light grid is open, with GlobalShadows (lg-height, lg-terms: 0.953 open
/// against 0.967 with GlobalShadows off or under Ambient).
const LIGHT_GRID_OPEN: f32 = 0.986;

/// The light grid's voxels as a 3D texture (RG8: the sky's visibility, the sun's light where its shadow maps are not
/// drawn or 1; X fastest, then Y, then Z), sampled trilinearly, in a texture `make` gives (a render's lease's, or a
/// new one).
fn light_grid_texture(gpu: &Gpu, make: impl FnOnce(&wgpu::TextureDescriptor) -> wgpu::Texture, dims: [u32; 3], values: &[u8], sun: Option<&[u8]>) -> wgpu::TextureView {
	let size = wgpu::Extent3d { width: dims[0], height: dims[1], depth_or_array_layers: dims[2] };
	let texels: Vec<u8> = match sun {
		Some(sun) => values.iter().zip(sun).flat_map(|(&sky, &sun)| [sky, sun]).collect(),
		None => values.iter().flat_map(|&sky| [sky, 255]).collect(),
	};
	let texture = make(&wgpu::TextureDescriptor {
		label: Some("light grid"),
		size,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D3,
		format: wgpu::TextureFormat::Rg8Unorm,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	gpu.queue.write_texture(
		texture.as_image_copy(),
		&texels,
		wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(dims[0] * 2), rows_per_image: Some(dims[1]) },
		size,
	);
	texture.create_view(&Default::default())
}

/// A local light at full strength gives x = 1.98 · Brightness · Color^2.2 · albedo · n·l (notes/render.md
/// "Local lights": light-falloff, light-range); `direct_light` divides the albedo by π.
const LIGHT_SCALE: f32 = 1.98;

/// How far a light's voxels reach (studs; common.wgsl `voxel_light`): its fade ends at 1.007 · Range,
/// and a voxel's neighbours and the trilinear read take it two voxels further along each axis.
fn voxel_reach(range: f32) -> f32 {
	1.007 * range + 8.0 * 3f32.sqrt()
}

/// Shadow maps of local lights with Shadows: six faces around the light (its position, behind a
/// SurfaceLight's face) for every shadowed light a view reaches, up to MAX_SHADOW_LIGHTS (each costs
/// a pixel it reaches 16 compares). Studio shadows all of them (light-many: 24 in view, each over
/// its own slab), so the faces get fewer texels the more lights there are: 1024 for up to 4, 512 for
/// up to 16, 256 beyond, the same memory (the filter keeps its width in the light's view).
const MAX_SHADOW_LIGHTS: usize = 64;
fn light_shadow_size(lights: usize) -> u32 {
	match lights {
		0..=4 => 1024,
		5..=16 => 512,
		_ => 256,
	}
}
/// Each face's field of view: wider than 90° so the filter taps around a face's edge stay on it.
const LIGHT_SHADOW_FOV: f32 = 100.0;
/// From this distance to the camera surfaces take local light from the lights' voxels alone, which
/// no part shadows (light-far; common.wgsl LOCAL_LIGHT_FAR_END): a light whose reach lies wholly
/// beyond it needs no shadow maps.
const LOCAL_LIGHT_FAR_END: f32 = 600.0;
/// From this distance surfaces start taking the lights' voxels (common.wgsl LOCAL_LIGHT_FAR_START)
const LOCAL_LIGHT_FAR_START: f32 = 580.0;

/// Whether every surface takes its local light from the lights' voxels, near and far, as under
/// LightingStyle Soft and at a graphics quality level that lights none per pixel (below 14, 17 under
/// PrioritizeLightingQuality false: crate::quality::Quality::light_reach; mesh.wgsl `lights_from_voxels`,
/// frame.counts.w bit 1): then no light gets shadow maps, a light's clusters reach its voxels' reach, and
/// the view's baked field holds the voxels where surfaces read them ([`voxel_field::surfaces`]).
fn lights_from_voxels(environment: &crate::scene::Environment, quality: &crate::quality::Quality) -> bool {
	environment.soft_lighting() || quality.light_reach <= 0.0
}

/// Where a view's surfaces start taking the local lights' voxels and where they take them alone, from the
/// camera: LOCAL_LIGHT_FAR_START and _END at a level lighting them per pixel to level 21's reach (and at one
/// lighting none per pixel, where every surface takes the voxels: [`lights_from_voxels`]), else a handover as
/// wide from 5 studs short of the level's reach (the client's `lightline`: a rail's shadow from lamps every 20
/// studs ends ~55 studs out at level 14, 104, 175, 275 and 410 at 15-18, the lamps' voxels lighting the rail
/// past it; crate::quality::Quality::light_reach). Handovers centred on the reach ended the shadow 5-8 studs
/// short of the client's, ending there 10-17 (with the voxels' light past it).
fn local_light_far(quality: &crate::quality::Quality) -> (f32, f32) {
	if quality.light_reach.is_finite() && quality.light_reach > 0.0 {
		let width = LOCAL_LIGHT_FAR_END - LOCAL_LIGHT_FAR_START;
		((quality.light_reach - 0.25 * width).max(0.0), quality.light_reach + 0.75 * width)
	} else {
		(LOCAL_LIGHT_FAR_START, LOCAL_LIGHT_FAR_END)
	}
}

/// The most local lights a view shades with: every pixel loops over them, so this bounds its cost.
const MAX_VIEW_LIGHTS: usize = 1024;

/// The lights whose reach (Range around the light, past a SurfaceLight's face corners) meets the
/// view frustum, nearest first, at most MAX_VIEW_LIGHTS, each with its place among `lights`.
fn view_lights(lights: &[LightData], view_projection: Mat4, eye: Vec3) -> Vec<(usize, LightData)> {
	let rows = [view_projection.row(0), view_projection.row(1), view_projection.row(2), view_projection.row(3)];
	// the four sides and the near plane (reversed Z: depth at most 1)
	let planes = [rows[3] + rows[0], rows[3] - rows[0], rows[3] + rows[1], rows[3] - rows[1], rows[3] - rows[2]];
	let mut visible: Vec<(f32, usize, LightData)> = lights
		.iter()
		.enumerate()
		.filter_map(|(index, light)| {
			let center = Vec3::new(light.position[0], light.position[1], light.position[2]);
			// parts within Range, particles, beams and trails within the voxels' reach
			let reach = light.color[3].max(light.voxel_color[3]) + glam::Vec2::new(light.right[3], light.up[3]).length();
			let outside = planes.iter().any(|plane| plane.truncate().dot(center) + plane.w < -reach * plane.truncate().length());
			(!outside).then(|| ((center.distance(eye) - reach).max(0.0), index, *light))
		})
		.collect();
	visible.sort_by(|a, b| a.0.total_cmp(&b.0));
	visible.truncate(MAX_VIEW_LIGHTS);
	visible.into_iter().map(|(_, index, light)| (index, light)).collect()
}

/// Where a shadowed light's maps are drawn from: its origin (a SurfaceLight's apex behind its face),
/// how far they reach, and for a SurfaceLight of Angle up to 120 the one face aimed down its normal:
/// (direction, the frustum's half-angle tangent; 0 for six faces) and its near plane; `face`: how far
/// along that direction a SurfaceLight's face lies from the origin (0 for other lights), behind which
/// nothing casts its shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShadowLight {
	origin: Vec3,
	reach: f32,
	aim: Vec4,
	near: f32,
	face: f32,
}

impl ShadowLight {
	/// The plane behind which nothing casts its shadow (xyz its normal, w its offset: what is behind
	/// has dot(p, xyz) + w < 0), for a SurfaceLight drawn with six faces; its aimed face's near plane
	/// lies on the face instead.
	fn clip_plane(&self) -> Option<Vec4> {
		let direction = self.aim.truncate();
		(self.face > 0.0 && self.aim.w <= 0.0).then(|| {
			let centre = self.origin + direction * self.face;
			Vec4::from((direction, -direction.dot(centre)))
		})
	}
}

/// The view-projections of a light's six shadow faces (+X, -X, +Y, -Y, +Z, -Z, the order the mesh
/// shader picks them in by the major axis), or of its one aimed face (in each layer; the shader reads
/// the first).
fn light_shadow_matrices_at(light: &ShadowLight) -> [Mat4; 6] {
	if light.aim.w > 0.0 {
		let direction = light.aim.truncate();
		let up = if direction.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
		let projection = Mat4::perspective_rh(2.0 * light.aim.w.atan(), 1.0, light.near, light.reach.max(light.near + 0.1));
		return [projection * Mat4::look_at_rh(light.origin, light.origin + direction, up); 6];
	}
	let projection = Mat4::perspective_rh(LIGHT_SHADOW_FOV.to_radians(), 1.0, 0.05, light.reach.max(0.1));
	let faces = [(Vec3::X, Vec3::Y), (Vec3::NEG_X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::NEG_Y, Vec3::Z), (Vec3::Z, Vec3::Y), (Vec3::NEG_Z, Vec3::Y)];
	let position = light.origin;
	faces.map(|(direction, up)| projection * Mat4::look_at_rh(position, position + direction, up))
}

/// A local light for the shader: its position (a SurfaceLight's face centre), the direction it faces
/// (the parent's `Face` normal), the cone's cos(Angle / 2), its radiance and Range, a SurfaceLight's
/// face axes with their half extents, and whether it has Shadows (each view gives shadow map layers
/// to the nearest few).
fn light_data(light: &crate::scene::Light) -> LightData {
	let normals = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	let face = normals[light.face.min(5) as usize];
	let world = |axis: Vec3| Vec3::from(light.cframe.matrix3 * glam::Vec3A::from(axis)).normalize_or_zero();
	let direction = world(face);
	let kind = match light.kind {
		LightKind::Point => 0.0,
		LightKind::Spot => 1.0,
		LightKind::Surface => 2.0,
	};
	let mut position = Vec3::from(light.cframe.translation);
	let mut right = [0.0; 4];
	let mut up = [0.0; 4];
	let mut apex = 0.0;
	let mut aim = 0.0;
	if light.kind == LightKind::Surface {
		let size = light.part_size.abs();
		position += direction * (size.dot(face.abs()) * 0.5);
		let (a, b) = match light.face.min(5) % 3 {
			0 => (Vec3::Y, Vec3::Z),
			1 => (Vec3::X, Vec3::Z),
			_ => (Vec3::X, Vec3::Y),
		};
		let (a_world, b_world) = (world(a), world(b));
		right = [a_world.x, a_world.y, a_world.z, size.dot(a) * 0.5];
		up = [b_world.x, b_world.y, b_world.z, size.dot(b) * 0.5];
		// its shadow is cast from behind the face, where a frustum of its Angle just spans the face's
		// longer side; past 120 degrees as if at 90 (light-surface-rect, -apex: a slab's shadow edge
		// puts the point 3.46 / 2.00 / 1.70 / 1.39 behind a 4-stud face at Angle 60 / 90 / 100 / 110,
		// 1.14 at 120 (light-shadows surface), 2.00 at 130, 140, 150, 170 and 180, also on a carrier 3
		// studs thick; 4.1 behind 8 x 2, 2 x 8 and 8 x 8 faces at 90, 4.0 at 180)
		let angle = if light.angle > 120.0 { 90.0 } else { light.angle.max(1.0) };
		apex = right[3].max(up[3]) / (angle * 0.5).to_radians().tan();
		// up to 120 degrees what it lights lies in that frustum: one face aimed down it, a little wider
		// for the filter's taps, keeps Studio's crisp narrow shadows (light-surface-apex-small: Angle 0-60,
		// edges within a texel), where six 100-degree faces from an apex hundreds of studs back blurred
		// them away (a cutscene's Angle-0 rim light, 489 studs back: 1.1-stud texels)
		if light.angle <= 120.0 {
			aim = (angle * 0.5).to_radians().tan() * 1.1;
		}
	}
	// Color decodes with a 2.2 power (light-range: (0.2, 0.5, 0.8) reads 0.028, 0.215, 0.613); Range
	// is not clamped to 60 (light-range: Range 120 fades at 120)
	// a negative Brightness lights nothing, here as in the voxels particles read (not measured in
	// Studio; none of one game's 326 lights)
	let color = light.color.max(Vec3::ZERO).powf(2.2) * (light.brightness.max(0.0) * LIGHT_SCALE * std::f32::consts::PI);
	// what its voxels give decodes with a square (light-particle-voxels orange: green 0.105-0.115 of
	// red where Color² gives 0.111, Color^2.2 0.089)
	let voxel_color = light.color.max(Vec3::ZERO).powf(2.0) * (light.brightness.max(0.0) * LIGHT_SCALE);
	LightData {
		position: [position.x, position.y, position.z, kind],
		direction: [direction.x, direction.y, direction.z, (light.angle.clamp(0.0, 180.0).to_radians() * 0.5).cos()],
		color: [color.x, color.y, color.z, light.range],
		right,
		up,
		shadow: [-1.0, if light.shadows { 1.0 } else { 0.0 }, apex, 0.0],
		voxel_color: [voxel_color.x, voxel_color.y, voxel_color.z, voxel_reach(light.range.max(0.0))],
		extra: [if light.placed { 0.0 } else { 1.0 }, light.range, aim, 0.0],
	}
}

/// Where `light`'s shadow maps are drawn from ([`ShadowLight`]).
fn shadow_light(light: &LightData) -> ShadowLight {
	let direction = Vec3::new(light.direction[0], light.direction[1], light.direction[2]);
	let origin = Vec3::new(light.position[0], light.position[1], light.position[2]) - direction * light.shadow[2];
	// nothing behind the face casts (fixtures/local-lights.luau "carriers": an opaque carrier, or a plate
	// between the face and this point, shadows nothing in a Roblox client): the aimed face's near plane
	// lies on the face, six faces cut it in the shadow pass (ShadowLight::clip_plane)
	let near = light.shadow[2].max(0.05);
	ShadowLight { origin, reach: light.color[3] + light.shadow[2], aim: Vec4::from((direction, light.extra[2])), near, face: light.shadow[2] }
}

/// A shadowed light's grazing darkening (mesh.wgsl `light_graze`) goes by the longest Range of the
/// shadowed lights at its very position (light-graze-pairs: a shadowed PointLight of Range 25 darkens
/// a grazed floor to 0.91 22 studs out alone, beside a shadowed one of Range 25 or an unshadowed one of
/// Range 42 alike, to 0.97 beside a shadowed one of Range 40 on the same part, as the rule at Range 40
/// gives, and not at all beside one of Range 120, on one part or two Attachments together; 0.5 studs
/// apart, on parts or Attachments, each its own. An effect's clash puts its Range 25 light beside a Range
/// 120 one).
fn shared_graze_ranges(lights: &mut [LightData]) {
	let at = |light: &LightData| Vec3::new(light.position[0], light.position[1], light.position[2]);
	for index in 0..lights.len() {
		if lights[index].shadow[1] <= 0.0 {
			continue;
		}
		let here = at(&lights[index]);
		let range = lights.iter().filter(|other| other.shadow[1] > 0.0 && at(other).distance(here) < 0.01).map(|other| other.color[3]).fold(lights[index].color[3], f32::max);
		lights[index].extra[1] = range;
	}
}

/// The pipelines a [`Renderer`] draws with, compiled for its device. Renderers made on one [`Gpu`] share them: the
/// device keeps them only while a renderer holds them (a renderer's pipelines hold its device, which goes with the
/// last of them), and a renderer made beside a living one compiles none (on a software adapter, as on CI's runners,
/// [`Pipelines::new`] and the effects' compile on the CPU: 28 s on four of a desktop's cores, 30-50 s on a runner).
/// They are the compiled shaders and their layouts alone: what a renderer caches stays its own.
#[derive(Default)]
pub(crate) struct DevicePipelines {
	pipelines: std::sync::Weak<Pipelines>,
	post: std::sync::Weak<post::PostPipelines>,
	beams: std::sync::Weak<beams::BeamPipelines>,
	particles: std::sync::Weak<particles::ParticlePipelines>,
	guis: std::sync::Weak<guis::GuiPipelines>,
}

/// [`DevicePipelines`] held
type SharedPipelines = (Arc<Pipelines>, Arc<post::PostPipelines>, Arc<beams::BeamPipelines>, Arc<particles::ParticlePipelines>, Arc<guis::GuiPipelines>);

impl DevicePipelines {
	/// The pipelines of the renderers on `gpu`, compiled when none holds them
	fn take(gpu: &Gpu) -> SharedPipelines {
		let mut kept = gpu.pipelines.lock();
		if let (Some(pipelines), Some(post), Some(beams), Some(particles), Some(guis)) = (kept.pipelines.upgrade(), kept.post.upgrade(), kept.beams.upgrade(), kept.particles.upgrade(), kept.guis.upgrade()) {
			return (pipelines, post, beams, particles, guis);
		}
		let device = &gpu.device;
		let cache = gpu.pipeline_cache.as_ref();
		// the pipelines on threads of their own (Pipelines::new), the effects' after the frame layout
		let (pipelines, post) = std::thread::scope(|scope| {
			let post = scope.spawn(|| post::PostPipelines::new(device, pipelines::COMMON, pipelines::FULLSCREEN, cache));
			(Pipelines::new(device, cache), post.join().expect("the post pipelines' thread"))
		});
		let frame_layout = &pipelines.frame_layout;
		let (beams, particles, guis) = std::thread::scope(|scope| {
			let beams = scope.spawn(|| beams::BeamPipelines::new(device, frame_layout, cache));
			let particles = scope.spawn(|| particles::ParticlePipelines::new(device, frame_layout, cache));
			let guis = guis::GuiPipelines::new(device, frame_layout, cache);
			let made = "the effects' pipelines' threads";
			(beams.join().expect(made), particles.join().expect(made), guis)
		});
		let shared = (Arc::new(pipelines), Arc::new(post), Arc::new(beams), Arc::new(particles), Arc::new(guis));
		*kept = Self { pipelines: Arc::downgrade(&shared.0), post: Arc::downgrade(&shared.1), beams: Arc::downgrade(&shared.2), particles: Arc::downgrade(&shared.3), guis: Arc::downgrade(&shared.4) };
		shared
	}
}

/// One device for the tests that only draw, with its pipelines kept from test to test ([`DevicePipelines`]): their
/// renderers are fresh but for the compiled shaders, so they draw and report what renderers on devices of their own
/// do, and the software adapter compiles the pipelines once a process instead of once a test. Tests of the device's
/// memory, its counters or its end make their own.
#[cfg(test)]
pub(crate) fn test_gpu() -> Arc<Gpu> {
	static SHARED: std::sync::OnceLock<(Arc<Gpu>, SharedPipelines)> = std::sync::OnceLock::new();
	SHARED
		.get_or_init(|| {
			let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
			let pipelines = DevicePipelines::take(&gpu);
			(gpu, pipelines)
		})
		.0
		.clone()
}

impl Renderer {
	pub fn new(gpu: Arc<Gpu>, assets: ror_assets::Assets) -> Result<Self> {
		let device = &gpu.device;
		let (pipelines, post, beams, particles, guis) = DevicePipelines::take(&gpu);
		let white = GpuTexture::solid(&gpu, "white", [255, 255, 255, 255], TextureUse::Color);
		let normal = GpuTexture::solid(&gpu, "flat normal", [128, 128, 255, 255], TextureUse::Data);
		let black = GpuTexture::solid(&gpu, "black", [0, 0, 0, 255], TextureUse::Data);
		let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("material"),
			address_mode_u: wgpu::AddressMode::Repeat,
			address_mode_v: wgpu::AddressMode::Repeat,
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Linear,
			anisotropy_clamp: 16,
			..Default::default()
		});
		let clamped_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("material, clamped"),
			address_mode_u: wgpu::AddressMode::ClampToEdge,
			address_mode_v: wgpu::AddressMode::ClampToEdge,
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Linear,
			anisotropy_clamp: 16,
			..Default::default()
		});
		let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("linear"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});
		let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("shadow"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			compare: Some(wgpu::CompareFunction::LessEqual),
			..Default::default()
		});
		// Studio's own split-sum table when the install has it (notes/render.md "Sky"), else the same
		// integral computed
		let studio_lut = assets.fetch("rbxasset://textures/brdfLUT.dds").ok().and_then(|fetched| environment::studio_brdf_lut(&fetched.bytes));
		let (lut_size, lut) = studio_lut.unwrap_or_else(|| {
			let size = 128;
			(size, environment::brdf_lut(size).iter().flat_map(|[a, b]| [half(*a), half(*b)]).collect())
		});
		let brdf_texture = device.create_texture_with_data(
			&gpu.queue,
			&wgpu::TextureDescriptor {
				label: Some("brdf lut"),
				size: wgpu::Extent3d { width: lut_size, height: lut_size, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rg16Float,
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			},
			wgpu::util::TextureDataOrder::LayerMajor,
			bytemuck::cast_slice(&lut),
		);
		let empty_shadow = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("no shadow"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: SHADOW_FORMAT,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&Default::default());
		let empty_moments = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("no sun moments"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: SUN_MOMENTS_FORMAT,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&Default::default());
		let empty_light_shadows = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("no light shadows"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: SHADOW_FORMAT,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
		let no_bloom = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("no bloom"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: HDR_FORMAT,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&Default::default());
		let no_sky_images = device
			.create_texture(&wgpu::TextureDescriptor {
				label: Some("no sky images"),
				size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 6 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rgba8Unorm,
				usage: wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
			.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
		let wang_index = assets
			.fetch(WANG_INDEX)
			.ok()
			.and_then(|fetched| ror_image::decode(&fetched.bytes).ok())
			.and_then(|image| GpuTexture::upload(&gpu, WANG_INDEX, &ror_image::to_rgba8(&image), TextureUse::Data).ok());
		let defaults = Defaults {
			no_bloom,
			no_sky_images,
			white: white.into(),
			normal: normal.into(),
			black: black.into(),
			material_sampler,
			clamped_sampler,
			linear_sampler,
			shadow_sampler,
			brdf_lut: brdf_texture.create_view(&Default::default()),
			empty_shadow,
			empty_moments,
			empty_light_shadows,
			no_light_matrices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
				label: Some("no light shadow matrices"),
				contents: bytemuck::cast_slice(&Mat4::IDENTITY.to_cols_array_2d()),
				usage: wgpu::BufferUsages::STORAGE,
			}),
			no_light_clusters: device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("no light clusters"), contents: &[0; 16], usage: wgpu::BufferUsages::STORAGE }),
			no_light_grid: light_grid_texture(&gpu, |desc| gpu.device.create_texture(desc), [1, 1, 1], &[255], None),
			no_clouds: clouds::empty(&gpu),
			cloud_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
				label: Some("clouds"),
				address_mode_u: wgpu::AddressMode::Repeat,
				address_mode_v: wgpu::AddressMode::Repeat,
				address_mode_w: wgpu::AddressMode::Repeat,
				mag_filter: wgpu::FilterMode::Linear,
				min_filter: wgpu::FilterMode::Linear,
				mipmap_filter: wgpu::FilterMode::Linear,
				..Default::default()
			}),
			no_voxel_field: device
				.create_texture_with_data(
					&gpu.queue,
					&wgpu::TextureDescriptor {
						label: Some("no voxel field"),
						size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
						mip_level_count: 1,
						sample_count: 1,
						dimension: wgpu::TextureDimension::D3,
						format: voxel_field::FORMAT,
						usage: wgpu::TextureUsages::TEXTURE_BINDING,
						view_formats: &[],
					},
					wgpu::util::TextureDataOrder::LayerMajor,
					&[0; 8],
				)
				.create_view(&Default::default()),
			wang: wang_index.is_some(),
			wang_index: wang_index.unwrap_or_else(|| GpuTexture::solid(&gpu, "no wang index", [0, 0, 0, 0], TextureUse::Data)).into(),
		};
		let mut primitives = HashMap::new();
		for (kind, mesh) in [
			(Primitive::Block, geometry::block()),
			(Primitive::Wedge, geometry::wedge()),
			(Primitive::CornerWedge, geometry::corner_wedge()),
			(Primitive::Sphere, geometry::sphere(6)),
			(Primitive::Cylinder, geometry::cylinder(6)),
		] {
			primitives.insert(kind, Arc::new(GpuMesh::upload(&gpu, &format!("{kind:?}"), &mesh)));
		}
		// the driver's compiles of every pipeline above, for the next process (render::gpu)
		gpu.save_pipeline_cache();
		Ok(Self {
			gpu,
			assets,
			inner: Arc::new(RendererInner {
				pipelines,
				defaults,
				primitives,
				// what stays between renders on a machine shared with Studios and games (a render's own may
				// go past it while it draws): a large place seen whole takes 1.6 GB of textures and 44 MB of
				// meshes, a map 200-300 MB and 4 MB, a character 40-80 MB and 1 MB, an attack's effects
				// 234 MB; the disk cache gives the rest back in a few seconds
				meshes: Cache::new(cache_budget("MESH", 256)),
				textures: Cache::new(cache_budget("TEXTURE", 384)),
				packs: Cache::new(64 << 20),
				// a sky's maps take ~4 MB over its images (in `skies`, once per sky); a clip whose Lighting
				// changes makes them every frame, so few are kept
				environments: Cache::new(cache_budget("ENVIRONMENT", 64)),
				skies: Cache::new(cache_budget("SKY", 64)),
				materials: Mutex::new(HashMap::new()),
				skins: skinning::Skins::default(),
				beams,
				ids: std::sync::OnceLock::new(),
				particles,
				post,
				guis,
				gui_textures: Cache::new(128 << 20),
				gui_failures: guis::GuiFailures::default(),
				gui_uploads: Default::default(),
				effect_bindings: Default::default(),
				reuse: reuse::Reuse::default(),
				preparations: prepared::Preparations::new(cache_budget("PREPARED", 256)),
				// a view's sun atlas is 84 MB (4096 x 1280 moments of four floats): under 128 MB a still
				// camera's frames draw the sun's shadows once (at 64 MB none was ever kept)
				shadows: shadow_reuse::Shadows::new(cache_budget("SHADOW", 128)),
				nv12: std::sync::OnceLock::new(),
				layers: std::sync::OnceLock::new(),
				voxel_field: std::sync::OnceLock::new(),
				clouds: std::sync::OnceLock::new(),
				clouds_flat: std::sync::OnceLock::new(),
				staging: texture_files::Staging::new(),
				recent_keys: Mutex::default(),
				residency_from: std::sync::atomic::AtomicU64::new(residency::FEEDBACK_FROM),
			}),
		})
	}

	/// Builds (once) the per-part id and Highlight mask pipelines on first use so renders with no
	/// Highlights, `--ids`, or `--layers` never compile or cache them.
	fn id_pipelines(&self) -> &ids::IdPipelines {
		self.ids.get_or_init(|| {
			let pipelines = ids::IdPipelines::new(&self.gpu.device, &self.pipelines.frame_layout, self.gpu.pipeline_cache.as_ref());
			self.gpu.save_pipeline_cache();
			pipelines
		})
	}

	/// A view of this renderer sharing the GPU device, pipelines and every GPU cache, using `assets`
	/// for asset lookups (e.g. an offline handle on one shared render service).
	pub fn with_assets(&self, assets: ror_assets::Assets) -> Self {
		Self { gpu: self.gpu.clone(), assets, inner: self.inner.clone() }
	}

	/// A texture a view handed out ([`ViewRequest::texture`]) that its holder is done with: a later
	/// view draws into it instead of a new one.
	pub fn give_back(&self, texture: wgpu::Texture) {
		self.reuse.give_back(texture);
	}

	/// Lets go of the GPU memory kept from renders that has gone unused a while (each render does so
	/// itself): for a caller gone idle, a window between previews.
	pub fn trim(&self) {
		self.reuse.trim();
		self.preparations.trim();
		self.shadows.trim(&self.reuse);
		self.gui_uploads.trim(false);
		// what the preparations let go of past the caches' budgets
		self.textures.fit();
		self.meshes.fit();
	}

	/// Lets go of what only the renders just before drew with, for a service gone quiet: the targets
	/// and buffers kept for the next render, preparations and shadow maps (each kept 30 s unused
	/// anyway), the GUI images (each a frame's: an animated GUI is a new image every frame, and the
	/// disk cache gives back the rest as fast), the posed skins (a pose per frame of an animation) and
	/// the downloaded files' bytes the asset cache keeps in memory.
	/// Textures, meshes and sky maps stay: the next render of the same models finds them. What the
	/// GPU still owes from dropped resources is collected too.
	pub fn release_idle(&self) -> Released {
		let kept = self.reuse.clear() + self.preparations.clear() + prepared::release_part_keys() + self.shadows.clear() + self.staging.release() + self.gui_uploads.trim(true);
		let guis = self.gui_textures.shrink_to(0);
		self.effect_bindings.lock().clear();
		let skins = self.skins.release_posed();
		let assets = self.assets.release_memory() as u64;
		self.materials.lock().retain(|_, material| material.strong_count() > 0);
		// the textures and meshes the dropped draws held past the caches' budgets
		self.textures.fit();
		self.meshes.fit();
		let _ = self.gpu.poll(wgpu::PollType::Poll);
		Released { kept, guis, skins, assets }
	}

	/// Lets go of the textures, meshes, texture packs and sky maps too, for a service quiet a while
	/// longer: idle after a map's views and clip, 786-791 -> 348-356 MB of commit. The next job
	/// loads them again from the disk cache (those views 0.47-0.54 s -> 0.58 s). Returns their bytes.
	pub fn release_assets(&self) -> u64 {
		let freed = self.textures.shrink_to(0) + self.meshes.shrink_to(0) + self.packs.shrink_to(0) + self.environments.shrink_to(0) + self.skies.shrink_to(0);
		self.materials.lock().retain(|_, material| material.strong_count() > 0);
		let _ = self.gpu.poll(wgpu::PollType::Poll);
		freed
	}

	/// Refuses a mesh or texture of `bytes` that would take this process past 90 % of the GPU memory
	/// the system gives it ([`gpu::Gpu::vram`]): past the budget, Windows backs GPU memory with the
	/// system's (a render of 932 models at once took 47.6 GB of it, the machine's RAM down to 1 GB,
	/// and the driver then failed allocations anyway). The job fails with [`GPU_FULL`].
	fn gpu_room(&self, bytes: u64) -> Result<()> {
		if let Some(vram) = self.gpu.vram()
			&& vram.budget > 0
			&& vram.usage + bytes > vram.budget / 100 * gpu_room_percent()
		{
			return Err(anyhow!("{GPU_FULL} ({} of {} MB in use)", vram.usage >> 20, vram.budget >> 20));
		}
		Ok(())
	}

	/// [`crate::memory_checkpoint`] with the GPU memory this process uses beside it.
	fn memory_checkpoint(&self, label: &str) {
		if std::env::var_os("ROR_MEMORY_REPORT").is_some() {
			let gpu = self.gpu.vram().map(|vram| format!(", GPU {} MB", vram.usage >> 20)).unwrap_or_default();
			crate::memory_checkpoint(&format!("{label}{gpu}"));
		}
	}

	/// What the renderer holds after a job, by kind, as each cache counts its bytes (`ROR_MEMORY_REPORT`).
	pub fn memory_breakdown(&self) -> String {
		let mb = |bytes: u64| bytes >> 20;
		let (_, kept_textures, _, kept_buffers) = self.reuse.stats();
		let (_, preparations) = self.preparations.stats();
		let (_, shadows) = self.shadows.stats();
		let vram = self.gpu.vram().map(|vram| format!("; GPU {} MB in use", vram.usage >> 20)).unwrap_or_default();
		let kinds: Vec<String> = self.reuse.kinds().into_iter().take(14).map(|(name, count, bytes)| format!("{count} x {name} = {} MB", bytes >> 20)).collect();
		format!(
			"textures {} MB, meshes {} MB, packs {} MB, sky maps {} + {} MB, GUI images {} MB, kept targets {} MB and buffers {} MB, preparations {} MB, shadow maps {} MB, downloads in memory {} MB{vram}\n  kept targets by kind: {}",
			mb(self.textures.bytes()),
			mb(self.meshes.bytes()),
			mb(self.packs.bytes()),
			mb(self.environments.bytes()),
			mb(self.skies.bytes()),
			mb(self.gui_textures.bytes()),
			mb(kept_textures),
			mb(kept_buffers),
			mb(preparations),
			mb(shadows),
			mb(self.assets.memory_cache_bytes() as u64),
			kinds.join("; "),
		)
	}

	/// Keeps this process within both the adapter's measured free GPU memory ([`gpu::Gpu::free_memory`],
	/// every program counted) and this process's budget ([`gpu::Gpu::vram`]): in every mode, when
	/// adapter-wide free memory drops below [`crate::budget::GPU_HEADROOM_PERCENT`] (10 %) or process
	/// usage passes 85 % of its budget (down to 70 %), what renders keep for later goes — unheld targets
	/// and buffers, preparations, shadow maps, posed skins, then least recently used GUI images,
	/// textures, meshes and sky maps. Only idle caches are released: no draw waits and no picture
	/// changes. Returns this process's budget reading before relief, if available.
	pub fn fit_vram(&self) -> Option<gpu::Vram> {
		self.fit_vram_with(self.gpu.vram(), self.gpu.free_memory())
	}

	pub(crate) fn fit_vram_with(&self, vram: Option<gpu::Vram>, free: Option<crate::budget::GpuFree>) -> Option<gpu::Vram> {
		let adapter_short = free.map_or(0, |free| {
			let headroom = free.total / 100 * crate::budget::GPU_HEADROOM_PERCENT;
			headroom.saturating_sub(free.free)
		});
		let budget_short = vram.map_or(0, |vram| if vram.usage > vram.budget / 100 * 85 { vram.usage.saturating_sub(vram.budget / 100 * 70) } else { 0 });
		let goal = adapter_short.max(budget_short);
		if goal > 0 {
			let freed = self.relieve(goal);
			if adapter_short >= budget_short
				&& let Some(free) = free
			{
				let headroom = free.total / 100 * crate::budget::GPU_HEADROOM_PERCENT;
				eprintln!("ror: {} MB of the GPU free, under the {} MB headroom: let go of {} MB kept for later renders", free.free >> 20, headroom >> 20, freed >> 20);
			} else if let Some(vram) = vram {
				eprintln!("ror: GPU memory {} MB in use of a {} MB budget: let go of {} MB kept for later renders", vram.usage >> 20, vram.budget >> 20, freed >> 20);
			}
		}
		vram
	}

	/// Lets go of what renders keep for later ones until `goal` bytes are freed, while renders draw (none
	/// of it is in use): the targets and buffers kept between renders, preparations, shadow maps and posed
	/// skins, then the least recently used GUI images, textures, meshes and sky maps no render holds. For
	/// the GPU's budget ([`Renderer::fit_vram`]) and the machine's memory (a job queued for memory,
	/// [`crate::budget::admit`], or a service short of it): each byte kept on the GPU commits one of system
	/// memory too. Returns the bytes freed. The scratch targets a draw holds stay (`Reuse::release_unheld`):
	/// taken from the pool, the next draw of their size made a second set beside them, and beside a game
	/// the relief each second did so to every clip's frames.
	pub fn relieve(&self, goal: u64) -> u64 {
		let mut freed = self.reuse.release_unheld() + self.preparations.clear() + self.shadows.clear() + self.skins.release_posed();
		let caches: [&dyn Fn(u64) -> u64; 5] = [
			&|cut| self.gui_textures.shrink_to(self.gui_textures.idle_bytes().saturating_sub(cut)),
			&|cut| self.textures.shrink_to(self.textures.idle_bytes().saturating_sub(cut)),
			&|cut| self.meshes.shrink_to(self.meshes.idle_bytes().saturating_sub(cut)),
			&|cut| self.environments.shrink_to(self.environments.idle_bytes().saturating_sub(cut)),
			&|cut| self.skies.shrink_to(self.skies.idle_bytes().saturating_sub(cut)),
		];
		for shrink in caches {
			if freed >= goal {
				break;
			}
			freed += shrink(goal - freed);
		}
		self.materials.lock().retain(|_, material| material.strong_count() > 0);
		// the dropped resources' memory goes back once the GPU is done with them
		let _ = self.gpu.poll(wgpu::PollType::Poll);
		freed
	}

	/// For a draw short of GPU memory beside others ([`crate::budget::GpuGate`]): the targets and buffers
	/// kept from earlier draws that none draws into now, made again in a moment when needed. Textures and
	/// meshes stay (read again from disk, a map's took seconds), as do the scratch targets draws use now
	/// (taken from the pool, the next draw of their size would make a second set). Returns their bytes.
	pub fn release_unheld_targets(&self) -> u64 {
		let freed = self.reuse.release_unheld();
		let _ = self.gpu.poll(wgpu::PollType::Poll);
		freed
	}

	/// The targets `views` draw into ([`crate::budget::GpuGate`]): a set per size drawn (views of one size
	/// draw one after another into the same targets, each view's given back once it is submitted), whether
	/// one is kept, and its GPU memory. Measured after one view on a fresh service (2026-11-02, `/health`'s
	/// kinds, 4x MSAA): a unit at 1024², 1920 x 1080, 3840 x 2160 and 3840 x 2160 supersampled twice 189,
	/// 290, 828 and 2442 MB, a large place at 1080p 354 MB (a larger sun atlas and a light's cube). That is
	/// per pixel drawn the colour and depth at its samples and two colour and a depth target at one, per
	/// pixel shown 24 bytes (the image, its bloom), and shadow maps and a quarter-size pass, under 192 MB
	/// (that place's: a 4096 x 1280 sun atlas, a 1280² light's map, a 512² cube of 48 layers): this gives
	/// 284, 374, 920 and 2534 MB for them, over each.
	pub fn target_sets(&self, views: &[ViewRequest]) -> Vec<crate::budget::TargetSet> {
		let (hdr, depth) = (pipelines::HDR_FORMAT.block_copy_size(None).unwrap_or(8) as u64, 4u64);
		let mut sets: Vec<crate::budget::TargetSet> = Vec::new();
		for view in views {
			let supersample = view.supersample.clamp(1, 4);
			let samples = view_samples(view);
			let (width, height) = (view.width.max(1), view.height.max(1));
			let key = (width * supersample, height * supersample, samples);
			if sets.iter().any(|set| set.key == key) {
				continue;
			}
			let (drawn, shown) = (key.0 as u64 * key.1 as u64, width as u64 * height as u64);
			let bytes = (192u64 << 20) + drawn * ((hdr + depth) * samples as u64 + 2 * hdr + depth) + shown * 24;
			let kept = self.reuse.holds_scratch(key.0, key.1, pipelines::HDR_FORMAT, samples);
			sets.push(crate::budget::TargetSet { key, bytes, kept });
		}
		sets
	}

	/// Makes the sky's maps for `scene`'s Lighting before its render needs them, for
	/// [`Renderer::render_with`]: a clip's frames whose Lighting changes have theirs made on every core
	/// ahead of the draws (held here, whatever the cache lets go of meanwhile).
	pub fn prepare_environment(&self, scene: &Scene) -> PreparedSky {
		let env_start = Instant::now();
		let mut missing = Vec::new();
		let mut untrusted_hashes = 0usize;
		let maps = self.environment(scene, &mut missing, &mut untrusted_hashes);
		// the key its preparation is found by, made here too, off the thread that draws (a map clip's
		// 3,710 parts encode 1.5 MB, 0.6-0.8 ms of each frame): unless the render draws a copy of the
		// scene instead (submit_views)
		let key = (scene.environment.viewport.is_none() || !scene.has_more_than_parts()).then(|| scene_key(scene)).flatten().map(|key| self.shared_key(key));
		let env_ms = env_start.elapsed().as_secs_f64() * 1000.0;
		let particle_start = Instant::now();
		let particles = particles::simulate_all(scene);
		let particle_sim_ms = particle_start.elapsed().as_secs_f64() * 1000.0;
		PreparedSky { maps, missing, key, particles: Some(particles), untrusted_hashes, env_ms, particle_sim_ms }
	}

	/// [`Renderer::prepare_environment`] for a clip's frames whose parts are all the same (a playback that
	/// changes nothing): their preparation key made by the first, shared by the rest (a map's
	/// 3,710 parts encoded 1.5 MB a frame, 12 % of a busy service's samples under eight clips).
	pub fn prepare_environment_sharing(&self, scene: &Scene, parts: &PartsKey) -> PreparedSky {
		let env_start = Instant::now();
		let mut missing = Vec::new();
		let mut untrusted_hashes = 0usize;
		let maps = self.environment(scene, &mut missing, &mut untrusted_hashes);
		let key = if scene.environment.viewport.is_none() || !scene.has_more_than_parts() { parts.0.get_or_init(|| scene_key(scene).map(|key| self.shared_key(key))).clone() } else { None };
		let env_ms = env_start.elapsed().as_secs_f64() * 1000.0;
		let particle_start = Instant::now();
		let particles = particles::simulate_all(scene);
		let particle_sim_ms = particle_start.elapsed().as_secs_f64() * 1000.0;
		PreparedSky { maps, missing, key, particles: Some(particles), untrusted_hashes, env_ms, particle_sim_ms }
	}

	/// `key`, or an equal one an earlier frame made still holds: a clip's frames of an unchanging
	/// scene share one (each made ahead holding its own, a map's 1.5 MB, added 100 MB to its
	/// clip's peak).
	fn shared_key(&self, key: Arc<prepared::SceneKey>) -> Arc<prepared::SceneKey> {
		let mut recent = self.recent_keys.lock();
		recent.retain(|known| known.strong_count() > 0);
		if let Some(known) = recent.iter().filter_map(std::sync::Weak::upgrade).find(|known| known.same(&key)) {
			return known;
		}
		recent.push(Arc::downgrade(&key));
		if recent.len() > 8 {
			recent.remove(0);
		}
		key
	}

	/// [`Renderer::render`] with the sky's maps made ahead for the scene's Lighting.
	pub fn render_with(&self, scene: &Scene, views: &[ViewRequest], sky: PreparedSky) -> Result<(Vec<ViewImage>, RenderStats)> {
		self.render_views(scene, views, Some(sky))
	}

	/// Submits views without waiting for their readbacks, for a bounded video pipeline.
	pub fn submit_with(&self, scene: &Scene, views: &[ViewRequest], sky: PreparedSky) -> Result<(Vec<PendingView<'_>>, RenderStats)> {
		self.submit_views(scene, views, Some(sky), None, None, None, None)
	}

	/// A video frame: [`Renderer::submit_with`] (or, with `readback`, MP4's compact NV12, retaining
	/// lossless RGBA only for contact-sheet moments or overlays), drawing its GUIs
	/// with `guis` (resolved and uploaded before it is admitted: [`Renderer::resolve_frame_guis`]); a GUI
	/// they miss is found as any render finds it. Admitted within the GPU's free memory by `gate`
	/// ([`crate::budget::GpuGate`]): the frames of several clips record side by side into the same
	/// targets (reuse::Lease::scratch), the first of a size reserving them for the rest.
	///
	/// `kept` is what the clip keeps between frames: its occlusion and its residency's (`keep`: the next
	/// frame's camera is this one's).
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn submit_frame(
		&self,
		scene: &Scene,
		views: &[ViewRequest],
		sky: PreparedSky,
		readback: Option<VideoReadback>,
		guis: (guis::GuiTextures, HashMap<guis::GuiKey, String>),
		gate: Option<&crate::budget::GpuGate>,
		kept: Option<(&ClipKept, bool)>,
	) -> Result<(Vec<PendingView<'_>>, RenderStats)> {
		self.submit_views(scene, views, Some(sky), readback, Some(guis), gate, kept)
	}

	/// Sizes and hit counts of the resource caches.
	pub fn cache_stats(&self) -> serde_json::Value {
		use std::sync::atomic::Ordering;
		let (reused_textures, reused_texture_bytes, reused_buffers, reused_buffer_bytes) = self.reuse.stats();
		let (prepared_entries, prepared_bytes) = self.preparations.stats();
		let (prepared_parts, prepared_part_bytes, prepared_grids, prepared_grid_bytes) = self.preparations.part_stats();
		let (shadow_entries, shadow_bytes) = self.shadows.stats();
		let counters = self.gpu.device.get_internal_counters();
		let hal = &counters.hal;
		let wgpu_counters = serde_json::json!({"buffers": hal.buffers.read(), "textures": hal.textures.read(), "views": hal.texture_views.read(), "bindGroups": hal.bind_groups.read(), "encoders": hal.command_encoders.read(), "bufferMemory": hal.buffer_memory.read(), "textureMemory": hal.texture_memory.read(), "allocations": hal.memory_allocations.read()});
		serde_json::json!({
			"meshes": {"entries": self.meshes.len(), "bytes": self.meshes.bytes(), "hits": self.meshes.hits.load(Ordering::Relaxed), "misses": self.meshes.misses.load(Ordering::Relaxed)},
			"textures": {"entries": self.textures.len(), "bytes": self.textures.bytes(), "hits": self.textures.hits.load(Ordering::Relaxed), "misses": self.textures.misses.load(Ordering::Relaxed)},
			"environments": {"entries": self.environments.len(), "bytes": self.environments.bytes()},
			"skies": {"entries": self.skies.len(), "bytes": self.skies.bytes()},
			"guis": {"entries": self.gui_textures.len(), "bytes": self.gui_textures.bytes(), "hits": self.gui_textures.hits.load(Ordering::Relaxed), "misses": self.gui_textures.misses.load(Ordering::Relaxed)},
			"materials": self.materials.lock().len(),
			"skins": self.skins.stats(),
			"prepared": {"entries": prepared_entries, "bytes": prepared_bytes, "parts": prepared_parts, "partBytes": prepared_part_bytes, "grids": prepared_grids, "gridBytes": prepared_grid_bytes},
			"shadows": {"entries": shadow_entries, "bytes": shadow_bytes},
			"reused": {"textures": reused_textures, "texture_bytes": reused_texture_bytes, "buffers": reused_buffers, "buffer_bytes": reused_buffer_bytes, "kinds": self.reuse.kinds().into_iter().take(16).map(|(kind, count, bytes)| serde_json::json!([kind, count, bytes >> 20])).collect::<Vec<_>>()},
			"vram": self.gpu.vram(),
			"assets": {"memoryBytes": self.assets.memory_cache_bytes()},
			"wgpu": wgpu_counters,
		})
	}

	/// Renders every view of a scene.
	pub fn render(&self, scene: &Scene, views: &[ViewRequest]) -> Result<(Vec<ViewImage>, RenderStats)> {
		self.render_gated(scene, views, None)
	}

	/// [`Renderer::render`], admitted within the GPU's free memory by `gate` from its start until its views
	/// are submitted ([`crate::budget::GpuGate`]); their readbacks wait outside.
	pub fn render_gated(&self, scene: &Scene, views: &[ViewRequest], gate: Option<&crate::budget::GpuGate>) -> Result<(Vec<ViewImage>, RenderStats)> {
		let (pending, mut stats) = self.submit_views(scene, views, None, None, None, gate, None)?;
		let images = pending.into_iter().map(|view| view.finish(&mut stats)).collect::<Result<Vec<_>>>()?;
		Ok((images, stats))
	}

	fn render_views(&self, scene: &Scene, views: &[ViewRequest], sky: Option<PreparedSky>) -> Result<(Vec<ViewImage>, RenderStats)> {
		let (pending, mut stats) = self.submit_views(scene, views, sky, None, None, None, None)?;
		let images = pending.into_iter().map(|view| view.finish(&mut stats)).collect::<Result<Vec<_>>>()?;
		Ok((images, stats))
	}

	#[allow(clippy::too_many_arguments)]
	fn submit_views(
		&self,
		scene: &Scene,
		views: &[ViewRequest],
		sky: Option<PreparedSky>,
		video: Option<VideoReadback>,
		guis: Option<(guis::GuiTextures, HashMap<guis::GuiKey, String>)>,
		gate: Option<&crate::budget::GpuGate>,
		kept: Option<(&ClipKept, bool)>,
	) -> Result<(Vec<PendingView<'_>>, RenderStats)> {
		// within the GPU's free memory, until its views are submitted (what they took shows in its use then)
		let relieve = |_| self.release_unheld_targets();
		let admission = gate.map(|gate| gate.admit(&self.target_sets(views), &|| self.gpu.free_memory(), &relieve)).transpose()?;
		let ao = kept.map(|(kept, keep)| ao::HistoryUse { history: &kept.ao, keep });
		let residency = kept.map(|(kept, keep)| (&kept.residency, keep));
		// a ViewportFrame draws its parts alone: no local lights, Highlights, GUIs, particles, beams or
		// trails (notes/render.md "ViewportFrames"; Scene::in_viewport_frame leaves them out once, a
		// scene given its lighting otherwise is copied without them here)
		let parts_only;
		let scene = if scene.environment.viewport.is_some() && scene.has_more_than_parts() {
			let mut copy = scene.clone();
			copy.keep_parts_only();
			parts_only = copy;
			&parts_only
		} else {
			scene
		};
		let mut stats = RenderStats { parts: scene.parts.len(), gpu_wait_ms: admission.as_ref().map_or(0.0, |admission| admission.waited.as_secs_f64() * 1000.0), ..Default::default() };
		let start = Instant::now();
		// what the render draws with, given back when it returns (after its submissions)
		let lease = self.reuse.lease(&self.gpu);
		let skins_before = self.skins.totals();
		// a clip frame under a view its residency has seen: the views decide its maps' levels
		// (residency::ClipResidency), no estimate here
		let defer_maps = residency.is_some_and(|(clip, _)| clip.has_seen());
		let mut prepared = self.prepare(scene, &mut stats, &lease, sky, views, defer_maps, kept.map(|(kept, _)| &kept.geometry))?;
		stats.prepare_ms = start.elapsed().as_secs_f64() * 1000.0;
		let skins = self.skins.totals();
		stats.skinned = (skins[0] - skins_before[0]) as usize;
		stats.skin_ms = (skins[1] - skins_before[1]) as f64 / 1e6;
		stats.skin_upload_ms = (skins[2] - skins_before[2]) as f64 / 1e6;
		stats.skin_upload_bytes = skins[3] - skins_before[3];
		stats.draws = prepared.draws.len();
		stats.triangles = prepared.draws.iter().map(|draw| (draw.indices.len() / 3) as u64).sum();
		// every view's GUIs first: one batch of textures for all of them
		let gui_start = Instant::now();
		for skip in &scene.skipped_guis {
			stats.gui_report.record_skipped(skip.path.clone(), skip.reason.clone());
		}
		let mut gui_plans: Vec<Vec<guis::GuiDraw>> = Vec::with_capacity(views.len());
		for view in views {
			let (draws, skips) = guis::plan_with_skips(scene, &view.camera.place(prepared.bounds, view.width.max(1), view.height.max(1)), view);
			for skip in skips {
				stats.gui_report.record_skipped(skip.path, skip.reason);
			}
			gui_plans.push(draws);
		}
		let gui_draws: Vec<&guis::GuiDraw> = gui_plans.iter().flatten().collect();
		if !gui_draws.is_empty() {
			let mut untrusted_gui = std::collections::HashSet::new();
			for draw in &gui_draws {
				draw.untrusted_paths(&mut untrusted_gui);
			}
			stats.untrusted_stamp_hashes += untrusted_gui.len();
		}
		let (mut ready, mut gui_failures) = guis.unwrap_or_default();
		let gui_textures = if gui_draws.is_empty() {
			HashMap::new()
		} else {
			// What was resolved ahead (a video frame's), plus any missing keys retried at draw time
			// (restoring recovery for GUIs that failed transiently during prefetch while never
			// retrying permanent environment causes like missing/invalid `rur` and avoiding duplicate
			// warnings; persistent `rur` call failures stay cached in `self.gui_failures`).
			let rest: Vec<&guis::GuiDraw> = gui_draws
				.iter()
				.copied()
				.filter(|draw| {
					draw.keys().any(|key| {
						!ready.contains_key(&key)
							&& !gui_failures.get(&key).is_some_and(|reason| guis::is_unrecoverable_gui_skip(reason))
					})
				})
				.collect();
			if !rest.is_empty() {
				let mut retry_warnings = Vec::new();
				let mut retry_failures = HashMap::new();
				let recovered = self.gui_textures_reported(scene, &rest, &mut retry_warnings, &mut retry_failures);
				for key in recovered.keys() {
					gui_failures.remove(key);
				}
				ready.extend(recovered);
				gui_failures.extend(retry_failures);
				for warning in retry_warnings {
					if !stats.warnings.contains(&warning) {
						stats.warnings.push(warning);
					}
				}
			}
			ready
		};
		if !gui_draws.is_empty() {
			stats.gui_ms = gui_start.elapsed().as_secs_f64() * 1000.0;
		}
		// the material maps' levels the views sample, read before any of them draws (render::residency):
		// one wait for the GPU for all of them, not one per view
		let groups = {
			let plans: Vec<residency::ViewPlan> = views.iter().map(|view| self.residency_plan(scene, &prepared, view)).collect();
			let mut report = residency::Report::default();
			// views under levels of detail draw other triangles than the feedback would: their maps whole
			let whole = views.iter().any(|view| view.lod.is_some() || view.quality.is_some());
			let groups = self.view_groups(&prepared, &plans, &mut report, residency, whole)?;
			(stats.residency_total_ms, stats.residency_ms, stats.residency_reads, stats.residency_read_bytes) = (report.ms, report.feedback_ms, report.read, report.read_bytes);
			groups
		};
		let mut images = Vec::with_capacity(views.len());
		for (view, gui_plan) in views.iter().zip(&gui_plans) {
			// a view's targets go back once it is submitted, and the next view draws into them (the GPU
			// runs the submissions in order): each with a set of its own, a unit's three views at 1024²
			// held three sets, 410 MB, and a group of eight a map's eight
			self.memory_checkpoint("view");
			let view_lease = self.reuse.lease(&self.gpu);
			// the worlds their ViewportFrames show: a window's drawn by this view's camera, a framed one's by
			// its own
			let worlds = self.render_worlds(scene, gui_plan, view, prepared.bounds, &mut stats.warnings);
			let view_guis = self.gui_geometry(scene, gui_plan, &gui_textures, &worlds, &view_lease);
			for draw in gui_plan {
				let Some(gui) = scene.guis.get(draw.gui) else {
					continue;
				};
				if gui_textures.contains_key(&draw.key) {
					stats.gui_report.record_drawn(gui.path.clone());
				} else {
					let reason = gui_failures.get(&draw.key).cloned().unwrap_or_else(|| "texture unavailable".into());
					stats.gui_report.record_skipped(gui.path.clone(), reason);
				}
			}
			stats.guis += view_guis.draws.len();
			// Roblox's levels of detail from this view's camera: its own copy of the draws while it draws. A
			// graphics quality level without `lod` culls by the client's view distance and size cull at it
			let client = view.quality.filter(|_| view.lod.is_none()).map(|_| view_quality(view, &scene.environment));
			let lod = view.lod.or(client.map(|quality| crate::lod::Lod { quality: quality.level, cull: true })).map(|lod| {
				let camera = view.camera.place(prepared.bounds, view.width.max(1), view.height.max(1));
				let (geometry, report) = levels::for_view(scene, &prepared.geometry, &camera, lod, client.as_ref());
				if let Some(geometry) = geometry {
					prepared.casters = Some(std::mem::replace(&mut prepared.geometry, Arc::new(geometry)));
				}
				report
			});
			let pending = self.render_view(scene, &prepared, view, &view_guis, &groups, &mut stats, (&view_lease, video, ao));
			if let Some(shared) = prepared.casters.take() {
				prepared.geometry = shared;
			}
			let pending = match lod {
				Some(report) => pending.map(|pending| pending.with_lod(report)),
				None => pending,
			};
			images.push(pending?);
			drop(view_guis);
			for (_, worlds) in worlds {
				if let Some(window) = worlds.window {
					self.give_back(window.texture);
					if let Some(share) = window.share {
						self.give_back(share);
					}
				}
				for picture in worlds.pictures {
					self.give_back(picture.texture);
				}
			}
			drop(view_lease);
		}
		drop(admission);
		if std::env::var_os("ROR_PROFILE").is_some() {
			eprintln!("profile: frame prepare {:.3} ms (reused={}) grid {:.3} ms sun {:.3} ms local {:.3} ms encode {:.3} ms readback {:.3} ms", stats.prepare_ms, stats.geometry_reused, stats.light_grid_ms, stats.sun_shadow_ms, stats.light_shadow_ms, stats.draw_ms, stats.readback_ms);
			eprintln!("profile: residency {:.3} ms (feedback {:.3} ms), {} maps read, {} MB", stats.residency_total_ms, stats.residency_ms, stats.residency_reads, stats.residency_read_bytes >> 20);
		}
		// each failure once, however many views met it: every view loads the sun's and the moon's textures
		// and the clouds for itself, and a render of two views reported a sun that could not load twice
		let mut reported = std::collections::HashSet::new();
		stats.missing.retain(|reason| reported.insert(reason.clone()));
		// as prepare_geometry's, for what loads after the parts (effects' textures, GUIs)
		if let Some(full) = stats.missing.iter().find(|reason| reason.contains(GPU_FULL)) {
			return Err(anyhow!("{} parts: {full}", scene.parts.len()));
		}
		Ok((images, stats))
	}

	/// A mesh by its content URI (a local file's by its contents' stamp too, [`content_key`]);
	/// `<uri>#lod=N` draws the file's level N (its lowest when it has
	/// fewer), placed as the full mesh is (MeshPart scale comes from InitialSize): what a
	/// Performance-fidelity copy shows at a distance (notes/cost.md "Levels of detail").
	fn load_mesh(&self, uri: &str) -> Result<Arc<GpuMesh>> {
		let (source, level) = match uri.rsplit_once("#lod=") {
			Some((source, level)) => (source, level.parse::<usize>().with_context(|| format!("{uri}: #lod= takes a level number"))?),
			None => (uri, 0),
		};
		let local_snap = local_snapshot_for(source);
		let stamped = match &local_snap {
			Some((_, snap)) => snap.stamp.stamp_uri(uri),
			None => content_key(uri),
		};
		self.meshes.get_or_load_checked(&format!("mesh:{stamped}"), self.assets.is_offline_view(), || {
			use resources::{LoadError, LoadOutcome, StampProof};
			let (fetched, proof, keep) = match &local_snap {
				Some((path, snap)) => {
					run_test_read_hook(path);
					match self.assets.fetch_local_with_snapshot(source, path, *snap) {
						Ok((fetched, verified)) => (fetched, if verified { StampProof::Proven(stamped.clone()) } else { StampProof::Moved }, verified),
						Err(error) => {
							return Err(if snap.still_unchanged(path) { LoadError::Cached(anyhow!("{error}")) } else { LoadError::Uncached(anyhow!("{error}")) });
						}
					}
				}
				None => match self.assets.fetch(source) {
					Ok(fetched) => (fetched, StampProof::Remote, true),
					Err(error) => return Err(LoadError::Cached(anyhow!("{error}"))),
				},
			};
			let mut mesh = match ror_mesh::decode_mesh(&fetched.bytes) {
				Ok(mesh) => mesh,
				Err(error) => {
					let err = anyhow!("{uri}: {error}");
					return Err(if keep { LoadError::Cached(err) } else { LoadError::Uncached(err) });
				}
			};
			if level > 0 && !mesh.lods.is_empty() {
				mesh.lods = vec![mesh.lods[level.min(mesh.lods.len() - 1)].clone()];
			}
			let cpu = geometry::from_mesh(&mesh);
			if let Err(err) = self.gpu_room((std::mem::size_of_val(cpu.vertices.as_slice()) + std::mem::size_of_val(cpu.indices.as_slice())) as u64) {
				return Err(LoadError::Uncached(err));
			}
			let mut uploaded = GpuMesh::upload(&self.gpu, uri, &cpu);
			uploaded.proof = proof;
			Ok(LoadOutcome { value: uploaded, keep, offline_fallback: false })
		})
	}

	fn load_union(&self, asset: Option<&str>, data: Option<&Arc<Vec<u8>>>) -> Result<Arc<GpuMesh>> {
		let local_snap = match (data, asset) {
			(None, Some(asset)) => local_snapshot_for(asset),
			_ => None,
		};
		let stamped_asset = match (&local_snap, asset) {
			(Some((_, snap)), Some(asset)) => Some(snap.stamp.stamp_uri(asset)),
			(None, Some(asset)) => Some(content_key(asset)),
			_ => None,
		};
		let key = match (data, stamped_asset.as_deref()) {
			(Some(data), _) => format!("union-data:{:016x}", fnv1a(data)),
			(None, Some(stamped)) => format!("union:{stamped}"),
			(None, None) => return Err(anyhow!("union without mesh data or AssetId")),
		};
		self.meshes.get_or_load_checked(&key, self.assets.is_offline_view(), || {
			use resources::{LoadError, LoadOutcome, StampProof};
			let (bytes, proof, keep) = match (data, asset, &local_snap) {
				(Some(data), _, _) => (data.as_ref().clone(), StampProof::Remote, true),
				(None, Some(asset), Some((path, snap))) => {
					run_test_read_hook(path);
					match self.assets.fetch_local_with_snapshot(asset, path, *snap) {
						Ok((fetched, verified)) => {
							let stamped = stamped_asset.clone().unwrap_or_default();
							(fetched.bytes.clone(), if verified { StampProof::Proven(stamped) } else { StampProof::Moved }, verified)
						}
						Err(error) => {
							return Err(if snap.still_unchanged(path) { LoadError::Cached(anyhow!("{error}")) } else { LoadError::Uncached(anyhow!("{error}")) });
						}
					}
				}
				(None, Some(asset), None) => match self.assets.fetch(asset) {
					Ok(fetched) => (fetched.bytes.clone(), StampProof::Remote, true),
					Err(error) => return Err(LoadError::Cached(anyhow!("{error}"))),
				},
				(None, None, _) => unreachable!(),
			};
			let mesh = match ror_mesh::decode_union(&bytes) {
				Ok(mesh) => mesh,
				Err(error) => {
					let err = anyhow!("union: {error}");
					return Err(if keep { LoadError::Cached(err) } else { LoadError::Uncached(err) });
				}
			};
			let cpu = geometry::from_mesh(&mesh);
			if let Err(err) = self.gpu_room((std::mem::size_of_val(cpu.vertices.as_slice()) + std::mem::size_of_val(cpu.indices.as_slice())) as u64) {
				return Err(LoadError::Uncached(err));
			}
			let mut uploaded = GpuMesh::upload(&self.gpu, "union", &cpu);
			uploaded.proof = proof;
			Ok(LoadOutcome { value: uploaded, keep, offline_fallback: false })
		})
	}

	/// A texture by its URI (a local file's by its contents' stamp too, [`content_key`]): what it is and
	/// its levels' source; one kept on disk goes to the GPU only once asked for (a material map's levels
	/// from the finest a view samples, render::residency; [`Renderer::load_whole`]).
	fn load_texture(&self, uri: &str, usage: TextureUse) -> Result<Arc<GpuTexture>> {
		let composite_snaps = uri.starts_with(composite::KEY_PREFIX).then(|| composite::snapshot_layers(uri));
		let local_snap = if composite_snaps.is_none() {
			local_snapshot_for(uri).map(|(path, snap)| {
				let stamped = snap.stamp.stamp_uri(uri);
				(path, snap, stamped)
			})
		} else {
			None
		};
		let cache_key = match (&composite_snaps, &local_snap) {
			(Some((stamped, _)), _) => stamped.clone(),
			(_, Some((_, _, stamped))) => stamped.clone(),
			_ => content_key(uri),
		};
		self.textures.get_or_load_checked(&(cache_key, usage), self.assets.is_offline_view(), || self.make_texture_verified(uri, usage, local_snap.as_ref(), composite_snaps.as_ref()))
	}

	/// [`Renderer::load_texture`] with every level on the GPU (what effects, the sky's bodies and the
	/// like draw with: [`GpuTexture::view`]).
	pub(super) fn load_whole(&self, uri: &str, usage: TextureUse) -> Result<Arc<GpuTexture>> {
		let texture = self.load_texture(uri, usage)?;
		texture.levels_from(&self.gpu, 0, &|bytes| self.gpu_room(bytes)).with_context(|| uri.to_string())?;
		Ok(texture)
	}

	/// A texture pack (SurfaceAppearance / MaterialVariant TexturePack, a built-in material's pack).
	fn texture_pack(&self, uri: &str) -> Result<Arc<TexturePack>> {
		let local_snap = local_snapshot_for(uri);
		let stamped = match &local_snap {
			Some((_, snap)) => snap.stamp.stamp_uri(uri),
			None => content_key(uri),
		};
		self.packs.get_or_load_checked(&stamped, self.assets.is_offline_view(), || {
			use resources::{LoadError, LoadOutcome, StampProof};
			let (fetched, proof, keep) = match &local_snap {
				Some((path, snap)) => {
					run_test_read_hook(path);
					match self.assets.fetch_local_with_snapshot(uri, path, *snap) {
						Ok((fetched, verified)) => (fetched, if verified { StampProof::Proven(stamped.clone()) } else { StampProof::Moved }, verified),
						Err(error) => {
							return Err(if snap.still_unchanged(path) { LoadError::Cached(anyhow!("{error}")) } else { LoadError::Uncached(anyhow!("{error}")) });
						}
					}
				}
				None => match self.assets.fetch(uri) {
					Ok(fetched) => (fetched, StampProof::Remote, true),
					Err(error) => return Err(LoadError::Cached(anyhow!("{error}"))),
				},
			};
			let mut pack = match texture_pack::parse(&fetched.bytes).with_context(|| format!("{uri}: texture pack")) {
				Ok(pack) => pack,
				Err(err) => return Err(if keep { LoadError::Cached(err) } else { LoadError::Uncached(err) }),
			};
			pack.proof = proof;
			Ok(LoadOutcome { value: pack, keep, offline_fallback: false })
		})
	}

	fn material_bind_group(&self, key: &MaterialKey, missing: &mut Vec<String>) -> Arc<MaterialBinding> {
		// kept by its files' stamps: the binding of a rewritten image's first version lives on in the
		// geometry prepared with it, and each later preparation that found it by its path kept it alive
		let kept = key.stamped();
		if let Some(material) = self.materials.lock().get(kept.as_ref()).and_then(std::sync::Weak::upgrade) {
			return material;
		}
		let mut load = |uri: &Option<String>, usage: TextureUse| -> Option<Arc<GpuTexture>> {
			let uri = uri.as_ref()?;
			match self.load_texture(uri, usage) {
				Ok(texture) => Some(texture),
				Err(error) => {
					missing.push(format!("{error:#}"));
					None
				}
			}
		};
		let maps = [load(&key.color, TextureUse::Color), load(&key.normal, TextureUse::Normal), load(&key.metalness, TextureUse::Scalar), load(&key.roughness, TextureUse::Scalar), load(&key.emissive, TextureUse::Color)];
		// each map's full size, what the shader picks its mips by, and its residency slot (mesh.wgsl
		// MaterialInfo); a missing map's fallback is 1 x 1, held whole
		let mut info = [0u32; 20];
		for (index, map) in maps.iter().enumerate() {
			let (width, height, id) = map.as_ref().map_or((1.0f32, 1.0f32, 0), |map| (map.width as f32, map.height as f32, map.id));
			info[index * 2] = width.to_bits();
			info[index * 2 + 1] = height.to_bits();
			info[12 + index] = id;
		}
		let info = self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("material info"), contents: bytemuck::cast_slice(&info), usage: wgpu::BufferUsages::UNIFORM });
		let sampler = if key.clamped { &self.defaults.clamped_sampler } else { &self.defaults.material_sampler };
		let feedback = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("material residency"),
			layout: &self.pipelines.residency_material_layout,
			entries: &[wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(sampler) }, wgpu::BindGroupEntry { binding: 6, resource: info.as_entire_binding() }],
		});
		// only fully loaded materials whose local textures match `kept`'s stamps are kept; one with a
		// failed or moving texture asks the texture cache again next time
		let uris = [&key.color, &key.normal, &key.metalness, &key.roughness, &key.emissive];
		let kept_uris = [&kept.color, &kept.normal, &kept.metalness, &kept.roughness, &kept.emissive];
		let complete = uris.iter().zip(&maps).all(|(uri, texture)| uri.is_none() || texture.is_some());
		let stable = uris.iter().zip(kept_uris).zip(&maps).all(|((raw, kept_uri), texture)| match (raw, kept_uri, texture) {
			(Some(raw), Some(ku), Some(tex)) => match &tex.proof {
				resources::StampProof::Remote => !names_local_file(raw),
				resources::StampProof::Proven(actual) => actual == ku,
				resources::StampProof::Moved => false,
			},
			_ => true,
		});
		let binding = Arc::new(MaterialBinding { maps, clamped: key.clamped, info, feedback, group: Mutex::new(None) });
		if complete && stable {
			self.materials.lock().insert(kept.into_owned(), Arc::downgrade(&binding));
		}
		binding
	}

	/// A material's bind group as its maps are held now (made again once they hold more levels than
	/// when it was last made).
	fn material_group(&self, material: &MaterialBinding) -> wgpu::BindGroup {
		let held: [Option<resources::Held>; 5] = std::array::from_fn(|index| material.maps[index].as_ref().and_then(|map| map.held()));
		let bases = held.each_ref().map(|held| held.as_ref().map_or(u32::MAX, |held| held.base));
		let mut group = material.group.lock();
		if let Some((made, group)) = group.as_ref()
			&& *made == bases
		{
			return group.clone();
		}
		let fallbacks = [&self.defaults.white, &self.defaults.normal, &self.defaults.black, &self.defaults.white, &self.defaults.black];
		let views: [wgpu::TextureView; 5] = std::array::from_fn(|index| held[index].as_ref().map_or_else(|| fallbacks[index].view.clone(), |held| held.view.clone()));
		let made = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("material"),
			layout: &self.pipelines.material_layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[0]) },
				wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[1]) },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[2]) },
				wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[3]) },
				wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(if material.clamped { &self.defaults.clamped_sampler } else { &self.defaults.material_sampler }) },
				wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[4]) },
				wgpu::BindGroupEntry { binding: 6, resource: material.info.as_entire_binding() },
			],
		});
		*group = Some((bases, made.clone()));
		made
	}

	fn environment(&self, scene: &Scene, missing: &mut Vec<String>, untrusted: &mut usize) -> Arc<EnvironmentMaps> {
		let sky = scene.environment.sky.clone().unwrap_or_default();
		let optional = scene.environment.default_sky_unused();
		// the Atmosphere's haze and glare change the sky drawn and reflected (atmosphere-light), so they
		// and, with a glare, where it sits, its share by the hour and Brightness key the maps too
		let lighting = &scene.environment;
		let atmosphere = lighting.atmosphere.filter(|atmosphere| atmosphere.haze > 0.0);
		let glare_light = environment::glare_light(lighting.clock_time, lighting.geographic_latitude);
		let glare_tint = environment::glare_tint(lighting.color_shift_top, lighting.environment_diffuse_scale);
		let atmosphere_key = atmosphere.map(|atmosphere| {
			let glare = (atmosphere.glare > 0.0).then(|| format!("{:.4?}|{:.4}|{:.4?}", glare_light, lighting.brightness, glare_tint));
			format!("{:.4}|{:.4?}|{:.4?}|{:.4}|{glare:?}", atmosphere.haze, atmosphere.color, atmosphere.decay, atmosphere.glare)
		});
		// the time of day tints the skybox and its light (sky-time, sky-night), at night by the sky's
		// light too (light-night-sky2)
		let tint = environment::sky_tint(lighting.clock_time, lighting.environment_diffuse_scale);
		// and through the twilights its light less than the drawn sky's tint gives (sky_light_twilight)
		let twilight = environment::sky_light_twilight(lighting.clock_time);
		let tint_key = tint.map(|tint| format!("{tint:.4?}|{twilight:.4}"));
		let sky_snaps: [Option<(std::path::PathBuf, ror_assets::FileSnapshot)>; 6] = std::array::from_fn(|index| local_snapshot_for(&sky.skybox[index]));
		let mut untrusted_sky = std::collections::HashSet::new();
		for (path, snap) in sky_snaps.iter().flatten() {
			if snap.is_untrusted_hash() {
				untrusted_sky.insert(path.clone());
			}
		}
		*untrusted += untrusted_sky.len();
		let skybox: [String; 6] = std::array::from_fn(|index| match &sky_snaps[index] {
			Some((_, snap)) => snap.stamp.stamp_uri(&sky.skybox[index]),
			None => content_key(&sky.skybox[index]),
		});
		let sky_key = format!("{skybox:?}|{:?}", sky.skybox_orientation);
		let key = format!("{sky_key}|{atmosphere_key:?}|{tint_key:?}");
		if let Some(maps) = self.environments.get(&key) {
			return maps;
		}
		// Made on a thread of its own, its parallel work on a pool of its own. A rayon worker waiting
		// for another pool (or for its own pool's par_iter) runs its pool's queued work meanwhile: a
		// worker making these maps took on another frame of the clip, which asked for the same maps
		// and waited on the initialiser its own thread was running (four videos started together on a
		// new service hung so, at 0 % CPU). And a sky's work on the main pool could find every worker
		// waiting for these maps. Here neither the initialiser's thread nor the inner pool's work
		// waits on a cache.
		let transient_failure = self.assets.is_offline_view();
		let result = std::thread::scope(|scope| {
			scope
				.spawn(|| {
					let base = self.skies.get_or_load_checked(&sky_key, transient_failure, || inner_pool().install(|| self.sky_base(&sky, &sky_snaps)));
					self.environments.get_or_load_checked(&key, transient_failure, || {
						let base = match base {
							Ok(base) => base,
							Err(error) => {
								let moved = sky_snaps.iter().any(|snap| snap.as_ref().is_some_and(|(path, s)| !s.still_unchanged(path)));
								return Err(if moved { resources::LoadError::Uncached(error) } else { resources::LoadError::Cached(error) });
							}
						};
						let stable = base.stable;
						let maps = inner_pool().install(|| self.environment_maps(&sky, &base, tint, twilight, atmosphere, glare_light, glare_tint, lighting.brightness));
						Ok(resources::LoadOutcome { value: maps, keep: stable, offline_fallback: false })
					})
				})
				.join()
				.unwrap_or_else(|_| Err(anyhow!("making the sky's maps panicked")))
		});
		match result {
			Ok(maps) => maps,
			Err(error) => {
				if !optional {
					missing.push(format!("{error:#} (using a gradient sky)"));
				}
				self.gradient_environment()
			}
		}
	}

	/// The gradient sky drawn where a skybox's maps cannot be made, made once as those maps are
	/// ([`Renderer::environment`]): on a thread of its own, its prefiltering on the inner pool. Made on
	/// the frame's rayon worker, the prefiltering's par_iter took on another frame whose sky failed
	/// too, which waited on the gradient its own thread was making (every frame then waiting, 0 % CPU).
	fn gradient_environment(&self) -> Arc<EnvironmentMaps> {
		let key = "gradient".to_string();
		if let Some(maps) = self.environments.get(&key) {
			return maps;
		}
		let made = std::thread::scope(|scope| {
			scope
				.spawn(|| {
					self.environments.get_or_load(&key, || {
						Ok(inner_pool().install(|| {
							let fallback = environment::gradient_cube(64, Vec3::new(0.22, 0.42, 0.85), Vec3::new(0.62, 0.74, 0.92), Vec3::new(0.35, 0.33, 0.3));
							let mut light = fallback.clone();
							while light.size > SKY_LIGHT_TEXELS {
								light = light.downsample();
							}
							self.upload_environment(&fallback, &fallback, None, None, &fallback, light.ambient_cube(), None)
						}))
					})
				})
				.join()
		});
		made.expect("making the gradient sky panicked").expect("gradient sky")
	}

	/// A Lighting's sky maps from its skybox's base ([`Renderer::environment`]).
	#[allow(clippy::too_many_arguments)]
	fn environment_maps(&self, sky: &crate::scene::environment::Sky, base: &SkyBase, tint: Option<(Vec3, Vec3)>, twilight: f32, atmosphere: Option<crate::scene::environment::Atmosphere>, glare_light: (Vec3, Vec3), glare_tint: Vec3, brightness: f32) -> EnvironmentMaps {
		// SkyboxOrientation turns the skybox drawn and reflected, not the sky light nor what distant
		// surfaces fade into (sky-orientation: Studio's ball lit alike and a far sphere fading into
		// the same unturned sky under five orientations, its reflections turning)
		fn tinted(cube: &environment::CubeImage, tint: Option<(Vec3, Vec3)>) -> std::borrow::Cow<'_, environment::CubeImage> {
			match tint {
				Some(tint) => std::borrow::Cow::Owned(cube.tinted(tint)),
				None => std::borrow::Cow::Borrowed(cube),
			}
		}
		let light = base.upright.as_ref().map_or(&base.light, |(_, light)| light);
		let ambient = match tint {
			Some(tint) => light.tinted_by_halves(tint).ambient_cube().map(|light| light * twilight),
			None => light.ambient_cube(),
		};
		let cube = tinted(&base.cube, tint);
		let cube = cube.as_ref();
		let upright = base.upright.as_ref().map(|(upright, _)| tinted(upright, tint));
		// the sky light stays the skybox's own (atmosphere-light: the sky diffuse is the same with
		// and without haze); what is drawn and reflected is the sky through the haze
		let hazed = atmosphere.map(|atmosphere| environment::AtmosphereSky::new(&atmosphere, ambient, glare_light, brightness, glare_tint));
		let shown = hazed.as_ref().map(|hazed| environment::atmosphere_cube(cube, hazed));
		// by the time of day's tint the haze drawn, and what distant surfaces fade into, is lit by the
		// skybox's untinted light, the haze reflected by the tinted (light-night-lobby, -haze: a
		// place's Atmosphere at midnight drawn nearly as bright as at 14.5 below the horizon and
		// above, whatever Brightness, EnvironmentDiffuseScale and skybox; a floor 30-70 studs off
		// fading into it as ror's untinted haze gives with EnvironmentSpecularScale 0, while the sky
		// specular takes 0.02 of the floor's light where reflecting the untinted haze added up to
		// 0.09 of blue)
		let drawn = match (tint, atmosphere) {
			(Some(_), Some(atmosphere)) => Some(environment::AtmosphereSky::new(&atmosphere, light.ambient_cube(), glare_light, brightness, glare_tint)),
			_ => None,
		};
		let drawn_cube = drawn.as_ref().map(|drawn| environment::atmosphere_cube(cube, drawn));
		let haze = hazed.as_ref().zip(drawn.as_ref()).map(|(hazed, drawn)| (hazed.light_over(drawn), *hazed));
		let drawn_params = drawn.as_ref().or(hazed.as_ref()).map(|drawn| drawn.shader_params());
		// the images are the sky base's, counted with it
		let images = base.images.clone().map(|(view, _)| SkyImages { view, bytes: 0, rotation: environment::sky_rotation(sky.skybox_orientation), tint, atmosphere: drawn_params });
		let reflected = shown.as_ref().unwrap_or(cube);
		let faded = upright.as_ref().map(|upright| match &hazed {
			Some(hazed) => environment::atmosphere_cube(upright, hazed),
			None => upright.as_ref().clone(),
		});
		let skybox = upright.as_deref().unwrap_or(cube);
		self.upload_environment(drawn_cube.as_ref().unwrap_or(reflected), reflected, faded.as_ref(), images, skybox, ambient, haze)
	}

	/// A skybox's six images decoded: the cubes its maps start from and the images the sky pass draws.
	fn sky_base(&self, sky: &crate::scene::environment::Sky, snaps: &[Option<(std::path::PathBuf, ror_assets::FileSnapshot)>; 6]) -> std::result::Result<resources::LoadOutcome<SkyBase>, resources::LoadError> {
		let faces: Vec<Result<SkyFaceImages>> = sky.skybox.par_iter().enumerate().map(|(index, uri)| self.sky_face(uri, snaps[index].as_ref())).collect();
		if !faces.iter().all(|face| face.is_ok()) {
			let reasons: Vec<String> = faces.into_iter().filter_map(|face| face.err()).map(|error| format!("{error:#}")).collect();
			let moved = snaps.iter().any(|snap| snap.as_ref().is_some_and(|(path, s)| !s.still_unchanged(path)));
			let err = anyhow!("sky: {}", reasons.join("; "));
			return Err(if moved { resources::LoadError::Uncached(err) } else { resources::LoadError::Cached(err) });
		}
		let mut cubes = Vec::new();
		let mut lights = Vec::new();
		let mut sizes = Vec::new();
		let mut blocks = Vec::new();
		let mut stable = true;
		for face in faces.into_iter().flatten() {
			stable &= face.verified;
			cubes.push(face.radiance);
			lights.push(face.light);
			sizes.push((face.width, face.height));
			blocks.push(face.blocks);
		}
		let faces: [environment::SkyFace; 6] = cubes.try_into().map_err(|_| resources::LoadError::Cached(anyhow!("six sky faces")))?;
		let lights: [environment::SkyFace; 6] = lights.try_into().map_err(|_| resources::LoadError::Cached(anyhow!("six sky faces")))?;
		// the sky pass draws the images themselves when they share one square size: BC1 images (the
		// default sky's DXT1 .tex files) as their blocks and mips, which the GPU decodes as it does
		// Studio's
		let size = sizes[0].0;
		let square = sizes.iter().all(|(width, height)| *width == size && *height == size);
		let levels = blocks.first().and_then(|first| first.as_ref()).map(|first| first.len());
		let images = if square && self.gpu.supports_bc() && levels.is_some() && blocks.iter().all(|face| face.as_ref().map(|face| face.len()) == levels) {
			let blocks: Vec<Vec<Vec<u8>>> = blocks.into_iter().flatten().collect();
			Some(self.upload_sky_blocks(&blocks, size))
		} else {
			square.then(|| {
				let texels: Vec<&[u8]> = faces.iter().map(|face| face.texels().unwrap_or_default()).collect();
				self.upload_sky_images(&texels, size)
			})
		};
		let cube = environment::sky_cube(&faces, SKY_CUBE_TEXELS, sky.skybox_orientation);
		let light = environment::sky_cube(&lights, SKY_LIGHT_TEXELS, sky.skybox_orientation);
		let upright = (sky.skybox_orientation != Vec3::ZERO).then(|| (environment::sky_cube(&faces, SKY_CUBE_TEXELS, Vec3::ZERO), environment::sky_cube(&lights, SKY_LIGHT_TEXELS, Vec3::ZERO)));
		let cubes = [Some(&cube), Some(&light), upright.as_ref().map(|(cube, _)| cube), upright.as_ref().map(|(_, light)| light)];
		let bytes = cubes.iter().flatten().map(|cube| cube.faces.iter().map(|face| face.len() as u64 * 12).sum::<u64>()).sum::<u64>() + images.as_ref().map_or(0, |(_, bytes)| *bytes);
		Ok(resources::LoadOutcome { value: SkyBase { cube, light, upright, images, bytes, stable }, keep: stable, offline_fallback: false })
	}

	/// A skybox image as radiance, the same at the sky light's resolution, and its texels as stored
	/// for the sky pass.
	fn sky_face(&self, uri: &str, snap: Option<&(std::path::PathBuf, ror_assets::FileSnapshot)>) -> Result<SkyFaceImages> {
		// the uploaded image, not its transcode (lobby-sky: a place's skybox within 2 of 255 on
		// 98.8-99.9 % of pixels from its PNGs, 76-84 % from their KTX2 transcodes)
		let (fetched, verified) = match snap {
			Some((path, snap)) => {
				run_test_read_hook(path);
				self.assets.fetch_local_with_snapshot(uri, path, *snap).map_err(|error| anyhow!("{error}"))?
			}
			None => (self.assets.fetch(uri).map_err(|error| anyhow!("{error}"))?, !names_local_file(uri)),
		};
		let image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow!("{uri}: {error}"))?;
		let blocks = (image.format == ror_image::PixelFormat::Bc1 && image.layers <= 1 && image.depth <= 1 && image.width % 4 == 0 && image.height % 4 == 0).then(|| image.levels.clone());
		let mut image = ror_image::to_rgba8(&image);
		let level = image.levels.first().context("sky face without pixels")?;
		// Studio shows sky texels as radiance 1.935 · texel² (sky-faces: mean error 0.6 of 255): each
		// 8-bit value's, read so as the cube is made (environment::SkyPixels)
		let radiance = |texel: Vec3| linear(texel) * SKY_RADIANCE;
		let table: Box<[f32; 256]> = Box::new(std::array::from_fn(|value| radiance(Vec3::splat(value as f32 / 255.0)).x));
		// the sky light reads the image's own mip of SKY_LIGHT_TEXELS texels a side (the default
		// sky's stored 4 x 4 BC1 mips: Studio's ambient cube within 1 %, the full image box-filtered
		// in linear light 2-5 % off, redder), else the texels averaged in linear light
		// (lobby-sky-light: a white ball under a place's PNG skybox within 1-3 of 255, 3-4 below
		// it averaged before the square)
		let mip = (0..image.levels.len()).find(|&level| {
			let (width, height, _) = image.level_size(level);
			width <= SKY_LIGHT_TEXELS && height <= SKY_LIGHT_TEXELS
		});
		let light = match mip {
			Some(mip) if mip > 0 => {
				let (width, height, _) = image.level_size(mip);
				let pixels = image.levels[mip].chunks_exact(4).map(|px| radiance(Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32) / 255.0)).collect();
				environment::SkyFace { width, height, pixels: environment::SkyPixels::Radiance(pixels) }
			}
			_ => {
				let side = SKY_LIGHT_TEXELS;
				let mut pixels = Vec::with_capacity((side * side) as usize);
				for y in 0..side {
					for x in 0..side {
						let (x0, x1) = (x * image.width / side, ((x + 1) * image.width / side).max(x * image.width / side + 1).min(image.width));
						let (y0, y1) = (y * image.height / side, ((y + 1) * image.height / side).max(y * image.height / side + 1).min(image.height));
						let mut sum = Vec3::ZERO;
						for py in y0.min(image.height - 1)..y1 {
							for px in x0.min(image.width - 1)..x1 {
								let at = ((py * image.width + px) * 4) as usize;
								sum += radiance(Vec3::new(level[at] as f32, level[at + 1] as f32, level[at + 2] as f32) / 255.0);
							}
						}
						let count = ((x1 - x0.min(image.width - 1)) * (y1 - y0.min(image.height - 1))).max(1) as f32;
						pixels.push(sum / count);
					}
				}
				environment::SkyFace { width: side, height: side, pixels: environment::SkyPixels::Radiance(pixels) }
			}
		};
		let (width, height) = (image.width, image.height);
		let rgba = std::mem::take(&mut image.levels[0]);
		let face = environment::SkyFace { width, height, pixels: environment::SkyPixels::Texels { rgba, radiance: table } };
		Ok(SkyFaceImages { radiance: face, light, blocks, width, height, verified })
	}

	/// The skybox's six images as stored (all `size` square) in a 2D array with mips box-filtered on
	/// the stored values, from the first level of at most SKY_IMAGE_TEXELS a side, for the sky pass;
	/// returns the array and its size in bytes.
	fn upload_sky_images(&self, images: &[&[u8]], size: u32) -> (wgpu::TextureView, u64) {
		let device = &self.gpu.device;
		// the levels below the images themselves, each halved from the one above
		let mut below: Vec<Vec<Vec<u8>>> = Vec::new();
		fn level_faces<'a>(images: &[&'a [u8]], below: &'a [Vec<Vec<u8>>], level: usize) -> Vec<&'a [u8]> {
			if level == 0 { images.to_vec() } else { below[level - 1].iter().map(Vec::as_slice).collect() }
		}
		let mut level_size = size;
		while level_size > 1 {
			let next = (level_size / 2).max(1);
			let previous = level_faces(images, &below, below.len());
			let halved: Vec<Vec<u8>> = previous
				.par_iter()
				.map(|face| {
					let mut out = vec![0u8; (next * next * 4) as usize];
					for y in 0..next {
						for x in 0..next {
							for channel in 0..4 {
								let at = |dx: u32, dy: u32| face[((((y * 2 + dy).min(level_size - 1)) * level_size + (x * 2 + dx).min(level_size - 1)) * 4 + channel) as usize] as u32;
								out[((y * next + x) * 4 + channel) as usize] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
							}
						}
					}
					out
				})
				.collect();
			below.push(halved);
			level_size = next;
		}
		let count = below.len() + 1;
		let first = (0..count).find(|&level| (size >> level) <= SKY_IMAGE_TEXELS).unwrap_or(0);
		let levels: Vec<Vec<&[u8]>> = (first..count).map(|level| level_faces(images, &below, level)).collect();
		let size = (size >> first).max(1);
		let texture = device.create_texture(&wgpu::TextureDescriptor {
			label: Some("sky images"),
			size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 6 },
			mip_level_count: levels.len() as u32,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::Rgba8Unorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});
		for (level, faces) in levels.iter().enumerate() {
			let side = (size >> level).max(1);
			for (layer, face) in faces.iter().enumerate() {
				self.gpu.queue.write_texture(
					wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 }, aspect: wgpu::TextureAspect::All },
					face,
					wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
					wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
				);
			}
		}
		let bytes = levels.iter().flatten().map(|face| face.len() as u64).sum();
		(texture.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() }), bytes)
	}

	/// The skybox's six BC1 images (all `size` square, the same number of levels) in a 2D array of
	/// their own blocks and mips, from the first level of at most SKY_IMAGE_TEXELS a side; returns the
	/// array and its size in bytes.
	fn upload_sky_blocks(&self, images: &[Vec<Vec<u8>>], size: u32) -> (wgpu::TextureView, u64) {
		let device = &self.gpu.device;
		let levels = images[0].len();
		let first = (0..levels).find(|&level| (size >> level) <= SKY_IMAGE_TEXELS).unwrap_or(0);
		let side = (size >> first).max(1);
		let texture = device.create_texture(&wgpu::TextureDescriptor {
			label: Some("sky images"),
			size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 6 },
			mip_level_count: (levels - first) as u32,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::Bc1RgbaUnorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});
		for level in first..levels {
			let width = (size >> level).max(1);
			let blocks = width.div_ceil(4);
			for (layer, face) in images.iter().enumerate() {
				// levels below 4 x 4 still occupy whole blocks: the copy is the block-rounded size
				self.gpu.queue.write_texture(
					wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: (level - first) as u32, origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 }, aspect: wgpu::TextureAspect::All },
					&face[level],
					wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(blocks * 8), rows_per_image: Some(blocks) },
					wgpu::Extent3d { width: blocks * 4, height: blocks * 4, depth_or_array_layers: 1 },
				);
			}
		}
		let bytes = images.iter().flat_map(|face| &face[first..]).map(|level| level.len() as u64).sum();
		(texture.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() }), bytes)
	}

	/// Uploads the sky shown (the skybox, or the skybox through the Atmosphere) and the specular chain
	/// of the sky `reflected` (the one shown but for the haze's light at night); `images` are the
	/// skybox's own images the sky pass draws instead when there are, `skybox` (the skybox itself)
	/// gives the irradiance particles, beams and GUIs read, `light` is the sky light's ambient cube and
	/// `haze` what the shown haze adds to the reflected one's ([`environment::AtmosphereSky::light_over`])
	/// with the reflected Atmosphere, whose haze share the chain's alpha holds, blurred as its colour;
	/// `faded`, when it differs from `reflected` (the skybox unturned), gets a chain of its own that
	/// distant surfaces fade into.
	fn upload_environment(&self, cube: &CubeImage, reflected: &CubeImage, faded: Option<&CubeImage>, images: Option<SkyImages>, skybox: &CubeImage, light: [Vec3; 6], haze: Option<([Vec3; 6], environment::AtmosphereSky)>) -> EnvironmentMaps {
		let device = &self.gpu.device;
		let to_halves = |cube: &CubeImage, alpha: Option<&CubeImage>| -> Vec<u16> {
			let mut halves = Vec::with_capacity(cube.faces.iter().map(|face| face.len() * 4).sum());
			for (face, texels) in cube.faces.iter().enumerate() {
				for (index, c) in texels.iter().enumerate() {
					halves.extend([half(c.x), half(c.y), half(c.z), half(alpha.map_or(1.0, |alpha| alpha.faces[face][index].x))]);
				}
			}
			halves
		};
		let sky_texture = device.create_texture_with_data(
			&self.gpu.queue,
			&wgpu::TextureDescriptor {
				label: Some("sky"),
				size: wgpu::Extent3d { width: cube.size, height: cube.size, depth_or_array_layers: 6 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rgba16Float,
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			},
			wgpu::util::TextureDataOrder::LayerMajor,
			bytemuck::cast_slice(&to_halves(cube, None)),
		);
		let levels = 6;
		// a specular chain (128 texels a side at most) and its size, the haze's share in its alpha
		let chain_texture = |source: &CubeImage| -> (wgpu::Texture, u32) {
			let mut base = source.clone();
			while base.size > 128 {
				base = base.downsample();
			}
			let (chain, shares) = match &haze {
				Some((_, hazed)) => {
					let [chain, shares] = environment::prefilter_specular_all([&base, &hazed.haze_cube(base.size)], levels);
					(chain, Some(shares))
				}
				None => (environment::prefilter_specular(&base, levels), None),
			};
			let texture = device.create_texture(&wgpu::TextureDescriptor {
				label: Some("specular"),
				size: wgpu::Extent3d { width: base.size, height: base.size, depth_or_array_layers: 6 },
				mip_level_count: levels,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::Rgba16Float,
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			});
			for (level, image) in chain.iter().enumerate() {
				self.gpu.queue.write_texture(
					wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
					bytemuck::cast_slice(&to_halves(image, shares.as_ref().map(|shares| &shares[level]))),
					wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(image.size * 8), rows_per_image: Some(image.size) },
					wgpu::Extent3d { width: image.size, height: image.size, depth_or_array_layers: 6 },
				);
			}
			(texture, base.size)
		};
		let (specular_texture, chain_size) = chain_texture(reflected);
		let faded_texture = faded.map(|faded| chain_texture(faded).0);
		let mut small = skybox.clone();
		while small.size > 32 {
			small = small.downsample();
		}
		let cube_view = |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::Cube), ..Default::default() });
		let image_bytes = images.as_ref().map_or(0, |images| images.bytes);
		let chain_bytes = (chain_size * chain_size * 6 * 8 * 4 / 3) as u64 * if faded_texture.is_some() { 2 } else { 1 };
		let specular = cube_view(&specular_texture);
		EnvironmentMaps {
			sky: cube_view(&sky_texture),
			images,
			faded: faded_texture.as_ref().map_or_else(|| specular.clone(), cube_view),
			specular,
			specular_levels: levels,
			sh: small.sh9(),
			cube: light,
			haze: haze.map(|(light, _)| light),
			bytes: (cube.size * cube.size * 6 * 8) as u64 + chain_bytes + image_bytes,
		}
	}

	/// The unit mesh and its local transform (into the part's space) for a part's geometry.
	fn part_mesh(&self, part: &Part) -> Result<(Arc<GpuMesh>, Affine3A, bool)> {
		let primitive = |kind: Primitive| self.primitives[&kind].clone();
		let size = part.size;
		Ok(match &part.geometry {
			Geometry::Block | Geometry::Truss { .. } => (primitive(Primitive::Block), Affine3A::from_scale(size), false),
			Geometry::Ball => (primitive(Primitive::Sphere), Affine3A::from_scale(Vec3::splat(size.min_element())), false),
			Geometry::Cylinder => {
				let diameter = size.y.min(size.z);
				(primitive(Primitive::Cylinder), Affine3A::from_scale(Vec3::new(size.x, diameter, diameter)), false)
			}
			Geometry::Wedge => (primitive(Primitive::Wedge), Affine3A::from_scale(size), false),
			Geometry::CornerWedge => (primitive(Primitive::CornerWedge), Affine3A::from_scale(size), false),
			Geometry::MeshPart { mesh, initial_size, .. } => {
				// the mesh's own origin at the part, scaled by Size / InitialSize; an absent InitialSize
				// counts as (1, 1, 1) (mesh-origin; notes/mesh.md "Placement in Studio")
				let gpu_mesh = self.load_mesh(mesh)?;
				let reference = Vec3::select(initial_size.cmpgt(Vec3::splat(1e-6)), *initial_size, Vec3::ONE);
				(gpu_mesh, Affine3A::from_scale(size / reference), true)
			}
			Geometry::Union { asset, mesh_data, initial_size, .. } => {
				let gpu_mesh = self.load_union(asset.as_deref(), mesh_data.as_ref())?;
				let (min, max) = gpu_mesh.bounds;
				let extent = (max - min).max(Vec3::splat(1e-6));
				let reference = if initial_size.min_element() > 1e-6 { *initial_size } else { extent };
				let center = (min + max) * 0.5;
				(gpu_mesh, Affine3A::from_scale(size / reference) * Affine3A::from_translation(-center), true)
			}
			// collision data is centred on the part, in the mesh's unscaled space (scene::collision)
			Geometry::Collision { mesh, key, initial_size } => {
				let gpu_mesh = self.meshes.get_or_load(&format!("collision:{key:016x}"), || {
					let triangles = ror_mesh::Mesh {
						positions: mesh.positions.clone(),
						normals: mesh.normals.clone(),
						colors: mesh.colors.clone(),
						indices: mesh.indices.clone(),
						lods: vec![0..mesh.indices.len() as u32],
						..Default::default()
					};
					Ok(GpuMesh::upload(&self.gpu, "collision", &geometry::from_mesh(&triangles)))
				})?;
				let reference = Vec3::select(initial_size.cmpgt(Vec3::splat(1e-6)), *initial_size, Vec3::ONE);
				(gpu_mesh, Affine3A::from_scale(size / reference), true)
			}
			Geometry::Special { mesh_type, mesh, scale, offset } => {
				use crate::scene::extract::mesh_type as kind;
				// Measured on Roblox client 0.748.0.7481526: built-in primitive SpecialMeshes
				// (Brick, Sphere, Cylinder, Wedge, Head) apply `abs(Scale)` (never mirrored or
				// inverted), whereas FileMesh applies signed `Scale` with standard back-face
				// culling (odd negative axes intentionally invert winding across main, Highlight,
				// and occlusion passes).
				let abs_scale = scale.abs();
				match *mesh_type {
					kind::FILE_MESH => {
						let uri = mesh.as_ref().context("FileMesh without MeshId")?;
						(self.load_mesh(uri)?, Affine3A::from_translation(*offset) * Affine3A::from_scale(*scale), true)
					}
					kind::SPHERE => (primitive(Primitive::Sphere), Affine3A::from_translation(*offset) * Affine3A::from_scale(size * abs_scale), false),
					// along X, like Part cylinders (specialmesh-sizes: its top view is a rectangle)
					kind::CYLINDER => (primitive(Primitive::Cylinder), Affine3A::from_translation(*offset) * Affine3A::from_scale(size * abs_scale), false),
					kind::WEDGE => (primitive(Primitive::Wedge), Affine3A::from_translation(*offset) * Affine3A::from_scale(size * abs_scale), false),
					kind::HEAD => {
						// Studio's head mesh, sized by the part's smaller horizontal extent and its height
						// (notes/render.md "SpecialMesh"); a block until the mesh is available
						let horizontal = (size.x * abs_scale.x).min(size.z * abs_scale.z);
						let vertical = size.y * abs_scale.y;
						match self.load_mesh(HEAD_MESH) {
							Ok(mesh) => (mesh, Affine3A::from_translation(*offset) * Affine3A::from_scale(Vec3::new(horizontal * 0.797, vertical * 0.822, horizontal * 0.797)), false),
							Err(_) => (primitive(Primitive::Cylinder), Affine3A::from_translation(*offset) * Affine3A::from_scale(Vec3::new(horizontal * 0.955, vertical * 0.988, horizontal * 0.955)) * Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2), false),
						}
					}
					_ => (primitive(Primitive::Block), Affine3A::from_translation(*offset) * Affine3A::from_scale(size * abs_scale), false),
				}
			}
		})
	}

	/// Loads what `scene` draws with into the caches (meshes; material, decal, particle, beam and trail
	/// textures) without drawing it: for a caller that readies its next scene on another thread while
	/// this renderer draws the current one. Failures are left for the render to report.
	pub fn prefetch(&self, scene: &Scene) {
		let _: Vec<Result<(Arc<GpuMesh>, Affine3A, bool)>> = scene.parts.par_iter().enumerate().map(|(index, part)| self.part_mesh(part).map(|drawn| self.skinned(part, drawn, scene.live.then_some(index)))).collect();
		let materials: Vec<PartMaterial> = scene.parts.par_iter().map(|part| self.part_material(part, &scene.materials, &mut Vec::new())).collect();
		let keys: Vec<&MaterialKey> = materials.iter().map(|material| &material.key).collect::<std::collections::HashSet<_>>().into_iter().collect();
		keys.par_iter().for_each(|key| {
			for (uri, usage) in key.textures() {
				let _ = self.load_texture(uri, usage);
			}
		});
		let images: Vec<String> = scene
			.parts
			.iter()
			.flat_map(|part| part.decals.iter().map(|decal| transcode::image_key(&decal.texture)))
			.chain(scene.particles.iter().filter_map(|emitter| emitter.texture.as_deref().map(transcode::image_key)))
			.collect::<std::collections::HashSet<_>>()
			.into_iter()
			.collect();
		images.par_iter().for_each(|uri| {
			let _ = self.load_texture(uri, TextureUse::Color);
		});
		let mut missing = Vec::new();
		let _ = self.beam_textures(scene, &mut missing);
		let _ = self.trail_textures(scene, &mut missing);
	}

	/// The bounds a view's orbit frames for this scene, as [`Renderer::render`] frames them: the
	/// parts' boxes widened by what their meshes draw (loads the meshes).
	pub fn framing_bounds(&self, scene: &Scene) -> Option<(Vec3, Vec3)> {
		scene.framing_bounds_drawn(&self.drawn_boxes(scene))
	}

	/// Each part's box as it draws, in its own space (`None`: its mesh did not load, it draws its
	/// box), as [`Scene::framing_bounds_drawn`] takes them (loads the meshes).
	pub fn drawn_boxes(&self, scene: &Scene) -> Vec<Option<(Vec3, Vec3)>> {
		let meshes: Vec<Result<(Arc<GpuMesh>, Affine3A, bool)>> = each_part(scene, |index, part| self.part_mesh(part).map(|drawn| self.skinned(part, drawn, scene.live.then_some(index))));
		drawn_boxes(&meshes)
	}

	#[allow(clippy::too_many_arguments)]
	fn prepare(&self, scene: &Scene, stats: &mut RenderStats, lease: &reuse::Lease, sky: Option<PreparedSky>, views: &[ViewRequest], defer_maps: bool, clip: Option<&prepared::ClipGeometry>) -> Result<Prepared> {
		self.memory_checkpoint("prepare");
		let neon_share = scene.environment.viewport.as_ref().map_or(1.0, |_| environment::viewport_neon());
		let (sky, key, ahead) = match sky {
			Some(PreparedSky { maps, missing, key, particles, untrusted_hashes, .. }) => (Some((maps, missing, untrusted_hashes)), key, particles),
			None => (None, None, None),
		};
		let key = key.or_else(|| scene_key(scene));
		stats.untrusted_stamp_hashes += key.as_ref().map_or(0, |key| key.untrusted_hashes);
		// a clip's frame after one that moved starts from that frame (prepared::ClipGeometry): it is not
		// a scene kept whole (moving scenes are not), and looking for one compared its whole key
		let last = clip.and_then(|clip| clip.last.lock().take());
		let offline_view = self.assets.is_offline_view();
		let found = if last.as_ref().is_some_and(|last| last.moving) { None } else { key.as_ref().and_then(|key| self.preparations.get(key, self.dropped_failures(offline_view), offline_view)) };
		let geometry = match found {
			Some(geometry) => {
				stats.geometry_reused = true;
				// what its loads could not load, reported as its fresh preparation reported it
				stats.missing.extend(geometry.missing.iter().cloned());
				if let (Some(clip), Some(mut last)) = (clip, last) {
					last.moving = false;
					*clip.last.lock() = Some(last);
				}
				self.under_this_sun(scene, geometry, views, lease)
			}
			None => {
				let failures = self.failures(offline_view);
				let (mut geometry, moving, frame, remembered, offline_fallback, all_stable) = self.prepare_geometry(scene, stats, neon_share, key.as_deref(), lease, views, defer_maps, last)?;
				// A missing asset can succeed on a later request: a scene some of whose loads failed is kept
				// only while the caches remember every failure it met, until the first of them is retried
				// (prepared::Retry), keeping their ordinary retry policy. An offline view that hit any
				// offline miss or silent fallback (e.g. TexturePack layers or Head mesh) stays offline-only
				// so it can never poison online renders. A scene some of whose parts changed since the last
				// is moving: its next frame differs again (its parts are kept instead, `prepare_geometry`),
				// unless it is drawn again unchanged (Preparations::repeated). A scene whose local files
				// moved between keying and loading (`!all_stable`) is drawn for this render alone and never
				// kept under the stale key.
				let had_failures = !geometry.missing.is_empty() || offline_fallback;
				let retry = (had_failures && remembered).then(|| prepared::Retry::of(failures, self.failures(offline_view))).flatten();
				let offline_only = offline_view && had_failures;
				let keep = all_stable && (!had_failures || retry.is_some()) && key.as_ref().is_some_and(|key| !moving || self.preparations.repeated(key));
				if keep && moving {
					// (prepare_geometry counts what a moving scene holds as nothing: it is not kept)
					geometry.held = Self::held_by(&geometry.draws, geometry.organic_noise.as_ref(), geometry.surface_maps.as_ref());
				}
				let geometry = Arc::new(geometry);
				if all_stable
					&& let (Some(clip), Some(frame), Some(key)) = (clip, frame, key.clone())
				{
					*clip.last.lock() = Some(frame.finish(key, geometry.clone(), moving));
				}
				if keep && let Some(key) = key && self.preparations.insert(key, geometry.clone(), retry, offline_only) {
					lease.forget_buffer(&geometry.instances);
					lease.forget_buffer(&geometry.no_lights);
					if let Some(texture) = &geometry.light_grid_texture {
						lease.hand_out(texture);
					}
				}
				geometry
			}
		};
		let assets_start = Instant::now();
		let environment = match sky {
			Some((maps, missing, untrusted_hashes)) => {
				stats.missing.extend(missing);
				stats.untrusted_stamp_hashes += untrusted_hashes;
				maps
			}
			None => self.environment(scene, &mut stats.missing, &mut stats.untrusted_stamp_hashes),
		};
		let effects = self.effect_bindings(scene, &mut stats.missing, &mut stats.untrusted_stamp_hashes);
		let (beam_textures, trail_textures) = (effects.beams.clone(), effects.trails.clone());
		let particles = self.prepare_particles(scene, &effects.particles, ahead);
		drop(effects);
		(stats.particles, stats.emitters, stats.beams, stats.trails) = (particles.drawn(), scene.particles.len(), scene.beams.len(), scene.trails.len());
		stats.assets_ms += assets_start.elapsed().as_secs_f64() * 1000.0;
		let bounds = geometry.bounds.or_else(|| effect_bounds(scene, &particles));
		stats.framed = bounds.map(|(min, max)| [min.into(), max.into()]);
		if bounds.is_none() {
			stats.warnings.push("nothing to frame: no part draws and no effect is in the scene (orbit views look at the origin)".to_string());
		}
		let mut lights: Vec<LightData> = scene.lights.iter().filter(|light| light.brightness > 0.0 && light.range > 0.0).map(light_data).collect();
		shared_graze_ranges(&mut lights);
		let direct = match &scene.environment.viewport {
			Some(viewport) => environment::viewport_light(viewport),
			None => environment::direct_light(scene.environment.clock_time, scene.environment.geographic_latitude, scene.environment.environment_diffuse_scale),
		};
		let highlights = if scene.highlights.is_empty() {
			None
		} else {
			highlights::PreparedHighlights::new(lease.device(), self.id_pipelines(), &scene.highlights, &geometry.part_draws, geometry.instance_count)
		};
		Ok(Prepared { geometry, casters: None, immutable: !scene.live, lights, environment, sun_shadows: scene.environment.global_shadows && scene.environment.sun_shadow_maps && direct.strength.max_element() > 0.0, bounds, sun_direction: direct.direction, sun_strength: direct.strength, beam_textures, trail_textures, highlights, particles })
	}

	/// The effects' textures and bind groups ([`EffectBindings`]): the last frames' when they drew the
	/// same textures in the same order, else loaded (on every core) and made now, and kept unless one
	/// failed or a local file moved between hashing and reading.
	fn effect_bindings(&self, scene: &Scene, missing: &mut Vec<String>, untrusted: &mut usize) -> Arc<EffectBindings> {
		let (hash, local_keys, untrusted_effects) = effect_hash(scene);
		*untrusted += untrusted_effects;
		if let Some(found) = self.effect_bindings.lock().iter().find(|effects| effects.hash == hash && effects.held.iter().all(|(texture, view)| texture.held().is_some_and(|held| held.base == 0 && held.view == *view))) {
			return found.clone();
		}
		let failed_before = missing.len();
		// the effects' textures loaded on every core first (as the parts' are, prepare_geometry), held
		// until their bind groups hold them; one after another, a recorded fight's 145 particle and beam
		// textures took 160 ms of its first frame. A failed one fails again below, where it is reported
		let loaded_by_uri: HashMap<String, Arc<GpuTexture>> = {
			let uris: std::collections::HashSet<String> = scene
				.particles
				.iter()
				.filter_map(|emitter| emitter.texture.as_deref())
				.chain(scene.beams.iter().filter_map(|beam| beam.texture.as_deref()))
				.chain(scene.trails.iter().filter_map(|trail| trail.texture.as_deref()))
				.map(transcode::image_key)
				.collect();
			// those already loaded found here, the rest loaded together (a clip's frames find them all)
			let mut found: Vec<(String, Arc<GpuTexture>)> = Vec::new();
			let wanted: Vec<&String> = uris
				.iter()
				.filter(|uri| {
					let lookup = local_keys.get(uri.as_str()).cloned().unwrap_or_else(|| content_key(uri));
					match self.textures.get(&(lookup, TextureUse::Color)).filter(|texture| texture.held().is_some_and(|held| held.base == 0)) {
						Some(texture) => {
							found.push(((*uri).clone(), texture));
							false
						}
						None => true,
					}
				})
				.collect();
			let load = |uri: &&String| self.load_whole(uri, TextureUse::Color).ok().map(|texture| ((*uri).clone(), texture));
			if scene.live || wanted.len() < 2 || !crate::par::spread(wanted.len(), 8) {
				found.extend(wanted.iter().filter_map(load));
			} else {
				found.par_extend(wanted.par_iter().filter_map(load));
				// the decodes' buffers freed on rayon's workers, given back (as prepare_geometry's): kept by
				// their heaps they held 240 MB through a GUI still's view
				crate::release_free_memory();
			}
			found.into_iter().collect()
		};
		let all_stable = loaded_by_uri.iter().all(|(uri, texture)| proof_matches(uri, &texture.proof, &local_keys));
		let beams = self.beam_textures_with(scene, &loaded_by_uri, missing);
		let trails = self.trail_textures_with(scene, &loaded_by_uri, missing);
		let particles = self.particle_groups_with(scene, &loaded_by_uri, missing);
		let held = loaded_by_uri.into_values().filter_map(|texture| texture.held().map(|held| (texture, held.view))).collect();
		let made = Arc::new(EffectBindings { hash, held, particles, beams, trails });
		if missing.len() == failed_before && all_stable {
			let mut kept = self.effect_bindings.lock();
			if kept.len() >= EFFECT_BINDINGS_KEPT {
				kept.remove(0);
			}
			kept.push(made.clone());
		}
		made
	}

	/// The parts' draws, instance buffer and light grid, whether the scene is moving (some of its parts
	/// were kept from an earlier scene, others changed), and whether every load that failed is a failure
	/// the resource caches remember (prepared::Retry). A part kept from an earlier scene under the same
	/// material configuration (`prepared::Preparations::parts`) is laid out again as it was prepared;
	/// the others are prepared now and kept when their assets loaded. A live scene's parts are not
	/// kept: its skinned meshes are written over in place.
	#[allow(clippy::too_many_arguments)]
	fn prepare_geometry(&self, scene: &Scene, stats: &mut RenderStats, neon_share: f32, key: Option<&prepared::SceneKey>, lease: &reuse::Lease, views: &[ViewRequest], defer_maps: bool, last: Option<prepared::LastFrame>) -> Result<(prepared::PreparedGeometry, bool, Option<prepared::FramePieces>, bool, bool, bool)> {
		let assets_start = Instant::now();
		let profile = std::env::var_os("ROR_PROFILE").is_some();
		let missing_before = stats.missing.len();
		let offline_errors_before = self.assets.is_offline_view().then(|| self.offline_errors());
		// the textures loaded whole below whose levels could not be read: not a failure the texture
		// cache remembers
		let mut remembered = true;
		// without the atlas (no Studio install) surface types draw nothing
		let mut surface_missing = None;
		let surface_maps = scene
			.parts
			.iter()
			.any(|part| surface_groups(part) != 0)
			.then(|| {
				let albedo = self.load_whole(SURFACE_ALBEDO, TextureUse::Data);
				let normal = self.load_whole(SURFACE_NORMAL, TextureUse::Data);
				remembered &= self.remembered(&albedo, SURFACE_ALBEDO) && self.remembered(&normal, SURFACE_NORMAL);
				albedo.and_then(|albedo| normal.map(|normal| (albedo, normal))).map_err(|error| surface_missing = Some(format!("surface atlas: {error:#}"))).ok()
			})
			.flatten();
		let highlighted: std::collections::HashSet<usize> = scene.highlights.iter().flat_map(|highlight| highlight.parts.iter().copied()).collect();
		let kept_glass: std::collections::HashSet<usize> = scene.kept_glass.iter().copied().collect();
		let flags: Vec<u8> = (0..scene.parts.len()).map(|position| u8::from(highlighted.contains(&position)) | (u8::from(kept_glass.contains(&position)) << 1)).collect();
		let context = PartContext { neon_share, viewport: scene.environment.viewport.is_some(), surface_maps: surface_maps.is_some(), play: scene.play };
		let table = key.map(|_| prepared::part_context(&scene.materials, neon_share, context.viewport, context.surface_maps, context.play));
		// the parts the last frame of a clip prepared the same (prepared::ClipGeometry), the others looked
		// for among the kept ones
		let last = last.filter(|_| prepared::clip_geometry() != prepared::ClipGeometryMode::Off);
		let reused: Vec<bool> = match (&last, key, &table) {
			(Some(last), Some(key), Some(table)) if last.key.parts.len() == key.parts.len() && last.context == *table => (0..scene.parts.len()).map(|position| last.same_part(position, key, &flags)).collect(),
			_ => vec![false; scene.parts.len()],
		};
		let mut kept: Vec<Option<Arc<prepared::PreparedPart>>> = match (key, &table) {
			(Some(key), Some(table)) => self.preparations.parts_where(table, key, &flags, |position| !reused[position]),
			_ => vec![None; scene.parts.len()],
		};
		if let Some(last) = &last {
			for (position, part) in kept.iter_mut().enumerate().filter(|(position, _)| reused[*position]) {
				*part = Some(last.geometry.parts[position].clone());
			}
		}
		let fresh: Vec<usize> = kept.iter().enumerate().filter(|(_, part)| part.is_none()).map(|(position, _)| position).collect();

		// Load every new part's mesh and textures in parallel (the caches de-duplicate concurrent loads).
		let loads_before = self.meshes.misses.load(std::sync::atomic::Ordering::Relaxed) + self.textures.misses.load(std::sync::atomic::Ordering::Relaxed);
		let phase = Instant::now();
		let meshes: Vec<Result<((Arc<GpuMesh>, Affine3A, bool), bool)>> = each_of(scene, &fresh, |index, part| self.part_mesh(part).map(|drawn| self.skinned_checked(part, drawn, scene.live.then_some(index))));
		if profile {
			eprintln!("profile: meshes {:.1} ms", phase.elapsed().as_secs_f64() * 1000.0);
		}
		self.memory_checkpoint("meshes");
		let phase = Instant::now();
		// (a part drawn without its appearance in a recording of play is settled: reported, but kept for
		// later frames as a part whose assets loaded, not prepared again each frame)
		let resolved: Vec<(PartMaterial, Vec<String>, Option<(Part, String)>)> = each_of(scene, &fresh, |_, part| {
			let mut missing = Vec::new();
			let material = self.part_material(part, &scene.materials, &mut missing);
			match self.playable(part, &material, scene.play) {
				Some((plain, reason)) => {
					let material = self.part_material(&plain, &scene.materials, &mut missing);
					(material, missing, Some((plain, reason)))
				}
				None => (material, missing, None),
			}
		});
		if profile {
			eprintln!("profile: materials {:.1} ms", phase.elapsed().as_secs_f64() * 1000.0);
		}
		self.memory_checkpoint("materials");
		// the decodes' buffers, freed on rayon's workers, stay with each worker's heap until it allocates
		// again: given back now (a map's textures left 400 MB so, through every later view); not when
		// nothing was loaded (a clip's moved parts prepared again, every frame, waited for a collect on
		// every pool thread)
		if !fresh.is_empty() && self.meshes.misses.load(std::sync::atomic::Ordering::Relaxed) + self.textures.misses.load(std::sync::atomic::Ordering::Relaxed) > loads_before {
			crate::release_free_memory();
		}
		let phase = Instant::now();
		let mut keys = Vec::with_capacity(resolved.len());
		let mut complete = Vec::with_capacity(resolved.len());
		let mut plain: Vec<Option<Part>> = Vec::with_capacity(resolved.len());
		for (material, missing, without) in resolved {
			complete.push(missing.is_empty());
			stats.missing.extend(missing);
			keys.push(material);
			plain.push(without.map(|(part, reason)| {
				stats.missing.push(reason);
				part
			}));
		}
		// what the preload loads is held until the parts' materials hold it: past the texture cache's
		// budget, a texture let go of before its parts were prepared was loaded again for them
		let preloaded: Mutex<Vec<Arc<GpuTexture>>> = Mutex::default();
		{
			let unique: Vec<&MaterialKey> = keys.iter().map(|material| &material.key).collect::<std::collections::HashSet<_>>().into_iter().collect();
			if profile {
				eprintln!("profile: {} unique material keys", unique.len());
			}
			let load = |key: &&MaterialKey| {
				for (uri, usage) in key.textures() {
					let start = Instant::now();
					let result = self.load_texture(uri, usage);
					let elapsed = start.elapsed().as_secs_f64() * 1000.0;
					if profile && elapsed > 20.0 {
						eprintln!("profile: slow texture {elapsed:.1} ms {uri} ok={}", result.is_ok());
					}
					if let Ok(texture) = result {
						preloaded.lock().push(texture);
					}
				}
			};
			// as each_part
			if scene.live || !crate::par::spread(unique.len(), 8) {
				unique.iter().for_each(load);
			} else {
				unique.par_iter().for_each(load);
			}
		}
		self.materials.lock().retain(|_, material| material.strong_count() > 0);
		if profile {
			eprintln!("profile: textures {:.1} ms", phase.elapsed().as_secs_f64() * 1000.0);
		}
		self.memory_checkpoint("textures");
		// without the noise texture (no Studio install) Organic materials tile regularly
		let organic = kept.iter().flatten().any(|part| part.organic) || keys.iter().any(|material| material.flags & TEX_ORGANIC != 0);
		let organic_noise = organic.then(|| self.load_whole(ORGANIC_NOISE, TextureUse::Data)).and_then(|noise| {
			remembered &= self.remembered(&noise, ORGANIC_NOISE);
			noise.map_err(|error| stats.missing.push(format!("Organic pattern noise: {error:#}"))).ok()
		});
		stats.missing.extend(surface_missing);
		stats.assets_ms = assets_start.elapsed().as_secs_f64() * 1000.0;

		let phase = Instant::now();
		let local_keys = key.map(|key| &key.local_keys);
		let inputs: Vec<(usize, Result<((Arc<GpuMesh>, Affine3A, bool), bool)>, PartMaterial, Option<Part>)> =
			fresh.iter().copied().zip(meshes).zip(keys).zip(plain).map(|(((position, mesh), material), plain)| (position, mesh, material, plain)).collect();
		let prepare = |(position, mesh, material, plain): (usize, Result<((Arc<GpuMesh>, Affine3A, bool), bool)>, PartMaterial, Option<Part>)| {
			let (part, stable) = self.prepare_part(plain.as_ref().unwrap_or(&scene.parts[position]), mesh, material, flags[position], &context, local_keys);
			(Arc::new(part), stable)
		};
		// as each_part
		let prepared_results: Vec<(Arc<prepared::PreparedPart>, bool)> = if scene.live || !crate::par::spread(inputs.len(), CLIP_PARTS) { inputs.into_iter().map(&prepare).collect() } else { inputs.into_par_iter().map(&prepare).collect() };
		let mut prepared: Vec<Arc<prepared::PreparedPart>> = Vec::with_capacity(prepared_results.len());
		let mut fresh_stable: Vec<bool> = Vec::with_capacity(prepared_results.len());
		for (part, stable) in prepared_results {
			stats.missing.extend(part.missing.iter().cloned());
			prepared.push(part);
			fresh_stable.push(stable);
		}
		let all_stable = fresh_stable.iter().all(|&stable| stable);
		// a scene the GPU cannot hold fails as a whole, rather than drawing what fit
		if let Some(full) = stats.missing.iter().find(|reason| reason.contains(GPU_FULL)) {
			return Err(anyhow!("{} parts: {full}", scene.parts.len()));
		}
		// a part whose assets all loaded and matched the key's stamps is kept for later scenes (a
		// missing or moving one may load cleanly next time); a skinned part's pose is cached by the
		// skins themselves. Skip publishing parts to the shared table when an offline view hit any
		// offline miss or silent fallback (e.g. pack layers, Head mesh).
		let offline_fallback = offline_errors_before.is_some_and(|before| self.offline_errors() > before);
		if !offline_fallback && let (Some(key), Some(table)) = (key, &table) {
			let keep = fresh
				.iter()
				.zip(&prepared)
				.zip(&complete)
				.zip(&fresh_stable)
				.filter(|(((position, part), complete), stable)| **stable && **complete && part.missing.is_empty() && scene.parts[**position].skeleton.is_none());
			self.preparations.keep_parts(table, keep.map(|(((position, part), _), _)| (&key.parts[*position][..], flags[*position], part.clone())));
		}
		let moving = !prepared.is_empty() && prepared.len() < scene.parts.len();
		// which parts a later frame may take as they are: what the kept table takes, and a skinned part too
		// (its key holds its pose, and its posed mesh is its own unless the scene is live): a map's 253 tufts
		// of grass, skinned and never moving, were prepared again every frame of a clip
		let mut reusable: Vec<bool> = kept.iter().map(Option::is_some).collect();
		for (((position, part), complete), stable) in fresh.iter().zip(&prepared).zip(&complete).zip(&fresh_stable) {
			reusable[*position] = !offline_fallback && *stable && *complete && part.missing.is_empty() && (scene.parts[*position].skeleton.is_none() || !scene.live);
		}

		// the frame's parts in order, each part's instances and draws after those before it: the last
		// frame's with the parts that changed written over, where those draw as much as they did there
		let mut prepared = prepared.into_iter();
		let parts: Vec<Arc<prepared::PreparedPart>> = kept.into_iter().map(|part| part.unwrap_or_else(|| prepared.next().expect("a part prepared for each one not kept"))).collect();
		let laid = match (last.as_ref().and_then(|last| last.patched(&parts, &reused)), prepared::clip_geometry()) {
			(Some(patched), prepared::ClipGeometryMode::Check) => {
				if let Some(difference) = prepared::Layout::of(&parts).difference(&patched) {
					panic!("a clip frame's layout patched from the last frame's differs from the one laid out anew: {difference}");
				}
				patched
			}
			(Some(patched), _) => patched,
			(None, _) => prepared::Layout::of(&parts),
		};
		drop(last);
		let prepared::Layout { mut instances, draws, part_draws, draw_parts, drawn, offsets } = laid;
		cluster_boxes(&mut instances);
		if profile {
			eprintln!("profile: parts {:.1} ms ({} of {} prepared)", phase.elapsed().as_secs_f64() * 1000.0, fresh.len(), scene.parts.len());
		}
		drop(preloaded);
		// (one zeroed for a buffer that holds none; not the layout's, which the next frame starts from)
		let none = instances.is_empty();
		if none {
			instances.push(InstanceData::zeroed());
		}
		// the material maps whole now, as the meshes went up, unless the views could sample much less
		// of them: then each view's feedback says what (render::residency); or a clip's residency
		// decides at the views (the estimate took 1.15 ms of a 5,053-part map's frame)
		let bounds = scene.framing_bounds_drawn(&drawn);
		if !defer_maps && !residency::worth_feedback(&draws, &instances, views, bounds, self.residency_from.load(std::sync::atomic::Ordering::Relaxed)) {
			let mut seen = std::collections::HashSet::new();
			let whole: Vec<(&Arc<GpuTexture>, u32)> = draws
				.iter()
				.flat_map(|draw| draw.material.maps.iter().flatten())
				.filter(|map| map.id != 0 && map.held().is_none_or(|held| held.base > 0) && seen.insert(map.id))
				.map(|map| (map, 0))
				.collect();
			self.staging.read(&self.gpu, &whole, &|bytes| self.gpu_room(bytes)).map_err(|error| anyhow!("{} parts: {error:#}", scene.parts.len()))?;
			self.memory_checkpoint("maps whole");
		}
		// A cached preparation takes these out of the lease before it returns. Uncached renders
		// still give their memory back to the pool, including live scenes and failed asset loads.
		let instance_buffer = lease.buffer("instances", wgpu::BufferUsages::STORAGE, bytemuck::cast_slice(&instances));
		let no_lights = lease.buffer("no lights", wgpu::BufferUsages::STORAGE, bytemuck::bytes_of(&LightData::zeroed()));
		// the light grid only exists with GlobalShadows (lg-terms: OutdoorAmbient lights a roofed
		// floor fully without them)
		self.memory_checkpoint("parts prepared");
		let light_grid_start = Instant::now();
		let grid = (scene.environment.global_shadows && light_grid_enabled()).then(|| self.light_grid(scene, lease, grid_sun_for(scene, views, bounds))).flatten();
		self.memory_checkpoint("light grid");
		stats.light_grid_ms = light_grid_start.elapsed().as_secs_f64() * 1000.0;

		let instance_count = instances.len();
		// what it owns, and what it holds that its caches own too (prepared::Preparations::insert)
		let bytes = grid.as_ref().map_or(0, |grid| grid.bytes) + instance_buffer.size() + no_lights.size() + std::mem::size_of_val(draws.as_slice()) as u64;
		// (counted for a scene kept whole: a moving one is not, prepare)
		let held = if moving { 0 } else { Self::held_by(&draws, organic_noise.as_ref(), surface_maps.as_ref()) };
		let prepared = prepared::PreparedGeometry {
			draws,
			instances: instance_buffer,
			no_lights,
			light_grid: grid.as_ref().map(|grid| (grid.view.clone(), grid.origin, grid.size, grid.sun)),
			light_grid_texture: grid.as_ref().map(|grid| grid.texture.clone()),
			bounds,
			organic_noise,
			surface_maps,
			part_draws,
			parts,
			draw_parts,
			instance_count,
			bytes,
			held,
			missing: stats.missing[missing_before..].to_vec(),
		};
		if none {
			instances.clear();
		}
		// (kept as the parts laid them, which a later frame's changed parts are: the boxes come anew each frame)
		for instance in &mut instances {
			(instance.cluster_low, instance.cluster_high) = ([0.0; 4], [0.0; 4]);
		}
		let frame = table.map(|context| prepared::FramePieces { context, flags, reusable, instances, drawn, offsets });
		Ok((prepared, moving, frame, remembered, offline_fallback, all_stable))
	}

	/// Monotonic count of offline-view misses and isolated fallbacks across the preparation caches.
	fn offline_errors(&self) -> u64 {
		self.meshes.offline_errors() + self.textures.offline_errors() + self.packs.offline_errors() + self.skins.sources.offline_errors()
	}

	/// Whether a texture [`Renderer::load_whole`] gave (`loaded`) loaded, or failed as the texture cache
	/// remembers (not its levels: the GPU's memory, its file), so a kept preparation may stand on the
	/// failure until it is retried (prepared::Retry).
	fn remembered<T>(&self, loaded: &Result<T>, uri: &str) -> bool {
		loaded.is_ok() || self.load_texture(uri, TextureUse::Data).is_err()
	}

	/// What the resource caches a scene's preparation loads through remember of failed loads now
	/// (prepared::Failures).
	fn failures(&self, offline_view: bool) -> prepared::Failures {
		prepared::Failures::default().and(self.meshes.failures_for(offline_view)).and(self.textures.failures_for(offline_view)).and(self.packs.failures_for(offline_view))
	}

	/// The failures those caches let go of before their time, so far ([`Renderer::failures`]).
	fn dropped_failures(&self, offline_view: bool) -> u64 {
		self.meshes.dropped_failures_for(offline_view) + self.textures.dropped_failures_for(offline_view) + self.packs.dropped_failures_for(offline_view)
	}

	/// A kept preparation whose light grid lacks the sun these views read ([`grid_sun_for`]: made for views that saw
	/// nothing past their sun shadow maps, or under another sun, a clip's ClockTime moving) draws the grid made for
	/// this sun (kept by its own key); the preparation stays as kept.
	fn under_this_sun(&self, scene: &Scene, geometry: Arc<prepared::PreparedGeometry>, views: &[ViewRequest], lease: &reuse::Lease) -> Arc<prepared::PreparedGeometry> {
		let Some((_, _, _, sun)) = &geometry.light_grid else {
			return geometry;
		};
		let wanted = grid_sun_for(scene, views, geometry.bounds);
		if wanted.is_none() || *sun == wanted {
			return geometry;
		}
		match self.light_grid(scene, lease, wanted) {
			Some(grid) => Arc::new(geometry.with_grid(&grid)),
			None => geometry,
		}
	}

	/// The light grid (`lightgrid::build`), kept by the parts that fill it: a clip whose moving parts
	/// do not fill it (meshes, invisible or shadowless parts) builds it once. A scene following one
	/// with as many fillers (a clip's next frame, its limbs moved) is updated from that one's grid
	/// where they differ (`lightgrid::build_from`, exactly the grid built anew); a scene's first grid
	/// keeps only what tells the next one so.
	fn light_grid(&self, scene: &Scene, lease: &reuse::Lease, sun: Option<Vec3>) -> Option<Arc<prepared::PreparedGrid>> {
		let key = lightgrid::key(scene, sun);
		if let Some(grid) = self.preparations.grid(&key) {
			return Some(grid);
		}
		let previous = self.preparations.take_grid_state(lightgrid::filler_count(scene));
		let moving = previous.is_some();
		let mut state = lightgrid::build_from(scene, previous, moving, sun)?;
		let dims = [state.dims[0] as u32, state.dims[1] as u32, state.dims[2] as u32];
		let size = Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32) * lightgrid::VOXEL;
		let mut texture = None;
		let view = light_grid_texture(&self.gpu, |desc| {
			let made = lease.texture(desc);
			texture = Some(made.clone());
			made
		}, dims, &state.values, state.sun.as_deref());
		let prepared = Arc::new(prepared::PreparedGrid { texture: texture?, view, origin: state.origin, size, sun: state.sun.as_ref().and(sun), bytes: state.values.len() as u64 * 2 });
		// a moving scene's grid is drawn once: its texture goes back to the pool with the lease
		if !moving && self.preparations.keep_grid(key, prepared.clone()) {
			lease.hand_out(&prepared.texture);
		}
		if !state.kept() {
			state.forget();
		}
		self.preparations.keep_grid_state(state);
		Some(prepared)
	}

	/// One part's draws and instances, indexed from its own first instance and draw
	/// ([`Renderer::prepare_geometry`] lays them out after the parts before it). Also returns
	/// whether every local resource loaded for the part matched `local_keys` (the stamps encoded
	/// into `SceneKey`).
	fn prepare_part(&self, part: &Part, mesh: Result<((Arc<GpuMesh>, Affine3A, bool), bool)>, material: PartMaterial, place: u8, context: &PartContext, local_keys: Option<&HashMap<String, String>>) -> (prepared::PreparedPart, bool) {
		let mut out = prepared::PreparedPart {
			instances: Vec::new(),
			draws: Vec::new(),
			main: None,
			drawn: mesh.as_ref().ok().map(|((mesh, local, _), _)| transformed_bounds(local, mesh.drawn)),
			organic: material.flags & TEX_ORGANIC != 0,
			missing: Vec::new(),
		};
		let mut stable = true;
		let (instances, draws, missing) = (&mut out.instances, &mut out.draws, &mut out.missing);
		let neon_share = context.neon_share;
		{
			let PartMaterial { mut key, mut flags, params, studs_per_tile, emissive: emission, offset, noise_offset, noise_frequency, normal_strength, normal_falloff, lobe, albedo, far, pack_proofs } = material;
			if let Some(local_keys) = local_keys {
				for (uri, proof) in &pack_proofs {
					stable &= proof_matches(uri, proof, local_keys);
				}
			}
			let mesh_uri: Option<&str> = match &part.geometry {
				Geometry::MeshPart { mesh, .. } => Some(mesh.as_str()),
				Geometry::Union { asset: Some(asset), mesh_data: None, .. } => Some(asset.as_str()),
				Geometry::Special { mesh_type: crate::scene::extract::mesh_type::FILE_MESH, mesh: Some(uri), .. } => Some(uri.as_str()),
				_ => None,
			};
			let local_mesh = mesh_uri.is_some_and(names_local_file);
			// a MeshPart without a mesh draws its box darker (MESHLESS_BOX_ALBEDO)
			let box_albedo = if part.class == "MeshPart" && matches!(part.geometry, Geometry::Block) { MESHLESS_BOX_ALBEDO } else { 1.0 };
			let (mesh, local, is_mesh) = match mesh {
				Ok((mesh, skin_stable)) => {
					stable &= skin_stable;
					if let (Some(local_keys), Some(uri)) = (local_keys, mesh_uri) {
						stable &= proof_matches(uri, &mesh.0.proof, local_keys);
					}
					mesh
				}
				// a MeshPart's, a FileMesh's or a union's mesh the client cannot load draws nothing, decals and
				// all (texture-missing "meshpart-no-mesh", "specialmesh-no-mesh"; a Roblox client 0.747: a
				// union whose asset fails, an R15 limb whose MeshId fails)
				Err(error) if matches!(&part.geometry, Geometry::Union { .. } | Geometry::Special { mesh_type: crate::scene::extract::mesh_type::FILE_MESH, mesh: Some(_), .. }) || (part.class == "MeshPart" && matches!(&part.geometry, Geometry::MeshPart { .. })) => {
					missing.push(format!("{}: {error:#} (not drawn)", part.path));
					return (out, stable && !local_mesh);
				}
				Err(error) => {
					missing.push(format!("{}: {error:#}", part.path));
					stable &= !local_mesh;
					// an R6 limb's mesh (Studio's own, or a CharacterMesh's) or a FileMesh's without a MeshId:
					// the part's box, so the shape is still there (texture-missing "specialmesh-empty-mesh"; a
					// Roblox client drew a limb whose CharacterMesh fails as its box, plain in one session and
					// darker in another)
					(self.primitives[&Primitive::Block].clone(), Affine3A::from_scale(part.size), false)
				}
			};
			// a SurfaceAppearance any of whose maps Studio cannot load hides its part, decals and all
			// (texture-missing "sa-*": a ColorMap, NormalMap, MetalnessMap, RoughnessMap or EmissiveMask
			// missing, refused (HTTP 403) or no image; one whose TexturePack fails draws as without maps)
			// (in a recording of play such a part comes here without its appearance: Renderer::playable)
			if part.surface_appearance.is_some()
				&& let Some((uri, error)) = key.textures().find_map(|(uri, usage)| self.load_texture(uri, usage).err().map(|error| (uri, error)))
			{
				missing.push(format!("{}: {error:#} (its SurfaceAppearance hides it)", part.path));
				return (out, stable && !names_local_file(uri));
			}
			// a mesh texture Studio cannot load draws black (texture-missing "specialmesh", "meshpart":
			// missing, refused or no image; filemesh-instance's under the default light, its sheen alone)
			let mut black = false;
			if let (Some(texture), Some(color)) = (&part.texture, &key.color)
				&& part.surface_appearance.is_none()
				&& *color == transcode::image_key(texture)
				&& let Err(error) = self.load_texture(color, TextureUse::Color)
			{
				missing.push(format!("{}: {error:#} (drawn black)", part.path));
				if names_local_file(color) {
					stable = false;
				}
				key.color = None;
				flags &= !(TEX_COLOR | TEX_ALPHA_OVERLAY);
				black = true;
			}
			let part_index = draws.len() as u32;
			// Studio's head mesh (not the cylinder drawn while it is unavailable)
			let is_head_mesh = matches!(part.geometry, Geometry::Special { mesh_type: crate::scene::extract::mesh_type::HEAD, .. }) && !Arc::ptr_eq(&mesh, &self.primitives[&Primitive::Cylinder]);
			// decals project onto the box of what is drawn (a SpecialMesh head's mesh, not the part's
			// 2-stud box), normalised to -0.5..0.5
			let (box_min, box_max) = transformed_bounds(&local, mesh.bounds);
			let box_size = (box_max - box_min).max(Vec3::splat(1e-4));
			let projection_box = part.cframe * Affine3A::from_translation((box_min + box_max) * 0.5) * Affine3A::from_scale(box_size);
			let part_inverse = Mat4::from(projection_box).inverse();
			let special_mesh = match &part.geometry {
				Geometry::Special { mesh_type, .. } => Some(*mesh_type),
				_ => None,
			};
			// a classic head's and a file mesh's decals wrap with the mesh's uv, whichever face they are on
			// (vfx-decals faces: Studio's head mesh, whose uv covers only its front, shows Top and Back
			// decals on its front; one cone mesh shows decals on all six faces alike round its
			// walls; decal-meshes: a character's face on a classic head, whose Back decal shows nothing from
			// behind); a file mesh that could not be loaded projects them from its box's faces
			let mesh_uv = is_head_mesh || (special_mesh == Some(crate::scene::extract::mesh_type::FILE_MESH) && is_mesh);
			// a Ball's and a Cylinder's decals map through the face of the shape they name alone, by its own
			// coordinates: the cube-sphere's face, the cylinder's side or cap (decal-shapes: a Front image
			// fills the ball's front cube face, 0.71 of its radius from the middle seen head-on, where ror's
			// projection of the box face reached the silhouette; a Top one lies turned half round from the
			// box's, as the material textures' faces are; a Texture tiles that face in its studs)
			let face_only = !mesh_uv && matches!(part.geometry, Geometry::Ball | Geometry::Cylinder);
			for (decal_index, decal) in part.decals.iter().enumerate().filter(|_| !part.shadow_only) {
				use crate::scene::extract::mesh_type;
				// without the head mesh (a cylinder) a classic head shows its Front decal only
				if !mesh_uv && matches!(special_mesh, Some(mesh_type::HEAD | mesh_type::FILE_MESH)) && decal.face != 5 {
					continue;
				}
				// a Decal's image does not repeat past its edges, a Texture's tiles (decal-shapes: blockUpperLeft
				// on a ball's face, whose far edges ror's repeating sampler drew red)
				let decal_key = MaterialKey { color: Some(transcode::image_key(&decal.texture)), clamped: decal.tiling.is_none(), ..MaterialKey::default() };
				// a decal whose image Studio cannot load draws nothing (texture-missing "decal-*", "texture")
				if let Some(Err(error)) = decal_key.color.as_deref().map(|uri| self.load_texture(uri, TextureUse::Color)) {
					missing.push(format!("{}: {error:#} (its decal is not drawn)", part.path));
					if decal_key.color.as_deref().is_some_and(names_local_file) {
						stable = false;
					}
					continue;
				}
				let decal_material = self.material_bind_group(&decal_key, missing);
				if let (Some(local_keys), Some(uri), Some(tex)) = (local_keys, decal_key.color.as_deref(), decal_material.maps[0].as_ref()) {
					stable &= proof_matches(uri, &tex.proof, local_keys);
				}
				let color = linear(decal.color);
				let mut decal_flags = TEX_COLOR | TEX_DECAL;
				let mut tiling = [0.0; 4];
				if mesh_uv {
					decal_flags |= TEX_DECAL_MESH_UV;
				}
				if face_only {
					decal_flags |= TEX_DECAL_MESH_UV | TEX_DECAL_FACE_ONLY;
				}
				// a MeshPart's decals draw on the faces whose normal points most nearly their way in the part
				// (decal-meshes: the MeshPart runs 99.24-99.99 → 99.79-100 % within 2; a face turned the
				// decal's way but more another way took it too), a posed skinned mesh's as its bind pose put
				// them (`vs_decal_bind`)
				if is_mesh && matches!(part.geometry, Geometry::MeshPart { .. }) {
					decal_flags |= TEX_DECAL_DOMINANT;
				}
				if mesh.bind.is_some() {
					decal_flags |= TEX_DECAL_BIND;
				}
				// a Texture tiles in studs from the face's centre on a MeshPart, not at all on a file mesh, and
				// on a classic head across the head's own studs through its uv: the uv times the part's
				// smaller horizontal extent and its height, as the head mesh is sized (decal-meshes measured
				// 1-stud-tall heads, where that is the uv itself; a model scaled down with ScaleTo shrinks
				// StudsPerTile and the head together and keeps its one face: a character at 0.43, whose
				// 0.43-stud tiles ror drew twice across his face)
				if let Some(t) = decal.tiling.filter(|_| special_mesh != Some(mesh_type::FILE_MESH)) {
					decal_flags |= TEX_DECAL_TILED;
					if matches!(part.geometry, Geometry::MeshPart { .. }) {
						decal_flags |= TEX_DECAL_CENTERED;
					}
					let head = if mesh_uv { glam::Vec2::new(part.size.x.min(part.size.z), part.size.y).max(glam::Vec2::splat(1e-4)) } else { glam::Vec2::ONE };
					tiling = [t.studs_per_tile_u / head.x, t.studs_per_tile_v / head.y, t.offset_studs_u / head.x, t.offset_studs_v / head.y];
				} else if decal.uv_offset != [0.0; 2] || decal.uv_scale != [1.0; 2] {
					// a Decal's UVOffset and UVScale: its image at uv * scale + offset (notes/render.md
					// "Decals": 4-stud blocks with one Front decal, the image at half size in the face's
					// top-left quarter at scale 2, moved left by a quarter of the face at offset 0.25)
					decal_flags |= TEX_DECAL_UV;
					tiling = [decal.uv_scale[0], decal.uv_scale[1], decal.uv_offset[0], decal.uv_offset[1]];
				}
				let instance_index = instances.len() as u32;
				let world = part.cframe * local;
				// below Transparency 0 the opacity keeps growing as 1 - Transparency times the texel's alpha,
				// at most 1 (`fs_decal`; vfx-decal-negative: an effect's ring, alpha at most 0.45, shows
				// opaque at -2 and -10; an opaque gray square reads the same at -10 as at 0 over black and
				// white, no extrapolation)
				instances.push(InstanceData {
					model: Mat4::from(world).to_cols_array_2d(),
					normal_matrix: Mat4::from_mat3(glam::Mat3::from(world.matrix3).inverse().transpose()).to_cols_array_2d(),
					color: [color.x, color.y, color.z, 1.0 - decal.transparency.min(1.0)],
					params: [0.45, 0.0, 0.0, 0.0],
					size: [box_size.x, box_size.y, box_size.z, 4.0],
					flags: [materials::SHADING_STANDARD, decal_flags, decal.face.min(5), 0],
					tiling,
					part_inverse: part_inverse.to_cols_array_2d(),
					specular: specular_params(&materials::params(272)),
					material: [0.0; 4],
					texture: [0.0; 4],
					tint: [1.0; 4],
					lobe: [0.0; 4],
					cluster_low: [0.0; 4],
					cluster_high: [0.0; 4],
				});
				let runs = self.decal_runs(&mesh, &local, box_size, decal.face.min(5), decal_flags);
				draws.push(DrawItem {
					mesh: mesh.clone(),
					indices: draw_lod(&mesh),
					runs,
					instance: instance_index,
					material: decal_material,
					// a see-through part's decals sort with it among the transparent things, drawn right
					// after it (particle-decal-order)
					transparent: part.transparency > 0.0,
					double_sided: false,
					casts_shadow: false,
					casts_light_shadow: false,
					shadow_cutout: false,
					// with a posed skinned part, where its Bones put it (as the part sorts, below)
					center: if mesh.bind.is_some() { world.transform_point3((mesh.drawn.0 + mesh.drawn.1) * 0.5) } else { Vec3::from(part.cframe.translation) },
					bound: (Vec3::from(part.cframe.translation), 0.0),
					drawn_bounds: culling::Bounds::new(mesh.drawn, world),
					decal: true,
					forcefield: false,
					write_depth: false,
					glass: false,
					decal_order: (part_index, decal.z_index, decal_index as u32),
					highlight_only: false,
					occludes: true,
				});
			}
			// an invisible part draws nothing, but a Highlight on it still draws (an invisible Glass part:
			// scene::extract keeps those only for their Highlights), and draws it as Glass (`fs_glass`)
			let highlight_only = part.transparency >= 1.0 && !part.shadow_only;
			let kept_glass = highlight_only && place & 2 != 0;
			if highlight_only && !kept_glass && place & 1 == 0 {
				return (out, stable);
			}
			// a shadow-only part (scene::SHADOW_ONLY) is here for the shadow maps alone
			if part.shadow_only && !part.cast_shadow {
				return (out, stable);
			}
			let material = self.material_bind_group(&key, missing);
			if let Some(local_keys) = local_keys {
				let uris = [&key.color, &key.normal, &key.metalness, &key.roughness, &key.emissive];
				for (uri, tex) in uris.iter().zip(&material.maps) {
					if let (Some(uri), Some(tex)) = (uri, tex) {
						stable &= proof_matches(uri, &tex.proof, local_keys);
					}
				}
			}
			let normal_dxt5nm = material.maps[1].as_ref().is_some_and(|texture| texture.dxt5nm);
			let normal_rg = material.maps[1].as_ref().is_some_and(|texture| texture.normal_rg);
			let world = part.cframe * local;
			let model = Mat4::from(world);
			let normal_matrix = Mat4::from_mat3(glam::Mat3::from(world.matrix3).inverse().transpose());
			let mut flags = flags;
			let surfaces = if context.surface_maps { surface_groups(part) } else { 0 };
			let uv_mode = uv_mode(flags, is_mesh, &part.geometry, surfaces);
			// the drawn shape's size (a ball's diameter, a cylinder's length and diameter); meshes
			// project onto the part's box
			let drawn_size = if is_mesh { part.size } else { Vec3::new(local.matrix3.x_axis.length(), local.matrix3.y_axis.length(), local.matrix3.z_axis.length()) };
			if normal_rg {
				flags |= TEX_NORMAL_RG;
			}
			if normal_dxt5nm {
				flags |= TEX_NORMAL_DXT5NM;
			}
			// ForceField scales its alpha by 1 - Transparency, negative Transparency included
			// (forcefield-blend, forcefield-angles); the shader caps it. Glass a Highlight keeps past
			// Transparency 1 bends what is behind it the other way (`fs_glass`)
			let forcefield = params.shading == materials::SHADING_FORCEFIELD;
			// Transparency up to 0.01 draws opaque, in the ambient occlusion too (ao-transparency: bars at
			// 0.005 and 0.01 show none of the black panel behind them, 182.3 of 255 as at 0, and occlude
			// and take the occlusion; from 0.015 they blend, 180.3, and do neither)
			let transparency = if part.transparency <= crate::scene::OPAQUE_TRANSPARENCY { 0.0 } else { part.transparency };
			// a MeshPart's or SpecialMesh's own texture (TextureID, TextureContent, TextureId: part_material), not
			// a SurfaceAppearance's map, a material's pack or a body part's composite
			let part_texture = part.surface_appearance.is_none() && flags & TEX_COLOR != 0 && flags & (TEX_MATERIAL | TEX_ALPHA_OVERLAY | TEX_ALPHA_TRANSPARENCY | TEX_ALPHA_TINTMASK) == 0;
			// its alpha is coverage past OPAQUE_TRANSPARENCY alone, there times 1 - Transparency (the shadows'
			// cut too: shadow_cutout); up to it a Roblox client draws the stored colour under any alpha, alpha 0
			// included, and Color nowhere (fixtures/texture-alpha.luau in a 0.747 client: (204, 153, 102) at
			// alpha 0, 64, 128 and 192 in image files, EditableImages and an uploaded face, MeshParts and
			// SpecialMeshes alike, the texel at Transparency 0 and 0.01, faded by the alpha from 0.02)
			if part_texture && transparency == 0.0 {
				flags |= TEX_ALPHA_IGNORED;
			}
			// a MeshPart's FileMesh vertex alpha: in a ViewportFrame the image's (render::guis windows); in
			// the world it scales a transparent part's opacity and does nothing to an opaque one
			// (vertex-alpha: a map's GlowCard, white with alpha fading out from its middle, is a soft
			// glow at Transparency 0.06 and 0.5 and a solid disc at 0, Neon and Plastic alike; an effect's
			// Destruction's opaque mask mesh in the Workspace draws opaque)
			if is_mesh && matches!(part.geometry, Geometry::MeshPart { .. }) && (context.viewport || transparency > 0.0) {
				flags |= TEX_VERTEX_ALPHA;
			}
			// a Glass MeshPart's in the world, at any Transparency, is the sky's share in its place, over whatever
			// lies behind it (glass-zero in a Roblox client as in Studio: a flat square and a rock whose every
			// vertex has alpha 0, as Glass at Transparency 0 and 0.5, show the sky in their place even before a
			// wall, a SmoothPlastic one at 0.5 nothing; the inside-out sphere, alpha 1, draws as any Glass)
			if is_mesh && matches!(part.geometry, Geometry::MeshPart { .. }) && !context.viewport && params.shading == materials::SHADING_GLASS {
				flags |= TEX_GLASS_VERTEX_SKY;
			}
			let opacity = if forcefield || kept_glass { 1.0 - part.transparency } else { 1.0 - transparency.clamp(0.0, 1.0) };
			let emissive = if params.shading == materials::SHADING_NEON { neon_emission(part.color) * neon_share } else { params.emissive };
			let colour = match (&part.surface_appearance, &part.geometry) {
				_ if black => Vec3::ZERO,
				(Some(appearance), _) if appearance.alpha_mode != 0 => linear(appearance.color),
				// a SpecialMesh texture replaces the part colour, tinted by VertexColor decoded like a
				// colour (squared; specialmesh-placement "vc-*"); without a texture VertexColor does nothing
				(None, Geometry::Special { .. }) if part.texture.is_some() => linear(part.vertex_color),
				// Plastic body parts, R6 and R15, heads too, draw their colour as a colour texture would,
				// 2.9 % darker (character-light: ror / Studio 0.996-1.002 with it, 1.023-1.032 without,
				// under flat Ambient, OutdoorAmbient and the sun alone; humanoid-parts: "Torso" flat-lit
				// 143.8 against a Plastic block's 146.0)
				_ if part.body_part.is_some() && part.material == 256 => linear(part.color) * COMPOSITED_ALBEDO,
				_ => linear(part.color) * albedo,
			} * box_albedo;
			// a MeshPart's own texture replaces its colour, its box's too (its vertex colours tint the texture:
			// mesh.wgsl)
			let base = if part_texture && part.class == "MeshPart" { Vec3::ONE } else { colour };
			// SurfaceAppearance.Color tints the colour map in AlphaMode Overlay too, squared like any
			// colour (a ship model's SA parts: Studio / ror radiance 0.66-0.68 at Color 0.82 (0.672), 0.49-0.53
			// at 0.71 (0.504))
			let tint = match &part.surface_appearance {
				Some(appearance) if appearance.alpha_mode == 0 => linear(appearance.color),
				_ => Vec3::ONE,
			};
			// a mesh texture turns Part.Reflectance's mirror off (reflectance-meshes: a SpecialMesh or
			// MeshPart with a TextureId at Reflectance 0.5 reads 0.98-1.03 of itself at 0, untextured
			// ones 0.63-0.94; the Linked Sword's 0.4 shows its texture, no sky)
			let reflectance = if part.texture.is_some() { 0.0 } else { part.reflectance };
			let index = instances.len() as u32;
			let specular = specular_params(&params);
			instances.push(InstanceData {
				model: model.to_cols_array_2d(),
				normal_matrix: normal_matrix.to_cols_array_2d(),
				color: [base.x, base.y, base.z, opacity],
				params: [tune_spec().filter(|_| params.metalness == 0.0 && flags & TEX_MATERIAL == 0 && params.shading == materials::SHADING_STANDARD).map(|(roughness, ..)| roughness).unwrap_or(params.roughness), params.metalness, reflectance, emissive],
				size: [drawn_size.x, drawn_size.y, drawn_size.z, studs_per_tile],
				flags: [params.shading, flags, surfaces, uv_mode],
				// parts: the material texture offset in tiles and the Organic noise's (decals use these for
				// their tiling)
				tiling: [offset[0], offset[1], noise_offset[0], noise_offset[1]],
				part_inverse: part_inverse.to_cols_array_2d(),
				specular: [specular[0], specular[1], specular[2], normal_strength],
				material: [emission.x, emission.y, emission.z, noise_frequency],
				// what of it shadows (`fs_shadow_cutout`): 1 - Transparency, a shadow-only part's whole
				texture: [far[0], far[1], if part.shadow_only { 1.0 } else { 1.0 - part.transparency }, normal_falloff.1],
				tint: [tint.x, tint.y, tint.z, normal_falloff.0],
				lobe: [lobe.1, lobe.0, 0.0, 0.0],
				cluster_low: [0.0; 4],
				cluster_high: [0.0; 4],
			});
			let lod = draw_lod(&mesh);
			// a posed skinned mesh sorts by where its Bones put it, not by its part (skin-sort: a character
			// moved 4 studs before a panel or SurfaceGui, its MeshPart 6 behind it, draws over it in
			// Studio, and under it the other way round; a cutscene dragon's crown, its part
			// before the portal, drew over the portal's window in ror)
			let center = if mesh.bind.is_some() {
				world.transform_point3((mesh.drawn.0 + mesh.drawn.1) * 0.5)
			} else {
				Vec3::from(part.cframe.translation)
			};
			let mesh_bounds = mesh.bounds;
			// a part's own texture's alpha counts only on a part transparent already (TEX_ALPHA_IGNORED); up to
			// OPAQUE_TRANSPARENCY it draws like any part, a SpecialMesh's triangles depth-tested among themselves
			// (the classic red cap's raised logo before the patch behind it, drawn later in its mesh)
			let texture_alpha = flags & TEX_ALPHA_TRANSPARENCY != 0;
			let transparent = forcefield || opacity < 1.0 || texture_alpha || part.shadow_only;
			// the sun's and local lights' shadows alike (scene::casts_shadow); a shadow-only part's at any
			// Transparency
			let casts = part.cast_shadow && (part.shadow_only || (crate::scene::casts_shadow(part.transparency) && !highlight_only));
			// a SurfaceAppearance in AlphaMode Transparency casts them where its colour map's alpha times
			// 1 - Transparency is over the cut in Transparency alone (fixtures/leaf-shadows.luau in a
			// Roblox 0.747 client: alpha 65 of 255 casts the full shadow and 64 none, 129 and 128 at
			// Transparency 0.5, 215 and 214 at 0.7; the sun's and shadowed PointLights' alike, texel by
			// texel; Overlay and TintMask by Transparency alone)
			// A part's own texture with alpha does the same past OPAQUE_TRANSPARENCY, and up to it casts the part's
			// shadow whole, a SpecialMesh's too (fixtures/texture-alpha.luau in a 0.747 client: MeshParts and
			// SpecialMeshes under the tan image at alpha 0 to 255 cast the full shadow at Transparency 0, at 0.3
			// alpha 0 and 64 none and 128 and 255 the full one, at 0.5 alpha 128 none and 192 the full one; a
			// checkerboard at 0.5 in squares from the sun and a shadowed PointLight, whole at 0)
			// (a shadow-only part's whole, as at Transparency 0)
			let texture_cutout = part_texture && transparency > 0.0 && !part.shadow_only && material.maps[0].as_ref().is_some_and(|texture| texture.has_alpha);
			// and a MeshPart's vertex alpha past it, times the texture's (TEX_VERTEX_ALPHA; the vertex alpha quad of
			// fixtures/texture-alpha.luau, cells at alpha 255, 128, 64 and 0: all four cast at Transparency 0, the
			// first two alone at 0.3)
			let vertex_cutout = flags & TEX_VERTEX_ALPHA != 0 && !context.viewport && !part.shadow_only && mesh.vertex_alpha;
			let shadow_cutout = casts && ((flags & TEX_MATERIAL == 0 && (flags & TEX_ALPHA_TRANSPARENCY != 0 || texture_cutout)) || vertex_cutout);
			out.main = Some((index, draws.len()));
			draws.push(DrawItem {
				drawn_bounds: culling::Bounds::new(mesh.drawn, world),
				mesh,
				indices: lod,
				runs: None,
				instance: index,
				material,
				transparent,
				// ForceField draws both faces (a quad shows from behind, a closed part two layers:
				// forcefield-geometry). A part casts a full shadow below the cut in its Transparency and
				// none from it up, whatever its Material (glass-shadows: Glass and SmoothPlastic blocks at
				// 0.7 to 0.745 shadow the floor as an opaque block does, 129 / 182 / 210 against 124 / 177 /
				// 206 on a floor lit at 221 / 231 / 238; at 0.75, 0.8 and up nothing; ForceField the same,
				// where ror cast its full shadow up to Transparency 1)
				double_sided: part.double_sided || forcefield,
				casts_shadow: casts,
				// local lights' shadows follow the sun's cut (light-transparency: SmoothPlastic at
				// Transparency 0.25 and 0.5, Glass at 0 and 0.5 and ForceField at 0 shadow the floor fully,
				// 0.75, 0.9, 0.98 and Glass at 0.9 not at all; ror cast them all, 9 of 255 too dark there)
				casts_light_shadow: casts,
				shadow_cutout,
				center,
				bound: {
					let (low, high) = mesh_bounds;
					let scale = world.matrix3.x_axis.length().max(world.matrix3.y_axis.length()).max(world.matrix3.z_axis.length());
					(world.transform_point3((low + high) * 0.5), (high - low).length() * 0.5 * scale)
				},
				decal: false,
				forcefield: forcefield && !part.shadow_only,
				// a SurfaceAppearance in AlphaMode Transparency on a part at Transparency 0 hides what is
				// behind its opaque texels, even what sorts after it: Studio shows none of the NPC aura's
				// particles inside its legs' clothes, but the sparks pulled out in front of them
				// (particle-model on a character; particle-occlusion still to be captured)
				write_depth: !forcefield && opacity >= 1.0 && flags & TEX_ALPHA_TRANSPARENCY != 0 && !part.shadow_only,
				glass: params.shading == materials::SHADING_GLASS && opacity < 1.0 && !part.shadow_only,
				decal_order: (0, 0, 0),
				highlight_only: highlight_only || part.shadow_only,
				occludes: true,
			});
			if !highlight_only && !part.shadow_only {
				self.push_joints(part, instances, draws, missing);
			}
		}
		(out, stable)
	}

	/// The geometry of a part's Hinge, Motor and SteppingMotor surfaces (notes/render.md "Surfaces"):
	/// on each such face of the part's box, at its centre, a yellow pin 0.4 studs across reaching
	/// 0.5 studs out of the face (and as far into it), and for a motor a dark disc 0.8 across standing
	/// 0.125 studs out; opaque and the same size on every part (a Roblox client 0.747, fixture
	/// geo1: blocks of 1 to 16 studs, a 0.2-stud plate the pin shows under, Balls, Cylinders and
	/// wedges at their box's faces, every material, a part at Transparency 0.5; nothing at 1).
	fn push_joints(&self, part: &Part, instances: &mut Vec<InstanceData>, draws: &mut Vec<DrawItem>, missing: &mut Vec<String>) {
		if part.transparency >= 1.0 || !matches!(part.geometry, Geometry::Block | Geometry::Ball | Geometry::Cylinder | Geometry::Wedge | Geometry::CornerWedge) {
			return;
		}
		if !part.surfaces.iter().any(|&surface| matches!(surface, SURFACE_HINGE | SURFACE_MOTOR | SURFACE_STEPPING_MOTOR)) {
			return;
		}
		let mesh = self.primitives[&Primitive::Cylinder].clone();
		let material = self.material_bind_group(&MaterialKey::default(), missing);
		let params = materials::params(272);
		for (face, surface) in part.surfaces.iter().enumerate() {
			let pieces: &[(f32, f32, Vec3)] = match *surface {
				SURFACE_HINGE => &[JOINT_PIN],
				SURFACE_MOTOR | SURFACE_STEPPING_MOTOR => &[JOINT_PIN, JOINT_DISC],
				_ => continue,
			};
			// NormalId order: Right, Top, Back, Left, Bottom, Front
			let normal = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z][face];
			let center = normal * (part.size * 0.5).dot(normal.abs());
			let turn = glam::Quat::from_rotation_arc(Vec3::X, normal);
			for &(length, diameter, color) in pieces {
				// the cylinder primitive lies along X, 1 stud long and across, centred on the face
				let world = part.cframe * Affine3A::from_scale_rotation_translation(Vec3::new(length, diameter, diameter), turn, center);
				let index = instances.len() as u32;
				instances.push(InstanceData {
					model: Mat4::from(world).to_cols_array_2d(),
					normal_matrix: Mat4::from_mat3(glam::Mat3::from(world.matrix3).inverse().transpose()).to_cols_array_2d(),
					color: [color.x, color.y, color.z, 1.0],
					params: [params.roughness, params.metalness, 0.0, 0.0],
					size: [length, diameter, diameter, 4.0],
					flags: [params.shading, 0, 0, UV_MESH],
					tiling: [0.0; 4],
					// its part's box, as a decal's: the part's shading tier and cluster (cluster_boxes)
					part_inverse: Mat4::from(part.cframe * Affine3A::from_scale(part.size)).inverse().to_cols_array_2d(),
					specular: specular_params(&params),
					material: [0.0; 4],
					texture: [0.0, 0.0, 1.0, 0.0],
					tint: [1.0; 4],
					lobe: [0.0; 4],
					cluster_low: [0.0; 4],
					cluster_high: [0.0; 4],
				});
				draws.push(DrawItem {
					drawn_bounds: culling::Bounds::new(mesh.drawn, world),
					mesh: mesh.clone(),
					indices: draw_lod(&mesh),
					runs: None,
					instance: index,
					material: material.clone(),
					transparent: false,
					double_sided: false,
					casts_shadow: part.cast_shadow,
					casts_light_shadow: part.cast_shadow,
					shadow_cutout: false,
					center: world.translation.into(),
					bound: (world.translation.into(), 0.5 * length.max(diameter) * std::f32::consts::SQRT_2),
					decal: false,
					forcefield: false,
					write_depth: false,
					glass: false,
					decal_order: (0, 0, 0),
					highlight_only: false,
					// the client's occlusion leaves joints out: the face round a pin stays as bright as the
					// rest of it, and the pin as its top (geo1 top and side views: 182 and 127 to the edge)
					occludes: false,
				});
			}
		}
	}

	/// What a preparation of these draws holds that its caches own too (prepared::PreparedGeometry::held).
	fn held_by(draws: &[DrawItem], organic_noise: Option<&Arc<GpuTexture>>, surface_maps: Option<&(Arc<GpuTexture>, Arc<GpuTexture>)>) -> u64 {
		Self::held_bytes(draws) + organic_noise.map_or(0, |texture| texture.bytes()) + surface_maps.map_or(0, |(albedo, normal)| albedo.bytes() + normal.bytes())
	}

	/// The meshes and the material textures `draws` hold, each once (the textures whole: what their
	/// views may come to read of them).
	fn held_bytes(draws: &[DrawItem]) -> u64 {
		let mut meshes = std::collections::HashSet::new();
		let mesh_bytes: u64 = draws.iter().filter(|draw| meshes.insert(Arc::as_ptr(&draw.mesh))).map(|draw| draw.mesh.bytes).sum();
		let mut textures = std::collections::HashSet::new();
		let texture_bytes: u64 = draws.iter().flat_map(|draw| draw.material.maps.iter().flatten()).filter(|texture| textures.insert(Arc::as_ptr(texture))).map(|texture| texture.full_bytes).sum();
		mesh_bytes + texture_bytes
	}

	/// Depth from shadowed local lights (each its origin and reach) through their six faces of `size`
	/// texels, one layer per face: the faces of parts with CastShadow turned towards the light, those
	/// below the shadow cut in Transparency (light-shadows, light-transparency), and of them only what reaches the
	/// light's sphere and the face's view.
	#[allow(clippy::too_many_arguments)]
	fn render_light_shadow_maps(&self, prepared: &Prepared, lights: &[ShadowLight], size: u32, groups: &residency::ViewGroups, lease: &reuse::Lease, profile: &profile::Profile, encoder: &mut wgpu::CommandEncoder, publish: &mut Vec<PendingShadows>) -> wgpu::TextureView {
		let key = shadow_reuse::local_key(lights, size);
		if prepared.immutable && let Some(maps) = self.shadows.get(prepared.casters(), &key, &self.reuse) {
			let view = maps.view.clone();
			lease.hold(maps);
			return view;
		}
		let keep = prepared.immutable && self.shadows.repeated(prepared.casters(), &key);
		let matrices: Vec<Mat4> = lights.iter().flat_map(light_shadow_matrices_at).collect();
		// room for a power of two of lights: a clip whose lights come and go draws into a few arrays
		// kept between its frames, not one per count (a barrage clip kept nine, 567 MB; the faces past
		// the lights drawn are never read)
		let most = self.gpu.device.limits().max_texture_array_layers / 6 * 6;
		let layers = (6 * lights.len().next_power_of_two() as u32).min(most);
		let desc = wgpu::TextureDescriptor {
			label: Some("light shadow maps"),
			size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: layers },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: SHADOW_FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		};
		// maps the cache keeps are its alone; the rest are this frame's scratch
		let texture = if keep { lease.kept(&desc) } else { lease.scratch(&desc) };
		let faces: Vec<(wgpu::TextureView, wgpu::BindGroup)> = matrices
			.iter()
			.enumerate()
			.map(|(layer, matrix)| {
				let view = texture.create_view(&wgpu::TextureViewDescriptor {
					label: Some("light shadow face"),
					dimension: Some(wgpu::TextureViewDimension::D2),
					base_array_layer: layer as u32,
					array_layer_count: Some(1),
					..Default::default()
				});
				let mut frame = self.frame_uniform(None, 1, 1, 0);
				frame.shadow_view_proj = matrix.to_cols_array_2d();
				// a SurfaceLight's six faces: the plane behind which nothing casts (mesh.wgsl fs_shadow_clip)
				if let Some(plane) = lights[layer / 6].clip_plane() {
					frame.camera_position = plane.into();
				}
				let frame_buffer = lease.buffer("light shadow frame", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&frame));
				(view, self.frame_bind_group(prepared, &frame_buffer, &self.no_light_bindings(prepared), &self.defaults.empty_moments, &self.defaults.empty_shadow, &self.defaults.white.view, &self.defaults.black.view))
			})
			.collect();
		// a face sees the directions within 59.3 degrees of its axis (the corners of its 100-degree
		// square); casters by twice their bounding sphere (a posed skinned mesh keeps its bind pose's)
		let face_axes = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z];
		let face_cos = ((LIGHT_SHADOW_FOV * 0.5).to_radians().tan() * std::f32::consts::SQRT_2).atan().cos();
		// into the frame's encoder (render_view): a submission per light took 0.5 ms of CPU each
		for ((shadow, light), face_matrices) in lights.iter().zip(faces.chunks(6)).zip(matrices.chunks(6)) {
			let (origin, reach, aimed) = (shadow.origin, shadow.reach, shadow.aim.w > 0.0);
			let casters: Vec<&DrawItem> = prepared
				.casters()
				.draws
				.iter()
				.filter(|draw| draw.casts_light_shadow && !draw.decal && draw.bound.0.distance(origin) <= reach + draw.bound.1 * 2.0)
				.collect();
			let cutouts = self.shadow_cutouts(groups, casters.iter().copied());
			for (face, ((view, bind_group), axis)) in light.iter().zip(face_axes).enumerate() {
				// an aimed light's one face (the shader reads no other)
				if aimed && face > 0 {
					continue;
				}
				let frustum = culling::Frustum::new(face_matrices[face]);
				let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
					label: Some("light shadow"),
					color_attachments: &[],
					depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
						view,
						depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
						stencil_ops: None,
					}),
					timestamp_writes: profile.pass("light shadow"),
					occlusion_query_set: None,
				});
				pass.set_bind_group(0, bind_group, &[]);
				// in the face's cone: the angle to the centre less the sphere's half angle inside the face's
				let in_face = |draw: &&&DrawItem| {
					let offset = draw.bound.0 - origin;
					let distance = offset.length();
					let radius = draw.bound.1 * 2.0;
					if distance <= radius || aimed {
						return true;
					}
					let cos_center = offset.dot(axis) / distance;
					let half = (radius / distance).asin();
					cos_center >= (face_cos.acos() + half).min(std::f32::consts::PI).cos()
				};
				let [single, double] = if shadow.clip_plane().is_some() { self.pipelines.light_shadow_clip(&self.gpu.device) } else { [&self.pipelines.light_shadow, &self.pipelines.shadow] };
				for (double_sided, pipeline) in [(false, single), (true, double)] {
					pass.set_pipeline(pipeline);
					let mut runs = runs::Runs::new(1);
					for draw in casters.iter().filter(|draw| !draw.shadow_cutout && draw.double_sided == double_sided).filter(|draw| draw.in_frustum(&frustum)).filter(in_face) {
						runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, None);
					}
					runs.finish(&mut pass);
				}
				if !cutouts.is_empty() {
					let pipelines = self.pipelines.shadow_cutout(&self.gpu.device);
					for (double_sided, pipeline) in [(false, &pipelines[2]), (true, &pipelines[3])] {
						pass.set_pipeline(pipeline);
						let mut runs = runs::Runs::new(1);
						for (draw, group) in cutouts.iter().filter(|(draw, _)| draw.double_sided == double_sided && draw.in_frustum(&frustum) && in_face(&draw)) {
							runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(group));
						}
						runs.finish(&mut pass);
					}
				}
			}
		}
		let view = texture.create_view(&wgpu::TextureViewDescriptor { label: Some("light shadow maps"), dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
		if keep {
			publish.push(PendingShadows { key, maps: shadow_reuse::Maps { view: view.clone(), columns: Vec::new(), bytes: size as u64 * size as u64 * layers as u64 * SHADOW_FORMAT.block_copy_size(Some(wgpu::TextureAspect::DepthOnly)).unwrap() as u64 }, texture });
		}
		view
	}

	/// The draws among `casters` whose colour map's alpha cuts their shadow, each with its material's
	/// bind group: the views' (render::residency holds their colour maps for the shadow maps too), else
	/// one made from what its maps hold.
	fn shadow_cutouts<'a>(&self, groups: &residency::ViewGroups, casters: impl Iterator<Item = &'a DrawItem>) -> Vec<(&'a DrawItem, wgpu::BindGroup)> {
		casters.filter(|draw| draw.shadow_cutout).map(|draw| (draw, groups.get(&draw.material).cloned().unwrap_or_else(|| self.material_group(&draw.material)))).collect()
	}

	/// A view's sun cascades: per cascade, depth from the sun, then its warped moments blurred along
	/// rows and along columns into the cascade's column of one atlas, all into the frame's encoder
	/// (render_view): a submission each took 0.5 ms of CPU, 2 of a map clip's 6 ms frames.
	/// Every cascade draws its depth and rows into the top-left of one pair of targets as big as the
	/// largest (a viewport of its side; both passes read them by texel, clamped to that side), so they
	/// take no more memory than when each submission gave them back to the next. Returns the atlas and
	/// each cascade's column.
	#[allow(clippy::too_many_arguments)]
	fn render_sun_shadows(&self, prepared: &Prepared, cascades: &[SunCascade], groups: &residency::ViewGroups, lease: &reuse::Lease, profile: &profile::Profile, encoder: &mut wgpu::CommandEncoder, publish: &mut Vec<PendingShadows>) -> (wgpu::TextureView, Vec<u32>) {
		let key = shadow_reuse::sun_key(cascades, prepared.sun_direction);
		if prepared.immutable && let Some(maps) = self.shadows.get(prepared.casters(), &key, &self.reuse) {
			let found = (maps.view.clone(), maps.columns.clone());
			lease.hold(maps);
			return found;
		}
		let keep = prepared.immutable && self.shadows.repeated(prepared.casters(), &key);
		let device = &self.gpu.device;
		let target = |lease: &reuse::Lease, label: &str, side: u32, format: wgpu::TextureFormat| {
			lease.scratch_view(&wgpu::TextureDescriptor {
				label: Some(label),
				size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
		};
		let width = cascades.iter().map(|cascade| cascade.texels).sum::<u32>().max(1);
		let height = cascades.iter().map(|cascade| cascade.texels).max().unwrap_or(1);
		let atlas_desc = wgpu::TextureDescriptor {
			label: Some("sun moments"),
			size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: SUN_MOMENTS_FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		};
		// an atlas the cache keeps is its alone; the rest are this frame's scratch
		let atlas_texture = if keep { lease.kept(&atlas_desc) } else { lease.scratch(&atlas_desc) };
		let atlas = atlas_texture.create_view(&Default::default());
		let mut columns = Vec::new();
		let mut column = 0;
		let depth = target(lease, "sun depth", height, SHADOW_FORMAT);
		let rows = target(lease, "sun moment rows", height, SUN_MOMENTS_FORMAT);
		for cascade in cascades {
			columns.push(column);
			let side = cascade.texels as f32;
			let mut frame = self.frame_uniform(None, 1, 1, 1);
			frame.shadow_view_proj = cascade.view_proj.to_cols_array_2d();
			let frame_buffer = lease.buffer("sun depth frame", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&frame));
			let frame_group = self.frame_bind_group(prepared, &frame_buffer, &self.no_light_bindings(prepared), &self.defaults.empty_moments, &self.defaults.empty_shadow, &self.defaults.white.view, &self.defaults.black.view);
			let blur = SunBlurUniform {
				params: [cascade.sigma, (3.0 * cascade.sigma).ceil(), SUN_WARP, 0.0],
				depth: [cascade.far, cascade.reach, cascade.radius, 0.0],
				region: [cascade.texels as f32, column as f32, 0.0, 0.0],
			};
			let blur_buffer = lease.buffer("sun blur", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&blur));
			let moments_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("sun moments"),
				layout: &self.pipelines.sun_moments_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: blur_buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&depth) },
				],
			});
			let blur_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("sun blur"),
				layout: &self.pipelines.sun_blur_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: blur_buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&rows) },
				],
			});
			let frustum = culling::Frustum::new(cascade.view_proj);
			{
				let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
					label: Some("sun depth"),
					color_attachments: &[],
					depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
						view: &depth,
						depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
						stencil_ops: None,
					}),
					timestamp_writes: profile.pass("sun depth"),
					occlusion_query_set: None,
				});
				pass.set_viewport(0.0, 0.0, side, side, 0.0, 1.0);
				pass.set_bind_group(0, &frame_group, &[]);
				// casters whose sphere reaches the cascade's square (SunCascade::reaches)
				let reaches = |draw: &&DrawItem| cascade.reaches(draw, prepared.sun_direction);
				for (double_sided, pipeline) in [(false, &self.pipelines.sun_depth_front), (true, &self.pipelines.sun_depth)] {
					pass.set_pipeline(pipeline);
					let mut runs = runs::Runs::new(1);
					for draw in prepared.casters().draws.iter().filter(|draw| draw.casts_shadow && !draw.shadow_cutout && !draw.decal && draw.double_sided == double_sided).filter(reaches).filter(|draw| draw.in_frustum(&frustum)) {
						runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, None);
					}
					runs.finish(&mut pass);
				}
				// those whose colour map's alpha cuts their shadow, through it (`fs_shadow_cutout`)
				let cutouts = self.shadow_cutouts(groups, prepared.casters().draws.iter().filter(|draw| draw.casts_shadow && !draw.decal).filter(reaches).filter(|draw| draw.in_frustum(&frustum)));
				if !cutouts.is_empty() {
					let pipelines = self.pipelines.shadow_cutout(&self.gpu.device);
					for (double_sided, pipeline) in [(false, &pipelines[0]), (true, &pipelines[1])] {
						pass.set_pipeline(pipeline);
						let mut runs = runs::Runs::new(1);
						for (draw, group) in cutouts.iter().filter(|(draw, _)| draw.double_sided == double_sided) {
							runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(group));
						}
						runs.finish(&mut pass);
					}
				}
			}
			let color_pass = |encoder: &mut wgpu::CommandEncoder, label: &str, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>| {
				encoder
					.begin_render_pass(&wgpu::RenderPassDescriptor {
						label: Some(label),
						color_attachments: &[Some(wgpu::RenderPassColorAttachment { view, resolve_target: None, depth_slice: None, ops: wgpu::Operations { load, store: wgpu::StoreOp::Store } })],
						depth_stencil_attachment: None,
						timestamp_writes: profile.pass(label),
						occlusion_query_set: None,
					})
					.forget_lifetime()
			};
			{
				let mut pass = color_pass(encoder, "sun moment rows", &rows, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
				pass.set_viewport(0.0, 0.0, side, side, 0.0, 1.0);
				pass.set_pipeline(&self.pipelines.sun_moments);
				pass.set_bind_group(0, &moments_group, &[]);
				pass.draw(0..3, 0..1);
			}
			{
				// into the cascade's column, keeping the others' (the first clears what an earlier render
				// left in the atlas)
				let load = if column == 0 { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load };
				let mut pass = color_pass(encoder, "sun moment columns", &atlas, load);
				pass.set_viewport(column as f32, 0.0, cascade.texels as f32, cascade.texels as f32, 0.0, 1.0);
				pass.set_pipeline(&self.pipelines.sun_blur);
				pass.set_bind_group(0, &blur_group, &[]);
				pass.draw(0..3, 0..1);
			}
			column += cascade.texels;
		}
		if keep {
			publish.push(PendingShadows { key, maps: shadow_reuse::Maps { view: atlas.clone(), columns: columns.clone(), bytes: width as u64 * height as u64 * SUN_MOMENTS_FORMAT.block_copy_size(None).unwrap() as u64 }, texture: atlas_texture });
		}
		(atlas, columns)
	}

	fn frame_uniform(&self, camera: Option<&Camera>, width: u32, height: u32, light_count: u32) -> FrameUniform {
		FrameUniform::zeroed().with(|frame| {
			if let Some(camera) = camera {
				let view_proj = camera.view_projection(width as f32 / height as f32);
				frame.view_proj = view_proj.to_cols_array_2d();
				frame.inv_view_proj = view_proj.inverse().to_cols_array_2d();
				frame.camera_position = Vec4::from((camera.eye, camera.near)).into();
			}
			frame.counts = [light_count, 0, 0, 0];
			frame.viewport = [width as f32, height as f32, 1.0 / width as f32, 1.0 / height as f32];
			frame.direct_specular = direct_specular();
			frame.direct_reflectance = direct_reflectance();
			frame.sky_specular_colour = sky_specular_colour();
			frame.direct_metal = direct_metal();
			frame.specular_fade = specular_fade();
			// level 21: the high-quality shading everywhere, and the sun as round the focus (faces turned from it unlit)
			frame.quality_shading = quality_shading(&crate::quality::Quality::top());
			frame.quality_focus = [0.0, 0.0, 0.0, 1e9];
		})
	}

	/// Local light bindings for passes that shade nothing.
	fn no_light_bindings<'a>(&'a self, prepared: &'a Prepared) -> LightBindings<'a> {
		LightBindings { lights: &prepared.no_lights, shadow_maps: &self.defaults.empty_light_shadows, shadow_matrices: &self.defaults.no_light_matrices, voxel_field: &self.defaults.no_voxel_field, clusters: &self.defaults.no_light_clusters }
	}

	fn frame_bind_group(&self, prepared: &Prepared, frame_buffer: &wgpu::Buffer, lights: &LightBindings, shadow: &wgpu::TextureView, scene_depth: &wgpu::TextureView, occlusion: &wgpu::TextureView, scene_color: &wgpu::TextureView) -> wgpu::BindGroup {
		self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("frame"),
			layout: &self.pipelines.frame_layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: frame_buffer.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 1, resource: prepared.instances.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(shadow) },
				wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.defaults.shadow_sampler) },
				wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&prepared.environment.specular) },
				wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.defaults.linear_sampler) },
				wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&self.defaults.brdf_lut) },
				wgpu::BindGroupEntry { binding: 7, resource: lights.lights.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(scene_depth) },
				// black: level 0 everywhere, the plain tiling (shifted by a part's texture offset)
				wgpu::BindGroupEntry {
					binding: 9,
					resource: wgpu::BindingResource::TextureView(&prepared.organic_noise.as_ref().map_or_else(|| self.defaults.black.view.clone(), |noise| noise.view())),
				},
				wgpu::BindGroupEntry {
					binding: 10,
					resource: wgpu::BindingResource::TextureView(&prepared.surface_maps.as_ref().map_or_else(|| self.defaults.black.view.clone(), |(albedo, _)| albedo.view())),
				},
				wgpu::BindGroupEntry {
					binding: 11,
					resource: wgpu::BindingResource::TextureView(&prepared.surface_maps.as_ref().map_or_else(|| self.defaults.black.view.clone(), |(_, normal)| normal.view())),
				},
				wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(&self.defaults.wang_index.view) },
				wgpu::BindGroupEntry { binding: 13, resource: wgpu::BindingResource::TextureView(lights.shadow_maps) },
				wgpu::BindGroupEntry { binding: 14, resource: lights.shadow_matrices.as_entire_binding() },
				wgpu::BindGroupEntry {
					binding: 15,
					resource: wgpu::BindingResource::TextureView(prepared.light_grid.as_ref().map(|(view, _, _, _)| view).unwrap_or(&self.defaults.no_light_grid)),
				},
				wgpu::BindGroupEntry { binding: 16, resource: wgpu::BindingResource::TextureView(occlusion) },
				wgpu::BindGroupEntry { binding: 17, resource: wgpu::BindingResource::TextureView(scene_color) },
				wgpu::BindGroupEntry { binding: 19, resource: wgpu::BindingResource::TextureView(&prepared.environment.faded) },
				wgpu::BindGroupEntry { binding: 18, resource: wgpu::BindingResource::TextureView(lights.voxel_field) },
				wgpu::BindGroupEntry { binding: 20, resource: lights.clusters.as_entire_binding() },
			],
		})
	}

	/// Draws a decal over its part (the frame bind group and `self.pipelines.decal(samples, bind)` for
	/// its mesh are set); on a posed skinned mesh projected from its bind pose (`vs_decal_bind`).
	fn draw_decal(&self, pass: &mut wgpu::RenderPass<'_>, groups: &residency::ViewGroups, draw: &DrawItem) {
		pass.set_bind_group(1, groups.of(&draw.material), &[]);
		pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
		if let Some(bind) = &draw.mesh.bind {
			pass.set_vertex_buffer(1, bind.slice(..));
		}
		pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
		draw.draw_indexed(pass);
	}

	/// The triangles a decal on `face` of a part can draw on ([`resources::DecalFaces`]), where its
	/// mesh says and the part's transform keeps that exact: `None` (every triangle) for a decal
	/// projected from a posed skinned mesh's bind pose, wrapped by the mesh's uv, or drawn at another
	/// level (`ROR_TUNE_LOD`); for one through the part's box only where the mesh's transform `local`
	/// is a positive scale (the part's space then turns each normal's components by a positive factor
	/// each) and the box is no more than 30 times longer one way than another (in f32 the part's
	/// inverse box, its error growing as the square of that, then leaves a face with no vertex normal
	/// towards it under the shader's 1e-3 by more than tenfold).
	fn decal_runs(&self, mesh: &GpuMesh, local: &Affine3A, box_size: Vec3, face: u32, flags: u32) -> Option<Arc<[Range<u32>]>> {
		let faces = mesh.decal_faces.as_ref()?;
		if mesh.bind.is_some() || mesh.lods.first() != Some(&draw_lod(mesh)) {
			return None;
		}
		if flags & TEX_DECAL_FACE_ONLY != 0 {
			return Some(faces.code[face as usize].clone());
		}
		if flags & TEX_DECAL_MESH_UV != 0 {
			return None;
		}
		let m = local.matrix3;
		let diagonal = Vec3::new(m.x_axis.x, m.y_axis.y, m.z_axis.z);
		let off = [m.x_axis.y, m.x_axis.z, m.y_axis.x, m.y_axis.z, m.z_axis.x, m.z_axis.y];
		let scaled = off.iter().all(|&value| value == 0.0) && diagonal.min_element() > 0.0;
		(scaled && box_size.min_element() > 0.0 && box_size.max_element() <= 30.0 * box_size.min_element()).then(|| faces.normal[face as usize].clone())
	}

	#[allow(clippy::too_many_arguments)]
	fn render_view(&self, scene: &Scene, prepared: &Prepared, request: &ViewRequest, view_guis: &guis::ViewGuis, groups: &residency::ViewGroups, stats: &mut RenderStats, output: (&reuse::Lease, Option<VideoReadback>, Option<ao::HistoryUse<'_>>)) -> Result<PendingView<'_>> {
		let (lease, video, ao_history) = output;
		let draw_start = Instant::now();
		let capture = renderdoc::view();
		let profile = profile::Profile::new(&self.gpu);
		let device = &self.gpu.device;
		// the frame's one submission: its shadows, the occlusion, the scene and its effects
		let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("view") });
		// shadow maps to keep, kept once the frame is submitted (shadow_reuse: published read-only)
		let mut publish: Vec<PendingShadows> = Vec::new();
		let supersample = request.supersample.clamp(1, 4);
		let samples = view_samples(request);
		let (width, height) = (request.width.max(1), request.height.max(1));
		let (render_width, render_height) = (width * supersample, height * supersample);
		let limit = device.limits().max_texture_dimension_2d;
		if render_width > limit || render_height > limit {
			return Err(anyhow!("{render_width}x{render_height} exceeds the GPU's texture limit {limit}"));
		}
		let camera = request.camera.place(prepared.bounds, width, height);
		let frustum = culling::Frustum::new(camera.view_projection(width as f32 / height as f32));
		let visible_draws: Vec<&DrawItem> = prepared.draws.iter().filter(|draw| draw.in_frustum(&frustum)).collect();
		let environment = &scene.environment;
		// what the view's graphics quality level draws (crate::quality; level 21 without one)
		let quality = view_quality(request, environment);
		// the lights this view reaches, nearest first; the nearest MAX_SHADOW_LIGHTS with Shadows within the
		// level's per-pixel reach get shadow maps, of a size for how many there are (none where every surface
		// takes the lights' voxels: lights_from_voxels)
		let soft = lights_from_voxels(environment, &quality);
		let (far_start, far_end) = local_light_far(&quality);
		let (places, mut lights): (Vec<usize>, Vec<LightData>) = view_lights(&prepared.lights, camera.view_projection(width as f32 / height as f32), camera.eye).into_iter().unzip();
		let casts = |light: &LightData| {
			let position = Vec3::new(light.position[0], light.position[1], light.position[2]);
			!soft && light.shadow[1] > 0.0 && position.distance(camera.eye) - light.color[3] - light.shadow[2] - light.right[3].max(light.up[3]) < far_end
		};
		// six array layers a light: as many as the device holds (any GPU holds 42)
		let most = MAX_SHADOW_LIGHTS.min(self.gpu.device.limits().max_texture_array_layers as usize / 6);
		let mut shadowed: Vec<usize> = (0..lights.len()).filter(|&index| casts(&lights[index])).take(most).collect();
		let shadow_size = light_shadow_size(shadowed.len());
		// their maps' layers in the scene's order, not the camera's: a camera that moves draws with the
		// maps kept for the last frame while the same lights cast (shadow_reuse::local_key); in its
		// order, an orbit around a lit place drew every light's six faces again every frame
		shadowed.sort_by_key(|&index| places[index]);
		let mut shadow_lights: Vec<ShadowLight> = Vec::with_capacity(shadowed.len());
		for index in shadowed {
			let light = &mut lights[index];
			light.shadow[0] = (shadow_lights.len() * 6) as f32;
			light.shadow[3] = shadow_size as f32;
			// from behind a SurfaceLight's face (light_data)
			shadow_lights.push(shadow_light(light));
		}
		let shadow_matrices: Vec<Mat4> = shadow_lights.iter().flat_map(light_shadow_matrices_at).collect();
		let lights_buffer = (!lights.is_empty()).then(|| lease.buffer("lights", wgpu::BufferUsages::STORAGE, bytemuck::cast_slice(&lights)));
		let shadow_start = Instant::now();
		let shadow_maps = (!shadow_lights.is_empty()).then(|| self.render_light_shadow_maps(prepared, &shadow_lights, shadow_size, groups, lease, &profile, &mut encoder, &mut publish));
		stats.light_shadow_ms += shadow_start.elapsed().as_secs_f64() * 1000.0;
		let shadow_matrices_buffer = (!shadow_matrices.is_empty()).then(|| {
			let columns: Vec<[[f32; 4]; 4]> = shadow_matrices.iter().map(Mat4::to_cols_array_2d).collect();
			lease.buffer("light shadow matrices", wgpu::BufferUsages::STORAGE, bytemuck::cast_slice(&columns))
		});

		let mut frame = self.frame_uniform(Some(&camera), render_width, render_height, lights.len() as u32);
		// bit 1: every surface from the voxels; bit 2: LightingStyle Soft's ambient occlusion; bit 3: Glass
		// moves nothing behind it, below quality level 16 (mesh.wgsl)
		frame.counts[3] = u32::from(soft) | u32::from(environment.soft_lighting()) << 1 | u32::from(!quality.glass_refraction) << 2;
		if (far_start, far_end) != (LOCAL_LIGHT_FAR_START, LOCAL_LIGHT_FAR_END) {
			frame.local_light_far = [far_start, far_end, 0.0, 0.0];
		}
		// the level's shading: high quality within its radius of the camera, per pixel within its radius of the
		// focus, the lowest shading past that (mesh.wgsl `shading_tier`)
		frame.quality_shading = quality_shading(&quality);
		// and where faces turned from the sun take some of it: past the level's radius of the focus, by cluster
		frame.quality_focus = Vec4::from((view_focus(&request.camera, &camera, prepared.bounds), quality.back_light_radius.min(1e9))).into();
		// which of them each cluster of the view's pixels can be lit by: the light within its Range (past
		// a SurfaceLight's face), the far surfaces' voxels 6 studs out within their reach (under Soft the
		// near surfaces' too, a light placed with a playback's as well, and past a lower level's per-pixel
		// reach, where it lights up to 580-600 studs)
		let placed_far = frame.local_light_far[1] > 0.0;
		let forward = -Vec3::from(camera.world.matrix3.z_axis).normalize();
		let reaches: Vec<light_clusters::Reach> = lights
			.iter()
			.map(|light| {
				let face = glam::Vec2::new(light.right[3], light.up[3]).length();
				let voxels = light.voxel_color[3] + 6.0 + face;
				light_clusters::Reach { centre: Vec3::new(light.position[0], light.position[1], light.position[2]), near: if soft { voxels } else { light.color[3] + face }, far: (light.extra[0] > 0.0 || placed_far).then_some(voxels) }
			})
			.collect();
		let clusters = light_clusters::build(&reaches, camera.view(), camera.projection(width as f32 / height as f32), camera.eye, forward, camera.fov.to_radians(), render_width, render_height, far_end, far_start);
		let clusters_buffer = clusters.as_ref().map(|clusters| {
			frame.light_clusters = clusters.grid;
			frame.light_cluster_view = clusters.view;
			lease.buffer("light clusters", wgpu::BufferUsages::STORAGE, bytemuck::cast_slice(&clusters.masks))
		});
		let sun = prepared.sun_direction;
		// the sun's cascades for this camera at the view's graphics quality (notes/render.md "Shadows",
		// "Graphics quality levels")
		let sun_cascades = view_sun_cascades(&camera, width as f32 / height as f32, prepared, environment, &quality);
		let shadow_start = Instant::now();
		let sun_atlas = (!sun_cascades.is_empty()).then(|| self.render_sun_shadows(prepared, &sun_cascades, groups, lease, &profile, &mut encoder, &mut publish));
		stats.sun_shadow_ms += shadow_start.elapsed().as_secs_f64() * 1000.0;
		let shadows_on = sun_atlas.is_some();
		frame.sun_direction = [sun.x, sun.y, sun.z, if shadows_on { 1.0 } else { 0.0 }];
		// A white surface facing the sun reaches Brightness (in the tone curve's input units): the
		// Lambert term albedo / π · E needs E = π · Brightness (notes/render.md "Tone curve"), times
		// its strength (environment::direct_light), tinted by ColorShift_Top (environment::sun_tint), the
		// moon too (light-night-sky shift-top)
		let sun_strength = environment::sun_tint(prepared.sun_strength, environment.color_shift_top, environment.environment_diffuse_scale);
		let sun_radiance = sun_strength * (environment.brightness * std::f32::consts::PI);
		frame.sun_color = Vec4::from((sun_radiance, 0.0)).into();
		// Terrain Clouds dim it by their cover towards it from the camera (notes/render.md "Clouds")
		if environment.viewport.is_none() {
			frame.cloud_sun = [clouds::sun_cover(&self.assets, camera.eye, sun, environment.clouds.as_ref()), camera.eye.x, camera.eye.z, 0.0];
		}
		// Flat ambient: 1.98 · colour², Ambient and OutdoorAmbient combined by a per-channel maximum in
		// the open (notes/render.md "Ambient"); a ViewportFrame's own Ambient instead
		let ambient = environment.viewport.as_ref().map_or(linear(environment.ambient) * 1.98, environment::viewport_ambient);
		frame.ambient = Vec4::from((ambient, 0.0)).into();
		frame.outdoor_ambient = Vec4::from((linear(environment.outdoor_ambient) * 1.98, 0.0)).into();
		for (index, coefficient) in prepared.environment.sh.iter().enumerate() {
			frame.sky_sh[index] = [coefficient.x, coefficient.y, coefficient.z, 0.0];
		}
		// the sky light: Studio lights with the sky 4.4 % brighter than it shows it (sky-kernel: an
		// all-white sky gives 2.024 · albedo against the sky's 1.935)
		for (index, light) in prepared.environment.cube.iter().enumerate() {
			let light = *light * SKY_LIGHT_SCALE;
			frame.sky_cube[index] = [light.x, light.y, light.z, 0.0];
		}
		frame.environment = [
			environment.environment_diffuse_scale,
			environment.environment_specular_scale,
			2f32.powf(environment.exposure_compensation),
			(prepared.environment.specular_levels - 1) as f32,
		];
		// with GlobalShadows on, open OutdoorAmbient reads 0.986 of what it does with them off
		// (lg-terms: 0.953 / 0.967 open, roofed and not)
		if let Some((_, origin, size, sun)) = &prepared.light_grid {
			// (2: the grid carries the sun's light along its direction, which stops the sun past the view's
			// shadow maps: lightgrid::grid_sun)
			frame.light_grid = [origin.x, origin.y, origin.z, if sun.is_some() && *sun == lightgrid::grid_sun(scene) { 2.0 } else { 1.0 }];
			frame.light_grid_scale = [1.0 / size.x, 1.0 / size.y, 1.0 / size.z, LIGHT_GRID_OPEN];
		} else {
			frame.light_grid_scale = [0.0, 0.0, 0.0, if environment.global_shadows { LIGHT_GRID_OPEN } else { 1.0 }];
		}
		let reach = sun_reach(&quality, environment).unwrap_or(crate::quality::SunReach { full: 0.0, gone: 0.0 });
		if let Some((_, columns)) = &sun_atlas {
			for (cascade, column) in sun_cascades.iter().zip(columns) {
				frame.shadow_cascades[cascade.index] = cascade.view_proj.to_cols_array_2d();
				frame.shadow_spheres[cascade.index] = [cascade.center.x, cascade.center.y, cascade.center.z, cascade.radius];
				frame.shadow_regions[cascade.index] = [*column as f32, cascade.texels as f32, reach.gone, cascade.least_variance()];
			}
		}
		// the shadow maps reach to `gone` from the camera, fading into the light grid's voxel sun from `full`
		// (mesh.wgsl sun_shade; both 0 at a level without the maps: the voxels everywhere)
		frame.shadow = [reach.full, SUN_WARP, reach.gone, SUN_DARKEN];
		// Lighting's fog, also at the default FogEnd of 100000 (atmosphere-explore: black tiles 1024-4096
		// studs away read 6.6 / 13 / 25 of 255 without an Atmosphere); its colour shows like a sky texel,
		// 1.935 · FogColor² (fog). The sky is not fogged.
		let fog = linear(environment.fog_color) * SKY_RADIANCE;
		frame.fog_color = [fog.x, fog.y, fog.z, 0.0];
		frame.fog = [environment.fog_start, environment.fog_end.min(FOG_END_LIMIT), if environment.viewport.is_some() { 0.0 } else { 1.0 }, 0.0];
		// an Atmosphere with Density replaces the fog; one of Density 0 leaves it (fog: FogEnd 100 with an
		// Atmosphere of Density 0 fogs as without one, with Density 0.5 not at all)
		if let Some(atmosphere) = environment.atmosphere.filter(|atmosphere| atmosphere.density > 0.0) {
			frame.fog[2] = 0.0;
			let (power, top, offset_power) = atmosphere_levels();
			let top = top.min(frame.environment[3]);
			frame.atmosphere = [ATMOSPHERE_EXTINCTION * atmosphere.density.max(0.0).powi(4), top, top * atmosphere.offset.clamp(0.0, 1.0).powf(offset_power), power];
			if let Some(light) = prepared.environment.haze {
				frame.haze_light = light.map(|face| face.extend(0.0).to_array());
				frame.haze_shape = [0.0, 0.0, 0.0, 1.0];
			}
		}

		// the effects this view shows (the particles' notes kept for where they were made before)
		let mut particle_missing = Vec::new();
		let particle_view = self.particle_geometry(scene, &prepared.particles, &camera, width as f32 / height as f32, quality.particle_rate, &mut particle_missing, lease);
		let beam_view = self.beam_geometry(scene, &camera, lease);
		let trail_view = self.trail_geometry(scene, &camera, lease);
		// emitters and beams that ZOffset brings in front of what hides them where they stand are dropped by
		// a Roblox client's occlusion culling, which tests where they stand (`fixtures/draw-order.luau`
		// "cull", "cull2": a beam and a square 36 studs out with ZOffset 10 behind a 10 x 10 block at 30, head-on
		// and from 6 studs aside, show nothing; reaching past the block's edge, over 1-stud blocks, or flying
		// out in front of it, they show; notes/occlusion.md)
		let (hidden_emitters, hidden_beams) = {
			let aspect = width as f32 / height as f32;
			let emitters: Vec<(usize, (Vec3, Vec3))> = particle_view.draws.iter().filter(|draw| scene.particles[draw.emitter].z_offset > 0.0).filter_map(|draw| particles_box(prepared.particles.of(draw.emitter)).map(|bounds| (draw.emitter, bounds))).collect();
			let beams: Vec<(usize, (Vec3, Vec3))> = beam_view.draws.iter().filter(|draw| scene.beams[draw.beam].z_offset > 0.0).map(|draw| (draw.beam, beam_box(&scene.beams[draw.beam]))).collect();
			let load = |parts: &[Arc<Part>]| crate::cost::meshes::Meshes::load(parts, &self.assets);
			let hidden_emitters = hidden_effects(scene, &emitters, &camera, aspect, load);
			let hidden_beams = hidden_effects(scene, &beams, &camera, aspect, load);
			(hidden_emitters, hidden_beams)
		};
		// the lights' voxels where those with LightInfluence read them, baked once (voxel_field), as far
		// from the camera as they take any (VOXEL_LIGHT_REACH)
		let lit = voxel_field::union(voxel_field::union(particle_view.lit, beam_view.lit), trail_view.lit).map(|(low, high)| {
			let reach = Vec3::splat(voxel_field::LIGHT_REACH + voxel_field::PARTICLE_READ);
			(low.max(camera.eye - reach), high.min(camera.eye + reach))
		});
		// and where surfaces read them when every surface takes them (6 studs out along their normal,
		// short of the far surfaces' distance: mesh.wgsl `local_lights`), as far out as one field holds
		// them (voxel_field::view_extent)
		let surfaces_lit = soft.then(|| voxel_field::surfaces(visible_draws.iter().map(|draw| draw.bound), camera.eye)).flatten();
		let reaches = || lights.iter().map(|light| (Vec3::new(light.position[0], light.position[1], light.position[2]), light.voxel_color[3] + light.right[3] + light.up[3]));
		let aspect = width as f32 / height as f32;
		let extent = lights_buffer.as_ref().and_then(|_| voxel_field::view_extent(lit, surfaces_lit, |depth| {
			// grown by the read's offset: a surface at its edge reads 6 studs out of it
			let (low, high) = voxel_field::frustum_box(&camera, aspect, depth);
			(low - voxel_field::SURFACE_READ, high + voxel_field::SURFACE_READ)
		}, reaches));
		let baked = extent.zip(lights_buffer.as_ref()).map(|(extent, lights_buffer)| {
			let baker = self.voxel_field.get_or_init(|| {
				let baker = voxel_field::Baker::new(device, self.gpu.pipeline_cache.as_ref());
				self.gpu.save_pipeline_cache();
				baker
			});
			frame.voxel_field = [extent.origin.x as f32, extent.origin.y as f32, extent.origin.z as f32, 1.0];
			frame.voxel_field_size = [extent.size.x as f32, extent.size.y as f32, extent.size.z as f32, 0.0];
			baker.bake(&mut encoder, lease, lights_buffer, lights.len() as u32, extent, &profile)
		});
		let light_bindings = LightBindings {
			lights: lights_buffer.as_ref().unwrap_or(&prepared.no_lights),
			shadow_maps: shadow_maps.as_ref().unwrap_or(&self.defaults.empty_light_shadows),
			shadow_matrices: shadow_matrices_buffer.as_ref().unwrap_or(&self.defaults.no_light_matrices),
			voxel_field: baked.as_ref().unwrap_or(&self.defaults.no_voxel_field),
			clusters: clusters_buffer.as_ref().unwrap_or(&self.defaults.no_light_clusters),
		};

		let frame_buffer = lease.buffer("frame", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&frame));
		let shadow_view = sun_atlas.as_ref().map(|(atlas, _)| atlas).unwrap_or(&self.defaults.empty_moments);
		// post effects: DepthOfField and Blur on the scene's radiance (the bloom reads it between the two),
		// SunRays added to the radiance (over the sky only: not on flat or transparent backgrounds); a
		// graphics quality level below 16 draws no DepthOfField and no SunRays, Blur at every level
		let depth_of_field = post::depth_of_field(&environment.effects).filter(|_| quality.sun_rays_depth_of_field);
		let blur = post::blur(&environment.effects, height).map(|blur| post::Blur { sigma: blur.sigma * render_height as f32 / height as f32, ..blur });
		let sun_rays = post::sun_rays(&environment.effects).filter(|_| request.background == Background::Sky && quality.sun_rays_depth_of_field).and_then(|rays| {
			let view_proj = camera.view_projection(width as f32 / height as f32);
			let sun = environment::sun_direction(environment.clock_time, environment.geographic_latitude);
			post::sun_rays_uniform(rays, sun, view_proj, camera.ray_unprojection(width as f32 / height as f32), width as f32 / height as f32)
		});
		// ForceField's contact glow, soft particles, Glass's refraction, the depth of field and the sun
		// rays read the opaque parts' depth, drawn first (single-sampled): not for particles all off
		// the view
		let scene_depth_on = visible_draws.iter().copied().any(|draw| draw.forcefield || draw.glass) || !particle_view.draws.is_empty() || depth_of_field.is_some() || sun_rays.is_some();
		// the ambient occlusion, when the flat ambient or the sky's diffuse light shows (not in a
		// ViewportFrame; from graphics quality level 20)
		let ambient_lit = (environment.ambient.max_element() > 0.0 || environment.outdoor_ambient.max_element() > 0.0 || environment.environment_diffuse_scale > 0.0) && ambient_occlusion_enabled() && quality.ambient_occlusion && environment.viewport.is_none();
		let ao_on = ambient_lit && visible_draws.iter().copied().any(|draw| (!draw.transparent || (samples > 1 && draw.write_depth)) && !draw.decal);
		let scene_depth = scene_depth_on.then(|| {
			lease.scratch_view(&wgpu::TextureDescriptor {
				label: Some("scene depth"),
				size: wgpu::Extent3d { width: render_width, height: render_height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: DEPTH_FORMAT,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
		});
		let ao_targets = ao_on.then(|| ao::AoTargets::new(lease, render_width, render_height, ao_history));
		let occlusion = ao_targets.as_ref().map(|targets| &targets.occlusion).unwrap_or(&self.defaults.white.view);
		// Glass shows the opaque scene through it, bent (mesh.wgsl `fs_glass`): a copy of it, taken before
		// the transparent parts
		let glass_copy = visible_draws.iter().copied().any(|draw| draw.glass).then(|| {
			lease.scratch(&wgpu::TextureDescriptor {
				label: Some("opaque scene"),
				size: wgpu::Extent3d { width: render_width, height: render_height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: HDR_FORMAT,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			})
		});
		let glass_copy_view = glass_copy.as_ref().map(|texture| texture.create_view(&Default::default()));
		let frame_group = self.frame_bind_group(prepared, &frame_buffer, &light_bindings, shadow_view, scene_depth.as_ref().unwrap_or(&self.defaults.empty_shadow), occlusion, glass_copy_view.as_ref().unwrap_or(&self.defaults.black.view));
		// the opaque parts' pass resolves into the copy, so it binds the frame without it
		let opaque_group = glass_copy_view.as_ref().map(|_| self.frame_bind_group(prepared, &frame_buffer, &light_bindings, shadow_view, scene_depth.as_ref().unwrap_or(&self.defaults.empty_shadow), occlusion, &self.defaults.black.view));
		// the depth pass binds the frame without the texture it draws into
		let depth_group = scene_depth.as_ref().map(|_| self.frame_bind_group(prepared, &frame_buffer, &light_bindings, shadow_view, &self.defaults.empty_shadow, &self.defaults.white.view, &self.defaults.black.view));

		let (sky_on, background) = match request.background {
			Background::Sky => (1.0, [0.0, 0.0, 0.0, 1.0]),
			Background::Transparent => (0.0, [0.0, 0.0, 0.0, 0.0]),
			Background::Frame { color, alpha } => {
				let color = linear(Vec3::from(color));
				(0.0, [color.x, color.y, color.z, alpha.clamp(0.0, 1.0)])
			}
			Background::Color(color) => {
				let color = linear(Vec3::from(color));
				(0.0, [color.x, color.y, color.z, 1.0])
			}
		};
		let bodies = self.celestial_bodies(environment, &mut stats.missing);
		let mut sky_uniform = SkyUniform::zeroed().with(|uniform| {
			uniform.params = [1.0, sky_on, if matches!(request.background, Background::Frame { .. }) { 1.0 } else { 0.0 }, 0.0];
			uniform.background = background;
			uniform.sun = bodies.sun;
			uniform.moon = bodies.moon;
			uniform.bodies = bodies.weights;
			uniform.rays = camera.ray_unprojection(render_width as f32 / render_height as f32).to_cols_array_2d();
			uniform.stars = star_uniform(environment, supersample);
		});
		if let Some(images) = &prepared.environment.images {
			sky_uniform.images = [1.0, 0.0, SKY_RADIANCE, 0.0];
			sky_uniform.rotation = [images.rotation.x_axis, images.rotation.y_axis, images.rotation.z_axis].map(|column| column.extend(0.0).to_array());
			if let Some((above, below)) = images.tint {
				sky_uniform.tint_above = above.extend(1.0).to_array();
				sky_uniform.tint_below = below.extend(1.0).to_array();
			}
			if let Some(atmosphere) = images.atmosphere {
				sky_uniform.haze = atmosphere.haze;
				sky_uniform.haze_color = atmosphere.color;
				sky_uniform.haze_light = atmosphere.light;
				sky_uniform.glare = atmosphere.glare;
				sky_uniform.decay = atmosphere.decay;
				sky_uniform.glare_sun = atmosphere.sun;
			}
		}
		let volume = match environment.clouds.filter(|_| sky_on > 0.5) {
			Some(_) => match self.clouds.get_or_init(|| clouds::load(&self.gpu, &self.assets)) {
				Ok(volume) => Some(volume),
				Err(error) => {
					stats.missing.push(format!("Terrain Clouds: {error}"));
					None
				}
			},
			None => None,
		};
		// the look by the view's graphics quality (crate::quality::Quality::clouds): the lowest two lay
		// the volume's 2D stand-in flat (render::clouds::Flat)
		let (buffer_width, buffer_height) = clouds::buffer_size(render_width, render_height, supersample);
		let flat = match volume.filter(|_| clouds::draws_flat(quality.clouds)) {
			Some(_) => match self.clouds_flat.get_or_init(|| Ok((clouds::load_flat(&self.gpu, &self.assets)?, clouds::load_flat_detail(&self.gpu, &self.assets)?))) {
				Ok(views) => Some(views),
				Err(error) => {
					stats.missing.push(format!("Terrain Clouds: {error}"));
					None
				}
			},
			None => None,
		};
		if volume.is_some() {
			let pixel = 2.0 * (camera.fov.to_radians() * 0.5).tan() / buffer_height as f32;
			// an Atmosphere fades them by its Density whatever its Haze, over a skybox drawn from its images
			let atmosphere = environment.atmosphere.as_ref().filter(|_| prepared.environment.images.is_some());
			let look = if flat.is_some() || !clouds::draws_flat(quality.clouds) { quality.clouds } else { 3 };
			sky_uniform.clouds = clouds::sky_uniform(environment.clouds.as_ref(), look, pixel, atmosphere);
		}
		let drawn_clouds = volume.is_some();
		let (flat, flat_detail) = flat.map(|(stand_in, detail)| (stand_in, detail)).unwrap_or((&self.defaults.black.view, &self.defaults.black.view));
		let (volume, detail) = volume.map(|volumes| (&volumes.shape, &volumes.detail)).unwrap_or((&self.defaults.no_clouds, &self.defaults.no_clouds));
		let sky_buffer = lease.buffer("sky params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&sky_uniform));
		let body_view = |texture: &Option<Arc<GpuTexture>>| texture.as_ref().map(|texture| texture.view()).unwrap_or_else(|| self.defaults.black.view.clone());
		let (sun_view, moon_view) = (body_view(&bodies.sun_texture), body_view(&bodies.moon_texture));
		// the sky's bind group with `cloud_buffer` (binding 10) the clouds' buffer it reads
		let sky_group_with = |cloud_buffer: &wgpu::TextureView| {
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("sky"),
				layout: &self.pipelines.sky_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: frame_buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&prepared.environment.sky) },
					wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.defaults.linear_sampler) },
					wgpu::BindGroupEntry { binding: 3, resource: sky_buffer.as_entire_binding() },
					wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&sun_view) },
					wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&moon_view) },
					wgpu::BindGroupEntry {
						binding: 6,
						resource: wgpu::BindingResource::TextureView(prepared.environment.images.as_ref().map(|images| &images.view).unwrap_or(&self.defaults.no_sky_images)),
					},
					wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(volume) },
					wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::Sampler(&self.defaults.cloud_sampler) },
					wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(detail) },
					wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(cloud_buffer) },
					wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::TextureView(flat) },
					wgpu::BindGroupEntry { binding: 12, resource: wgpu::BindingResource::TextureView(flat_detail) },
				],
			})
		};
		// Terrain Clouds: marched into a buffer at a reduced size, blurred there, and read by the sky pass
		// through a second blur (render::clouds::buffer_size, notes/render.md "Clouds")
		let cloud_view = if drawn_clouds {
			let (width, height) = (buffer_width, buffer_height);
			let buffer = |label: &str| {
				lease.scratch_view(&wgpu::TextureDescriptor {
					label: Some(label),
					size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
					mip_level_count: 1,
					sample_count: 1,
					dimension: wgpu::TextureDimension::D2,
					format: HDR_FORMAT,
					usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
					view_formats: &[],
				})
			};
			let (marched, blurred) = (buffer("clouds"), buffer("clouds blurred"));
			let [march, blur] = self.pipelines.clouds(device);
			for (pipeline, input, output, name) in [(march, &self.defaults.black.view, &marched, "clouds"), (blur, &marched, &blurred, "clouds blur")] {
				let group = sky_group_with(input);
				let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
					label: Some(name),
					color_attachments: &[Some(wgpu::RenderPassColorAttachment {
						view: output,
						depth_slice: None,
						resolve_target: None,
						ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
					})],
					depth_stencil_attachment: None,
					timestamp_writes: None,
					occlusion_query_set: None,
				});
				let time = profile.start(&mut pass, name);
				pass.set_pipeline(pipeline);
				pass.set_bind_group(0, &group, &[]);
				pass.draw(0..3, 0..1);
				profile.end(&mut pass, time);
			}
			blurred
		} else {
			self.defaults.black.view.clone()
		};
		let sky_group = sky_group_with(&cloud_view);

		// the occlusion first, its targets then the main pass's (same kinds: ao::AoTargets::release), the
		// frame's memory going round instead of doubling (a 1080p view's AO took 4-sample colour and depth
		// as big as the main pass's, 95 MB beside them)
		let coverage = samples > 1;
		if let Some(targets) = &ao_targets {
			let opaque = visible_draws.iter().copied().filter(|draw| !draw.transparent && !draw.decal && draw.occludes).map(|draw| (&draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, draw.double_sided));
			// and the parts drawn by coverage where they cover every sample (particle-sa-coverage: Studio's
			// panel darkens beside the dome map's opaque texels 3 studs in front of it, not beside the cone
			// map's texels on 3 samples or fewer)
			let by_coverage = visible_draws.iter().copied().filter(|draw| coverage && draw.write_depth && !draw.highlight_only).map(|draw| (&draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, draw.double_sided, groups.of(&draw.material)));
			targets.encode(lease, &mut encoder, &self.pipelines, &frame_group, &camera, opaque, by_coverage, &profile);
			targets.release(lease);
		}
		let target = |label: &str, format: wgpu::TextureFormat, sample_count: u32, usage: wgpu::TextureUsages| {
			lease.scratch(&wgpu::TextureDescriptor {
				label: Some(label),
				size: wgpu::Extent3d { width: render_width, height: render_height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count,
				dimension: wgpu::TextureDimension::D2,
				format,
				usage,
				view_formats: &[],
			})
		};
		// sampled usage too, as the occlusion's targets of these kinds have
		let color_msaa = (samples > 1).then(|| target("color msaa", HDR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING));
		let color = target("color", HDR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
		// a window's world reads its opaque share from the depth after the opaque parts
		let opaque_share = request.opaque_share && request.texture && supersample == 1;
		let depth = target("depth", DEPTH_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
		// the occlusion's prepass depth when it is this one (the view at the AO's size and samples): the
		// main pass starts from it, its hidden fragments rejected before they shade (ao::AoTargets::depth;
		// `ROR_MAIN_DEPTH=clear` clears it instead)
		let depth_drawn = ao_targets.as_ref().and_then(|targets| targets.depth()).is_some_and(|drawn| *drawn == depth) && std::env::var("ROR_MAIN_DEPTH").map_or(true, |value| value != "clear");
		let color_view = color.create_view(&Default::default());
		let color_msaa_view = color_msaa.as_ref().map(|texture| texture.create_view(&Default::default()));
		let depth_view = depth.create_view(&Default::default());
		// legacy effects blend in linear light in a layer of their own (its MSAA target, the layer, the
		// scene with it composited over), before the depth of field and the bloom (render::particles)
		let legacy_targets = scene.particles.iter().any(|emitter| emitter.linear).then(|| {
			let msaa = (samples > 1).then(|| target("legacy msaa", HDR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default()));
			let layer = target("legacy layer", HDR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING).create_view(&Default::default());
			let composited = target("legacy composite", HDR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING).create_view(&Default::default());
			(msaa, layer, composited)
		});
		// transparent parts and beams together, back to front along this camera's forward axis
		// (Camera::depth; beam-order: a transparent part in front of a beam covers it)
		// with multisampling, parts at Transparency 0 whose SurfaceAppearance is in AlphaMode Transparency
		// are drawn by alpha to coverage with the opaque parts, not blended in this order
		let mut transparent: Vec<(f32, Blended)> = visible_draws.iter().copied().filter(|draw| draw.transparent && !draw.decal && (!draw.highlight_only || draw.glass) && !(coverage && draw.write_depth)).map(|draw| (camera.depth(draw.center), Blended::Part(draw))).collect();
		// a see-through part's decals (an invisible one's too) sort with it, drawn after it in ZIndex
		// order: Studio draws a particle 1.5 studs behind an invisible plate's decal under the decal and
		// one in front over it, and a half-transparent plate under its own decal (particle-decal-order;
		// an effect's clash: its smoke behind the comet's decals)
		let mut see_through_decals: Vec<&DrawItem> = visible_draws.iter().copied().filter(|draw| draw.decal && draw.transparent).collect();
		see_through_decals.sort_by_key(|draw| draw.decal_order);
		transparent.extend(see_through_decals.into_iter().map(|draw| (camera.depth(draw.center), Blended::Decal(draw))));
		transparent.extend(beam_view.draws.iter().filter(|draw| prepared.beam_textures[draw.beam].is_some() && !hidden_beams.contains(&draw.beam)).map(|draw| (camera.depth(draw.center), Blended::Beam(draw))));
		transparent.extend(trail_view.draws.iter().filter(|draw| prepared.trail_textures[draw.trail].is_some()).map(|draw| (camera.depth(draw.center), Blended::Trail(draw))));
		stats.missing.extend(particle_missing);
		// particles by their emitter's position moved towards the camera by ZOffset (particle-sort
		// glass-zoffset: a particle behind a glass block's centre pulled out in front of it draws over it)
		transparent.extend(particle_view.draws.iter().filter(|draw| !scene.particles[draw.emitter].linear && !hidden_emitters.contains(&draw.emitter)).map(|draw| (camera.depth(draw.center) - scene.particles[draw.emitter].z_offset, Blended::Particles(draw))));
		// legacy effects take their place in the order too (`fixtures/draw-order.luau` "legacy", "legacy2" in a
		// Roblox client: a Fire 2.5 studs behind a SmoothPlastic pane at Transparency 0.2, 0.5 or 0.8, a beam
		// or a ParticleEmitter square at 0.5 shows through it, one 2.5 studs in front over it, two either side
		// of one pane each so); under a depth of field they go into their layer, back to front among
		// themselves, before what blends
		let interleaved = depth_of_field.is_none() || scene_depth.is_none();
		let mut legacy_draws: Vec<(f32, &particles::ParticleDraw)> = Vec::new();
		for draw in particle_view.draws.iter().filter(|draw| scene.particles[draw.emitter].linear) {
			if interleaved {
				transparent.push((camera.depth(draw.center), Blended::Legacy(draw)));
			} else {
				legacy_draws.push((camera.depth(draw.center), draw));
			}
		}
		legacy_draws.sort_by(|a, b| b.0.total_cmp(&a.0));
		// GUIs sort by their face's centre or their anchor; on a shared face a higher ZOffset draws later
		transparent.extend(view_guis.draws.iter().filter(|draw| !draw.overlay).map(|draw| (camera.depth(draw.center) - draw.z_offset * 1e-3, Blended::Gui(draw))));
		// at one distance parts, beams and trails draw before particles and GUIs after them
		// (particle-part-ties: a white particle at the centre of a red ball, ForceField or
		// SmoothPlastic, draws over it whatever their sizes or their order in the file); emitters
		// there draw the one whose particles moved further away from the camera since they spawned
		// last, over the others (vfx-moving-ties: a white square sent 1 stud away or towards the
		// camera from a red one's attachment, Sizes 0.8 against 1 and 1 against 0.8, Studio's top
		// colour in all four the reverse of their depths, a sideways one by their sizes; Rule's aura:
		// its black emitters on its part, their particles a little towards the camera, under the glow
		// of those on its attachment; only at one place: an effect's fire, its emitters a few studs
		// apart, keeps the order of their places), then their largest particles first, the smaller over
		// them, and those of one size in their order in the file (particle-ties, -ties2, -ties3: the
		// smaller of two squares at one place on top, whichever comes first; their largest Size over
		// their life, not their size now; 0.002 studs nearer wins over any size; where they spawned
		// in their part does not count, vfx-sibling-order's pairs on parts keeping their sizes' order).
		// Parts at one distance draw the larger first too (an effect's rings: eight Glass spheres on one
		// centre, 7.42 to 4.67 studs across in no order in the file, each ring drawn where no smaller
		// sphere covers it). Beams tied in distance keep their order in the file: Studio's order there
		// follows no rule (beam-ties: the same pairs swap between a head-on and an oblique view)
		// each drawn emitter's largest Size, found once (not twice a comparison)
		let mut largest = vec![0.0; scene.particles.len()];
		for draw in &particle_view.draws {
			largest[draw.emitter] = scene.particles[draw.emitter].largest_size();
		}
		let tie = |blended: &Blended| match blended {
			Blended::Part(draw) => (0, 0.0, -draw.bound.1),
			Blended::Decal(_) => (0, 0.0, f32::MAX),
			Blended::Beam(_) | Blended::Trail(_) => (0, 0.0, 0.0),
			Blended::Particles(draw) => (1, draw.moved, -largest[draw.emitter]),
			// a legacy effect's systems at one place keep their own order (a Fire's wisps over its flame:
			// legacy-fire-cover)
			Blended::Legacy(_) => (1, 0.0, 0.0),
			Blended::Gui(_) => (2, 0.0, 0.0),
		};
		transparent.sort_by(|a, b| {
			let (first, second) = (tie(&a.1), tie(&b.1));
			b.0.total_cmp(&a.0).then(first.0.cmp(&second.0)).then(first.1.total_cmp(&second.1)).then(first.2.total_cmp(&second.2))
		});
		// the order's runs: what blends up to the first legacy effect goes with the scene; then each run of
		// legacy effects into their layer, composited over what is drawn so far, and each run of what blends
		// after one into a layer of its own (premultiplied in √x over transparent black, as the scene would
		// take it) laid over that, turn by turn
		let runs = blend_runs(&transparent.iter().map(|(_, item)| matches!(item, Blended::Legacy(_))).collect::<Vec<_>>());
		let first_run = match runs.first() {
			Some(BlendRun::Blended(range)) => range.clone(),
			_ => 0..0,
		};
		// each run after the first writes the scene anew, into one of two targets in turn
		let composites: Vec<wgpu::TextureView> = match &legacy_targets {
			Some((_, _, composited)) if interleaved && runs.len() > 2 => {
				vec![composited.clone(), target("legacy composite 2", HDR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING).create_view(&Default::default())]
			}
			Some((_, _, composited)) => vec![composited.clone()],
			None => Vec::new(),
		};
		let lit_view = match &legacy_targets {
			Some(_) if interleaved && runs.len() > 1 => composites[(runs.len() - 2) % composites.len()].clone(),
			Some((_, _, composited)) if !interleaved => composited.clone(),
			_ => color_view.clone(),
		};
		// the depth of field blurs the opaque scene; what blends (particles, beams, trails, transparent
		// parts and decals, GUIs; Glass goes with the scene) takes its far blur by its own depth, not the
		// depth behind it, and its near blur from the scene behind it (post-depth-blended: under Roblox's
		// default field a white Beam, particle and half-transparent part blur alike before a tile just
		// behind them and before a backdrop 600 studs out, as an opaque tile at their depth does; under a
		// near-only field those before the far backdrop stay sharp and those before a near tile blur with
		// it; post-depth-effects: a particle, a Beam and a half-transparent part 160 studs out stay sharp
		// under a cutscene's field, whose far blur starts at 115 studs). So they draw into
		// layers by their far blur (a step of a sixth of the largest), premultiplied in √x over
		// transparent black, as the scene would take them, each blurred by its step and laid over the
		// field's far image in turn, the field's near blur over all. Back to front the steps only fall
		// (the far blur grows with depth), so each layer holds a run of the order.
		let far_steps: Vec<u8> = match depth_of_field {
			Some(focus) if scene_depth.is_some() => {
				transparent.iter().map(|(_, item)| {
					let center = match item {
						Blended::Part(draw) | Blended::Decal(draw) => draw.center,
						Blended::Beam(draw) => draw.center,
						Blended::Trail(draw) => draw.center,
						Blended::Particles(draw) | Blended::Legacy(draw) => draw.center,
						Blended::Gui(draw) => draw.center,
					};
					(post::far_blur_share(focus, camera.depth(center)) * post::FAR_STEPS as f32).round() as u8
				}).collect()
			}
			_ => Vec::new(),
		};
		let mut late_steps: Vec<u8> = far_steps.clone();
		late_steps.sort_unstable_by(|a, b| b.cmp(a));
		late_steps.dedup();
		if late_steps.is_empty() {
			late_steps.push(0);
		}
		let late_layers: Vec<(u8, Option<wgpu::TextureView>, wgpu::TextureView)> = if depth_of_field.is_some() && scene_depth.is_some() {
			late_steps
				.iter()
				.map(|&step| {
					let msaa = (samples > 1).then(|| target("late msaa", HDR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default()));
					let layer = target("late layer", HDR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING).create_view(&Default::default());
					(step, msaa, layer)
				})
				.collect()
		} else {
			Vec::new()
		};
		let mut dof_passes = post::PostPasses::new(&self.post);
		// what the bloom and the resolve read: the scene, or the scene through the depth of field
		let scene_view = match (depth_of_field, &scene_depth) {
			(Some(focus), Some(scene_depth)) => {
				let late: Vec<(f32, &wgpu::TextureView)> = late_layers.iter().map(|(step, _, layer)| (*step as f32 / post::FAR_STEPS as f32, layer)).collect();
				let index = dof_passes.depth_of_field(lease, &lit_view, scene_depth, (render_width, render_height), focus, camera.near, &self.defaults.no_bloom, &late);
				dof_passes.view(index).clone()
			}
			_ => lit_view.clone(),
		};
		// BlurEffect after the depth of field (both blur the radiance); the bloom reads the scene before the
		// blur and its glow stays sharp (post-blur size8-bloom: 95.9 % within 2 of 255, 91.6 with the bloom
		// of the blurred scene)
		let bloom_source = scene_view.clone();
		let scene_view = match blur {
			Some(blur) => {
				let index = dof_passes.blur(lease, &scene_view, (render_width, render_height), blur, &self.defaults.no_bloom, &self.defaults.empty_shadow);
				dof_passes.view(index).clone()
			}
			None => scene_view,
		};
		let rays_view = match (&sun_rays, &scene_depth) {
			(Some(uniform), Some(scene_depth)) => {
				let index = dof_passes.sun_rays(lease, uniform, scene_depth, (width, height), &self.defaults.no_bloom);
				Some(dof_passes.view(index).clone())
			}
			_ => None,
		};

		// a kept image is the caller's alone; any other is read back in this submission
		let output_desc = wgpu::TextureDescriptor {
			label: Some("output"),
			size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: OUTPUT_FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | if request.texture || video.is_some() { wgpu::TextureUsages::TEXTURE_BINDING } else { wgpu::TextureUsages::empty() },
			view_formats: &[],
		};
		let output = if request.texture { lease.texture(&output_desc) } else { lease.scratch(&output_desc) };
		// a kept image leaves with the caller, back once it is done with it (Renderer::give_back)
		if request.texture {
			lease.hand_out(&output);
		}
		let output_view = output.create_view(&Default::default());
		let bloom = bloom::bloom_parameters(environment, &quality);
		let bloom_count = bloom.level_count();
		let bloom_on = request.bloom && request.linear_scale.is_none() && bloom_count > 0 && environment.viewport.is_none();
		let bloom_target = |label: &str, (width, height): (u32, u32)| {
			lease.scratch_view(&wgpu::TextureDescriptor {
				label: Some(label),
				size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: HDR_FORMAT,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			})
		};
		let step = |input: &wgpu::TextureView, addend: Option<&wgpu::TextureView>, uniform: BloomUniform| {
			let buffer = lease.buffer("bloom params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&uniform));
			let mut entries = vec![
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(input) },
				wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
			];
			if let Some(addend) = addend {
				entries.push(wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(addend) });
			}
			let layout = if addend.is_some() { &self.pipelines.bloom_pair_layout } else { &self.pipelines.bloom_layout };
			device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("bloom"), layout, entries: &entries })
		};
		// the exposed scene clamped and averaged onto the working grid (1116 px tall), the chain of
		// half-resolution levels down (the first keeping the excess over the threshold), then back up:
		// each level adds the coarser one, upsampled and blurred by a tent, to its own texels; the
		// resolve samples the first level
		let mut bloom_views: Vec<wgpu::TextureView> = Vec::new();
		let mut bloom_steps: Vec<(wgpu::BindGroup, &wgpu::RenderPipeline, usize)> = Vec::new();
		let mut bloom_output = usize::MAX;
		// render pixels per texel of the first level, and the weight the resolve gives it (the first
		// level's own when it is the only one)
		let mut bloom_texel = 1.0f32;
		let mut bloom_weight = 1.0f32;
		if bloom_on {
			let footprint = render_height as f32 / bloom.grid_height;
			let working = (((render_width as f32 / footprint).ceil() as u32).max(1), bloom.grid_height as u32);
			bloom_texel = 2.0 * footprint;
			// (source.w past 1: how much of the threshold's peak reads the luminance; footprint.z how much of the
			// weighting does)
			let uniform = |threshold: bool, blur: [f32; 4]| BloomUniform {
				source: [frame.environment[2], bloom.threshold, bloom.cap, if threshold { 1.0 + bloom.luminance[1] } else { 0.0 }],
				footprint: [footprint, footprint, bloom.luminance[0], bloom.weighting[if threshold { 0 } else { 1 }]],
				blur,
			};
			// the textures: the working grid, each level's texels, then for each level below the last
			// the coarser one upsampled, its horizontal blur and the level's sum
			bloom_views.push(bloom_target("bloom source", working));
			let mut size = working;
			let mut levels = Vec::new();
			for _ in 0..bloom_count {
				size = (size.0.div_ceil(2), size.1.div_ceil(2));
				bloom_views.push(bloom_target("bloom level", size));
				levels.push((bloom_views.len() - 1, size));
			}
			let mut sums = Vec::new();
			for &(_, size) in &levels[..levels.len() - 1] {
				for label in ["bloom up", "bloom pass", "bloom sum"] {
					bloom_views.push(bloom_target(label, size));
				}
				sums.push(bloom_views.len() - 3);
			}
			bloom_steps.push((step(&bloom_source, None, uniform(false, [0.0; 4])), &self.pipelines.bloom_extract, 0));
			let mut previous = 0;
			for (index, &(level, _)) in levels.iter().enumerate() {
				bloom_steps.push((step(&bloom_views[previous], None, uniform(index == 0, [0.0, 0.0, 0.0, bloom.taps[index]])), &self.pipelines.bloom_down, level));
				previous = level;
			}
			// the last level enters with its weight
			let (mut upper, mut weight) = (previous, bloom.levels[levels.len() - 1]);
			for index in (0..levels.len() - 1).rev() {
				let up = sums[index];
				let tent = bloom.tents[index];
				bloom_steps.push((step(&bloom_views[upper], None, uniform(false, [0.0, 0.0, weight, 0.0])), &self.pipelines.bloom_up, up));
				bloom_steps.push((step(&bloom_views[up], None, uniform(false, [tent, 0.0, 0.0, 0.0])), &self.pipelines.bloom_blur, up + 1));
				let addend = Some(&bloom_views[levels[index].0]);
				bloom_steps.push((step(&bloom_views[up + 1], addend, uniform(false, [tent, 1.0, bloom.levels[index], 0.0])), &self.pipelines.bloom_blur_add, up + 2));
				(upper, weight) = (up + 2, 1.0);
			}
			(bloom_output, bloom_weight) = (upper, weight);
		}
		let (mut correction, tint) = color_correction(environment);
		// calibration: the exposed radiance as it is, scaled, without the tone curve
		let (exposure, tonemapper) = match request.linear_scale {
			Some(scale) => {
				correction[3] = 0.0;
				(frame.environment[2] * scale, 0.0)
			}
			// a ViewportFrame shows its radiance as it is, clipped (notes/render.md "ViewportFrames")
			None if environment.viewport.is_some() => (frame.environment[2], 0.0),
			None => (frame.environment[2], tone_curve(environment)),
		};
		let resolve_uniform = ResolveUniform {
			params: [exposure, tonemapper, supersample as f32, match request.background {
				Background::Transparent => 1.0,
				Background::Frame { .. } => 2.0,
				_ => 0.0,
			}],
			correction,
			tint,
			bloom: [bloom_weight, bloom_texel, if bloom_on { 1.0 } else { 0.0 }, 0.0],
			rays: [if rays_view.is_some() { 1.0 } else { 0.0 }, if nan_check() { 1.0 } else { 0.0 }, 0.0, 0.0],
		};
		let resolve_buffer = lease.buffer("resolve params", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&resolve_uniform));
		let resolve_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("resolve"),
			layout: &self.pipelines.resolve_layout,
			entries: &[
				wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scene_view) },
				wgpu::BindGroupEntry { binding: 1, resource: resolve_buffer.as_entire_binding() },
				wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(bloom_views.get(bloom_output).unwrap_or(&self.defaults.no_bloom)) },
				wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.defaults.linear_sampler) },
				wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(rays_view.as_ref().unwrap_or(&self.defaults.no_bloom)) },
			],
		});

		let mut decals: Vec<&DrawItem> = visible_draws.iter().copied().filter(|draw| draw.decal && !draw.transparent).collect();
		decals.sort_by_key(|draw| draw.decal_order);

		if let (Some(view), Some(group)) = (&scene_depth, &depth_group) {
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("scene depth"),
				color_attachments: &[],
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view,
					depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
					stencil_ops: None,
				}),
				timestamp_writes: profile.pass("scene depth"),
				occlusion_query_set: None,
			});
			pass.set_bind_group(0, group, &[]);
			for (double_sided, pipeline) in [(false, &self.pipelines.scene_depth.0), (true, &self.pipelines.scene_depth.1)] {
				pass.set_pipeline(pipeline);
				let mut runs = runs::Runs::new(1);
				for draw in visible_draws.iter().copied().filter(|draw| !draw.transparent && !draw.decal && draw.double_sided == double_sided) {
					runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, None);
				}
				runs.finish(&mut pass);
			}
			// and the transparent parts that write depth where they are opaque (particle-sort
			// clothes-front: a particle half a Size in front of such a head shows at half opacity), drawn
			// by coverage where they cover 2 or more of the 4 samples (particle-sa-coverage)
			let cutout = if samples > 1 { &self.pipelines.scene_depth_coverage } else { &self.pipelines.scene_depth_cutout };
			for (double_sided, pipeline) in [(false, &cutout.0), (true, &cutout.1)] {
				pass.set_pipeline(pipeline);
				let mut runs = runs::Runs::new(1);
				for draw in visible_draws.iter().copied().filter(|draw| draw.write_depth && draw.double_sided == double_sided) {
					runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(groups.of(&draw.material)));
				}
				runs.finish(&mut pass);
			}
		}
		// draws the transparent items onto a pass whose frame group is set: Glass first, back to front
		// among itself, writing its depth (the opaque scene's copy, bent and tinted, with its reflection),
		// then the rest in their order, depth-tested against it: what is behind a Glass surface is hidden
		// and what is in front draws over it, whatever their centres (glass-compose: parts, particles,
		// beams and Glass behind a pane hidden, those in front over it; glass-particles: a particle 0.5
		// studs behind a Glass Ball's centre hidden, behind an inside-out sphere's centre, in front of the
		// far half it draws, shown; an effect's black hole draws its particles over its eight spheres).
		// `glass` / `rest` say which of the two it draws (the depth of field's layers take the rest),
		// `step` the far blur step of the rest it draws (its layer), every one when None.
		let draw_transparent = |pass: &mut wgpu::RenderPass, glass: bool, rest: bool, step: Option<u8>, range: std::ops::Range<usize>| {
			for (_, item) in transparent.iter().filter(|_| glass) {
				if let Blended::Part(draw) = item
					&& draw.glass
				{
					pass.set_pipeline(self.pipelines.glass(samples, draw.double_sided));
					pass.set_bind_group(1, groups.of(&draw.material), &[]);
					pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
					pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
					pass.draw_indexed(draw.indices.clone(), 0, draw.instance..draw.instance + 1);
				}
			}
			for (index, (_, item)) in transparent.iter().enumerate() {
				if !rest || !range.contains(&index) || step.is_some_and(|step| far_steps.get(index).copied().unwrap_or(0) != step) {
					continue;
				}
				match item {
					Blended::Part(draw) if draw.glass => {}
					Blended::Part(draw) => {
						pass.set_bind_group(1, groups.of(&draw.material), &[]);
						pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
						pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
						// its opaque texels' depth first, so the mesh's own faces behind them do not blend
						// over them (the Valkyrie Helm's insides showed through its front, its triangles drawn
						// in mesh order)
						if draw.write_depth {
							pass.set_pipeline(self.pipelines.cutout_depth(samples, draw.double_sided));
							pass.draw_indexed(draw.indices.clone(), 0, draw.instance..draw.instance + 1);
						}
						pass.set_pipeline(self.pipelines.transparent(samples, draw.double_sided));
						pass.draw_indexed(draw.indices.clone(), 0, draw.instance..draw.instance + 1);
					}
					Blended::Decal(draw) => {
						pass.set_pipeline(self.pipelines.decal(samples, draw.mesh.bind.is_some()));
						self.draw_decal(pass, &groups, draw);
					}
					Blended::Beam(draw) => self.draw_beam(pass, &beam_view, draw, &prepared.beam_textures, samples),
					Blended::Trail(draw) => self.draw_trail(pass, &trail_view, draw, &prepared.trail_textures, samples),
					Blended::Particles(draw) => self.draw_particles(pass, &particle_view, draw, &prepared.particles, samples, false),
					// in their own layer (`runs`)
					Blended::Legacy(_) => {}
					Blended::Gui(draw) => self.draw_gui(pass, view_guis, draw, samples),
				}
			}
		};
		{
			let (view, resolve_target) = match &color_msaa_view {
				Some(msaa) => (msaa, Some(&color_view)),
				None => (&color_view, None),
			};
			let split = glass_copy_view.is_some() || !late_layers.is_empty();
			let opaque_resolve = match (&color_msaa_view, &glass_copy_view) {
				(Some(_), Some(copy)) => Some(copy),
				_ => resolve_target,
			};
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("main"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view,
					depth_slice: None,
					resolve_target: opaque_resolve,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view: &depth_view,
					// kept for the transparent pass after Glass's copy, for the legacy effects' layer and for
					// a window's opaque share
					depth_ops: Some(wgpu::Operations { load: if depth_drawn { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(0.0) }, store: if split || legacy_targets.is_some() || opaque_share { wgpu::StoreOp::Store } else { wgpu::StoreOp::Discard } }),
					stencil_ops: None,
				}),
				timestamp_writes: profile.pass("main"),
				occlusion_query_set: None,
			});
			let mesh_time = profile.start(&mut pass, "main meshes");
			pass.set_bind_group(0, opaque_group.as_ref().unwrap_or(&frame_group), &[]);
			for double_sided in [false, true] {
				pass.set_pipeline(self.pipelines.opaque(samples, double_sided));
				let mut runs = runs::Runs::new(1);
				for draw in visible_draws.iter().copied().filter(|draw| !draw.transparent && !draw.decal && draw.double_sided == double_sided) {
					runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(groups.of(&draw.material)));
				}
				runs.finish(&mut pass);
			}
			// a SurfaceAppearance in AlphaMode Transparency on a part at Transparency 0 covers its texels'
			// alpha's share of the samples, opaque, with its depth: the sky, the parts, particles and
			// beams behind show in the rest, whatever their order (particle-sa-coverage: Studio's light
			// through such a quad falls in steps with its alpha; an effect's sky dome)
			if coverage {
				for double_sided in [false, true] {
					pass.set_pipeline(self.pipelines.coverage(samples, double_sided));
					let mut runs = runs::Runs::new(1);
					for draw in visible_draws.iter().copied().filter(|draw| draw.write_depth && !draw.highlight_only && draw.double_sided == double_sided) {
						runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(groups.of(&draw.material)));
					}
					runs.finish(&mut pass);
				}
			}
			profile.end(&mut pass, mesh_time);
			let sky_time = profile.start(&mut pass, "main sky");
			pass.set_pipeline(self.pipelines.sky(samples));
			pass.set_bind_group(0, &sky_group, &[]);
			pass.draw(0..3, 0..1);
			pass.set_bind_group(0, opaque_group.as_ref().unwrap_or(&frame_group), &[]);
			// decals after the sky: an invisible part's decals write no depth, and Studio shows them over
			// the sky (vfx-decals: white, Color3 2 and half transparent decals on Transparency 1 plates;
			// mesh effects, the comet of an effect's clash)
			profile.end(&mut pass, sky_time);
			let decal_time = profile.start(&mut pass, "main decals");
			let mut set = None;
			for draw in &decals {
				let bind_pose = draw.mesh.bind.is_some();
				if set != Some(bind_pose) {
					pass.set_pipeline(self.pipelines.decal(samples, bind_pose));
					set = Some(bind_pose);
				}
				self.draw_decal(&mut pass, &groups, draw);
			}
			profile.end(&mut pass, decal_time);
			if !split {
				let transparent_time = profile.start(&mut pass, "main transparent");
				draw_transparent(&mut pass, true, true, None, first_run.clone());
				profile.end(&mut pass, transparent_time);
			}
		}
		// a window's world: the share of each pixel's samples its opaque parts cover (opaque_share.wgsl),
		// kept with the image
		let share_texture = opaque_share.then(|| {
			let texture = lease.texture(&wgpu::TextureDescriptor {
				label: Some("opaque share"),
				size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: OPAQUE_SHARE_FORMAT,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
				view_formats: &[],
			});
			lease.hand_out(&texture);
			let (layout, pipeline) = &self.pipelines.opaque_share[(samples > 1) as usize];
			let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("opaque share"),
				layout,
				entries: &[wgpu::BindGroupEntry { binding: (samples > 1) as u32, resource: wgpu::BindingResource::TextureView(&depth_view) }],
			});
			let target = texture.create_view(&Default::default());
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("opaque share"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &target,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("opaque share"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(pipeline);
			pass.set_bind_group(0, &group, &[]);
			pass.draw(0..3, 0..1);
			drop(pass);
			texture
		});
		if let Some(copy) = &glass_copy {
			if color_msaa_view.is_none() {
				encoder.copy_texture_to_texture(color.as_image_copy(), copy.as_image_copy(), wgpu::Extent3d { width: render_width, height: render_height, depth_or_array_layers: 1 });
			}
			let (view, resolve_target) = match &color_msaa_view {
				Some(msaa) => (msaa, Some(&color_view)),
				None => (&color_view, None),
			};
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("transparent"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view,
					depth_slice: None,
					resolve_target,
					ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view: &depth_view,
					depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: if legacy_targets.is_some() || !late_layers.is_empty() { wgpu::StoreOp::Store } else { wgpu::StoreOp::Discard } }),
					stencil_ops: None,
				}),
				timestamp_writes: profile.pass("transparent"),
				occlusion_query_set: None,
			});
			pass.set_bind_group(0, &frame_group, &[]);
			draw_transparent(&mut pass, true, late_layers.is_empty(), None, first_run.clone());
		}
		// what blends, into its layers for the depth of field, the farthest-blurred first, depth-tested
		// against the scene (and the depth its cut-out parts write, kept from layer to layer)
		for (index, (step, msaa, layer)) in late_layers.iter().enumerate() {
			let last = index + 1 == late_layers.len();
			let (view, resolve_target) = match msaa {
				Some(msaa) => (msaa, Some(layer)),
				None => (layer, None),
			};
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("late transparent"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view,
					depth_slice: None,
					resolve_target,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
					view: &depth_view,
					depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: if legacy_targets.is_some() || !last { wgpu::StoreOp::Store } else { wgpu::StoreOp::Discard } }),
					stencil_ops: None,
				}),
				timestamp_writes: if index == 0 { profile.pass("late transparent") } else { None },
				occlusion_query_set: None,
			});
			pass.set_bind_group(0, &frame_group, &[]);
			draw_transparent(&mut pass, false, true, Some(*step), 0..transparent.len());
		}
		if let Some((msaa, layer, _)) = &legacy_targets
			&& interleaved
		{
			let (view, resolve_target) = match msaa {
				Some(msaa) => (msaa, Some(layer)),
				None => (layer, None),
			};
			let mut below = color_view.clone();
			for (turn, run) in runs.iter().skip(1).enumerate() {
				let legacy = matches!(run, BlendRun::Legacy(_));
				{
					let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
						label: Some(if legacy { "legacy effects" } else { "blended after legacy effects" }),
						color_attachments: &[Some(wgpu::RenderPassColorAttachment {
							view,
							depth_slice: None,
							resolve_target,
							// legacy effects: no light, everything behind seen; what blends: transparent black
							ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: if legacy { 1.0 } else { 0.0 } }), store: wgpu::StoreOp::Store },
						})],
						// what blends may write its cut-outs' depth for what comes after it
						depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }), stencil_ops: None }),
						timestamp_writes: if turn == 0 { profile.pass("legacy effects") } else { None },
						occlusion_query_set: None,
					});
					pass.set_bind_group(0, &frame_group, &[]);
					match run {
						BlendRun::Legacy(range) => {
							for (_, item) in &transparent[range.clone()] {
								if let Blended::Legacy(draw) = item {
									self.draw_particles(&mut pass, &particle_view, draw, &prepared.particles, samples, true);
								}
							}
						}
						BlendRun::Blended(range) => draw_transparent(&mut pass, false, true, None, range.clone()),
					}
				}
				let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
					label: Some("legacy composite"),
					layout: &self.particles.composite_layout,
					entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&below) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(layer) }],
				});
				let out = &composites[turn % composites.len()];
				let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
					label: Some("legacy composite"),
					color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: out, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
					depth_stencil_attachment: None,
					timestamp_writes: if turn == 0 { profile.pass("legacy composite") } else { None },
					occlusion_query_set: None,
				});
				pass.set_pipeline(if legacy { &self.particles.composite } else { &self.particles.over });
				pass.set_bind_group(0, &group, &[]);
				pass.draw(0..3, 0..1);
				drop(pass);
				below = out.clone();
			}
		} else if let Some((msaa, layer, composited)) = &legacy_targets {
			{
				let (view, resolve_target) = match msaa {
					Some(msaa) => (msaa, Some(layer)),
					None => (layer, None),
				};
				let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
					label: Some("legacy effects"),
					color_attachments: &[Some(wgpu::RenderPassColorAttachment {
						view,
						depth_slice: None,
						resolve_target,
						// no light, everything behind seen
						ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }), store: wgpu::StoreOp::Store },
					})],
					depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Discard }), stencil_ops: None }),
					timestamp_writes: profile.pass("legacy effects"),
					occlusion_query_set: None,
				});
				pass.set_bind_group(0, &frame_group, &[]);
				for (_, draw) in &legacy_draws {
					self.draw_particles(&mut pass, &particle_view, draw, &prepared.particles, samples, true);
				}
			}
			let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("legacy composite"),
				layout: &self.particles.composite_layout,
				entries: &[
					wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&color_view) },
					wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(layer) },
				],
			});
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("legacy composite"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: composited,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("legacy composite"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&self.particles.composite);
			pass.set_bind_group(0, &group, &[]);
			pass.draw(0..3, 0..1);
		}
		dof_passes.encode(&mut encoder, &profile);
		for (group, pipeline, target) in &bloom_steps {
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("bloom"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &bloom_views[*target],
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("bloom"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(pipeline);
			pass.set_bind_group(0, group, &[]);
			pass.draw(0..3, 0..1);
		}
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("resolve"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &output_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
				})],
				depth_stencil_attachment: None,
				timestamp_writes: profile.pass("resolve"),
				occlusion_query_set: None,
			});
			pass.set_pipeline(&self.pipelines.resolve);
			pass.set_bind_group(0, &resolve_group, &[]);
			pass.draw(0..3, 0..1);
		}
		// ViewRequest::texture: the image stays on the GPU, without what is composited after a readback
		let keep = request.texture;
		let gui_layer = if keep { None } else { self.encode_gui_overlays(&mut encoder, prepared, view_guis, &camera, width, height, &profile) };
		let highlight_mask = if keep { None } else { prepared.highlights.as_ref().map(|prepared_highlights| highlights::encode_mask(device, &mut encoder, self.id_pipelines(), prepared_highlights, &frame_group, &prepared.draws, width, height, &profile)) };
		let part_ids = (request.ids && !keep).then(|| {
			let mut ids = vec![0u32; prepared.instance_count.max(1)];
			for (part, draw) in prepared.part_draws.iter().enumerate() {
				if let Some((instance, _)) = draw {
					ids[*instance as usize] = part as u32 + 1;
				}
			}
			let id_pipelines = self.id_pipelines();
			let group = id_pipelines.bind_group(device, &ids);
			ids::encode_part_ids(device, &mut encoder, id_pipelines, &group, &frame_group, &prepared.draws, width, height, &profile)
		});
		let layers = (request.layers && !keep).then(|| {
			let mut owners = vec![0u32; prepared.instance_count.max(1)];
			for (part, draw) in prepared.part_draws.iter().enumerate() {
				if let Some((instance, _)) = draw {
					owners[*instance as usize] = part as u32 + 1;
				}
			}
			for draw in prepared.draws.iter().filter(|draw| draw.decal) {
				if let Some(owner) = owners.get_mut(draw.instance as usize) {
					*owner = draw.decal_order.0 + 1;
				}
			}
			let id_pipelines = self.id_pipelines();
			let pipelines = self.layers.get_or_init(|| {
				let pipelines = ids::LayerPipelines::new(device, &self.pipelines.frame_layout, self.gpu.pipeline_cache.as_ref());
				self.gpu.save_pipeline_cache();
				pipelines
			});
			ids::encode_layers(device, &mut encoder, id_pipelines, pipelines, &owners, prepared.part_draws.len(), &frame_group, &prepared.draws, width, height, &profile)
		});
		let bytes_per_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
		let readback_size = bytes_per_row as u64 * height as u64;
		// Highlights and AlwaysOnTop GUIs finish on the CPU; convert their fully composited RGBA there.
		let compact_size = (width as u64 * height as u64 * 3 / 2).next_multiple_of(4);
		let compact = video.filter(|_| !keep && highlight_mask.is_none() && gui_layer.is_none() && compact_size <= device.limits().max_storage_buffer_binding_size as u64);
		let nv12 = compact.map(|video| self.nv12.get_or_init(|| {
			let conversion = nv12::Conversion::new(device, self.gpu.pipeline_cache.as_ref());
			self.gpu.save_pipeline_cache();
			conversion
		}).encode(&mut encoder, lease, &output_view, (width, height), video.flatten, &profile));
		let readback = (!keep && compact.is_none_or(|video| video.rgba)).then(|| {
			let readback = lease.empty_buffer("readback", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, readback_size);
			encoder.copy_texture_to_buffer(
				wgpu::TexelCopyTextureInfo { texture: &output, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
				wgpu::TexelCopyBufferInfo {
					buffer: &readback,
					layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(height) },
				},
				wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			);
			readback
		});
		let profile_readback = profile.resolve(device, &mut encoder);
		let commands = encoder.finish();
		let submission = self.gpu.submit([commands]);
		if let Some(capture) = capture {
			capture.end();
		}
		for PendingShadows { key, maps, texture } in publish {
			if self.shadows.insert(prepared.casters(), key, maps, texture.clone(), &self.reuse) {
				lease.hand_out(&texture);
			}
		}
		profile.report(&self.gpu, submission, profile_readback, width, height);
		stats.draw_ms += draw_start.elapsed().as_secs_f64() * 1000.0;
		if let Some(buffer) = &readback {
			// Render targets can be reused after submission; the staging buffer stays out until read.
			lease.forget_buffer(buffer);
		}
		if let Some(readback) = &nv12 {
			lease.forget_buffer(&readback.buffer);
		}
		let readback = readback.map(|buffer| ids::Readback { buffer, bytes_per_row });
		let highlight = highlight_mask.zip(prepared.highlights.as_ref().map(|highlight| highlight.styles.clone()));
		let image = ViewImage { width, height, rgba: Vec::new(), texture: keep.then_some(output), opaque_share: share_texture, camera, ids: None, layers: None, lod: None };
		Ok(PendingView::new(self, image, submission, readback, highlight, gui_layer, part_ids).with_layers(layers).with_video(nv12, video.is_some_and(|video| video.flatten)))
	}
}

trait With: Sized {
	fn with(mut self, f: impl FnOnce(&mut Self)) -> Self {
		f(&mut self);
		self
	}
}

impl With for FrameUniform {}

impl With for SkyUniform {}

/// The sun's and the moon's discs for the sky pass (shaders/sky.wgsl).
struct CelestialBodies {
	/// Direction and the tangent of half the angular size; w 0 when not drawn
	sun: [f32; 4],
	moon: [f32; 4],
	/// x the sun's weight, y the moon's
	weights: [f32; 4],
	sun_texture: Option<Arc<GpuTexture>>,
	moon_texture: Option<Arc<GpuTexture>>,
}

impl Renderer {
	/// The sun by day and the moon by night, when the Sky shows them (CelestialBodiesShown; without a
	/// Sky, Studio's default sky shows both).
	fn celestial_bodies(&self, lighting: &crate::scene::Environment, missing: &mut Vec<String>) -> CelestialBodies {
		let mut bodies = CelestialBodies { sun: [0.0; 4], moon: [0.0; 4], weights: [0.0; 4], sun_texture: None, moon_texture: None };
		let sky = lighting.sky.clone().unwrap_or_default();
		if !sky.celestial_bodies_shown {
			return bodies;
		}
		let optional = lighting.default_sky_unused();
		let mut load = |uri: &str| match self.load_whole(uri, TextureUse::Color) {
			Ok(texture) => Some(texture),
			Err(error) => {
				if !optional {
					missing.push(format!("{error:#}"));
				}
				None
			}
		};
		let sun = environment::sun_direction(lighting.clock_time, lighting.geographic_latitude);
		if sun.y > 0.0 && sky.sun_angular_size > 0.0 {
			bodies.weights[0] = match lighting.atmosphere {
				Some(atmosphere) => {
					let haze = environment::AtmosphereSky::new(&atmosphere, [Vec3::ZERO; 6], (sun, Vec3::ONE), lighting.brightness, Vec3::ONE).haze(sun);
					SUN_ATMOSPHERE_WEIGHT * lighting.brightness.max(0.0) * (1.0 - SUN_HAZE_LOSS * haze)
				}
				None => SUN_WEIGHT,
			};
			bodies.sun = [sun.x, sun.y, sun.z, (sky.sun_angular_size.min(179.0) * 0.5).to_radians().tan()];
			bodies.sun_texture = load(&sky.sun_texture);
		}
		let moon = environment::moon_direction(lighting.clock_time, lighting.geographic_latitude);
		// without a Sky the moon is a little smaller (sky-elements moon-default: its edges 6-7 px inside
		// those of a Sky's default MoonAngularSize 11, 350 px across)
		let moon_size = if lighting.sky.is_some() { sky.moon_angular_size } else { sky.moon_angular_size * 0.963 };
		if moon.y > 0.0 && moon_size > 0.0 {
			bodies.weights[1] = MOON_WEIGHT;
			bodies.moon = [moon.x, moon.y, moon.z, (moon_size.min(179.0) * 0.5).to_radians().tan()];
			bodies.moon_texture = load(&sky.moon_texture);
		}
		bodies
	}
}

/// The resolve's tone curve: 1 the default one, 2 ColorGradingEffect's Retro (TonemapperPreset 1;
/// the last enabled effect's preset counts; tone-retro).
fn tone_curve(environment: &crate::scene::Environment) -> f32 {
	let preset = environment.effects.iter().rev().find_map(|effect| match effect {
		crate::scene::environment::PostEffect::ColorGrading { tonemapper } => Some(*tonemapper),
		_ => None,
	});
	if preset == Some(1) { 2.0 } else { 1.0 }
}

/// Every ColorCorrectionEffect as one: Brightness, Contrast and Saturation added, TintColor (as it is,
/// on the shown values) multiplied (post-color "two", "two-tints"; shaders/resolve.wgsl). Returns
/// ([brightness, contrast, saturation, on], [tint, 1]).
fn color_correction(environment: &crate::scene::Environment) -> ([f32; 4], [f32; 4]) {
	let mut correction = [0.0f32; 4];
	let mut tint = Vec3::ONE;
	for effect in &environment.effects {
		if let crate::scene::environment::PostEffect::ColorCorrection { brightness, contrast, saturation, tint: color } = effect {
			correction[0] += brightness;
			correction[1] += contrast;
			correction[2] += saturation;
			correction[3] = 1.0;
			tint *= *color;
		}
	}
	(correction, [tint.x, tint.y, tint.z, 1.0])
}

/// The maps of a texture pack, as material texture URIs.
fn pack_maps(pack: &TexturePack) -> [Option<String>; 5] {
	[pack.color.clone(), pack.normal.clone(), pack.metalness.clone(), pack.roughness.clone(), pack.emissive.clone()]
}

/// Whether a pack lists this map (both set and naming the same asset).
fn same_map(listed: &Option<String>, map: &Option<String>) -> bool {
	let canonical = |uri: &String| ror_assets::ContentId::parse(uri).map(|id| id.canonical()).unwrap_or_else(|_| uri.clone());
	matches!((listed, map), (Some(listed), Some(map)) if canonical(listed) == canonical(map))
}

impl Renderer {
	/// A part's textures (SurfaceAppearance, MeshPart.TextureID, else its MaterialVariant or the
	/// built-in material's pack), how they apply, and its shading parameters.
	/// In a recording of play (Scene::play), a part whose SurfaceAppearance has a map that cannot be
	/// loaded, as Studio's running game draws it: without the appearance at all (a playtest
	/// 2026-10-16 logs "Unable to generate ... due to some of the textures being unavailable" and draws
	/// the bare MeshPart, where edit mode hides the part). The part to draw instead and why.
	fn playable(&self, part: &Part, material: &PartMaterial, play: bool) -> Option<(Part, String)> {
		if !play || part.surface_appearance.is_none() {
			return None;
		}
		let error = material.key.textures().find_map(|(uri, usage)| self.load_texture(uri, usage).err())?;
		let mut plain = part.clone();
		plain.surface_appearance = None;
		Some((plain, format!("{}: {error:#} (its SurfaceAppearance cannot be generated: drawn without it, as a running game does)", part.path)))
	}

	fn part_material(&self, part: &Part, materials: &Materials, missing: &mut Vec<String>) -> PartMaterial {
		// a SurfaceAppearance turns Neon's glow off: the part is lit as Plastic is, in its appearance's
		// maps (particle-sa-blend: an effect's dark gray Neon cones in an orange appearance read exactly as
		// their Plastic copy, at Transparency 0 and 0.1, in AlphaMode Transparency and Overlay, white Neon
		// as a lit gray; ror drew them at 4-5 times that light)
		let shading_material = if part.material == materials::NEON && part.surface_appearance.is_some() { materials::PLASTIC } else { part.material };
		let params = materials::params(shading_material);
		let mut material = PartMaterial {
			key: MaterialKey::default(),
			flags: 0,
			params,
			studs_per_tile: materials::BUILT_IN_STUDS_PER_TILE,
			emissive: Vec3::ZERO,
			offset: [0.0; 2],
			noise_offset: [0.0; 2],
			noise_frequency: materials::VARIANT_ORGANIC_NOISE,
			normal_strength: 1.0,
			normal_falloff: (1.0, 0.0),
			lobe: (0.0, 0.0),
			albedo: 1.0,
			far: [0.0; 2],
			pack_proofs: Vec::new(),
		};
		if params.shading == materials::SHADING_FORCEFIELD {
			// a ForceField's TextureID only shows as a band sweeping through its texel values over
			// time (forcefield-time); between sweeps, the state drawn here, it looks untextured
			return material;
		}
		let composite_key = part.composite.as_ref().map(composite::key).filter(|key| self.composite_loads(key, &part.path, missing));
		let mut pack_proofs: Vec<(String, resources::StampProof)> = Vec::new();
		let mut pack_or = |pack: Option<&String>, maps: [Option<String>; 5]| -> [Option<String>; 5] {
			// the per-map properties win; a published pack fills in when they are all empty
			if maps.iter().any(Option::is_some) {
				return maps;
			}
			match pack.map(|uri| (uri, self.texture_pack(uri))) {
				Some((uri, Ok(pack))) => {
					if names_local_file(uri) {
						pack_proofs.push((uri.clone(), pack.proof.clone()));
					}
					pack_maps(&pack)
				}
				Some((_, Err(error))) => {
					missing.push(format!("{}: {error:#}", part.path));
					maps
				}
				None => maps,
			}
		};
		if let Some(appearance) = &part.surface_appearance {
			// Studio draws a SurfaceAppearance with a TexturePack from the pack's transcoded layers,
			// not its per-map properties (after a ship close-up Roblox's HTTP cache holds every
			// pack of its packs' layers and none of its per-map images): colour in gamma 2 (decoded
			// with the square, which on the uploaded texels is the sRGB curve), a DXT5nm normal,
			// metalness and roughness in the red and green of one texture
			let layers = appearance.texture_pack.as_ref().filter(|_| transcode::enabled()).and_then(|pack| self.pack_layers(pack).map(|key| (pack, key)));
			if let Some((pack, key)) = layers {
				material.key = key;
				material.flags |= TEX_COLOR | TEX_ROUGHNESS_GREEN;
				// the pack's third layer holds its emissive map in blue, as the built-in packs do (Time
				// Goddess's clock: the blue is the mask's red channel, rms 1.9 of 255)
				if appearance.emissive_mask.is_some() && self.texture_pack(pack).is_ok_and(|xml| xml.emissive.is_some()) {
					material.key.emissive = material.key.roughness.clone();
					material.flags |= TEX_EMISSIVE_BLUE;
				}
			} else {
				// the maps one by one, each its image transcode (a pack's own when the properties are empty)
				let [color, normal, metalness, roughness, pack_emissive] =
					pack_or(appearance.texture_pack.as_ref(), [appearance.color_map.clone(), appearance.normal_map.clone(), appearance.metalness_map.clone(), appearance.roughness_map.clone(), None])
						.map(|map| map.map(|uri| transcode::image_key(&uri)));
				let emissive = appearance.emissive_mask.as_ref().and_then(|mask| pack_emissive.or_else(|| Some(transcode::image_key(mask))));
				// a mask image glows by its red channel alone, as a pack's third layer holds it (sa-emission-sweep:
				// the portal atlas, red 55 and blue 191 on average, glows gray under a white map, Studio 109 against
				// ror's 116 / 39 / 211; a uniform (76, 18, 30) image as one of 76)
				if emissive.is_some() {
					material.flags |= TEX_EMISSIVE_RED;
				}
				material.key = MaterialKey { color, normal, metalness, roughness, emissive, clamped: false };
				if material.key.color.is_some() {
					// sRGB like the material maps (close-up comparisons: within 8 of 255 on 78 % of
					// a ship exterior's pixels instead of 70 %, 92 % instead of 88 % on a character);
					// EditableImage snapshots decode with the square (fixtures/vertex-colour-maps.luau:
					// saov-tan 0.615 / 0.347 / 0.156 against 0.621 / 0.350 / 0.155 squared, 0.586 /
					// 0.309 / 0.129 sRGB)
					material.flags |= TEX_COLOR;
					if !material.key.color.as_deref().is_some_and(is_editable_image_uri) {
						material.flags |= TEX_SRGB;
					}
				}
			}
			// the mask glows as a MaterialVariant's does (materials::VARIANT_EMISSION), in the appearance's
			// colour (its map and Color, squared) rather than the part's (sa-emission, in darkness: masks of
			// 255, 130 and 76 at strength 1 on a white map emit 1.937, 0.98 and 0.58, strength 0.5 0.966, a
			// tint (1, 0.5, 0.25) as stored, Color (1, 0.5, 0.25) squared, a map of 130 decoded with the sRGB
			// curve 0.434; a gray part under the white map as a white one)
			if appearance.emissive_mask.is_some() {
				// in AlphaMode Overlay and TintMask the shader takes Color with the overlaid or tinted colour
				// (mesh.wgsl)
				let color = if matches!(appearance.alpha_mode, 0 | 2) && material.key.color.is_some() { Vec3::ONE } else { linear(appearance.color) };
				material.emissive = color * appearance.emissive_tint * appearance.emissive_strength * materials::VARIANT_EMISSION;
			}
			if material.key.color.is_some() {
				material.flags |= match appearance.alpha_mode {
					0 => TEX_ALPHA_OVERLAY,
					1 => TEX_ALPHA_TRANSPARENCY,
					2 => TEX_ALPHA_TINTMASK,
					_ => 0,
				};
			}
			// without a roughness map it reflects as at 127 / 255, a TexturePack's own constant where it
			// has no roughness map, not as its part material (sa-sun sa_nomap on a Plastic MeshPart: log
			// ratio rms 0.087 over 20 bins at 0.498, 0.38 at Plastic's 0.6)
			if material.key.roughness.is_none() {
				material.params.roughness = materials::SA_DEFAULT_ROUGHNESS;
			}
		} else if let Some(key) = composite_key {
			// a dressed character's body part draws its composited texture: the body colours under
			// the pants, shirt and T-shirt, a colour texture (decoded with a square); without it
			// (a template failed) the body colour
			material.key.color = Some(key);
			material.flags |= TEX_COLOR | TEX_ALPHA_OVERLAY;
		} else if let Some(texture) = &part.texture {
			material.key.color = Some(transcode::image_key(texture));
			// a MeshPart's or SpecialMesh's texture replaces its colour, Color showing nowhere (a MeshPart's
			// vertex colours tint it: TEX_VERTEX_TINT); its alpha is the part's coverage only past
			// OPAQUE_TRANSPARENCY (TEX_ALPHA_IGNORED, by instance)
			// TODO(parity): square or sRGB? (no measurable difference on the models tried)
			material.flags |= TEX_COLOR;
		} else if !matches!(part.geometry, Geometry::Special { .. }) {
			// material textures: the part's MaterialVariant (or the base material's override), else
			// the built-in material's pack; the colour map's texels sRGB-encoded (PNGs) or gamma 2
			// (transcoded layers)
			let mut srgb = true;
			let maps = if let Some(variant) = materials.variant(part.material, &part.material_variant) {
				material.studs_per_tile = variant.studs_per_tile.max(1e-3);
				if variant.pattern == 1 {
					material.flags |= TEX_ORGANIC;
				}
				// the mask glows in the part colour times EmissiveTint (as stored) and EmissiveStrength
				// (materials::VARIANT_EMISSION)
				material.emissive = linear(part.color) * variant.emissive_tint * variant.emissive_strength * materials::VARIANT_EMISSION;
				// Studio draws a variant's maps and only those, whatever its TexturePack (variant-pack: a
				// pack alone draws the plain part, a pack next to another colour map that map, a pack's
				// emissive map nothing without the variant's mask); each map the TexturePack lists too
				// draws as the pack's transcoded layer, as Roblox's HTTP cache shows after a large place
				// comparison (CircleGrid's layers, not its images). variant-pack: Brick's colour map with
				// the pack listing it within 2 of 255 on 99.0 % of pixels from the layer, 95.5 % from the
				// PNG; organic-sun: Slate's maps 100.0 / 99.9 % flat / sun-lit from the layers of the pack
				// listing them, 83.6 / 77.6 % from the PNGs. Maps no pack lists Studio makes a pack of,
				// which ror cannot fetch: their PNGs then
				let maps = [variant.color_map.clone(), variant.normal_map.clone(), variant.metalness_map.clone(), variant.roughness_map.clone(), variant.emissive_mask.clone()];
				let pack = variant.texture_pack.as_ref().filter(|_| transcode::enabled() && maps.iter().any(Option::is_some));
				let listed = pack.and_then(|pack| self.texture_pack(pack).ok()).map(|xml| pack_maps(&xml));
				let layers = pack.filter(|_| listed.as_ref().is_some_and(|listed| (0..5).any(|index| same_map(&listed[index], &maps[index])))).and_then(|pack| self.material_layers(pack));
				match (layers, listed) {
					(Some(layers), Some(listed)) => {
						let layers = [layers.color, layers.normal, layers.metalness, layers.roughness, layers.emissive];
						let from_pack: [bool; 5] = std::array::from_fn(|index| same_map(&listed[index], &maps[index]));
						srgb = !from_pack[0];
						if from_pack[3] {
							material.flags |= TEX_ROUGHNESS_GREEN;
						}
						if from_pack[4] {
							material.flags |= TEX_EMISSIVE_BLUE;
						}
						std::array::from_fn(|index| if from_pack[index] { layers[index].clone() } else { maps[index].clone() })
					}
					_ => maps,
				}
			} else if part.body_part.is_some() && part.material == 256 {
				// a Humanoid's Plastic body part shows no Plastic texture (humanoid-parts: R6 "Torso"
				// has no grain under a low sun, high-pass 0.21 against a "Block" beside it 6.70; R15's
				// limbs in the R15 compare); other materials keep theirs ("Left Arm" in Brick)
				[None, None, None, None, None]
			} else if let Some(textures) = materials::built_in_textures(part.material, materials.use_2022) {
				material.studs_per_tile = textures.studs_per_tile;
				if textures.random_offset {
					material.offset = part_offset(part);
				}
				if textures.wang && self.defaults.wang {
					material.flags |= TEX_WANG;
				}
				if let Some(far) = textures.far {
					material.far = [far.scale, 1.0 / far.depth];
				}
				if let Some(frequency) = textures.organic {
					material.flags |= TEX_ORGANIC;
					// Leather and Plaster: frequency 0 and no random offsets (materials::ORGANIC_FIXED)
					if textures.random_offset {
						material.noise_offset = part_noise_offset(part);
					}
					material.noise_frequency = frequency;
					// offsets measured from a Studio capture, for seeded comparisons
					if let Some([noise_u, noise_v, texture_u, texture_v]) = measured_organic_offsets().get(part.path.rsplit('.').next().unwrap_or_default()) {
						material.noise_offset = [*noise_u, *noise_v];
						material.offset = [*texture_u, *texture_v];
					}
				}
				// CrackedLava's emissive map glows in the part colour (materials::EMISSIVE_STRENGTH)
				material.emissive = linear(part.color) * materials::EMISSIVE_STRENGTH;
				let pack = format!("rbxassetid://{}", textures.pack);
				// the pack's transcoded layers, as Studio draws them; its PNGs, which match them, where
				// the layers do not load (materials::PACKS_2022)
				let mut maps = match transcode::enabled().then(|| self.material_layers(&pack)).flatten() {
					Some(layers) => {
						srgb = false;
						material.flags |= TEX_ROUGHNESS_GREEN | TEX_EMISSIVE_BLUE;
						[layers.color, layers.normal, layers.metalness, layers.roughness, layers.emissive]
					}
					None => match materials::public_maps(textures.pack) {
						// a pack only the account can read lists public maps
						Some(maps) if self.texture_pack(&pack).is_err() => maps.map(|map| map.map(|id| format!("rbxassetid://{id}"))),
						_ => pack_or(Some(&pack), [None, None, None, None, None]),
					},
				};
				if textures.skip_color {
					maps[0] = None;
					material.albedo = materials::untextured_level(part.material, materials.use_2022);
				}
				// the colour map's alpha masks where the part colour tints it (alphamode 2, every 2022
				// pack): Brick's mortar and CrackedLava's cracks keep their own colour on a coloured part
				// (material-tint). Glass's colour map (alpha 0.002 on average) is tinted all over.
				if params.shading != materials::SHADING_GLASS && self.texture_pack(&pack).is_ok_and(|pack| pack.alpha_mode == 2) {
					material.flags |= TEX_ALPHA_TINTMASK;
				}
				material.normal_strength = textures.normal_strength;
				material.normal_falloff = textures.normal_falloff;
				material.lobe = textures.lobe;
				maps
			} else {
				[None, None, None, None, None]
			};
			let [color, normal, metalness, roughness, emissive] = maps;
			material.key = MaterialKey { color, normal, metalness, roughness, emissive, clamped: false };
			if material.key != MaterialKey::default() {
				material.flags |= TEX_MATERIAL;
				if material.key.color.is_some() {
					material.flags |= TEX_COLOR;
				}
				if material.key.color.is_some() && srgb {
					material.flags |= TEX_SRGB;
				}
			} else {
				material.flags &= !(TEX_ORGANIC | TEX_WANG);
			}
		}
		let key = &material.key;
		if key.normal.is_some() {
			material.flags |= TEX_NORMAL;
		}
		if key.metalness.is_some() {
			material.flags |= TEX_METALNESS;
		}
		if key.roughness.is_some() {
			material.flags |= TEX_ROUGHNESS;
		}
		if key.emissive.is_some() {
			material.flags |= TEX_EMISSIVE;
		}
		if matches!(part.geometry, Geometry::Union { use_part_color: false, .. } | Geometry::Collision { .. }) {
			material.flags |= TEX_VERTEX_COLOR;
		}
		// FileMesh vertex colours on MeshParts tint the part colour; under a SurfaceAppearance only in
		// AlphaMode Overlay (the colour its map reveals, with or without a ColorMap), and nothing in
		// Transparency or TintMask, where the appearance's Color and map make the colour
		// (fixtures/vertex-colour-maps.luau in a Roblox 0.747 client: EditableMesh quads under
		// EditableImage maps; docs/materials.md "MeshPart colour")
		let tinted = match &part.surface_appearance {
			None => true,
			Some(appearance) => appearance.alpha_mode == 0,
		};
		if matches!(part.geometry, Geometry::MeshPart { .. }) && tinted {
			material.flags |= TEX_VERTEX_TINT;
		}
		material.pack_proofs = pack_proofs;
		material
	}
}

/// The instance's specular parameters: dielectric F0, Fresnel strength and visibility mode
/// (`ROR_TUNE_SPEC="roughness,f0,fresnel"` / `ROR_TUNE_VIS=<mode>` override them for untextured
/// dielectrics while fitting).
fn specular_params(params: &MaterialParams) -> [f32; 4] {
	let mode = tune_visibility().unwrap_or(params.visibility as f32);
	match tune_spec() {
		Some((_, f0, fresnel)) if params.metalness == 0.0 => [f0, fresnel, mode, 0.0],
		_ => [params.f0, params.fresnel, mode, 0.0],
	}
}

/// Four numbers from an environment variable ("a,b,c,d"), for the laws' fits.
fn tuned_four(name: &str) -> Option<[f32; 4]> {
	let values: Vec<f32> = std::env::var(name).ok()?.split(',').filter_map(|part| part.trim().parse::<f32>().ok()).collect();
	values.try_into().ok()
}

/// The direct specular's lobe for the shader's `direct_light` (`Frame.direct_specular`):
/// `ROR_TUNE_DIRECT_SPEC="w,width,k0,k1"` overrides [`materials::DIRECT_SPECULAR`] for fits
/// (tools/analysis/direct_specular.py).
fn direct_specular() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| tuned_four("ROR_TUNE_DIRECT_SPEC").unwrap_or(materials::DIRECT_SPECULAR))
}

/// The direct specular's reflectance for the shader's `direct_light` (`Frame.direct_reflectance`):
/// `ROR_TUNE_DIRECT_REFLECTANCE="g,c,fresnel,compensation"` overrides [`materials::DIRECT_REFLECTANCE`].
fn direct_reflectance() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| tuned_four("ROR_TUNE_DIRECT_REFLECTANCE").unwrap_or(materials::DIRECT_REFLECTANCE))
}

/// The direct specular's metallic share for the shader's `direct_light` (`Frame.direct_metal`):
/// `ROR_TUNE_DIRECT_METAL="a,b,width,separate"` overrides [`materials::DIRECT_METAL`] for fits.
fn direct_metal() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| tuned_four("ROR_TUNE_DIRECT_METAL").unwrap_or(materials::DIRECT_METAL))
}

/// The sky specular's colour for the shader (`Frame.sky_specular_colour`):
/// `ROR_TUNE_SKY_SPECULAR_COLOUR="r,g,b"` overrides [`materials::SKY_SPECULAR_COLOUR`] for fits.
fn sky_specular_colour() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| {
		let tuned: Option<[f32; 3]> = std::env::var("ROR_TUNE_SKY_SPECULAR_COLOUR").ok().and_then(|text| text.split(',').filter_map(|part| part.trim().parse::<f32>().ok()).collect::<Vec<f32>>().try_into().ok());
		let [r, g, b] = tuned.unwrap_or(materials::SKY_SPECULAR_COLOUR);
		[r, g, b, 0.0]
	})
}

/// The sky specular and metalness fade for the shader (`Frame.specular_fade`):
/// `ROR_TUNE_SPECULAR_FADE="level depth,metalness depth,level,metalness"` overrides
/// [`materials::SPECULAR_FADE`] for fits.
fn specular_fade() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| {
		let tuned = std::env::var("ROR_TUNE_SPECULAR_FADE").ok().map(|text| text.split(',').filter_map(|part| part.trim().parse::<f32>().ok()).collect::<Vec<f32>>());
		let [level_depth, metalness_depth, level, metalness] = match tuned {
			Some(values) if values.len() == 4 => [values[0], values[1], values[2], values[3]],
			_ => materials::SPECULAR_FADE,
		};
		[1.0 / level_depth, 1.0 / metalness_depth, level, metalness]
	})
}

fn tune_spec() -> Option<(f32, f32, f32)> {
	static TUNE: std::sync::OnceLock<Option<(f32, f32, f32)>> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| {
		let text = std::env::var("ROR_TUNE_SPEC").ok()?;
		let values: Vec<f32> = text.split(',').filter_map(|part| part.trim().parse().ok()).collect();
		(values.len() == 3).then(|| (values[0], values[1], values[2]))
	})
}

/// A content URI as the renderer's caches know it: a local file's URI with its length and modified
/// time appended, so a file rewritten under the same path is loaded again (the render service lives on
/// between jobs; ror-assets re-reads such a file by the same stamp). Other URIs as they are.
/// Whether `uri` names a local file ([`content_key`] stamps its key with the file's).
pub(crate) fn names_local_file(uri: &str) -> bool {
	if uri.starts_with(composite::KEY_PREFIX) {
		return composite::has_local_file(uri);
	}
	let path = uri.split_once('#').map_or(uri, |(path, _)| path);
	if let Some((scheme, _)) = path.split_once(':')
		&& scheme.len() > 1
		&& !scheme.contains(['/', '\\'])
		&& !scheme.eq_ignore_ascii_case("file")
	{
		return false;
	}
	matches!(ror_assets::ContentId::parse(path), Ok(ror_assets::ContentId::File(_)))
}

/// If `uri` (stripped of any `#fragment`) names a local file, takes its [`ror_assets::FileSnapshot`].
pub(crate) fn local_snapshot_for(uri: &str) -> Option<(std::path::PathBuf, ror_assets::FileSnapshot)> {
	if uri.starts_with(composite::KEY_PREFIX) {
		return None;
	}
	let path = uri.split_once('#').map_or(uri, |(path, _)| path);
	if let Some((scheme, _)) = path.split_once(':')
		&& scheme.len() > 1
		&& !scheme.contains(['/', '\\'])
		&& !scheme.eq_ignore_ascii_case("file")
	{
		return None;
	}
	match ror_assets::ContentId::parse(path) {
		Ok(ror_assets::ContentId::File(file)) => ror_assets::FileStamp::snapshot(&file).ok().map(|snap| (file, snap)),
		_ => None,
	}
}

/// Whether a loaded resource's [`resources::StampProof`] agrees with the stamped keys recorded in a
/// `SceneKey` or `effect_hash`.
fn proof_matches(uri: &str, proof: &resources::StampProof, local_keys: &HashMap<String, String>) -> bool {
	match proof {
		resources::StampProof::Remote => !names_local_file(uri),
		resources::StampProof::Moved => false,
		resources::StampProof::Proven(actual) => {
			if uri.starts_with(composite::KEY_PREFIX) {
				composite::expected_stamped_key(uri, local_keys).is_some_and(|expected| expected == *actual)
			} else {
				local_keys.get(uri).is_some_and(|expected| expected == actual)
			}
		}
	}
}

#[cfg(test)]
type ReadHook = Box<dyn FnMut() + Send>;

#[cfg(test)]
static TEST_READ_HOOKS: std::sync::OnceLock<Mutex<HashMap<std::path::PathBuf, ReadHook>>> = std::sync::OnceLock::new();

/// Registers a one-shot hook that runs deterministically after stamping a local file and right
/// before its bytes are read, for race tests (no sleeps).
#[cfg(test)]
pub(crate) fn set_test_read_hook(path: std::path::PathBuf, hook: impl FnMut() + Send + 'static) {
	TEST_READ_HOOKS.get_or_init(Default::default).lock().insert(path, Box::new(hook));
}

#[cfg(test)]
pub(crate) fn run_test_read_hook(path: &std::path::Path) {
	let Some(hooks) = TEST_READ_HOOKS.get() else {
		return;
	};
	let hook = hooks.lock().remove(path);
	if let Some(mut f) = hook {
		f();
	}
}

#[cfg(not(test))]
#[inline]
pub(crate) fn run_test_read_hook(_path: &std::path::Path) {}

pub(crate) fn content_key(uri: &str) -> String {
	content_key_checked(uri).0
}

/// Like [`content_key`], also returning `true` when the file had to be hashed in full because its
/// volume provided no trusted OS change token.
pub(crate) fn content_key_checked(uri: &str) -> (String, bool) {
	if uri.starts_with(composite::KEY_PREFIX) {
		let (keyed, untrusted) = composite::stamped_key_checked(uri);
		return (keyed, untrusted > 0);
	}
	let path = uri.split_once('#').map_or(uri, |(path, _)| path);
	// a URI with a scheme other than file: (rbxassetid://, a transcode's or a composite's key) names no
	// local file, and parsing a key no scheme matches looks for one on disk: 36,500 file probes in the
	// first frame of a 5,000-part map, 220 in every frame after it
	if let Some((scheme, _)) = path.split_once(':')
		&& scheme.len() > 1
		&& !scheme.contains(['/', '\\'])
		&& !scheme.eq_ignore_ascii_case("file")
	{
		return (uri.to_string(), false);
	}
	match ror_assets::ContentId::parse(path) {
		Ok(ror_assets::ContentId::File(file)) => match ror_assets::FileStamp::of(&file) {
			Some(stamp) => (stamp.stamp_uri(uri), stamp.is_untrusted_hash()),
			None => (uri.to_string(), false),
		},
		_ => (uri.to_string(), false),
	}
}

/// The index range a mesh draws: LOD0, or `ROR_TUNE_LOD=<level>` while measuring Studio's LODs.
fn draw_lod(mesh: &GpuMesh) -> Range<u32> {
	static TUNE: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
	let level = TUNE.get_or_init(|| std::env::var("ROR_TUNE_LOD").ok().and_then(|value| value.parse::<usize>().ok())).unwrap_or(0);
	mesh.lods.get(level.min(mesh.lods.len().saturating_sub(1))).cloned().unwrap_or(0..0)
}

fn tune_visibility() -> Option<f32> {
	static TUNE: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| std::env::var("ROR_TUNE_VIS").ok().and_then(|value| value.parse::<f32>().ok()))
}

/// Built-in Organic parts' offsets measured from a Studio capture, for seeded comparisons:
/// `ROR_ORGANIC_OFFSETS` names a JSON file of part name → [noise u, noise v, texture u, texture v]
/// (`tools/analysis/organic.py offsets --json`). Studio draws them at random per part and load.
fn measured_organic_offsets() -> &'static HashMap<String, [f32; 4]> {
	static OFFSETS: std::sync::OnceLock<HashMap<String, [f32; 4]>> = std::sync::OnceLock::new();
	OFFSETS.get_or_init(|| {
		let Some(text) = std::env::var_os("ROR_ORGANIC_OFFSETS").and_then(|path| std::fs::read_to_string(path).ok()) else {
			return HashMap::new();
		};
		let parsed: HashMap<String, Vec<f32>> = serde_json::from_str(&text).unwrap_or_default();
		parsed.into_iter().filter(|(_, values)| values.len() >= 4).map(|(name, values)| (name, [values[0], values[1], values[2], values[3]])).collect()
	})
}

/// Axis-aligned bounds of a box after an affine transform.
/// Each part's drawn box in its own space (its mesh's bounds through the mesh's placement), `None`
/// where the mesh failed to load (the part's box is drawn then).
/// What a scene's effects draw at the rendered moment, for framing one that draws no part: the live
/// particles, the beams' and trails' ends, and (nothing alive yet) where the emitters are; at least a
/// stud across, so a lone emitter still frames.
fn effect_bounds(scene: &Scene, particles: &particles::PreparedParticles) -> Option<(Vec3, Vec3)> {
	let mut points: Vec<Vec3> = scene.beams.iter().flat_map(|beam| [Vec3::from(beam.attachment0.translation), Vec3::from(beam.attachment1.translation)]).collect();
	points.extend(scene.trails.iter().flat_map(|trail| [Vec3::from(trail.cframe0.translation), Vec3::from(trail.cframe1.translation)]));
	let mut bounds = particles.bounds();
	if bounds.is_none() && points.is_empty() {
		points.extend(scene.particles.iter().map(|emitter| Vec3::from(emitter.cframe.translation)));
	}
	for point in points {
		bounds = Some(bounds.map_or((point, point), |(low, high)| (low.min(point), high.max(point))));
	}
	bounds.map(|(low, high)| {
		let middle = (low + high) * 0.5;
		let half = ((high - low) * 0.5).max(Vec3::splat(0.5));
		(middle - half, middle + half)
	})
}

/// The parts a clip's frame prepares on its own thread ([`crate::par`]): 250 parts prepared again every frame
/// (a map's skinned tufts of grass, before a skinned part was kept from the last frame) took 1.3 ms there and
/// as long on the pool's eight threads, which spent 12 ms of CPU on them waiting for the texture cache's lock
/// and each other.
const CLIP_PARTS: usize = 1024;

/// `f` of each part of `scene`, in order: on the calling thread for a live scene (`Scene::live`),
/// across rayon's threads otherwise. A live scene's files are loaded by its first render (or a
/// prefetch), so each later lookup is a cache hit: handing a character's 51 parts to
/// rayon's threads took ~0.2 ms a pass against 0.01-0.03 here, its preview's frames 2.2 ms against 1.3.
/// A clip's frames keep the few parts that changed to their thread too ([`crate::par`]).
fn each_part<T: Send>(scene: &Scene, f: impl Fn(usize, &Part) -> T + Sync) -> Vec<T> {
	if scene.live || !crate::par::spread(scene.parts.len(), CLIP_PARTS) {
		scene.parts.iter().enumerate().map(|(index, part)| f(index, part)).collect()
	} else {
		scene.parts.par_iter().enumerate().map(|(index, part)| f(index, part)).collect()
	}
}

/// `f` of the parts of `scene` at `positions`, in their order, as [`each_part`].
fn each_of<T: Send>(scene: &Scene, positions: &[usize], f: impl Fn(usize, &Part) -> T + Sync) -> Vec<T> {
	if scene.live || !crate::par::spread(positions.len(), CLIP_PARTS) {
		positions.iter().map(|&index| f(index, &scene.parts[index])).collect()
	} else {
		positions.par_iter().map(|&index| f(index, &scene.parts[index])).collect()
	}
}

/// What a part's preparation reads of its scene beyond the part (its key: `prepared::part_context`,
/// with the scene's MaterialService).
struct PartContext {
	neon_share: f32,
	/// In a ViewportFrame: MeshParts draw their vertex alpha
	viewport: bool,
	/// The surface types' atlas loaded
	surface_maps: bool,
	/// A recording of play: a SurfaceAppearance map that cannot be loaded is left out (Scene::play)
	play: bool,
}

fn drawn_boxes(meshes: &[Result<(Arc<GpuMesh>, Affine3A, bool)>]) -> Vec<Option<(Vec3, Vec3)>> {
	meshes.iter().map(|mesh| mesh.as_ref().ok().map(|(mesh, local, _)| transformed_bounds(local, mesh.drawn))).collect()
}

fn transformed_bounds(transform: &Affine3A, (min, max): (Vec3, Vec3)) -> (Vec3, Vec3) {
	let mut low = Vec3::splat(f32::INFINITY);
	let mut high = Vec3::splat(f32::NEG_INFINITY);
	for index in 0..8 {
		let corner = Vec3::new(
			if index & 1 == 0 { min.x } else { max.x },
			if index & 2 == 0 { min.y } else { max.y },
			if index & 4 == 0 { min.z } else { max.z },
		);
		let point = transform.transform_point3(corner);
		low = low.min(point);
		high = high.max(point);
	}
	(low, high)
}

fn fnv1a(bytes: &[u8]) -> u64 {
	let mut hash = 0xcbf29ce484222325u64;
	for &byte in bytes {
		hash ^= byte as u64;
		hash = hash.wrapping_mul(0x100000001b3);
	}
	hash
}

/// f32 → IEEE half (round to nearest, no denormal handling needed for colour data).
fn half(value: f32) -> u16 {
	let bits = value.to_bits();
	let sign = ((bits >> 16) & 0x8000) as u16;
	let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
	let mantissa = bits & 0x7f_ffff;
	if exponent <= 0 {
		return sign;
	}
	if exponent >= 31 {
		return sign | 0x7c00;
	}
	let rounded = (exponent as u32) << 10 | (mantissa + 0x1000) >> 13;
	sign | rounded.min(0x7bff) as u16
}

#[cfg(test)]
mod sky_tests {
	use super::*;

	/// Clips' frames ask for their sky's maps on rayon's threads (each clip's producer; a service draws
	/// several clips at once). When the skybox cannot load, each falls back to the gradient sky, made
	/// once: the thread making it must not take on another frame that waits for it (made on the
	/// frame's worker, the first round hung).
	#[test]
	fn frames_whose_skybox_fails_share_the_gradient_sky() {
		let state = std::env::temp_dir().join(format!("ror-sky-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Arc::new(Renderer::new(gpu, assets).expect("a renderer"));
		let mut scene = Scene::default();
		scene.environment.sky = Some(crate::scene::environment::Sky { skybox: std::array::from_fn(|face| format!("rbxasset://textures/sky/missing-{face}.tex")), ..Default::default() });
		let (sender, receiver) = std::sync::mpsc::channel();
		let rounds = 200;
		std::thread::spawn(move || {
			for _ in 0..rounds {
				// the gradient sky made again each round, asked for by several clips' producers at once
				renderer.environments.shrink_to(0);
				let fallbacks: usize = std::thread::scope(|scope| {
					let clips: Vec<_> = (0..8)
						.map(|_| scope.spawn(|| (0..4).into_par_iter().filter(|_| renderer.prepare_environment(&scene).missing.iter().any(|reason| reason.contains("gradient sky"))).count()))
						.collect();
					clips.into_iter().map(|clip| clip.join().unwrap_or(0)).sum()
				});
				let _ = sender.send(fallbacks);
			}
		});
		for round in 0..rounds {
			let fallbacks = receiver.recv_timeout(std::time::Duration::from_secs(60)).unwrap_or_else(|_| panic!("round {round}: the frames waited on each other for the gradient sky"));
			assert_eq!(fallbacks, 32);
		}
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod gate_tests {
	use super::*;

	/// A draw's targets as `target_sets` estimates them, against what the pool holds after the draw, and
	/// draws admitted by the gate drawing what they draw without it.
	#[test]
	fn a_draw_takes_no_more_than_its_targets_estimate_and_the_gate_changes_no_pixel() {
		let state = std::env::temp_dir().join(format!("ror-gate-test-{}", std::process::id()));
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: glam::Vec3::new(8.0, 1.0, 8.0), ..Default::default() });
		let scene = crate::scene::extract::extract(&model.into_document());
		let camera = CameraSpec::LookAt { eye: [12.0, 10.0, -12.0], target: [0.0; 3], fov: 70.0 };
		let views = [ViewRequest { width: 640, height: 360, camera: camera.clone(), ..Default::default() }, ViewRequest { width: 320, height: 320, supersample: 2, camera, ..Default::default() }];
		let sets = renderer.target_sets(&views);
		assert_eq!(sets.iter().map(|set| set.key).collect::<Vec<_>>(), [(640, 360, 4), (640, 640, 4)]);
		assert!(sets.iter().all(|set| !set.kept));
		let gate = crate::budget::GpuGate::default();
		let (gated, _) = renderer.render_gated(&scene, &views, Some(&gate)).unwrap();
		let (kept_textures, kept_bytes, _, _) = renderer.reuse.stats();
		let estimate: u64 = sets.iter().map(|set| set.bytes).sum();
		assert!(kept_textures > 0 && kept_bytes <= estimate, "the draw kept {kept_bytes} bytes of targets, estimated {estimate}");
		assert!(renderer.target_sets(&views).iter().all(|set| set.kept), "both sizes' sets kept for the next draw");
		assert_eq!((gate.status()["admitted"].as_u64(), gate.status()["drawing"].as_u64()), (Some(1), Some(0)));
		let (plain, _) = renderer.render(&scene, &views).unwrap();
		assert!(gated.iter().zip(&plain).all(|(gated, plain)| gated.rgba == plain.rgba), "the same pixels through the gate");
		let _ = std::fs::remove_dir_all(&state);
	}

	/// `relieve` while a draw runs (a draw short of GPU memory beside a game, a busy service's check each
	/// second) keeps the scratch targets the draw holds, so a draw beside it takes them and makes no second
	/// set, and what it let go of is made again for the next draw with the same pixels.
	#[test]
	fn relieving_keeps_the_targets_a_draw_holds_and_changes_no_pixel() {
		let state = std::env::temp_dir().join(format!("ror-relieve-test-{}", std::process::id()));
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: glam::Vec3::new(8.0, 1.0, 8.0), ..Default::default() });
		let scene = crate::scene::extract::extract(&model.into_document());
		let views = [ViewRequest { width: 320, height: 180, camera: CameraSpec::LookAt { eye: [12.0, 10.0, -12.0], target: [0.0; 3], fov: 70.0 }, ..Default::default() }];
		let (before, _) = renderer.render(&scene, &views).unwrap();
		assert!(renderer.target_sets(&views).iter().all(|set| set.kept));
		let target = wgpu::TextureDescriptor {
			label: Some("held"),
			size: wgpu::Extent3d { width: 48, height: 16, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::Rgba16Float,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
			view_formats: &[],
		};
		let drawing = renderer.reuse.lease(&renderer.gpu);
		let held = drawing.scratch(&target);
		assert!(renderer.relieve(u64::MAX) > 0);
		assert!(renderer.target_sets(&views).iter().all(|set| !set.kept), "the targets no draw holds were let go of");
		assert!(renderer.reuse.lease(&renderer.gpu).scratch(&target) == held, "a draw beside the one holding a target made a second");
		drop(drawing);
		let (after, _) = renderer.render(&scene, &views).unwrap();
		assert!(before.iter().zip(&after).all(|(before, after)| before.rgba == after.rgba), "other pixels once what relieve let go of was made again");
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod decal_uv_tests {
	use super::*;

	/// A Decal's UVOffset and UVScale map the face's uv to its image's as uv * UVScale + UVOffset (Studio,
	/// 4-stud blocks with one Front decal at these five pairs): a red square over the middle of an
	/// otherwise clear image, on an 8-stud plate's front seen head-on, lands where that puts it, the face
	/// still bounding it.
	#[test]
	fn a_decals_uv_offset_and_scale_map_the_face_to_its_image() {
		let state = std::env::temp_dir().join(format!("ror-decal-uv-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		// a 64-texel image, clear but for an opaque red square from 0.375 to 0.625 on both axes
		let (side, low, high) = (64u32, 0.375f32, 0.625f32);
		let inside = |texel: u32| (low..high).contains(&((texel as f32 + 0.5) / side as f32));
		let rgba: Vec<u8> = (0..side * side).flat_map(|index| if inside(index % side) && inside(index / side) { [255, 0, 0, 255] } else { [0; 4] }).collect();
		let mut image = ror_image::Image::from_rgba8(side, side, rgba);
		ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true);
		let path = state.join("square.tex");
		assert!(texture_files::write(&path, &image, TextureUse::Color));
		let uri = "rbxassetid://454545";
		let key = transcode::image_key(uri);
		let texture = renderer.textures.get_or_load(&(content_key(&key), TextureUse::Color), || texture_files::open(&path, &key).ok_or_else(|| anyhow!("the texture file"))).unwrap();
		// the plate's front 9.5 studs from the camera: its half width in pixels
		let (size, distance) = (128u32, 10.0f32);
		let half = 4.0 / ((distance - 0.5) * 35f32.to_radians().tan()) * size as f32 * 0.5;
		let face = |pixel: u32| (pixel as f32 + 0.5 - (size as f32 * 0.5 - half)) / (2.0 * half);
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -distance], target: [0.0; 3], fov: 70.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
		for (offset, scale) in [([0.0, 0.0], [1.0, 1.0]), ([0.0, 0.0], [2.0, 2.0]), ([0.25, 0.0], [1.0, 1.0]), ([0.0, 0.5], [1.0, 0.75]), ([-0.37, -0.22], [1.5, 1.5])] {
			let mut model = crate::scene::build::ModelBuilder::new("Plate");
			model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: Vec3::new(8.0, 8.0, 1.0), ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			Arc::make_mut(&mut scene.parts[0]).decals.push(crate::scene::Decal { face: 5, texture: uri.into(), color: Vec3::ONE, transparency: 0.0, z_index: 1, tiling: None, uv_offset: offset, uv_scale: scale });
			let (images, _) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			// the red pixels' extent in the face's uv (u to the right, v down): min u, min v, max u, max v
			let mut extent = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
			for (index, pixel) in images[0].rgba.chunks(4).enumerate() {
				let (red, rest) = (pixel[0] as f32, pixel[1].max(pixel[2]) as f32);
				if red > 60.0 && red > 2.0 * rest {
					let (u, v) = (face(index as u32 % size), face(index as u32 / size));
					extent = [extent[0].min(u), extent[1].min(v), extent[2].max(u), extent[3].max(v)];
				}
			}
			let expected: [f32; 4] = std::array::from_fn(|i| ((if i < 2 { low } else { high } - offset[i % 2]) / scale[i % 2]).clamp(0.0, 1.0));
			assert!(extent.iter().zip(expected).all(|(got, want)| (got - want).abs() < 0.04), "UVOffset {offset:?} UVScale {scale:?}: the square over {extent:?} of the face, expected {expected:?}");
		}
		drop(texture);
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod joint_and_fallback_tests {
	use super::*;

	/// What a Roblox client draws for joint surfaces and for meshes that are not there: a Motor's yellow
	/// pin and dark disc on its face (a Hinge's pin alone, nothing on an invisible part), a MeshPart
	/// without a mesh as its box at 0.64 of a block's albedo, and nothing for a union whose data fails.
	#[test]
	fn joints_draw_and_missing_meshes_fall_back_as_a_client_does() {
		let state = std::env::temp_dir().join(format!("ror-joint-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let environment = crate::scene::Environment { brightness: 0.0, ambient: Vec3::splat(0.7), outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, global_shadows: false, ..Default::default() };
		// straight down onto a 4-stud block's top from 20 studs: 64 pixels across 7.05 studs
		let size = 64u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 22.0, -0.01], target: [0.0, 2.0, 0.0], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
		let draw = |class: &'static str, setup: &dyn Fn(&mut crate::scene::Part)| -> Vec<[u8; 3]> {
			let mut model = crate::scene::build::ModelBuilder::new("Joint");
			model.part(&crate::scene::build::PartSpec { class, name: "Block".into(), size: Vec3::splat(4.0), color: [255, 255, 255], cast_shadow: false, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			scene.environment = environment.clone();
			setup(Arc::make_mut(&mut scene.parts[0]));
			let (images, _) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			images[0].rgba.chunks(4).map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect()
		};
		// the pixel `studs` right of the face's centre
		let at = |image: &[[u8; 3]], studs: f32| image[(size / 2 * size) as usize + (size as f32 * 0.5 + studs * size as f32 / 7.05) as usize];
		let motor = draw("Part", &|part| part.surfaces[1] = SURFACE_MOTOR);
		let (pin, disc, face) = (at(&motor, 0.05), at(&motor, 0.32), at(&motor, 1.2));
		assert!(pin[0] > 100 && pin[0].abs_diff(pin[1]) < 4 && pin[2] < 10, "the pin: {pin:?}");
		assert!(disc[0] < 60 && disc[0].abs_diff(disc[2]) < 4, "the disc: {disc:?}");
		assert!(face[0] > 150, "the face: {face:?}");
		let hinge = draw("Part", &|part| part.surfaces[1] = SURFACE_HINGE);
		assert_eq!(at(&hinge, 0.05), pin);
		assert!(at(&hinge, 0.32)[0] > 150, "a Hinge has no disc: {:?}", at(&hinge, 0.32));
		let invisible = draw("Part", &|part| {
			part.surfaces[1] = SURFACE_MOTOR;
			part.transparency = 1.0;
		});
		assert_eq!(at(&invisible, 0.05), [0, 0, 0], "an invisible part's joint");
		// a MeshPart without a mesh: its box, darker than a block of its colour
		let block = draw("Part", &|_| ());
		let meshless = draw("MeshPart", &|part| assert!(matches!(part.geometry, Geometry::Block)));
		let (lit, dark) = (at(&block, 1.2)[0] as f32, at(&meshless, 1.2)[0] as f32);
		assert!((0.78..0.86).contains(&(dark / lit)), "a MeshPart without a mesh {dark} against a block's {lit}");
		// a union whose asset cannot be loaded draws nothing
		let union = draw("Part", &|part| part.geometry = Geometry::Union { asset: Some("rbxassetid://1".into()), mesh_data: None, initial_size: Vec3::splat(4.0), use_part_color: true });
		assert_eq!(at(&union, 1.2), [0, 0, 0], "a union whose data fails");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod tint_mask_tests {
	use super::*;

	/// A SurfaceAppearance in AlphaMode TintMask multiplies its texel by Color mixed from white by the
	/// texel's alpha, both as stored, and decodes the product as it decodes the map (Studio, tintmask-ramp):
	/// each cell draws as an untinted map holding that product does, the texel itself at alpha 0.
	#[test]
	fn a_tint_mask_mixes_color_into_the_stored_texel() {
		let state = std::env::temp_dir().join(format!("ror-tint-mask-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let (texel, color) = ([204u8, 128, 77], Vec3::new(0.3, 0.6, 0.9));
		let size = 32u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
		let draw = |name: &str, rgba: [u8; 4], color: Vec3| -> [i32; 3] {
			let path = state.join(format!("{name}.png"));
			crate::output::write_png(&path, 8, 8, &rgba.repeat(64), false).unwrap();
			let mut model = crate::scene::build::ModelBuilder::new("Plate");
			model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: Vec3::new(4.0, 4.0, 1.0), cast_shadow: false, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			scene.environment.brightness = 0.0;
			scene.environment.ambient = Vec3::splat(0.7);
			scene.environment.outdoor_ambient = Vec3::ZERO;
			scene.environment.environment_diffuse_scale = 0.0;
			scene.environment.environment_specular_scale = 0.0;
			scene.environment.global_shadows = false;
			Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(format!("file:///{}", path.display().to_string().replace('\\', "/"))),
				normal_map: None,
				metalness_map: None,
				roughness_map: None,
				texture_pack: None,
				alpha_mode: 2,
				color,
				emissive_mask: None,
				emissive_strength: 1.0,
				emissive_tint: Vec3::ONE,
			});
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			// the colour map loaded; without a Studio install the default sky and sun (rbxasset://) are
			// missing too, and the sky drawn instead lights nothing here
			assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "{name}: missing {:?}", stats.missing);
			let index = ((size / 2 * size + size / 2) * 4) as usize;
			std::array::from_fn(|c| images[0].rgba[index + c] as i32)
		};
		for alpha in [0u8, 73, 146, 219, 255] {
			let weight = alpha as f32 / 255.0;
			let stored: [u8; 3] = std::array::from_fn(|c| (texel[c] as f32 * (1.0 + (color[c] - 1.0) * weight)).round() as u8);
			let tinted = draw(&format!("tinted-{alpha}"), [texel[0], texel[1], texel[2], alpha], color);
			let reference = draw(&format!("reference-{alpha}"), [stored[0], stored[1], stored[2], 0], Vec3::ONE);
			assert!(tinted.iter().zip(reference).all(|(tinted, reference)| (tinted - reference).abs() <= 2), "alpha {alpha}: tinted {tinted:?}, an untinted map of {stored:?} {reference:?}");
			assert!(reference.iter().all(|value| (12..240).contains(value)), "alpha {alpha}: the plate lit within range {reference:?}");
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Under TintMask the glow takes the tinted colour, not the stored texel, and the mask by its red
	/// channel (Studio, tintmask-emission): a texel O / C at alpha 1 under Color C glows as O at alpha 0
	/// does with a white mask; with its own stored texel as the mask it glows as O's own mask does once
	/// EmissiveTint is C's red.
	#[test]
	fn a_tint_mask_glows_in_the_tinted_colour() {
		let state = std::env::temp_dir().join(format!("ror-tint-mask-emission-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let size = 32u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
		let uri = |name: &str, rgba: [u8; 4]| -> String {
			let path = state.join(format!("{name}.png"));
			crate::output::write_png(&path, 8, 8, &rgba.repeat(64), false).unwrap();
			format!("file:///{}", path.display().to_string().replace('\\', "/"))
		};
		let draw = |name: &str, texel: [u8; 4], color: Vec3, mask: [u8; 3], tint: Vec3, strength: f32| -> [i32; 3] {
			let mut model = crate::scene::build::ModelBuilder::new("Plate");
			model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: Vec3::new(4.0, 4.0, 1.0), cast_shadow: false, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			scene.environment.brightness = 0.0;
			scene.environment.ambient = Vec3::splat(0.3);
			scene.environment.outdoor_ambient = Vec3::ZERO;
			scene.environment.environment_diffuse_scale = 0.0;
			scene.environment.environment_specular_scale = 0.0;
			scene.environment.global_shadows = false;
			Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(uri(&format!("{name}-map"), texel)),
				normal_map: None,
				metalness_map: None,
				roughness_map: None,
				texture_pack: None,
				alpha_mode: 2,
				color,
				emissive_mask: Some(uri(&format!("{name}-mask"), [mask[0], mask[1], mask[2], 255])),
				emissive_strength: strength,
				emissive_tint: tint,
			});
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "{name}: missing {:?}", stats.missing);
			let index = ((size / 2 * size + size / 2) * 4) as usize;
			std::array::from_fn(|c| images[0].rgba[index + c] as i32)
		};
		let cases: [(&str, [u8; 3], Vec3); 2] = [("gray", [82, 82, 82], Vec3::splat(0.4)), ("warm", [92, 38, 20], Vec3::new(0.6, 0.3, 0.2))];
		for (case, original, color) in cases {
			let stored: [u8; 3] = std::array::from_fn(|c| (original[c] as f32 / color[c]).round().min(255.0) as u8);
			let dark = draw(&format!("{case}-dark"), [original[0], original[1], original[2], 0], Vec3::ONE, [255; 3], Vec3::ONE, 0.0);
			for strength in [1.0f32, 2.0, 4.0] {
				let label = format!("{case} strength {strength}");
				let reference = draw(&format!("{case}-{strength}-original"), [original[0], original[1], original[2], 0], Vec3::ONE, [255; 3], Vec3::ONE, strength);
				assert!(reference[0] - dark[0] > 20, "{label}: the white mask glows {reference:?} over {dark:?}");
				let tinted = draw(&format!("{case}-{strength}-tinted"), [stored[0], stored[1], stored[2], 255], color, [255; 3], Vec3::ONE, strength);
				assert!(tinted.iter().zip(reference).all(|(tinted, reference)| (tinted - reference).abs() <= 2), "{label}: {stored:?} under {color} glows {tinted:?}, {original:?} {reference:?}");
				let own = draw(&format!("{case}-{strength}-own"), [original[0], original[1], original[2], 0], Vec3::ONE, original, Vec3::ONE, strength);
				let masked = draw(&format!("{case}-{strength}-masked"), [stored[0], stored[1], stored[2], 255], color, stored, Vec3::splat(color.x), strength);
				assert!(masked.iter().zip(own).all(|(masked, own)| (masked - own).abs() <= 2), "{label}: its stored mask under EmissiveTint {} glows {masked:?}, {original:?}'s own {own:?}", color.x);
			}
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod vertex_colour_tests {
	use super::*;
	use rbx_dom_weak::types::{Content, Vector3};

	/// A 4 x 4 stud quad facing -Z (both sides) as a FileMesh 2.00 whose every vertex has `color`.
	pub(super) fn quad_mesh(color: [u8; 4]) -> Vec<u8> {
		let mut bytes = b"version 2.00\n".to_vec();
		bytes.extend_from_slice(&12u16.to_le_bytes());
		bytes.extend_from_slice(&[40, 12]);
		bytes.extend_from_slice(&4u32.to_le_bytes());
		bytes.extend_from_slice(&4u32.to_le_bytes());
		for (x, y) in [(-2.0f32, -2.0f32), (2.0, -2.0), (2.0, 2.0), (-2.0, 2.0)] {
			for value in [x, y, 0.0, 0.0, 0.0, -1.0, (x + 2.0) / 4.0, (y + 2.0) / 4.0] {
				bytes.extend_from_slice(&value.to_le_bytes());
			}
			bytes.extend_from_slice(&[255, 127, 127, 127]);
			bytes.extend_from_slice(&color);
		}
		for face in [[0u32, 1, 2], [0, 2, 3], [0, 2, 1], [0, 3, 2]] {
			for index in face {
				bytes.extend_from_slice(&index.to_le_bytes());
			}
		}
		bytes
	}

	/// A MeshPart's FileMesh vertex colours multiply its TextureID / TextureContent as they multiply its
	/// colour, whatever its Color; under a SurfaceAppearance in AlphaMode Overlay they tint the part
	/// colour its map reveals and not the map, and in AlphaMode Transparency they do nothing
	/// (fixtures/vertex-colour-maps.luau in a Roblox 0.747 client). Each case draws as a quad without
	/// vertex colours whose colour or texture holds that product does: before, a textured quad drew its
	/// texture alone.
	#[test]
	fn vertex_colours_tint_a_mesh_parts_colour_map() {
		let state = std::env::temp_dir().join(format!("ror-vertex-colour-map-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let uri = |path: &std::path::Path| format!("file:///{}", path.display().to_string().replace('\\', "/"));
		let file = |name: &str, bytes: &[u8]| -> String {
			let path = state.join(name);
			std::fs::write(&path, bytes).unwrap();
			uri(&path)
		};
		let image = |name: &str, rgba: [u8; 4]| -> String {
			let path = state.join(format!("{name}.png"));
			crate::output::write_png(&path, 8, 8, &rgba.repeat(64), false).unwrap();
			uri(&path)
		};
		let vertex = [255u8, 128, 64];
		let tinted_mesh = file("tinted.mesh", &quad_mesh([vertex[0], vertex[1], vertex[2], 255]));
		let white_mesh = file("white.mesh", &quad_mesh([255; 4]));
		let times = |a: [u8; 3], b: [u8; 3]| -> [u8; 3] { std::array::from_fn(|c| (a[c] as f32 * b[c] as f32 / 255.0).round() as u8) };
		let size = 32u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
		// (mesh, Color, TextureContent, SurfaceAppearance (ColorMap, AlphaMode))
		let draw = |name: &str, mesh: &str, color: [u8; 3], texture: Option<String>, appearance: Option<(String, u32)>| -> [i32; 3] {
			let mut model = crate::scene::build::ModelBuilder::new("Model");
			let part = model.part(&crate::scene::build::PartSpec { class: "MeshPart", name: "Quad".into(), size: glam::Vec3::new(4.0, 4.0, 0.05), color, material: 272, cast_shadow: false, ..Default::default() });
			model.set(part, "MeshContent", Content::from_uri(mesh));
			model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
			if let Some(texture) = &texture {
				model.set(part, "TextureContent", Content::from_uri(texture.as_str()));
			}
			let mut scene = crate::scene::extract::extract(&model.into_document());
			scene.environment.brightness = 0.0;
			scene.environment.ambient = Vec3::splat(0.7);
			scene.environment.outdoor_ambient = Vec3::ZERO;
			scene.environment.environment_diffuse_scale = 0.0;
			scene.environment.environment_specular_scale = 0.0;
			scene.environment.global_shadows = false;
			if let Some((map, alpha_mode)) = appearance {
				Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
					color_map: Some(map),
					normal_map: None,
					metalness_map: None,
					roughness_map: None,
					texture_pack: None,
					alpha_mode,
					color: Vec3::ONE,
					emissive_mask: None,
					emissive_strength: 1.0,
					emissive_tint: Vec3::ONE,
				});
			}
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "{name}: missing {:?}", stats.missing);
			let index = ((size / 2 * size + size / 2) * 4) as usize;
			let pixel: [i32; 3] = std::array::from_fn(|c| images[0].rgba[index + c] as i32);
			assert!(pixel.iter().any(|value| *value > 20), "{name}: the quad drawn {pixel:?}");
			pixel
		};
		let same = |label: &str, got: [i32; 3], want: [i32; 3]| assert!(got.iter().zip(want).all(|(got, want)| (got - want).abs() <= 2), "{label}: {got:?}, expected {want:?}");
		let cream = [204u8, 204, 128];
		// a white map leaves the vertex colours as they are, a tan one takes them; the part's Color shows nowhere
		for texel in [[255u8, 255, 255], [204, 153, 102]] {
			let map = image(&format!("map-{}-{}-{}", texel[0], texel[1], texel[2]), [texel[0], texel[1], texel[2], 255]);
			let product = times(texel, vertex);
			let reference = draw("reference", &white_mesh, [255; 3], Some(image(&format!("product-{}-{}-{}", product[0], product[1], product[2]), [product[0], product[1], product[2], 255])), None);
			same(&format!("map {texel:?} under vertex colour {vertex:?}"), draw("tinted", &tinted_mesh, [255; 3], Some(map.clone()), None), reference);
			same(&format!("map {texel:?} under vertex colour {vertex:?} on a cream part"), draw("tinted-cream", &tinted_mesh, cream, Some(map), None), reference);
		}
		// an Overlay map at alpha 128 over the part colour: the vertex colours tint the colour, not the map
		let overlay = image("overlay", [204, 153, 102, 128]);
		let reference = draw("overlay-reference", &white_mesh, times(cream, vertex), None, Some((overlay.clone(), 0)));
		same("Overlay over a vertex-coloured part", draw("overlay", &tinted_mesh, cream, None, Some((overlay, 0))), reference);
		// in AlphaMode Transparency the map alone: no tint
		let opaque = image("opaque", [204, 153, 102, 255]);
		let reference = draw("transparency-reference", &white_mesh, cream, None, Some((opaque.clone(), 1)));
		same("Transparency over a vertex-coloured part", draw("transparency", &tinted_mesh, cream, None, Some((opaque.clone(), 1))), reference);
		// an EditableImage ColorMap on a SurfaceAppearance decodes with the square (matching MeshPart
		// TextureContent), while an ordinary ColorMap decodes with the sRGB curve
		let editable_dir = state.join("editable-images");
		std::fs::create_dir_all(&editable_dir).unwrap();
		let editable_path = editable_dir.join("1-0.png");
		crate::output::write_png(&editable_path, 8, 8, &[204, 153, 102, 255].repeat(64), false).unwrap();
		let editable_uri = uri(&editable_path);
		let tex_ref = draw("editable-ref", &white_mesh, [255; 3], Some(opaque.clone()), None);
		let editable_sa = draw("editable-sa", &white_mesh, [255; 3], None, Some((editable_uri, 0)));
		let srgb_sa = draw("srgb-sa", &white_mesh, [255; 3], None, Some((opaque, 0)));
		same("EditableImage SurfaceAppearance ColorMap decodes with the square", editable_sa, tex_ref);
		assert!(editable_sa[1] - srgb_sa[1] >= 6, "ordinary ColorMap stays sRGB-decoded ({srgb_sa:?}) below square-decoded EditableImage ({editable_sa:?})");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod reflectance_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::InstanceBuilder;

	/// Part.Reflectance blends a mirror sky lit by half the diffuse light across flat ambient, sky
	/// diffuse and sun alike, leaves direct specular unmixed on top, and scales Glass's subtracted
	/// sky diffuse by (1 - Reflectance) so a reflective Glass part at T < 0.5 does not darken.
	#[test]
	fn reflectance_weights_diffuse_evenly_keeps_direct_specular_and_scales_glass_sky_diffuse() {
		let state = std::env::temp_dir().join(format!("ror-reflectance-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let sky_uri = |name: &str, rgb: [u8; 3]| -> String {
			let path = state.join(format!("{name}.png"));
			crate::output::write_png(&path, 8, 8, &[rgb[0], rgb[1], rgb[2], 255].repeat(64), false).unwrap();
			format!("file:///{}", path.display().to_string().replace('\\', "/"))
		};
		let white_sky = sky_uri("white-sky", [255, 255, 255]);
		let black_sky = sky_uri("black-sky", [0, 0, 0]);
		let set_sky = |scene: &mut Scene, face: &str| {
			scene.environment.sky = Some(crate::scene::environment::Sky {
				skybox: std::array::from_fn(|_| face.to_string()),
				celestial_bodies_shown: false,
				..Default::default()
			});
		};
		let size = 32u32;
		let render_center = |scene: &Scene, eye: [f32; 3]| -> [f32; 3] {
			let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye, target: [0.0; 3], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
			let (images, _) = renderer.render(scene, std::slice::from_ref(&request)).unwrap();
			let index = ((size / 2 * size + size / 2) * 4) as usize;
			std::array::from_fn(|c| images[0].rgba[index + c] as f32)
		};
		// 1. Direct specular from a PointLight stays unmixed at Reflectance 1 over a black sky.
		let glint_scene = |reflectance: f32| {
			let mut model = ModelBuilder::new("Glint");
			model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(10.0, 10.0, 1.0), color: [0, 0, 0], material: 272, reflectance, cast_shadow: false, ..Default::default() });
			let lamp = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, -6.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			model.child(lamp, InstanceBuilder::new("PointLight").with_property("Range", 30.0f32).with_property("Brightness", 4.0f32));
			let mut scene = crate::scene::extract::extract(&model.into_document());
			set_sky(&mut scene, &black_sky);
			let env = &mut scene.environment;
			(env.ambient, env.outdoor_ambient, env.brightness, env.environment_diffuse_scale, env.environment_specular_scale, env.global_shadows) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0, false);
			scene
		};
		let glint_r0 = render_center(&glint_scene(0.0), [0.0, 0.0, -10.0]);
		let glint_r1 = render_center(&glint_scene(1.0), [0.0, 0.0, -10.0]);
		assert!(glint_r0[0] > 40.0, "expected a visible PointLight specular glint at R=0, got {glint_r0:?}");
		assert!((glint_r1[0] - glint_r0[0]).abs() <= 2.0, "direct specular must survive Reflectance 1: R=0 {glint_r0:?}, R=1 {glint_r1:?}");

		// 2. Diffuse light weights are equal (0.5) across flat ambient, sky diffuse and sun alone.
		// Match each mode's R=0 white plate level from the same camera, ClockTime and reflection
		// direction so mirror and tone curve are identical; R=1 black then lands on mirror * 0.5 * L.
		let plate_scene = |color: [u8; 3], reflectance: f32, mode: &str, strength: f32| {
			let mut model = ModelBuilder::new("Plate");
			model.part(&PartSpec { name: "Plate".into(), size: Vec3::new(10.0, 1.0, 10.0), color, material: 272, reflectance, cast_shadow: false, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			set_sky(&mut scene, &white_sky);
			let env = &mut scene.environment;
			(env.ambient, env.outdoor_ambient, env.brightness, env.environment_diffuse_scale, env.environment_specular_scale, env.global_shadows, env.clock_time) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0, false, 7.5);
			match mode {
				"ambient" => env.ambient = Vec3::splat(strength),
				"sky" => env.environment_diffuse_scale = strength,
				"sun" => env.brightness = strength,
				_ => unreachable!(),
			}
			scene
		};
		let top_eye = [0.0, 10.0, 0.01];
		let matched = |mode: &str, target: f32| {
			let (mut lo, mut hi) = (0.0f32, 16.0f32);
			for _ in 0..15 {
				let mid = 0.5 * (lo + hi);
				if render_center(&plate_scene([255, 255, 255], 0.0, mode, mid), top_eye)[0] < target {
					lo = mid;
				} else {
					hi = mid;
				}
			}
			let s = 0.5 * (lo + hi);
			let r0 = render_center(&plate_scene([255, 255, 255], 0.0, mode, s), top_eye)[0];
			let r1 = render_center(&plate_scene([0, 0, 0], 1.0, mode, s), top_eye)[0];
			assert!((r0 - target).abs() <= 2.0, "{mode}: matched r0 {r0} to {target}");
			r1
		};
		let target = 150.0;
		let amb = matched("ambient", target);
		let sky = matched("sky", target);
		let sun = matched("sun", target);
		assert!((sky - amb).abs() <= 2.0, "sky diffuse weight must match flat ambient (0.5): amb {amb:.1}, sky {sky:.1}");
		assert!((sun - amb).abs() <= 2.0, "sun diffuse weight must match flat ambient (0.5): amb {amb:.1}, sun {sun:.1}");

		// 3. Glass at Transparency 0.3, Reflectance 1 under sky diffuse alone keeps its mirror image (no
		// unscaled sky_diffuse subtracted from it), black Glass's half the sky, white's the sky itself
		// (glass_tests for the rule).
		let glass_scene = |color: [u8; 3]| {
			let mut model = ModelBuilder::new("GlassRefl");
			model.part(&PartSpec { name: "Pane".into(), size: Vec3::new(10.0, 10.0, 1.0), color, material: 1568, transparency: 0.3, reflectance: 1.0, cast_shadow: false, ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			set_sky(&mut scene, &white_sky);
			let env = &mut scene.environment;
			(env.ambient, env.outdoor_ambient, env.brightness, env.environment_diffuse_scale, env.environment_specular_scale, env.global_shadows) = (Vec3::ZERO, Vec3::ZERO, 0.0, 1.0, 0.0, false);
			scene
		};
		let glass_white = render_center(&glass_scene([255, 255, 255]), [0.0, 0.0, -10.0]);
		let glass_black = render_center(&glass_scene([0, 0, 0]), [0.0, 0.0, -10.0]);
		assert!(glass_black[0] > 40.0, "reflective Glass under sky diffuse should be lit, got {glass_black:?}");
		assert!((0..3).all(|c| glass_white[c] >= glass_black[c]), "white Glass at R=1 T=0.3 reflects at least what black Glass does: white {glass_white:?}, black {glass_black:?}");

		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod viewport_frame_tests {
	use super::*;

	/// Surface types without a material texture (the pre-2022 Plastic) still take the face's frame.
	#[test]
	fn surface_types_tile_the_face_frame_without_a_texture_pack() {
		assert_eq!(uv_mode(0, false, &Geometry::Cylinder, 1 << 4), UV_PLANAR);
		assert_eq!(uv_mode(0, false, &Geometry::Ball, 1 << 4), UV_BALL);
		assert_eq!(uv_mode(0, false, &Geometry::Block, 0), UV_MESH);
		assert_eq!(uv_mode(TEX_MATERIAL, false, &Geometry::Block, 0), UV_PLANAR);
	}

	/// Without a Roblox install (a CI runner, a machine without Studio), the default sky's built-in
	/// textures can't be read: a world misses them, but a ViewportFrame without a Sky draws no sky and
	/// takes no light from one, so it misses nothing.
	#[test]
	fn a_viewport_frame_without_a_sky_needs_no_built_in_sky() {
		let state = std::env::temp_dir().join(format!("ror-viewport-frame-sky-test-{}", std::process::id()));
		let no_install = state.join("no-roblox-install");
		std::fs::create_dir_all(&no_install).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, studio_content: Some(no_install), ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: Vec3::new(4.0, 4.0, 1.0), material: 272, color: [255, 255, 255], ..Default::default() });
		let world = crate::scene::extract::extract(&model.into_document());
		let request = ViewRequest { width: 32, height: 32, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 40.0 }, background: Background::Frame { color: [0.2, 0.4, 0.6], alpha: 1.0 }, bloom: false, ..Default::default() };
		let (_, stats) = renderer.render(&world, std::slice::from_ref(&request)).unwrap();
		assert!(stats.missing.iter().any(|missing| missing.contains("sky")), "a world misses the built-in sky: {:?}", stats.missing);
		let mut framed = world.clone();
		framed.in_viewport_frame(crate::scene::environment::ViewportLighting { ambient: Vec3::ONE, light_color: Vec3::ZERO, light_direction: Vec3::NEG_Z, sky: None });
		let (_, stats) = renderer.render(&framed, std::slice::from_ref(&request)).unwrap();
		assert!(stats.missing.is_empty(), "a ViewportFrame without a Sky misses nothing: {:?}", stats.missing);
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A ViewportFrame's target (`Background::Frame`) is cleared to the frame's background colour at its
	/// opacity, and a part's anti-aliased edge averages the part's colour with the clear colour as it is,
	/// whatever the opacity (Studio 0.747: a white part's edge over a half-transparent background is the
	/// mean of white and that colour at the mean opacity).
	#[test]
	fn a_viewport_frames_target_is_cleared_to_its_background() {
		let state = std::env::temp_dir().join(format!("ror-viewport-frame-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: Vec3::new(4.0, 4.0, 1.0), material: 272, color: [255, 255, 255], ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		scene.in_viewport_frame(crate::scene::environment::ViewportLighting { ambient: Vec3::ONE, light_color: Vec3::ZERO, light_direction: Vec3::NEG_Z, sky: None });
		let colour = [0.2f32, 0.4, 0.6];
		let size = 64u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 40.0 }, background: Background::Frame { color: colour, alpha: 0.5 }, bloom: false, ..Default::default() };
		let (images, _) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		let pixel = |x: u32, y: u32| -> [f32; 4] { let i = ((y * size + x) * 4) as usize; std::array::from_fn(|c| images[0].rgba[i + c] as f32 / 255.0) };
		let corner = pixel(1, 1);
		assert!((0..3).all(|c| (corner[c] - colour[c]).abs() < 1.5 / 255.0) && (corner[3] - 0.5).abs() < 1.5 / 255.0, "the clear colour at its opacity: {corner:?}");
		let middle = pixel(size / 2, size / 2);
		assert!(middle[3] > 254.0 / 255.0 && middle[0] > 0.9, "the part opaque: {middle:?}");
		// along the middle row, every pixel the part covers in part: the straight mean, at the mean opacity
		let row: Vec<[f32; 4]> = (0..size).map(|x| pixel(x, size / 2)).collect();
		let edges: Vec<&[f32; 4]> = row.iter().filter(|p| p[3] > 0.52 && p[3] < 0.98).collect();
		assert!(!edges.is_empty(), "an anti-aliased edge");
		for edge in edges {
			let covered = (edge[3] - 0.5) / 0.5;
			let blue = 1.0 * covered + colour[2] * (1.0 - covered);
			assert!((edge[2] - blue).abs() < 3.0 / 255.0, "the edge's colour is the straight mean ({blue}): {edge:?}");
		}
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod shadow_caster_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::types::{Attributes, Variant};

	/// The cut itself (the engine, 0.747: plates in steps of 0.00005 cast the sun's full shadow up to
	/// Transparency 0.74795 and none from 0.748, whatever their Material).
	#[test]
	fn a_part_casts_shadows_below_the_cut_in_its_transparency() {
		for (transparency, casts) in [(-1.0, true), (0.0, true), (0.5, true), (0.747, true), (0.74795, true), (0.748, false), (0.75, false), (0.999, false), (1.0, false)] {
			assert_eq!(crate::scene::casts_shadow(transparency), casts, "Transparency {transparency}");
		}
	}

	/// A roof over a floor, lit by the sun alone and then by one shadowed PointLight above the roof
	/// alone, no sky light either time: the floor under it is as dark as under an opaque roof or as
	/// lit as without one, by the roof's Transparency and not its Material (ForceField at 0.9 cast a full
	/// shadow before). A roof with the shadow-only attribute shades the floor at any Transparency and is
	/// not in the picture from below; without CastShadow it does nothing.
	#[test]
	fn transparency_cuts_a_parts_shadows_and_a_shadow_only_part_casts_unseen() {
		let state = std::env::temp_dir().join(format!("ror-shadow-caster-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		// (Material, Transparency, CastShadow, the shadow-only attribute)
		type Roof = (u32, f32, bool, bool);
		let scene = |roof: Option<Roof>, lamp: bool| {
			let mut model = ModelBuilder::new("Shade");
			model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(160.0, 1.0, 160.0), color: [255; 3], ..Default::default() });
			if let Some((material, transparency, cast_shadow, shadow_only)) = roof {
				let part = model.part(&PartSpec { name: "Roof".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 20.0, 0.0)), size: Vec3::new(100.0, 1.0, 100.0), material, transparency, cast_shadow, ..Default::default() });
				if shadow_only {
					model.set(part, "Attributes", Attributes::new().with(crate::scene::SHADOW_ONLY, Variant::Bool(true)));
				}
			}
			if lamp {
				let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 30.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
				model.child(carrier, rbx_dom_weak::InstanceBuilder::new("PointLight").with_property("Shadows", true).with_property("Range", 60.0f32).with_property("Brightness", 5.0f32));
			}
			let mut scene = crate::scene::extract::extract(&model.into_document());
			let environment = &mut scene.environment;
			(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
			if lamp {
				environment.brightness = 0.0;
			}
			scene
		};
		// the floor under the roof's middle from below the roof, and the roof's underside from the floor
		let view = |eye: [f32; 3], target: [f32; 3]| ViewRequest { width: 64, height: 64, camera: CameraSpec::LookAt { eye, target, fov: 40.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
		let views = [view([0.0, 12.0, -3.0], [0.0; 3]), view([0.0, 2.0, -1.0], [0.0, 20.0, 0.0])];
		let draw = |roof: Option<Roof>, lamp: bool| {
			let (images, _) = renderer.render(&scene(roof, lamp), &views).unwrap();
			let level = |image: &ViewImage| image.rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (image.rgba.len() / 4) as f32;
			(level(&images[0]), images[1].rgba.clone())
		};
		const SMOOTH_PLASTIC: u32 = 272;
		const GLASS: u32 = 1568;
		const FORCEFIELD: u32 = 1584;
		for lamp in [false, true] {
			let by = if lamp { "a PointLight" } else { "the sun" };
			let (lit, sky) = draw(None, lamp);
			let (dark, roofed) = draw(Some((SMOOTH_PLASTIC, 0.0, true, false)), lamp);
			assert!(lit - dark > 40.0, "{by}: an opaque roof takes the light off the floor ({lit} lit, {dark} under it)");
			assert!(roofed != sky, "{by}: an opaque roof is in the picture from below");
			// (the roof, whether it shades the floor, whether it is in the picture from below: a ForceField one
			// seen head-on shows nothing either way)
			let cases: [(Roof, bool, Option<bool>); 9] = [
				((SMOOTH_PLASTIC, 0.74, true, false), true, Some(true)),
				((SMOOTH_PLASTIC, 0.75, true, false), false, Some(true)),
				((GLASS, 0.74, true, false), true, Some(true)),
				((GLASS, 0.9, true, false), false, Some(true)),
				((FORCEFIELD, 0.5, true, false), true, None),
				((FORCEFIELD, 0.9, true, false), false, None),
				((FORCEFIELD, 0.999, true, true), true, Some(false)),
				((SMOOTH_PLASTIC, 1.0, true, true), true, Some(false)),
				((SMOOTH_PLASTIC, 0.0, false, true), false, Some(false)),
			];
			for (roof, shades, seen) in cases {
				let (level, below) = draw(Some(roof), lamp);
				let share = (lit - level) / (lit - dark);
				assert!(if shades { share > 0.9 } else { share < 0.1 }, "{by}, roof {roof:?}: {share} of an opaque roof's shadow on the floor");
				assert!(seen.is_none_or(|seen| seen == (below != sky)), "{by}, roof {roof:?}: in the picture from below, {}", below != sky);
			}
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A floor lit by the sun alone under a roof of blocks. With the sun's shadow maps (the default) any
	/// roof shades it. Without them (the Lighting attribute scene::NO_SUN_SHADOW_MAPS) only the light grid
	/// stops the sun: a roof 20 studs up closes the grid over the floor and still shades it, a roof 3 studs
	/// up lies under the voxel the floor reads and leaves it lit, and an open floor is lit as before.
	#[test]
	fn without_the_suns_shadow_maps_only_the_light_grid_stops_the_sun() {
		let state = std::env::temp_dir().join(format!("ror-no-sun-shadow-maps-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let scene = |roof: Option<f32>, maps: bool| {
			let mut model = ModelBuilder::new("Shade");
			model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(160.0, 1.0, 160.0), color: [255; 3], ..Default::default() });
			if let Some(height) = roof {
				model.part(&PartSpec { name: "Roof".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, height, 0.0)), size: Vec3::new(100.0, 1.0, 100.0), ..Default::default() });
			}
			let mut document = model.into_document();
			if !maps {
				let root = document.dom.root_ref();
				let lighting = document.dom.insert(root, rbx_dom_weak::InstanceBuilder::new("Lighting").with_property("Attributes", Attributes::new().with(crate::scene::NO_SUN_SHADOW_MAPS, Variant::Bool(true))));
				assert!(!crate::scene::Environment::from_lighting(&document.dom, document.dom.get_by_ref(lighting).unwrap()).sun_shadow_maps, "the attribute on Lighting turns the maps off");
			}
			let mut scene = crate::scene::extract::extract(&document);
			let environment = &mut scene.environment;
			(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
			environment.sun_shadow_maps = maps;
			scene
		};
		// the floor under the roof's middle, from under the lower roof
		let views = [ViewRequest { width: 64, height: 64, camera: CameraSpec::LookAt { eye: [0.0, 2.0, -3.0], target: [0.0; 3], fov: 40.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() }];
		let draw = |roof: Option<f32>, maps: bool| {
			let (images, _) = renderer.render(&scene(roof, maps), &views).unwrap();
			images[0].rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (images[0].rgba.len() / 4) as f32
		};
		let (lit, dark) = (draw(None, true), draw(Some(20.0), true));
		assert!(lit - dark > 40.0, "with the maps a roof takes the sun off the floor ({lit} lit, {dark} under it)");
		// (the roof's height, the maps, whether the floor under it is shaded)
		for (roof, maps, shades) in [(Some(3.0), true, true), (None, false, false), (Some(20.0), false, true), (Some(3.0), false, false)] {
			let share = (lit - draw(roof, maps)) / (lit - dark);
			assert!(if shades { share > 0.9 } else { share < 0.1 }, "roof {roof:?}, shadow maps {maps}: {share} of a roof's shadow on the floor");
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod lighting_style_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};

	/// A shadowed PointLight (Range 16, Brightness 2) 8 studs over a white floor with an opaque 6 x 0.4
	/// x 6 plate beside and under it, no other light, seen straight down at three points of the floor:
	/// in the plate's shadow, as far from the light on the other side, and 17.5 studs from the light,
	/// past its Range. Under Realistic the shadow is dark and past the Range nothing is lit; under Soft
	/// (the engine's frames, fixtures/lighting-style.luau) the floor takes the lights' voxels: no shadow
	/// map, the shadow lit like the other side, light past the Range, Shadows or not, and a light placed
	/// with a playback lights the floor near the camera too.
	#[test]
	fn soft_lighting_style_lights_surfaces_from_the_voxels() {
		let state = std::env::temp_dir().join(format!("ror-lighting-style-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let scene = |soft: bool, shadows: bool, placed: bool| {
			let mut model = ModelBuilder::new("Style");
			model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(60.0, 1.0, 60.0), color: [255; 3], material: 272, ..Default::default() });
			model.part(&PartSpec { name: "Plate".into(), cframe: Affine3A::from_translation(Vec3::new(5.0, 4.0, 0.0)), size: Vec3::new(6.0, 0.4, 6.0), color: [255; 3], material: 272, ..Default::default() });
			let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 8.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			model.child(carrier, rbx_dom_weak::InstanceBuilder::new("PointLight").with_property("Shadows", shadows).with_property("Range", 16.0f32).with_property("Brightness", 2.0f32));
			let mut scene = crate::scene::extract::extract(&model.into_document());
			for light in &mut scene.lights {
				light.placed = placed;
			}
			let environment = &mut scene.environment;
			(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0);
			environment.lighting_style = if soft { crate::scene::environment::LIGHTING_STYLE_SOFT } else { 0 };
			scene
		};
		// the floor straight under each camera: in the plate's shadow, the other side, past the Range
		let view = |x: f32, z: f32| ViewRequest { width: 16, height: 16, camera: CameraSpec::LookAt { eye: [x, 40.0, z - 0.05], target: [x, 0.0, z], fov: 4.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
		let views = [view(10.0, 0.0), view(-10.0, 0.0), view(0.0, 17.5)];
		let draw = |soft: bool, shadows: bool, placed: bool| -> [f32; 3] {
			let (images, _) = renderer.render(&scene(soft, shadows, placed), &views).unwrap();
			let level = |image: &ViewImage| image.rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (image.rgba.len() / 4) as f32;
			[level(&images[0]), level(&images[1]), level(&images[2])]
		};
		let [shade, open, past] = draw(false, true, false);
		assert!(open > 150.0 && shade < 0.15 * open, "Realistic: the plate shadows the floor ({shade} in its shadow, {open} on the other side)");
		assert!(past < 3.0, "Realistic: nothing lit past the Range ({past})");
		let [shade, open, past] = draw(true, true, false);
		assert!(open > 150.0 && shade > 0.9 * open, "Soft: the plate's shadow is lit like the other side ({shade} and {open})");
		assert!(past > 40.0, "Soft: the voxels light the floor past the Range ({past})");
		for (shadows, placed) in [(false, false), (true, true)] {
			let other = draw(true, shadows, placed);
			assert!(other.iter().zip([shade, open, past]).all(|(a, b)| (a - b).abs() < 1.0), "Soft, Shadows {shadows}, placed with a playback {placed}: {other:?} as {:?}", [shade, open, past]);
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A graphics quality level's per-pixel reach for local lights (crate::quality::Quality::light_reach; the
	/// client's `lightline`): the same shadowed PointLight and plate under Realistic, seen from 100 studs up.
	/// Without a level and at 17 (per pixel to 275 studs) the plate's shadow is dark; at 14 (per pixel to ~55)
	/// the floor 100 studs off takes the lights' voxels, the shadow lit like the other side, and at 13 (none
	/// per pixel) every surface does, as under Soft.
	#[test]
	fn a_quality_level_lights_per_pixel_only_to_its_reach() {
		let state = std::env::temp_dir().join(format!("ror-quality-lights-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = ModelBuilder::new("Reach");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(60.0, 1.0, 60.0), color: [255; 3], material: 272, ..Default::default() });
		model.part(&PartSpec { name: "Plate".into(), cframe: Affine3A::from_translation(Vec3::new(5.0, 4.0, 0.0)), size: Vec3::new(6.0, 0.4, 6.0), color: [255; 3], material: 272, ..Default::default() });
		let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 8.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		model.child(carrier, rbx_dom_weak::InstanceBuilder::new("PointLight").with_property("Shadows", true).with_property("Range", 16.0f32).with_property("Brightness", 2.0f32));
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0);
		// the floor straight under each camera: in the plate's shadow, the other side
		let view = |x: f32, quality: Option<u32>| ViewRequest { width: 16, height: 16, camera: CameraSpec::LookAt { eye: [x, 100.0, -0.05], target: [x, 0.0, 0.0], fov: 1.6 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, quality, ..Default::default() };
		for (quality, shadowed) in [(None, true), (Some(17), true), (Some(14), false), (Some(13), false)] {
			let (images, _) = renderer.render(&scene, &[view(10.0, quality), view(-10.0, quality)]).unwrap();
			let level = |image: &ViewImage| image.rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (image.rgba.len() / 4) as f32;
			let (shade, open) = (level(&images[0]), level(&images[1]));
			assert!(open > 40.0, "level {quality:?}: the floor beside the light is lit ({open})");
			if shadowed {
				assert!(shade < 0.15 * open, "level {quality:?}: per pixel, the plate shadows the floor ({shade} in its shadow, {open} on the other side)");
			} else {
				assert!(shade > 0.9 * open, "level {quality:?}: from the voxels, the plate's shadow is lit like the other side ({shade} and {open})");
			}
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A white cube on a white floor under Ambient alone, seen straight down. The light-grid
	/// fixture's "contacts" in the engine (0.747): LightingStyle Soft darkens it by 0.874 of Realistic's darkening.
	#[test]
	fn soft_lighting_style_darkens_contacts_less() {
		let state = std::env::temp_dir().join(format!("ror-soft-occlusion-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let scene = |soft: bool, cube: bool| {
			let mut model = ModelBuilder::new("Contact");
			model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(40.0, 1.0, 40.0), color: [255; 3], material: 272, ..Default::default() });
			if cube {
				model.part(&PartSpec { name: "Cube".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, 0.0)), size: Vec3::splat(4.0), color: [255; 3], material: 272, ..Default::default() });
			}
			let mut scene = crate::scene::extract::extract(&model.into_document());
			let environment = &mut scene.environment;
			(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::splat(0.5), Vec3::ZERO, 0.0, 0.0, 0.0);
			environment.global_shadows = false;
			environment.lighting_style = if soft { crate::scene::environment::LIGHTING_STYLE_SOFT } else { 0 };
			scene
		};
		// straight down over the cube: the floor round it and its top, as lit as the open floor but for the occlusion
		let view = |linear_scale: f32| ViewRequest { width: 64, height: 64, camera: CameraSpec::LookAt { eye: [0.0, 12.0, -0.05], target: [0.0; 3], fov: 40.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, linear_scale: Some(linear_scale), ..Default::default() };
		// each pixel's radiance (green's): without the tone curve the image is the square root of the exposed radiance
		// times the view's scale
		let draw = |soft: bool, cube: bool, linear_scale: f32| -> Vec<f32> {
			let (images, _) = renderer.render(&scene(soft, cube), &[view(linear_scale)]).unwrap();
			images[0].rgba.chunks(4).map(|pixel| (pixel[1] as f32 / 255.0).powi(2)).collect()
		};
		// read in radiance, the open floor near the top of the 8 bits: through the tone curve the darkening is 1 to 4
		// levels of 255 and Soft's smaller share of it is lost in the rounding (a GPU read 0.82 of Realistic's, the
		// software adapter 0.78, the two a level apart on a tenth of the pixels)
		let floor = draw(false, false, 1.0).iter().sum::<f32>() / (64 * 64) as f32;
		assert!(floor > 0.001 && floor < 1.0, "the open floor's radiance read unclipped ({floor})");
		let linear_scale = 0.8 / floor;
		let (open, realistic, soft) = (draw(false, false, linear_scale), draw(false, true, linear_scale), draw(true, true, linear_scale));
		assert!(open.iter().all(|&open| (0.7..0.9).contains(&open)), "the open floor near the top of the range");
		// over the pixels Realistic's occlusion darkens by 2 % or more
		let (mut darkened, mut by_realistic, mut by_soft) = (0, 0.0, 0.0);
		for ((open, realistic), soft) in open.iter().zip(&realistic).zip(&soft) {
			if open - realistic > 0.02 * open {
				darkened += 1;
				by_realistic += open - realistic;
				by_soft += open - soft;
			}
		}
		assert!(darkened > 20, "Realistic darkens the cube's foot ({darkened} pixels)");
		let ratio = by_soft / by_realistic;
		assert!((0.8..0.95).contains(&ratio), "Soft darkens it {ratio} as much");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A white floor under a 48-stud roof whose bottom is 8 studs up, lit by the sky's specular light alone at EV +2,
	/// seen straight down from under the roof's middle. The light-grid fixture's "sky" in the engine (0.747): it reflects
	/// the closed grid's warm grey ((9.6, 9.6, 8.8) of 255 against the open floor's blue sky (16, 37, 59)), where ror kept
	/// 0.12 of the blue sky ((2, 5, 9)).
	#[test]
	fn the_closed_grid_reflects_a_grey_sky() {
		let state = std::env::temp_dir().join(format!("ror-closed-specular-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = ModelBuilder::new("Roofed");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(64.0, 1.0, 64.0), color: [255; 3], material: 272, ..Default::default() });
		model.part(&PartSpec { name: "Roof".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 10.0, 0.0)), size: Vec3::new(48.0, 4.0, 48.0), color: [255; 3], material: 272, ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness, environment.exposure_compensation) = (Vec3::ZERO, Vec3::ZERO, 0.0, 1.0, 0.0, 2.0);
		let views = [ViewRequest { width: 32, height: 32, camera: CameraSpec::LookAt { eye: [0.0, 7.5, -0.05], target: [0.0; 3], fov: 10.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() }];
		let (images, _) = renderer.render(&scene, &views).unwrap();
		let mean = |channel: usize| images[0].rgba.chunks(4).map(|pixel| pixel[channel] as f32).sum::<f32>() / (images[0].rgba.len() / 4) as f32;
		let [red, green, blue] = [mean(0), mean(1), mean(2)];
		assert!(green > 5.0 && red >= blue - 0.5 && green >= blue - 0.5, "a grey under the roof, not the blue sky: ({red}, {green}, {blue})");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod surface_light_shadow_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::types::Enum;

	/// A shadowed SurfaceLight (Brightness 2) on a carrier, its shadows as a Roblox client draws them in
	/// the dark (notes/render.md "Local lights", fixtures/local-lights.luau).
	struct Lamp {
		/// The carrier's centre and size, opaque or not
		at: Vec3,
		size: Vec3,
		opaque: bool,
		face: u32,
		angle: f32,
		range: f32,
		shadows: bool,
	}

	fn scene(lamp: &Lamp, extra: &[(Vec3, Vec3)], floor: bool) -> Scene {
		let mut model = ModelBuilder::new("SurfaceShadows");
		if floor {
			model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(80.0, 1.0, 80.0), color: [255; 3], material: 272, ..Default::default() });
		}
		for (index, (at, size)) in extra.iter().enumerate() {
			model.part(&PartSpec { name: format!("Block{index}"), cframe: Affine3A::from_translation(*at), size: *size, color: [255; 3], material: 272, ..Default::default() });
		}
		let (transparency, cast_shadow) = if lamp.opaque { (0.0, true) } else { (1.0, false) };
		let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(lamp.at), size: lamp.size, color: [255; 3], material: 272, transparency, cast_shadow, ..Default::default() });
		model.child(
			carrier,
			rbx_dom_weak::InstanceBuilder::new("SurfaceLight")
				.with_property("Face", Enum::from_u32(lamp.face))
				.with_property("Angle", lamp.angle)
				.with_property("Shadows", lamp.shadows)
				.with_property("Range", lamp.range)
				.with_property("Brightness", 2.0f32),
		);
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0);
		scene
	}

	/// The mean level of a small view from `eye` at `target`.
	fn level(renderer: &Renderer, scene: &Scene, eye: [f32; 3], target: [f32; 3]) -> f32 {
		let view = ViewRequest { width: 16, height: 16, camera: CameraSpec::LookAt { eye, target, fov: 3.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
		let (images, _) = renderer.render(scene, &[view]).unwrap();
		let image = &images[0];
		image.rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (image.rgba.len() / 4) as f32
	}

	#[test]
	fn surface_light_shadows() {
		let state = std::env::temp_dir().join(format!("ror-surface-shadow-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		// nothing behind the face casts: a face down from the underside of a carrier 8 studs up (Bottom),
		// opaque or with a plate between the face and its shadow's point (Angle 60: one aimed face, its
		// near plane on the face; 180: six faces, cut at the face), lights the floor under it as an
		// invisible carrier does (the client: 4.02-4.27 against 4.14 in every case; ror shadowed it whole)
		let under = |angle: f32, opaque: bool, plate: bool| {
			let lamp = Lamp { at: Vec3::new(0.0, 8.5, 0.0), size: Vec3::new(4.0, 1.0, 4.0), opaque, face: 4, angle, range: 16.0, shadows: true };
			let plate = if plate { vec![(Vec3::new(0.0, 9.0, 0.0), Vec3::new(6.0, 0.2, 6.0))] } else { Vec::new() };
			level(&renderer, &scene(&lamp, &plate, true), [0.0, 5.0, -12.0], [0.0, 0.0, 0.0])
		};
		for angle in [60.0, 180.0] {
			let open = under(angle, false, false);
			assert!(open > 100.0, "Angle {angle}: the floor under an invisible carrier is lit ({open})");
			for (opaque, plate) in [(true, false), (false, true)] {
				let shut = under(angle, opaque, plate);
				assert!((shut - open).abs() < 2.0, "Angle {angle}, opaque carrier {opaque}, plate behind the face {plate}: {shut} against {open}");
			}
		}
		// a surface turned away from the face: the far side of a board 1 stud thick 1.4 studs before an
		// 8-stud face of Angle 0 (Back) takes its size term, all of it shadowed by the board (the client:
		// none of it; ror's lifted lookup crossed the board and kept all of it)
		let back = |shadows: bool| {
			let lamp = Lamp { at: Vec3::new(0.0, 20.0, -0.5), size: Vec3::new(8.0, 8.0, 1.0), opaque: false, face: 2, angle: 0.0, range: 30.0, shadows };
			level(&renderer, &scene(&lamp, &[(Vec3::new(0.0, 20.0, 1.9), Vec3::new(4.0, 4.0, 1.0))], false), [3.0, 22.0, 32.4], [0.0, 20.0, 2.4])
		};
		let (lit, shadowed) = (back(false), back(true));
		assert!(lit > 50.0 && shadowed < 0.1 * lit, "the board's far side: {shadowed} with Shadows, {lit} without");
		// a 2-stud face of Angle 180 along a floor (Front) with its centre 0.5 studs up, so that its
		// shadow's point lies 1 stud behind it at that height: 13 studs out the floor keeps 0.93 of the
		// unshadowed light in Studio (light-graze-kinds), as the grazing rule from that point gives at a
		// quarter of a PointLight's u; ror had darkened nothing
		let graze = |shadows: bool| {
			let lamp = Lamp { at: Vec3::new(0.0, 0.5, 0.0), size: Vec3::new(2.0, 2.0, 0.2), opaque: false, face: 5, angle: 180.0, range: 16.0, shadows };
			level(&renderer, &scene(&lamp, &[], true), [0.0, 20.0, -13.05], [0.0, 0.0, -13.0])
		};
		let (lit, shadowed) = (graze(false), graze(true));
		assert!(lit > 30.0 && shadowed < 0.985 * lit && shadowed > 0.9 * lit, "the grazed floor 13 studs out: {shadowed} with Shadows, {lit} without");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod shadow_cutout_tests {
	use super::*;
	use crate::render::resources::TextureUse;
	use crate::render::{content_key, texture_files, transcode};
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::types::{Content, Vector3};

	/// A card's colour map: its left half (u < 0.5) at alpha `left`, its right half at `right`.
	fn halves(side: u32, left: u8, right: u8) -> Vec<u8> {
		(0..side * side).flat_map(|index| [60, 140, 60, if index % side < side / 2 { left } else { right }]).collect()
	}

	/// A 4 x 4 stud card (the quad of vertex_colour_tests, both faces) lying flat `height` studs over a
	/// white floor, under a SurfaceAppearance (its ColorMap, AlphaMode) or none, at `transparency`; the
	/// sun overhead, or (`lamp`) a shadowed PointLight 4 studs over the card and no sun. No other light.
	fn scene(mesh: &str, card: Option<(Option<(String, u32)>, f32)>, height: f32, at: Vec3, lamp: bool) -> Scene {
		let mut model = ModelBuilder::new("Leaves");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(200.0, 1.0, 200.0), color: [255; 3], material: 272, ..Default::default() });
		if let Some((_, transparency)) = &card {
			let cframe = Affine3A::from_translation(at + Vec3::new(0.0, height, 0.0)) * Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2);
			let part = model.part(&PartSpec { class: "MeshPart", name: "Card".into(), cframe, size: Vec3::new(4.0, 4.0, 0.05), material: 272, transparency: *transparency, ..Default::default() });
			model.set(part, "MeshContent", Content::from_uri(mesh));
			model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
		}
		if lamp {
			let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(at + Vec3::new(0.0, height + 4.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			model.child(carrier, rbx_dom_weak::InstanceBuilder::new("PointLight").with_property("Shadows", true).with_property("Range", 30.0f32).with_property("Brightness", 3.0f32));
		}
		let mut scene = crate::scene::extract::extract(&model.into_document());
		if let Some((Some((map, alpha_mode)), _)) = card {
			let part = scene.parts.iter_mut().find(|part| matches!(part.geometry, crate::scene::Geometry::MeshPart { .. })).unwrap();
			Arc::make_mut(part).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(map),
				normal_map: None,
				metalness_map: None,
				roughness_map: None,
				texture_pack: None,
				alpha_mode,
				color: Vec3::ONE,
				emissive_mask: None,
				emissive_strength: 1.0,
				emissive_tint: Vec3::ONE,
			});
		}
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
		// the sun 6.5 degrees from overhead (`view`)
		(environment.clock_time, environment.geographic_latitude) = (12.0, 30.0);
		if lamp {
			environment.brightness = 0.0;
		}
		scene
	}

	/// The floor where the sun puts the shadow of a card `height` studs over `at`, from between them (the
	/// card out of view, above the camera).
	fn view(at: Vec3, height: f32) -> ViewRequest {
		let sun = environment::sun_direction(12.0, 30.0);
		let target = at - Vec3::new(sun.x, 0.0, sun.z) * (height / sun.y);
		ViewRequest { width: 96, height: 96, camera: CameraSpec::LookAt { eye: (target + Vec3::new(0.0, 2.5, -0.5)).into(), target: target.into(), fov: 110.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() }
	}

	fn levels(images: &[ViewImage]) -> Vec<f32> {
		images[0].rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).collect()
	}

	/// Of the floor an opaque card shades (`none` lit, `opaque` under it), the shares a case leaves dark
	/// (over 0.8 of the opaque card's shadow) and lit (under 0.2).
	fn split(none: &[f32], opaque: &[f32], case: &[f32]) -> (f32, f32) {
		let footprint: Vec<usize> = (0..none.len()).filter(|&index| none[index] - opaque[index] > 30.0).collect();
		assert!(footprint.len() > 400, "the opaque card's shadow covers {} pixels", footprint.len());
		let share = |index: usize| (none[index] - case[index]) / (none[index] - opaque[index]);
		let dark = footprint.iter().filter(|&&index| share(index) > 0.8).count() as f32 / footprint.len() as f32;
		let lit = footprint.iter().filter(|&&index| share(index) < 0.2).count() as f32 / footprint.len() as f32;
		(dark, lit)
	}

	/// A SurfaceAppearance in AlphaMode Transparency casts the sun's and a shadowed light's shadow where its
	/// colour map's alpha times 1 - Transparency is over 0.252, its full shadow there and none elsewhere
	/// (fixtures/leaf-shadows.luau in a Roblox 0.747 client): a card opaque on one half shades the floor
	/// under that half alone, at alpha 40 of 255 on the other half and at Transparency 0.7 with alpha 200
	/// there (0.235: alpha alone would cast); in AlphaMode Overlay alpha cuts nothing. ror cast no sun
	/// shadow from such a card and a light's over the whole card. A card out of view whose shadow the view
	/// shows: its colour map, read for nothing else, is held for the shadow map from the level the shadow
	/// map's texels pick (render::residency), not whole, and cuts the shadow as the whole map does.
	#[test]
	fn a_surface_appearances_alpha_cuts_its_shadow_in_view_and_out() {
		let state = std::env::temp_dir().join(format!("ror-shadow-cutout-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let uri = |path: &std::path::Path| format!("file:///{}", path.display().to_string().replace('\\', "/"));
		let mesh_path = state.join("card.mesh");
		std::fs::write(&mesh_path, super::vertex_colour_tests::quad_mesh([255; 4])).unwrap();
		let mesh = uri(&mesh_path);
		let map = |left: u8, right: u8| {
			let path = state.join(format!("halves-{left}-{right}.png"));
			crate::output::write_png(&path, 64, 64, &halves(64, left, right), false).unwrap();
			uri(&path)
		};
		const TRANSPARENCY: u32 = 1;
		const OVERLAY: u32 = 0;
		let draw = |card: Option<(Option<(String, u32)>, f32)>, height: f32, at: Vec3, lamp: bool| {
			let (images, stats) = renderer.render(&scene(&mesh, card, height, at, lamp), &[view(at, if lamp { 0.0 } else { height })]).unwrap();
			assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "missing {:?}", stats.missing);
			levels(&images)
		};
		for lamp in [false, true] {
			let by = if lamp { "a PointLight" } else { "the sun" };
			let none = draw(None, 6.0, Vec3::ZERO, lamp);
			let opaque = draw(Some((None, 0.0)), 6.0, Vec3::ZERO, lamp);
			let cases = [
				("alpha 255 | 40", Some((map(255, 40), TRANSPARENCY)), 0.0, true),
				("alpha 255 | 200 at Transparency 0.7", Some((map(255, 200), TRANSPARENCY)), 0.7, true),
				("Overlay, alpha 255 | 0", Some((map(255, 0), OVERLAY)), 0.0, false),
			];
			for (label, appearance, transparency, cut) in cases {
				let (dark, lit) = split(&none, &opaque, &draw(Some((appearance, transparency)), 6.0, Vec3::ZERO, lamp));
				if cut {
					assert!((0.35..0.65).contains(&dark) && (0.35..0.65).contains(&lit), "{by}, {label}: {dark} of the card's shadow dark, {lit} lit; half of it each expected");
				} else {
					assert!(dark > 0.95, "{by}, {label}: {dark} of the card's shadow dark; all of it expected");
				}
			}
		}

		// out of view, its map's levels through the feedback (every render's, however few maps)
		renderer.residency_from.store(0, std::sync::atomic::Ordering::Relaxed);
		// a 1024 x 1024 map with its mips, as a kept texture file the cache finds for the card's ColorMap
		let mut image = ror_image::Image::from_rgba8(1024, 1024, halves(1024, 255, 0));
		ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true);
		let path = state.join("halves.tex");
		assert!(texture_files::write(&path, &image, TextureUse::Color));
		let map_uri = "rbxassetid://454545";
		let key = transcode::image_key(map_uri);
		let texture = renderer.textures.get_or_load(&(content_key(&key), TextureUse::Color), || texture_files::open(&path, &key).ok_or_else(|| anyhow::anyhow!("the texture file"))).unwrap();
		assert!(texture.held().is_none());
		let at = Vec3::new(0.0, 0.0, 30.0);
		let none = draw(None, 20.0, at, false);
		let opaque = draw(Some((None, 0.0)), 20.0, at, false);
		let (dark, lit) = split(&none, &opaque, &draw(Some((Some((map_uri.to_string(), TRANSPARENCY)), 0.0)), 20.0, at, false));
		let held = texture.held().expect("its levels read for the shadow");
		assert!(held.base > 0 && held.base <= texture.max_base(), "held from level {} of {}", held.base, texture.max_base());
		assert!((0.35..0.65).contains(&dark) && (0.35..0.65).contains(&lit), "out of view: {dark} of the card's shadow dark, {lit} lit; half of it each expected");
		drop(texture);
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod camera_reuse_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Content, Vector3};

	/// A renderer reading Roblox's built-in content from an empty folder, as on a machine without Studio or
	/// the Player (CI's): the default sky's textures and the sun's cannot load wherever the tests run.
	fn renderer(state: &std::path::Path) -> Renderer {
		let gpu = crate::render::test_gpu();
		let no_install = state.join("no-roblox-install");
		std::fs::create_dir_all(&no_install).unwrap();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, studio_content: Some(no_install), ..ror_assets::AssetConfig::new(state) });
		Renderer::new(gpu, assets).expect("a renderer")
	}

	fn view(eye: [f32; 3]) -> ViewRequest {
		ViewRequest { width: 160, height: 96, camera: CameraSpec::LookAt { eye, target: [0.0, 1.0, 0.0], fov: 70.0 }, ..Default::default() }
	}

	/// A floor, a row of blocks, a mesh that cannot load (offline) and a decal whose image cannot load.
	fn scene() -> Scene {
		let mut model = ModelBuilder::new("Yard");
		let part = |name: &str, at: Vec3, size: Vec3| PartSpec { name: name.into(), cframe: Affine3A::from_translation(at), size, ..Default::default() };
		model.part(&part("Floor", Vec3::new(0.0, -0.5, 0.0), Vec3::new(40.0, 1.0, 40.0)));
		for index in 0..6 {
			model.part(&part(&format!("Block{index}"), Vec3::new(index as f32 * 4.0 - 10.0, 1.0, 2.0), Vec3::new(2.0, 2.0, 2.0)));
		}
		let broken = model.part(&PartSpec { class: "MeshPart", ..part("Broken", Vec3::new(4.0, 1.5, -4.0), Vec3::splat(3.0)) });
		model.set(broken, "MeshContent", Content::from_uri("rbxassetid://4040404"));
		model.set(broken, "InitialSize", Vector3::new(3.0, 3.0, 3.0));
		let signed = model.part(&part("Sign", Vec3::new(-4.0, 1.5, -4.0), Vec3::new(3.0, 3.0, 0.4)));
		model.child(signed, InstanceBuilder::new("Decal").with_property("TextureContent", Content::from_uri("rbxassetid://5050505")));
		crate::scene::extract::extract(&model.into_document())
	}

	/// A renderer that has drawn a plain floor. A process's first render does work once for every renderer
	/// after it (4 s on a desktop's software adapter), which the tests below would count against the 30 s
	/// their renders must follow each other within.
	fn warmed(state: &std::path::Path) -> Renderer {
		let renderer = renderer(state);
		let mut model = ModelBuilder::new("Warm");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(40.0, 1.0, 40.0), ..Default::default() });
		renderer.render(&crate::scene::extract::extract(&model.into_document()), &[view([14.0, 9.0, -16.0])]).unwrap();
		renderer
	}

	fn assert_fresh(state: &std::path::Path, scene: &Scene, views: &[ViewRequest], (images, missing): (&[ViewImage], &[String]), label: &str) {
		let (fresh, stats) = renderer(state).render(scene, views).unwrap();
		assert!(images.iter().zip(&fresh).all(|(image, fresh)| image.rgba == fresh.rgba), "{label}: other pixels than a fresh renderer's");
		assert_eq!(missing, stats.missing, "{label}: other failures than a fresh renderer reports");
	}

	/// A scene drawn again from another camera takes its preparation from the render before, also when
	/// some of its loads failed (reported again, as a fresh preparation reports them; until they are
	/// retried: prepared::retry_tests), and an edited one from its second preparation on; every render
	/// draws and reports what a fresh renderer does. Before, a scene with one asset that could not load
	/// was prepared anew for every camera.
	///
	/// The renders follow each other and are compared with fresh renderers' after: the preparation stands
	/// for a fresh one only until the failed loads are retried, 30 s after the first render met them, and a
	/// renderer takes longer than that to start on CI's software adapter (its pipelines are compiled on the
	/// CPU), so one started between two renders had the second prepare the scene again.
	#[test]
	fn a_scene_drawn_from_a_moving_camera_reuses_its_preparation_through_failed_loads() {
		let state = std::env::temp_dir().join(format!("ror-camera-reuse-test-{}", std::process::id()));
		let renderer = warmed(&state);
		let scene = scene();
		let mut edited = scene.clone();
		Arc::make_mut(&mut edited.parts[2]).cframe.translation.y += 1.5;
		let mut drawn = Vec::new();
		let started = std::time::Instant::now();
		let first_views = vec![view([14.0, 9.0, -16.0])];
		let (first, first_stats) = renderer.render(&scene, &first_views).unwrap();
		assert!(!first_stats.geometry_reused);
		assert!(first_stats.missing.iter().any(|reason| reason.contains("4040404")) && first_stats.missing.iter().any(|reason| reason.contains("5050505")), "the failed loads: {:?}", first_stats.missing);
		drawn.push((&scene, first_views, (first, first_stats.missing.clone()), "the first render".to_string()));
		for (index, eye) in [[-12.0, 7.0, -18.0], [0.0, 14.0, -20.0], [18.0, 5.0, 6.0]].into_iter().enumerate() {
			let views = vec![view(eye), view([eye[2], eye[1], eye[0]])];
			let (images, stats) = renderer.render(&scene, &views).unwrap();
			assert!(stats.geometry_reused, "camera {index}: the preparation kept from the render before ({:.1} s after the first)", started.elapsed().as_secs_f32());
			assert_eq!(stats.missing, first_stats.missing, "camera {index}: the same failures reported");
			drawn.push((&scene, views, (images, stats.missing), format!("camera {index}")));
		}
		// an edited scene is prepared again, then kept once it is drawn again unchanged (its failed parts
		// are prepared anew each time it is prepared, which made it a moving scene for good)
		let mut reused = Vec::new();
		for (index, eye) in [[10.0, 9.0, -14.0], [-9.0, 9.0, -14.0], [0.0, 12.0, -18.0], [6.0, 6.0, -12.0]].into_iter().enumerate() {
			let views = vec![view(eye)];
			let (images, stats) = renderer.render(&edited, &views).unwrap();
			assert_eq!(stats.missing, first_stats.missing);
			reused.push(stats.geometry_reused);
			drawn.push((&edited, views, (images, stats.missing), format!("the edited scene, render {index}")));
		}
		assert_eq!(reused, [false, false, true, true], "the edited scene kept from its second preparation ({:.1} s after the first render)", started.elapsed().as_secs_f32());
		for (scene, views, (images, missing), label) in &drawn {
			assert_fresh(&state, scene, views, (images, missing), label);
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A render reports each failed load once, however many views it draws. Each view loads the sun's
	/// texture for itself, and without Roblox's built-in content (`renderer`) a render of three views
	/// reported the sun's texture missing three times: a scene drawn again from two cameras reported more
	/// failures than its first render from one.
	#[test]
	fn a_render_reports_each_failure_once_however_many_views_it_draws() {
		let state = std::env::temp_dir().join(format!("ror-failures-once-test-{}", std::process::id()));
		let renderer = renderer(&state);
		let scene = scene();
		let views = [[14.0, 9.0, -16.0], [-12.0, 7.0, -18.0], [0.0, 14.0, -20.0]].map(view);
		let (_, three) = renderer.render(&scene, &views).unwrap();
		let sun = three.missing.iter().filter(|reason| reason.contains("sky/sun.jpg")).count();
		assert_eq!(sun, 1, "the sun's texture reported missing once: {:?}", three.missing);
		let (_, one) = renderer.render(&scene, &views[..1]).unwrap();
		assert_eq!(three.missing, one.missing, "three views report what one does");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Shadowed local lights' maps take their layers in the scene's order, so cameras that reach the
	/// same lights draw with the same maps whichever is nearest; they draw what a fresh renderer draws.
	/// Before, each camera's nearest-first order made maps of their own, kept for none.
	///
	/// The renders follow each other and are compared with fresh renderers' after: maps are kept once seen
	/// twice within 30 s, which a renderer started between the two renders outlasted on CI.
	#[test]
	fn cameras_reaching_the_same_lights_share_their_shadow_maps() {
		let state = std::env::temp_dir().join(format!("ror-camera-shadows-test-{}", std::process::id()));
		let renderer = warmed(&state);
		let mut model = ModelBuilder::new("Hall");
		let part = |name: &str, at: Vec3, size: Vec3| PartSpec { name: name.into(), cframe: Affine3A::from_translation(at), size, ..Default::default() };
		model.part(&part("Floor", Vec3::new(0.0, -0.5, 0.0), Vec3::new(60.0, 1.0, 30.0)));
		for (index, x) in [-20.0f32, 0.0, 20.0].into_iter().enumerate() {
			model.part(&part(&format!("Pillar{index}"), Vec3::new(x + 3.0, 2.0, 2.0), Vec3::new(1.0, 4.0, 1.0)));
			let lamp = model.part(&PartSpec { cast_shadow: false, ..part(&format!("Lamp{index}"), Vec3::new(x, 6.0, 0.0), Vec3::splat(0.5)) });
			model.child(lamp, InstanceBuilder::new("PointLight").with_property("Shadows", true).with_property("Range", 16.0f32).with_property("Brightness", 3.0f32));
		}
		let mut scene = crate::scene::extract::extract(&model.into_document());
		// no sun: its cascades follow the camera
		scene.environment.global_shadows = false;
		scene.environment.clock_time = 0.0;
		let (left, right) = (view([-45.0, 14.0, -30.0]), view([45.0, 14.0, -30.0]));
		let mut drawn = Vec::new();
		for (round, request) in [&left, &right, &left, &right].into_iter().enumerate() {
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(request)).unwrap();
			if round == 1 {
				assert_eq!(renderer.shadows.stats().0, 1, "the second camera's maps are the first's, kept once seen twice");
			}
			drawn.push((request, (images, stats.missing)));
		}
		for (round, (request, (images, missing))) in drawn.iter().enumerate() {
			assert_fresh(&state, &scene, std::slice::from_ref(*request), (images, missing), &format!("render {round}"));
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Direct renderer wiring: `load_mesh` on an offline view must isolate its failure from the shared online cache.
	#[test]
	fn offline_load_mesh_does_not_poison_online_failure_cache() {
		let state = std::env::temp_dir().join(format!("ror-offline-mesh-wiring-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let config = ror_assets::AssetConfig { offline: false, store: None, studio_content: Some(state.join("no-studio")), ..ror_assets::AssetConfig::new(&state) };
		let online_assets = ror_assets::Assets::new(config);
		let online = Renderer::new(gpu, online_assets.clone()).expect("a renderer");
		let offline = online.with_assets(online_assets.with_offline(true));
		assert!(offline.load_mesh("rbxassetid://8080808").is_err());
		assert_eq!(online.meshes.failures(), (None, 0), "offline load_mesh miss poisoned online failure cache");
		assert!(online.meshes.failures_for(true).0.is_some(), "offline load_mesh miss should be remembered for offline callers");
		drop((offline, online));
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A silent offline fallback (`SurfaceAppearance.TexturePack` uncached while `ColorMap` is local,
	/// leaving `stats.missing` empty) must neither publish shared `PreparedPart`s nor cache a
	/// non-`offline_only` `PreparedGeometry`, while still letting repeat offline views reuse it.
	#[test]
	fn silent_offline_pack_fallback_does_not_poison_online_preparations() {
		let state = std::env::temp_dir().join(format!("ror-silent-pack-fallback-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let config = ror_assets::AssetConfig { offline: false, store: None, studio_content: Some(state.join("no-studio")), ..ror_assets::AssetConfig::new(&state) };
		let online_assets = ror_assets::Assets::new(config);
		let online = Renderer::new(gpu, online_assets.clone()).expect("a renderer");
		let offline = online.with_assets(online_assets.with_offline(true));
		let png = state.join("albedo.png");
		crate::output::write_png(&png, 4, 4, &[200, 100, 50, 255].repeat(16), true).unwrap();
		let png_uri = format!("file:///{}", png.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let mut model = ModelBuilder::new("SilentPack");
		model.part(&PartSpec { name: "Block".into(), size: Vec3::splat(4.0), ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
			color_map: Some(png_uri),
			normal_map: None,
			metalness_map: None,
			roughness_map: None,
			texture_pack: Some("rbxassetid://9090909".into()),
			alpha_mode: 0,
			color: Vec3::ONE,
			emissive_mask: None,
			emissive_strength: 0.0,
			emissive_tint: Vec3::ONE,
		});
		let request = view([8.0, 6.0, -10.0]);
		let (_, first) = offline.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert!(first.missing.iter().all(|m| !m.contains("9090909")), "pack fallback is silent: {:?}", first.missing);
		let key = super::scene_key(&scene).unwrap();
		assert!(online.preparations.get(&key, online.dropped_failures(false), false).is_none(), "silent offline pack fallback poisoned online Preparations");
		assert_eq!(online.preparations.part_stats().0, 0, "silent offline pack fallback published shared PreparedParts");
		let (_, second) = offline.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert!(second.geometry_reused, "offline view should still reuse its isolated offline_only preparation");
		drop((offline, online));
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A clip frame prepared on an offline view that hit a silent fallback (uncached `TexturePack`
	/// with a local `ColorMap`, leaving `stats.missing` empty) must NOT mark its parts reusable in
	/// `ClipKept`, so subsequent frames re-prepare instead of carrying the silent fallback forward.
	#[test]
	fn silent_offline_pack_fallback_is_not_reusable_across_clip_frames() {
		let state = std::env::temp_dir().join(format!("ror-silent-pack-clip-reusable-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let config = ror_assets::AssetConfig { offline: false, store: None, studio_content: Some(state.join("no-studio")), ..ror_assets::AssetConfig::new(&state) };
		let online_assets = ror_assets::Assets::new(config);
		let online = Renderer::new(gpu, online_assets.clone()).expect("a renderer");
		let offline = online.with_assets(online_assets.with_offline(true));
		let png = state.join("albedo.png");
		crate::output::write_png(&png, 4, 4, &[200, 100, 50, 255].repeat(16), true).unwrap();
		let png_uri = format!("file:///{}", png.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let build_scene = |mover_x: f32| {
			let mut model = ModelBuilder::new("SilentPackClip");
			model.part(&PartSpec { name: "Block".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, 0.0)), size: Vec3::splat(4.0), ..Default::default() });
			model.part(&PartSpec { name: "Mover".into(), cframe: Affine3A::from_translation(Vec3::new(mover_x, 2.0, 0.0)), size: Vec3::splat(2.0), ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(png_uri.clone()),
				normal_map: None,
				metalness_map: None,
				roughness_map: None,
				texture_pack: Some("rbxassetid://9090910".into()),
				alpha_mode: 0,
				color: Vec3::ONE,
				emissive_mask: None,
				emissive_strength: 0.0,
				emissive_tint: Vec3::ONE,
			});
			scene
		};
		let request = view([8.0, 6.0, -14.0]);
		let kept = super::ClipKept::new();
		let scene0 = build_scene(0.0);
		let sky0 = offline.prepare_environment(&scene0);
		let (mut views0, mut stats0) = offline.submit_frame(&scene0, std::slice::from_ref(&request), sky0, None, Default::default(), None, Some((&kept, false))).unwrap();
		views0.pop().unwrap().finish(&mut stats0).unwrap();
		assert!(stats0.missing.iter().all(|m| !m.contains("9090910")), "pack fallback is silent: {:?}", stats0.missing);
		let key0 = super::scene_key(&scene0).unwrap();
		let flags = vec![0u8; scene0.parts.len()];
		assert!(
			!kept.geometry.last.lock().as_ref().expect("clip keeps last frame").same_part(0, &key0, &flags),
			"silent offline fallback part must not be marked reusable in ClipKept"
		);
		// Frame 1 moves part 1 while part 0 stays identical: because part 0 was not marked reusable,
		// all parts are prepared anew (`moving == false`), not patched from frame 0's fallback.
		let scene1 = build_scene(5.0);
		let sky1 = offline.prepare_environment(&scene1);
		let (mut views1, mut stats1) = offline.submit_frame(&scene1, std::slice::from_ref(&request), sky1, None, Default::default(), None, Some((&kept, false))).unwrap();
		views1.pop().unwrap().finish(&mut stats1).unwrap();
		assert!(
			!kept.geometry.last.lock().as_ref().expect("clip keeps last frame").moving,
			"frame 1 reused silent-fallback part 0 from ClipKept instead of re-preparing it"
		);
		drop((offline, online));
		let _ = std::fs::remove_dir_all(&state);
	}

	/// An offline `Transcode::Image` fallback to a cached uploaded PNG must neither write a `.tex`
	/// file under the transcode key to disk/store nor promote the fallback into the shared texture cache.
	#[test]
	fn offline_transcode_fallback_does_not_poison_online_textures_or_store() {
		let state = std::env::temp_dir().join(format!("ror-offline-transcode-fallback-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let store = state.join("store");
		let gpu = crate::render::test_gpu();
		let config = ror_assets::AssetConfig { offline: false, store: Some(store.clone()), studio_content: Some(state.join("no-studio")), ..ror_assets::AssetConfig::new(&state) };
		let online_assets = ror_assets::Assets::new(config);
		let online = Renderer::new(gpu, online_assets.clone()).expect("a renderer");
		let offline = online.with_assets(online_assets.with_offline(true));
		let png = state.join("upload.png");
		crate::output::write_png(&png, 4, 4, &[40, 180, 90, 255].repeat(16), true).unwrap();
		let raw_uri = "rbxassetid://7070707";
		let mut hash = 0x6c62272e07bb014262b821756295c58du128;
		for byte in raw_uri.as_bytes() {
			hash ^= *byte as u128;
			hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
		}
		let hex = format!("{hash:032x}");
		let asset_dir = state.join("assets").join(&hex[..2]);
		std::fs::create_dir_all(&asset_dir).unwrap();
		let png_bytes = std::fs::read(&png).unwrap();
		std::fs::write(asset_dir.join(format!("{hex}.bin")), &png_bytes).unwrap();
		let meta = serde_json::json!({"uri": raw_uri, "fetched_at": 1, "source_host": "test", "size": png_bytes.len(), "kind": "png"});
		std::fs::write(asset_dir.join(format!("{hex}.json")), meta.to_string()).unwrap();

		let transcode_key = super::transcode::image_key(raw_uri);
		let tex_count_before = online.textures.len();
		let fallback_tex = offline.load_texture(&transcode_key, super::resources::TextureUse::Color).expect("falls back to cached upload PNG");
		assert_eq!((fallback_tex.width, fallback_tex.height), (4, 4));
		assert_eq!(online.textures.len(), tex_count_before, "offline transcode fallback must not promote into shared online texture cache");
		assert!(online.textures.get(&(super::content_key(&transcode_key), super::resources::TextureUse::Color)).is_none());
		let store_tex_dir = store.join("textures").join(env!("ROR_TEXTURE_CODE"));
		let store_tex_count = std::fs::read_dir(&store_tex_dir).map(|entries| entries.flatten().filter(|e| e.path().extension().is_some_and(|ext| ext == "tex")).count()).unwrap_or(0);
		assert_eq!(store_tex_count, 0, "offline transcode fallback must not publish a .tex file to the shared store");
		drop((fallback_tex, offline, online));
		let _ = std::fs::remove_dir_all(&state);
	}

	/// `fit_vram_with` relieves idle caches whenever measured adapter-wide free VRAM falls below
	/// `GPU_HEADROOM_PERCENT` (even when this process's Vulkan budget still looks ample or is absent),
	/// keeps caches under healthy headroom, and never changes rendered pixels or blocks a solo draw.
	#[test]
	fn fit_vram_relieves_idle_caches_on_adapter_free_headroom_and_preserves_pixels() {
		const GIB: u64 = 1 << 30;
		let state = std::env::temp_dir().join(format!("ror-fit-vram-headroom-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		let renderer = warmed(&state);
		let scene = scene();
		let request = view([14.0, 9.0, -16.0]);
		let gate = crate::budget::GpuGate::default();
		let (before, _) = renderer.render_gated(&scene, std::slice::from_ref(&request), Some(&gate)).unwrap();
		let prep_before = renderer.preparations.stats().0;
		let reuse_before = renderer.reuse.stats().0;
		assert!(prep_before > 0 && reuse_before > 0, "render must populate idle caches (prep={prep_before}, reuse={reuse_before})");

		let ample_vram = Some(super::gpu::Vram { usage: GIB, budget: 16 * GIB });
		let healthy_free = Some(crate::budget::GpuFree { free: 10 * GIB, total: 16 * GIB });
		let low_free = Some(crate::budget::GpuFree { free: 512 << 20, total: 16 * GIB });

		// Healthy adapter headroom and ample budget keep idle caches warm.
		assert_eq!(renderer.fit_vram_with(ample_vram, healthy_free), ample_vram);
		assert_eq!(renderer.preparations.stats().0, prep_before, "healthy headroom must not evict preparations");
		assert_eq!(renderer.reuse.stats().0, reuse_before, "healthy headroom must not evict reused targets");

		// Low adapter-wide free VRAM (< 10% headroom) relieves idle caches even while process budget usage is low.
		assert_eq!(renderer.fit_vram_with(ample_vram, low_free), ample_vram);
		assert_eq!(renderer.preparations.stats().0, 0, "low adapter free VRAM must relieve idle preparations");
		assert_eq!(renderer.reuse.stats().0, 0, "low adapter free VRAM must relieve unheld targets");

		// Next gated render draws byte-identical pixels without waiting and repopulates caches.
		let (after, _) = renderer.render_gated(&scene, std::slice::from_ref(&request), Some(&gate)).unwrap();
		assert_eq!(before[0].rgba, after[0].rgba, "cache relief must not change rendered pixels");
		assert_eq!(gate.status()["waited"], 0, "solo draw must never wait after cache relief");
		assert!(renderer.preparations.stats().0 > 0 && renderer.reuse.stats().0 > 0);

		// Works even without VK_EXT_memory_budget (`vram == None`) when adapter counter shows low free VRAM.
		assert_eq!(renderer.fit_vram_with(None, low_free), None);
		assert_eq!(renderer.preparations.stats().0, 0, "adapter free relief must still run when vram budget is None");
		assert_eq!(renderer.reuse.stats().0, 0);

		// Process VRAM budget threshold (> 85% usage) still relieves when adapter counter is absent.
		let _ = renderer.render_gated(&scene, std::slice::from_ref(&request), Some(&gate)).unwrap();
		assert!(renderer.preparations.stats().0 > 0);
		let high_vram = Some(super::gpu::Vram { usage: 15 * GIB, budget: 16 * GIB });
		assert_eq!(renderer.fit_vram_with(high_vram, None), high_vram);
		assert_eq!(renderer.preparations.stats().0, 0, "process budget over 85% must still relieve idle caches");

		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Deterministic A -> B -> A race tests (zero sleeps):
	/// 1. File moves A -> B between `load_texture`'s snapshot and `std::fs::read`, then returns to A.
	/// 2. File moves A -> B between `SceneKey` (`prepare_environment`) and `load_texture` (`render_with`),
	///    then returns to A.
	/// In both cases, what is kept under A's key must never be B, so the final render on A draws A.
	#[test]
	fn a_to_b_to_a_during_render_never_caches_b_under_a() {
		let state = std::env::temp_dir().join(format!("ror-race-render-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let tex = state.join("tex.png");
		let red_png = {
			let p = state.join("red.png");
			crate::output::write_png(&p, 8, 8, &[235, 30, 30, 255].repeat(64), true).unwrap();
			std::fs::read(&p).unwrap()
		};
		let blue_png = {
			let p = state.join("blue.png");
			crate::output::write_png(&p, 8, 8, &[30, 60, 235, 255].repeat(64), true).unwrap();
			std::fs::read(&p).unwrap()
		};
		std::fs::write(&tex, &red_png).unwrap();
		let tex_uri = format!("file:///{}", tex.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let mut model = ModelBuilder::new("RacePanel");
		model.part(&PartSpec { name: "Panel".into(), size: Vec3::new(6.0, 6.0, 1.0), color: [255, 255, 255], ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		Arc::make_mut(&mut scene.parts[0]).surface_appearance = Some(crate::scene::SurfaceAppearance {
			color_map: Some(tex_uri),
			normal_map: None,
			metalness_map: None,
			roughness_map: None,
			texture_pack: None,
			alpha_mode: 0,
			color: Vec3::ONE,
			emissive_mask: None,
			emissive_strength: 0.0,
			emissive_tint: Vec3::ONE,
		});
		let request = ViewRequest { width: 64, height: 64, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -8.0], target: [0.0; 3], fov: 50.0 }, ..Default::default() };

		// Capture clean reference renders for A (red) and B (blue).
		let ref_renderer = renderer(&state.join("ref"));
		let (want_red, _) = ref_renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		std::fs::write(&tex, &blue_png).unwrap();
		let (want_blue, _) = ref_renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert_ne!(want_red[0].rgba, want_blue[0].rgba, "red and blue reference renders must differ");
		drop(ref_renderer);

		// Case 1: A -> B right after `load_texture`'s snapshot and before `std::fs::read`, then back to A.
		let r1 = renderer(&state.join("case1"));
		std::fs::write(&tex, &red_png).unwrap();
		let hook_tex = tex.clone();
		let hook_blue = blue_png.clone();
		super::set_test_read_hook(tex.clone(), move || {
			std::fs::write(&hook_tex, &hook_blue).unwrap();
		});
		let (during1, _) = r1.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert_eq!(during1[0].rgba, want_blue[0].rgba, "case 1: hook should have swapped file to B before read");
		std::fs::write(&tex, &red_png).unwrap();
		let (after1, stats1) = r1.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert!(!stats1.geometry_reused, "case 1: moving render must not have been kept in Preparations");
		assert_eq!(after1[0].rgba, want_red[0].rgba, "case 1: returning to A must draw A, not cached B");
		drop(r1);

		// Case 2: A -> B between `SceneKey` (`prepare_environment`) and `load_texture` (`render_with`),
		// then back to A (even though `load_texture` cleanly read B with a valid stamp for B!).
		let r2 = renderer(&state.join("case2"));
		std::fs::write(&tex, &red_png).unwrap();
		let sky_keyed_at_a = r2.prepare_environment(&scene);
		std::fs::write(&tex, &blue_png).unwrap();
		let (during2, _) = r2.render_with(&scene, std::slice::from_ref(&request), sky_keyed_at_a).unwrap();
		assert_eq!(during2[0].rgba, want_blue[0].rgba, "case 2: render saw B during load_texture");
		std::fs::write(&tex, &red_png).unwrap();
		let (after2, stats2) = r2.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert!(!stats2.geometry_reused, "case 2: mismatched SceneKey(A) + texture(B) must not be kept in Preparations");
		assert_eq!(after2[0].rgba, want_red[0].rgba, "case 2: returning to A must draw A, not cached B");
		drop(r2);

		let _ = std::fs::remove_dir_all(&state);
	}

	/// Deterministic A -> B -> A race test on `load_mesh` (zero sleeps): a local mesh that moves
	/// between snapshot and read is never kept in `Renderer::meshes` under A's key.
	#[test]
	fn a_to_b_to_a_on_local_mesh_never_caches_b_under_a() {
		let state = std::env::temp_dir().join(format!("ror-race-mesh-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let mesh_bytes = |extent: f32| -> Vec<u8> {
			format!("version 1.01\n1\n[0,0,0][0,0,1][0,0,0][{extent},0,0][0,0,1][1,0,0][0,{extent},0][0,0,1][0,1,0]\n").into_bytes()
		};
		let mesh_a = mesh_bytes(1.0);
		let mesh_b = mesh_bytes(5.0);
		let path = state.join("shape.mesh");
		std::fs::write(&path, &mesh_a).unwrap();
		let uri = format!("file:///{}", path.display().to_string().replace('\\', "/").replace(' ', "%20"));

		let r = renderer(&state.join("state"));
		let hook_path = path.clone();
		let hook_b = mesh_b.clone();
		super::set_test_read_hook(path.clone(), move || {
			std::fs::write(&hook_path, &hook_b).unwrap();
		});
		let during = r.load_mesh(&uri).unwrap();
		assert!((during.bounds.1.x - 5.0).abs() < 1e-3, "hook should have swapped mesh to B (bounds.x=5)");
		assert_eq!(during.proof, super::resources::StampProof::Moved, "moving mesh must be marked Moved");
		std::fs::write(&path, &mesh_a).unwrap();
		let after = r.load_mesh(&uri).unwrap();
		assert!((after.bounds.1.x - 1.0).abs() < 1e-3, "returning to A must load A (bounds.x=1), not cached B");
		assert!(matches!(after.proof, super::resources::StampProof::Proven(_)), "settled load of A must be Proven");
		drop(r);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Guards stamping the local asset URI in `Renderer::load_union`: rewriting a local CSGMDL union
	/// under the same path must reload it instead of serving the cached earlier union geometry.
	#[test]
	fn rewriting_local_union_reloads_union_mesh() {
		fn csgmdl_v2(extent_x: f32) -> Vec<u8> {
			const CSG_KEY: [u8; 31] = [86, 46, 110, 88, 49, 32, 48, 4, 52, 105, 12, 119, 12, 1, 94, 0, 26, 96, 55, 105, 29, 82, 43, 7, 79, 36, 89, 101, 83, 4, 122];
			let mut b = Vec::new();
			b.extend_from_slice(b"CSGMDL");
			b.extend_from_slice(&2u32.to_le_bytes());
			b.extend_from_slice(&[0xAB; 32]);
			b.extend_from_slice(&4u32.to_le_bytes());
			b.extend_from_slice(&84u32.to_le_bytes());
			for (i, pos) in [[0.0f32, 0.0, 0.0], [extent_x, 0.0, 0.0], [extent_x, 1.0, 0.0], [0.0, 1.0, 0.0]].iter().enumerate() {
				for v in pos {
					b.extend_from_slice(&v.to_le_bytes());
				}
				for v in [0.0f32, 0.0, 1.0] {
					b.extend_from_slice(&v.to_le_bytes());
				}
				b.extend_from_slice(&[200, 100, 50, 255]);
				b.extend_from_slice(&((i as u32 % 6) + 1).to_le_bytes());
				b.extend_from_slice(&0.0f32.to_le_bytes());
				b.extend_from_slice(&0.0f32.to_le_bytes());
				b.extend_from_slice(&[0u8; 16]);
				for v in [1.0f32, 0.0, 0.0] {
					b.extend_from_slice(&v.to_le_bytes());
				}
				b.extend_from_slice(&[0u8; 16]);
			}
			b.extend_from_slice(&6u32.to_le_bytes());
			for idx in [0u32, 1, 2, 0, 2, 3] {
				b.extend_from_slice(&idx.to_le_bytes());
			}
			for (offset, byte) in b.iter_mut().enumerate() {
				*byte ^= CSG_KEY[offset % CSG_KEY.len()];
			}
			b
		}

		let state = std::env::temp_dir().join(format!("ror-union-stamp-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let path = state.join("part.csg");
		let uri = format!("file:///{}", path.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let r = renderer(&state.join("state"));

		std::fs::write(&path, csgmdl_v2(1.0)).unwrap();
		let first = r.load_union(Some(&uri), None).unwrap();
		assert!((first.bounds.1.x - 1.0).abs() < 1e-3, "first union bounds {:?}", first.bounds);

		std::fs::write(&path, csgmdl_v2(5.0)).unwrap();
		let second = r.load_union(Some(&uri), None).unwrap();
		assert!((second.bounds.1.x - 5.0).abs() < 1e-3, "rewriting local union must reload geometry, got {:?}", second.bounds);

		drop(r);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Guards stamping the local URI in `Renderer::texture_pack`: rewriting a local texture-pack XML
	/// under the same path must reload its map references instead of returning the cached pack.
	#[test]
	fn rewriting_local_texture_pack_reloads_pack() {
		let state = std::env::temp_dir().join(format!("ror-pack-stamp-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let path = state.join("pack.xml");
		let uri = format!("file:///{}", path.display().to_string().replace('\\', "/").replace(' ', "%20"));
		let r = renderer(&state.join("state"));

		std::fs::write(&path, b"<roblox><texturepack_version>2</texturepack_version><color>111111</color></roblox>").unwrap();
		let first = r.texture_pack(&uri).unwrap();
		assert_eq!(first.color.as_deref(), Some("rbxassetid://111111"));

		std::fs::write(&path, b"<roblox><texturepack_version>2</texturepack_version><color>222222</color></roblox>").unwrap();
		let second = r.texture_pack(&uri).unwrap();
		assert_eq!(second.color.as_deref(), Some("rbxassetid://222222"), "rewriting local texture pack must reload it");

		drop(r);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Guards stamping local skybox face URIs in `Renderer::environment`: rewriting a local skybox face
	/// PNG must invalidate `self.skies` and `self.environments` and render the new sky colour.
	#[test]
	fn rewriting_local_skybox_face_reloads_sky_environment() {
		let state = std::env::temp_dir().join(format!("ror-sky-stamp-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let make_png = |name: &str, rgba: [u8; 4]| -> Vec<u8> {
			let p = state.join(name);
			crate::output::write_png(&p, 8, 8, &rgba.repeat(64), true).unwrap();
			std::fs::read(&p).unwrap()
		};
		let red = make_png("red.png", [235, 30, 30, 255]);
		let blue = make_png("blue.png", [30, 50, 235, 255]);
		let faces: [std::path::PathBuf; 6] = std::array::from_fn(|i| state.join(format!("face_{i}.png")));
		for p in &faces {
			std::fs::write(p, &red).unwrap();
		}
		let skybox: [String; 6] = std::array::from_fn(|i| format!("file:///{}", faces[i].display().to_string().replace('\\', "/").replace(' ', "%20")));
		let mut scene = crate::scene::Scene::default();
		scene.environment.sky = Some(crate::scene::environment::Sky { skybox, celestial_bodies_shown: false, ..Default::default() });
		let request = ViewRequest { width: 32, height: 32, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -5.0], target: [0.0; 3], fov: 60.0 }, background: Background::Sky, bloom: false, ..Default::default() };

		let r = renderer(&state.join("state"));
		let (first, _) = r.render(&scene, std::slice::from_ref(&request)).unwrap();
		let mid = |rgba: &[u8]| {
			let i = ((16 * 32 + 16) * 4) as usize;
			(rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32)
		};
		let (r1, _, b1) = mid(&first[0].rgba);
		assert!(r1 > b1 + 40, "initial skybox should be red, got {r1},{b1}");

		for p in &faces {
			std::fs::write(p, &blue).unwrap();
		}
		let (second, _) = r.render(&scene, std::slice::from_ref(&request)).unwrap();
		let (r2, _, b2) = mid(&second[0].rgba);
		assert!(b2 > r2 + 40, "rewritten local skybox faces must redraw blue, got {r2},{b2}");

		drop(r);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod blend_order_tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, ContentId, NumberRange, NumberSequence, NumberSequenceKeypoint, Variant};

	/// A see-through part (Transparency 0.3, its top face at y 0, 40 x 24 studs) with four unlit red beams
	/// 0.1 over the face at x -15, -5, 5 and 15, each facing the camera and ordered whole: a beam is drawn over the face or
	/// under it by its depth along the camera's forward axis against the part's middle's, not by their
	/// distances from the eye (blend-depth-order, the engine 0.747, rows x -15 / -5 / 5 / 15). By distance
	/// the second and third cases read over in every row and the fourth under at x -15 and 15. Drawn once
	/// with the beams and once with an emitter's still particles in their place: both take the one key.
	#[test]
	fn a_see_through_part_and_effects_order_by_depth_along_the_cameras_forward_axis() {
		let state = std::env::temp_dir().join(format!("ror-blend-order-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		const ROWS: [f32; 4] = [-15.0, -5.0, 5.0, 15.0];
		// each row a little further across, so that from a low eye no beam stands before the next row's
		const STAGGER: f32 = 0.6;
		std::fs::create_dir_all(&state).unwrap();
		let white = state.join("white.png");
		crate::output::write_png(&white, 8, 8, &[255u8; 4].repeat(64), false).unwrap();
		let white = format!("file:///{}", white.display().to_string().replace('\\', "/"));
		let scene = |height: f32, emitters: bool| {
			let mut model = ModelBuilder::new("Order");
			model.part(&PartSpec { name: "Water".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -height * 0.5, 0.0)), size: Vec3::new(40.0, height, 24.0), color: [0, 110, 255], transparency: 0.3, cast_shadow: false, ..Default::default() });
			let red = Color3::new(1.0, 0.0, 0.0);
			for x in ROWS {
				let holder = model.part(&PartSpec { name: "Holder".into(), cframe: Affine3A::from_translation(Vec3::new(x, 0.1, x * STAGGER)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
				if emitters {
					// ten particles that stay where they were emitted: a red square 2 studs across at the holder
					model.child(
						holder,
						InstanceBuilder::new("ParticleEmitter")
							.with_property("Texture", ContentId::from(white.clone()))
							.with_property("Color", ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, red), ColorSequenceKeypoint::new(1.0, red)] })
							.with_property("Size", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 2.0, 0.0), NumberSequenceKeypoint::new(1.0, 2.0, 0.0)] })
							.with_property("Lifetime", NumberRange::new(5.0, 5.0))
							.with_property("Speed", NumberRange::new(0.0, 0.0))
							.with_property("Rate", 2.0f32)
							.with_property("LightEmission", 0.0f32)
							.with_property("LightInfluence", 0.0f32),
					);
					continue;
				}
				let end =|z: f32| InstanceBuilder::new("Attachment").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(Vec3::new(0.0, 0.0, z))));
				let (a0, a1) = (model.child(holder, end(-1.5)), model.child(holder, end(1.5)));
				model.child(
					holder,
					InstanceBuilder::new("Beam")
						.with_property("Attachment0", Variant::Ref(a0))
						.with_property("Attachment1", Variant::Ref(a1))
						.with_property("Color", ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, red), ColorSequenceKeypoint::new(1.0, red)] })
						.with_property("Transparency", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 0.0, 0.0)] })
						.with_property("Width0", 2.0f32)
						.with_property("Width1", 2.0f32)
						.with_property("FaceCamera", true)
						.with_property("LightEmission", 0.0f32)
						.with_property("LightInfluence", 0.0f32),
				);
			}
			crate::scene::extract::extract(&model.into_document())
		};
		const SIZE: (u32, u32) = (320, 180);
		// (the part's height, the eye, where it looks, the field of view, each row over the face)
		let cases: [(f32, [f32; 3], [f32; 3], f32, [bool; 4]); 4] = [
			(180.0, [-32.0, 6.0, 0.0], [0.0, 0.0, 0.0], 50.0, [true, true, true, true]),
			(180.0, [-32.0, 6.0, 0.0], [0.0, 10.0, 0.0], 75.0, [true, false, false, false]),
			(180.0, [-150.0, 12.0, 0.0], [0.0, 0.0, 0.0], 16.0, [true, true, true, false]),
			(0.4, [0.0, 100.0, 0.0], [0.0, 0.0, 0.0], 30.0, [true, true, true, true]),
		];
		for (emitters, (height, eye, target, fov, over)) in [false, true].into_iter().flat_map(|emitters| cases.map(|case| (emitters, case))) {
			let effects = if emitters { "emitters" } else { "beams" };
			let spec = CameraSpec::LookAt { eye, target, fov };
			let view = ViewRequest { width: SIZE.0, height: SIZE.1, camera: spec.clone(), background: Background::Color([0.0, 0.0, 0.0]), bloom: false, ..Default::default() };
			let (images, _) = renderer.render(&scene(height, emitters), &[view]).unwrap();
			let project = spec.place(None, SIZE.0, SIZE.1).view_projection(SIZE.0 as f32 / SIZE.1 as f32);
			let mut read = Vec::new();
			let drawn: Vec<bool> = ROWS
				.iter()
				.map(|&x| {
					let clip = project * Vec3::new(x, 0.1, x * STAGGER).extend(1.0);
					let (column, row) = (((clip.x / clip.w * 0.5 + 0.5) * SIZE.0 as f32) as usize, ((0.5 - clip.y / clip.w * 0.5) * SIZE.1 as f32) as usize);
					let pixel = &images[0].rgba[(row * SIZE.0 as usize + column) * 4..][..3];
					read.push([pixel[0], pixel[1], pixel[2]]);
					assert!(pixel[0] > 60, "{effects}, eye {eye:?} looking at {target:?}: nothing drawn at x {x} ({pixel:?})");
					// its own red, or that under 0.7 of the part's blue
					pixel[2] < 60
				})
				.collect();
			assert_eq!(drawn, over, "{effects}, part {height} studs tall, eye {eye:?} looking at {target:?}: the rows drawn over the face (read {read:?})");
		}
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn the_blend_order_runs_legacy_effects_apart() {
		assert_eq!(blend_runs(&[]), vec![BlendRun::Blended(0..0)]);
		assert_eq!(blend_runs(&[false, false]), vec![BlendRun::Blended(0..2)]);
		assert_eq!(blend_runs(&[true, false]), vec![BlendRun::Blended(0..0), BlendRun::Legacy(0..1), BlendRun::Blended(1..2)]);
		assert_eq!(blend_runs(&[false, true, true, false, true]), vec![BlendRun::Blended(0..1), BlendRun::Legacy(1..3), BlendRun::Blended(3..4), BlendRun::Legacy(4..5)]);
	}

	/// A red square 36 studs out with ZOffset 10 behind a 10 x 10 opaque block 30 studs out: a Roblox
	/// client's occlusion culling drops it where it stands, before its offset would bring it in front
	/// (`fixtures/draw-order.luau` "cull"); one reaching past the block's edge draws over it; without the
	/// block it draws.
	#[test]
	fn an_effect_behind_an_occluder_is_culled_before_its_zoffset() {
		let state = std::env::temp_dir().join(format!("ror-zoffset-cull-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let white = state.join("white.png");
		crate::output::write_png(&white, 8, 8, &[255u8; 4].repeat(64), false).unwrap();
		let white = format!("file:///{}", white.display().to_string().replace('\\', "/"));
		let red = Color3::new(1.0, 0.0, 0.0);
		// (the square's place across the view, whether the block stands)
		let scene = |x: f32, wall: bool| {
			let mut model = ModelBuilder::new("Cull");
			if wall {
				model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 30.0)), size: Vec3::new(10.0, 10.0, 1.0), color: [128, 128, 128], cast_shadow: false, ..Default::default() });
			}
			let holder = model.part(&PartSpec { name: "Holder".into(), cframe: Affine3A::from_translation(Vec3::new(x, 0.0, 36.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			model.child(
				holder,
				InstanceBuilder::new("ParticleEmitter")
					.with_property("Texture", ContentId::from(white.clone()))
					.with_property("Color", ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, red), ColorSequenceKeypoint::new(1.0, red)] })
					.with_property("Size", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 1.0, 0.0), NumberSequenceKeypoint::new(1.0, 1.0, 0.0)] })
					.with_property("Lifetime", NumberRange::new(5.0, 5.0))
					.with_property("Speed", NumberRange::new(0.0, 0.0))
					.with_property("Rate", 2.0f32)
					.with_property("ZOffset", 10.0f32)
					.with_property("LightEmission", 0.0f32)
					.with_property("LightInfluence", 0.0f32),
			);
			crate::scene::extract::extract(&model.into_document())
		};
		const SIZE: (u32, u32) = (160, 120);
		let spec = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 1.0], fov: 50.0 };
		let project = spec.place(None, SIZE.0, SIZE.1).view_projection(SIZE.0 as f32 / SIZE.1 as f32);
		let red_at = |x: f32, wall: bool| {
			let view = ViewRequest { width: SIZE.0, height: SIZE.1, camera: spec.clone(), background: Background::Color([0.0, 0.0, 0.0]), bloom: false, ..Default::default() };
			let (images, _) = renderer.render(&scene(x, wall), &[view]).unwrap();
			let clip = project * Vec3::new(x, 0.0, 36.0).extend(1.0);
			let (column, row) = (((clip.x / clip.w * 0.5 + 0.5) * SIZE.0 as f32) as usize, ((0.5 - clip.y / clip.w * 0.5) * SIZE.1 as f32) as usize);
			let pixel = &images[0].rgba[(row * SIZE.0 as usize + column) * 4..][..3];
			pixel[0] > 120 && pixel[1] < 60
		};
		assert!(red_at(0.0, false), "the square in the open");
		assert!(!red_at(0.0, true), "the square wholly behind the block");
		assert!(red_at(5.0, true), "the square reaching past the block's edge, over it");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn id_pipelines_build_lazily_on_first_id_or_highlight_render() {
		let state = std::env::temp_dir().join(format!("ror-lazy-ids-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		assert!(renderer.ids.get().is_none(), "IdPipelines should not compile before first use");

		let mut model = ModelBuilder::new("Box");
		model.part(&PartSpec { name: "Block".into(), ..Default::default() });
		let scene = crate::scene::extract::extract(&model.into_document());
		let plain = ViewRequest { width: 64, height: 64, msaa: 1, bloom: false, ..Default::default() };
		let _ = renderer.render(&scene, std::slice::from_ref(&plain)).unwrap();
		assert!(renderer.ids.get().is_none(), "plain render without Highlights, --ids, or --layers must leave IdPipelines uncompiled");

		let with_ids = ViewRequest { ids: true, ..plain };
		let (images, _) = renderer.render(&scene, std::slice::from_ref(&with_ids)).unwrap();
		assert!(images[0].ids.is_some());
		assert!(renderer.ids.get().is_some(), "--ids render must lazily initialize IdPipelines");

		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}

#[cfg(test)]
mod star_tests {
	use super::*;

	/// Stars over a black skybox at midnight, seen straight up (notes/render.md "Stars"): drawn only
	/// with CelestialBodiesShown, about 0.6 · StarCount / 2π a steradian above the horizon, StarCount
	/// past 5,000 drawing 5,000's, none at noon.
	#[test]
	fn stars_are_drawn_at_night_with_the_celestial_bodies() {
		let state = std::env::temp_dir().join(format!("ror-star-test-{}", std::process::id()));
		let content = state.join("content");
		std::fs::create_dir_all(content.join("textures")).unwrap();
		crate::output::write_png(&content.join("textures").join("black.png"), 1, 1, &[0, 0, 0, 255], true).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, studio_content: Some(content), ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -500.0, 0.0)), size: Vec3::new(1.0, 1.0, 1.0), ..Default::default() });
		let world = crate::scene::extract::extract(&model.into_document());
		let size = 256u32;
		let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 10.0, 0.0], target: [0.0, 110.0, 0.01], fov: 90.0 }, background: Background::Sky, bloom: false, ..Default::default() };
		let lit = |clock_time: f32, shown: bool, star_count: i32| -> Vec<u8> {
			let mut scene = world.clone();
			scene.environment.clock_time = clock_time;
			scene.environment.sky = Some(crate::scene::environment::Sky {
				skybox: std::array::from_fn(|_| "rbxasset://textures/black.png".into()),
				celestial_bodies_shown: shown,
				sun_angular_size: 0.0,
				moon_angular_size: 0.0,
				star_count,
				..Default::default()
			});
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
			assert!(stats.missing.is_empty(), "{:?}", stats.missing);
			let rgba = &images[0].rgba;
			(0..rgba.len()).step_by(4).map(|index| rgba[index].max(rgba[index + 1]).max(rgba[index + 2])).collect()
		};
		let count = |pixels: &[u8]| pixels.iter().filter(|&&value| value > 2).count();
		let night = lit(0.0, true, 3000);
		// 0.6 · 3000 / 2π stars a steradian over the view's 2.09 sr, most of them 2-3 pixels, the
		// faintest under 3 of 255
		let shown = count(&night);
		assert!((600..2400).contains(&shown), "{shown} star pixels at midnight");
		assert_eq!(count(&lit(0.0, false, 3000)), 0, "no stars without the celestial bodies");
		assert_eq!(count(&lit(0.0, true, 0)), 0, "none at StarCount 0");
		assert_eq!(count(&lit(12.0, true, 3000)), 0, "none at noon");
		let most = lit(0.0, true, 5000);
		assert_eq!(lit(0.0, true, 10000), most, "StarCount past 5,000 draws 5,000's");
		assert!(count(&most) as f32 > 1.4 * shown as f32, "5,000 stars over 3,000: {} against {shown}", count(&most));
		// in the day's hours a quarter of midnight's light: fewer pixels over 2 of 255
		let dawn = count(&lit(5.5, true, 3000));
		assert!(dawn > 0 && dawn < shown, "{dawn} star pixels at 5.5 against {shown}");
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn star_light_follows_the_clock_and_the_diffuse_scale() {
		use environment::star_light;
		assert_eq!(star_light(0.0, 1.0), 1.0);
		assert_eq!(star_light(12.0, 1.0), 0.0);
		assert_eq!(star_light(7.3, 1.0), 0.0);
		assert_eq!(star_light(16.7, 1.0), 0.0);
		assert!((star_light(7.2, 1.0) - 0.453).abs() < 0.01 && (star_light(16.8, 1.0) - 0.456).abs() < 0.01);
		// the night's stars against the day's either side of 5.165 and 18.835
		assert!(star_light(5.16, 1.0) > 0.98 && (star_light(5.17, 1.0) - 0.235).abs() < 0.01);
		assert!((star_light(18.83, 1.0) - 0.218).abs() < 0.01 && star_light(18.84, 1.0) > 0.92);
		assert!((star_light(-0.5, 1.0) - star_light(23.5, 1.0)).abs() < 1e-6);
		assert!((star_light(0.0, 0.0) - 0.89).abs() < 1e-6 && (star_light(0.0, 0.5) - 0.945).abs() < 1e-6);
	}
}
