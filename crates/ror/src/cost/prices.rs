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
	/// Not calibrated for this device preset (reported as 0 ms; see `fidelity` and `unmodeled`).
	Uncalibrated,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
	pub key: &'static str,
	/// Numeric unit price, or `None` (`null` in JSON) when `basis == Basis::Uncalibrated`.
	pub value: Option<f64>,
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
/// Ratio of phone to desktop CPU frame cost per draw, part and triangle (phone / desktop),
/// scaled from paired main-thread render timings (2.78 x 0.80 = 2.224).
pub const DESKTOP_CPU_DIVISOR: f64 = 2.224;

/// Pricing profile for one device preset: CPU prices in frame ms and optional GPU ms conversion.
/// Only the named `desktop` preset uses desktop pricing; `phone` and no-`--preset` (`custom`) runs
/// keep the calibrated phone frame pricing unless the caller asks for `--preset desktop`.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
	pub desktop: bool,
	pub draw_ms: f64,
	pub part_ms: f64,
	pub kilo_triangle_ms: f64,
	pub gpu_ms_per_unit: Option<f64>,
	pub gpu_to_frame: Option<f64>,
}

impl Profile {
	pub fn for_preset(preset: &Preset) -> Self {
		if preset.uses_desktop_pricing() {
			Self {
				desktop: true,
				draw_ms: DRAW_MS / DESKTOP_CPU_DIVISOR,
				part_ms: PART_MS / DESKTOP_CPU_DIVISOR,
				kilo_triangle_ms: KILO_TRIANGLE_MS / DESKTOP_CPU_DIVISOR,
				gpu_ms_per_unit: None,
				gpu_to_frame: None,
			}
		} else {
			Self {
				desktop: false,
				draw_ms: DRAW_MS,
				part_ms: PART_MS,
				kilo_triangle_ms: KILO_TRIANGLE_MS,
				gpu_ms_per_unit: Some(GPU_MS_PER_UNIT),
				gpu_to_frame: Some(GPU_TO_FRAME),
			}
		}
	}
}

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
	/// Quality scale requested: `"edit"` (Studio 1–21) or `"client"` (player 1–10 slider).
	pub scale: &'static str,
	/// Player slider bar (1–10) when `scale == "client"`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub client_level: Option<u32>,
	/// Distance culling horizon in studs for `level` (`crate::lod::Lod::view_distance`).
	pub view_distance: f32,
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
		Self::of_scale(level, None, phone)
	}

	pub fn of_scale(level: u32, client_level: Option<u32>, phone: bool) -> Self {
		let level = level.clamp(1, 21);
		let client_level = client_level.map(|bar| bar.clamp(1, 10));
		let view_distance = crate::lod::Lod { quality: level, cull: true }.view_distance();
		Self {
			level,
			scale: if client_level.is_some() { "client" } else { "edit" },
			client_level,
			view_distance,
			sun_shadows: level >= 12,
			per_pixel_lights: level >= 16,
			bloom: level >= 6,
			max_rate: if phone { 100.0 } else { 400.0 },
		}
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

	/// No preset named: the run's own size and quality (400 particles/s rate cap, priced as phone
	/// frame ms unless `--preset desktop` is explicitly requested).
	pub fn custom(width: u32, height: u32, quality: u32) -> Self {
		let scale = if width * height > 1_500_000 { 2 } else { 1 };
		Self { name: "custom".into(), width, height, analysis_scale: scale, quality, phone: false }
	}

	/// Whether this preset explicitly requested desktop pricing (`--preset desktop`).
	pub fn uses_desktop_pricing(&self) -> bool {
		self.name == "desktop"
	}

	pub fn analysis_size(&self) -> (u32, u32) {
		((self.width / self.analysis_scale).max(16), (self.height / self.analysis_scale).max(16))
	}
}

/// The prices as the analysis reports them.
pub fn table() -> Vec<Price> {
	let (fit, waits, anchor) = ("12 phone measurements of a large place (frame ms)", "phone measurements: 0.874 x 5.65 ms of GPU waits / 25.3 ms of GPU", "a reference place's 16 views on the phone preset = 4.3 ms, its GPU time with and without its map");
	vec![
		Price { key: "draw", value: Some(DRAW_MS), unit: "phone frame ms per draw call", basis: Basis::Fitted, source: fit },
		Price { key: "part", value: Some(PART_MS), unit: "phone frame ms per part in the place", basis: Basis::Fitted, source: "the same fit" },
		Price { key: "kiloTriangle", value: Some(KILO_TRIANGLE_MS), unit: "phone frame ms per 1,000 triangles drawn", basis: Basis::Fitted, source: "the same fit" },
		Price { key: "gpuToFrame", value: Some(GPU_TO_FRAME), unit: "phone frame ms per phone GPU ms", basis: Basis::Measured, source: waits },
		Price { key: "gpuUnit", value: Some(GPU_MS_PER_UNIT), unit: "phone GPU ms per viewport of opaque Plastic", basis: Basis::Anchored, source: anchor },
		Price { key: "weights", value: Some(1.0), unit: "relative per-pixel weights (textured 1.4, neon 0.8, glass 1.2, sky 0.3, blended 1.6, alpha SurfaceAppearance 2.0, particle 1.3, light 0.25, sun shadow 0.2)", basis: Basis::Estimated, source: "relative shading work, not measured on a phone" },
		Price { key: "shadowRedraw", value: Some(super::attribute::SHADOW_REDRAW_SHARE), unit: "share of the casters' shadow triangles charged a frame", basis: Basis::Estimated, source: "Roblox caches shadow maps and redraws them as the camera moves" },
	]
}

