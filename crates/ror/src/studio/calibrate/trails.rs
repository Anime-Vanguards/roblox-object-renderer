//! Trail experiments (notes/render.md "Trails"): trails on invisible mover parts that a timeline
//! moves (studio::calibrate::timeline), white, opaque and unlit on black unless a case says
//! otherwise; Studio's frame and ror's drawing of it at the frame's exact time.

use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, ContentId, Enum, NumberSequence, NumberSequenceKeypoint, Variant};

use super::timeline::{Timeline, register};
use super::{Experiment, Run, dark};
use crate::render::camera::CameraSpec;
use crate::scene::build::{ModelBuilder, PartSpec, cframe};
use crate::scene::playback::{Action, Motion, MotionKey, Playback};
use crate::scene::Environment;

/// Trail properties a case sets.
type Props = Vec<(&'static str, Variant)>;

const ARROW: &str = "rbxasset://textures/ArrowCursor.png";
const BLOCK: &str = "rbxasset://textures/blockUpperLeft.png";

fn numbers(keys: &[(f32, f32)]) -> Variant {
	NumberSequence { keypoints: keys.iter().map(|(time, value)| NumberSequenceKeypoint::new(*time, *value, 0.0)).collect() }.into()
}

fn constant(value: f32) -> Variant {
	numbers(&[(0.0, value), (1.0, value)])
}

fn colors(keys: &[(f32, [f32; 3])]) -> Variant {
	ColorSequence { keypoints: keys.iter().map(|(time, c)| ColorSequenceKeypoint::new(*time, Color3::new(c[0], c[1], c[2]))).collect() }.into()
}

/// A trail on an invisible mover part at `at`: its attachments at `a0` and `a1` in the part's frame,
/// white, opaque, unlit, Brightness 1, Lifetime 100 and MinLength 0 unless `set` overrides.
fn add_trail(model: &mut ModelBuilder, name: &str, at: Affine3A, a0: Vec3, a1: Vec3, set: &[(&str, Variant)]) {
	let part = model.part(&PartSpec { name: name.into(), cframe: at, size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
	let attachment0 = model.child(part, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", cframe(Affine3A::from_translation(a0))));
	let attachment1 = model.child(part, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", cframe(Affine3A::from_translation(a1))));
	let mut trail = InstanceBuilder::new("Trail")
		.with_name("Trail")
		.with_property("Attachment0", Variant::Ref(attachment0))
		.with_property("Attachment1", Variant::Ref(attachment1))
		.with_property("Color", colors(&[(0.0, [1.0, 1.0, 1.0]), (1.0, [1.0, 1.0, 1.0])]))
		.with_property("Transparency", constant(0.0))
		.with_property("WidthScale", constant(1.0))
		.with_property("Brightness", 1.0f32)
		.with_property("LightEmission", 0.0f32)
		.with_property("LightInfluence", 0.0f32)
		.with_property("Lifetime", 100.0f32)
		.with_property("MinLength", 0.0f32)
		.with_property("MaxLength", 0.0f32)
		.with_property("FaceCamera", false)
		.with_property("Enabled", true);
	for (property, value) in set {
		let property = if *property == "Texture" { "TextureContent" } else { *property };
		let value = match (property, value) {
			("TextureContent", Variant::ContentId(uri)) => rbx_dom_weak::types::Content::from_uri(uri.as_str()).into(),
			_ => value.clone(),
		};
		trail = trail.with_property(property, value);
	}
	model.child(part, trail);
}

/// Relative keys: a straight move of `distance` along the mover's X axis over `seconds`.
fn line(distance: f32, seconds: f32) -> Vec<MotionKey> {
	vec![MotionKey { at: 0.0, cframe: vec![0.0, 0.0, 0.0], ..Default::default() }, MotionKey { at: seconds, cframe: vec![distance, 0.0, 0.0], ..Default::default() }]
}

/// Relative keys along `turns` of a circle of `radius` in the mover's XY plane, from its start point
/// (the circle's centre `radius` above it, counterclockwise seen from -Z), one key a 1/60 s.
fn circle(radius: f32, turns: f32, seconds: f32) -> Vec<MotionKey> {
	let count = (seconds * 60.0).round().max(1.0) as usize;
	(0..=count)
		.map(|index| {
			let f = index as f32 / count as f32;
			let angle = -std::f32::consts::FRAC_PI_2 + f * turns * std::f32::consts::TAU;
			MotionKey { at: f * seconds, cframe: vec![radius * angle.cos(), radius + radius * angle.sin(), 0.0], ..Default::default() }
		})
		.collect()
}

/// Black floor and wall behind the cases (unlit in the dark environment), and behind each case a
/// panel black on its left (+X) half and white on its right (blends show over both).
fn backdrop(model: &mut ModelBuilder, centers: &[Vec3], panels: bool) {
	if panels {
		for (index, center) in centers.iter().enumerate() {
			for (side, color) in [(1.0f32, [0u8, 0, 0]), (-1.0, [255, 255, 255])] {
				model.part(&PartSpec { name: format!("Panel{index}"), cframe: Affine3A::from_translation(*center + Vec3::new(side * 6.0, 0.0, 8.0)), size: Vec3::new(12.0, 14.0, 0.5), color, cast_shadow: false, ..Default::default() });
			}
		}
	}
	let middle = centers.iter().fold(Vec3::ZERO, |sum, center| sum + *center) / centers.len().max(1) as f32;
	let span = centers.iter().map(|center| (center.x - middle.x).abs()).fold(0.0, f32::max) * 2.0 + 100.0;
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(middle + Vec3::new(0.0, -12.0, 0.0)), size: Vec3::new(span, 1.0, 200.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(middle + Vec3::new(0.0, 0.0, 14.0)), size: Vec3::new(span, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
}

/// One case of a trail experiment: the trail's attachments and properties, the mover's motion, the
/// moment captured and the actions (enabled, Clear) on the way, and the views.
struct Case {
	label: &'static str,
	a0: Vec3,
	a1: Vec3,
	set: Props,
	keys: Vec<MotionKey>,
	time: f32,
	actions: Vec<(f32, Option<bool>, bool)>,
	views: Vec<(&'static str, Vec3)>,
	environment: Environment,
}

fn front() -> Vec<(&'static str, Vec3)> {
	vec![("front", Vec3::new(0.0, 0.0, -30.0))]
}

fn case(label: &'static str, set: Props) -> Case {
	Case { label, a0: Vec3::new(0.0, 0.5, 0.0), a1: Vec3::new(0.0, -0.5, 0.0), set, keys: line(8.0, 0.5), time: 0.8, actions: Vec::new(), views: front(), environment: dark() }
}

/// Builds an experiment of trail cases 40 studs apart (each mover starting 4 studs left of its
/// case's centre): every run plays all the cases' motions, as the model does in Studio.
fn experiment(name: &str, model_name: &str, cases: Vec<Case>, panels: bool) -> Experiment {
	let mut model = ModelBuilder::new(model_name);
	let centers: Vec<Vec3> = (0..cases.len()).map(|index| Vec3::new(index as f32 * 40.0, 0.0, 0.0)).collect();
	let mut motion = Vec::new();
	for (case, center) in cases.iter().zip(&centers) {
		add_trail(&mut model, case.label, Affine3A::from_translation(*center + Vec3::new(-4.0, 0.0, 0.0)), case.a0, case.a1, &case.set);
		motion.push(Motion { target: format!("{model_name}.{}", case.label), keys: case.keys.clone(), relative: true, tagged: None, physics_from: None });
	}
	backdrop(&mut model, &centers, panels);
	let mut runs = Vec::new();
	for (case, center) in cases.iter().zip(&centers) {
		let actions: Vec<Action> = case
			.actions
			.iter()
			.map(|&(at, enabled, clear)| Action { at, target: format!("{model_name}.{}.Trail", case.label), enabled, clear, ..Default::default() })
			.collect();
		for (view, offset) in &case.views {
			let label = if case.views.len() > 1 { format!("{}-{view}", case.label) } else { case.label.to_string() };
			runs.push(Run { label: label.clone(), environment: case.environment.clone(), camera: CameraSpec::LookAt { eye: (*center + *offset).into(), target: (*center).into(), fov: 30.0 } });
			let playback = Playback { time: case.time, motion: motion.clone(), actions: actions.clone(), ..Default::default() };
			register(name, &label, Timeline { playback, duration: case.time + 0.25 });
		}
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Where a trail lies: between its attachments (moving along X with them at ±0.5 Y), FaceCamera
/// (attachments along the view: how wide, which way), WidthScale about which point, a circle (one
/// point a frame?), MinLength and MaxLength, Enabled off and on, Clear.
pub fn trail_shape() -> Experiment {
	let toward = (Vec3::new(0.0, 0.0, 0.5), Vec3::new(0.0, 0.0, -0.5));
	let mut cases = vec![case("straight", vec![])];
	cases.push(Case { a0: toward.0, a1: toward.1, views: vec![("front", Vec3::new(0.0, 0.0, -30.0)), ("above", Vec3::new(0.0, 25.0, -18.0))], ..case("facecamera", vec![("FaceCamera", true.into())]) });
	cases.push(Case { a0: Vec3::new(0.0, 1.0, 0.0), a1: Vec3::ZERO, ..case("widthscale", vec![("WidthScale", constant(0.5))]) });
	cases.push(Case { a0: Vec3::new(0.0, 1.0, 0.0), a1: Vec3::ZERO, ..case("widthscale-facecamera", vec![("WidthScale", constant(0.5)), ("FaceCamera", true.into())]) });
	cases.push(Case { a0: Vec3::new(0.0, 0.25, 0.0), a1: Vec3::new(0.0, -0.25, 0.0), keys: circle(4.0, 0.75, 1.0), time: 1.3, ..case("circle", vec![]) });
	cases.push(Case { a0: Vec3::new(0.0, 0.25, 0.0), a1: Vec3::new(0.0, -0.25, 0.0), keys: circle(4.0, 0.75, 1.0), time: 1.3, ..case("minlength", vec![("MinLength", 2.0f32.into())]) });
	cases.push(Case { keys: line(8.0, 0.5), ..case("maxlength", vec![("MaxLength", 3.0f32.into())]) });
	cases.push(Case { keys: line(8.0, 1.0), time: 1.3, actions: vec![(0.4, Some(false), false), (0.6, Some(true), false)], ..case("gap", vec![]) });
	cases.push(Case { keys: line(8.0, 1.0), time: 1.3, actions: vec![(0.5, None, true)], ..case("clear", vec![]) });
	cases.push(Case { keys: line(8.0, 0.5), time: 0.25, ..case("moving", vec![]) });
	experiment("trail-shape", "TrailShape", cases, false)
}

/// Trail shading as beams': Brightness, a colour, Transparency and LightEmission over a backdrop
/// black on the left and white on the right, LightInfluence under flat ambient and the sun.
pub fn trail_shading() -> Experiment {
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let sun = Environment { brightness: 2.0, ..dark() };
	let cases = vec![
		case("white", vec![]),
		case("brightness3", vec![("Brightness", 3.0f32.into())]),
		case("orange", vec![("Color", colors(&[(0.0, [1.0, 0.5, 0.0]), (1.0, [1.0, 0.5, 0.0])]))]),
		case("half", vec![("Transparency", constant(0.5))]),
		case("additive", vec![("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into())]),
		case("negative", vec![("Transparency", constant(0.5)), ("LightEmission", (-4.0f32).into())]),
		Case { environment: flat, ..case("lit-flat", vec![("LightInfluence", 1.0f32.into())]) },
		Case { environment: sun, ..case("lit-sun", vec![("LightInfluence", 1.0f32.into())]) },
	];
	experiment("trail-shading", "TrailShading", cases, true)
}

/// Trail textures: the arrow pointer (asymmetric both ways) and blockUpperLeft in each TextureMode
/// and two TextureLengths, still and while moving, with FaceCamera.
pub fn trail_texture() -> Experiment {
	let texture = |uri: &str, mode: u32, length: f32| -> Props { vec![("Texture", ContentId::from(uri).into()), ("TextureMode", Enum::from_u32(mode).into()), ("TextureLength", length.into())] };
	let tall = (Vec3::new(0.0, 1.5, 0.0), Vec3::new(0.0, -1.5, 0.0));
	let cases = vec![
		Case { a0: tall.0, a1: tall.1, ..case("stretch-1", texture(ARROW, 0, 1.0)) },
		Case { a0: tall.0, a1: tall.1, ..case("stretch-2", texture(ARROW, 0, 2.0)) },
		Case { a0: tall.0, a1: tall.1, ..case("wrap-3", texture(ARROW, 1, 3.0)) },
		Case { a0: tall.0, a1: tall.1, ..case("static-3", texture(ARROW, 2, 3.0)) },
		Case { a0: tall.0, a1: tall.1, time: 0.3, ..case("wrap-3-moving", texture(ARROW, 1, 3.0)) },
		Case { a0: tall.0, a1: tall.1, time: 0.3, ..case("static-3-moving", texture(ARROW, 2, 3.0)) },
		Case { a0: tall.0, a1: tall.1, time: 0.3, ..case("stretch-1-moving", texture(ARROW, 0, 1.0)) },
		Case { a0: tall.0, a1: tall.1, ..case("block-stretch", texture(BLOCK, 0, 1.0)) },
		Case { a0: Vec3::new(0.0, 0.0, 1.5), a1: Vec3::new(0.0, 0.0, -1.5), ..case("facecamera", { let mut set = texture(ARROW, 0, 1.0); set.push(("FaceCamera", true.into())); set }) },
	];
	experiment("trail-texture", "TrailTexture", cases, false)
}

/// Trails over their life: Lifetime (the older part gone), Transparency, Color and WidthScale over a
/// point's life (one sequence each, a 2 s trail laid at 4 studs a second), and a transparency spike
/// (how the sequences are sampled).
pub fn trail_life() -> Experiment {
	let long = |label: &'static str, set: Props| Case { keys: line(8.0, 2.0), time: 2.0, a0: Vec3::new(0.0, 0.5, 0.0), a1: Vec3::new(0.0, -0.5, 0.0), ..case(label, set) };
	let mut cases = vec![Case { keys: line(8.0, 1.0), time: 1.0, ..case("lifetime", vec![("Lifetime", 0.5f32.into())]) }];
	cases.push(Case { keys: line(8.0, 1.0), time: 1.4, ..case("lifetime-after", vec![("Lifetime", 0.5f32.into())]) });
	cases.push(long("fade", vec![("Lifetime", 2.0f32.into()), ("Transparency", numbers(&[(0.0, 0.0), (1.0, 1.0)]))]));
	cases.push(long("color", vec![("Lifetime", 2.0f32.into()), ("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])]))]));
	cases.push(long("width", vec![("Lifetime", 2.0f32.into()), ("WidthScale", numbers(&[(0.0, 1.0), (1.0, 0.0)]))]));
	cases.push(long("spike", vec![("Lifetime", 2.0f32.into()), ("Transparency", numbers(&[(0.0, 1.0), (0.45, 1.0), (0.5, 0.0), (0.55, 1.0), (1.0, 1.0)]))]));
	experiment("trail-life", "TrailLife", cases, false)
}
