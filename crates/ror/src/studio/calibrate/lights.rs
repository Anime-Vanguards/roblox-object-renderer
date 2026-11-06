//! Local light experiments: PointLight, SpotLight and SurfaceLight on white SmoothPlastic floors and
//! walls in the dark (notes/render.md "Local lights"; tools/analysis/light_fit.py).

// fixture helpers take every property of what they place; case tables are tuples
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use glam::{Affine3A, Quat, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, Enum};

use super::{Experiment, Probe, Run, dark};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};

/// A light's properties.
#[derive(Clone, Copy)]
struct Light {
	class: &'static str,
	brightness: f32,
	range: f32,
	color: [f32; 3],
	angle: f32,
	/// `Enum.NormalId` (Right 0, Top 1, Back 2, Left 3, Bottom 4, Front 5)
	face: u32,
	shadows: bool,
}

impl Light {
	fn point(brightness: f32, range: f32) -> Self {
		Light { class: "PointLight", brightness, range, color: [1.0; 3], angle: 90.0, face: 5, shadows: false }
	}
}

/// An invisible carrier part (fully transparent, no shadow) of `size` holding `light`.
fn carrier(model: &mut ModelBuilder, name: &str, cframe: Affine3A, size: Vec3, light: Light) {
	let part = model.part(&PartSpec { name: name.into(), cframe, size, transparency: 1.0, cast_shadow: false, ..Default::default() });
	model.child(
		part,
		InstanceBuilder::new(light.class)
			.with_name(light.class)
			.with_property("Brightness", light.brightness)
			.with_property("Range", light.range)
			.with_property("Color", Color3::new(light.color[0], light.color[1], light.color[2]))
			.with_property("Angle", light.angle)
			.with_property("Face", Enum::from_u32(light.face))
			.with_property("Shadows", light.shadows)
			.with_property("Enabled", true),
	);
}

fn light_inputs(light: &Light) -> serde_json::Value {
	serde_json::json!({"class": light.class, "brightness": light.brightness, "range": light.range, "color": light.color, "angle": light.angle, "face": light.face, "shadows": light.shadows})
}

/// A white SmoothPlastic slab whose top face is at y = 0, spanning `x0..x1` and `z0..z1`.
fn floor(model: &mut ModelBuilder, name: &str, origin: Vec3, x0: f32, x1: f32, z0: f32, z1: f32, gray: u8) {
	let center = origin + Vec3::new((x0 + x1) * 0.5, -0.5, (z0 + z1) * 0.5);
	model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation(center), size: Vec3::new(x1 - x0, 1.0, z1 - z0), color: [gray; 3], material: 272, ..Default::default() });
}

/// Straight down onto the floor around `center` (+Z up in the image, +X to the left).
fn down(center: Vec3, height: f32) -> CameraSpec {
	CameraSpec::LookAt { eye: (center + Vec3::new(0.0, height, -0.05)).into(), target: center.into(), fov: 40.0 }
}

/// PointLight falloff: a light at height h above a white floor strip that runs 64 studs from under
/// it, one configuration per group (Range 8-60, heights 2-12, Brightness 1-4, a coloured light, a
/// shadow-casting one with nothing in the way), seen straight down at EV 0 and -3.
pub fn light_falloff() -> Experiment {
	let mut model = ModelBuilder::new("LightFalloff");
	let mut probes = Vec::new();
	let configs: [(&str, f32, Light); 8] = [
		("r60", 2.0, Light::point(1.0, 60.0)),
		("r30", 2.0, Light::point(1.0, 30.0)),
		("r15", 2.0, Light::point(1.0, 15.0)),
		("r8", 2.0, Light::point(1.0, 8.0)),
		("h6", 6.0, Light::point(1.0, 30.0)),
		("b4", 2.0, Light::point(4.0, 30.0)),
		("orange", 2.0, Light { color: [1.0, 0.5, 0.0], ..Light::point(1.0, 30.0) }),
		("h12", 12.0, Light { shadows: true, ..Light::point(2.0, 60.0) }),
	];
	let mut runs = Vec::new();
	for (index, (label, height, light)) in configs.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -6.0, 64.0, -14.0, 14.0, 255);
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::Y * *height), Vec3::splat(0.2), *light);
		let mut inputs = light_inputs(light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["height"] = serde_json::json!(height);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		for exposure in [0.0f32, -3.0] {
			runs.push(Run { label: format!("{label}-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: down(origin + Vec3::new(29.0, 0.0, 0.0), 66.0) });
		}
	}
	Experiment { name: "light-falloff".into(), model, probes, runs }
}

/// PointLight fade near Range without grazing light: the light at 0.6 · Range above a white floor,
/// so n·l ≥ 0.6 where it fades (Range 4-60, Range 60 with Shadows, Range 120 for the clamp); a
/// coloured light (0.2, 0.5, 0.8) for the colour decode; two overlapping lights alone and over
/// Ambient 0.5 for additivity. Seen straight down from above the light at EV 0 and -2.
pub fn light_range() -> Experiment {
	let mut model = ModelBuilder::new("LightRange");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, light height, floor half-size, camera height, lights at x offsets)
	let configs: [(&str, f32, f32, f32, Light, &[f32]); 10] = [
		("f4", 2.4, 4.0, 10.0, Light::point(1.0, 4.0), &[0.0]),
		("f8", 4.8, 8.0, 19.0, Light::point(1.0, 8.0), &[0.0]),
		("f16", 9.6, 15.0, 38.0, Light::point(1.0, 16.0), &[0.0]),
		("f30", 18.0, 27.0, 71.0, Light::point(1.0, 30.0), &[0.0]),
		("f45", 27.0, 40.0, 106.0, Light::point(1.0, 45.0), &[0.0]),
		("f60", 36.0, 54.0, 141.0, Light::point(1.0, 60.0), &[0.0]),
		("f60s", 36.0, 54.0, 141.0, Light { shadows: true, ..Light::point(1.0, 60.0) }, &[0.0]),
		("f120", 36.0, 116.0, 300.0, Light::point(1.0, 120.0), &[0.0]),
		("color", 9.6, 15.0, 38.0, Light { color: [0.2, 0.5, 0.8], ..Light::point(1.0, 16.0) }, &[0.0]),
		("pair", 6.0, 20.0, 50.0, Light::point(1.0, 16.0), &[-5.0, 5.0]),
	];
	for (index, (label, height, half, camera, light, offsets)) in configs.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 400.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -half, *half, -half, *half, 255);
		for (k, offset) in offsets.iter().enumerate() {
			carrier(&mut model, &format!("Light-{label}-{k}"), Affine3A::from_translation(origin + Vec3::new(*offset, *height, 0.0)), Vec3::splat(0.2), *light);
		}
		let mut inputs = light_inputs(light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["height"] = serde_json::json!(height);
		inputs["offsets"] = serde_json::json!(offsets);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		for exposure in [0.0f32, -2.0] {
			runs.push(Run { label: format!("{label}-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: down(origin, *camera) });
		}
		if *label == "pair" {
			let ambient = Environment { ambient: Vec3::splat(0.5), ..dark() };
			runs.push(Run { label: "pair-ambient".into(), environment: ambient, camera: down(origin, *camera) });
		}
	}
	Experiment { name: "light-range".into(), model, probes, runs }
}

/// SpotLight cones on a white wall 8 studs in front of the light (Angle 30-180, Range 40,
/// Brightness 2), one turned with Face = Right towards a second wall, seen head-on from behind the
/// light at EV 0 and -2.
pub fn light_spot() -> Experiment {
	let mut model = ModelBuilder::new("LightSpot");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let angles = [30.0f32, 60.0, 90.0, 120.0, 150.0, 180.0];
	for (index, angle) in angles.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		// the wall: its front face at z = 0, the light 8 studs before it, facing +Z (Face Back)
		model.part(&PartSpec { name: format!("Wall{angle}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(90.0, 90.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
		let light = Light { class: "SpotLight", angle: *angle, face: 2, ..Light::point(2.0, 40.0) };
		carrier(&mut model, &format!("Spot{angle}"), Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, -8.0)), Vec3::splat(0.2), light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["distance"] = serde_json::json!(8.0);
		probes.push(Probe { label: format!("spot{angle}"), position: origin.into(), inputs });
		for exposure in [0.0f32, -2.0] {
			runs.push(Run {
				label: format!("a{angle}-ev{exposure}"),
				environment: Environment { exposure_compensation: exposure, ..dark() },
				camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 0.0, -60.0)).into(), target: origin.into(), fov: 40.0 },
			});
		}
	}
	// Face = Right: the carrier turned 90° about Y so its +X (Right) points to +Z, at the wall
	let origin = Vec3::new(angles.len() as f32 * 150.0, 0.0, 0.0);
	model.part(&PartSpec { name: "WallFace".into(), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(90.0, 90.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
	let light = Light { class: "SpotLight", angle: 60.0, face: 0, ..Light::point(2.0, 40.0) };
	carrier(&mut model, "SpotRight", Affine3A::from_rotation_translation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2), origin + Vec3::new(0.0, 0.0, -8.0)), Vec3::splat(0.2), light);
	let mut inputs = light_inputs(&light);
	inputs["origin"] = serde_json::json!(origin.to_array());
	probes.push(Probe { label: "spot-right".into(), position: origin.into(), inputs });
	runs.push(Run { label: "face-right".into(), environment: dark(), camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 0.0, -60.0)).into(), target: origin.into(), fov: 40.0 } });
	Experiment { name: "light-spot".into(), model, probes, runs }
}

/// SurfaceLight: lights on the front face of invisible carriers of three sizes (1, 6 and 12 studs
/// square) 4 and 12 studs before a white wall, Angle 90 (and 30 and 180 on the 6-stud face),
/// Range 30, Brightness 2; seen head-on at EV 0 and -2. Area light or a point at the face?
pub fn light_surface() -> Experiment {
	let mut model = ModelBuilder::new("LightSurface");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let cases: [(&str, f32, f32, f32); 8] = [
		("s1-d4", 1.0, 4.0, 90.0),
		("s6-d4", 6.0, 4.0, 90.0),
		("s12-d4", 12.0, 4.0, 90.0),
		("s1-d12", 1.0, 12.0, 90.0),
		("s6-d12", 6.0, 12.0, 90.0),
		("s12-d12", 12.0, 12.0, 90.0),
		("s6-d4-a30", 6.0, 4.0, 30.0),
		("s6-d4-a180", 6.0, 4.0, 180.0),
	];
	for (index, (label, side, distance, angle)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(90.0, 90.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
		// the carrier's Back face (+Z) emits towards the wall; 1 stud deep, its face `distance` away
		let light = Light { class: "SurfaceLight", angle: *angle, face: 2, ..Light::point(2.0, 30.0) };
		carrier(&mut model, &format!("Surface-{label}"), Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, -distance - 0.5)), Vec3::new(*side, *side, 1.0), light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["distance"] = serde_json::json!(distance);
		inputs["side"] = serde_json::json!(side);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		for exposure in [0.0f32, -2.0] {
			runs.push(Run {
				label: format!("{label}-ev{exposure}"),
				environment: Environment { exposure_compensation: exposure, ..dark() },
				camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 0.0, -60.0)).into(), target: origin.into(), fov: 40.0 },
			});
		}
	}
	Experiment { name: "light-surface".into(), model, probes, runs }
}

/// Narrow and zero Angles (a cutscene rims its character with SurfaceLights of Angle 0):
/// SurfaceLights on a 6-stud face 4 studs before a white wall and SpotLights 8 studs before one at
/// Angle 0, 1, 5 and 15, and a 6-stud SurfaceLight of Angle 0 and 5 at right angles to a floor (its
/// lower edge 3 studs up); Range 30, Brightness 2, seen at EV 0 and -2.
pub fn light_angle_small() -> Experiment {
	let mut model = ModelBuilder::new("LightAngleSmall");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let mut index = 0;
	for class in ["SurfaceLight", "SpotLight"] {
		for angle in [0.0f32, 1.0, 5.0, 15.0] {
			let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
			index += 1;
			let label = format!("{}-a{angle}", if class == "SurfaceLight" { "surface" } else { "spot" });
			model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(90.0, 90.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
			let light = Light { class, angle, face: 2, ..Light::point(2.0, 30.0) };
			let (distance, size) = if class == "SurfaceLight" { (4.0, Vec3::new(6.0, 6.0, 1.0)) } else { (8.0, Vec3::splat(0.2)) };
			carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, -distance - size.z * 0.5)), size, light);
			let mut inputs = light_inputs(&light);
			inputs["origin"] = serde_json::json!(origin.to_array());
			inputs["distance"] = serde_json::json!(distance);
			probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
			for exposure in [0.0f32, -2.0] {
				runs.push(Run {
					label: format!("{label}-ev{exposure}"),
					environment: Environment { exposure_compensation: exposure, ..dark() },
					camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 0.0, -60.0)).into(), target: origin.into(), fov: 40.0 },
				});
			}
		}
	}
	for angle in [0.0f32, 5.0] {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		index += 1;
		let label = format!("floor-a{angle}");
		floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -6.0, 40.0, 255);
		let light = Light { class: "SurfaceLight", angle, face: 2, ..Light::point(2.0, 30.0) };
		let center = Vec3::new(0.0, 6.0, -0.5);
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + center), Vec3::new(6.0, 6.0, 1.0), light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["center"] = serde_json::json!((origin + center).to_array());
		probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
		for exposure in [0.0f32, -2.0] {
			runs.push(Run {
				label: format!("{label}-ev{exposure}"),
				environment: Environment { exposure_compensation: exposure, ..dark() },
				camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 45.0, -10.0)).into(), target: (origin + Vec3::new(0.0, 0.0, 16.0)).into(), fov: 50.0 },
			});
		}
	}
	Experiment { name: "light-angle-small".into(), model, probes, runs }
}

