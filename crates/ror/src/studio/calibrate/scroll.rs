//! Beams over time (notes/render.md "Beams"): TextureSpeed scrolling the texture, played by a timeline
//! (studio::calibrate::timeline); Studio's frame against ror's drawing of it at the frame's time.

use glam::{Affine3A, Vec3};
use rbx_dom_weak::types::{ContentId, Enum, Variant};

use super::timeline::{Timeline, register};
use super::{BeamProperties, Experiment, Run, add_beam, dark};
use crate::render::camera::CameraSpec;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::playback::{Action, Playback, Tween, TweenKey};

/// The arrow pointer: asymmetric both ways, so a shift of the pattern shows which way and how far.
const ARROW: &str = "rbxasset://textures/ArrowCursor.png";

/// The beams' rows, from the top: (label, TextureMode, TextureLength, TextureSpeed, enabled at first).
const ROWS: [(&str, u32, f32, f32, bool); 10] = [
	("wrap-1", 1, 3.0, 1.0, true),
	("wrap-half", 1, 3.0, 0.5, true),
	("wrap-minus2", 1, 3.0, -2.0, true),
	("stretch-1", 0, 1.0, 1.0, true),
	("stretch-2-quarter", 0, 2.0, 0.25, true),
	("static-1", 2, 3.0, 1.0, true),
	("late", 1, 3.0, 1.0, false),
	("still", 1, 3.0, 0.0, true),
	("toggle", 1, 3.0, 0.5, true),
	("tween", 1, 3.0, 0.0, true),
];

/// Scrolling: rows of 12-stud beams with the arrow pointer, white and unlit on black, seen from the
/// front, at three moments: how far TextureSpeed has moved the image (cycles per second, which way,
/// from when), in Wrap, Stretch and Static; a beam enabled late, one off from 0.2 to 0.45 s (does it
/// keep its place?) and one whose TextureSpeed is tweened from 0 to 3 over 0.6 s.
pub fn beam_scroll() -> Experiment {
	let name = "beam-scroll";
	let model_name = "BeamScroll";
	let mut model = ModelBuilder::new(model_name);
	for (index, (label, mode, length, speed, enabled)) in ROWS.iter().enumerate() {
		let set: BeamProperties = vec![
			("Texture", ContentId::from(ARROW).into()),
			("TextureMode", Enum::from_u32(*mode).into()),
			("TextureLength", (*length).into()),
			("TextureSpeed", (*speed).into()),
			("Width0", 1.4f32.into()),
			("Width1", 1.4f32.into()),
			("Enabled", Variant::Bool(*enabled)),
		];
		let y = (ROWS.len() as f32 - 1.0) - index as f32 * 2.0;
		add_beam(&mut model, label, Vec3::new(0.0, y, 0.0), Affine3A::from_translation(Vec3::new(-6.0, 0.0, 0.0)), Affine3A::from_translation(Vec3::new(6.0, 0.0, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 12.0)), size: Vec3::new(200.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let beam = |label: &str| format!("{model_name}.{label}.Beam");
	let switch = |at: f32, label: &str, enabled: bool| Action { at, target: beam(label), enabled: Some(enabled), ..Default::default() };
	let actions = vec![switch(0.25, "late", true), switch(0.2, "toggle", false), switch(0.45, "toggle", true)];
	let key = |at: f32, value: f32| TweenKey { at, value: serde_json::json!(value), easing: Some("Linear".into()), direction: Some("Out".into()) };
	let tweens = vec![Tween { target: beam("tween"), property: "TextureSpeed".into(), keys: vec![key(0.0, 0.0), key(0.6, 3.0)], ..Default::default() }];
	let mut runs = Vec::new();
	for moment in [0.12f32, 0.37, 0.71] {
		let label = format!("t{moment}");
		runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -42.0], target: [0.0, 0.0, 0.0], fov: 30.0 } });
		let playback = Playback { time: moment, actions: actions.clone(), tweens: tweens.clone(), ..Default::default() };
		register(name, &label, Timeline { playback, duration: moment + 0.25 });
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}
