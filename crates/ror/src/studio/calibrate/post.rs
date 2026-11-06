//! Post effect experiments: ColorCorrection, BlurEffect, DepthOfField and SunRays over fixtures of
//! known radiance (notes/render.md "Post effects"; tools/analysis/post_fit.py).

// fixture helpers take every property of what they place
#![allow(clippy::too_many_arguments)]

use glam::{Affine3A, Vec3};

use super::{Experiment, Probe, Run, dark};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::environment::{PostEffect, Sky};

/// A flat tile facing the camera (-Z), its front face at z = -0.1 · thickness.
fn tile(model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, center: Vec3, size: Vec3, color: [u8; 3], material: u32, inputs: serde_json::Value) {
	model.part(&PartSpec { name: label.clone(), cframe: Affine3A::from_translation(center), size, color, material, cast_shadow: false, ..Default::default() });
	probes.push(Probe { label, position: (center - Vec3::Z * size.z * 0.5).into(), inputs });
}

/// A black, unlit wall behind the fixtures.
fn backdrop(model: &mut ModelBuilder, center: Vec3, size: Vec3) {
	model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(center), size, color: [0, 0, 0], material: 288, cast_shadow: false, ..Default::default() });
}

/// Flat light alone (Ambient 1, no sun, no sky light): SmoothPlastic of colour c shows x = 1.98 c².
fn flat() -> Environment {
	Environment { ambient: Vec3::ONE, ..dark() }
}

fn correction(brightness: f32, contrast: f32, saturation: f32, tint: [f32; 3]) -> PostEffect {
	PostEffect::ColorCorrection { brightness, contrast, saturation, tint: Vec3::from(tint) }
}

/// ColorCorrectionEffect: a gray ramp (16 SmoothPlastic tiles, x 0 to 1.98), a row of colours and
/// a row of Neon (x 2.6 to 26) under flat light, head-on, with Brightness, Contrast, Saturation and
/// TintColor swept one at a time, the lobby's, two effects at once, and at +1 and -1 EV.
pub fn post_color() -> Experiment {
	let mut model = ModelBuilder::new("PostColor");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::new(0.0, 0.0, 2.0), Vec3::new(120.0, 80.0, 1.0));
	let size = Vec3::new(2.4, 2.4, 0.2);
	for index in 0..16 {
		let gray = (index * 17) as u8;
		let x = 1.98 * (gray as f32 / 255.0).powi(2);
		tile(&mut model, &mut probes, format!("gray{gray}"), Vec3::new(22.5 - index as f32 * 3.0, 6.0, 0.0), size, [gray; 3], 272, serde_json::json!({"kind": "gray", "x": [x, x, x]}));
	}
	let colors: [[u8; 3]; 12] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255], [255, 0, 255], [196, 40, 28], [13, 105, 172], [75, 151, 75], [245, 205, 48], [128, 64, 32], [100, 150, 200]];
	for (index, color) in colors.iter().enumerate() {
		let x: Vec<f32> = color.iter().map(|c| 1.98 * (*c as f32 / 255.0).powi(2)).collect();
		tile(&mut model, &mut probes, format!("color{index}"), Vec3::new(16.5 - index as f32 * 3.0, 1.0, 0.0), size, *color, 272, serde_json::json!({"kind": "color", "color": color, "x": x}));
	}
	for index in 0..8 {
		let gray = 160 + index as u8 * 13;
		let x = 25.75 * (gray as f32 / 255.0).powi(4);
		tile(&mut model, &mut probes, format!("neon{gray}"), Vec3::new(10.5 - index as f32 * 3.0, -4.0, 0.0), size, [gray; 3], 288, serde_json::json!({"kind": "neon", "x": [x, x, x]}));
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 1.0, -55.0], target: [0.0, 1.0, 0.0], fov: 40.0 };
	let white = [1.0f32; 3];
	let mut cases: Vec<(String, Vec<PostEffect>, f32)> = vec![("none".into(), vec![], 0.0)];
	for brightness in [-0.5f32, -0.2, 0.2, 0.5] {
		cases.push((format!("brightness{brightness}"), vec![correction(brightness, 0.0, 0.0, white)], 0.0));
	}
	for contrast in [-0.5f32, 0.3, 1.0] {
		cases.push((format!("contrast{contrast}"), vec![correction(0.0, contrast, 0.0, white)], 0.0));
	}
	for saturation in [-1.0f32, -0.5, 0.5, 1.0] {
		cases.push((format!("saturation{saturation}"), vec![correction(0.0, 0.0, saturation, white)], 0.0));
	}
	cases.push(("tint-orange".into(), vec![correction(0.0, 0.0, 0.0, [1.0, 0.5, 0.25])], 0.0));
	cases.push(("tint-lobby".into(), vec![correction(0.0, 0.0, 0.0, [233.0 / 255.0, 247.0 / 255.0, 1.0])], 0.0));
	cases.push(("lobby".into(), vec![correction(0.0, 0.3, -0.15, [233.0 / 255.0, 247.0 / 255.0, 1.0])], 0.0));
	cases.push(("two".into(), vec![correction(0.2, 0.0, 0.0, white), correction(0.0, 0.3, 0.0, white)], 0.0));
	cases.push(("two-tints".into(), vec![correction(0.0, 0.0, 0.0, [1.0, 0.5, 0.25]), correction(0.0, 0.0, 0.0, [0.5, 1.0, 1.0])], 0.0));
	cases.push(("contrast0.3-ev1".into(), vec![correction(0.0, 0.3, 0.0, white)], 1.0));
	cases.push(("brightness0.2-ev-1".into(), vec![correction(0.2, 0.0, 0.0, white)], -1.0));
	cases.push(("ev1".into(), vec![], 1.0));
	cases.push(("ev-1".into(), vec![], -1.0));
	let runs = cases
		.into_iter()
		.map(|(label, effects, exposure)| Run { label, environment: Environment { effects, exposure_compensation: exposure, ..flat() }, camera: camera.clone() })
		.collect();
	Experiment { name: "post-color".into(), model, probes, runs }
}

