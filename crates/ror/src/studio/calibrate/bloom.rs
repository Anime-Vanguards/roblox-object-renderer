//! Bloom experiments: Studio's built-in bloom (no BloomEffect) and BloomEffect, on Neon, lit
//! SmoothPlastic and beam sources over a black, unlit backdrop (notes/render.md "Bloom";
//! tools/analysis/bloom_fit.py). Unless noted the camera looks straight at the z = 0 plane from 60
//! studs through a 40° field of view: 25.5 px per stud at the 1116 px viewport.

// fixture helpers take every property of the source they place
#![allow(clippy::too_many_arguments)]

use glam::{Affine3A, Quat, Vec3};
use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Variant};

use super::{Experiment, Probe, Run, add_beam, dark};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::environment::PostEffect;

/// The view every bloom experiment shares (or scales, `bloom-kernel`).
fn camera(target: Vec3) -> CameraSpec {
	CameraSpec::LookAt { eye: (target - Vec3::Z * 60.0).into(), target: target.into(), fov: 40.0 }
}

/// A black, unlit wall behind the sources (black Neon: nothing lights it).
fn backdrop(model: &mut ModelBuilder, center: Vec3) {
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 6.0)),
		size: Vec3::new(160.0, 100.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		cast_shadow: false,
		..Default::default()
	});
}

/// A flat square tile facing the camera (-Z): its front face at z = -0.1.
fn tile(model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, center: Vec3, size: Vec3, color: [u8; 3], material: u32, inputs: serde_json::Value) {
	model.part(&PartSpec { name: label.clone(), cframe: Affine3A::from_translation(center), size, color, material, cast_shadow: false, ..Default::default() });
	let mut inputs = inputs;
	inputs["center"] = serde_json::json!([center.x, center.y, center.z - size.z * 0.5]);
	inputs["size"] = serde_json::json!([size.x, size.y]);
	probes.push(Probe { label, position: (center - Vec3::Z * size.z * 0.5).into(), inputs });
}

fn solid(color: [f32; 3]) -> Variant {
	let c = Color3::new(color[0], color[1], color[2]);
	ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, c), ColorSequenceKeypoint::new(1.0, c)] }.into()
}

/// An unlit, opaque beam ribbon `width` x `height` studs centred on `center`, facing the camera.
fn beam_square(model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, center: Vec3, width: f32, height: f32, color: [f32; 3], brightness: f32, inputs: serde_json::Value) {
	// the attachments' X axes along +Y (the beam runs up), their Y axes (the width) along -X
	let up = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
	add_beam(
		model,
		&label,
		center,
		Affine3A::from_rotation_translation(up, Vec3::new(0.0, -height * 0.5, 0.0)),
		Affine3A::from_rotation_translation(up, Vec3::new(0.0, height * 0.5, 0.0)),
		&[("Color", solid(color)), ("Brightness", brightness.into()), ("Width0", width.into()), ("Width1", width.into())],
	);
	let mut inputs = inputs;
	inputs["center"] = serde_json::json!([center.x, center.y, center.z]);
	inputs["size"] = serde_json::json!([width, height]);
	probes.push(Probe { label, position: center.into(), inputs });
}

