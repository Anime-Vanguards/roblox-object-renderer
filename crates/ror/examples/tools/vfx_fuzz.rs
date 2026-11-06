// Random VFX scenes drawn small, each against the same scene without its effects, to find what breaks:
// panics, errors, slow renders, a scene drawn differently twice (another scene drawn between: what
// render::reuse keeps), and black pixels where nothing black was drawn (a NaN reaching the image).
// Particles, beams, trails (moved by a playback), local lights, decals and textures, Glass, Neon and
// ForceField parts and Highlights, each property from its usual range or its edges (0, negative,
// huge, infinite, NaN), combined at random.
//   cargo run -p ror --release --example tools -- vfx_fuzz [cases 300] [seed 1] [out dir] [garbage|extreme|sane]
// garbage (the default) takes NaN and infinities too and reports panics, errors, slow renders and
// drawings that differ; extreme keeps to finite edges and also reports black pixels; sane keeps to the
// properties' usual ranges, every effect enabled, and also reports an effect that draws nothing.
// One line per finding; each finding's model, playback and images go to the out dir.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Content, Enum, NumberRange, NumberSequence, NumberSequenceKeypoint, Ref, Variant, Vector2, Vector3};
use ror::render::camera::CameraSpec;
use ror::render::{Renderer, ViewRequest};
use ror::scene::build::{ModelBuilder, PartSpec, cframe};

const SIZE: u32 = 160;
const SLOW_SECONDS: f64 = 5.0;
const TEXTURES: [&str; 5] = [
	"rbxasset://textures/particles/sparkles_main.dds",
	"rbxasset://textures/particles/smoke_main.dds",
	"rbxasset://textures/particles/fire_main.dds",
	"rbxasset://textures/particles/SquareParticle.png",
	"rbxasset://textures/particles/forcefield_glow_main.dds",
];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
	Garbage,
	Extreme,
	Sane,
}

/// splitmix64, and how far its values reach
struct Rng(u64, Mode);

impl Rng {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
		z ^ (z >> 31)
	}

	fn unit(&mut self) -> f32 {
		(self.next() >> 40) as f32 / (1u64 << 24) as f32
	}

	fn chance(&mut self, p: f32) -> bool {
		self.unit() < p
	}

	fn pick<T: Copy>(&mut self, items: &[T]) -> T {
		items[(self.next() % items.len() as u64) as usize]
	}

	fn between(&mut self, low: f32, high: f32) -> f32 {
		low + (high - low) * self.unit()
	}

	/// A value from `low..high` most of the time, else one of the edges a float property can take
	fn value(&mut self, low: f32, high: f32) -> f32 {
		let usual = self.chance(0.75);
		match self.1 {
			_ if usual => self.between(low, high),
			Mode::Sane => self.between(low, high),
			Mode::Extreme => self.pick(&[0.0, -1.0, -0.5, 1.0, 2.0, 1e-6, -1e-6, 50.0, -50.0, 1e4, -1e4, 1e6, -1e6]),
			Mode::Garbage => self.pick(&[0.0, -1.0, -0.5, 1.0, 2.0, 1e-6, -1e-6, 50.0, -50.0, 1e4, -1e4, 1e9, -1e9, f32::INFINITY, f32::NEG_INFINITY, f32::NAN]),
		}
	}

	/// An on/off property: on with probability `p` (always in the sane mode)
	fn on(&mut self, p: f32) -> bool {
		self.1 == Mode::Sane || self.chance(p)
	}

	/// A colour channel: 0..1, above 1 (bright tricks), or below 0
	fn channel(&mut self) -> f32 {
		match (self.next() % 10, self.1) {
			(0..=6, _) => self.unit(),
			(7, _) => self.between(1.0, 5.0),
			(8, Mode::Sane) => self.between(5.0, 20.0),
			(8, _) => self.between(-5.0, 0.0),
			(_, Mode::Sane) => self.unit(),
			(_, Mode::Extreme) => self.pick(&[0.0, 1.0, 100.0, -100.0]),
			(_, Mode::Garbage) => self.pick(&[0.0, 1.0, 1e6, -1e6, f32::NAN]),
		}
	}

	fn color(&mut self) -> Color3 {
		Color3::new(self.channel(), self.channel(), self.channel())
	}

	fn enum_value(&mut self, count: u32) -> Enum {
		// now and then a value past the enum's end
		Enum::from_u32(if self.1 != Mode::Sane && self.chance(0.05) { count + (self.next() % 5) as u32 } else { (self.next() % count as u64) as u32 })
	}
}