/// ColorCorrectionEffect past post-color's range (a cutscene strobes an impact correction of
/// Contrast 15 / -15 and Saturation -0.9): post-color's fixtures under Contrast -15 to 15.
pub fn post_color_extreme() -> Experiment {
	let base = post_color();
	let camera = base.runs[0].camera.clone();
	let white = [1.0f32; 3];
	let mut cases: Vec<(String, Vec<PostEffect>)> = vec![("none".into(), vec![])];
	for contrast in [-15.0f32, -4.0, -2.0, -1.5, -1.0, -0.75, 1.5, 3.0, 15.0] {
		cases.push((format!("contrast{contrast}"), vec![correction(0.0, contrast, 0.0, white)]));
	}
	cases.push(("impact".into(), vec![correction(0.0, 15.0, -0.9, white)]));
	cases.push(("impact-inverted".into(), vec![correction(0.0, -15.0, -0.9, white)]));
	cases.push(("contrast-1-brightness0.3".into(), vec![correction(0.3, -1.0, 0.0, white)]));
	cases.push(("contrast-2-saturation-1".into(), vec![correction(0.0, -2.0, -1.0, white)]));
	let runs = cases
		.into_iter()
		.map(|(label, effects)| Run { label, environment: Environment { effects, ..flat() }, camera: camera.clone() })
		.collect();
	Experiment { name: "post-color-extreme".into(), model: base.model, probes: base.probes, runs }
}

/// The order of one ColorCorrection's terms (a cutscene's: Brightness 0.15, Contrast 0.4,
/// Saturation -0.5, TintColor 0.851 grey): post-color's fixtures under pairs of terms in one effect
/// (Tint against Brightness and Contrast, Brightness against Contrast, Saturation against Tint and
/// Contrast) and that whole correction.
pub fn post_color_order() -> Experiment {
	let base = post_color();
	let camera = base.runs[0].camera.clone();
	let white = [1.0f32; 3];
	let orange = [1.0f32, 0.5, 0.25];
	let grey = [0.851f32; 3];
	let cases: Vec<(&str, PostEffect)> = vec![
		("tint-brightness", correction(0.3, 0.0, 0.0, orange)),
		("tint-contrast", correction(0.0, 1.0, 0.0, orange)),
		("grey-contrast", correction(0.0, 1.0, 0.0, grey)),
		("brightness-contrast", correction(0.3, 1.0, 0.0, white)),
		("saturation-tint", correction(0.0, 0.0, -1.0, orange)),
		("saturation-contrast", correction(0.0, 1.0, -0.5, white)),
		("graded", correction(0.15, 0.4, -0.5, grey)),
	];
	let mut runs = vec![Run { label: "none".into(), environment: flat(), camera: camera.clone() }];
	runs.extend(cases.into_iter().map(|(label, effect)| Run { label: label.into(), environment: Environment { effects: vec![effect], ..flat() }, camera: camera.clone() }));
	Experiment { name: "post-color-order".into(), model: base.model, probes: base.probes, runs }
}

