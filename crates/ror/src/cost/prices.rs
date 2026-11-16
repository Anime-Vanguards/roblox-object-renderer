//! What the cost loop charges for each kind of work, in milliseconds of an average phone's frame,
//! and where each price comes from. Every price says whether it was measured, fitted to measurements,
//! anchored to one, or estimated; the analysis prints them with its numbers.
//!
//! The frame model is the one the maintainers' phone measurements use: frame = CPU work + 0.874 x the
//! render thread's waits for the GPU, the waits taken as the measured 5.65 ms times a piece's share of
//! the GPU's time (25.3 ms in the reference place). CPU-side prices (draw calls, parts, triangles) are
//! fitted to phone measurements that added and removed those in a large place and timed the frame;
//! GPU-side work (pixels shaded, blended layers, lights, post effects) is weighed in viewports of work
//! and charged through the waits.

use serde::Serialize;

/// How a price was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Basis {
	/// Read off a measurement directly.
	Measured,
	/// A least-squares fit to several measurements.
	Fitted,
	/// A relative weight scaled so that a measured total comes out.
	Anchored,
	/// Not measured: a stated assumption.
	Estimated,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
	pub key: &'static str,
	pub value: f64,
	pub unit: &'static str,
	pub basis: Basis,
	pub source: &'static str,
}

/// Phone frame ms per draw call. Fitted with [`PART_MS`] and [`KILO_TRIANGLE_MS`] to 12 phone
/// measurements of a large place (parts, Roblox's draw and triangle counters, the phone's frame ms);
/// residuals up to 1.6 ms, so read it as about 10-20 us.
pub const DRAW_MS: f64 = 0.0158;
/// Phone frame ms per part in the place, drawn or not (culling and the render queue walk every one:
/// 1,937 parts no view drew, taken out, read -1.95 ms on their own). Fitted with [`DRAW_MS`].
pub const PART_MS: f64 = 0.000264;
/// Phone frame ms per 1,000 triangles drawn. Fitted with [`DRAW_MS`]; triangles are mostly GPU work that
/// phones rarely wait for (quartering a place's meshes moved no CPU scope).
pub const KILO_TRIANGLE_MS: f64 = 0.00217;
/// Frame ms per phone GPU ms: 0.874 x 5.65 ms of waits / 25.3 ms of GPU ("GPU waits by share").
pub const GPU_TO_FRAME: f64 = 0.874 * 5.65 / 25.3;
/// Phone GPU ms per viewport-unit of work (one viewport of opaque Plastic shaded once). Anchored: the
/// reference place's 16 player views on the phone preset average 10.14 units a view, set to the 4.3 ms
/// of phone GPU the live game loses when a full-screen window hides that place's map (25.3 -> 21.0 ms).
pub const GPU_MS_PER_UNIT: f64 = 4.3 / 10.14;

/// Relative GPU weights per viewport of pixels (estimated: the shading each does per pixel, no phone
/// measurement yet). An opaque Plastic pixel is 1.
pub mod weight {
	/// Opaque surfaces by how they shade: plain materials, textured (2022 materials, a TextureID, a
	/// SurfaceAppearance's maps), Neon (no lighting, glows), Glass.
	pub const PLAIN: f64 = 1.0;
	pub const TEXTURED: f64 = 1.4;
	pub const NEON: f64 = 0.8;
	pub const GLASS: f64 = 1.2;
	/// The sky where no surface covers it.
	pub const SKY: f64 = 0.3;
	/// A blended layer (see-through part, decal, particle, beam): shaded and blended every time, no
	/// early depth rejection, read-modify-write of the target.
	pub const BLENDED: f64 = 1.6;
	/// A blended layer of a SurfaceAppearance with alpha (its maps sampled where it is clear too).
	pub const BLENDED_TEXTURED: f64 = 2.0;
	pub const PARTICLE: f64 = 1.3;
	/// Each local light whose range holds the pixel, where the quality lights per pixel
	/// ([`super::Quality::per_pixel_lights`]: 16 and up).
	pub const LIGHT: f64 = 0.25;
	/// Sampling the sun's shadow maps where shadows draw ([`super::Quality::sun_shadows`]: 12 and up).
	pub const SUN_SHADOW: f64 = 0.2;
}