fn number_sequence(rng: &mut Rng, low: f32, high: f32) -> Variant {
	let count = 2 + (rng.next() % 3) as usize;
	let mut times: Vec<f32> = (0..count).map(|index| if index == 0 { 0.0 } else if index == count - 1 { 1.0 } else { rng.unit() }).collect();
	times.sort_by(f32::total_cmp);
	let keypoints = times.iter().map(|&time| NumberSequenceKeypoint::new(time, rng.value(low, high), if rng.chance(0.3) { rng.value(0.0, (high - low) * 0.5) } else { 0.0 })).collect();
	NumberSequence { keypoints }.into()
}

/// A Transparency sequence: from below 0 to past 1, or (sane) mostly seen
fn transparency(rng: &mut Rng) -> Variant {
	if rng.1 == Mode::Sane { number_sequence(rng, 0.0, 0.6) } else { number_sequence(rng, -0.5, 1.2) }
}

fn color_sequence(rng: &mut Rng) -> Variant {
	let count = 2 + (rng.next() % 2) as usize;
	let keypoints = (0..count).map(|index| ColorSequenceKeypoint::new(index as f32 / (count - 1) as f32, rng.color())).collect();
	ColorSequence { keypoints }.into()
}

fn range(rng: &mut Rng, low: f32, high: f32) -> NumberRange {
	let (a, b) = (rng.value(low, high), rng.value(low, high));
	NumberRange::new(a.min(b), a.max(b))
}

fn texture(rng: &mut Rng) -> Option<Variant> {
	rng.chance(0.7).then(|| Content::from_uri(rng.pick(&TEXTURES)).into())
}

/// The case: what it holds (for the report), and whether the trail needs its carrier moved.
struct Case {
	model: ModelBuilder,
	parts: Vec<String>,
	moves: Vec<String>,
	time: f32,
	emit: bool,
}

/// The part an effect is on: turned at random unless `upright`, invisible half the time unless `seen`
fn carrier(rng: &mut Rng, model: &mut ModelBuilder, name: &str, position: Vec3, upright: bool, seen: bool) -> Ref {
	let material = if seen { rng.pick(&[272u32, 256, 288, 800]) } else { rng.pick(&[272u32, 256, 288, 1568, 1584, 800]) };
	let transparency = if seen { rng.between(0.0, 0.5) } else if rng.chance(0.5) { 1.0 } else { rng.value(0.0, 1.0) };
	let turn = if upright { glam::Quat::IDENTITY } else { glam::Quat::from_euler(glam::EulerRot::YXZ, rng.between(-3.0, 3.0), rng.between(-1.5, 1.5), rng.between(-3.0, 3.0)) };
	model.part(&PartSpec {
		name: name.into(),
		cframe: Affine3A::from_rotation_translation(turn, position),
		size: Vec3::new(rng.between(0.2, 3.0), rng.between(0.2, 3.0), rng.between(0.2, 3.0)),
		color: [(rng.next() % 256) as u8, (rng.next() % 256) as u8, (rng.next() % 256) as u8],
		material,
		shape: Some(rng.pick(&[0u32, 1, 2])),
		transparency,
		..Default::default()
	})
}

fn attachment(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str) -> Ref {
	turned_attachment(rng, model, parent, name, true)
}