/// ColorGradingEffect's TonemapperPreset (some games' scenery uses Retro):
/// 48 gray Neon tiles whose emission x = 25.75 c⁴ falls by 0.767x a tile from 25.75 to 1.0e-4 and a
/// row of coloured Neon, over the black backdrop, the bloom off (BloomEffect Intensity 0); Default
/// against Retro at -3, 0 and +2 EV, Retro with the built-in bloom and with the lobby's
/// ColorCorrection. ColorCorrection's Brightness acts on the shown values before they clip, so
/// darkened runs (-0.4, -0.8) show both curves above 255, the built-in bloom lifting the brightest
/// tiles' x past the 7.84 cap.
pub fn tone_retro() -> Experiment {
	let mut model = ModelBuilder::new("ToneRetro");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::new(0.0, 0.0, 2.0), Vec3::new(120.0, 80.0, 1.0));
	let size = Vec3::new(2.4, 2.4, 0.2);
	// screen x grows towards world -X (the camera looks along +Z)
	let position = |row: usize, column: usize| Vec3::new(11.2 - column as f32 * 3.2, 9.6 - row as f32 * 3.2, 0.0);
	for index in 0..48 {
		let level = (255.0 * 0.767f32.powf(index as f32 / 4.0)).round() as u8;
		let x = 25.75 * (level as f32 / 255.0).powi(4);
		tile(&mut model, &mut probes, format!("gray{index}"), position(index / 8, index % 8), size, [level; 3], 288, serde_json::json!({"kind": "gray", "level": level, "x": [x, x, x]}));
	}
	let colors: [[u8; 3]; 8] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 170, 0], [255, 0, 255], [0, 255, 255], [200, 120, 60], [120, 160, 220]];
	for (index, color) in colors.iter().enumerate() {
		let x: Vec<f32> = color.iter().map(|c| 25.75 * (*c as f32 / 255.0).powi(4)).collect();
		tile(&mut model, &mut probes, format!("color{index}"), position(6, index), size, *color, 288, serde_json::json!({"kind": "color", "color": color, "x": x}));
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -60.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let no_bloom = PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 };
	let retro = PostEffect::ColorGrading { tonemapper: 1 };
	let lobby = correction(0.1, 0.3, -0.15, [233.0 / 255.0, 247.0 / 255.0, 1.0]);
	let cases: Vec<(&str, Vec<PostEffect>, f32)> = vec![
		("default-ev0", vec![no_bloom.clone()], 0.0),
		("retro-ev0", vec![no_bloom.clone(), retro.clone()], 0.0),
		("default-ev-3", vec![no_bloom.clone()], -3.0),
		("retro-ev-3", vec![no_bloom.clone(), retro.clone()], -3.0),
		("default-ev2", vec![no_bloom.clone()], 2.0),
		("retro-ev2", vec![no_bloom.clone(), retro.clone()], 2.0),
		("retro-bloom", vec![retro.clone()], 0.0),
		("retro-cc", vec![no_bloom.clone(), retro.clone(), lobby], 0.0),
		("retro-dim0.4", vec![no_bloom.clone(), retro.clone(), correction(-0.4, 0.0, 0.0, [1.0; 3])], 0.0),
		("retro-dim0.8", vec![no_bloom.clone(), retro.clone(), correction(-0.8, 0.0, 0.0, [1.0; 3])], 0.0),
		("default-dim0.4", vec![no_bloom.clone(), correction(-0.4, 0.0, 0.0, [1.0; 3])], 0.0),
		("default-dim0.8", vec![no_bloom, correction(-0.8, 0.0, 0.0, [1.0; 3])], 0.0),
		("retro-bloom-dim0.8", vec![retro, correction(-0.8, 0.0, 0.0, [1.0; 3])], 0.0),
		("default-bloom-dim0.4", vec![correction(-0.4, 0.0, 0.0, [1.0; 3])], 0.0),
		("default-bloom-dim0.8", vec![correction(-0.8, 0.0, 0.0, [1.0; 3])], 0.0),
	];
	let runs = cases
		.into_iter()
		.map(|(label, effects, exposure)| Run { label: label.into(), environment: Environment { effects, exposure_compensation: exposure, ..dark() }, camera: camera.clone() })
		.collect();
	Experiment { name: "tone-retro".into(), model, probes, runs }
}