/// Surfaces turned away from a SurfaceLight's face (a cutscene rims its character from
/// behind with an 8.5 x 8 face 3 studs back): white SmoothPlastic balls of 4 studs 3 and 8 studs
/// before an 8-stud face of Angle 0 and 90 (Range 30, Brightness 2, no shadows), seen from the side
/// and from behind the ball (its far side, facing away from the face), at EV -2.
pub fn light_surface_back() -> Experiment {
	let mut model = ModelBuilder::new("LightSurfaceBack");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let mut index = 0;
	for angle in [0.0f32, 90.0] {
		for distance in [3.0f32, 8.0] {
			let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
			index += 1;
			let label = format!("a{angle}-d{distance}");
			// the carrier's Back face (+Z) at z = 0, the ball's centre `distance` along +Z
			let light = Light { class: "SurfaceLight", angle, face: 2, ..Light::point(2.0, 30.0) };
			carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, -0.5)), Vec3::new(8.0, 8.0, 1.0), light);
			let ball = origin + Vec3::new(0.0, 0.0, distance);
			model.part(&PartSpec { name: format!("Ball-{label}"), cframe: Affine3A::from_translation(ball), size: Vec3::splat(4.0), color: [255; 3], material: 272, shape: Some(0), ..Default::default() });
			let mut inputs = light_inputs(&light);
			inputs["origin"] = serde_json::json!(origin.to_array());
			inputs["ball"] = serde_json::json!(ball.to_array());
			probes.push(Probe { label: label.clone(), position: ball.into(), inputs });
			for (view, eye) in [("side", Vec3::new(30.0, 0.0, 0.0)), ("behind", Vec3::new(0.0, 4.0, 30.0))] {
				runs.push(Run {
					label: format!("{label}-{view}"),
					environment: Environment { exposure_compensation: -2.0, ..dark() },
					camera: CameraSpec::LookAt { eye: (ball + eye).into(), target: ball.into(), fov: 20.0 },
				});
			}
		}
	}
	// boards close before the face, seen on their side turned away from it, Shadows off and on
	for shadows in [false, true] {
		for distance in [1.5f32, 3.0] {
			let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
			index += 1;
			let label = format!("board-d{distance}{}", if shadows { "-shadows" } else { "" });
			let light = Light { class: "SurfaceLight", angle: 0.0, face: 2, shadows, ..Light::point(2.0, 30.0) };
			carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, -0.5)), Vec3::new(8.0, 8.0, 1.0), light);
			let board = origin + Vec3::new(0.0, 0.0, distance);
			model.part(&PartSpec { name: format!("Board-{label}"), cframe: Affine3A::from_translation(board), size: Vec3::new(4.0, 4.0, 0.2), color: [255; 3], material: 272, ..Default::default() });
			let mut inputs = light_inputs(&light);
			inputs["origin"] = serde_json::json!(origin.to_array());
			inputs["board"] = serde_json::json!(board.to_array());
			probes.push(Probe { label: label.clone(), position: board.into(), inputs });
			runs.push(Run {
				label: format!("{label}-behind"),
				environment: Environment { exposure_compensation: -2.0, ..dark() },
				camera: CameraSpec::LookAt { eye: (board + Vec3::new(3.0, 2.0, 30.0)).into(), target: board.into(), fov: 20.0 },
			});
		}
	}
	Experiment { name: "light-surface-back".into(), model, probes, runs }
}

/// SurfaceLight onto a floor at right angles to its face (does the receiver's n·l apply?): faces of
/// 1, 6 and 12 studs facing +Z with their lower edge 3 studs above a white floor, Angle 90 (and 180
/// on the 6-stud face), one facing down onto the floor for reference; Range 30, Brightness 2.
pub fn light_surface_floor() -> Experiment {
	let mut model = ModelBuilder::new("LightSurfaceFloor");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, side, angle, face)
	let cases: [(&str, f32, f32, u32); 5] = [("f6", 6.0, 90.0, 2), ("f1", 1.0, 90.0, 2), ("f12", 12.0, 90.0, 2), ("f6-a180", 6.0, 180.0, 2), ("down6", 6.0, 90.0, 4)];
	for (index, (label, side, angle, face)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -6.0, 40.0, 255);
		// the emitting face at z = 0 (Back, +Z) with its lower edge at y = 3; facing down: its face at y = 6
		let (center, size) = if *face == 2 { (Vec3::new(0.0, 3.0 + side / 2.0, -0.5), Vec3::new(*side, *side, 1.0)) } else { (Vec3::new(0.0, 6.5, 8.0), Vec3::new(*side, 1.0, *side)) };
		let light = Light { class: "SurfaceLight", angle: *angle, face: *face, ..Light::point(2.0, 30.0) };
		carrier(&mut model, &format!("Surface-{label}"), Affine3A::from_translation(origin + center), size, light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["center"] = serde_json::json!((origin + center).to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		for exposure in [0.0f32, -2.0] {
			runs.push(Run {
				label: format!("{label}-ev{exposure}"),
				environment: Environment { exposure_compensation: exposure, ..dark() },
				camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 45.0, -10.0)).into(), target: (origin + Vec3::new(0.0, 0.0, 16.0)).into(), fov: 50.0 },
			});
		}
	}
	Experiment { name: "light-surface-floor".into(), model, probes, runs }
}

/// Shadows = true: a PointLight 8 studs above a white floor with an occluding slab beside it, the
/// light inside its own opaque parent ball, inside a separate ball (casting shadows or not), next
/// to a small cube and a half-transparent one (penumbra, transparency); a SpotLight and a
/// SurfaceLight facing down past a slab. Seen straight down.
pub fn light_shadows() -> Experiment {
	let mut model = ModelBuilder::new("LightShadows");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let light = Light { shadows: true, ..Light::point(2.0, 16.0) };
	let groups = ["own-ball", "other-ball", "other-ball-noshadow", "cubes", "spot", "surface"];
	for (index, label) in groups.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -16.0, 16.0, -16.0, 16.0, 255);
		let at = origin + Vec3::new(0.0, 8.0, 0.0);
		// a slab 3 studs under the light, 3 studs off its axis
		model.part(&PartSpec { name: format!("Slab-{label}"), cframe: Affine3A::from_translation(at + Vec3::new(4.5, -3.0, 0.0)), size: Vec3::new(3.0, 0.4, 3.0), color: [255; 3], material: 272, ..Default::default() });
		match *label {
			"own-ball" => {
				let part = model.part(&PartSpec { name: "Bulb".into(), cframe: Affine3A::from_translation(at), size: Vec3::splat(1.0), color: [255; 3], material: 272, shape: Some(0), ..Default::default() });
				model.child(
					part,
					InstanceBuilder::new("PointLight").with_name("PointLight").with_property("Brightness", light.brightness).with_property("Range", light.range).with_property("Color", Color3::new(1.0, 1.0, 1.0)).with_property("Shadows", true).with_property("Enabled", true),
				);
			}
			"other-ball" | "other-ball-noshadow" => {
				model.part(&PartSpec { name: format!("Ball-{label}"), cframe: Affine3A::from_translation(at), size: Vec3::splat(1.5), color: [255; 3], material: 272, shape: Some(0), cast_shadow: *label == "other-ball", ..Default::default() });
				carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(at), Vec3::splat(0.2), light);
			}
			"cubes" => {
				carrier(&mut model, "Light-cubes", Affine3A::from_translation(at), Vec3::splat(0.2), light);
				model.part(&PartSpec { name: "Cube".into(), cframe: Affine3A::from_translation(at + Vec3::new(-3.0, -4.0, 2.0)), size: Vec3::splat(1.0), color: [255; 3], material: 272, ..Default::default() });
				model.part(&PartSpec { name: "Glass".into(), cframe: Affine3A::from_translation(at + Vec3::new(-3.0, -4.0, -3.0)), size: Vec3::new(2.0, 0.4, 2.0), color: [255; 3], material: 272, transparency: 0.5, ..Default::default() });
			}
			"spot" => {
				let spot = Light { class: "SpotLight", angle: 120.0, face: 4, ..light };
				carrier(&mut model, "Light-spot", Affine3A::from_translation(at), Vec3::splat(0.2), spot);
			}
			_ => {
				// a 4-stud face looking down (Bottom) whose centre is the light position
				let surface = Light { class: "SurfaceLight", angle: 120.0, face: 4, ..light };
				carrier(&mut model, "Light-surface", Affine3A::from_translation(at + Vec3::Y * 0.5), Vec3::new(4.0, 1.0, 4.0), surface);
			}
		}
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": at.to_array()}) });
		runs.push(Run { label: (*label).into(), environment: dark(), camera: down(origin, 40.0) });
	}
	Experiment { name: "light-shadows".into(), model, probes, runs }
}

/// What local lights reach: next to one PointLight (Range 16, Brightness 2), white SmoothPlastic,
/// gray Neon, ForceField, half-transparent white SmoothPlastic over black, a black SmoothPlastic
/// ball (the specular alone) and an occluder over a floor with Shadows on and off; a twin group
/// without the light for reference.
pub fn light_materials() -> Experiment {
	let mut model = ModelBuilder::new("LightMaterials");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (group, lit, shadows) in [(0, true, false), (1, false, false), (2, true, true)] {
		let origin = Vec3::new(group as f32 * 150.0, 0.0, 0.0);
		// black backdrop wall and floor
		model.part(&PartSpec { name: format!("Back{group}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 6.0, 8.5)), size: Vec3::new(60.0, 40.0, 1.0), color: [0, 0, 0], material: 288, ..Default::default() });
		floor(&mut model, &format!("Floor{group}"), origin, -20.0, 20.0, -10.0, 8.0, 255);
		// (name, centre, colour, material, transparency, shape)
		let items = [
			("plastic", Vec3::new(9.0, 2.0, 0.0), [255; 3], 272, 0.0, None),
			("neon", Vec3::new(4.5, 2.0, 0.0), [128; 3], 288, 0.0, None),
			("forcefield", Vec3::new(0.0, 2.0, 0.0), [255; 3], 1584, 0.0, None),
			("glassy", Vec3::new(-4.5, 2.0, 0.0), [255; 3], 272, 0.5, None),
			("black-ball", Vec3::new(-9.0, 2.0, 0.0), [0; 3], 272, 0.0, Some(0)),
		];
		for (name, center, color, material, transparency, shape) in items {
			model.part(&PartSpec {
				name: format!("{name}{group}"),
				cframe: Affine3A::from_translation(origin + center),
				size: Vec3::new(3.0, 3.0, 3.0),
				color,
				material,
				transparency,
				shape,
				cast_shadow: false,
				..Default::default()
			});
			probes.push(Probe { label: format!("{name}{group}"), position: (origin + center - Vec3::Z * 1.5).into(), inputs: serde_json::json!({"group": group, "lit": lit, "item": name}) });
		}
		// an occluder between the light and the floor
		model.part(&PartSpec { name: format!("Occluder{group}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 5.0, -6.0)), size: Vec3::new(3.0, 0.4, 3.0), color: [255; 3], material: 272, ..Default::default() });
		if lit {
			let light = Light { shadows, ..Light::point(2.0, 16.0) };
			carrier(&mut model, &format!("Light{group}"), Affine3A::from_translation(origin + Vec3::new(0.0, 8.0, -6.0)), Vec3::splat(0.2), light);
		}
		for exposure in [0.0f32, -2.0] {
			runs.push(Run {
				label: format!("g{group}-ev{exposure}"),
				environment: Environment { exposure_compensation: exposure, ..dark() },
				camera: CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 12.0, -36.0)).into(), target: (origin + Vec3::new(0.0, 2.0, 0.0)).into(), fov: 40.0 },
			});
		}
	}
	Experiment { name: "light-materials".into(), model, probes, runs }
}

/// Straight down onto `center` from `height` studs above it.
fn above(center: Vec3, height: f32, fov: f32) -> CameraSpec {
	CameraSpec::LookAt { eye: (center + Vec3::new(0.0, height, -0.05)).into(), target: center.into(), fov }
}

/// SurfaceLights whose faces are not square, and where a SurfaceLight's shadow is cast from: a 12 x
/// 2 face facing down 6 studs over a white floor at Angle 90 and 180; faces 2 wide and 12 tall, and
/// 12 wide and 2 tall, facing +Z with their lower edge 3 studs over a floor at right angles; 4-stud
/// faces facing down 8 studs over a floor at Angle 60, 90, 150 and 180, and faces of 8 x 2 and 2 x 8
/// at Angle 90, with Shadows and a 3 x 0.4 x 3 slab 2.2 studs under them beside their axis. Range
/// 30, Brightness 2; seen from above at EV -2 (and 0 for the unshadowed ones).
pub fn light_surface_rect() -> Experiment {
	let mut model = ModelBuilder::new("LightSurfaceRect");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, carrier size, carrier centre, face, Angle, Shadows)
	let cases: [(&str, Vec3, Vec3, u32, f32, bool); 10] = [
		("wide-a90", Vec3::new(12.0, 1.0, 2.0), Vec3::new(0.0, 6.5, 0.0), 4, 90.0, false),
		("wide-a180", Vec3::new(12.0, 1.0, 2.0), Vec3::new(0.0, 6.5, 0.0), 4, 180.0, false),
		("tall-floor", Vec3::new(2.0, 12.0, 1.0), Vec3::new(0.0, 9.0, -0.5), 2, 90.0, false),
		("flat-floor", Vec3::new(12.0, 2.0, 1.0), Vec3::new(0.0, 4.0, -0.5), 2, 90.0, false),
		("apex-a60", Vec3::new(4.0, 1.0, 4.0), Vec3::new(0.0, 8.5, 0.0), 4, 60.0, true),
		("apex-a90", Vec3::new(4.0, 1.0, 4.0), Vec3::new(0.0, 8.5, 0.0), 4, 90.0, true),
		("apex-a150", Vec3::new(4.0, 1.0, 4.0), Vec3::new(0.0, 8.5, 0.0), 4, 150.0, true),
		("apex-a180", Vec3::new(4.0, 1.0, 4.0), Vec3::new(0.0, 8.5, 0.0), 4, 180.0, true),
		("apex-8x2", Vec3::new(8.0, 1.0, 2.0), Vec3::new(0.0, 8.5, 0.0), 4, 90.0, true),
		("apex-2x8", Vec3::new(2.0, 1.0, 8.0), Vec3::new(0.0, 8.5, 0.0), 4, 90.0, true),
	];
	for (index, (label, size, center, face, angle, shadows)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		let floor_far = *face == 2;
		if floor_far {
			floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -6.0, 40.0, 255);
		} else {
			floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -30.0, 30.0, 255);
		}
		if *shadows {
			model.part(&PartSpec { name: format!("Slab-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(3.0, 5.6, 0.0)), size: Vec3::new(3.0, 0.4, 3.0), color: [255; 3], material: 272, ..Default::default() });
		}
		let light = Light { class: "SurfaceLight", angle: *angle, face: *face, shadows: *shadows, ..Light::point(2.0, 30.0) };
		carrier(&mut model, &format!("Surface-{label}"), Affine3A::from_translation(origin + *center), *size, light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["center"] = serde_json::json!((origin + *center).to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		let camera = if floor_far {
			CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 45.0, -10.0)).into(), target: (origin + Vec3::new(0.0, 0.0, 16.0)).into(), fov: 50.0 }
		} else if *shadows {
			above(origin, 30.0, 40.0)
		} else {
			above(origin, 45.0, 40.0)
		};
		let exposures: &[f32] = if *shadows { &[-2.0] } else { &[0.0, -2.0] };
		for exposure in exposures {
			runs.push(Run { label: format!("{label}-ev{exposure}"), environment: Environment { exposure_compensation: *exposure, ..dark() }, camera: camera.clone() });
		}
	}
	Experiment { name: "light-surface-rect".into(), model, probes, runs }
}

/// Brightness at the high end and saturated colours, as attack lights often use them (Brightness up
/// to 100, most of them saturated): PointLights 4 studs over a dark gray floor (gray 40) at
/// Brightness 1, 4, 10, 25, 50 and 100, at 25 in red, orange (255, 85, 0), blue (0, 85, 255) and
/// purple (113, 0, 170), and one at 4 over a white floor; Range 24; straight down at EV 0, -3 and -5.
pub fn light_brightness() -> Experiment {
	let mut model = ModelBuilder::new("LightBrightness");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let color = |c: [u8; 3]| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0];
	// (label, Brightness, colour, floor gray)
	let cases: [(&str, f32, [f32; 3], u8); 11] = [
		("b1", 1.0, [1.0; 3], 40),
		("b4", 4.0, [1.0; 3], 40),
		("b10", 10.0, [1.0; 3], 40),
		("b25", 25.0, [1.0; 3], 40),
		("b50", 50.0, [1.0; 3], 40),
		("b100", 100.0, [1.0; 3], 40),
		("red", 25.0, color([255, 0, 0]), 40),
		("orange", 25.0, color([255, 85, 0]), 40),
		("blue", 25.0, color([0, 85, 255]), 40),
		("purple", 25.0, color([113, 0, 170]), 40),
		("white-b4", 4.0, [1.0; 3], 255),
	];
	for (index, (label, brightness, light_color, gray)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -26.0, 26.0, -26.0, 26.0, *gray);
		let light = Light { color: *light_color, ..Light::point(*brightness, 24.0) };
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::Y * 4.0), Vec3::splat(0.2), light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["height"] = serde_json::json!(4.0);
		inputs["gray"] = serde_json::json!(gray);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		for exposure in [0.0f32, -3.0, -5.0] {
			runs.push(Run { label: format!("{label}-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: above(origin, 60.0, 40.0) });
		}
	}
	Experiment { name: "light-brightness".into(), model, probes, runs }
}


/// Local light on particles and beams: rows of 16 small white square particles (Size 0.8,
/// LightInfluence 1, a stud apart) running away from a PointLight (Range 16, Brightness 2) in their
/// plane, 4 studs in front of it (towards the camera) and 4 behind it; the front row again at
/// LightInfluence 0.5 and 0; one particle 10 studs across with the light 1 stud in front of its left
/// edge; a beam with LightInfluence 1 passing 2 studs over the light of the first row. The lights sit
/// at the centre of a 4-stud voxel. In the dark, head-on, at EV 0 and -2.
pub fn light_particles() -> Experiment {
	use super::particles::{add_emitter, constant, freeze, register};
	let name = "light-particles";
	let mut model = ModelBuilder::new("LightParticles");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, light offset in z from the particles' plane, LightInfluence, one big particle)
	let cases: [(&str, f32, f32, bool); 6] = [("plane", 0.0, 1.0, false), ("front", -4.0, 1.0, false), ("behind", 4.0, 1.0, false), ("half", -4.0, 0.5, false), ("unlit", -4.0, 0.0, false), ("big", -1.0, 1.0, true)];
	for (index, (label, dz, influence, big)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		// a black wall far behind, beyond the light's reach
		model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(10.0, 0.0, 60.0)), size: Vec3::new(200.0, 120.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		let light_at = origin + Vec3::new(2.0, 2.0, 2.0 + dz);
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(light_at), Vec3::splat(0.2), Light::point(2.0, 16.0));
		let lit: (&str, rbx_dom_weak::types::Variant) = ("LightInfluence", (*influence).into());
		if *big {
			add_emitter(&mut model, &format!("{label}-p"), Affine3A::from_translation(origin + Vec3::new(7.0, 2.0, 2.0)), &[lit.clone(), ("Size", constant(10.0))]);
		} else {
			for step in 0..16 {
				add_emitter(&mut model, &format!("{label}-p{step}"), Affine3A::from_translation(origin + Vec3::new(3.0 + step as f32, 2.0, 2.0)), &[lit.clone(), ("Size", constant(0.8))]);
			}
		}
		if *label == "plane" {
			let beam: super::BeamProperties = vec![("LightInfluence", 1.0f32.into()), ("FaceCamera", true.into())];
			super::add_beam(&mut model, "Beam-plane", origin + Vec3::new(10.0, 4.0, 2.0), Affine3A::from_translation(Vec3::new(-16.0, 0.0, 0.0)), Affine3A::from_translation(Vec3::new(16.0, 0.0, 0.0)), &beam);
		}
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": light_at.to_array(), "influence": influence, "big": big}) });
		let camera = CameraSpec::LookAt { eye: (origin + Vec3::new(10.0, 2.0, -38.0)).into(), target: (origin + Vec3::new(10.0, 2.0, 2.0)).into(), fov: 30.0 };
		for exposure in [0.0f32, -2.0] {
			let run = format!("{label}-ev{exposure}");
			register(name, &run, freeze(0.0, &[]));
			runs.push(Run { label: run, environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera.clone() });
		}
	}
	Experiment { name: name.into(), model, probes, runs }
}

/// Which parts shadow local lights: a PointLight (Shadows, Range 16, Brightness 2) 8 studs over a
/// white floor with a 3 x 0.4 x 3 occluder 3 studs under it and 3 off its axis: SmoothPlastic at
/// Transparency 0.25, 0.5, 0.75, 0.9 and 0.98, Glass at 0, 0.5 and 0.9, ForceField, Neon, and a block
/// with CastShadow off; seen straight down.
pub fn light_transparency() -> Experiment {
	let mut model = ModelBuilder::new("LightTransparency");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, material, Transparency, CastShadow)
	let cases: [(&str, u32, f32, bool); 11] = [
		("t0.25", 272, 0.25, true),
		("t0.5", 272, 0.5, true),
		("t0.75", 272, 0.75, true),
		("t0.9", 272, 0.9, true),
		("t0.98", 272, 0.98, true),
		("glass0", 1568, 0.0, true),
		("glass0.5", 1568, 0.5, true),
		("glass0.9", 1568, 0.9, true),
		("forcefield", 1584, 0.0, true),
		("neon", 288, 0.0, true),
		("noshadow", 272, 0.0, false),
	];
	for (index, (label, material, transparency, cast_shadow)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -16.0, 16.0, -16.0, 16.0, 255);
		let at = origin + Vec3::new(0.0, 8.0, 0.0);
		model.part(&PartSpec {
			name: format!("Occluder-{label}"),
			cframe: Affine3A::from_translation(at + Vec3::new(4.5, -3.0, 0.0)),
			size: Vec3::new(3.0, 0.4, 3.0),
			color: [255; 3],
			material: *material,
			transparency: *transparency,
			cast_shadow: *cast_shadow,
			..Default::default()
		});
		let light = Light { shadows: true, ..Light::point(2.0, 16.0) };
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(at), Vec3::splat(0.2), light);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": at.to_array(), "material": material, "transparency": transparency, "castShadow": cast_shadow}) });
		runs.push(Run { label: (*label).into(), environment: dark(), camera: above(origin, 40.0, 40.0) });
	}
	Experiment { name: "light-transparency".into(), model, probes, runs }
}