/// What a graphics quality level draws, as far as the cost loop models it. Measured in the capture
/// Studio under Future at edit quality 1-21 (2026-11-16, Studio 0.742; notes/cost.md "Quality"): a
/// column casting a sun shadow, a PointLight with Shadows behind a blocker, a Neon bar, captured at each
/// level.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
	pub level: u32,
	/// Sun shadows: none at 1-8, a short reach at 10 (not to a caster 59 studs from the camera), soft
	/// from 12, full from 16. Counted from 12.
	pub sun_shadows: bool,
	/// Local lights shaded per pixel, with their own shadows: from 16; below, the voxel light grid
	/// (soft, no shadow behind a blocker), which costs the GPU nothing per pixel.
	pub per_pixel_lights: bool,
	/// The built-in bloom round Neon and bright pixels: faint from 6, full from 12; none below 6.
	pub bloom: bool,
	/// Particles an emitter makes a second at most (estimated: Roblox's 400, 100 on phones).
	pub max_rate: f32,
}

impl Quality {
	pub fn of(level: u32, phone: bool) -> Self {
		let level = level.clamp(1, 21);
		Self { level, sun_shadows: level >= 12, per_pixel_lights: level >= 16, bloom: level >= 6, max_rate: if phone { 100.0 } else { 400.0 } }
	}
}

/// A device and graphics quality to cost for.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
	pub name: String,
	/// The view's size in device pixels.
	pub width: u32,
	pub height: u32,
	/// Pixels are counted on a raster this many times smaller each way, then scaled up.
	pub analysis_scale: u32,
	pub quality: u32,
	pub phone: bool,
}

impl Preset {
	/// `phone`: 844 x 390 points at 3x (2532 x 1170, an iPhone of the 13-15 class), graphics quality 2
	/// (most phones run below 4: no sun shadows, voxel lights, no bloom). `desktop`: 1920 x 1080 at
	/// quality 21 (Studio's highest, where ror's rules were measured).
	pub fn named(name: &str) -> Option<Self> {
		match name {
			"phone" => Some(Self { name: name.into(), width: 2532, height: 1170, analysis_scale: 3, quality: 2, phone: true }),
			"desktop" => Some(Self { name: name.into(), width: 1920, height: 1080, analysis_scale: 2, quality: 21, phone: false }),
			_ => None,
		}
	}

	/// No preset named: the run's own size and quality, counted as a desktop.
	pub fn custom(width: u32, height: u32, quality: u32) -> Self {
		let scale = if width * height > 1_500_000 { 2 } else { 1 };
		Self { name: "custom".into(), width, height, analysis_scale: scale, quality, phone: false }
	}

	pub fn analysis_size(&self) -> (u32, u32) {
		((self.width / self.analysis_scale).max(16), (self.height / self.analysis_scale).max(16))
	}
}

/// The prices as the analysis reports them.
pub fn table() -> Vec<Price> {
	let (fit, waits, anchor) = ("12 phone measurements of a large place (frame ms)", "phone measurements: 0.874 x 5.65 ms of GPU waits / 25.3 ms of GPU", "a reference place's 16 views on the phone preset = 4.3 ms, its GPU time with and without its map");
	vec![
		Price { key: "draw", value: DRAW_MS, unit: "phone frame ms per draw call", basis: Basis::Fitted, source: fit },
		Price { key: "part", value: PART_MS, unit: "phone frame ms per part in the place", basis: Basis::Fitted, source: "the same fit" },
		Price { key: "kiloTriangle", value: KILO_TRIANGLE_MS, unit: "phone frame ms per 1,000 triangles drawn", basis: Basis::Fitted, source: "the same fit" },
		Price { key: "gpuToFrame", value: GPU_TO_FRAME, unit: "phone frame ms per phone GPU ms", basis: Basis::Measured, source: waits },
		Price { key: "gpuUnit", value: GPU_MS_PER_UNIT, unit: "phone GPU ms per viewport of opaque Plastic", basis: Basis::Anchored, source: anchor },
		Price { key: "weights", value: 1.0, unit: "relative per-pixel weights (textured 1.4, neon 0.8, glass 1.2, sky 0.3, blended 1.6, alpha SurfaceAppearance 2.0, particle 1.3, light 0.25, sun shadow 0.2)", basis: Basis::Estimated, source: "relative shading work, not measured on a phone" },
		Price { key: "shadowRedraw", value: super::attribute::SHADOW_REDRAW_SHARE, unit: "share of the casters' shadow triangles charged a frame", basis: Basis::Estimated, source: "Roblox caches shadow maps and redraws them as the camera moves" },
	]
}