/// BlurEffect: a lit white square, a thin white line and a checker of white squares over the black
/// backdrop under flat light (x 1.98, under the bloom threshold), head-on; Size 0 to 56, at two
/// distances (screen-space or world-space blur?).
pub fn post_blur() -> Experiment {
	let mut model = ModelBuilder::new("PostBlur");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::new(0.0, 0.0, 2.0), Vec3::new(200.0, 120.0, 1.0));
	tile(&mut model, &mut probes, "square".into(), Vec3::new(10.0, 4.0, 0.0), Vec3::new(4.0, 4.0, 0.2), [255; 3], 272, serde_json::json!({"kind": "square"}));
	tile(&mut model, &mut probes, "dot".into(), Vec3::new(-2.0, 4.0, 0.0), Vec3::new(0.3, 0.3, 0.2), [255; 3], 272, serde_json::json!({"kind": "dot"}));
	tile(&mut model, &mut probes, "line".into(), Vec3::new(-12.0, 0.0, 0.0), Vec3::new(0.15, 14.0, 0.2), [255; 3], 272, serde_json::json!({"kind": "line"}));
	for row in 0..4 {
		for column in 0..4 {
			if (row + column) % 2 == 0 {
				let center = Vec3::new(4.0 - column as f32 * 2.0, -4.0 - row as f32 * 2.0, 0.0);
				tile(&mut model, &mut probes, format!("checker{row}{column}"), center, Vec3::new(2.0, 2.0, 0.2), [255; 3], 272, serde_json::json!({"kind": "checker"}));
			}
		}
	}
	let near = CameraSpec::LookAt { eye: [0.0, 0.0, -45.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let far = CameraSpec::LookAt { eye: [0.0, 0.0, -90.0], target: [0.0, 0.0, 0.0], fov: 20.0 };
	let mut runs = Vec::new();
	for size in [0.0f32, 2.0, 4.0, 8.0, 16.0, 24.0, 40.0, 56.0] {
		let environment = Environment { effects: vec![PostEffect::Blur { size }], ..flat() };
		runs.push(Run { label: format!("size{size}"), environment: environment.clone(), camera: near.clone() });
		if [8.0, 24.0].contains(&size) {
			runs.push(Run { label: format!("size{size}-far"), environment, camera: far.clone() });
		}
	}
	runs.push(Run { label: "none".into(), environment: flat(), camera: near.clone() });
	runs.push(Run { label: "two".into(), environment: Environment { effects: vec![PostEffect::Blur { size: 8.0 }, PostEffect::Blur { size: 8.0 }], ..flat() }, camera: near.clone() });
	runs.push(Run { label: "size8-bloom".into(), environment: Environment { effects: vec![PostEffect::Blur { size: 8.0 }], ambient: Vec3::splat(2.0), ..flat() }, camera: near });
	Experiment { name: "post-blur".into(), model, probes, runs }
}

/// DepthOfFieldEffect: rows of checkered tiles (white and black SmoothPlastic under flat light) at
/// depths 5 to 160 studs, each 0.08 x its depth wide so they look alike, seen down +Z through a
/// 50 degree field of view; FocusDistance, InFocusRadius, NearIntensity and FarIntensity swept.
pub fn post_depth() -> Experiment {
	let mut model = ModelBuilder::new("PostDepth");
	let mut probes = Vec::new();
	let depths = [5.0f32, 10.0, 20.0, 40.0, 80.0, 160.0];
	for (index, depth) in depths.iter().enumerate() {
		let side = 0.08 * depth;
		let x = (index as f32 - 2.5) * 0.17 * depth;
		for cell in 0..4 {
			let (cx, cy) = ((cell % 2) as f32, (cell / 2) as f32);
			let gray = if (cell % 2) == (cell / 2) { 255 } else { 0 };
			let center = Vec3::new(x + (cx - 0.5) * side * 0.5, (cy - 0.5) * side * 0.5, *depth);
			let label = format!("d{depth}-{cell}");
			model.part(&PartSpec { name: label.clone(), cframe: Affine3A::from_translation(center), size: Vec3::new(side * 0.5, side * 0.5, side * 0.02), color: [gray; 3], material: 272, cast_shadow: false, ..Default::default() });
			probes.push(Probe { label, position: (center - Vec3::Z * side * 0.01).into(), inputs: serde_json::json!({"depth": depth, "gray": gray}) });
		}
	}
	backdrop(&mut model, Vec3::new(0.0, 0.0, 400.0), Vec3::new(1000.0, 600.0, 1.0));
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 50.0 };
	let dof = |far: f32, focus: f32, radius: f32, near: f32| PostEffect::DepthOfField { far_intensity: far, focus_distance: focus, in_focus_radius: radius, near_intensity: near };
	let cases: Vec<(&str, Vec<PostEffect>)> = vec![
		("none", vec![]),
		("default", vec![dof(0.75, 0.05, 10.0, 0.75)]),
		("focus20", vec![dof(0.75, 20.0, 10.0, 0.75)]),
		("focus20-r0", vec![dof(0.75, 20.0, 0.0, 0.75)]),
		("focus20-r40", vec![dof(0.75, 20.0, 40.0, 0.75)]),
		("focus20-far1-near0", vec![dof(1.0, 20.0, 10.0, 0.0)]),
		("focus20-far0.25-near0", vec![dof(0.25, 20.0, 10.0, 0.0)]),
		("focus20-far0-near1", vec![dof(0.0, 20.0, 5.0, 1.0)]),
		("focus80-far1-near1", vec![dof(1.0, 80.0, 10.0, 1.0)]),
		("lobby", vec![dof(0.75, 80.42, 90.0, 0.0)]),
	];
	let runs = cases.into_iter().map(|(label, effects)| Run { label: label.into(), environment: Environment { effects, ..flat() }, camera: camera.clone() }).collect();
	Experiment { name: "post-depth".into(), model, probes, runs }
}

/// Where an in-focus outline meets the depth of field's blurred background: 60 px square tiles 20 studs
/// out, white on a black backdrop and black on a light one 400 studs out, each tile's left and top edge
/// a pixel further along than the last's (phases 0-7 of a 4 px grid), under three far fields and none.
pub fn post_depth_grid() -> Experiment {
	let mut model = ModelBuilder::new("PostDepthGrid");
	let mut probes = Vec::new();
	// the camera's pixels per stud at depth d: 558 / tan(25°) / d, x and y from the view's centre (822, 558)
	let depth = 20.0f32;
	let per_stud = 558.0 / 25f32.to_radians().tan() / depth;
	let world = |px: f32, py: f32| Vec3::new((822.0 - px) / per_stud, (558.0 - py) / per_stud, depth);
	for (row, top, color) in [("black", 460.0f32, [0u8; 3]), ("white", 620.0, [255; 3])] {
		for k in 0..8 {
			let (left, top) = (300.0 + 105.0 * k as f32, top + k as f32);
			let center = world(left + 30.0, top + 30.0);
			tile(&mut model, &mut probes, format!("{row}-{k}"), center, Vec3::new(60.0 / per_stud, 60.0 / per_stud, 0.02), color, 272, serde_json::json!({"phase": k, "row": row}));
		}
	}
	model.part(&PartSpec { name: "Light".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 150.0, 400.0)), size: Vec3::new(1000.0, 300.0, 1.0), color: [180; 3], material: 272, cast_shadow: false, ..Default::default() });
	backdrop(&mut model, Vec3::new(0.0, -150.0, 400.0), Vec3::new(1000.0, 300.0, 1.0));
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 50.0 };
	let dof = |far: f32, focus: f32, radius: f32| PostEffect::DepthOfField { far_intensity: far, focus_distance: focus, in_focus_radius: radius, near_intensity: 0.0 };
	let cases: Vec<(&str, Vec<PostEffect>)> = vec![("none", vec![]), ("far1", vec![dof(1.0, 20.0, 10.0)]), ("far0.25", vec![dof(0.25, 20.0, 10.0)]), ("lobby", vec![dof(0.75, 80.42, 90.0)])];
	let runs = cases.into_iter().map(|(label, effects)| Run { label: label.into(), environment: Environment { effects, ..flat() }, camera: camera.clone() }).collect();
	Experiment { name: "post-depth-grid".into(), model, probes, runs }
}