/// Many lights at once: 8, 32 and 128 coincident PointLights 4 studs over a white floor (Range 16,
/// each of Brightness 2 / N: together one light of Brightness 2); a 16 x 16 grid of PointLights 4
/// studs apart 2 studs over a 72-stud floor (Range 6, Brightness 0.5); 8 and 24 shadowed PointLights
/// (Range 5, Brightness 1) 6 studs apart, each 4 studs over a floor with a 2 x 0.3 x 2 slab 1.5
/// studs under it and 1 off its axis. Seen straight down at EV -2.
pub fn light_many() -> Experiment {
	let mut model = ModelBuilder::new("LightMany");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, count) in [8usize, 32, 128].iter().enumerate() {
		let origin = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-stack{count}"), origin, -20.0, 20.0, -20.0, 20.0, 255);
		for k in 0..*count {
			carrier(&mut model, &format!("Light-stack{count}-{k}"), Affine3A::from_translation(origin + Vec3::Y * 4.0), Vec3::splat(0.2), Light::point(2.0 / *count as f32, 16.0));
		}
		probes.push(Probe { label: format!("stack{count}"), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "count": count}) });
		runs.push(Run { label: format!("stack{count}"), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: above(origin, 45.0, 40.0) });
	}
	// the grid
	let origin = Vec3::new(400.0, 0.0, 0.0);
	floor(&mut model, "Floor-grid", origin, -36.0, 36.0, -36.0, 36.0, 255);
	for i in 0..16 {
		for j in 0..16 {
			let at = origin + Vec3::new((i as f32 - 7.5) * 4.0, 2.0, (j as f32 - 7.5) * 4.0);
			carrier(&mut model, &format!("Light-grid-{i}-{j}"), Affine3A::from_translation(at), Vec3::splat(0.2), Light::point(0.5, 6.0));
		}
	}
	probes.push(Probe { label: "grid".into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array()}) });
	runs.push(Run { label: "grid".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: above(origin, 110.0, 40.0) });
	// rows of shadowed lights
	for (index, count) in [8usize, 24].iter().enumerate() {
		let origin = Vec3::new(600.0 + index as f32 * 250.0, 0.0, 0.0);
		let columns = (*count).min(12);
		let rows = count.div_ceil(columns);
		let width = columns as f32 * 6.0;
		floor(&mut model, &format!("Floor-shadowed{count}"), origin, -width * 0.5 - 6.0, width * 0.5 + 6.0, -12.0, 12.0, 255);
		for k in 0..*count {
			let (row, column) = (k / columns, k % columns);
			let at = origin + Vec3::new((column as f32 - (columns as f32 - 1.0) * 0.5) * 6.0, 4.0, (row as f32 - (rows as f32 - 1.0) * 0.5) * 12.0);
			carrier(&mut model, &format!("Light-shadowed{count}-{k}"), Affine3A::from_translation(at), Vec3::splat(0.2), Light { shadows: true, ..Light::point(1.0, 5.0) });
			model.part(&PartSpec { name: format!("Slab-shadowed{count}-{k}"), cframe: Affine3A::from_translation(at + Vec3::new(1.0, -1.5, 0.0)), size: Vec3::new(2.0, 0.3, 2.0), color: [255; 3], material: 272, ..Default::default() });
		}
		probes.push(Probe { label: format!("shadowed{count}"), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "count": count}) });
		runs.push(Run { label: format!("shadowed{count}"), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: above(origin, if *count > 12 { 70.0 } else { 45.0 }, 50.0) });
	}
	Experiment { name: "light-many".into(), model, probes, runs }
}

/// Lights at or inside surfaces: PointLights (Range 8, Brightness 1) 0.05, 0.25, 0.5, 1 and 2 studs
/// over a white floor, with Shadows and without; one 1 stud over the floor of an open-topped white
/// box (4 studs inside, walls 0.4 thick and 3 high) with Shadows and without, the floor around it in
/// view; seen from 50 degrees above at EV 0.
pub fn light_near() -> Experiment {
	let mut model = ModelBuilder::new("LightNear");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let mut cases: Vec<(String, f32, bool, bool)> = Vec::new();
	for height in [0.05f32, 0.25, 0.5, 1.0, 2.0] {
		for shadows in [false, true] {
			cases.push((format!("h{height}{}", if shadows { "-s" } else { "" }), height, shadows, false));
		}
	}
	cases.push(("box".into(), 1.0, false, true));
	cases.push(("box-s".into(), 1.0, true, true));
	for (index, (label, height, shadows, boxed)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -14.0, 14.0, -14.0, 14.0, 255);
		if *boxed {
			// the walls stand on the floor around a 4 x 4 interior
			for (name, center, size) in [
				("N", Vec3::new(0.0, 1.5, 2.2), Vec3::new(4.8, 3.0, 0.4)),
				("S", Vec3::new(0.0, 1.5, -2.2), Vec3::new(4.8, 3.0, 0.4)),
				("E", Vec3::new(2.2, 1.5, 0.0), Vec3::new(0.4, 3.0, 4.0)),
				("W", Vec3::new(-2.2, 1.5, 0.0), Vec3::new(0.4, 3.0, 4.0)),
			] {
				model.part(&PartSpec { name: format!("Box{name}-{label}"), cframe: Affine3A::from_translation(origin + center), size, color: [255; 3], material: 272, ..Default::default() });
			}
		}
		let light = Light { shadows: *shadows, ..Light::point(1.0, 8.0) };
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(origin + Vec3::Y * *height), Vec3::splat(0.2), light);
		probes.push(Probe { label: label.clone(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "height": height, "shadows": shadows, "box": boxed}) });
		let target = origin + Vec3::new(0.0, 0.5, 0.0);
		let eye = target + Vec3::new(0.0, 50f32.to_radians().sin(), -50f32.to_radians().cos()) * 24.0;
		runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 40.0 } });
	}
	Experiment { name: "light-near".into(), model, probes, runs }
}