fn turned_attachment(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str, turned: bool) -> Ref {
	let offset = Vec3::new(rng.between(-2.0, 2.0), rng.between(-2.0, 2.0), rng.between(-2.0, 2.0));
	let turn = if turned { glam::Quat::from_euler(glam::EulerRot::YXZ, rng.between(-3.0, 3.0), rng.between(-3.0, 3.0), rng.between(-3.0, 3.0)) } else { glam::Quat::IDENTITY };
	model.child(parent, InstanceBuilder::new("Attachment").with_name(name).with_property("CFrame", cframe(Affine3A::from_rotation_translation(turn, offset))))
}

fn particles(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str) {
	let mut emitter = InstanceBuilder::new("ParticleEmitter")
		.with_name(name)
		.with_property("Enabled", rng.on(0.85))
		.with_property("Rate", rng.value(if rng.1 == Mode::Sane { 5.0 } else { 0.0 }, 200.0))
		.with_property("Lifetime", if rng.1 == Mode::Sane { range(rng, 0.3, 4.0) } else { range(rng, 0.1, 4.0) })
		.with_property("Speed", if rng.1 == Mode::Sane { range(rng, 0.5, 10.0) } else { range(rng, -5.0, 10.0) })
		.with_property("SpreadAngle", Vector2::new(rng.value(0.0, 360.0), rng.value(0.0, 360.0)))
		.with_property("Acceleration", Vector3::new(rng.value(-10.0, 10.0), rng.value(-10.0, 10.0), rng.value(-10.0, 10.0)))
		.with_property("Drag", rng.value(0.0, 10.0))
		.with_property("VelocityInheritance", rng.value(0.0, 1.0))
		.with_property("LockedToPart", rng.chance(0.3))
		.with_property("EmissionDirection", rng.enum_value(6))
		.with_property("Shape", rng.enum_value(4))
		.with_property("ShapeInOut", rng.enum_value(3))
		.with_property("ShapeStyle", rng.enum_value(2))
		.with_property("ShapePartial", rng.value(-1.0, 2.0))
		.with_property("Rotation", range(rng, -360.0, 360.0))
		.with_property("RotSpeed", range(rng, -360.0, 360.0))
		.with_property("Size", number_sequence(rng, if rng.1 == Mode::Sane { 0.3 } else { 0.0 }, 2.0))
		.with_property("Squash", number_sequence(rng, -3.0, 3.0))
		.with_property("Transparency", transparency(rng))
		.with_property("Color", color_sequence(rng))
		.with_property("LightEmission", rng.value(if rng.1 == Mode::Sane { 0.0 } else { -2.0 }, 1.0))
		.with_property("LightInfluence", rng.value(0.0, 1.0))
		.with_property("Brightness", rng.value(if rng.1 == Mode::Sane { 0.5 } else { 0.0 }, 20.0))
		.with_property("Orientation", rng.enum_value(4))
		.with_property("ZOffset", rng.value(-5.0, 5.0))
		.with_property("TimeScale", rng.value(if rng.1 == Mode::Sane { 0.2 } else { 0.0 }, 2.0))
		.with_property("FlipbookLayout", rng.enum_value(5))
		.with_property("FlipbookMode", rng.enum_value(4))
		.with_property("FlipbookFramerate", range(rng, 0.0, 60.0))
		.with_property("FlipbookStartRandom", rng.chance(0.3))
		.with_property("FlipbookSizeX", if rng.chance(0.9) { (1 + rng.next() % 8) as i32 } else { rng.pick(&[0, -3, 1 << 16, i32::MAX]) })
		.with_property("FlipbookSizeY", if rng.chance(0.9) { (1 + rng.next() % 8) as i32 } else { rng.pick(&[0, -3, 1 << 16, i32::MAX]) });
	if let Some(texture) = texture(rng) {
		emitter = emitter.with_property("TextureContent", texture);
	}
	model.child(parent, emitter);
}