/// How far an in-focus outline spreads into the blurred background by the background's depth: a white
/// 60 px tile 20 studs out (in focus) before its own black slab at 30, 40, 60, 100, 200 and 400 studs,
/// under FarIntensity 1, 0.5 and 0.25 (FocusDistance 20, InFocusRadius 5) and none.
pub fn post_depth_halo() -> Experiment {
	let mut model = ModelBuilder::new("PostDepthHalo");
	let mut probes = Vec::new();
	let depth = 20.0f32;
	let focal = 558.0 / 25f32.to_radians().tan();
	for (k, slab) in [30.0f32, 40.0, 60.0, 100.0, 200.0, 400.0].into_iter().enumerate() {
		let px = 162.0 + 264.0 * k as f32;
		let center = Vec3::new((822.0 - px) * depth / focal, 0.0, depth);
		tile(&mut model, &mut probes, format!("tile-{slab}"), center, Vec3::new(60.0 * depth / focal, 60.0 * depth / focal, 0.02), [255; 3], 272, serde_json::json!({"slab": slab}));
		// the slab spans 264 px across and the whole height at its own depth
		let size = Vec3::new(264.0 * slab / focal, 1200.0 * slab / focal, 1.0);
		model.part(&PartSpec { name: format!("slab-{slab}"), cframe: Affine3A::from_translation(Vec3::new((822.0 - px) * slab / focal, 0.0, slab)), size, color: [0, 0, 0], material: 288, cast_shadow: false, ..Default::default() });
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 50.0 };
	let dof = |far: f32| PostEffect::DepthOfField { far_intensity: far, focus_distance: 20.0, in_focus_radius: 5.0, near_intensity: 0.0 };
	let cases: Vec<(&str, Vec<PostEffect>)> = vec![("none", vec![]), ("far1", vec![dof(1.0)]), ("far0.5", vec![dof(0.5)]), ("far0.25", vec![dof(0.25)])];
	let runs = cases.into_iter().map(|(label, effects)| Run { label: label.into(), environment: Environment { effects, ..flat() }, camera: camera.clone() }).collect();
	Experiment { name: "post-depth-halo".into(), model, probes, runs }
}

/// What DepthOfField does to what draws without depth: a particle, a half-transparent part and a Beam
/// beside an opaque tile, each white, far (160 studs) and near (5), before the black backdrop 400 studs
/// out and under the open sky above it, with the field off, far-only (a cutscene's:
/// FocusDistance 85, InFocusRadius 30, FarIntensity 0.79, whose particle moon Studio leaves sharp) and
/// near-only. Studio freezes the particles at age 0.
pub fn post_depth_effects() -> Experiment {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::Variant;
	let name = "post-depth-effects";
	let mut model = ModelBuilder::new("PostDepthEffects");
	let mut probes = Vec::new();
	for (depth, height) in [(160.0f32, 0.0f32), (5.0, -0.6)] {
		let side = 0.06 * depth;
		let row = |column: f32| Vec3::new((column - 1.5) * side * 1.6, height * depth * 0.2 + side * 0.2, depth);
		tile(&mut model, &mut probes, format!("opaque-d{depth}"), row(0.0), Vec3::new(side, side, side * 0.02), [255; 3], 272, serde_json::json!({"depth": depth, "kind": "opaque"}));
		model.part(&PartSpec { name: format!("glass-d{depth}"), cframe: Affine3A::from_translation(row(1.0)), size: Vec3::new(side, side, side * 0.02), color: [255; 3], material: 272, transparency: 0.5, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label: format!("glass-d{depth}"), position: row(1.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "transparent"}) });
		super::particles::add_emitter(&mut model, &format!("particle-d{depth}"), Affine3A::from_translation(row(2.0)), &[("Size", super::particles::constant(side * 0.5))]);
		probes.push(Probe { label: format!("particle-d{depth}"), position: row(2.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "particle"}) });
		let anchor = model.part(&PartSpec { name: format!("beam-d{depth}"), cframe: Affine3A::from_translation(row(3.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let half = Vec3::new(0.0, side * 0.5, 0.0);
		let a0 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(-half))));
		let a1 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(half))));
		model.child(
			anchor,
			InstanceBuilder::new("Beam")
				.with_name("Beam")
				.with_property("Attachment0", Variant::Ref(a0))
				.with_property("Attachment1", Variant::Ref(a1))
				.with_property("Color", super::particles::solid([1.0, 1.0, 1.0]))
				.with_property("Transparency", super::particles::constant(0.0))
				.with_property("Width0", side)
				.with_property("Width1", side)
				.with_property("FaceCamera", true)
				.with_property("Segments", 1i32)
				.with_property("LightEmission", 0.0f32)
				.with_property("LightInfluence", 0.0f32)
				.with_property("TextureSpeed", 0.0f32),
		);
		probes.push(Probe { label: format!("beam-d{depth}"), position: row(3.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "beam"}) });
	}
	backdrop(&mut model, Vec3::new(0.0, -40.0, 400.0), Vec3::new(1000.0, 120.0, 1.0));
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 50.0 };
	let dof = |far: f32, focus: f32, radius: f32, near: f32| PostEffect::DepthOfField { far_intensity: far, focus_distance: focus, in_focus_radius: radius, near_intensity: near };
	let cases: Vec<(&str, Vec<PostEffect>)> = vec![("none", vec![]), ("cutscene", vec![dof(0.79, 85.0, 30.0, 0.0)]), ("near", vec![dof(0.0, 40.0, 10.0, 1.0)])];
	let mut runs = Vec::new();
	for (label, effects) in cases {
		runs.push(Run { label: label.into(), environment: Environment { effects, ..flat() }, camera: camera.clone() });
		super::particles::register(name, label, super::particles::freeze(0.0, &[]));
	}
	Experiment { name: name.into(), model, probes, runs }
}