/// The prices for a specific device preset. Phone and no-`--preset` (`custom`) runs use the
/// calibrated phone fit and GPU wait anchor; `--preset desktop` scales CPU prices by one reference-PC
/// main-thread ratio (`1 / DESKTOP_CPU_DIVISOR`, estimated rather than per-term fitted) and refuses
/// desktop GPU ms (`value: null`, `basis: "uncalibrated"`, work reported in `gpuUnits`).
pub fn table_for(preset: &Preset) -> Vec<Price> {
	if !preset.uses_desktop_pricing() {
		return table();
	}
	let profile = Profile::for_preset(preset);
	let cpu_source = "estimated: one whole-frame main-thread ratio (1/2.224, reference PC vs live phones) applied alike to draw, part and triangle prices; estimates that reference PC, not players' desktops";
	vec![
		Price { key: "draw", value: Some(profile.draw_ms), unit: "desktop CPU frame ms per draw call", basis: Basis::Estimated, source: cpu_source },
		Price { key: "part", value: Some(profile.part_ms), unit: "desktop CPU frame ms per part in the place", basis: Basis::Estimated, source: "the same single reference-PC CPU ratio (estimated)" },
		Price { key: "kiloTriangle", value: Some(profile.kilo_triangle_ms), unit: "desktop CPU frame ms per 1,000 triangles drawn", basis: Basis::Estimated, source: "the same single reference-PC CPU ratio (estimated)" },
		Price { key: "gpuToFrame", value: None, unit: "uncalibrated on desktop (GPU work reported in gpuUnits, not ms)", basis: Basis::Uncalibrated, source: "desktop GPU waits are not calibrated; compare gpuUnits directly" },
		Price { key: "gpuUnit", value: None, unit: "uncalibrated on desktop (1.0 gpuUnit = 1 viewport of opaque Plastic)", basis: Basis::Uncalibrated, source: "desktop GPU ms is not calibrated; compare gpuUnits directly" },
		Price { key: "weights", value: Some(1.0), unit: "relative per-pixel weights in gpuUnits (textured 1.4, neon 0.8, glass 1.2, sky 0.3, blended 1.6, alpha SurfaceAppearance 2.0, particle 1.3, light 0.25, sun shadow 0.2)", basis: Basis::Estimated, source: "relative shading work, not measured per shader" },
		Price { key: "shadowRedraw", value: Some(super::attribute::SHADOW_REDRAW_SHARE), unit: "share of the casters' shadow triangles charged a frame", basis: Basis::Estimated, source: "Roblox caches shadow maps and redraws them as the camera moves" },
	]
}

/// Calibration fidelity flags emitted in `analysis.json`.
pub fn fidelity(preset: &Preset, pixels: bool) -> serde_json::Value {
	let desktop = preset.uses_desktop_pricing();
	let mut map = serde_json::Map::new();
	map.insert("preset".into(), serde_json::json!(preset.name));
	map.insert("phone".into(), serde_json::json!(preset.phone));
	map.insert("pricing".into(), serde_json::json!(if desktop { "desktop" } else { "phone" }));
	map.insert("status".into(), serde_json::json!(if desktop { "partial" } else { "calibrated" }));
	map.insert("cpuCalibrated".into(), serde_json::json!(!desktop));
	map.insert("gpuCalibrated".into(), serde_json::json!(!desktop));
	map.insert("pixelsCounted".into(), serde_json::json!(pixels));
	map.insert("cpuBasis".into(), serde_json::json!(if desktop { "estimated" } else { "fitted" }));
	map.insert("gpuBasis".into(), serde_json::json!(if desktop { "uncalibrated" } else { "anchored" }));
	map.insert("msIncludesGpu".into(), serde_json::json!(!desktop));
	if desktop {
		map.insert(
			"cpuEstimate".into(),
			serde_json::json!({
				"reason": "Desktop CPU ms applies one whole-frame main-thread ratio (1/2.224) from one reference PC against live phones to draw, part and triangle terms alike; it is an estimate for that reference PC, not players' desktops.",
				"settleWith": "Per-term desktop knock-arm measurements (draws, parts, triangles) across target player desktop hardware.",
			}),
		);
		map.insert(
			"gpuRefusal".into(),
			serde_json::json!({
				"reason": "Desktop GPU milliseconds and GPU-to-frame wait conversion are uncalibrated; gpuMs is null and shading/overdraw work is reported in gpuUnits instead.",
				"settleWith": "Paired desktop GPU MicroProfiler or GPU-capture timings across representative maps to fit ms per gpuUnit and GPU wait share.",
			}),
		);
	}
	serde_json::Value::Object(map)
}