fn beam(rng: &mut Rng, model: &mut ModelBuilder, from: Ref, to: Ref, name: &str) {
	let parent = from;
	let mut builder = InstanceBuilder::new("Beam")
		.with_name(name)
		.with_property("Attachment0", Variant::Ref(from))
		.with_property("Attachment1", Variant::Ref(to))
		.with_property("Enabled", rng.on(0.9))
		.with_property("Color", color_sequence(rng))
		.with_property("Transparency", transparency(rng))
		.with_property("Brightness", rng.value(0.0, 20.0))
		.with_property("LightEmission", rng.value(-2.0, 1.0))
		.with_property("LightInfluence", rng.value(0.0, 1.0))
		.with_property("TextureLength", rng.value(0.1, 4.0))
		.with_property("TextureMode", rng.enum_value(3))
		.with_property("TextureSpeed", rng.value(-2.0, 2.0))
		.with_property("Width0", rng.value(if rng.1 == Mode::Sane { 0.3 } else { 0.0 }, 3.0))
		.with_property("Width1", rng.value(if rng.1 == Mode::Sane { 0.3 } else { 0.0 }, 3.0))
		.with_property("CurveSize0", rng.value(-5.0, 5.0))
		.with_property("CurveSize1", rng.value(-5.0, 5.0))
		.with_property("Segments", if rng.chance(0.85) { (1 + rng.next() % 40) as i32 } else { rng.pick(&[0, -5, 1000, 100_000, i32::MAX]) })
		.with_property("FaceCamera", rng.on(0.5))
		.with_property("ZOffset", rng.value(-5.0, 5.0));
	if let Some(texture) = texture(rng) {
		builder = builder.with_property("TextureContent", texture);
	}
	model.child(parent, builder);
}

fn trail(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, from: Ref, to: Ref, name: &str) {
	let mut builder = InstanceBuilder::new("Trail")
		.with_name(name)
		.with_property("Attachment0", Variant::Ref(from))
		.with_property("Attachment1", Variant::Ref(to))
		.with_property("Enabled", rng.on(0.9))
		.with_property("Color", color_sequence(rng))
		.with_property("Transparency", transparency(rng))
		.with_property("WidthScale", number_sequence(rng, if rng.1 == Mode::Sane { 0.3 } else { -0.5 }, 2.0))
		.with_property("Brightness", rng.value(0.0, 20.0))
		.with_property("LightEmission", rng.value(-2.0, 1.0))
		.with_property("LightInfluence", rng.value(0.0, 1.0))
		.with_property("Lifetime", rng.value(if rng.1 == Mode::Sane { 0.3 } else { 0.0 }, 3.0))
		.with_property("MinLength", rng.value(0.0, if rng.1 == Mode::Sane { 0.2 } else { 1.0 }))
		.with_property("MaxLength", rng.value(0.0, 20.0))
		.with_property("FaceCamera", rng.chance(0.5))
		.with_property("TextureLength", rng.value(0.1, 4.0))
		.with_property("TextureMode", rng.enum_value(3));
	if let Some(texture) = texture(rng) {
		builder = builder.with_property("TextureContent", texture);
	}
	model.child(parent, builder);
}

fn light(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str) {
	let class = rng.pick(&["PointLight", "SpotLight", "SurfaceLight"]);
	model.child(
		parent,
		InstanceBuilder::new(class)
			.with_name(name)
			.with_property("Enabled", rng.on(0.9))
			.with_property("Color", rng.color())
			.with_property("Brightness", rng.value(if rng.1 == Mode::Sane { 1.0 } else { 0.0 }, 10.0))
			.with_property("Range", rng.value(if rng.1 == Mode::Sane { 8.0 } else { 0.0 }, 30.0))
			.with_property("Angle", rng.value(if rng.1 == Mode::Sane { 30.0 } else { 0.0 }, 180.0))
			.with_property("Face", if rng.1 == Mode::Sane { Enum::from_u32(4) } else { rng.enum_value(6) })
			.with_property("Shadows", rng.chance(0.5)),
	);
}