/// What DepthOfField does to what blends, by its own depth and by the depth behind it: white Beams,
/// particles and half-transparent parts at 20, 50, 100 and 200 studs, each beside an opaque tile, in
/// two lanes: one before a black tile 10 % of its depth behind it, one before the black backdrop 600
/// studs out. Fields: Roblox's default (FocusDistance 0.05, InFocusRadius 10, Far and Near 0.75), its
/// far half alone, a cutscene's (85, 30, 0.79, near 0) and a near-only one.
/// Studio freezes the particles at age 0.
pub fn post_depth_blended() -> Experiment {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::Variant;
	let name = "post-depth-blended";
	let mut model = ModelBuilder::new("PostDepthBlended");
	let mut probes = Vec::new();
	for (lane, behind) in [(0usize, true), (1, false)] {
		for (row, depth) in [20.0f32, 50.0, 100.0, 200.0].into_iter().enumerate() {
			let side = 0.05 * depth;
			// screen position: lanes left and right, rows down the view
			let at = |column: f32| Vec3::new(((lane as f32 - 0.5) * 6.0 + (column - 1.5) * 1.4) * side / 1.0, (1.5 - row as f32) * 1.3 * side, depth);
			let label = |kind: &str| format!("{kind}-d{depth}-{}", if behind { "near" } else { "far" });
			tile(&mut model, &mut probes, label("opaque"), at(0.0), Vec3::new(side, side, side * 0.02), [255; 3], 272, serde_json::json!({"depth": depth, "kind": "opaque", "behind": behind}));
			model.part(&PartSpec { name: label("glass"), cframe: Affine3A::from_translation(at(1.0)), size: Vec3::new(side, side, side * 0.02), color: [255; 3], material: 272, transparency: 0.5, cast_shadow: false, ..Default::default() });
			probes.push(Probe { label: label("glass"), position: at(1.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "transparent", "behind": behind}) });
			super::particles::add_emitter(&mut model, &label("particle"), Affine3A::from_translation(at(2.0)), &[("Size", super::particles::constant(side * 0.5))]);
			probes.push(Probe { label: label("particle"), position: at(2.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "particle", "behind": behind}) });
			let anchor = model.part(&PartSpec { name: label("beam"), cframe: Affine3A::from_translation(at(3.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			let half = Vec3::new(0.0, side * 0.5, 0.0);
			let a0 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(-half))));
			let a1 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(half))));
			model.child(
				anchor,
				InstanceBuilder::new("Beam")
					.with_name("Beam")
					.with_property("Attachment0", Variant::Ref(a0))
					.with_property("Attachment1", Variant::Ref(a1))
					.with_property("Color", super::particles::solid([1.0, 1.0, 1.0]))
					.with_property("Transparency", super::particles::constant(0.0))
					.with_property("Width0", side)
					.with_property("Width1", side)
					.with_property("FaceCamera", true)
					.with_property("Segments", 1i32)
					.with_property("LightEmission", 0.0f32)
					.with_property("LightInfluence", 0.0f32)
					.with_property("TextureSpeed", 0.0f32),
			);
			probes.push(Probe { label: label("beam"), position: at(3.0).into(), inputs: serde_json::json!({"depth": depth, "kind": "beam", "behind": behind}) });
			if behind {
				// a black tile behind the lane's row, 10 % of its depth further out
				let center = Vec3::new(((lane as f32 - 0.5) * 6.0) * side, (1.5 - row as f32) * 1.3 * side, depth * 1.1);
				backdrop(&mut model, center, Vec3::new(side * 6.5 * 1.1, side * 1.25 * 1.1, 0.2));
			}
		}
	}
	backdrop(&mut model, Vec3::new(0.0, 0.0, 600.0), Vec3::new(2000.0, 2000.0, 1.0));
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 10.0], fov: 50.0 };
	let dof = |far: f32, focus: f32, radius: f32, near: f32| PostEffect::DepthOfField { far_intensity: far, focus_distance: focus, in_focus_radius: radius, near_intensity: near };
	let cases: Vec<(&str, Vec<PostEffect>)> = vec![("none", vec![]), ("default", vec![dof(0.75, 0.05, 10.0, 0.75)]), ("default-far", vec![dof(0.75, 0.05, 10.0, 0.0)]), ("cutscene", vec![dof(0.79, 85.0, 30.0, 0.0)]), ("near", vec![dof(0.0, 300.0, 10.0, 1.0)])];
	let mut runs = Vec::new();
	for (label, effects) in cases {
		runs.push(Run { label: label.into(), environment: Environment { effects, ..flat() }, camera: camera.clone() });
		super::particles::register(name, label, super::particles::freeze(0.0, &[]));
	}
	Experiment { name: name.into(), model, probes, runs }
}