/// Where the halo starts: three ladders of 2.4-stud squares whose radiance steps across the
/// threshold — Neon grays (x = 25 c⁴), white-to-gray SmoothPlastic under a flat Ambient of 1
/// (x = 1.98 c²) and white unlit beams (x = 1.935 Brightness) — at ExposureCompensation 0.5-2.5,
/// so every ladder crosses it several times, and at -1 and -2 (the ladders' relative brightness
/// where the tone curve is best known). Neon and beams at equal x are compared directly.
pub fn bloom_threshold() -> Experiment {
	let mut model = ModelBuilder::new("BloomThreshold");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let size = Vec3::new(2.4, 2.4, 0.2);
	for index in 0..10 {
		let x = 27.0 - index as f32 * 6.0;
		let gray = 128 + index as u8 * 2;
		let emission = 25.0 * (gray as f32 / 255.0).powi(4);
		tile(&mut model, &mut probes, format!("neon{gray}"), Vec3::new(x, 10.0, 0.0), size, [gray; 3], 288, serde_json::json!({"kind": "neon", "x": emission}));
		let plastic = 200 + index as u8 * 6;
		tile(&mut model, &mut probes, format!("plastic{plastic}"), Vec3::new(x, 0.0, 0.0), size, [plastic; 3], 272, serde_json::json!({"kind": "plastic", "x": 1.98 * (plastic as f32 / 255.0).powi(2)}));
		let brightness = 0.82 + index as f32 * 0.06;
		beam_square(&mut model, &mut probes, format!("beam{brightness:.2}"), Vec3::new(x, -10.0, 0.0), 2.4, 2.4, [1.0; 3], brightness, serde_json::json!({"kind": "beam", "x": 1.935 * brightness}));
	}
	let lit = Environment { ambient: Vec3::ONE, ..dark() };
	let mut runs: Vec<Run> = [-2.0f32, -1.0, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5]
		.iter()
		.map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..lit.clone() }, camera: camera(Vec3::ZERO) })
		.collect();
	// Neon and beams without the ambient (is Neon's brightness its own?)
	for exposure in [-2.0f32, -1.0] {
		runs.push(Run { label: format!("dark-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) });
	}
	Experiment { name: "bloom-threshold".into(), model, probes, runs }
}

/// Does the halo follow the source's area? White Neon squares of 1 to 82 px and thin lines (a
/// capped source at EV 0 and -1, x = 25 and 12.5; uncapped at -2, x = 6.25), and the same shapes in
/// gray Neon (x = 25 (150/255)⁴ = 2.99) pushed just over the threshold at EV +0.5 and +1.
pub fn bloom_size() -> Experiment {
	let mut model = ModelBuilder::new("BloomSize");
	let mut probes = Vec::new();
	let sizes = [0.04f32, 0.08, 0.12, 0.2, 0.32, 0.5, 0.8, 1.2, 2.0, 3.2];
	for (group, gray) in [(0usize, 255u8), (1, 150)] {
		let origin = Vec3::new(group as f32 * 400.0, 0.0, 0.0);
		backdrop(&mut model, origin);
		let emission = 25.0 * (gray as f32 / 255.0).powi(4);
		for (index, side) in sizes.iter().enumerate() {
			let center = origin + Vec3::new(25.0 - (index % 5) as f32 * 12.5, 10.0 - (index / 5) as f32 * 13.0, 0.0);
			tile(&mut model, &mut probes, format!("g{gray}-square{side}"), center, Vec3::new(*side, *side, 0.2), [gray; 3], 288, serde_json::json!({"kind": "square", "x": emission, "side": side}));
		}
		for (index, thickness) in [0.04f32, 0.12].iter().enumerate() {
			let center = origin + Vec3::new(12.0 - index as f32 * 24.0, -14.0, 0.0);
			tile(&mut model, &mut probes, format!("g{gray}-line{thickness}"), center, Vec3::new(4.0, *thickness, 0.2), [gray; 3], 288, serde_json::json!({"kind": "line", "x": emission, "thickness": thickness}));
		}
	}
	let mut runs = Vec::new();
	for exposure in [0.0f32, -1.0, -2.0] {
		runs.push(Run { label: format!("white-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) });
	}
	for exposure in [0.5f32, 1.0] {
		runs.push(Run { label: format!("gray-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::new(400.0, 0.0, 0.0)) });
	}
	Experiment { name: "bloom-size".into(), model, probes, runs }
}

/// Pixels per stud on the target plane of [`camera`] (1116 px tall, 40°, 60 studs), less the
/// thin tiles' half depth.
const PIXELS_PER_STUD: f32 = 1116.0 * 0.5 / 0.363_970_23 / 59.99;

/// The world Y of the edge at pixel row `row` (a pixel boundary) on that plane.
fn row_y(row: f32) -> f32 {
	(558.0 - row) / PIXELS_PER_STUD
}

/// The world X of the edge at pixel column `column` on that plane (screen x grows towards world -X:
/// the camera looks along +Z).
fn column_x(column: f32) -> f32 {
	-(column - 822.0) / PIXELS_PER_STUD
}

/// A flat white Neon rectangle (0.02 studs deep) whose front face covers the pixel rectangle
/// `left`..`left + width` x `top`..`top + height` of a view at `origin` (pixel boundaries may be
/// fractional).
fn pixel_tile(model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, origin: Vec3, left: f32, top: f32, width: f32, height: f32, inputs: serde_json::Value) {
	let size = Vec3::new(width / PIXELS_PER_STUD, height / PIXELS_PER_STUD, 0.02);
	let center = origin + Vec3::new(column_x(left) - size.x * 0.5, row_y(top) - size.y * 0.5, 0.0);
	let mut inputs = inputs;
	inputs["left"] = serde_json::json!(left);
	inputs["top"] = serde_json::json!(top);
	inputs["depth"] = serde_json::json!(0.02);
	tile(model, probes, label, center, size, [255; 3], 288, inputs);
}

/// Where a thin source sits on the pixel grid: white Neon lines 2.5-6 px thick and 120 px long
/// (rows), their top edges stepping through the rows modulo 8 (columns 205 px apart); 400 studs away
/// the same as vertical lines, their left edges through the columns modulo 8. At EV 0 and -1
/// (clamped), -2 (x = 6.4) and -2.5 (4.55): a bloom that thresholds on a grid coarser than the
/// pixels shows a period and halos off the lines' centres (bloom-lines: Studio's halo of a 4 px
/// line at EV -2 is centred 1.5 px above it, a 3 px line's 1 px below).
pub fn bloom_phase() -> Experiment {
	let mut model = ModelBuilder::new("BloomPhase");
	let mut probes = Vec::new();
	let widths = [2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0];
	let origin = Vec3::new(400.0, 0.0, 0.0);
	backdrop(&mut model, Vec3::ZERO);
	backdrop(&mut model, origin);
	for (row, px) in widths.iter().enumerate() {
		for phase in 0..8 {
			// rows 60 + 184 r + k: phases (4 + k) mod 8
			let top = 60.0 + row as f32 * 184.0 + phase as f32;
			let inputs = serde_json::json!({"kind": "line", "x": 25.0, "px": px, "axis": "h", "phase": phase});
			pixel_tile(&mut model, &mut probes, format!("h{px}-{phase}"), Vec3::ZERO, 42.0 + phase as f32 * 205.0, top, 120.0, *px, inputs);
			// columns 60 + 201 k: phases (4 + k) mod 8
			let left = 60.0 + phase as f32 * 201.0;
			let inputs = serde_json::json!({"kind": "line", "x": 25.0, "px": px, "axis": "v", "phase": phase});
			pixel_tile(&mut model, &mut probes, format!("v{px}-{phase}"), origin, left, 60.0 + row as f32 * 184.0, *px, 120.0, inputs);
		}
	}
	let mut runs = Vec::new();
	for (group, target) in [("h", Vec3::ZERO), ("v", origin)] {
		for exposure in [0.0f32, -1.0, -2.0, -2.5] {
			runs.push(Run { label: format!("{group}-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(target) });
		}
	}
	Experiment { name: "bloom-phase".into(), model, probes, runs }
}

/// Small sources at every phase of an 8 px grid in both axes: white Neon squares of 4 and 6 px
/// whose top-left corners step through the columns and rows modulo 8 (8 x 8 of each size, 101 and
/// 137 px apart), at EV 0 (clamped) and -2 (x = 6.4). Does the halo's centre or strength jump?
pub fn bloom_dots() -> Experiment {
	let mut model = ModelBuilder::new("BloomDots");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	for column in 0..16 {
		let side = if column < 8 { 4.0f32 } else { 6.0 };
		// columns 30 + 101 c: phases (6 + 5 c) mod 8; rows 40 + 137 r: phases r mod 8
		let left = 30.0 + column as f32 * 101.0;
		for row in 0..8 {
			let top = 40.0 + row as f32 * 137.0;
			let inputs = serde_json::json!({"kind": "square", "x": 25.0, "px": side});
			pixel_tile(&mut model, &mut probes, format!("d{side}-{column}-{row}"), Vec3::ZERO, left, top, side, side, inputs);
		}
	}
	let runs = [0.0f32, -2.0]
		.iter()
		.map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) })
		.collect();
	Experiment { name: "bloom-dots".into(), model, probes, runs }
}

/// The halo's shape out to where it ends: white Neon squares of 2 to 96 px (edges on pixel
/// boundaries, some one pixel off the others' phase) and a 16 px bar over the black backdrop, under
/// ColorGradingEffect's Retro, whose toe shows the faint end 2-3x finer than the default curve (x 0.003
/// shows 5 of 255 against 1.5), at EV 0 (capped), -2 (x 6.44) and -2.5 (4.55), the default curve at EV 0,
/// and a BloomEffect of Threshold 0 at EV -2 (every source glows: the chain's response to the 2-6 px
/// squares, which the built-in threshold leaves dark).
pub fn bloom_shape() -> Experiment {
	let mut model = ModelBuilder::new("BloomShape");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	// (label, left, top, side), at least 240 px apart
	let squares: [(&str, f32, f32, f32); 12] = [
		("s2", 120.0, 110.0, 2.0),
		("s3", 420.0, 110.0, 3.0),
		("s6", 720.0, 110.0, 6.0),
		("s12", 1020.0, 110.0, 12.0),
		("s24", 1320.0, 110.0, 24.0),
		("s6b", 121.0, 421.0, 6.0),
		("s12b", 421.0, 421.0, 12.0),
		("s24b", 721.0, 415.0, 24.0),
		("s48", 1021.0, 403.0, 48.0),
		("s3b", 1321.0, 421.0, 3.0),
		("s96", 120.0, 650.0, 96.0),
		("s48b", 600.0, 674.0, 48.0),
	];
	for (label, left, top, side) in squares {
		pixel_tile(&mut model, &mut probes, label.into(), Vec3::ZERO, left, top, side, side, serde_json::json!({"kind": "square", "x": 25.75, "px": side}));
	}
	pixel_tile(&mut model, &mut probes, "bar".into(), Vec3::ZERO, 900.0, 690.0, 520.0, 16.0, serde_json::json!({"kind": "bar", "x": 25.75, "px": 16.0}));
	let retro = PostEffect::ColorGrading { tonemapper: 1 };
	let cases: Vec<(&str, Vec<PostEffect>, f32)> = vec![
		("default-ev0", vec![], 0.0),
		("retro-ev0", vec![retro.clone()], 0.0),
		("retro-ev-2", vec![retro.clone()], -2.0),
		("retro-ev-2.5", vec![retro.clone()], -2.5),
		("retro-threshold0", vec![retro, PostEffect::Bloom { intensity: 1.0, size: 24.0, threshold: 0.0 }], -2.0),
	];
	let runs = cases.into_iter().map(|(label, effects, exposure)| Run { label: label.into(), environment: Environment { effects, exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) }).collect();
	Experiment { name: "bloom-shape".into(), model, probes, runs }
}

/// The halo's colour from just over the threshold to the cap: Neon tiles of 1.6 studs in 20 colours
/// (a channel at 255 and the others at 0 or at 0.10, 0.25, 0.50 and 0.75 of its x: c = 143, 180, 214,
/// 237; a ship model's strip colour; near-whites), 10 studs apart, under ColorGradingEffect's Retro (finer
/// in the dark) at EV -2.5 (the strongest channel at x 4.55, just over the threshold), -2.25, -2, -1
/// (capped) and 0, and the default curve at -2.
pub fn bloom_hue() -> Experiment {
	let mut model = ModelBuilder::new("BloomHue");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let colors: [[u8; 3]; 20] = [
		[255, 0, 0],
		[0, 255, 0],
		[0, 0, 255],
		[255, 255, 255],
		[255, 214, 0],
		[255, 180, 0],
		[255, 237, 0],
		[0, 214, 255],
		[0, 255, 214],
		[214, 0, 255],
		[255, 0, 214],
		[180, 255, 0],
		[255, 214, 180],
		[180, 214, 255],
		[214, 255, 180],
		[255, 143, 143],
		[143, 255, 255],
		[140, 177, 121],
		[237, 255, 237],
		[255, 255, 180],
	];
	for (index, color) in colors.iter().enumerate() {
		// screen x grows towards world -X (the camera looks along +Z)
		let center = Vec3::new(20.0 - (index % 5) as f32 * 10.0, 15.0 - (index / 5) as f32 * 10.0, 0.0);
		let x: Vec<f32> = color.iter().map(|c| 25.75 * (*c as f32 / 255.0).powi(4)).collect();
		tile(&mut model, &mut probes, format!("neon{}-{}-{}", color[0], color[1], color[2]), center, Vec3::new(1.6, 1.6, 0.2), *color, 288, serde_json::json!({"kind": "neon", "color": color, "x": x}));
	}
	let retro = PostEffect::ColorGrading { tonemapper: 1 };
	let mut runs: Vec<Run> = [-2.5f32, -2.25, -2.0, -1.0, 0.0]
		.iter()
		.map(|&exposure| Run { label: format!("retro-ev{exposure}"), environment: Environment { effects: vec![retro.clone()], exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) })
		.collect();
	runs.push(Run { label: "default-ev-2".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: camera(Vec3::ZERO) });
	Experiment { name: "bloom-hue".into(), model, probes, runs }
}

/// Do thin sources merge before the threshold? Pairs of white Neon lines 2 px thick (neither glows
/// alone at any phase, bloom-lines) 1, 2, 3, 4 and 6 px apart and of 1 px lines 1 px apart, 120 px
/// long, their top edges through the rows modulo 8 (columns), at EV 0 and -1 (clamped).
pub fn bloom_pairs() -> Experiment {
	let mut model = ModelBuilder::new("BloomPairs");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let pairs: [(f32, f32); 6] = [(2.0, 1.0), (2.0, 2.0), (2.0, 3.0), (2.0, 4.0), (2.0, 6.0), (1.0, 1.0)];
	for (row, (px, gap)) in pairs.iter().enumerate() {
		for phase in 0..8 {
			let top = 60.0 + row as f32 * 184.0 + phase as f32;
			let left = 42.0 + phase as f32 * 205.0;
			for (index, offset) in [0.0, px + gap].iter().enumerate() {
				let inputs = serde_json::json!({"kind": "line", "x": 25.0, "px": px, "gap": gap, "axis": "h", "phase": phase});
				pixel_tile(&mut model, &mut probes, format!("p{px}-{gap}-{phase}-{index}"), Vec3::ZERO, left, top + offset, 120.0, *px, inputs);
			}
		}
	}
	let runs = [0.0f32, -1.0]
		.iter()
		.map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) })
		.collect();
	Experiment { name: "bloom-pairs".into(), model, probes, runs }
}

/// How the halo depends on a thin source's width and where it sits on the pixel grid: flat white
/// Neon lines (0.02 studs deep, edges on pixel boundaries), horizontal ones 1-16 px thick and 10
/// studs long stacked 153 px apart in two columns, vertical ones in a third; and, in a second view,
/// sixteen 2-px lines whose top edges step through the rows modulo 16 (a coarser grid in the bloom
/// would show as a period). At EV 0, -1, -2, -2.5 and +1 (x = 25, 12.5, 6.25, 4.4, 50).
pub fn bloom_lines() -> Experiment {
	let mut model = ModelBuilder::new("BloomLines");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let thicknesses = [[1.0f32, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0], [1.5, 2.5, 3.5, 5.0, 7.0, 10.0, 16.0]];
	for (column, list) in thicknesses.iter().enumerate() {
		for (row, px) in list.iter().enumerate() {
			// top edge on a pixel boundary: rows 100, 253, 406, ...
			let top = 100.0 + row as f32 * 153.0;
			let height = px / PIXELS_PER_STUD;
			let center = Vec3::new(if column == 0 { 15.0 } else { -3.0 }, row_y(top) - height * 0.5, 0.0);
			tile(&mut model, &mut probes, format!("h{px}"), center, Vec3::new(10.0, height, 0.02), [255; 3], 288, serde_json::json!({"kind": "line", "x": 25.0, "px": px, "axis": "h", "top": top}));
		}
	}
	// vertical lines at the right: left edges on pixel columns
	for (index, px) in [1.0f32, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0].iter().enumerate() {
		let left = 1240.0 + index as f32 * 55.0;
		let width = px / PIXELS_PER_STUD;
		// screen x grows towards world -X (the camera looks along +Z)
		let x = -(left - 822.0) / PIXELS_PER_STUD - width * 0.5;
		tile(&mut model, &mut probes, format!("v{px}"), Vec3::new(x, 0.0, 0.0), Vec3::new(width, 30.0, 0.02), [255; 3], 288, serde_json::json!({"kind": "line", "x": 25.0, "px": px, "axis": "v", "left": left}));
	}
	// the phase group, 400 studs away: 2-px lines, 4 studs long, top edges at rows 16 k + k
	let origin = Vec3::new(400.0, 0.0, 0.0);
	backdrop(&mut model, origin);
	for k in 0..16 {
		let column = (k % 4) as f32;
		let row = (k / 4) as f32;
		let top = 120.0 + row * 256.0 + k as f32;
		let height = 2.0 / PIXELS_PER_STUD;
		let center = origin + Vec3::new(22.5 - column * 15.0, row_y(top) - height * 0.5, 0.0);
		tile(&mut model, &mut probes, format!("phase{k}"), center, Vec3::new(4.0, height, 0.02), [255; 3], 288, serde_json::json!({"kind": "line", "x": 25.0, "px": 2.0, "axis": "h", "top": top, "phase": k}));
	}
	let mut runs = Vec::new();
	for exposure in [0.0f32, -1.0, -2.0, -2.5, 1.0] {
		runs.push(Run { label: format!("lines-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(Vec3::ZERO) });
	}
	for exposure in [0.0f32, -2.0] {
		runs.push(Run { label: format!("phase-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera(origin) });
	}
	Experiment { name: "bloom-lines".into(), model, probes, runs }
}

/// The halo's colour: saturated Neon colours (two rows) and coloured unlit beams (Brightness 3, a
/// row) as 2.4-stud squares, and Neon at a character's ring colour, at ExposureCompensation -1, 0
/// and +1.
pub fn bloom_colors() -> Experiment {
	let mut model = ModelBuilder::new("BloomColors");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let size = Vec3::new(2.4, 2.4, 0.2);
	let neon: [[u8; 3]; 12] = [
		[255, 170, 0],
		[255, 0, 0],
		[0, 255, 0],
		[0, 0, 255],
		[255, 239, 20],
		[0, 255, 255],
		[255, 0, 255],
		[255, 128, 64],
		[64, 128, 255],
		[128, 255, 64],
		[255, 200, 150],
		[200, 200, 200],
	];
	for (index, color) in neon.iter().enumerate() {
		let center = Vec3::new(25.0 - (index % 6) as f32 * 10.0, 14.0 - (index / 6) as f32 * 10.0, 0.0);
		let x: Vec<f32> = color.iter().map(|c| 25.0 * (*c as f32 / 255.0).powi(4)).collect();
		tile(&mut model, &mut probes, format!("neon{}-{}-{}", color[0], color[1], color[2]), center, size, *color, 288, serde_json::json!({"kind": "neon", "color": color, "x": x}));
	}
	let beams: [[f32; 3]; 6] = [[1.0, 0.28235295, 0.0], [1.0, 0.5, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]];
	for (index, color) in beams.iter().enumerate() {
		let center = Vec3::new(25.0 - index as f32 * 10.0, -6.0, 0.0);
		let x: Vec<f32> = color.iter().map(|c| 1.935 * 3.0 * c * c).collect();
		beam_square(&mut model, &mut probes, format!("beam{}-{}-{}", color[0], color[1], color[2]), center, 2.4, 2.4, *color, 3.0, serde_json::json!({"kind": "beam", "color": color, "x": x}));
	}
	// lit SmoothPlastic, coloured, under a flat ambient of 1 (x = 1.98 c²; glows from EV +1)
	let plastic: [[u8; 3]; 6] = [[255, 170, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255], [255, 128, 64]];
	for (index, color) in plastic.iter().enumerate() {
		let center = Vec3::new(25.0 - index as f32 * 10.0, -16.0, 0.0);
		let x: Vec<f32> = color.iter().map(|c| 1.98 * (*c as f32 / 255.0).powi(2)).collect();
		tile(&mut model, &mut probes, format!("plastic{}-{}-{}", color[0], color[1], color[2]), center, size, *color, 272, serde_json::json!({"kind": "plastic", "color": color, "x": x}));
	}
	let lit = Environment { ambient: Vec3::ONE, ..dark() };
	let runs = [-1.0f32, 0.0, 1.0, 2.0]
		.iter()
		.map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..lit.clone() }, camera: camera(Vec3::ZERO) })
		.collect();
	Experiment { name: "bloom-colors".into(), model, probes, runs }
}

/// The kernel's shape: white Neon squares of 4, 15, 61 and 204 px far apart, one cut by the
/// viewport's left edge and one by its bottom edge; seen through 40°, 70° and 20° fields of view
/// from distances that keep 25.5 px per stud (a screen-space kernel then gives identical halos),
/// at EV 0 (x = 25.75, over the cap) and -2 (6.4, under it).
pub fn bloom_kernel() -> Experiment {
	let mut model = ModelBuilder::new("BloomKernel");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let squares: [(&str, f32, Vec3); 6] = [
		("dot", 0.16, Vec3::new(18.0, 12.0, 0.0)),
		("small", 0.6, Vec3::new(0.0, 12.0, 0.0)),
		("medium", 2.4, Vec3::new(-18.0, 12.0, 0.0)),
		("large", 8.0, Vec3::new(4.0, -9.0, 0.0)),
		// half past the left edge (x = 32.2 at the target plane) and the bottom edge (y = -21.8)
		("left-edge", 2.4, Vec3::new(32.2, 2.0, 0.0)),
		("bottom-edge", 2.4, Vec3::new(-20.0, -21.8, 0.0)),
	];
	for (label, side, center) in squares {
		tile(&mut model, &mut probes, label.into(), center, Vec3::new(side, side, 0.2), [255; 3], 288, serde_json::json!({"kind": "square", "x": 25.0, "side": side}));
	}
	let distance = |fov: f32| 60.0 * 20f32.to_radians().tan() / (fov * 0.5).to_radians().tan();
	let view = |fov: f32| CameraSpec::LookAt { eye: [0.0, 0.0, -distance(fov)], target: [0.0, 0.0, 0.0], fov };
	let runs = vec![
		Run { label: "fov40".into(), environment: dark(), camera: view(40.0) },
		Run { label: "fov40-ev-2".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: view(40.0) },
		Run { label: "fov70".into(), environment: dark(), camera: view(70.0) },
		Run { label: "fov20".into(), environment: dark(), camera: view(20.0) },
	];
	Experiment { name: "bloom-kernel".into(), model, probes, runs }
}

/// Is the 7.84 cap on transparent Neon's emission (neon-transparency, at EV 0) in exposed units, as
/// opaque Neon's anti-aliased edges show (bloom-lines)? White Neon squares at Transparency 0.5 and
/// 0.8 and red Neon at 0.5 over the black backdrop, a BloomEffect of Intensity 0 (no halo), at EV
/// -2, -1, 0 and +1: capped before the exposure, √out = 0.5 √7.84 reads 0.49 at -2; after it, 1.61.
pub fn neon_cap() -> Experiment {
	let mut model = ModelBuilder::new("NeonCap");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	for (index, (label, color, transparency)) in [("white50", [255u8; 3], 0.5f32), ("white80", [255; 3], 0.8), ("red50", [255, 0, 0], 0.5)].iter().enumerate() {
		let center = Vec3::new(12.0 - index as f32 * 12.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*label).into(), cframe: Affine3A::from_translation(center), size: Vec3::new(4.0, 4.0, 0.2), color: *color, material: 288, transparency: *transparency, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label: (*label).into(), position: (center - Vec3::Z * 0.1).into(), inputs: serde_json::json!({"color": color, "transparency": transparency}) });
	}
	let runs = [-2.0f32, -1.0, 0.0, 1.0]
		.iter()
		.map(|&exposure| Run {
			label: format!("ev{exposure}"),
			environment: Environment { exposure_compensation: exposure, effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }], ..dark() },
			camera: camera(Vec3::ZERO),
		})
		.collect();
	Experiment { name: "neon-cap".into(), model, probes, runs }
}

/// BloomEffect: the same sources (capped white Neon, Neon at x = 2.99 and 4.94, white SmoothPlastic
/// at x = 1.98 under a flat Ambient of 1, orange Neon, a white beam at x = 3.87) without an effect and
/// with BloomEffects of other Intensity, Size and Threshold. Does it add to the built-in bloom or
/// replace it?
pub fn bloom_effect() -> Experiment {
	let mut model = ModelBuilder::new("BloomEffect");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	let size = Vec3::new(2.4, 2.4, 0.2);
	let neon: [(&str, [u8; 3]); 4] = [("white", [255; 3]), ("gray150", [150; 3]), ("gray170", [170; 3]), ("orange", [255, 170, 0])];
	for (index, (label, color)) in neon.iter().enumerate() {
		let x: Vec<f32> = color.iter().map(|c| 25.0 * (*c as f32 / 255.0).powi(4)).collect();
		tile(&mut model, &mut probes, (*label).into(), Vec3::new(21.0 - index as f32 * 14.0, 9.0, 0.0), size, *color, 288, serde_json::json!({"kind": "neon", "color": color, "x": x}));
	}
	tile(&mut model, &mut probes, "plastic".into(), Vec3::new(14.0, -9.0, 0.0), size, [255; 3], 272, serde_json::json!({"kind": "plastic", "color": [255, 255, 255], "x": [1.98, 1.98, 1.98]}));
	beam_square(&mut model, &mut probes, "beam".into(), Vec3::new(-14.0, -9.0, 0.0), 2.4, 2.4, [1.0; 3], 2.0, serde_json::json!({"kind": "beam", "color": [1, 1, 1], "x": [3.87, 3.87, 3.87]}));
	let lit = Environment { ambient: Vec3::ONE, ..dark() };
	let with = |intensity: f32, size: f32, threshold: f32| Environment { effects: vec![PostEffect::Bloom { intensity, size, threshold }], ..lit.clone() };
	let cases: [(&str, Environment); 10] = [
		("none", lit.clone()),
		("default", with(0.4, 24.0, 0.95)),
		("intensity0", with(0.0, 24.0, 0.95)),
		("intensity1", with(1.0, 24.0, 0.95)),
		("intensity2", with(2.0, 24.0, 0.95)),
		("size8", with(1.0, 8.0, 0.95)),
		("size56", with(1.0, 56.0, 0.95)),
		("threshold0.5", with(1.0, 24.0, 0.5)),
		("threshold1.5", with(1.0, 24.0, 1.5)),
		("threshold3", with(1.0, 24.0, 3.0)),
	];
	let runs = cases.into_iter().map(|(label, environment)| Run { label: label.into(), environment, camera: camera(Vec3::ZERO) }).collect();
	Experiment { name: "bloom-effect".into(), model, probes, runs }
}

/// BloomEffect.Size between bloom-effect's 8, 24 and 56: a white Neon square of 2.4 studs and a
/// 4 px line, both capped, under BloomEffects of Intensity 1 and Threshold 0.95 with Size 2-80.
pub fn bloom_effect_size() -> Experiment {
	let mut model = ModelBuilder::new("BloomEffectSize");
	let mut probes = Vec::new();
	backdrop(&mut model, Vec3::ZERO);
	tile(&mut model, &mut probes, "square".into(), Vec3::new(10.0, 0.0, 0.0), Vec3::new(2.4, 2.4, 0.2), [255; 3], 288, serde_json::json!({"kind": "neon", "color": [255, 255, 255], "x": [25.0, 25.0, 25.0]}));
	pixel_tile(&mut model, &mut probes, "line".into(), Vec3::ZERO, 900.0, 559.0, 240.0, 4.0, serde_json::json!({"kind": "line", "x": 25.0, "px": 4.0, "axis": "h"}));
	let runs = [2.0f32, 4.0, 12.0, 16.0, 32.0, 40.0, 80.0]
		.iter()
		.map(|&size| Run {
			label: format!("size{size}"),
			environment: Environment { effects: vec![PostEffect::Bloom { intensity: 1.0, size, threshold: 0.95 }], ..dark() },
			camera: camera(Vec3::ZERO),
		})
		.collect();
	Experiment { name: "bloom-effect-size".into(), model, probes, runs }
}