fn decal(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str, face: Option<u32>) {
	let class = rng.pick(&["Decal", "Texture"]);
	let face = match face {
		Some(face) => Enum::from_u32(face),
		None => rng.enum_value(6),
	};
	let mut builder = InstanceBuilder::new(class)
		.with_name(name)
		.with_property("TextureContent", Content::from_uri(rng.pick(&TEXTURES)))
		.with_property("Face", face)
		.with_property("Color3", rng.color())
		.with_property("Transparency", rng.value(if rng.1 == Mode::Sane { 0.0 } else { -2.0 }, if rng.1 == Mode::Sane { 0.6 } else { 1.2 }))
		.with_property("ZIndex", if rng.chance(0.9) { (rng.next() % 5) as i32 } else { rng.pick(&[i32::MIN, -1, i32::MAX]) });
	if class == "Texture" {
		builder = builder
			.with_property("StudsPerTileU", rng.value(0.1, 4.0))
			.with_property("StudsPerTileV", rng.value(0.1, 4.0))
			.with_property("OffsetStudsU", rng.value(-4.0, 4.0))
			.with_property("OffsetStudsV", rng.value(-4.0, 4.0));
	}
	model.child(parent, builder);
}

fn highlight(rng: &mut Rng, model: &mut ModelBuilder, parent: Ref, name: &str) {
	model.child(
		parent,
		InstanceBuilder::new("Highlight")
			.with_name(name)
			.with_property("Enabled", rng.on(0.8))
			.with_property("FillColor", rng.color())
			.with_property("OutlineColor", rng.color())
			.with_property("FillTransparency", if rng.1 == Mode::Sane { rng.between(0.0, 0.8) } else { rng.value(-1.0, 1.2) })
			.with_property("OutlineTransparency", if rng.1 == Mode::Sane { rng.between(0.0, 0.8) } else { rng.value(-1.0, 1.2) })
			.with_property("DepthMode", rng.enum_value(2)),
	);
}

fn build(rng: &mut Rng) -> Case {
	let mut model = ModelBuilder::new("Fuzz");
	let root = model.model;
	// a lit floor and a backdrop, the same in every case
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(40.0, 1.0, 40.0), color: [200, 200, 200], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 6.0, 8.0)), size: Vec3::new(40.0, 14.0, 1.0), color: [90, 110, 140], ..Default::default() });
	let mut parts = Vec::new();
	let mut moves = Vec::new();
	let sane = rng.1 == Mode::Sane;
	let effects = 1 + (rng.next() % 3) as usize;
	for index in 0..effects {
		let position = Vec3::new(rng.between(-3.0, 3.0), rng.between(1.0, 5.0), rng.between(-2.0, 3.0));
		let name = format!("Carrier{index}");
		let kind = rng.pick(&["particles", "beam", "trail", "light", "decal", "glass", "highlight"]);
		let part = carrier(rng, &mut model, &name, position, sane && kind == "light", sane && matches!(kind, "decal" | "highlight"));
		parts.push(format!("{name}:{kind}"));
		match kind {
			"particles" => {
				let parent = if rng.chance(0.5) { attachment(rng, &mut model, part, "Emitter") } else { part };
				particles(rng, &mut model, parent, "Particles");
			}
			"beam" => {
				let a = attachment(rng, &mut model, part, "A0");
				let b = attachment(rng, &mut model, part, "A1");
				let to = if !sane && rng.chance(0.1) { a } else { b };
				beam(rng, &mut model, a, to, "Beam");
			}
			"trail" => {
				let a = attachment(rng, &mut model, part, "T0");
				let b = attachment(rng, &mut model, part, "T1");
				let to = if !sane && rng.chance(0.1) { a } else { b };
				trail(rng, &mut model, part, a, to, "Trail");
				moves.push(format!("Fuzz.{name}"));
			}
			"light" => {
				let parent = if rng.chance(0.4) { turned_attachment(rng, &mut model, part, "Light", !sane) } else { part };
				light(rng, &mut model, parent, "Light");
			}
			"decal" => {
				// sane: one on every face, so some face the camera
				let count = if sane { 6 } else { 1 + rng.next() % 3 };
				for face in 0..count {
					decal(rng, &mut model, part, &format!("Decal{face}"), sane.then_some(face as u32));
				}
			}
			"glass" => {
				model.set(part, "Material", Enum::from_u32(1568));
				let glass = if sane { rng.between(0.0, 0.95) } else { rng.value(0.0, 3.0) };
				model.set(part, "Transparency", glass);
				if rng.chance(0.5) {
					highlight(rng, &mut model, part, "Highlight");
				}
				if rng.chance(0.3) {
					particles(rng, &mut model, part, "Inside");
				}
			}
			_ => {
				let adornee = if rng.chance(0.3) { root } else { part };
				highlight(rng, &mut model, adornee, "Highlight");
			}
		}
	}
	let time = rng.between(0.05, 1.5);
	Case { model, parts, moves, time, emit: rng.chance(0.5) }
}