/// SunRaysEffect: the sun in view (ClockTime 16.5, low in the west) over the default sky, with and
/// without a comb of black slabs in front of it (the rays come through the gaps), Intensity and
/// Spread swept; and the sun just outside the view.
pub fn post_sunrays() -> Experiment {
	let mut model = ModelBuilder::new("PostSunRays");
	let mut probes = Vec::new();
	let clock = 16.5f32;
	let sun = crate::render::environment::sun_direction(clock, 0.0);
	let right = sun.cross(Vec3::Y).normalize();
	let up = right.cross(sun).normalize();
	let center = sun * 60.0;
	for index in 0..5 {
		let offset = right * ((index as f32 - 2.0) * 3.0);
		let cframe = crate::scene::build::facing(center + offset, -sun);
		model.part(&PartSpec { name: format!("slab{index}"), cframe, size: Vec3::new(1.2, 14.0, 0.5), color: [0, 0, 0], material: 272, cast_shadow: false, ..Default::default() });
	}
	probes.push(Probe { label: "sun".into(), position: (sun * 1000.0).into(), inputs: serde_json::json!({"kind": "sun"}) });
	let eye = Vec3::ZERO;
	let toward = CameraSpec::LookAt { eye: eye.into(), target: (sun * 100.0).into(), fov: 50.0 };
	let beside = CameraSpec::LookAt { eye: eye.into(), target: ((sun + right * 1.2).normalize() * 100.0).into(), fov: 50.0 };
	let above = CameraSpec::LookAt { eye: eye.into(), target: ((sun + up * 0.35).normalize() * 100.0).into(), fov: 50.0 };
	let base = Environment { clock_time: clock, ..Environment::default() };
	let rays = |intensity: f32, spread: f32| vec![PostEffect::SunRays { intensity, spread }];
	let cases: Vec<(&str, Vec<PostEffect>, &CameraSpec)> = vec![
		("none", vec![], &toward),
		("default", rays(0.25, 1.0), &toward),
		("i0.1", rays(0.1, 1.0), &toward),
		("i0.5", rays(0.5, 1.0), &toward),
		("i1", rays(1.0, 1.0), &toward),
		("s0.1", rays(0.25, 0.1), &toward),
		("s0.5", rays(0.25, 0.5), &toward),
		("lobby", rays(0.01, 0.1), &toward),
		("none-above", vec![], &above),
		("default-above", rays(0.25, 1.0), &above),
		("none-beside", vec![], &beside),
		("default-beside", rays(0.25, 1.0), &beside),
		// under ColorGradingEffect's Retro tone curve (tone-retro)
		("retro", vec![PostEffect::ColorGrading { tonemapper: 1 }], &toward),
		("retro-i0.5", vec![PostEffect::SunRays { intensity: 0.5, spread: 1.0 }, PostEffect::ColorGrading { tonemapper: 1 }], &toward),
	];
	let runs = cases.into_iter().map(|(label, effects, camera)| Run { label: label.into(), environment: Environment { effects, ..base.clone() }, camera: camera.clone() }).collect();
	Experiment { name: "post-sunrays".into(), model, probes, runs }
}