/// Rendering costs explicitly not modeled or uncalibrated for this preset/quality.
pub fn unmodeled(preset: &Preset, quality: &Quality) -> Vec<serde_json::Value> {
	let mut items = vec![
		serde_json::json!({
			"kind": "lightGridCpu",
			"scope": "cpu",
			"reason": "Voxel light-grid CPU updates for local lights are unmeasured; lights that reach no pixel are listed with 0 ms saving.",
			"settleWith": "Measured light-grid CPU scope deltas while toggling local lights in a reference place.",
		}),
		serde_json::json!({
			"kind": "terrainAndGrass",
			"scope": "draws,gpu",
			"reason": "Roblox Terrain voxels and Terrain Grass are not drawn or counted.",
			"settleWith": "Terrain voxel/grass draw and pixel support in the renderer.",
		}),
		serde_json::json!({
			"kind": "textureStreaming",
			"scope": "gpu,memory",
			"reason": "Runtime texture streaming bandwidth and VRAM pressure are not converted into frame ms.",
			"settleWith": "Device memory-pressure and streaming-budget benchmarks.",
		}),
	];
	if preset.uses_desktop_pricing() {
		items.push(serde_json::json!({
			"kind": "desktopGpuMs",
			"scope": "gpu",
			"reason": "Desktop GPU milliseconds and GPU-to-frame wait conversion are uncalibrated; gpuMs is null and shader/overdraw work is reported in gpuUnits instead.",
			"settleWith": "Paired desktop GPU MicroProfiler or GPU-capture timings across representative maps to fit ms per gpuUnit and GPU wait share.",
		}));
	}
	if !quality.per_pixel_lights {
		items.push(serde_json::json!({
			"kind": "perPixelLightsDisabled",
			"scope": "gpu",
			"reason": "At graphics quality below 16, local lights use the voxel grid rather than per-pixel shading.",
			"settleWith": "Measure at quality 16+ for per-pixel local light shading cost.",
		}));
	}
	items
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn desktop_preset_uses_scaled_cpu_prices_and_uncalibrated_gpu_while_custom_keeps_phone() {
		let phone = Preset::named("phone").unwrap();
		let custom = Preset::custom(1920, 1080, 21);
		let desktop = Preset::named("desktop").unwrap();
		let p_prof = Profile::for_preset(&phone);
		let c_prof = Profile::for_preset(&custom);
		let d_prof = Profile::for_preset(&desktop);
		assert!(!p_prof.desktop && p_prof.gpu_ms_per_unit.is_some() && p_prof.gpu_to_frame.is_some());
		assert!(!c_prof.desktop && c_prof.draw_ms == p_prof.draw_ms && c_prof.gpu_ms_per_unit == p_prof.gpu_ms_per_unit);
		assert!(d_prof.desktop && d_prof.gpu_ms_per_unit.is_none() && d_prof.gpu_to_frame.is_none());
		assert!((d_prof.draw_ms - DRAW_MS / DESKTOP_CPU_DIVISOR).abs() < 1e-12);
		let d_table = table_for(&desktop);
		assert!(d_table.iter().all(|p| !p.unit.to_ascii_lowercase().contains("phone")));
		let gpu_unit = d_table.iter().find(|p| p.key == "gpuUnit").unwrap();
		assert_eq!(gpu_unit.basis, Basis::Uncalibrated);
		assert_eq!(gpu_unit.value, None);
		assert_eq!(d_table.iter().find(|p| p.key == "gpuToFrame").unwrap().value, None);
		let p_fid = fidelity(&phone, true);
		let c_fid = fidelity(&custom, true);
		let d_fid = fidelity(&desktop, true);
		assert_eq!((p_fid["cpuCalibrated"].as_bool(), p_fid["gpuCalibrated"].as_bool()), (Some(true), Some(true)));
		assert_eq!((c_fid["pricing"].as_str(), c_fid["gpuCalibrated"].as_bool()), (Some("phone"), Some(true)));
		assert_eq!((d_fid["cpuCalibrated"].as_bool(), d_fid["cpuBasis"].as_str(), d_fid["gpuCalibrated"].as_bool()), (Some(false), Some("estimated"), Some(false)));
		assert!(d_fid["gpuRefusal"]["settleWith"].is_string());
		let d_unm = unmodeled(&desktop, &Quality::of(21, false));
		assert!(d_unm.iter().any(|item| item["kind"] == "desktopGpuMs"));
		assert!(!unmodeled(&custom, &Quality::of(21, false)).iter().any(|item| item["kind"] == "desktopGpuMs"));
	}
}