/// The case's playback: its emitters emitted by their attributes at 0 (or running from 0), a trail's
/// carrier moving across the view.
fn playback(case: &Case) -> ror::scene::Playback {
	let mut json = serde_json::json!({ "time": case.time, "actions": [], "motion": [] });
	if case.emit {
		json["actions"] = serde_json::json!([{ "at": 0.0, "target": "", "emit": "attributes" }]);
	}
	json["motion"] = serde_json::Value::Array(
		case.moves
			.iter()
			.map(|target| serde_json::json!({ "target": target, "relative": true, "keys": [
				{ "at": 0.0, "cframe": [0.0, 0.0, 0.0] },
				{ "at": case.time, "cframe": [6.0, 2.0, 1.0], "easing": "Quad", "direction": "Out" }
			] }))
			.collect(),
	);
	serde_json::from_value(json).expect("playback JSON")
}

fn scene_of(case: &Case, bare: bool) -> anyhow::Result<ror::scene::Scene> {
	let bytes = case.model.to_rbxm()?;
	let mut document = ror::scene::load::read_bytes(&bytes)?;
	let playback = playback(case);
	let resolved = playback.resolve(&document)?;
	resolved.apply_state(&mut document)?;
	let mut scene = ror::scene::extract::extract(&document);
	resolved.apply_history(&document, &mut scene);
	if bare {
		scene.particles.clear();
		scene.beams.clear();
		scene.trails.clear();
		scene.lights.clear();
		scene.highlights.clear();
		scene.parts.retain(|part| part.material != 1568 || !part.path.contains("Carrier"));
		for part in &mut scene.parts {
			std::sync::Arc::make_mut(part).decals.clear();
		}
	}
	Ok(scene)
}