/// SunRaysEffect's colour and reach (post-sunrays-sky's set-up, no wall): the sun looked at from
/// sunrise to sunset, at noon 60 degrees north (the elevation of 10:00 at the equator), the moon at
/// night; Brightness 1 and 4, a pink ColorShift_Top; 25 and 90 degree fields of view; the sun near
/// the edge of the view and just outside it; Spread 0.05, 0.15 and 0.75.
pub fn post_sunrays_time() -> Experiment {
	let mut model = ModelBuilder::new("PostSunRaysTime");
	model.part(&PartSpec { name: "Anchor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -2000.0, 0.0)), size: Vec3::splat(0.2), ..Default::default() });
	let probes = Vec::new();
	let black = Sky { celestial_bodies_shown: true, ..super::environment::uniform_sky(super::SKY_DARK) };
	let base = Environment { clock_time: 16.5, sky: Some(black), ..Environment::default() };
	let look = |direction: Vec3, fov: f32| CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: (direction.normalize() * 100.0).into(), fov };
	let sun_at = |clock: f32, latitude: f32| crate::render::environment::sun_direction(clock, latitude);
	let sun = sun_at(16.5, 0.0);
	let right = sun.cross(Vec3::Y).normalize();
	let rays = |intensity: f32, spread: f32| vec![PostEffect::SunRays { intensity, spread }];
	let mut cases: Vec<(String, Environment, Vec<PostEffect>, CameraSpec)> = Vec::new();
	for clock in [6.25f32, 6.5, 7.0, 8.0, 9.0, 10.0, 12.0, 14.0, 16.0, 17.0, 17.5, 17.75] {
		cases.push((format!("t{clock}"), Environment { clock_time: clock, ..base.clone() }, rays(0.05, 1.0), look(sun_at(clock, 0.0), 50.0)));
	}
	cases.push(("noon-lat60".into(), Environment { clock_time: 12.0, geographic_latitude: 60.0, ..base.clone() }, rays(0.05, 1.0), look(sun_at(12.0, 60.0), 50.0)));
	for clock in [22.0f32, 0.0] {
		let moon = crate::render::environment::moon_direction(clock, 0.0);
		cases.push((format!("moon{clock}"), Environment { clock_time: clock, ..base.clone() }, rays(0.25, 1.0), look(moon, 50.0)));
	}
	cases.push(("b1".into(), Environment { brightness: 1.0, ..base.clone() }, rays(0.05, 1.0), look(sun, 50.0)));
	cases.push(("b4".into(), Environment { brightness: 4.0, ..base.clone() }, rays(0.05, 1.0), look(sun, 50.0)));
	cases.push(("shift".into(), Environment { color_shift_top: Vec3::new(1.0, 0.525, 0.675), ..base.clone() }, rays(0.05, 1.0), look(sun, 50.0)));
	cases.push(("fov25".into(), base.clone(), rays(0.05, 1.0), look(sun, 25.0)));
	cases.push(("fov90".into(), base.clone(), rays(0.05, 1.0), look(sun, 90.0)));
	cases.push(("fov25-s0.1".into(), base.clone(), rays(0.01, 0.1), look(sun, 25.0)));
	cases.push(("edge-in".into(), base.clone(), rays(0.05, 1.0), look(sun - right * 0.55, 50.0)));
	cases.push(("edge-out".into(), base.clone(), rays(0.05, 1.0), look(sun - right * 0.9, 50.0)));
	cases.push(("edge-in-s0.1".into(), base.clone(), rays(0.01, 0.1), look(sun - right * 0.55, 50.0)));
	for (spread, intensity) in [(0.05f32, 0.01f32), (0.1, 0.01), (0.15, 0.01), (0.25, 0.02), (0.75, 0.05)] {
		cases.push((format!("s{spread}"), base.clone(), rays(intensity, spread), look(sun, 50.0)));
	}
	let mut runs = Vec::new();
	for (label, environment, effects, camera) in cases {
		runs.push(Run { label: format!("{label}-none"), environment: environment.clone(), camera: camera.clone() });
		runs.push(Run { label, environment: Environment { effects, ..environment }, camera });
	}
	Experiment { name: "post-sunrays-time".into(), model, probes, runs }
}

/// Does an occluder's distance change the shadow it casts in SunRaysEffect? A black slab over the sun
/// (1.15 by 13.3 degrees, as post-sunrays' middle slab) seen from 10 to 2000 studs, each from its own
/// camera far from the others, Intensity 0.25 and Spread 1 over the default sky.
pub fn post_sunrays_depth() -> Experiment {
	let mut model = ModelBuilder::new("PostSunRaysDepth");
	let clock = 16.5f32;
	let sun = crate::render::environment::sun_direction(clock, 0.0);
	let base = Environment { clock_time: clock, ..Environment::default() };
	let rays = vec![PostEffect::SunRays { intensity: 0.25, spread: 1.0 }];
	let mut runs = Vec::new();
	for (index, distance) in [10.0f32, 30.0, 60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0].into_iter().enumerate() {
		let centre = Vec3::new(index as f32 * 6000.0 - 21000.0, 100.0, 0.0);
		model.part(&PartSpec {
			name: format!("slab{distance}"),
			cframe: crate::scene::build::facing(centre, -sun),
			size: Vec3::new(0.02 * distance, 0.2333 * distance, 0.01 * distance),
			color: [0, 0, 0],
			material: 288,
			cast_shadow: false,
			..Default::default()
		});
		let eye = centre - sun * distance;
		let camera = CameraSpec::LookAt { eye: eye.into(), target: centre.into(), fov: 50.0 };
		runs.push(Run { label: format!("d{distance}-none"), environment: base.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("d{distance}"), environment: Environment { effects: rays.clone(), ..base.clone() }, camera });
	}
	Experiment { name: "post-sunrays-depth".into(), model, probes: Vec::new(), runs }
}

/// SunRaysEffect's colour through dawn and dusk, finely (Intensity 0.05, Spread 1, the sun hidden over
/// a black skybox, looked at); 40 degrees south at 16:00 and noon (does the colour follow the clock or
/// the sun's elevation?).
pub fn post_sunrays_clock() -> Experiment {
	let mut model = ModelBuilder::new("PostSunRaysClock");
	model.part(&PartSpec { name: "Anchor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -2000.0, 0.0)), size: Vec3::splat(0.2), ..Default::default() });
	let base = Environment { sky: Some(super::environment::uniform_sky(super::SKY_DARK)), ..Environment::default() };
	let look = |direction: Vec3| CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: (direction.normalize() * 100.0).into(), fov: 50.0 };
	let rays = vec![PostEffect::SunRays { intensity: 0.05, spread: 1.0 }];
	let mut cases: Vec<(String, f32, f32)> = Vec::new();
	for clock in [5.95f32, 6.0, 6.05, 6.125, 6.375, 6.75, 7.25, 7.5, 7.75] {
		cases.push((format!("t{clock}"), clock, 0.0));
	}
	for clock in [16.125f32, 16.25, 16.375, 16.5, 16.625, 16.75, 16.875, 17.0, 17.25, 17.625, 17.875, 17.95, 18.0, 18.05] {
		cases.push((format!("t{clock}"), clock, 0.0));
	}
	cases.push(("lat-40-t16".into(), 16.0, -40.0));
	cases.push(("lat-40-t12".into(), 12.0, -40.0));
	let mut runs = Vec::new();
	for (label, clock, latitude) in cases {
		let environment = Environment { clock_time: clock, geographic_latitude: latitude, ..base.clone() };
		let camera = look(crate::render::environment::sun_direction(clock, latitude));
		runs.push(Run { label: format!("{label}-none"), environment: environment.clone(), camera: camera.clone() });
		runs.push(Run { label, environment: Environment { effects: rays.clone(), ..environment }, camera });
	}
	Experiment { name: "post-sunrays-clock".into(), model, probes: Vec::new(), runs }
}