/// Where a SurfaceLight's shadow is cast from at wide Angles: 4-stud faces facing down 8 studs over a
/// floor at Angle 100, 110, 130, 140 and 170, 8-stud faces at 90 and 180, an 8 x 2 face at 180 and
/// a 4-stud face on a carrier 3 studs thick at 180, each with Shadows and a 3 x 0.4 x 3 slab 2.2 studs
/// under the face beside its axis (as light-surface-rect's apex runs); seen from above at EV -2.
pub fn light_surface_apex() -> Experiment {
	let mut model = ModelBuilder::new("LightSurfaceApex");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, carrier size, Angle)
	let cases: [(&str, Vec3, f32); 9] = [
		("a100", Vec3::new(4.0, 1.0, 4.0), 100.0),
		("a110", Vec3::new(4.0, 1.0, 4.0), 110.0),
		("a130", Vec3::new(4.0, 1.0, 4.0), 130.0),
		("a140", Vec3::new(4.0, 1.0, 4.0), 140.0),
		("a170", Vec3::new(4.0, 1.0, 4.0), 170.0),
		("s8-a90", Vec3::new(8.0, 1.0, 8.0), 90.0),
		("s8-a180", Vec3::new(8.0, 1.0, 8.0), 180.0),
		("8x2-a180", Vec3::new(8.0, 1.0, 2.0), 180.0),
		("thick-a180", Vec3::new(4.0, 3.0, 4.0), 180.0),
	];
	for (index, (label, size, angle)) in cases.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -30.0, 30.0, 255);
		// the slab beside the axis, outside an 8-stud face's footprint for the 8-stud cases
		let slab_x = if size.x > 4.0 && size.z > 4.0 { 5.5 } else { 3.0 };
		model.part(&PartSpec { name: format!("Slab-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(slab_x, 5.6, 0.0)), size: Vec3::new(3.0, 0.4, 3.0), color: [255; 3], material: 272, ..Default::default() });
		let light = Light { class: "SurfaceLight", angle: *angle, face: 4, shadows: true, ..Light::point(2.0, 30.0) };
		let center = Vec3::new(0.0, 8.0 + size.y * 0.5, 0.0);
		carrier(&mut model, &format!("Surface-{label}"), Affine3A::from_translation(origin + center), *size, light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["center"] = serde_json::json!((origin + center).to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		inputs["slab"] = serde_json::json!([slab_x, 5.6, 0.0]);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		runs.push(Run { label: format!("{label}-ev-2"), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: above(origin, 30.0, 40.0) });
	}
	Experiment { name: "light-surface-apex".into(), model, probes, runs }
}

/// Where a SurfaceLight's shadow is cast from at narrow Angles (light-surface-apex's layout): 4-stud
/// faces facing down 8 studs over a floor at Angle 0, 5, 15, 30 and 60 with Shadows, a 3 x 0.4 x 3
/// slab 2.2 studs under the face across the footprint's edge and a 1 x 0.4 x 1 one under its centre;
/// seen from above at EV -2.
pub fn light_surface_apex_small() -> Experiment {
	let mut model = ModelBuilder::new("LightSurfaceApexSmall");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, angle) in [0.0f32, 5.0, 15.0, 30.0, 60.0].into_iter().enumerate() {
		let label = format!("a{angle}");
		let origin = Vec3::new(index as f32 * 150.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -30.0, 30.0, -30.0, 30.0, 255);
		model.part(&PartSpec { name: format!("Slab-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(3.0, 5.6, 0.0)), size: Vec3::new(3.0, 0.4, 3.0), color: [255; 3], material: 272, ..Default::default() });
		model.part(&PartSpec { name: format!("Block-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 5.6, 0.0)), size: Vec3::new(1.0, 0.4, 1.0), color: [255; 3], material: 272, ..Default::default() });
		let light = Light { class: "SurfaceLight", angle, face: 4, shadows: true, ..Light::point(2.0, 30.0) };
		let size = Vec3::new(4.0, 1.0, 4.0);
		let center = Vec3::new(0.0, 8.5, 0.0);
		carrier(&mut model, &format!("Surface-{label}"), Affine3A::from_translation(origin + center), size, light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["center"] = serde_json::json!((origin + center).to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		inputs["slab"] = serde_json::json!([3.0, 5.6, 0.0]);
		probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
		runs.push(Run { label: format!("{label}-ev-2"), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: above(origin, 30.0, 40.0) });
	}
	Experiment { name: "light-surface-apex-small".into(), model, probes, runs }
}

/// The light local lights give particles and beams, mapped: lattices of small white square particles
/// (Size 0.4, LightInfluence 1) a stud apart in the plane z = 2 around a PointLight (Brightness 2,
/// Range 16): the light in their plane at a voxel centre (seen from the front and from behind), 4
/// studs in front of the plane (both ways), at a voxel corner 2 in front; rows of particles facing
/// the camera and facing away (VelocityPerpendicular) with the light in front; lattices at Range 8
/// and, 2 studs apart, at Range 32; and beams with LightInfluence 1 along rows 0, 1, 2 and 4 studs
/// over the light (FaceCamera) and one lying flat. In the dark at EV -2.
pub fn light_particle_field() -> Experiment {
	use super::particles::{add_emitter, constant, fixed, freeze, register};
	let name = "light-particle-field";
	let mut model = ModelBuilder::new("LightParticleField");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, light offset from the origin, Range, lattice x and y spans, spacing, cameras)
	let lattices: [(&str, Vec3, f32, (i32, i32), (i32, i32), f32, &[&str]); 6] = [
		("plane", Vec3::new(2.0, 2.0, 2.0), 16.0, (-10, 18), (-6, 10), 1.0, &["front", "back"]),
		("front4", Vec3::new(2.0, 2.0, -2.0), 16.0, (-10, 18), (-6, 10), 1.0, &["front", "back"]),
		("corner", Vec3::new(4.0, 4.0, 0.0), 16.0, (-10, 18), (-6, 10), 1.0, &["front"]),
		("range8", Vec3::new(2.0, 2.0, 2.0), 8.0, (-6, 12), (-4, 8), 1.0, &["front"]),
		("range32", Vec3::new(2.0, 2.0, 2.0), 32.0, (-30, 34), (-6, 10), 2.0, &["front"]),
		("facing", Vec3::new(2.0, 2.0, -2.0), 16.0, (-10, 18), (-2, 6), 1.0, &["front"]),
	];
	let mut index = 0;
	for (label, light_offset, range, (x0, x1), (y0, y1), spacing, cameras) in lattices.iter() {
		let origin = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		index += 1;
		let light_at = origin + *light_offset;
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(light_at), Vec3::splat(0.2), Light::point(2.0, *range));
		// black walls far behind the lattice on both sides, beyond the light's reach
		for (side, z) in [("far", 60.0f32), ("near", -100.0)] {
			model.part(&PartSpec { name: format!("Wall-{label}-{side}"), cframe: Affine3A::from_translation(origin + Vec3::new(4.0, 0.0, z)), size: Vec3::new(300.0, 160.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		}
		let (columns, rows) = (((x1 - x0) as f32 / spacing) as i32, ((y1 - y0) as f32 / spacing) as i32);
		for row in 0..=rows {
			for column in 0..=columns {
				let at = origin + Vec3::new(*x0 as f32 + column as f32 * spacing, *y0 as f32 + row as f32 * spacing, 2.0);
				let lit: (&str, rbx_dom_weak::types::Variant) = ("LightInfluence", 1.0f32.into());
				if *label == "facing" {
					// VelocityPerpendicular: the quad faces along its velocity, towards the camera (-Z) on
					// even rows and away from it on odd ones
					let toward = row % 2 == 0;
					let turn = Affine3A::from_rotation_x(if toward { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 });
					let set = [lit, ("Size", constant(0.4)), ("Orientation", Enum::from_u32(3).into()), ("Speed", fixed(0.001))];
					add_emitter(&mut model, &format!("{label}-{row}-{column}"), Affine3A::from_translation(at) * turn, &set);
				} else {
					add_emitter(&mut model, &format!("{label}-{row}-{column}"), Affine3A::from_translation(at), &[lit, ("Size", constant(0.4))]);
				}
			}
		}
		let middle = origin + Vec3::new((*x0 + *x1) as f32 * 0.5, (*y0 + *y1) as f32 * 0.5, 2.0);
		let distance = if *range > 20.0 { 85.0 } else { 40.0 };
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": light_at.to_array(), "range": range, "x": [x0, x1], "y": [y0, y1], "spacing": spacing, "z": 2.0, "middle": middle.to_array()}) });
		for side in cameras.iter() {
			let eye = middle + Vec3::new(0.0, 0.0, if *side == "front" { -distance } else { distance });
			let run = format!("{label}-{side}");
			register(name, &run, freeze(0.0, &[]));
			runs.push(Run { label: run, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: eye.into(), target: middle.into(), fov: 40.0 } });
		}
	}
	// beams 0, 1, 2 and 4 studs over a light (at a voxel centre), facing the camera, and one lying flat
	let origin = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
	let light_at = origin + Vec3::new(2.0, 2.0, 2.0);
	carrier(&mut model, "Light-beams", Affine3A::from_translation(light_at), Vec3::splat(0.2), Light::point(2.0, 16.0));
	for (k, (height, face_camera)) in [(0.0f32, true), (1.0, true), (2.0, true), (4.0, true), (6.0, false)].iter().enumerate() {
		let mut beam: super::BeamProperties = vec![("LightInfluence", 1.0f32.into()), ("FaceCamera", (*face_camera).into()), ("Width0", 0.5f32.into()), ("Width1", 0.5f32.into())];
		if !face_camera {
			beam.push(("Width0", 2.0f32.into()));
			beam.push(("Width1", 2.0f32.into()));
		}
		// a flat beam: its width along the attachments' Y axis, turned to +Z
		let turn = if *face_camera { Affine3A::IDENTITY } else { Affine3A::from_rotation_x(std::f32::consts::FRAC_PI_2) };
		super::add_beam(&mut model, &format!("Beam-{k}"), origin + Vec3::new(4.0, 2.0 + height, 2.0), Affine3A::from_translation(Vec3::new(-14.0, 0.0, 0.0)) * turn, Affine3A::from_translation(Vec3::new(20.0, 0.0, 0.0)) * turn, &beam);
	}
	model.part(&PartSpec { name: "Wall-beams".into(), cframe: Affine3A::from_translation(origin + Vec3::new(4.0, 0.0, 60.0)), size: Vec3::new(200.0, 120.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
	probes.push(Probe { label: "beams".into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": light_at.to_array(), "range": 16.0}) });
	let middle = origin + Vec3::new(4.0, 4.0, 2.0);
	runs.push(Run { label: "beams-front".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (middle + Vec3::new(0.0, 0.0, -40.0)).into(), target: middle.into(), fov: 40.0 } });
	Experiment { name: name.into(), model, probes, runs }
}

/// The voxels local lights light particles from, read one at a time: particles (Size 1,
/// LightInfluence 1) every 4 studs in the plane 6 studs in front of a PointLight's voxel layer, so
/// that each reads about one voxel centre (particles read the grid 6 studs towards the camera,
/// light-particle-field), over rows 0 to 12 studs above the light; Range 8 to 60 at Brightness 2;
/// Brightness 1, 4 and 10 and an orange light at Range 16; a light off its voxel's centre; a light
/// with Shadows behind a wall that cuts the lattice. In the dark at EV -2.
pub fn light_particle_voxels() -> Experiment {
	use super::particles::{add_emitter, constant, freeze, register};
	let name = "light-particle-voxels";
	let mut model = ModelBuilder::new("LightParticleVoxels");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let orange = [1.0, 85.0 / 255.0, 0.0];
	// (label, Range, Brightness, colour, light offset within its voxel, a wall with Shadows)
	let cases: [(&str, f32, f32, [f32; 3], Vec3, bool); 13] = [
		("r8", 8.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r12", 12.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r16", 16.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r24", 24.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r32", 32.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r48", 48.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("r60", 60.0, 2.0, [1.0; 3], Vec3::ZERO, false),
		("b1", 16.0, 1.0, [1.0; 3], Vec3::ZERO, false),
		("b4", 16.0, 4.0, [1.0; 3], Vec3::ZERO, false),
		("b10", 16.0, 10.0, [1.0; 3], Vec3::ZERO, false),
		("orange", 16.0, 4.0, orange, Vec3::ZERO, false),
		("off", 16.0, 2.0, [1.0; 3], Vec3::new(1.3, 0.6, 0.9), false),
		("wall", 16.0, 2.0, [1.0; 3], Vec3::ZERO, true),
	];
	let mut x = 0.0;
	for (label, range, brightness, color, offset, wall) in cases.iter() {
		let span = ((range + 8.0) / 4.0).ceil() * 4.0;
		let origin = Vec3::new(x + span + 40.0, 0.0, 0.0);
		x = origin.x + span + 40.0;
		let light_at = origin + Vec3::new(2.0, 2.0, 2.0) + *offset;
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(light_at), Vec3::splat(0.2), Light { color: *color, shadows: *wall, ..Light::point(*brightness, *range) });
		if *wall {
			model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(6.2, 2.0, 2.0)), size: Vec3::new(0.4, 40.0, 20.0), color: [0; 3], material: 272, ..Default::default() });
		}
		// a black backdrop beyond the light's reach
		model.part(&PartSpec { name: format!("Back-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(2.0, 0.0, 100.0)), size: Vec3::new(span * 2.0 + 80.0, 300.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		let columns = (span / 4.0) as i32;
		for row in 0..=3 {
			for column in -columns..=columns {
				let at = origin + Vec3::new(2.0 + column as f32 * 4.0, 2.0 + row as f32 * 4.0, 8.0);
				add_emitter(&mut model, &format!("{label}-{row}-{column}"), Affine3A::from_translation(at), &[("LightInfluence", 1.0f32.into()), ("Size", constant(1.0))]);
			}
		}
		let middle = origin + Vec3::new(2.0, 8.0, 8.0);
		// wide enough for the whole row at a 40-degree field of view
		let distance = (span + 4.0) / (20f32.to_radians().tan() * 1644.0 / 1116.0);
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": light_at.to_array(), "range": range, "brightness": brightness, "color": color, "span": span, "wall": wall}) });
		register(name, label, freeze(0.0, &[]));
		runs.push(Run { label: (*label).into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (middle - Vec3::Z * distance).into(), target: middle.into(), fov: 40.0 } });
	}
	Experiment { name: name.into(), model, probes, runs }
}



/// What lights a scene at night besides its local lights: a white SmoothPlastic floor, a white ball
/// and white tiles turned 0, 30, 60 and 90 degrees towards +X and towards -X, at ClockTime 0, 3, 21
/// and 23 (the moon at its highest over the equator and lower) and at 14 for the sun, Brightness 1
/// and 3, with no Ambient, OutdoorAmbient or sky light; at Brightness 3 through the night to dawn and
/// dusk, and at midnight at latitudes -30, 23.5 and 60; and ClockTime 0 under the default Lighting.
/// Seen from 40 studs, 35 degrees up.
pub fn light_night() -> Experiment {
	let mut model = ModelBuilder::new("LightNight");
	let mut probes = Vec::new();
	floor(&mut model, "Floor", Vec3::ZERO, -30.0, 30.0, -30.0, 30.0, 255);
	model.part(&PartSpec { name: "Ball".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 3.0, 6.0)), size: Vec3::splat(6.0), color: [255; 3], material: 272, shape: Some(0), ..Default::default() });
	for (index, degrees) in [-90.0f32, -60.0, -30.0, 0.0, 30.0, 60.0, 90.0].iter().enumerate() {
		let x = (index as f32 - 3.0) * 6.0;
		// a tile whose normal leans `degrees` from up towards +X
		let normal = Vec3::new(degrees.to_radians().sin(), degrees.to_radians().cos(), 0.0);
		let center = Vec3::new(x, 3.0, -6.0);
		model.part(&PartSpec { name: format!("Tile{degrees}"), cframe: crate::scene::build::facing(center, normal), size: Vec3::new(4.0, 4.0, 0.2), color: [255; 3], material: 272, ..Default::default() });
		probes.push(Probe { label: format!("tile{degrees}"), position: (center + normal * 0.1).into(), inputs: serde_json::json!({"normal": normal.to_array()}) });
	}
	probes.push(Probe { label: "floor".into(), position: [12.0, 0.0, 12.0], inputs: serde_json::json!({"normal": [0.0, 1.0, 0.0]}) });
	let target = Vec3::new(0.0, 2.0, 0.0);
	let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, 35f32.to_radians().sin(), -35f32.to_radians().cos()) * 40.0).into(), target: target.into(), fov: 50.0 };
	let bare = Environment { ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, global_shadows: false, ..Environment::default() };
	let mut runs = Vec::new();
	for clock in [0.0f32, 3.0, 21.0, 23.0, 14.0] {
		for brightness in [1.0f32, 3.0] {
			runs.push(Run { label: format!("t{clock}-b{brightness}"), environment: Environment { clock_time: clock, brightness, ..bare.clone() }, camera: camera.clone() });
		}
	}
	// the moon's light against the time and against its height (GeographicLatitude moves it, not the
	// time), Brightness 3
	for clock in [1.0f32, 2.0, 4.0, 5.0, 5.5, 5.9, 6.1, 17.9, 18.1, 18.5] {
		runs.push(Run { label: format!("t{clock}-b3"), environment: Environment { clock_time: clock, brightness: 3.0, ..bare.clone() }, camera: camera.clone() });
	}
	for latitude in [-30.0f32, 23.5, 60.0] {
		runs.push(Run { label: format!("t0-lat{latitude}-b3"), environment: Environment { clock_time: 0.0, geographic_latitude: latitude, brightness: 3.0, ..bare.clone() }, camera: camera.clone() });
	}
	runs.push(Run { label: "t0-default".into(), environment: Environment { clock_time: 0.0, ..Environment::default() }, camera: camera.clone() });
	runs.push(Run { label: "t0-default-shadows-off".into(), environment: Environment { clock_time: 0.0, global_shadows: false, ..Environment::default() }, camera });
	Experiment { name: "light-night".into(), model, probes, runs }
}

/// Shadowed lights over a floor at grazing angles, by Range: PointLights 0.5 and 2 studs over a white
/// floor at Range 4, 8, 16 and 32 (Brightness 1), with Shadows and without; seen from above.
pub fn light_graze() -> Experiment {
	let mut model = ModelBuilder::new("LightGraze");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let mut index = 0;
	for range in [4.0f32, 8.0, 16.0, 32.0] {
		for height in [0.5f32, 2.0] {
			for shadows in [false, true] {
				let origin = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
				index += 1;
				floor(&mut model, &format!("Floor-{index}"), origin, -36.0, 36.0, -36.0, 36.0, 255);
				let light = Light { shadows, ..Light::point(1.0, range) };
				carrier(&mut model, &format!("Light-{index}"), Affine3A::from_translation(origin + Vec3::Y * height), Vec3::splat(0.2), light);
				let label = format!("r{range}-h{height}{}", if shadows { "-s" } else { "" });
				probes.push(Probe { label: label.clone(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "height": height, "range": range, "shadows": shadows}) });
				runs.push(Run { label, environment: dark(), camera: above(origin, range * 2.2 + 6.0, 50.0) });
			}
		}
	}
	Experiment { name: "light-graze".into(), model, probes, runs }
}

/// Where Studio takes local lights from: a PointLight on a part, on an Attachment, on an Attachment in
/// an Attachment and on one two levels deeper (each 3 studs over a white floor, Range 12, Brightness
/// 2), and one under a Model (not on a part); in the dark, from above.
pub fn light_parents() -> Experiment {
	let mut model = ModelBuilder::new("LightParents");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, label) in ["part", "attachment", "nested", "nested2", "model"].iter().enumerate() {
		let origin = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), origin, -14.0, 14.0, -14.0, 14.0, 255);
		let light = InstanceBuilder::new("PointLight").with_name("PointLight").with_property("Brightness", 2.0f32).with_property("Range", 12.0f32).with_property("Color", Color3::new(1.0, 1.0, 1.0));
		let holder = model.part(&PartSpec { name: format!("Holder-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 1.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let at = |offset: Vec3| crate::scene::build::cframe(Affine3A::from_translation(offset));
		match *label {
			"part" => {
				model.dom.destroy(holder);
				let part = model.part(&PartSpec { name: format!("Carrier-{label}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 3.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
				model.child(part, light);
			}
			"attachment" => {
				let first = model.child(holder, InstanceBuilder::new("Attachment").with_name("A").with_property("CFrame", at(Vec3::Y * 2.0)));
				model.child(first, light);
			}
			"nested" => {
				let first = model.child(holder, InstanceBuilder::new("Attachment").with_name("A").with_property("CFrame", at(Vec3::Y)));
				let second = model.child(first, InstanceBuilder::new("Attachment").with_name("B").with_property("CFrame", at(Vec3::Y)));
				model.child(second, light);
			}
			"nested2" => {
				let first = model.child(holder, InstanceBuilder::new("Attachment").with_name("A").with_property("CFrame", at(Vec3::Y * 0.5)));
				let second = model.child(first, InstanceBuilder::new("Attachment").with_name("B").with_property("CFrame", at(Vec3::Y * 0.5)));
				let third = model.child(second, InstanceBuilder::new("Attachment").with_name("C").with_property("CFrame", at(Vec3::Y)));
				model.child(third, light);
			}
			_ => {
				model.dom.destroy(holder);
				let group = model.child(model.model, InstanceBuilder::new("Model").with_name(format!("Group-{label}")));
				model.child(group, light);
			}
		}
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array()}) });
		runs.push(Run { label: (*label).into(), environment: dark(), camera: above(origin, 30.0, 50.0) });
	}
	Experiment { name: "light-parents".into(), model, probes, runs }
}

/// How far from the camera local lights still light particles: a 7 x 5 lattice of white square
/// particles (Size 1, LightInfluence 1) 3 studs apart around a PointLight (Brightness 2, Range 16) 2
/// studs behind it, the same lattice seen from 30 to 1000 studs away through a field of view that
/// keeps it 30 studs tall, from 300 in front and 300 to 500 to the side, and from 240 studs 300 studs
/// up. In the dark at EV -2.
pub fn light_particle_reach() -> Experiment {
	use super::particles::{add_emitter, constant, freeze, register};
	let name = "light-particle-reach";
	let mut model = ModelBuilder::new("LightParticleReach");
	let origin = Vec3::new(0.0, 100.0, 0.0);
	carrier(&mut model, "Light", Affine3A::from_translation(origin + Vec3::new(2.0, 2.0, 4.0)), Vec3::splat(0.2), Light::point(2.0, 16.0));
	for row in 0..5 {
		for column in 0..7 {
			let at = origin + Vec3::new((column as f32 - 3.0) * 3.0 + 2.0, (row as f32 - 2.0) * 3.0 + 2.0, 2.0);
			add_emitter(&mut model, &format!("p-{row}-{column}"), Affine3A::from_translation(at), &[("LightInfluence", 1.0f32.into()), ("Size", constant(1.0))]);
		}
	}
	// black backdrops behind and below, beyond the light's reach
	model.part(&PartSpec { name: "Back".into(), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 80.0)), size: Vec3::new(400.0, 400.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
	let probes = vec![Probe { label: "lattice".into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "light": (origin + Vec3::new(2.0, 2.0, 4.0)).to_array()}) }];
	let target = origin + Vec3::new(2.0, 2.0, 2.0);
	let mut runs = Vec::new();
	for distance in [30.0f32, 60.0, 120.0, 240.0, 360.0, 480.0, 520.0, 560.0, 600.0, 640.0, 680.0, 720.0, 1000.0] {
		let fov = 2.0 * (15.0 / distance).atan().to_degrees();
		let label = format!("d{distance}");
		register(name, &label, freeze(0.0, &[]));
		runs.push(Run { label, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (target - Vec3::Z * distance).into(), target: target.into(), fov } });
	}
	// sideways: 300 studs in front and 300 to 500 to the side
	for side in [300.0f32, 400.0, 500.0] {
		let label = format!("side{side}");
		register(name, &label, freeze(0.0, &[]));
		let eye = target + Vec3::new(side, 0.0, -300.0);
		runs.push(Run { label, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 2.0 * (15.0 / eye.distance(target)).atan().to_degrees() } });
	}
	let label = "high".to_string();
	register(name, &label, freeze(0.0, &[]));
	let eye = target + Vec3::new(0.0, 300.0, -240.0);
	runs.push(Run { label, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 2.0 * (15.0 / eye.distance(target)).atan().to_degrees() } });
	Experiment { name: name.into(), model, probes, runs }
}

/// How far from the camera local lights still light parts: a white wall with a PointLight (Brightness
/// 2, Range 16) 4 studs in front of it, and a second light with Shadows beside it, seen head-on from
/// 100 to 1500 studs away through a field of view that keeps 40 studs of it in view. In the dark.
pub fn light_reach() -> Experiment {
	let mut model = ModelBuilder::new("LightReach");
	let origin = Vec3::new(0.0, 60.0, 0.0);
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(80.0, 60.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
	carrier(&mut model, "Light", Affine3A::from_translation(origin + Vec3::new(-10.0, 0.0, -4.0)), Vec3::splat(0.2), Light::point(2.0, 16.0));
	carrier(&mut model, "Shadowed", Affine3A::from_translation(origin + Vec3::new(10.0, 0.0, -4.0)), Vec3::splat(0.2), Light { shadows: true, ..Light::point(2.0, 16.0) });
	let probes = vec![Probe { label: "wall".into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array()}) }];
	let mut runs = Vec::new();
	for distance in [100.0f32, 300.0, 450.0, 480.0, 500.0, 520.0, 600.0, 800.0, 1000.0, 1500.0] {
		let fov = 2.0 * (20.0 / distance).atan().to_degrees();
		runs.push(Run { label: format!("d{distance}"), environment: dark(), camera: CameraSpec::LookAt { eye: (origin - Vec3::Z * distance).into(), target: origin.into(), fov } });
	}
	Experiment { name: "light-reach".into(), model, probes, runs }
}

/// Local lights far from the camera, where Studio draws them coarsely (light-reach: sharp at 520
/// studs, one broad blob at 600): where the switch falls and what the coarse light is. In the dark.
/// - switch: PointLights of Range 8, 16 and 32 (Brightness 2) 4 studs before a white wall, 80 studs
///   apart, seen head-on from 480 to 700 studs (EV -2);
/// - straddle: a Range 60 light (Brightness 2) 20 studs over a white floor strip, the strip seen
///   along its length from 40 studs up, the light 500, 540, 560, 580 and 620 studs off (EV -2);
/// - shape: from 1,000 studs, lights 4 studs before a white wall on a 96-stud lattice of multiples of
///   16: PointLights of Range 8, 16, 24 and 32, Range 16 off the lattice by 4, 8 and (2, 6) studs, 0.5
///   and 12 studs from the wall, at Brightness 1 and 4 and in orange, a SpotLight (Angle 60) and a
///   SurfaceLight (a 4-stud face, Angle 90) facing the wall; a Range 60 light; a white ball (8 studs
///   across) with a light 12 studs to its left; one Range 16 light also from 1,600 and 2,400 (EV -3).
pub fn light_far() -> Experiment {
	let mut model = ModelBuilder::new("LightFar");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let aspect = 1096.0 / 744.0;
	// a view head-on onto the wall's plane z = 0 around `center`, `half_width` studs either side
	let head_on = |center: Vec3, distance: f32, half_width: f32| CameraSpec::LookAt {
		eye: (center - Vec3::Z * distance).into(),
		target: center.into(),
		fov: 2.0 * (half_width / aspect / distance).atan().to_degrees(),
	};
	let wall = |model: &mut ModelBuilder, name: &str, center: Vec3, width: f32, height: f32| {
		model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(width, height, 1.0), color: [255; 3], material: 272, ..Default::default() });
	};
	let near = Environment { exposure_compensation: -2.0, ..dark() };
	let far = Environment { exposure_compensation: -3.0, ..dark() };

	let switch = Vec3::new(0.0, 200.0, 0.0);
	wall(&mut model, "Wall-switch", switch, 320.0, 120.0);
	for (x, range) in [(-80.0f32, 8.0f32), (0.0, 16.0), (80.0, 32.0)] {
		let at = switch + Vec3::new(x, 0.0, -4.0);
		carrier(&mut model, &format!("Switch-r{range}"), Affine3A::from_translation(at), Vec3::splat(0.2), Light::point(2.0, range));
	}
	probes.push(Probe { label: "switch".into(), position: switch.into(), inputs: serde_json::json!({"origin": switch.to_array(), "lights": [[-80.0, 0.0, -4.0, 8.0], [0.0, 0.0, -4.0, 16.0], [80.0, 0.0, -4.0, 32.0]], "brightness": 2.0}) });
	for distance in [480.0f32, 500.0, 510.0, 520.0, 530.0, 540.0, 550.0, 560.0, 570.0, 580.0, 590.0, 600.0, 620.0, 650.0, 700.0] {
		runs.push(Run { label: format!("switch-d{distance}"), environment: near.clone(), camera: head_on(switch, distance, 125.0) });
	}

	for (index, along) in [500.0f32, 540.0, 560.0, 580.0, 620.0].iter().enumerate() {
		let strip = Vec3::new(1000.0 + index as f32 * 150.0, 0.0, 0.0);
		floor(&mut model, &format!("Strip-{along}"), strip, -30.0, 30.0, 400.0, 760.0, 255);
		carrier(&mut model, &format!("Straddle-{along}"), Affine3A::from_translation(strip + Vec3::new(0.0, 20.0, *along)), Vec3::splat(0.2), Light::point(2.0, 60.0));
		probes.push(Probe { label: format!("straddle-{along}"), position: strip.into(), inputs: serde_json::json!({"origin": strip.to_array(), "light": [0.0, 20.0, along], "range": 60.0, "brightness": 2.0}) });
		let eye = strip + Vec3::new(0.0, 40.0, 0.0);
		runs.push(Run { label: format!("straddle-{along}"), environment: near.clone(), camera: CameraSpec::LookAt { eye: eye.into(), target: (strip + Vec3::Z * 560.0).into(), fov: 2.2 } });
	}

	// the shape lattice: (column x, row y, light offset from the station, label, light)
	let shape = Vec3::new(0.0, 640.0, 0.0);
	wall(&mut model, "Wall-shape", shape, 1600.0, 420.0);
	let surface = Light { class: "SurfaceLight", angle: 90.0, face: 2, ..Light::point(2.0, 16.0) };
	let stations: [(f32, f32, Vec3, &str, Light, Vec3); 15] = [
		(-192.0, 96.0, Vec3::new(0.0, 0.0, -4.0), "r8", Light::point(2.0, 8.0), Vec3::splat(0.2)),
		(-96.0, 96.0, Vec3::new(0.0, 0.0, -4.0), "r16", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(0.0, 96.0, Vec3::new(4.0, 4.0, -4.0), "r16-off4", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(96.0, 96.0, Vec3::new(8.0, 8.0, -4.0), "r16-off8", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(192.0, 96.0, Vec3::new(0.0, 0.0, -4.0), "r32", Light::point(2.0, 32.0), Vec3::splat(0.2)),
		(-192.0, 0.0, Vec3::new(0.0, 0.0, -0.5), "r16-wall0.5", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(-96.0, 0.0, Vec3::new(0.0, 0.0, -12.0), "r16-wall12", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(0.0, 0.0, Vec3::new(0.0, 0.0, -4.0), "r16-b1", Light::point(1.0, 16.0), Vec3::splat(0.2)),
		(96.0, 0.0, Vec3::new(0.0, 0.0, -4.0), "r16-b4", Light::point(4.0, 16.0), Vec3::splat(0.2)),
		(192.0, 0.0, Vec3::new(0.0, 0.0, -4.0), "r16-orange", Light { color: [1.0, 85.0 / 255.0, 0.0], ..Light::point(2.0, 16.0) }, Vec3::splat(0.2)),
		(-192.0, -96.0, Vec3::new(0.0, 0.0, -4.0), "spot", Light { class: "SpotLight", angle: 60.0, face: 2, ..Light::point(2.0, 16.0) }, Vec3::splat(0.2)),
		(-96.0, -96.0, Vec3::new(0.0, 0.0, -4.0), "surface", surface, Vec3::new(4.0, 4.0, 0.2)),
		(0.0, -96.0, Vec3::new(2.0, 6.0, -4.0), "r16-off2-6", Light::point(2.0, 16.0), Vec3::splat(0.2)),
		(96.0, -96.0, Vec3::new(0.0, 0.0, -4.0), "r24", Light::point(2.0, 24.0), Vec3::splat(0.2)),
		(192.0, -96.0, Vec3::new(0.0, 0.0, -4.0), "r16-shadows", Light { shadows: true, ..Light::point(2.0, 16.0) }, Vec3::splat(0.2)),
	];
	let mut lattice = Vec::new();
	for (x, y, offset, label, light, size) in stations.iter() {
		let station = shape + Vec3::new(*x, *y, 0.0);
		carrier(&mut model, &format!("Shape-{label}"), Affine3A::from_translation(station + *offset), *size, *light);
		let mut inputs = light_inputs(light);
		inputs["station"] = serde_json::json!(station.to_array());
		inputs["light"] = serde_json::json!((station + *offset).to_array());
		lattice.push(serde_json::json!({"label": label, "inputs": inputs}));
	}
	probes.push(Probe { label: "shape".into(), position: shape.into(), inputs: serde_json::json!({"origin": shape.to_array(), "stations": lattice}) });
	for (row, y) in [("top", 96.0f32), ("middle", 0.0), ("bottom", -96.0)] {
		runs.push(Run { label: format!("shape-{row}"), environment: far.clone(), camera: head_on(shape + Vec3::new(0.0, y, 0.0), 1000.0, 245.0) });
	}
	for distance in [1000.0f32, 1600.0, 2400.0] {
		runs.push(Run { label: format!("shape-r16-d{distance}"), environment: far.clone(), camera: head_on(shape + Vec3::new(-96.0, 96.0, 0.0), distance, 48.0) });
	}

	let wide = shape + Vec3::new(-560.0, 0.0, 0.0);
	carrier(&mut model, "Shape-r60", Affine3A::from_translation(wide + Vec3::new(0.0, 0.0, -4.0)), Vec3::splat(0.2), Light::point(2.0, 60.0));
	probes.push(Probe { label: "r60".into(), position: wide.into(), inputs: serde_json::json!({"origin": wide.to_array(), "light": (wide + Vec3::new(0.0, 0.0, -4.0)).to_array(), "range": 60.0, "brightness": 2.0}) });
	runs.push(Run { label: "shape-r60".into(), environment: far.clone(), camera: head_on(wide, 1000.0, 120.0) });

	let ball = shape + Vec3::new(560.0, 0.0, -40.0);
	model.part(&PartSpec { name: "Ball".into(), cframe: Affine3A::from_translation(ball), size: Vec3::splat(8.0), color: [255; 3], material: 272, shape: Some(0), ..Default::default() });
	carrier(&mut model, "Ball-light", Affine3A::from_translation(ball - Vec3::X * 12.0), Vec3::splat(0.2), Light::point(2.0, 24.0));
	probes.push(Probe { label: "ball".into(), position: ball.into(), inputs: serde_json::json!({"ball": ball.to_array(), "radius": 4.0, "light": (ball - Vec3::X * 12.0).to_array(), "range": 24.0, "brightness": 2.0}) });
	runs.push(Run { label: "ball".into(), environment: far.clone(), camera: CameraSpec::LookAt { eye: (ball - Vec3::Z * 1000.0).into(), target: ball.into(), fov: 2.0 * (30.0f32 / aspect / 1000.0).atan().to_degrees() } });

	Experiment { name: "light-far".into(), model, probes, runs }
}

/// What lights a scene through the night and twilight, term by term: light-night's white floor, ball
/// and tiles with a white wall facing the camera at the back, under the default Lighting at ClockTime
/// 0-22 (every quarter hour around sunrise and sunset), and at each time with no sun or moon
/// (Brightness 0), the sky's light alone (no Ambient or OutdoorAmbient), OutdoorAmbient alone,
/// Ambient alone and the sun or moon alone (no Ambient, OutdoorAmbient or sky light); the sun alone
/// near the horizon at latitudes 30 and 60; ColorShift_Top and _Bottom at midnight and noon.
pub fn light_night_sky() -> Experiment {
	let mut model = ModelBuilder::new("LightNightSky");
	let mut probes = Vec::new();
	floor(&mut model, "Floor", Vec3::ZERO, -30.0, 30.0, -30.0, 30.0, 255);
	model.part(&PartSpec { name: "Ball".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 3.0, 6.0)), size: Vec3::splat(6.0), color: [255; 3], material: 272, shape: Some(0), ..Default::default() });
	model.part(&PartSpec { name: "Back".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 6.0, 26.5)), size: Vec3::new(30.0, 12.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
	for (index, degrees) in [-60.0f32, -30.0, 0.0, 30.0, 60.0].iter().enumerate() {
		let x = (index as f32 - 2.0) * 7.0;
		// a tile whose normal leans `degrees` from up towards +X
		let normal = Vec3::new(degrees.to_radians().sin(), degrees.to_radians().cos(), 0.0);
		let center = Vec3::new(x, 3.0, -6.0);
		model.part(&PartSpec { name: format!("Tile{degrees}"), cframe: crate::scene::build::facing(center, normal), size: Vec3::new(4.0, 4.0, 0.2), color: [255; 3], material: 272, ..Default::default() });
		probes.push(Probe { label: format!("tile{degrees}"), position: (center + normal * 0.1).into(), inputs: serde_json::json!({"normal": normal.to_array()}) });
	}
	probes.push(Probe { label: "floor".into(), position: [12.0, 0.0, 12.0], inputs: serde_json::json!({"normal": [0.0, 1.0, 0.0]}) });
	probes.push(Probe { label: "back".into(), position: [0.0, 6.0, 25.9], inputs: serde_json::json!({"normal": [0.0, 0.0, -1.0]}) });
	let target = Vec3::new(0.0, 2.0, 0.0);
	let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, 35f32.to_radians().sin(), -35f32.to_radians().cos()) * 40.0).into(), target: target.into(), fov: 50.0 };
	let mut runs = Vec::new();
	let base = Environment::default();
	let terms = [
		("default", base.clone()),
		("nodirect", Environment { brightness: 0.0, ..base.clone() }),
		("sky", Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, ..base.clone() }),
		("outdoor", Environment { brightness: 0.0, ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..base.clone() }),
		("ambient", Environment { brightness: 0.0, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..base.clone() }),
		("direct", Environment { ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..base.clone() }),
	];
	for clock in [0.0f32, 2.0, 4.0, 5.0, 5.5, 5.75, 6.0, 6.25, 6.5, 7.0, 8.0, 12.0, 16.5, 17.0, 17.5, 17.75, 18.0, 18.25, 18.5, 19.0, 20.0, 22.0] {
		for (term, environment) in terms.iter() {
			runs.push(Run { label: format!("t{clock}-{term}"), environment: Environment { clock_time: clock, ..environment.clone() }, camera: camera.clone() });
		}
	}
	let direct = terms[5].1.clone();
	for latitude in [30.0f32, 60.0] {
		for clock in [6.25f32, 6.5, 7.0, 17.5] {
			runs.push(Run { label: format!("t{clock}-lat{latitude}-direct"), environment: Environment { clock_time: clock, geographic_latitude: latitude, ..direct.clone() }, camera: camera.clone() });
		}
	}
	runs.push(Run { label: "t0-env0".into(), environment: Environment { clock_time: 0.0, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..base.clone() }, camera: camera.clone() });
	for clock in [0.0f32, 12.0] {
		runs.push(Run { label: format!("t{clock}-shift-top"), environment: Environment { clock_time: clock, color_shift_top: Vec3::new(1.0, 0.5, 0.0), ..base.clone() }, camera: camera.clone() });
		runs.push(Run { label: format!("t{clock}-shift-bottom"), environment: Environment { clock_time: clock, color_shift_bottom: Vec3::new(0.0, 0.5, 1.0), ..base.clone() }, camera: camera.clone() });
	}
	Experiment { name: "light-night-sky".into(), model, probes, runs }
}