pub fn main() -> anyhow::Result<()> {
	// non-finite light shows magenta
	// SAFETY: before any other thread starts
	unsafe { std::env::set_var("ROR_NAN_CHECK", "1") };
	let mut args = std::env::args().skip(1);
	let cases: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(300);
	let seed: u64 = args.next().and_then(|value| value.parse().ok()).unwrap_or(1);
	let out = std::path::PathBuf::from(args.next().unwrap_or_else(|| "out/vfx-fuzz".into()));
	std::fs::create_dir_all(&out)?;
	let gpu = Arc::new(ror::render::gpu::Gpu::new()?);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), offline: true, ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let mut renderer = Renderer::new(gpu.clone(), assets.clone())?;
	let camera = CameraSpec::LookAt { eye: [0.0, 4.0, -14.0], target: [0.0, 3.0, 0.0], fov: 70.0 };
	let request = ViewRequest { width: SIZE, height: SIZE, camera, ..Default::default() };
	let draw = |renderer: &Renderer, scene: &ror::scene::Scene| -> Result<Vec<u8>, String> {
		match catch_unwind(AssertUnwindSafe(|| renderer.render(scene, std::slice::from_ref(&request)))) {
			Ok(Ok((images, _))) => images.into_iter().next().map(|image| image.rgba).ok_or_else(|| "no image".into()),
			Ok(Err(error)) => Err(format!("error: {error:#}")),
			Err(panic) => Err(format!("panic: {}", panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string())).unwrap_or_default())),
		}
	};
	let mode = match args.next().as_deref() {
		Some("sane") => Mode::Sane,
		Some("extreme") => Mode::Extreme,
		_ => Mode::Garbage,
	};
	let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d), mode);
	let (mut findings, mut slowest) = (0, (0.0f64, 0usize));
	for number in 0..cases {
		let case = build(&mut rng);
		let mut notes = Vec::new();
		let started = Instant::now();
		let (effect, bare) = match (scene_of(&case, false), scene_of(&case, true)) {
			(Ok(effect), Ok(bare)) => (effect, bare),
			(Err(error), _) | (_, Err(error)) => {
				println!("case {number}: building the scene failed: {error:#} [{}]", case.parts.join(", "));
				continue;
			}
		};
		let first = draw(&renderer, &effect);
		let seconds = started.elapsed().as_secs_f64();
		if seconds > slowest.0 {
			slowest = (seconds, number);
		}
		let control = draw(&renderer, &bare);
		let again = draw(&renderer, &effect);
		// Highlights blend over the image after it is read back (their extrapolated blends reach any
		// colour, magenta too): the check for non-finite light draws without them
		let mut unhighlighted = effect.clone();
		unhighlighted.highlights.clear();
		let checked = draw(&renderer, &unhighlighted);
		match (&first, &control, &again) {
			(Ok(first), Ok(control), Ok(again)) => {
				let differ = first.chunks(4).zip(again.chunks(4)).filter(|(a, b)| a != b).count();
				if differ > 0 {
					notes.push(format!("drawn differently twice: {differ} pixels"));
				}
				// light that is not a finite number (ROR_NAN_CHECK draws it magenta)
				let magenta = checked.as_ref().map(|image| image.chunks(4).filter(|pixel| pixel[..3] == [255, 0, 255]).count()).unwrap_or(0);
				if magenta > 0 {
					notes.push(format!("{magenta} pixels of non-finite light"));
				}
				// pure black where the scene without the effect is not (the effect black, or a NaN)
				let black = first.chunks(4).zip(control.chunks(4)).filter(|(a, b)| a[..3] == [0, 0, 0] && b[..3].iter().any(|&c| c > 24)).count();
				if black > 0 && mode != Mode::Garbage {
					notes.push(format!("{black} pixels pure black (the effect black, or a NaN)"));
				}
				// every effect enabled and seen: it should change the image
				let changed = first.chunks(4).zip(control.chunks(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2)).count();
				if changed == 0 && mode == Mode::Sane {
					notes.push("the effect draws nothing".into());
				}
			}
			_ => {
				for result in [&first, &control, &again] {
					if let Err(error) = result {
						notes.push(error.clone());
					}
				}
				// a panic can leave the renderer unusable: start again
				renderer = Renderer::new(gpu.clone(), assets.clone())?;
			}
		}
		if seconds > SLOW_SECONDS {
			notes.push(format!("slow: {seconds:.1} s"));
		}
		if !notes.is_empty() {
			findings += 1;
			println!("case {number}: {} [{}]", notes.join("; "), case.parts.join(", "));
			std::fs::write(out.join(format!("case-{number}.rbxm")), case.model.to_rbxm()?)?;
			std::fs::write(out.join(format!("case-{number}.json")), serde_json::to_string_pretty(&playback(&case))?)?;
			for (label, image) in [("effect", &first), ("bare", &control), ("again", &again)] {
				if let Ok(rgba) = image {
					ror::output::write_png(&out.join(format!("case-{number}-{label}.png")), SIZE, SIZE, rgba, true)?;
				}
			}
		}
	}
	println!("{cases} cases, {findings} with findings; slowest case {} at {:.2} s", slowest.1, slowest.0);
	Ok(())
}