/// SpotLight and SurfaceLight cones on particles and beams: light-particle-field's lattices of small
/// white squares (Size 0.4, LightInfluence 1, a stud apart in the plane z = 2) around a light at a
/// voxel centre in their plane, Range 16, Brightness 2: SpotLights of Angle 60, 120 and 180 facing +X
/// (along the lattice), one of Angle 60 facing the camera (-Z), SurfaceLights on 4-stud faces facing
/// +X at Angle 90 and 180; FaceCamera beams 0, 2 and 4 studs over a SpotLight of Angle 60 facing +X.
/// In the dark at EV -2, from the front.
pub fn light_particle_cones() -> Experiment {
	use super::particles::{add_emitter, constant, freeze, register};
	let name = "light-particle-cones";
	let mut model = ModelBuilder::new("LightParticleCones");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let spot = |angle: f32, face: u32| Light { class: "SpotLight", angle, face, ..Light::point(2.0, 16.0) };
	let surface = |angle: f32| Light { class: "SurfaceLight", angle, face: 0, ..Light::point(2.0, 16.0) };
	// (label, light, carrier size)
	let lattices: [(&str, Light, Vec3); 6] = [
		("spot60", spot(60.0, 0), Vec3::splat(0.2)),
		("spot120", spot(120.0, 0), Vec3::splat(0.2)),
		("spot180", spot(180.0, 0), Vec3::splat(0.2)),
		("spot60-camera", spot(60.0, 5), Vec3::splat(0.2)),
		("surface90", surface(90.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface180", surface(180.0), Vec3::new(0.2, 4.0, 4.0)),
	];
	let (x0, x1, y0, y1) = (-10i32, 18i32, -6i32, 10i32);
	for (index, (label, light, size)) in lattices.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 200.0, 400.0, 0.0);
		let light_at = origin + Vec3::new(2.0, 2.0, 2.0);
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(light_at), *size, *light);
		for (side, z) in [("far", 60.0f32), ("near", -100.0)] {
			model.part(&PartSpec { name: format!("Wall-{label}-{side}"), cframe: Affine3A::from_translation(origin + Vec3::new(4.0, 0.0, z)), size: Vec3::new(300.0, 160.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		}
		for row in 0..=(y1 - y0) {
			for column in 0..=(x1 - x0) {
				let at = origin + Vec3::new((x0 + column) as f32, (y0 + row) as f32, 2.0);
				add_emitter(&mut model, &format!("{label}-{row}-{column}"), Affine3A::from_translation(at), &[("LightInfluence", 1.0f32.into()), ("Size", constant(0.4))]);
			}
		}
		let middle = origin + Vec3::new((x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5, 2.0);
		let mut inputs = light_inputs(light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["light"] = serde_json::json!(light_at.to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		inputs["x"] = serde_json::json!([x0, x1]);
		inputs["y"] = serde_json::json!([y0, y1]);
		inputs["spacing"] = serde_json::json!(1.0);
		inputs["z"] = serde_json::json!(2.0);
		inputs["middle"] = serde_json::json!(middle.to_array());
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		let run = format!("{label}-front");
		register(name, &run, freeze(0.0, &[]));
		runs.push(Run { label: run, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (middle - Vec3::Z * 40.0).into(), target: middle.into(), fov: 40.0 } });
	}
	let origin = Vec3::new(lattices.len() as f32 * 200.0, 400.0, 0.0);
	let light_at = origin + Vec3::new(2.0, 2.0, 2.0);
	carrier(&mut model, "Light-beams", Affine3A::from_translation(light_at), Vec3::splat(0.2), spot(60.0, 0));
	for (k, height) in [0.0f32, 2.0, 4.0].iter().enumerate() {
		let beam: super::BeamProperties = vec![("LightInfluence", 1.0f32.into()), ("FaceCamera", true.into()), ("Width0", 0.5f32.into()), ("Width1", 0.5f32.into())];
		super::add_beam(&mut model, &format!("Beam-{k}"), origin + Vec3::new(4.0, 2.0 + height, 2.0), Affine3A::from_translation(Vec3::new(-14.0, 0.0, 0.0)), Affine3A::from_translation(Vec3::new(20.0, 0.0, 0.0)), &beam);
	}
	model.part(&PartSpec { name: "Wall-beams".into(), cframe: Affine3A::from_translation(origin + Vec3::new(4.0, 0.0, 60.0)), size: Vec3::new(200.0, 120.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
	let mut inputs = light_inputs(&spot(60.0, 0));
	inputs["origin"] = serde_json::json!(origin.to_array());
	inputs["light"] = serde_json::json!(light_at.to_array());
	probes.push(Probe { label: "beams".into(), position: origin.into(), inputs });
	let middle = origin + Vec3::new(4.0, 4.0, 2.0);
	runs.push(Run { label: "beams-front".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (middle - Vec3::Z * 40.0).into(), target: middle.into(), fov: 40.0 } });
	Experiment { name: name.into(), model, probes, runs }
}

/// Narrow SurfaceLights on particles: light-particle-cones' lattices round SurfaceLights facing +X at
/// Angle 0, 1, 3, 10, 30 and 90 on 4-stud faces (Range 16, Brightness 2), and at Angle 0 on an 8-stud face
/// at Range 24 (a cutscene rim light's shape). In the dark at EV -2, from the front.
pub fn light_particle_narrow() -> Experiment {
	use super::particles::{add_emitter, constant, freeze, register};
	let name = "light-particle-narrow";
	let mut model = ModelBuilder::new("LightParticleNarrow");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let surface = |angle: f32, range: f32| Light { class: "SurfaceLight", angle, face: 0, ..Light::point(2.0, range) };
	let lattices: [(&str, Light, Vec3); 10] = [
		("surface0", surface(0.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface0.5", surface(0.5, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface1", surface(1.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface1.5", surface(1.5, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface2", surface(2.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface3", surface(3.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface10", surface(10.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface30", surface(30.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface90", surface(90.0, 16.0), Vec3::new(0.2, 4.0, 4.0)),
		("surface0-big", surface(0.0, 24.0), Vec3::new(0.2, 8.0, 8.0)),
	];
	let (x0, x1, y0, y1) = (-10i32, 26i32, -8i32, 12i32);
	for (index, (label, light, size)) in lattices.iter().enumerate() {
		let origin = Vec3::new(index as f32 * 200.0, 400.0, 0.0);
		let light_at = origin + Vec3::new(2.0, 2.0, 2.0);
		carrier(&mut model, &format!("Light-{label}"), Affine3A::from_translation(light_at), *size, *light);
		for (side, z) in [("far", 60.0f32), ("near", -100.0)] {
			model.part(&PartSpec { name: format!("Wall-{label}-{side}"), cframe: Affine3A::from_translation(origin + Vec3::new(4.0, 0.0, z)), size: Vec3::new(300.0, 160.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		}
		for row in 0..=(y1 - y0) {
			for column in 0..=(x1 - x0) {
				let at = origin + Vec3::new((x0 + column) as f32, (y0 + row) as f32, 2.0);
				add_emitter(&mut model, &format!("{label}-{row}-{column}"), Affine3A::from_translation(at), &[("LightInfluence", 1.0f32.into()), ("Size", constant(0.4))]);
			}
		}
		let middle = origin + Vec3::new((x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5, 2.0);
		let mut inputs = light_inputs(light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["light"] = serde_json::json!(light_at.to_array());
		inputs["size"] = serde_json::json!(size.to_array());
		probes.push(Probe { label: (*label).into(), position: origin.into(), inputs });
		let run = format!("{label}-front");
		register(name, &run, freeze(0.0, &[]));
		runs.push(Run { label: run, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (middle - Vec3::Z * 50.0).into(), target: middle.into(), fov: 40.0 } });
	}
	Experiment { name: name.into(), model, probes, runs }
}

/// Local light on particles after a camera cut: light-particle-field's lattice round a PointLight
/// (Range 16, Brightness 2) and a white wall lit by another, the camera turned 3,000 studs away for 3 s
/// (the lights on throughout, or switched on while it is away), the particles emitted and held there,
/// then the camera cut back and captured 0.05-4 s after. In the dark at EV -2.
pub fn light_particle_cut() -> Experiment {
	use super::particles::{add_emitter, constant, register, Setup};
	let name = "light-particle-cut";
	let mut model = ModelBuilder::new("LightParticleCut");
	let origin = Vec3::new(0.0, 400.0, 0.0);
	let light_at = origin + Vec3::new(2.0, 2.0, 2.0);
	carrier(&mut model, "Light-field", Affine3A::from_translation(light_at), Vec3::splat(0.2), Light::point(2.0, 16.0));
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(origin + Vec3::new(40.0, 0.0, 12.5)), size: Vec3::new(30.0, 30.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
	carrier(&mut model, "Light-wall", Affine3A::from_translation(origin + Vec3::new(40.0, 0.0, 8.0)), Vec3::splat(0.2), Light::point(2.0, 16.0));
	model.part(&PartSpec { name: "Back".into(), cframe: Affine3A::from_translation(origin + Vec3::new(20.0, 0.0, 60.0)), size: Vec3::new(300.0, 160.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
	for row in 0..=16 {
		for column in 0..=28 {
			let at = origin + Vec3::new((column - 10) as f32, (row - 6) as f32, 2.0);
			add_emitter(&mut model, &format!("p-{row}-{column}"), Affine3A::from_translation(at), &[("LightInfluence", 1.0f32.into()), ("Size", constant(0.4))]);
		}
	}
	let target = origin + Vec3::new(20.0, 2.0, 2.0);
	let mut runs = Vec::new();
	for (mode, enable_away) in [("on", false), ("enabled-away", true)] {
		for time in [0.05f32, 0.2, 0.5, 1.0, 2.0, 4.0] {
			let label = format!("{mode}-t{time}");
			let luau = format!(
				"local camera = workspace.CurrentCamera
local home = camera.CFrame
local lights = {{}}
for _, l in workspace.RorCapture:GetDescendants() do
	if l:IsA(\"PointLight\") then
		table.insert(lights, l)
		l.Enabled = not {enable_away}
	end
end
local emitters = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.TimeScale = 1
		e:Clear()
		table.insert(emitters, e)
	end
end
task.wait(0.3)
camera.CFrame = CFrame.new(home.Position + Vector3.new(3000, 0, 0)) * home.Rotation
task.wait(0.5)
for _, l in lights do
	l.Enabled = true
end
for _, e in emitters do
	e:Emit(1)
	e.TimeScale = 0
end
task.wait(2.5)
camera.CFrame = home
task.wait({time})
return 1"
			);
			register(name, &label, Setup { luau, bursts: vec![(0.0, 1)], counts: Vec::new(), seeds: 0 });
			runs.push(Run { label, environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (target - Vec3::Z * 60.0).into(), target: target.into(), fov: 40.0 } });
		}
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Shadowed lights grazing surfaces, more of them: PointLights 0.25, 1 and 4 studs over a white floor
/// at Range 8, 16, 32 and 60 (Brightness 1), with Shadows and without, seen from above; and a light
/// 1 stud before a white wall (Range 16), with Shadows and without, seen head-on.
pub fn light_graze_more() -> Experiment {
	let mut model = ModelBuilder::new("LightGrazeMore");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let mut index = 0;
	for range in [8.0f32, 16.0, 32.0, 60.0] {
		for height in [0.25f32, 1.0, 4.0] {
			for shadows in [false, true] {
				let origin = Vec3::new(index as f32 * 150.0, 0.0, 500.0);
				index += 1;
				let half = (range * 1.1).max(12.0);
				floor(&mut model, &format!("Floor-{index}"), origin, -half, half, -half, half, 255);
				let light = Light { shadows, ..Light::point(1.0, range) };
				carrier(&mut model, &format!("Light-{index}"), Affine3A::from_translation(origin + Vec3::Y * height), Vec3::splat(0.2), light);
				let label = format!("r{range}-h{height}{}", if shadows { "-s" } else { "" });
				let mut inputs = light_inputs(&light);
				inputs["origin"] = serde_json::json!(origin.to_array());
				inputs["height"] = serde_json::json!(height);
				probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
				runs.push(Run { label, environment: dark(), camera: down(origin, half * 2.6) });
			}
		}
	}
	for shadows in [false, true] {
		let origin = Vec3::new(index as f32 * 150.0, 100.0, 500.0);
		index += 1;
		model.part(&PartSpec { name: format!("Wall-{index}"), cframe: Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(40.0, 40.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
		let light = Light { shadows, ..Light::point(1.0, 16.0) };
		carrier(&mut model, &format!("Light-{index}"), Affine3A::from_translation(origin - Vec3::Z), Vec3::splat(0.2), light);
		let label = format!("wall-r16-d1{}", if shadows { "-s" } else { "" });
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["height"] = serde_json::json!(1.0);
		probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
		runs.push(Run { label, environment: dark(), camera: CameraSpec::LookAt { eye: (origin - Vec3::Z * 45.0).into(), target: origin.into(), fov: 45.0 } });
	}
	Experiment { name: "light-graze-more".into(), model, probes, runs }
}

/// What makes the moon brighter under the default Lighting than alone (light-night-sky: the default
/// Lighting at night lights like the moon over again, with its shadows): light-night-sky's scene at
/// ClockTime 0, 4, 5.75, 12, 18.25 and 20 with the sun or moon (Brightness 2) and one other term at a
/// time: OutdoorAmbient (default, white, red), Ambient, the sky's diffuse light, its specular, both;
/// and the default Lighting at Brightness 1 and 4. The sun alone every 0.05 hours after sunrise and
/// before sunset, where Studio dims it.
pub fn light_night_terms() -> Experiment {
	let mut experiment = light_night_sky();
	experiment.name = "light-night-terms".into();
	let camera = experiment.runs[0].camera.clone();
	let bare = Environment { ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..Environment::default() };
	let base = Environment::default();
	let terms = [
		("outdoor", Environment { outdoor_ambient: base.outdoor_ambient, ..bare.clone() }),
		("outdoor-white", Environment { outdoor_ambient: Vec3::ONE, ..bare.clone() }),
		("outdoor-red", Environment { outdoor_ambient: Vec3::new(1.0, 0.0, 0.0), ..bare.clone() }),
		("ambient", Environment { ambient: base.ambient, ..bare.clone() }),
		("diffuse", Environment { environment_diffuse_scale: 1.0, ..bare.clone() }),
		("specular", Environment { environment_specular_scale: 1.0, ..bare.clone() }),
		("sky", Environment { environment_diffuse_scale: 1.0, environment_specular_scale: 1.0, ..bare.clone() }),
		("default-b1", Environment { brightness: 1.0, ..base.clone() }),
		("default-b4", Environment { brightness: 4.0, ..base.clone() }),
	];
	let mut runs = Vec::new();
	for clock in [0.0f32, 4.0, 5.75, 12.0, 18.25, 20.0] {
		for (term, environment) in terms.iter() {
			runs.push(Run { label: format!("t{clock}-{term}"), environment: Environment { clock_time: clock, ..environment.clone() }, camera: camera.clone() });
		}
	}
	for clock in [6.05f32, 6.1, 6.15, 6.2, 6.3, 6.35, 6.4, 6.45, 17.55, 17.6, 17.65, 17.7, 17.8, 17.85, 17.9, 17.95] {
		runs.push(Run { label: format!("t{clock}-direct"), environment: Environment { clock_time: clock, ..bare.clone() }, camera: camera.clone() });
	}
	experiment.runs = runs;
	experiment
}

/// Whether transparent parts take the far voxel light (light-far: opaque walls do): white walls at
/// Transparency 0, 0.02, 0.5 and 0.9 (over black backdrops 30 studs behind), each with a PointLight
/// (Range 16, Brightness 2) 4 studs before it, seen head-on from 1,000 studs (EV -3) and from 300
/// (EV -2).
pub fn light_far_transparent() -> Experiment {
	let mut model = ModelBuilder::new("LightFarTransparent");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let aspect = 1096.0 / 744.0;
	let center = Vec3::new(0.0, 300.0, 0.0);
	let mut lights = Vec::new();
	for (index, transparency) in [0.0f32, 0.02, 0.5, 0.9].iter().enumerate() {
		let at = center + Vec3::new((index as f32 - 1.5) * 64.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("Wall-{transparency}"), cframe: Affine3A::from_translation(at + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(56.0, 56.0, 1.0), color: [255; 3], material: 272, transparency: *transparency, ..Default::default() });
		model.part(&PartSpec { name: format!("Back-{transparency}"), cframe: Affine3A::from_translation(at + Vec3::new(0.0, 0.0, 30.5)), size: Vec3::new(60.0, 60.0, 1.0), color: [0; 3], material: 272, cast_shadow: false, ..Default::default() });
		carrier(&mut model, &format!("Light-{transparency}"), Affine3A::from_translation(at + Vec3::new(0.0, 0.0, -4.0)), Vec3::splat(0.2), Light::point(2.0, 16.0));
		lights.push(serde_json::json!({"transparency": transparency, "station": at.to_array(), "light": (at + Vec3::new(0.0, 0.0, -4.0)).to_array()}));
	}
	probes.push(Probe { label: "walls".into(), position: center.into(), inputs: serde_json::json!({"origin": center.to_array(), "walls": lights, "range": 16.0, "brightness": 2.0}) });
	for (distance, exposure) in [(1000.0f32, -3.0f32), (300.0, -2.0)] {
		let fov = 2.0 * (135.0 / aspect / distance).atan().to_degrees();
		runs.push(Run { label: format!("d{distance}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: CameraSpec::LookAt { eye: (center - Vec3::Z * distance).into(), target: center.into(), fov } });
	}
	Experiment { name: "light-far-transparent".into(), model, probes, runs }
}

/// Long Ranges in the far voxel light (an effect's sky dome: a Range 120 light 885 studs off): white
/// walls with a PointLight (Brightness 1) 4 studs before each at Range 60, 90 and 120, 300 studs
/// apart, seen head-on from 1,000 and from 400 studs (EV -2).
pub fn light_far_range() -> Experiment {
	let mut model = ModelBuilder::new("LightFarRange");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let aspect = 1096.0 / 744.0;
	let center = Vec3::new(0.0, 900.0, 0.0);
	let mut walls = Vec::new();
	for (index, range) in [60.0f32, 90.0, 120.0].iter().enumerate() {
		let at = center + Vec3::new((index as f32 - 1.0) * 300.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("Wall-{range}"), cframe: Affine3A::from_translation(at + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(290.0, 290.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
		carrier(&mut model, &format!("Light-{range}"), Affine3A::from_translation(at + Vec3::new(0.0, 0.0, -4.0)), Vec3::splat(0.2), Light::point(1.0, *range));
		walls.push(serde_json::json!({"range": range, "station": at.to_array(), "light": (at + Vec3::new(0.0, 0.0, -4.0)).to_array()}));
	}
	probes.push(Probe { label: "walls".into(), position: center.into(), inputs: serde_json::json!({"origin": center.to_array(), "walls": walls, "brightness": 1.0}) });
	for distance in [1000.0f32, 400.0] {
		for (index, range) in [60.0f32, 90.0, 120.0].iter().enumerate() {
			let at = center + Vec3::new((index as f32 - 1.0) * 300.0, 0.0, 0.0);
			let fov = 2.0 * (145.0 / aspect / distance).atan().to_degrees();
			runs.push(Run { label: format!("r{range}-d{distance}"), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: CameraSpec::LookAt { eye: (at - Vec3::Z * distance).into(), target: at.into(), fov } });
		}
	}
	Experiment { name: "light-far-range".into(), model, probes, runs }
}


/// How soon a light shows once it appears (an effect's sky dome: its light, 885 studs off, lights nothing
/// in Studio's frames 0.1-0.6 s after the effect is placed): a white wall with a PointLight (Range 16,
/// Brightness 2) 4 studs before it, and 40 studs to its right a lattice of white squares (Size 0.8,
/// LightInfluence 1, one each emitted at the start and frozen) around a second such light, the model
/// placed at the start and seen 0.1 to 4 seconds later from 1,000 studs (the wall drawn from the
/// lights' voxels; EV -3) and from 100 (per pixel, the particles from the voxels; EV -2). In the dark.
pub fn light_latency() -> Experiment {
	use super::particles::{constant, emitter_builder};
	use super::timeline::{Timeline, register};
	use crate::scene::playback::{Action, Emit, Playback};
	let name = "light-latency";
	let mut model = ModelBuilder::new("LightLatency");
	let center = Vec3::new(0.0, 1500.0, 0.0);
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(60.0, 60.0, 1.0), color: [255; 3], material: 272, ..Default::default() });
	carrier(&mut model, "Light", Affine3A::from_translation(center + Vec3::new(0.0, 0.0, -4.0)), Vec3::splat(0.2), Light::point(2.0, 16.0));
	let field = center + Vec3::new(40.0, 0.0, -20.0);
	carrier(&mut model, "Light-field", Affine3A::from_translation(field), Vec3::splat(0.2), Light::point(2.0, 16.0));
	for row in 0..7 {
		for column in 0..9 {
			let at = field + Vec3::new(column as f32 * 2.0 - 8.0, row as f32 * 2.0 - 6.0, 0.0);
			let anchor = model.part(&PartSpec { name: format!("p-{row}-{column}"), cframe: Affine3A::from_translation(at), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			let attachment = model.child(anchor, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", crate::scene::build::cframe(Affine3A::IDENTITY)));
			model.child(attachment, emitter_builder("Emitter", &[("Size", constant(0.8)), ("LightInfluence", 1.0f32.into())]));
		}
	}
	let probes = vec![Probe { label: "wall".into(), position: center.into(), inputs: serde_json::json!({"origin": center.to_array(), "light": (center + Vec3::new(0.0, 0.0, -4.0)).to_array(), "field": field.to_array()}) }];
	let aspect = 1096.0 / 744.0;
	let mut runs = Vec::new();
	for (place, distance, exposure, times) in [("far", 1000.0f32, -3.0f32, &[0.1f32, 0.3, 0.6, 1.0, 2.0, 4.0][..]), ("near", 100.0, -2.0, &[0.1f32, 0.6, 2.0][..])] {
		for time in times {
			let label = format!("{place}-t{time}");
			let emit = Action { at: 0.0, target: "LightLatency".into(), emit: Some(Emit::Count(1)), ..Default::default() };
			let freeze = Action { at: 0.03, target: "LightLatency".into(), time_scale: Some(0.0), ..Default::default() };
			register(name, &label, Timeline { playback: Playback { time: *time, actions: vec![emit, freeze], ..Default::default() }, duration: time + 0.3 });
			let target = center + Vec3::new(20.0, 0.0, 0.0);
			runs.push(Run { label, environment: Environment { exposure_compensation: exposure, ..dark() }, camera: CameraSpec::LookAt { eye: (target - Vec3::Z * distance).into(), target: target.into(), fov: 2.0 * (60.0 / aspect / distance).atan().to_degrees() } });
		}
	}
	Experiment { name: name.into(), model, probes, runs }
}

/// The night's light around the twilights, finer (light-night-sky: under the default Lighting the moon
/// lights at 0.205 · Brightness all night, but nothing at 5 and 19): light-night-sky's scene every
/// quarter hour or less from 4 to 5.5 and 18.5 to 20, under the default Lighting, with no sun or moon
/// (Brightness 0), and with no Ambient, OutdoorAmbient or sky light.
pub fn light_night_twilight() -> Experiment {
	let mut experiment = light_night_sky();
	experiment.name = "light-night-twilight".into();
	let camera = experiment.runs[0].camera.clone();
	let base = Environment::default();
	let terms = [
		("default", base.clone()),
		("nodirect", Environment { brightness: 0.0, ..base.clone() }),
		("direct", Environment { ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..base.clone() }),
	];
	let mut runs = Vec::new();
	for clock in [4.25f32, 4.5, 4.75, 4.9, 5.1, 5.2, 5.3, 5.4, 18.6, 18.7, 18.8, 18.9, 19.1, 19.25, 19.5, 19.75] {
		for (term, environment) in terms.iter() {
			runs.push(Run { label: format!("t{clock}-{term}"), environment: Environment { clock_time: clock, ..environment.clone() }, camera: camera.clone() });
		}
	}
	experiment.runs = runs;
	experiment
}

/// The night with less sky light: light-night-sky's scene under the default Lighting with
/// EnvironmentDiffuseScale 0 at ClockTime 2.5-3.5 and 20.5-21.5 (where Studio's sky turns black
/// without it), and with EnvironmentDiffuseScale 0.5 at 0, 3 and 20, with and without the moon
/// (Brightness 0).
pub fn light_night_sky2() -> Experiment {
	let mut experiment = light_night_sky();
	experiment.name = "light-night-sky2".into();
	let camera = experiment.runs[0].camera.clone();
	let base = Environment::default();
	let mut runs = Vec::new();
	for clock in [2.5f32, 3.0, 3.5, 20.5, 21.0, 21.5] {
		runs.push(Run { label: format!("t{clock}-eds0"), environment: Environment { clock_time: clock, environment_diffuse_scale: 0.0, ..base.clone() }, camera: camera.clone() });
	}
	for clock in [0.0f32, 3.0, 20.0] {
		runs.push(Run { label: format!("t{clock}-eds0.5"), environment: Environment { clock_time: clock, environment_diffuse_scale: 0.5, ..base.clone() }, camera: camera.clone() });
		runs.push(Run { label: format!("t{clock}-eds0.5-nodirect"), environment: Environment { clock_time: clock, environment_diffuse_scale: 0.5, brightness: 0.0, ..base.clone() }, camera: camera.clone() });
	}
	experiment.runs = runs;
	experiment
}



/// The grazing darkening (light-graze) beyond PointLights in the dark: PointLights of Range 16 0.5
/// and 2 studs over a white floor, with Shadows and without, under the default daylight (at -2 EV),
/// by day without GlobalShadows and at midnight under the moon alone; shadowed PointLights 0.05, 0.1, 0.15
/// and 0.2 studs up in the dark; SpotLights of Angle 120 facing down and SurfaceLights of Angle 180 on
/// the underside of a 2-stud face, 0.5 and 2 studs up, with Shadows and without, in the dark; and
/// facing along the floor, SpotLights of Angle 90 and 150 and SurfaceLights of Angle 180 on a 2-stud
/// face, 0.5 and 2 studs up (the face's centre), with Shadows and without, in the dark. Seen from
/// above.
pub fn light_graze_kinds() -> Experiment {
	let mut model = ModelBuilder::new("LightGrazeKinds");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let night = Environment { clock_time: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..Environment::default() };
	let mut cases: Vec<(String, Light, f32, Environment)> = Vec::new();
	// by day at -2 EV, the sunlit white floor and the light's foot short of the tone curve's top
	let day = Environment { exposure_compensation: -2.0, ..Environment::default() };
	for (name, environment) in [("day", day.clone()), ("day-noshadows", Environment { global_shadows: false, ..day }), ("night", night)] {
		for height in [0.5f32, 2.0] {
			for shadows in [false, true] {
				cases.push((format!("{name}-h{height}{}", if shadows { "-s" } else { "" }), Light { shadows, ..Light::point(1.0, 16.0) }, height, environment.clone()));
			}
		}
	}
	for height in [0.05f32, 0.1, 0.15, 0.2] {
		cases.push((format!("dark-h{height}-s"), Light { shadows: true, ..Light::point(1.0, 16.0) }, height, dark()));
	}
	for (class, angle) in [("SpotLight", 120.0f32), ("SurfaceLight", 180.0)] {
		for height in [0.5f32, 2.0] {
			for shadows in [false, true] {
				let light = Light { class, angle, face: 4, shadows, ..Light::point(1.0, 16.0) };
				cases.push((format!("{}-h{height}{}", class.to_lowercase(), if shadows { "-s" } else { "" }), light, height, dark()));
			}
		}
	}
	// facing along the floor (+Z), so that it grazes the floor inside the cone
	for (class, angle) in [("SpotLight", 90.0f32), ("SpotLight", 150.0), ("SurfaceLight", 180.0)] {
		for height in [0.5f32, 2.0] {
			for shadows in [false, true] {
				let light = Light { class, angle, face: 5, shadows, ..Light::point(1.0, 16.0) };
				cases.push((format!("{}{angle}-front-h{height}{}", class.to_lowercase(), if shadows { "-s" } else { "" }), light, height, dark()));
			}
		}
	}
	for (index, (label, light, height, environment)) in cases.into_iter().enumerate() {
		let origin = Vec3::new(index as f32 * 100.0, 0.0, 900.0);
		floor(&mut model, &format!("Floor-{index}"), origin, -36.0, 36.0, -36.0, 36.0, 255);
		// a SurfaceLight's face 2 studs across: facing down its underside the given height up, facing
		// along the floor its centre (and the face 0.1 thick, so its bottom edge 1 stud lower)
		let down = light.face == 4;
		let size = match (light.class, down) {
			("SurfaceLight", true) => Vec3::new(2.0, 0.2, 2.0),
			("SurfaceLight", false) => Vec3::new(2.0, 2.0, 0.2),
			_ => Vec3::splat(0.2),
		};
		let centre = origin + Vec3::Y * (height + if light.class == "SurfaceLight" && down { 0.1 } else { 0.0 });
		carrier(&mut model, &format!("Light-{index}"), Affine3A::from_translation(centre), size, light);
		let mut inputs = light_inputs(&light);
		inputs["origin"] = serde_json::json!(origin.to_array());
		inputs["height"] = serde_json::json!(height);
		probes.push(Probe { label: label.clone(), position: origin.into(), inputs });
		runs.push(Run { label, environment, camera: above(origin, 16.0 * 2.2 + 6.0, 50.0) });
	}
	Experiment { name: "light-graze-kinds".into(), model, probes, runs }
}

/// What spared an effect's clash floor the grazing darkening (light-attack 6-clash: its shadowed
/// PointLight of Brightness 100 and Range 25, 3 studs up beside a shadowed one of Range 120 and an
/// unshadowed one of Range 42 on the same part, darkens nothing at 22 studs out where the rule takes
/// 9 %): PointLights 3 studs over a white floor, shadowed at Range 25 and Brightness 1 alone, at
/// Brightness 100 alone (-4 EV), red beside a shadowed blue one of Range 120, red beside an unshadowed
/// green one of Range 42, each unshadowed, red beside a shadowed blue one of Range 25, the clash's
/// three, red with the shadowed Range 120 one on a part of its own 0.5 and 4 studs away, red beside a
/// shadowed one of Range 40, shadowed lights of Range 60, 75, 90, 105 and 120 alone (over floors 144
/// studs across, seen from 90 studs up; Range 120 at Brightness 1, 4 and 10, Range 60 at 1 and 10),
/// and red and the Range 120 one
/// on two Attachments of one part, together and 0.5 studs apart; in the dark, from above, the single
/// lights also from 15 studs up beyond the floor's edge.
pub fn light_graze_pairs() -> Experiment {
	let mut model = ModelBuilder::new("LightGrazePairs");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let red = |shadows: bool, brightness: f32| Light { color: [1.0, 0.0, 0.0], shadows, ..Light::point(brightness, 25.0) };
	let cases: Vec<(&str, Vec<Light>, f32)> = vec![
		("b1", vec![red(true, 1.0)], 0.0),
		("b1-u", vec![red(false, 1.0)], 0.0),
		("b100", vec![red(true, 100.0)], -4.0),
		("b100-u", vec![red(false, 100.0)], -4.0),
		("pair120", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("pair120-u", vec![red(false, 1.0), Light { color: [0.0, 0.0, 1.0], ..Light::point(1.0, 120.0) }], 0.0),
		("pair42", vec![red(true, 1.0), Light { color: [0.0, 1.0, 0.0], ..Light::point(1.0, 42.0) }], 0.0),
		("pair25", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 25.0) }], 0.0),
		("triple", vec![red(true, 1.0), Light { color: [0.0, 1.0, 0.0], ..Light::point(1.0, 42.0) }, Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("apart0.5", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("apart4", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("pair40", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 40.0) }], 0.0),
		("single60", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(1.0, 60.0) }], 0.0),
		("single60-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(1.0, 60.0) }], 0.0),
		("single120", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("single120-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(1.0, 120.0) }], 0.0),
		("single75", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(1.0, 75.0) }], 0.0),
		("single75-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(1.0, 75.0) }], 0.0),
		("single90", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(1.0, 90.0) }], 0.0),
		("single90-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(1.0, 90.0) }], 0.0),
		("single105", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(1.0, 105.0) }], 0.0),
		("single105-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(1.0, 105.0) }], 0.0),
		("single120b10", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(10.0, 120.0) }], -3.0),
		("single120b10-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(10.0, 120.0) }], -3.0),
		("single120b4", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(4.0, 120.0) }], -2.0),
		("single120b4-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(4.0, 120.0) }], -2.0),
		("single60b10", vec![Light { color: [1.0, 0.0, 0.0], shadows: true, ..Light::point(10.0, 60.0) }], -3.0),
		("single60b10-u", vec![Light { color: [1.0, 0.0, 0.0], ..Light::point(10.0, 60.0) }], -3.0),
		("attach-same", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
		("attach-apart", vec![red(true, 1.0), Light { color: [0.0, 0.0, 1.0], shadows: true, ..Light::point(1.0, 120.0) }], 0.0),
	];
	for (index, (label, lights, exposure)) in cases.into_iter().enumerate() {
		let origin = Vec3::new(index as f32 * 300.0, 0.0, 1300.0);
		let half = if label.starts_with("single") { 72.0 } else { 50.0 };
		floor(&mut model, &format!("Floor-{index}"), origin, -half, half, -half, half, 255);
		let part = model.part(&PartSpec { name: format!("Light-{index}"), cframe: Affine3A::from_translation(origin + Vec3::Y * 3.0), size: Vec3::ONE, transparency: 1.0, cast_shadow: false, ..Default::default() });
		// the second light on a part of its own this far along +X (apart*), else on the same part
		let apart = label.strip_prefix("apart").and_then(|gap| gap.parse::<f32>().ok());
		let second = apart.map(|gap| model.part(&PartSpec { name: format!("Light-{index}-b"), cframe: Affine3A::from_translation(origin + Vec3::new(gap, 3.0, 0.0)), size: Vec3::ONE, transparency: 1.0, cast_shadow: false, ..Default::default() }));
		// on two Attachments of the part, at its centre or the second 0.5 studs along +X (attach-*)
		let attachments = label.strip_prefix("attach-").map(|how| {
			let gap = if how == "apart" { 0.5 } else { 0.0 };
			[0.0f32, gap].map(|x| model.child(part, InstanceBuilder::new("Attachment").with_name("Attachment").with_property("CFrame", rbx_dom_weak::types::CFrame::new(rbx_dom_weak::types::Vector3::new(x, 0.0, 0.0), rbx_dom_weak::types::Matrix3::identity()))))
		});
		for (which, light) in lights.iter().enumerate() {
			let holder = match attachments {
				Some(holders) => holders[which.min(1)],
				None if which > 0 => second.unwrap_or(part),
				None => part,
			};
			model.child(
				holder,
				InstanceBuilder::new(light.class)
					.with_name(light.class)
					.with_property("Brightness", light.brightness)
					.with_property("Range", light.range)
					.with_property("Color", Color3::new(light.color[0], light.color[1], light.color[2]))
					.with_property("Shadows", light.shadows)
					.with_property("Enabled", true),
			);
		}
		probes.push(Probe { label: label.into(), position: origin.into(), inputs: serde_json::json!({"origin": origin.to_array(), "height": 3.0, "lights": lights.iter().map(light_inputs).collect::<Vec<_>>()}) });
		let height = if label.starts_with("single") { 90.0 } else { 62.0 };
		runs.push(Run { label: label.into(), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: above(origin, height, 70.0) });
		// the single lights again from low over the floor's -Z edge, as attacks are seen
		if label.starts_with("single") || label.starts_with("b1") {
			let camera = CameraSpec::LookAt { eye: (origin + Vec3::new(0.0, 15.0, -58.0)).into(), target: (origin + Vec3::new(0.0, 0.0, 10.0)).into(), fov: 60.0 };
			runs.push(Run { label: format!("{label}-side").replace("-u-side", "-side-u"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera });
		}
	}
	Experiment { name: "light-graze-pairs".into(), model, probes, runs }
}
