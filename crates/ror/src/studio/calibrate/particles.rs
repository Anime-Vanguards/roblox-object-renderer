//! Particle experiments (notes/render.md "Particles"). Single particles are made deterministic: an
//! emitter with Rate 0 on an Attachment (a point), Speed and spreads fixed, emits with `Emit` and is
//! frozen with `TimeScale = 0` after a known age; ror draws the same particles as bursts.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, ContentId, Enum, NumberRange, NumberSequence, NumberSequenceKeypoint, Variant};

use super::{Experiment, Run, dark};
use crate::render::camera::CameraSpec;
use crate::scene::build::{ModelBuilder, PartSpec, cframe};
use crate::scene::{Environment, Scene};

/// What a particle run does before its capture: Luau for the capture Studio (after the model is
/// loaded) and the same particles for ror.
#[derive(Clone, Debug)]
pub struct Setup {
	pub luau: String,
	/// Every emitter's particles: (seconds before the capture, count).
	pub bursts: Vec<(f32, u32)>,
	/// Per emitter name, the count of each burst instead.
	pub counts: Vec<(String, u32)>,
	/// Live particles: ror also writes the mean of this many seeded snapshots (`ror-mean.png`).
	pub seeds: u32,
}

impl Setup {
	/// For live runs, ror's mean image over `seeds` snapshots (each emitter's seed varied).
	#[cfg(windows)]
	pub fn mean_renders(&self, renderer: &crate::render::Renderer, scene: &Scene, request: &crate::render::ViewRequest, dir: &std::path::Path) -> anyhow::Result<()> {
		if self.seeds == 0 {
			return Ok(());
		}
		let mut sum: Vec<f32> = Vec::new();
		let (mut width, mut height) = (0, 0);
		for index in 0..self.seeds {
			let mut scene = scene.clone();
			for emitter in &mut scene.particles {
				emitter.seed ^= (index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
			}
			let (images, _) = renderer.render(&scene, std::slice::from_ref(request))?;
			let image = images.into_iter().next().ok_or_else(|| anyhow::anyhow!("no image"))?;
			(width, height) = (image.width, image.height);
			if sum.is_empty() {
				sum = vec![0.0; image.rgba.len()];
			}
			for (total, &value) in sum.iter_mut().zip(&image.rgba) {
				*total += value as f32;
			}
		}
		let mean: Vec<u8> = sum.iter().map(|total| (total / self.seeds as f32).round() as u8).collect();
		crate::output::write_png(&dir.join("ror-mean.png"), width, height, &mean, true)
	}

	/// Gives ror's emitters the particles the setup emits in Studio.
	pub fn apply(&self, scene: &mut Scene) {
		for emitter in &mut scene.particles {
			// by name: the path's last step, `.name`, or `["name"]` for a name with dots in it
			let count = self
				.counts
				.iter()
				.find(|(name, _)| if name.contains('.') { emitter.path.ends_with(&format!("[{name:?}]")) } else { emitter.path == *name || emitter.path.strip_suffix(name.as_str()).is_some_and(|rest| rest.ends_with('.')) })
				.map(|(_, count)| *count);
			emitter.bursts = self.bursts.iter().map(|&(age, n)| (age, count.unwrap_or(n))).collect();
		}
	}
}

fn setups() -> &'static Mutex<HashMap<(String, String), Setup>> {
	static SETUPS: OnceLock<Mutex<HashMap<(String, String), Setup>>> = OnceLock::new();
	SETUPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The setup of an experiment's run, if it has one.
pub fn setup(experiment: &str, run: &str) -> Option<Setup> {
	setups().lock().ok()?.get(&(experiment.to_string(), run.to_string())).cloned()
}

pub(super) fn register(experiment: &str, run: &str, setup: Setup) {
	if let Ok(mut setups) = setups().lock() {
		setups.insert((experiment.to_string(), run.to_string()), setup);
	}
}

/// Studio: clear every emitter, emit a particle each (or its count by name), let them age `age`
/// seconds, then freeze them.
pub(super) fn freeze(age: f32, counts: &[(String, u32)]) -> Setup {
	staggered(&[(age, 1)], counts)
}

/// Studio: clear every emitter, emit `bursts` (seconds before the freeze, count) oldest first with
/// the waits between them, then freeze. Counts by emitter name override every burst's count.
fn staggered(bursts: &[(f32, u32)], counts: &[(String, u32)]) -> Setup {
	let mut bursts = bursts.to_vec();
	bursts.sort_by(|a, b| b.0.total_cmp(&a.0));
	let table: Vec<String> = counts.iter().map(|(name, count)| format!("[{name:?}] = {count}")).collect();
	let mut steps = String::new();
	for (index, &(age, count)) in bursts.iter().enumerate() {
		steps.push_str(&format!("for _, e in emitters do\n\te:Emit(counts[e.Name] or {count})\nend\n"));
		let next = bursts.get(index + 1).map(|(age, _)| *age).unwrap_or(0.0);
		if age - next > 0.0 {
			steps.push_str(&format!("task.wait({})\n", age - next));
		}
	}
	let luau = format!(
		"local counts = {{{}}}
local emitters = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.TimeScale = 1
		e:Clear()
		table.insert(emitters, e)
	end
end
-- Studio ages only the particles it draws; the camera was just moved onto them
task.wait(0.3)
{steps}for _, e in emitters do
	e.TimeScale = 0
end
return #emitters",
		table.join(", ")
	);
	Setup { luau, bursts, counts: counts.to_vec(), seeds: 0 }
}

/// Studio: let every emitter run from empty for `wait` seconds (past its longest lifetime: its steady
/// state); ror draws its own steady state and the mean of `seeds` snapshots.
fn steady(wait: f32, seeds: u32) -> Setup {
	let luau = format!(
		"local emitters = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.TimeScale = 1
		e:Clear()
		table.insert(emitters, e)
	end
end
task.wait({wait})
return #emitters"
	);
	Setup { luau, bursts: Vec::new(), counts: Vec::new(), seeds }
}

/// Experiments of live particles: a burst of Studio frames (count, ms apart) for their mean.
pub fn burst(name: &str) -> Option<(u32, u64)> {
	match name {
		"particle-spread" => Some((24, 250)),
		"particle-timescale" => Some((12, 250)),
		"particle-held-live" | "particle-held-frozen" => Some((16, 150)),
		"flare-near" | "cage-inside" => Some((16, 150)),
		name if name.starts_with("particle-model") => Some((32, 200)),
		name if name.starts_with("particle-view") => Some((60, 400)),
		_ => None,
	}
}

/// Property overrides of an experiment's emitter.
type Props = Vec<(&'static str, Variant)>;

pub const SQUARE: &str = "rbxasset://textures/particles/SquareParticle.png";
pub const ARROW: &str = "rbxasset://textures/ArrowCursor.png";

pub fn solid(c: [f32; 3]) -> Variant {
	ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into()
}

pub fn constant(value: f32) -> Variant {
	NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, value, 0.0), NumberSequenceKeypoint::new(1.0, value, 0.0)] }.into()
}

pub fn fixed(value: f32) -> Variant {
	NumberRange::new(value, value).into()
}

/// A particle emitter on an Attachment at `at` (on an invisible anchor part), still and alone
/// unless `set` overrides: Rate 0, Speed 0, Lifetime 10, Size 4, white SquareParticle, opaque,
/// unlit, Brightness 1, no rotation.
pub fn add_emitter(model: &mut ModelBuilder, name: &str, at: Affine3A, set: &[(&str, Variant)]) {
	let anchor = model.part(&PartSpec { name: format!("{name}-anchor"), cframe: Affine3A::from_translation(at.translation.into()), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
	let attachment = model.child(anchor, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", cframe(Affine3A::from_mat3(at.matrix3.into()))));
	model.child(attachment, emitter_builder(name, set));
}

/// A ParticleEmitter with the experiments' defaults (see `add_emitter`) and `set` on top.
pub(super) fn emitter_builder(name: &str, set: &[(&str, Variant)]) -> InstanceBuilder {
	let mut emitter = InstanceBuilder::new("ParticleEmitter")
		.with_name(name)
		.with_property("Rate", 0.0f32)
		.with_property("Speed", fixed(0.0))
		.with_property("Lifetime", NumberRange::new(10.0, 10.0))
		.with_property("Size", constant(4.0))
		.with_property("Transparency", constant(0.0))
		.with_property("Color", solid([1.0, 1.0, 1.0]))
		.with_property("TextureContent", rbx_dom_weak::types::Content::from_uri(SQUARE))
		.with_property("LightEmission", 0.0f32)
		.with_property("LightInfluence", 0.0f32)
		.with_property("Brightness", 1.0f32)
		.with_property("Rotation", fixed(0.0))
		.with_property("RotSpeed", fixed(0.0))
		.with_property("SpreadAngle", rbx_dom_weak::types::Vector2::new(0.0, 0.0))
		.with_property("Drag", 0.0f32)
		.with_property("Acceleration", rbx_dom_weak::types::Vector3::new(0.0, 0.0, 0.0))
		.with_property("ZOffset", 0.0f32)
		.with_property("Orientation", Enum::from_u32(0));
	for (property, value) in set {
		// the texture: Studio reads TextureContent before Texture
		let property = if *property == "Texture" { "TextureContent" } else { *property };
		let value = match (property, value) {
			("TextureContent", Variant::ContentId(uri)) => rbx_dom_weak::types::Content::from_uri(uri.as_str()).into(),
			_ => value.clone(),
		};
		emitter = emitter.with_property(property, value);
	}
	emitter
}

/// A black floor and wall, and behind each case a panel black on its left (+X) half and white on
/// its right, so blends show over both.
fn backdrop(model: &mut ModelBuilder, centers: &[Vec3]) {
	for (index, center) in centers.iter().enumerate() {
		for (side, color) in [(1.0f32, [0u8, 0, 0]), (-1.0, [255, 255, 255])] {
			model.part(&PartSpec {
				name: format!("Panel{index}"),
				cframe: Affine3A::from_translation(*center + Vec3::new(side * 4.0, 0.0, 6.0)),
				size: Vec3::new(8.0, 10.0, 0.5),
				color,
				cast_shadow: false,
				..Default::default()
			});
		}
	}
	let middle = centers.iter().fold(Vec3::ZERO, |sum, center| sum + *center) / centers.len().max(1) as f32;
	let span = centers.iter().map(|center| (center.x - middle.x).abs()).fold(0.0, f32::max) * 2.0 + 100.0;
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(middle + Vec3::new(0.0, -8.0, 0.0)), size: Vec3::new(span, 1.0, 200.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(middle + Vec3::new(0.0, 0.0, 20.0)), size: Vec3::new(span, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
}

/// One particle each, frozen at age 0, seen head-on: its size and texture orientation, Rotation and
/// Squash, Brightness and colour, Transparency and LightEmission (also negative) over black and
/// white, LightInfluence, and the orientation modes.
pub fn particle_basic() -> Experiment {
	let name = "particle-basic";
	let mut model = ModelBuilder::new("ParticleBasic");
	let up = Affine3A::IDENTITY;
	// the attachment's Y axis towards the camera (-Z): particles leave towards it
	let toward = Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2);
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let sun = Environment { brightness: 2.0, ..dark() };
	// (label, attachment rotation, properties, environment)
	let cases: Vec<(&str, Affine3A, Props, Environment)> = vec![
		("square", up, vec![], flat.clone()),
		("arrow", up, vec![("Texture", ContentId::from(ARROW).into())], flat.clone()),
		("arrow-rot45", up, vec![("Texture", ContentId::from(ARROW).into()), ("Rotation", fixed(45.0))], flat.clone()),
		("arrow-squash", up, vec![("Texture", ContentId::from(ARROW).into()), ("Squash", constant(0.5))], flat.clone()),
		("square-b3", up, vec![("Brightness", 3.0f32.into())], flat.clone()),
		("orange", up, vec![("Color", solid([1.0, 0.28235295, 0.0]))], flat.clone()),
		("half", up, vec![("Transparency", constant(0.5))], flat.clone()),
		("additive", up, vec![("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into())], flat.clone()),
		("negative", up, vec![("LightEmission", (-4.0f32).into())], flat.clone()),
		("negative-half", up, vec![("Transparency", constant(0.5)), ("LightEmission", (-4.0f32).into())], flat.clone()),
		("lit-flat", up, vec![("LightInfluence", 1.0f32.into())], flat.clone()),
		("lit-sun", up, vec![("LightInfluence", 1.0f32.into())], sun.clone()),
		("parallel", up, vec![("Texture", ContentId::from(ARROW).into()), ("Orientation", Enum::from_u32(2).into()), ("Speed", fixed(10.0))], flat.clone()),
		("perpendicular", toward, vec![("Texture", ContentId::from(ARROW).into()), ("Orientation", Enum::from_u32(3).into()), ("Speed", fixed(10.0))], flat.clone()),
	];
	let centers: Vec<Vec3> = (0..cases.len()).map(|index| Vec3::new(index as f32 * 30.0, 0.0, 0.0)).collect();
	let mut runs = Vec::new();
	for ((label, rotation, set, environment), center) in cases.into_iter().zip(&centers) {
		add_emitter(&mut model, label, Affine3A::from_translation(*center) * rotation, &set);
		runs.push(Run { label: label.into(), environment, camera: CameraSpec::LookAt { eye: (*center + Vec3::new(0.0, 0.0, -40.0)).into(), target: (*center).into(), fov: 20.0 } });
		register(name, label, freeze(0.0, &[]));
	}
	backdrop(&mut model, &centers);
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Particles over their life: one row of emitters (Lifetime 10) whose Size, Transparency, Color and
/// Squash change with age (one sequence with a sharp peak at half life), that move (Speed, Drag,
/// Acceleration) or spin (RotSpeed), frozen at ages 1, 2.5, 5 and 7.5 seconds, seen from the front.
pub fn particle_age() -> Experiment {
	let name = "particle-age";
	let mut model = ModelBuilder::new("ParticleAge");
	let keys = |keys: &[(f32, f32)]| -> Variant { NumberSequence { keypoints: keys.iter().map(|(time, value)| NumberSequenceKeypoint::new(*time, *value, 0.0)).collect() }.into() };
	let colors = |keys: &[(f32, [f32; 3])]| -> Variant { ColorSequence { keypoints: keys.iter().map(|(time, c)| ColorSequenceKeypoint::new(*time, Color3::new(c[0], c[1], c[2]))).collect() }.into() };
	let small = ("Size", constant(1.0));
	let cases: Vec<(&str, Vec<(&str, Variant)>)> = vec![
		("size-ramp", vec![("Size", keys(&[(0.0, 0.5), (1.0, 1.5)]))]),
		("size-peak", vec![("Size", keys(&[(0.0, 0.5), (0.5, 1.5), (1.0, 0.5)]))]),
		("fade", vec![small.clone(), ("Transparency", keys(&[(0.0, 0.0), (1.0, 1.0)]))]),
		("colour", vec![small.clone(), ("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])]))]),
		("squash", vec![("Texture", ContentId::from(ARROW).into()), ("Size", constant(1.5)), ("Squash", keys(&[(0.0, 0.0), (1.0, 2.0)]))]),
		("speed", vec![small.clone(), ("Speed", fixed(1.0))]),
		("drag", vec![small.clone(), ("Speed", fixed(2.0)), ("Drag", 0.5f32.into())]),
		("accel", vec![small.clone(), ("Acceleration", rbx_dom_weak::types::Vector3::new(0.0, 0.25, 0.0).into())]),
		("spin", vec![("Texture", ContentId::from(ARROW).into()), ("Size", constant(1.5)), ("RotSpeed", fixed(30.0))]),
	];
	let mut centers = Vec::new();
	for (index, (label, set)) in cases.into_iter().enumerate() {
		let center = Vec3::new((index as f32 - 4.0) * 4.5, -6.0, 0.0);
		add_emitter(&mut model, label, Affine3A::from_translation(center), &set);
		centers.push(center);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for age in [1.0f32, 2.5, 5.0, 7.5] {
		let label = format!("age{age}");
		runs.push(Run { label: label.clone(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -60.0], target: [0.0, 0.0, 0.0], fov: 30.0 } });
		register(name, &label, freeze(age, &[]));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// Draw order, ZOffset and emission direction: two opaque particles of one emitter at one spot (the
/// older green, the newer red: which is on top?), a particle that flew in front of a glass panel from
/// an emitter behind it (sorted by the emitter or the particle?), ZOffset pulling a particle out from
/// behind a block (its size on screen against an unoccluded twin), and EmissionDirection on a turned
/// attachment and a turned part.
pub fn particle_order() -> Experiment {
	let name = "particle-order";
	let mut model = ModelBuilder::new("ParticleOrder");
	let colors = |keys: &[(f32, [f32; 3])]| -> Variant { ColorSequence { keypoints: keys.iter().map(|(time, c)| ColorSequenceKeypoint::new(*time, Color3::new(c[0], c[1], c[2]))).collect() }.into() };
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let camera = |center: Vec3| CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -40.0)).into(), target: center.into(), fov: 25.0 };
	let mut runs = Vec::new();
	// (0) two ages at one spot
	let a = Vec3::new(0.0, 0.0, 0.0);
	add_emitter(&mut model, "two-ages", Affine3A::from_translation(a), &[("Size", constant(2.0)), ("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (0.15, [1.0, 0.0, 0.0]), (0.2, [0.0, 1.0, 0.0]), (1.0, [0.0, 1.0, 0.0])]))]);
	runs.push(Run { label: "two-ages".into(), environment: flat.clone(), camera: camera(a) });
	register(name, "two-ages", staggered(&[(3.0, 1), (1.0, 1)], &[]));
	// (1) an emitter 2 studs behind a red glass panel shoots its particle 4 studs towards the camera
	let b = Vec3::new(40.0, 0.0, 0.0);
	let toward = Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2);
	add_emitter(&mut model, "glass", Affine3A::from_translation(b + Vec3::new(0.0, 0.0, 2.0)) * toward, &[("Size", constant(1.5)), ("Speed", fixed(4.0))]);
	model.part(&PartSpec { name: "Glass".into(), cframe: Affine3A::from_translation(b), size: Vec3::new(6.0, 6.0, 0.2), color: [255, 0, 0], transparency: 0.5, cast_shadow: false, ..Default::default() });
	runs.push(Run { label: "glass".into(), environment: flat.clone(), camera: camera(b) });
	register(name, "glass", freeze(1.0, &[]));
	// (2) ZOffset on particles straddling a block's edge (so no culling hides the emitter): 3 pulls
	// one 1.5 studs behind out in front, -3 pushes one 1.5 studs in front behind; and in the open,
	// ZOffset 10 against a twin without (does its size on screen change?)
	let c = Vec3::new(80.0, 0.0, 0.0);
	model.part(&PartSpec { name: "Block".into(), cframe: Affine3A::from_translation(c + Vec3::new(2.5, 0.0, 0.0)), size: Vec3::new(5.0, 10.0, 0.5), color: [90, 90, 90], cast_shadow: false, ..Default::default() });
	add_emitter(&mut model, "pulled", Affine3A::from_translation(c + Vec3::new(0.0, 2.5, 1.5)), &[("Size", constant(1.0)), ("ZOffset", 3.0f32.into())]);
	add_emitter(&mut model, "pushed", Affine3A::from_translation(c + Vec3::new(0.0, -2.5, -1.5)), &[("Size", constant(1.0)), ("ZOffset", (-3.0f32).into())]);
	add_emitter(&mut model, "far", Affine3A::from_translation(c + Vec3::new(-4.0, 2.5, 0.0)), &[("Size", constant(0.75)), ("ZOffset", 10.0f32.into())]);
	add_emitter(&mut model, "twin", Affine3A::from_translation(c + Vec3::new(-4.0, -2.5, 0.0)), &[("Size", constant(0.75))]);
	runs.push(Run { label: "zoffset".into(), environment: flat.clone(), camera: camera(c) });
	register(name, "zoffset", freeze(0.0, &[]));
	// (3) EmissionDirection Top and Right on an attachment turned 90 degrees about Z (Y to -X, X to
	// +Y), and Right on a tiny part turned the same way
	let d = Vec3::new(120.0, 0.0, 0.0);
	let turned = Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
	add_emitter(&mut model, "attachment-top", Affine3A::from_translation(d + Vec3::new(-3.0, 0.0, 0.0)) * turned, &[("Size", constant(0.5)), ("Speed", fixed(2.0))]);
	add_emitter(&mut model, "attachment-right", Affine3A::from_translation(d + Vec3::new(0.0, -3.0, 0.0)) * turned, &[("Size", constant(0.5)), ("Speed", fixed(2.0)), ("EmissionDirection", Enum::from_u32(0).into())]);
	let part = model.part(&PartSpec { name: "Emitter".into(), cframe: Affine3A::from_translation(d + Vec3::new(3.0, 0.0, 0.0)) * turned, size: Vec3::splat(0.05), transparency: 1.0, cast_shadow: false, ..Default::default() });
	model.child(
		part,
		InstanceBuilder::new("ParticleEmitter")
			.with_name("part-right")
			.with_property("Rate", 0.0f32)
			.with_property("Speed", fixed(2.0))
			.with_property("Lifetime", NumberRange::new(10.0, 10.0))
			.with_property("Size", constant(0.5))
			.with_property("Transparency", constant(0.0))
			.with_property("Color", solid([1.0, 1.0, 1.0]))
			.with_property("TextureContent", rbx_dom_weak::types::Content::from_uri(SQUARE))
			.with_property("LightEmission", 0.0f32)
			.with_property("LightInfluence", 0.0f32)
			.with_property("Rotation", fixed(0.0))
			.with_property("RotSpeed", fixed(0.0))
			.with_property("SpreadAngle", rbx_dom_weak::types::Vector2::new(0.0, 0.0))
			.with_property("EmissionDirection", Enum::from_u32(0)),
	);
	runs.push(Run { label: "direction".into(), environment: flat.clone(), camera: camera(d) });
	register(name, "direction", freeze(1.0, &[]));
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(60.0, 0.0, 12.0)), size: Vec3::new(300.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Which emitters move their particles: Speed 2 from an attachment and from a tiny part, each unturned
/// and turned 90 degrees about Z, frozen at 1 second.
pub fn particle_motion() -> Experiment {
	let name = "particle-motion";
	let mut model = ModelBuilder::new("ParticleMotion");
	let turned = Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
	let moving = || -> Vec<(&'static str, Variant)> { vec![("Size", constant(0.5)), ("Speed", fixed(2.0))] };
	add_emitter(&mut model, "attachment", Affine3A::from_translation(Vec3::new(-6.0, -3.0, 0.0)), &moving());
	add_emitter(&mut model, "attachment-turned", Affine3A::from_translation(Vec3::new(-2.0, -3.0, 0.0)) * turned, &moving());
	for (label, x, rotation) in [("part", 2.0f32, Affine3A::IDENTITY), ("part-turned", 6.0, turned)] {
		let part = model.part(&PartSpec { name: format!("{label}-part"), cframe: Affine3A::from_translation(Vec3::new(x, -3.0, 0.0)) * rotation, size: Vec3::splat(0.05), transparency: 1.0, cast_shadow: false, ..Default::default() });
		model.child(
			part,
			InstanceBuilder::new("ParticleEmitter")
				.with_name(label)
				.with_property("Rate", 0.0f32)
				.with_property("Speed", fixed(2.0))
				.with_property("Lifetime", NumberRange::new(10.0, 10.0))
				.with_property("Size", constant(0.5))
				.with_property("Transparency", constant(0.0))
				.with_property("Color", solid([1.0, 1.0, 1.0]))
				.with_property("TextureContent", rbx_dom_weak::types::Content::from_uri(SQUARE))
				.with_property("LightEmission", 0.0f32)
				.with_property("LightInfluence", 0.0f32)
				.with_property("Rotation", fixed(0.0))
				.with_property("RotSpeed", fixed(0.0))
				.with_property("SpreadAngle", rbx_dom_weak::types::Vector2::new(0.0, 0.0)),
		);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 12.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let runs = vec![Run { label: "age1".into(), environment: flat, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0, 0.0, 0.0], fov: 25.0 } }];
	register(name, "age1", freeze(1.0, &[]));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// A particle emitter under a part of `size` (invisible) at `at`, otherwise as `add_emitter`.
pub fn add_part_emitter(model: &mut ModelBuilder, name: &str, at: Affine3A, size: Vec3, set: &[(&str, Variant)]) {
	let part = model.part(&PartSpec { name: format!("{name}-part"), cframe: at, size, transparency: 1.0, cast_shadow: false, ..Default::default() });
	model.child(part, emitter_builder(name, set));
}

/// Whole emitters, live: spreads (a 30-degree cone, a 90-degree fan, every direction), Drag and
/// Acceleration, and part emitters (a box's volume and surface, a sphere, a disc), small additive
/// particles at Rate 40 and Lifetime 1.5; Studio's mean over a burst of frames against ror's mean
/// over seeded snapshots, from the front and from above.
pub fn particle_spread() -> Experiment {
	let name = "particle-spread";
	let mut model = ModelBuilder::new("ParticleSpread");
	let live = |extra: Vec<(&'static str, Variant)>| -> Vec<(&'static str, Variant)> {
		let mut set = vec![
			("Rate", 40.0f32.into()),
			("Lifetime", NumberRange::new(1.5, 1.5).into()),
			("Size", constant(0.12)),
			("LightEmission", 1.0f32.into()),
			("Speed", fixed(4.0)),
		];
		set.extend(extra);
		set
	};
	let spread = |x: f32, y: f32| -> (&'static str, Variant) { ("SpreadAngle", rbx_dom_weak::types::Vector2::new(x, y).into()) };
	let at = |x: f32| Affine3A::from_translation(Vec3::new(x, -4.0, 0.0));
	add_emitter(&mut model, "cone30", at(-40.0), &live(vec![spread(30.0, 30.0)]));
	add_emitter(&mut model, "fan90", at(-30.0), &live(vec![spread(90.0, 0.0)]));
	add_emitter(&mut model, "sphere360", at(-20.0), &live(vec![spread(360.0, 360.0), ("Speed", fixed(2.5))]));
	add_emitter(&mut model, "drag", at(-10.0), &live(vec![spread(20.0, 20.0), ("Speed", fixed(12.0)), ("Drag", 3.0f32.into())]));
	add_emitter(&mut model, "accel", at(0.0), &live(vec![spread(30.0, 30.0), ("Speed", fixed(6.0)), ("Acceleration", rbx_dom_weak::types::Vector3::new(0.0, -8.0, 0.0).into())]));
	add_part_emitter(&mut model, "box-volume", at(10.0), Vec3::new(5.0, 1.0, 5.0), &live(vec![("Speed", fixed(1.5))]));
	add_part_emitter(&mut model, "box-surface", at(20.0), Vec3::new(5.0, 1.0, 5.0), &live(vec![("Speed", fixed(1.5)), ("ShapeStyle", Enum::from_u32(1).into())]));
	add_part_emitter(&mut model, "sphere", at(30.0) * Affine3A::from_translation(Vec3::new(0.0, 2.0, 0.0)), Vec3::splat(4.0), &live(vec![("Speed", fixed(1.0)), ("Shape", Enum::from_u32(1).into())]));
	add_part_emitter(&mut model, "disc", at(40.0), Vec3::new(1.0, 4.0, 4.0), &live(vec![("Speed", fixed(1.5)), ("Shape", Enum::from_u32(3).into())]));
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -20.0, 0.0)), size: Vec3::new(300.0, 1.0, 200.0), color: [0, 0, 0], ..Default::default() });
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let runs = vec![
		Run { label: "front".into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: [0.0, 4.0, -75.0], target: [0.0, 0.0, 0.0], fov: 45.0 } },
		Run { label: "top".into(), environment: flat, camera: CameraSpec::LookAt { eye: [0.0, 75.0, -0.1], target: [0.0, 0.0, 0.0], fov: 45.0 } },
	];
	for run in &runs {
		register(name, &run.label, steady(2.0, 12));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Spawn regions and directions: 300 particles from each emitter at once, frozen at age 0 (where they
/// start) and 1 second (where they went at Speed 3, no drag): SpreadAngle on one axis, the other and
/// both, 90 and 360 degrees; Box volume, surface and in-and-out, Sphere (also half), Cylinder and
/// Disc on parts of telling sizes; from the front and from above.
pub fn particle_shapes() -> Experiment {
	let name = "particle-shapes";
	let mut model = ModelBuilder::new("ParticleShapes");
	let cloud = |extra: Vec<(&'static str, Variant)>| -> Vec<(&'static str, Variant)> {
		let mut set = vec![("Size", constant(0.1)), ("LightEmission", 1.0f32.into()), ("Speed", fixed(3.0)), ("Transparency", constant(0.3))];
		set.extend(extra);
		set
	};
	let spread = |x: f32, y: f32| -> (&'static str, Variant) { ("SpreadAngle", rbx_dom_weak::types::Vector2::new(x, y).into()) };
	let shape = |value: u32| -> (&'static str, Variant) { ("Shape", Enum::from_u32(value).into()) };
	let row = |index: usize, z: f32| Affine3A::from_translation(Vec3::new((index as f32 - 3.0) * 12.0, 0.0, z));
	let attachments: Vec<(&str, Vec<(&str, Variant)>)> = vec![
		("spread-x30", cloud(vec![spread(30.0, 0.0)])),
		("spread-y30", cloud(vec![spread(0.0, 30.0)])),
		("spread-30", cloud(vec![spread(30.0, 30.0)])),
		("spread-90", cloud(vec![spread(90.0, 0.0)])),
		("spread-360", cloud(vec![spread(360.0, 360.0)])),
		("spread-180", cloud(vec![spread(180.0, 180.0)])),
		("spread-x30-front", cloud(vec![spread(30.0, 0.0), ("EmissionDirection", Enum::from_u32(5).into())])),
	];
	for (index, (label, set)) in attachments.into_iter().enumerate() {
		add_emitter(&mut model, label, row(index, -12.0), &set);
	}
	let parts: Vec<(&str, Vec3, Props)> = vec![
		("box-volume", Vec3::new(4.0, 1.0, 2.0), cloud(vec![])),
		("box-surface", Vec3::new(4.0, 1.0, 2.0), cloud(vec![("ShapeStyle", Enum::from_u32(1).into())])),
		("box-inout", Vec3::new(4.0, 1.0, 2.0), cloud(vec![("ShapeInOut", Enum::from_u32(2).into())])),
		("sphere", Vec3::new(4.0, 4.0, 4.0), cloud(vec![shape(1)])),
		("sphere-half", Vec3::new(4.0, 4.0, 4.0), cloud(vec![shape(1), ("ShapePartial", 0.5f32.into())])),
		("cylinder", Vec3::new(2.0, 4.0, 4.0), cloud(vec![shape(2)])),
		("disc", Vec3::new(2.0, 4.0, 4.0), cloud(vec![shape(3)])),
	];
	for (index, (label, size, set)) in parts.into_iter().enumerate() {
		add_part_emitter(&mut model, label, row(index, 12.0), size, &set);
	}
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -15.0, 0.0)), size: Vec3::new(300.0, 1.0, 300.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 40.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for age in [0.0f32, 1.0] {
		for (view, eye) in [("front", [0.0f32, 30.0, -75.0]), ("top", [0.0, 95.0, -0.1])] {
			let label = format!("{view}-age{age}");
			runs.push(Run { label: label.clone(), environment: flat.clone(), camera: CameraSpec::LookAt { eye, target: [0.0, 0.0, 0.0], fov: 45.0 } });
			register(name, &label, staggered(&[(age, 300)], &[]));
		}
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Cylinder and Disc spawn regions and ShapePartial (in one game 189 of the 363 Cylinder part emitters and
/// 2,117 of the 4,971 Disc ones set it, 1,199 Discs above 1): 200 particles from each at once,
/// frozen at age 0 where they spawn (Speed 0), mostly from 3-stud cubes: Cylinders at ShapePartial
/// -0.5, 0, 0.5, 1 and 2, at 0 also emitting along each other face and from the surface; Discs at
/// -0.5, 0, 0.5, 1 and 2, from the surface, emitting Right, Front and Bottom; both on a 2 x 3 x 4
/// part, a Disc on a flat one, Spheres turned and stretched. Each case three times, turned so that
/// the part's X, Z and Y axis runs along the view; two runs a view, four columns each.
pub fn particle_shape_partial() -> Experiment {
	let name = "particle-shape-partial";
	let mut model = ModelBuilder::new("ParticleShapePartial");
	let shape = |value: u32, partial: f32| -> Vec<(&'static str, Variant)> {
		vec![("Size", constant(0.1)), ("LightEmission", 1.0f32.into()), ("Transparency", constant(0.3)), ("Shape", Enum::from_u32(value).into()), ("ShapePartial", partial.into())]
	};
	let with = |mut set: Vec<(&'static str, Variant)>, property: &'static str, value: u32| {
		set.push((property, Enum::from_u32(value).into()));
		set
	};
	let (cylinder, disc, sphere) = (2, 3, 1);
	let direction = |set: Vec<(&'static str, Variant)>, face: u32| with(set, "EmissionDirection", face);
	let surface = |set: Vec<(&'static str, Variant)>| with(set, "ShapeStyle", 1);
	let (cube, block, flat) = (Vec3::splat(3.0), Vec3::new(2.0, 3.0, 4.0), Vec3::new(3.0, 0.4, 3.0));
	// (label, part size, properties); EmissionDirection 0 Right, 1 Top, 2 Back, 3 Left, 4 Bottom, 5 Front
	let cases: Vec<(&str, Vec3, Vec<(&str, Variant)>)> = vec![
		("cyl-1", cube, shape(cylinder, 1.0)),
		("cyl-0", cube, shape(cylinder, 0.0)),
		("cyl-0.5", cube, shape(cylinder, 0.5)),
		("cyl-2", cube, shape(cylinder, 2.0)),
		("cyl--0.5", cube, shape(cylinder, -0.5)),
		("cyl-0-right", cube, direction(shape(cylinder, 0.0), 0)),
		("cyl-0-left", cube, direction(shape(cylinder, 0.0), 3)),
		("cyl-0-front", cube, direction(shape(cylinder, 0.0), 5)),
		("cyl-0-back", cube, direction(shape(cylinder, 0.0), 2)),
		("cyl-0-bottom", cube, direction(shape(cylinder, 0.0), 4)),
		("cyl-0-surface", cube, surface(shape(cylinder, 0.0))),
		("cyl-1-surface", cube, surface(shape(cylinder, 1.0))),
		("cyl-0.5-block", block, shape(cylinder, 0.5)),
		("cyl-0.5-block-right", block, direction(shape(cylinder, 0.5), 0)),
		("disc-1", cube, shape(disc, 1.0)),
		("disc-0", cube, shape(disc, 0.0)),
		("disc-0.5", cube, shape(disc, 0.5)),
		("disc-2", cube, shape(disc, 2.0)),
		("disc--0.5", cube, shape(disc, -0.5)),
		("disc-1-surface", cube, surface(shape(disc, 1.0))),
		("disc-0.5-surface", cube, surface(shape(disc, 0.5))),
		("disc-0-right", cube, direction(shape(disc, 0.0), 0)),
		("disc-0.5-front", cube, direction(shape(disc, 0.5), 5)),
		("disc-1-bottom-surface", cube, surface(direction(shape(disc, 1.0), 4))),
		("disc-1-block", block, shape(disc, 1.0)),
		("disc-1-flat", flat, shape(disc, 1.0)),
		("sphere-0.5-right", cube, direction(shape(sphere, 0.5), 0)),
		("sphere-1-block", block, shape(sphere, 1.0)),
	];
	// the view runs along +Z: the part's X axis turned onto it for the face, its Y axis for the top
	let views: [(&str, Affine3A, f32); 3] = [
		("face", Affine3A::from_rotation_y(-std::f32::consts::FRAC_PI_2), 0.0),
		("side", Affine3A::IDENTITY, -60.0),
		("top", Affine3A::from_rotation_x(std::f32::consts::FRAC_PI_2), -120.0),
	];
	let cell = |index: usize, y: f32| Vec3::new((index % 7) as f32 * 8.0 - 24.0, y + 12.0 - (index / 7) as f32 * 8.0, 0.0);
	for (index, (label, size, set)) in cases.iter().enumerate() {
		for (view, turn, y) in &views {
			add_part_emitter(&mut model, &format!("{label}-{view}"), Affine3A::from_translation(cell(index, *y)) * *turn, *size, set);
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -60.0, 40.0)), size: Vec3::new(300.0, 400.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let mut runs = Vec::new();
	for (view, _, y) in &views {
		for (half, x) in [("a", -12.0f32), ("b", 12.0)] {
			let label = format!("{view}-{half}");
			runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: [x, *y, -38.0], target: [x, *y, 0.0], fov: 45.0 } });
			register(name, &label, staggered(&[(0.0, 200)], &[]));
		}
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Where along a tapered Cylinder its particles start: 12 x 2 x 2 Cylinders at ShapePartial 0, 0.5,
/// 2 and -0.5 and a Surface one at 0, 300 small particles each frozen where they spawn, seen from
/// their side and along their axis from far off (little perspective).
pub fn particle_shape_taper() -> Experiment {
	let name = "particle-shape-taper";
	let mut model = ModelBuilder::new("ParticleShapeTaper");
	let cylinder = |partial: f32, style: u32| -> Vec<(&'static str, Variant)> {
		vec![
			("Size", constant(0.05)),
			("LightEmission", 1.0f32.into()),
			("Transparency", constant(0.3)),
			("Shape", Enum::from_u32(2).into()),
			("ShapePartial", partial.into()),
			("ShapeStyle", Enum::from_u32(style).into()),
		]
	};
	let cases = [("cone", cylinder(0.0, 0)), ("half", cylinder(0.5, 0)), ("wide", cylinder(2.0, 0)), ("negative", cylinder(-0.5, 0)), ("cone-surface", cylinder(0.0, 1))];
	for (index, (label, set)) in cases.iter().enumerate() {
		add_part_emitter(&mut model, &format!("{label}-side"), Affine3A::from_translation(Vec3::new(0.0, 10.0 - index as f32 * 5.0, 0.0)), Vec3::new(12.0, 2.0, 2.0), set);
		// the axis along the view (+Z)
		let face = Affine3A::from_translation(Vec3::new(10.0 - index as f32 * 5.0, -60.0, 0.0)) * Affine3A::from_rotation_y(-std::f32::consts::FRAC_PI_2);
		add_part_emitter(&mut model, &format!("{label}-face"), face, Vec3::new(12.0, 2.0, 2.0), set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -30.0, 40.0)), size: Vec3::new(400.0, 400.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let mut runs = Vec::new();
	for (view, y) in [("side", 0.0f32), ("face", -60.0)] {
		runs.push(Run { label: view.into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, y, -150.0], target: [0.0, y, 0.0], fov: 11.0 } });
		register(name, view, staggered(&[(0.0, 300)], &[]));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Cylinders across a part far from round, as an effect's fire part (27 x 1.1 x 30 from Top, its
/// emitters Cylinders at ShapePartial 0): the cone and the full Cylinder, 400 small particles frozen
/// where they spawn; 2 x 1 x 8 Cylinders (volume and surface) and a Sphere whose particles leave at
/// 10 studs a second for 0.5 s (which way "away from the axis" or "radially" points when the round
/// is squashed), and an 8 x 4 x 4 cone's (whether they lean along its slant); 12-stud cones (2 and 8
/// across, the surface style, ShapePartial 0.5 and 2) and a 4-stud one, 2000 tiny particles each,
/// for where along the axis they start. Each case alone in its runs, seen along the part's X (face),
/// Z (side) or Y (top).
pub fn particle_shape_ellipse() -> Experiment {
	let name = "particle-shape-ellipse";
	let mut model = ModelBuilder::new("ParticleShapeEllipse");
	let emitter = |shape: u32, partial: f32, style: u32, speed: f32, size: f32| -> Vec<(&'static str, Variant)> {
		vec![
			("Size", constant(size)),
			("LightEmission", 1.0f32.into()),
			("Transparency", constant(0.3)),
			("Shape", Enum::from_u32(shape).into()),
			("ShapePartial", partial.into()),
			("ShapeStyle", Enum::from_u32(style).into()),
			("Speed", fixed(speed)),
		]
	};
	let (sphere, cylinder) = (1, 2);
	let flat = Vec3::new(27.0, 1.1, 30.0);
	let squashed = Vec3::new(2.0, 1.0, 8.0);
	let long = Vec3::new(12.0, 2.0, 2.0);
	let all: &[&str] = &["face", "side", "top"];
	// (label, part size, properties, particles, views)
	let cases: Vec<(&str, Vec3, Vec<(&str, Variant)>, u32, &[&str])> = vec![
		("cone", flat, emitter(cylinder, 0.0, 0, 0.0, 0.25), 400, all),
		("full", flat, emitter(cylinder, 1.0, 0, 0.0, 0.25), 400, all),
		("leaving", squashed, emitter(cylinder, 1.0, 0, 10.0, 0.25), 400, all),
		("leaving-surface", squashed, emitter(cylinder, 1.0, 1, 10.0, 0.25), 400, &["face"]),
		("leaving-sphere", squashed, emitter(sphere, 1.0, 0, 10.0, 0.25), 400, &["face", "top"]),
		("leaving-cone", Vec3::new(8.0, 4.0, 4.0), emitter(cylinder, 0.0, 0, 10.0, 0.25), 400, &["side"]),
		("cone-12", long, emitter(cylinder, 0.0, 0, 0.0, 0.05), 2000, &["side"]),
		("cone-12-fat", Vec3::new(12.0, 8.0, 8.0), emitter(cylinder, 0.0, 0, 0.0, 0.05), 2000, &["side"]),
		("cone-4", Vec3::new(4.0, 2.0, 2.0), emitter(cylinder, 0.0, 0, 0.0, 0.05), 2000, &["side"]),
		("half-12", long, emitter(cylinder, 0.5, 0, 0.0, 0.05), 2000, &["side"]),
		("wide-12", long, emitter(cylinder, 2.0, 0, 0.0, 0.05), 2000, &["side"]),
		("cone-12-surface", long, emitter(cylinder, 0.0, 1, 0.0, 0.05), 2000, &["side"]),
	];
	// the view runs along +Z: the part's X axis turned onto it (face), as it is (side), its Y axis
	// onto it (top)
	let views: [(&str, Affine3A); 3] = [
		("face", Affine3A::from_rotation_y(-std::f32::consts::FRAC_PI_2)),
		("side", Affine3A::IDENTITY),
		("top", Affine3A::from_rotation_x(std::f32::consts::FRAC_PI_2)),
	];
	let mut runs = Vec::new();
	for (column, (label, size, set, count, shown)) in cases.iter().enumerate() {
		// what the particles fill: the part's half extent, and 5 studs more where they leave
		let reach = size.max_element() * 0.5 + if label.starts_with("leaving") { 5.0 } else { 0.0 } + 1.0;
		for (row, (view, turn)) in views.iter().enumerate() {
			if !shown.contains(view) {
				continue;
			}
			let center = Vec3::new((column as f32 - 6.0) * 70.0, row as f32 * -70.0, 0.0);
			let emitter = format!("{label}-{view}");
			add_part_emitter(&mut model, &emitter, Affine3A::from_translation(center) * *turn, *size, set);
			// a narrow view from far off (little perspective), the reach filling three quarters of it
			runs.push(Run { label: emitter.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: [center.x, center.y, -reach * 7.4], target: center.to_array(), fov: 20.0 } });
			register(name, &emitter, staggered(&[(0.5, 0)], &[(emitter.clone(), *count)]));
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -70.0, 80.0)), size: Vec3::new(1000.0, 400.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Where along a Cylinder its particles start, by ShapePartial: 12 x 2 x 2 Cylinders at -0.5, 0, 0.1,
/// 0.25, 0.5, 0.75, 0.9, 1.5 and 2, surface ones at 0, 0.5 and 2, cones 27 and 4 studs long and one 8
/// across, 2000 faint additive particles each (Transparency 0.95: their summed light counts them
/// unclipped) frozen where they spawn, each alone in its run, seen from the side from far off.
pub fn particle_cylinder_axis() -> Experiment {
	let name = "particle-cylinder-axis";
	let mut model = ModelBuilder::new("ParticleCylinderAxis");
	let long = Vec3::new(12.0, 2.0, 2.0);
	// (label, part size, ShapePartial, ShapeStyle)
	let cases: [(&str, Vec3, f32, u32); 15] = [
		("p-0.5", long, -0.5, 0),
		("p0", long, 0.0, 0),
		("p0.1", long, 0.1, 0),
		("p0.25", long, 0.25, 0),
		("p0.5", long, 0.5, 0),
		("p0.75", long, 0.75, 0),
		("p0.9", long, 0.9, 0),
		("p1.5", long, 1.5, 0),
		("p2", long, 2.0, 0),
		("p0-surface", long, 0.0, 1),
		("p0.5-surface", long, 0.5, 1),
		("p2-surface", long, 2.0, 1),
		("p0-27", Vec3::new(27.0, 2.0, 2.0), 0.0, 0),
		("p0-4", Vec3::new(4.0, 2.0, 2.0), 0.0, 0),
		("p0-fat", Vec3::new(12.0, 8.0, 8.0), 0.0, 0),
	];
	let mut runs = Vec::new();
	for (index, (label, size, partial, style)) in cases.iter().enumerate() {
		let set: Vec<(&str, Variant)> = vec![
			("Size", constant(0.05)),
			("LightEmission", 1.0f32.into()),
			("Transparency", constant(0.8)),
			("Shape", Enum::from_u32(2).into()),
			("ShapePartial", (*partial).into()),
			("ShapeStyle", Enum::from_u32(*style).into()),
		];
		let center = Vec3::new((index as f32 - 7.0) * 40.0, 0.0, 0.0);
		add_part_emitter(&mut model, label, Affine3A::from_translation(center), *size, &set);
		let reach = size.max_element() * 0.5 + 1.0;
		runs.push(Run { label: (*label).into(), environment: dark(), camera: CameraSpec::LookAt { eye: [center.x, center.y, -reach * 7.4], target: center.to_array(), fov: 20.0 } });
		register(name, label, staggered(&[(0.5, 0)], &[((*label).to_string(), 2000)]));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 40.0)), size: Vec3::new(800.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Stacked additive light: 1, 2, 3, 5, 10 and 30 particles on one spot (LightEmission 1,
/// Transparency 0.3, Size 0.5), and the same with normal blending, frozen at age 0: how the sum and
/// its bloom grow.
pub fn particle_overlap() -> Experiment {
	let name = "particle-overlap";
	let mut model = ModelBuilder::new("ParticleOverlap");
	let counts = [1u32, 2, 3, 5, 10, 30, 300];
	let mut table = Vec::new();
	// rows: additive, normal, and additive tiny (Size 0.1)
	for (row, prefix, emission, size) in [(0usize, "add", 1.0f32, 0.5f32), (1, "alpha", 0.0, 0.5), (2, "tiny", 1.0, 0.1)] {
		for (index, count) in counts.iter().enumerate() {
			let label = format!("{prefix}{count}");
			add_emitter(
				&mut model,
				&label,
				Affine3A::from_translation(Vec3::new((index as f32 - 3.0) * 5.0, (1.0 - row as f32) * 6.0, 0.0)),
				&[("Size", constant(size)), ("Transparency", constant(0.3)), ("LightEmission", emission.into())],
			);
			table.push((label, *count));
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let runs = vec![Run { label: "stack".into(), environment: flat, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -45.0], target: [0.0, 0.0, 0.0], fov: 30.0 } }];
	register(name, "stack", freeze(0.0, &table));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// Copies of an emitter (a dragon fire effect: three same-named copies on one attachment): two emitters
/// named alike on one attachment, each emitting 20 small dots at 8 studs a second in every direction,
/// against one emitter emitting 20 and one emitting 40; frozen 0.5 s after the emit, seen from the
/// front. Does each copy draw its own dots?
pub fn particle_copies() -> Experiment {
	let name = "particle-copies";
	let mut model = ModelBuilder::new("ParticleCopies");
	let set: Props = vec![
		("Speed", fixed(8.0)),
		("SpreadAngle", rbx_dom_weak::types::Vector2::new(180.0, 180.0).into()),
		("Size", constant(0.12)),
		("LightEmission", 1.0f32.into()),
	];
	// (label, emitters on the attachment, their count each)
	let cases: [(&str, usize, u32); 3] = [("copies", 2, 20), ("one-20", 1, 20), ("one-40", 1, 40)];
	let mut counts = Vec::new();
	let mut runs = Vec::new();
	for (index, (label, emitters, count)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		let anchor = model.part(&PartSpec { name: format!("{label}-anchor"), cframe: Affine3A::from_translation(center), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let attachment = model.child(anchor, InstanceBuilder::new("Attachment").with_name("At"));
		for _ in 0..*emitters {
			model.child(attachment, emitter_builder(label, &set));
		}
		counts.push((label.to_string(), *count));
		runs.push(Run { label: (*label).into(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -16.0)).into(), target: center.into(), fov: 40.0 } });
	}
	for (label, ..) in cases.iter() {
		register(name, label, freeze(0.5, &counts));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(30.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Acceleration on turned emitters (a dragon fire effect: Acceleration (0, 1000, 0) from an attachment whose
/// up is the world's -X, LockedToPart): particles at rest pulled at 20 studs a second² along Y, frozen
/// 0.5 s after the emit (2.5 studs of pull), from an attachment turned 90° about Z (its up the world's
/// -X), locked and free; locked on an attachment not turned; locked on an attachment in a part turned
/// 90° about Z; locked with the dragon's turn (its up the world's -X, its back the world's up). Each
/// seen from the front: which way they went.
pub fn particle_locked_accel() -> Experiment {
	use glam::Mat3;

	let name = "particle-locked-accel";
	let mut model = ModelBuilder::new("ParticleLockedAccel");
	let turned = Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2);
	let dragon = Mat3::from_cols(Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y);
	// (label, part rotation, attachment rotation, LockedToPart)
	let cases: [(&str, Mat3, Mat3, bool); 5] = [
		("locked-turned", Mat3::IDENTITY, turned, true),
		("free-turned", Mat3::IDENTITY, turned, false),
		("locked-still", Mat3::IDENTITY, Mat3::IDENTITY, true),
		("locked-part-turned", turned, Mat3::IDENTITY, true),
		("locked-dragon", Mat3::IDENTITY, dragon, true),
	];
	let counts: Vec<(String, u32)> = cases.iter().map(|(label, ..)| (label.to_string(), 5)).collect();
	let mut runs = Vec::new();
	for (index, (label, part_rotation, rotation, locked)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 20.0, 0.0, 0.0);
		let set: Props = vec![
			("Size", constant(0.2)),
			("LightEmission", 1.0f32.into()),
			("Acceleration", rbx_dom_weak::types::Vector3::new(0.0, 20.0, 0.0).into()),
			("LockedToPart", (*locked).into()),
		];
		let anchor = model.part(&PartSpec { name: format!("{label}-anchor"), cframe: Affine3A::from_mat3_translation(*part_rotation, center), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let attachment = model.child(anchor, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", cframe(Affine3A::from_mat3(*rotation))));
		model.child(attachment, emitter_builder(label, &set));
		runs.push(Run { label: (*label).into(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -20.0)).into(), target: center.into(), fov: 40.0 } });
		register(name, label, freeze(0.5, &counts));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(40.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// SpreadAngle on turned emitters (a dragon fire effect: SpreadAngle (0, 180) from EmissionDirection Back
/// on an attachment turned so that Back points up fans along the flight in Studio, across it in ror):
/// 300 small particles at 10 studs a second, frozen 0.5 s after the emit, on attachments turned as the
/// dragon's (right -Z, up -X, back +Y), not turned, turned 90° about X and 45° about Y, from Back or
/// Top with one spread axis at 90°; and as the dragon's are, an attachment turned in a turned part,
/// Shape Box and Disc, the spread at 90° and 180°; each seen from the front and from above: the plane
/// the fan lies in.
pub fn particle_spread_frames() -> Experiment {
	use glam::Mat3;

	let name = "particle-spread-frames";
	let mut model = ModelBuilder::new("ParticleSpreadFrames");
	let dragon = Mat3::from_cols(Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y);
	// the dragon's dark part (right +X, up -Z, back +Y) and its attachment within it (right +Y, up -X)
	let dragon_part = Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y);
	let dragon_attachment = Mat3::from_cols(Vec3::Y, Vec3::NEG_X, Vec3::Z);
	// (label, part rotation, attachment rotation, EmissionDirection, SpreadAngle, Shape)
	let cases: [(&str, Mat3, Mat3, u32, (f32, f32), u32); 10] = [
		("dragon-y", Mat3::IDENTITY, dragon, 2, (0.0, 90.0), 0),
		("dragon-x", Mat3::IDENTITY, dragon, 2, (90.0, 0.0), 0),
		("still-y", Mat3::IDENTITY, Mat3::IDENTITY, 2, (0.0, 90.0), 0),
		("pitch-y", Mat3::IDENTITY, Mat3::from_rotation_x(std::f32::consts::FRAC_PI_2), 2, (0.0, 90.0), 0),
		("yaw-top-y", Mat3::IDENTITY, Mat3::from_rotation_y(std::f32::consts::FRAC_PI_4), 1, (0.0, 90.0), 0),
		("yaw-top-x", Mat3::IDENTITY, Mat3::from_rotation_y(std::f32::consts::FRAC_PI_4), 1, (90.0, 0.0), 0),
		("part-y", dragon_part, dragon_attachment, 2, (0.0, 90.0), 0),
		("part-disc-y", dragon_part, dragon_attachment, 2, (0.0, 90.0), 3),
		("disc-y", Mat3::IDENTITY, dragon, 2, (0.0, 90.0), 3),
		("part-y180", dragon_part, dragon_attachment, 2, (0.0, 180.0), 0),
	];
	let counts: Vec<(String, u32)> = cases.iter().map(|(label, ..)| (label.to_string(), 300)).collect();
	let mut runs = Vec::new();
	for (index, (label, part_rotation, rotation, direction, (x, y), shape)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		let set: Props = vec![
			("Speed", fixed(10.0)),
			("Size", constant(0.1)),
			("LightEmission", 1.0f32.into()),
			("EmissionDirection", Enum::from_u32(*direction).into()),
			("SpreadAngle", rbx_dom_weak::types::Vector2::new(*x, *y).into()),
			("Shape", Enum::from_u32(*shape).into()),
		];
		let anchor = model.part(&PartSpec { name: format!("{label}-anchor"), cframe: Affine3A::from_mat3_translation(*part_rotation, center), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let attachment = model.child(anchor, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", cframe(Affine3A::from_mat3(*rotation))));
		model.child(attachment, emitter_builder(label, &set));
		for (view, eye) in [("front", Vec3::new(0.0, 0.0, -25.0)), ("top", Vec3::new(0.0, 25.0, -0.01))] {
			let run = format!("{label}-{view}");
			runs.push(Run { label: run.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + eye).into(), target: center.into(), fov: 40.0 } });
			register(name, &run, freeze(0.5, &counts));
		}
	}
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(75.0, -30.0, 0.0)), size: Vec3::new(400.0, 1.0, 200.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(75.0, 0.0, 30.0)), size: Vec3::new(400.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Drag on a burst (an effect's sparks: Drag 10 at 17-61 studs a second spread further in Studio): 300
/// small additive particles at a fixed 20 studs a second in every direction, Drag 0, 3, 10, 20, 60 and
/// 115.2 (another effect's), 40 studs apart, frozen 0.1 and 0.4 s after the emit, each emitter its own run
/// framed on it: the shell they lie on shows how far they went.
pub fn particle_drag() -> Experiment {
	let name = "particle-drag";
	let mut model = ModelBuilder::new("ParticleDrag");
	let drags = [0.0f32, 3.0, 10.0, 20.0, 60.0, 115.2];
	let counts: Vec<(String, u32)> = drags.iter().map(|drag| (format!("drag{drag}"), 300)).collect();
	let mut runs = Vec::new();
	for (index, drag) in drags.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		let set: Props = vec![
			("Speed", fixed(20.0)),
			("SpreadAngle", rbx_dom_weak::types::Vector2::new(180.0, 180.0).into()),
			("Drag", (*drag).into()),
			("Size", constant(0.2)),
			("LightEmission", 1.0f32.into()),
			("Lifetime", NumberRange::new(5.0, 5.0).into()),
		];
		add_emitter(&mut model, &format!("drag{drag}"), Affine3A::from_translation(center), &set);
		for age in [0.1f32, 0.4] {
			let label = format!("drag{drag}-{age}");
			runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -26.0)).into(), target: center.into(), fov: 40.0 } });
			register(name, &label, freeze(age, &counts));
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(60.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Drag with Acceleration (a dragon fire effect: 1000 studs a second² up at Drag 7): particles at rest
/// pulled up at 40 studs a second², Drag 0, 3, 10, 20 and 60, 40 studs apart, frozen 0.2 and 0.4 s
/// after the emit, each emitter its own run framed on it: how high they rose.
pub fn particle_drag_accel() -> Experiment {
	let name = "particle-drag-accel";
	let mut model = ModelBuilder::new("ParticleDragAccel");
	let drags = [0.0f32, 3.0, 10.0, 20.0, 60.0];
	let counts: Vec<(String, u32)> = drags.iter().map(|drag| (format!("drag{drag}"), 3)).collect();
	let mut runs = Vec::new();
	for (index, drag) in drags.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		let set: Props = vec![
			("Acceleration", rbx_dom_weak::types::Vector3::new(0.0, 40.0, 0.0).into()),
			("Drag", (*drag).into()),
			("Size", constant(0.15)),
			("LightEmission", 1.0f32.into()),
			("Lifetime", NumberRange::new(5.0, 5.0).into()),
		];
		add_emitter(&mut model, &format!("drag{drag}"), Affine3A::from_translation(center + Vec3::new(0.0, -2.0, 0.0)), &set);
		for age in [0.2f32, 0.4] {
			let label = format!("drag{drag}-{age}");
			runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -26.0)).into(), target: center.into(), fov: 40.0 } });
			register(name, &label, freeze(age, &counts));
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(80.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// Soft particles: a red particle (Size 0.75) at 0.1 to 3 studs in front of a white SmoothPlastic
/// block, additive ones in front of a black block, and one 2 studs in front pushed back to 0.5 by
/// ZOffset, each seen head-on and frozen at age 0: how its opacity fades with the gap to the
/// surface behind it (`particle-sort` clothes-front: half opacity at 0.5 studs).
pub fn particle_soft() -> Experiment {
	let name = "particle-soft";
	let mut model = ModelBuilder::new("ParticleSoft");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	// (label, gap in front of the block, ZOffset, additive, block colour)
	let cases: [(&str, f32, f32, bool, [u8; 3]); 11] = [
		("gap0.1", 0.1, 0.0, false, [255, 255, 255]),
		("gap0.25", 0.25, 0.0, false, [255, 255, 255]),
		("gap0.5", 0.5, 0.0, false, [255, 255, 255]),
		("gap0.75", 0.75, 0.0, false, [255, 255, 255]),
		("gap1", 1.0, 0.0, false, [255, 255, 255]),
		("gap1.5", 1.5, 0.0, false, [255, 255, 255]),
		("gap3", 3.0, 0.0, false, [255, 255, 255]),
		("add-gap0.25", 0.25, 0.0, true, [0, 0, 0]),
		("add-gap0.5", 0.5, 0.0, true, [0, 0, 0]),
		("add-gap3", 3.0, 0.0, true, [0, 0, 0]),
		("zoffset-gap0.5", 2.0, -1.5, false, [255, 255, 255]),
	];
	let mut runs = Vec::new();
	for (index, (label, gap, z_offset, additive, color)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 20.0, 0.0, 0.0);
		// the block's front face at the centre's depth
		model.part(&PartSpec { name: label.into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(6.0, 6.0, 1.0), color, material: 272, cast_shadow: false, ..Default::default() });
		let set: Props = vec![
			("Size", constant(0.75)),
			("Color", solid([1.0, 0.0, 0.0])),
			("ZOffset", z_offset.into()),
			("LightEmission", if additive { 1.0f32 } else { 0.0 }.into()),
		];
		add_emitter(&mut model, &format!("{label}-particle"), Affine3A::from_translation(center + Vec3::new(0.0, 0.0, -gap)), &set);
		runs.push(Run { label: label.into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -40.0)).into(), target: center.into(), fov: 20.0 } });
		register(name, label, freeze(0.0, &[]));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(100.0, 0.0, 12.0)), size: Vec3::new(300.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// High Brightness: faint red particles (Transparency 0.9) at Brightness 1 to 50, additive above and
/// alpha-blended below, over black, frozen at age 0 (an aura's Brightness 25 particles drew 2.5x
/// Studio's red: is the emission capped, as transparent Neon's is?).
pub fn particle_bright() -> Experiment {
	let name = "particle-bright";
	let mut model = ModelBuilder::new("ParticleBright");
	let brightnesses = [1.0f32, 4.0, 8.0, 15.0, 25.0, 50.0];
	for (row, emission) in [(0usize, 1.0f32), (1, 0.0)] {
		for (index, brightness) in brightnesses.iter().enumerate() {
			let label = format!("{}{brightness}", if emission > 0.0 { "add" } else { "alpha" });
			let set: Props = vec![
				("Size", constant(1.0)),
				("Color", solid([1.0, 0.0, 0.0])),
				("Transparency", constant(0.9)),
				("LightEmission", emission.into()),
				("Brightness", (*brightness).into()),
			];
			add_emitter(&mut model, &label, Affine3A::from_translation(Vec3::new((index as f32 - 2.5) * 4.0, (0.5 - row as f32) * 5.0, 0.0)), &set);
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let runs = vec![Run { label: "rows".into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -45.0], target: [0.0, 0.0, 0.0], fov: 30.0 } }];
	register(name, "rows", freeze(0.0, &[]));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// The Brightness cap with a gray colour: faint additive particles (Transparency 0.9) of Color 0.5
/// (albedo 0.25) at Brightness 4 to 32, frozen at age 0 (a cap on the emitted colour lets them
/// brighten to Brightness 16; one on Brightness alone stops them at 4).
pub fn particle_bright_gray() -> Experiment {
	let name = "particle-bright-gray";
	let mut model = ModelBuilder::new("ParticleBrightGray");
	for (index, brightness) in [4.0f32, 8.0, 16.0, 32.0].iter().enumerate() {
		let set: Props = vec![
			("Size", constant(1.0)),
			("Color", solid([0.5, 0.5, 0.5])),
			("Transparency", constant(0.9)),
			("LightEmission", 1.0f32.into()),
			("Brightness", (*brightness).into()),
		];
		add_emitter(&mut model, &format!("gray{brightness}"), Affine3A::from_translation(Vec3::new((index as f32 - 1.5) * 4.0, 0.0, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let runs = vec![Run { label: "row".into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -45.0], target: [0.0, 0.0, 0.0], fov: 30.0 } }];
	register(name, "row", freeze(0.0, &[]));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// LightInfluence against Brightness: faint squares (Transparency 0.8) of the cutscene flare's blue
/// at LightInfluence 0 / 0.25 / 0.5 / 0.7 / 1 (rows) and Brightness 1 / 4 / 15 (columns), additive and
/// alpha-blended, in the dark (no lit share) and under a flat 0.5 ambient, frozen at age 0 (how much
/// of the emission does LightInfluence keep, and does Brightness reach the lit share?).
pub fn particle_influence() -> Experiment {
	let name = "particle-influence";
	let mut model = ModelBuilder::new("ParticleInfluence");
	let influences = [0.0f32, 0.25, 0.5, 0.7, 1.0];
	let brightnesses = [1.0f32, 4.0, 15.0];
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for (block, emission) in [(0usize, 1.0f32), (1, 0.0)] {
		let center = Vec3::new(block as f32 * 40.0, 0.0, 0.0);
		for (row, influence) in influences.iter().enumerate() {
			for (column, brightness) in brightnesses.iter().enumerate() {
				let set: Props = vec![
					("Size", constant(1.0)),
					("Color", solid([0.47, 0.48, 1.0])),
					("Transparency", constant(0.8)),
					("LightEmission", emission.into()),
					("LightInfluence", (*influence).into()),
					("Brightness", (*brightness).into()),
				];
				let at = center + Vec3::new((1.0 - column as f32) * 3.0, (2.0 - row as f32) * 3.0, 0.0);
				add_emitter(&mut model, &format!("b{block}-li{influence}-br{brightness}"), Affine3A::from_translation(at), &set);
			}
		}
		for (label, environment) in [("dark", dark()), ("flat", flat.clone())] {
			let run = format!("{}-{label}", if emission > 0.0 { "add" } else { "alpha" });
			runs.push(Run { label: run.clone(), environment, camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -45.0)).into(), target: center.into(), fov: 22.0 } });
			register(name, &run, freeze(0.0, &[]));
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(20.0, 0.0, 10.0)), size: Vec3::new(300.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// ZOffset past the camera: additive white squares 10 studs from the camera whose ZOffset brings them
/// to 9, 1, 0.5, 0.2-0.1, 0.05 studs from it and past it (10.5, 15, 50, a cutscene flare's), each
/// alone in its run (an emitter so close covers the view), over a black wall.
pub fn particle_zoffset_near() -> Experiment {
	let name = "particle-zoffset-near";
	let mut model = ModelBuilder::new("ParticleZOffsetNear");
	let offsets = [0.0f32, 1.0, 9.0, 9.5, 9.8, 9.82, 9.85, 9.88, 9.9, 9.95, 10.5, 15.0, 50.0];
	let mut runs = Vec::new();
	for (index, offset) in offsets.iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let set: Props = vec![("Size", constant(0.4)), ("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into()), ("ZOffset", (*offset).into())];
		add_emitter(&mut model, &format!("z{offset}"), Affine3A::from_translation(center), &set);
		let run = format!("z{offset}");
		runs.push(Run { label: run.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -10.0)).into(), target: center.into(), fov: 40.0 } });
		register(name, &run, freeze(0.0, &[]));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(650.0, 0.0, 30.0)), size: Vec3::new(1600.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// ZOffset pushing close particles away: additive white squares (Size 4) 0.3-5 studs before the camera,
/// beside it and behind it at ZOffset 0, -2 and -10 (a cutscene keeps its camera inside its
/// Sun's emitters, all at ZOffset -10), over a black wall 40 studs off; the `-occ` runs put a gray
/// Neon wall 6 studs before the camera, between the particle and where ZOffset -10 takes it.
pub fn particle_zoffset_close() -> Experiment {
	let name = "particle-zoffset-close";
	let mut model = ModelBuilder::new("ParticleZOffsetClose");
	// (label, the particle from the eye: right, up, forward; ZOffset; a near wall)
	let cases: [(&str, Vec3, f32, bool); 24] = [
		("u1-f0.05-zm10", Vec3::new(0.0, 1.0, 0.05), -10.0, false),
		("u1-f0.1-zm10", Vec3::new(0.0, 1.0, 0.1), -10.0, false),
		("u1-f0.15-zm10", Vec3::new(0.0, 1.0, 0.15), -10.0, false),
		("u1-f0.3-zm10", Vec3::new(0.0, 1.0, 0.3), -10.0, false),
		("u1-f0.05-z0", Vec3::new(0.0, 1.0, 0.05), 0.0, false),
		("u1-f0.15-z0", Vec3::new(0.0, 1.0, 0.15), 0.0, false),
		("u1-f0.05-zm10-occ", Vec3::new(0.0, 1.0, 0.05), -10.0, true),
		("u1-f0.3-zm10-occ", Vec3::new(0.0, 1.0, 0.3), -10.0, true),
		("r3-f4-zm6", Vec3::new(3.0, 0.0, 4.0), -6.0, false),
		("r3-f4-z2", Vec3::new(3.0, 0.0, 4.0), 2.0, false),
		("f1-z0", Vec3::new(0.0, 0.0, 1.0), 0.0, false),
		("f1-zm2", Vec3::new(0.0, 0.0, 1.0), -2.0, false),
		("f1-zm10", Vec3::new(0.0, 0.0, 1.0), -10.0, false),
		("f0.3-z0", Vec3::new(0.0, 0.0, 0.3), 0.0, false),
		("f0.3-zm10", Vec3::new(0.0, 0.0, 0.3), -10.0, false),
		("f5-z0", Vec3::new(0.0, 0.0, 5.0), 0.0, false),
		("f5-zm10", Vec3::new(0.0, 0.0, 5.0), -10.0, false),
		("side-zm10", Vec3::new(1.0, 0.0, 0.2), -10.0, false),
		("b1-z0", Vec3::new(0.0, 0.0, -1.0), 0.0, false),
		("b1-zm10", Vec3::new(0.0, 0.0, -1.0), -10.0, false),
		("f1-z0-occ", Vec3::new(0.0, 0.0, 1.0), 0.0, true),
		("f1-zm10-occ", Vec3::new(0.0, 0.0, 1.0), -10.0, true),
		("f5-zm10-occ", Vec3::new(0.0, 0.0, 5.0), -10.0, true),
		("f1-zm2-occ", Vec3::new(0.0, 0.0, 1.0), -2.0, true),
	];
	let mut runs = Vec::new();
	for (index, (label, offset, z_offset, wall)) in cases.iter().enumerate() {
		// the eye at (100 i, 0, 200 i) looking along +Z, each run's own wall 40 studs ahead: another
		// run's squares sit behind its camera or 200 studs ahead and off its view, never at a depth
		// near 0 (Studio draws such a square of another run across a view); Roblox's right is -X here
		let eye = Vec3::new(index as f32 * 100.0, 0.0, index as f32 * 200.0);
		let at = eye + Vec3::new(-offset.x, offset.y, offset.z);
		let set: Props = vec![("Size", constant(4.0)), ("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into()), ("ZOffset", (*z_offset).into())];
		add_emitter(&mut model, label, Affine3A::from_translation(at), &set);
		if *wall {
			model.part(&PartSpec { name: format!("{label}-near"), cframe: Affine3A::from_translation(eye + Vec3::new(1.5, 0.0, 6.0)), size: Vec3::new(3.0, 30.0, 0.2), color: [90, 90, 90], material: 288, cast_shadow: false, ..Default::default() });
		}
		model.part(&PartSpec { name: format!("{label}-wall"), cframe: Affine3A::from_translation(eye + Vec3::new(0.0, 0.0, 40.0)), size: Vec3::new(120.0, 120.0, 1.0), color: [0, 0, 0], ..Default::default() });
		runs.push(Run { label: (*label).into(), environment: dark(), camera: CameraSpec::LookAt { eye: eye.into(), target: (eye + Vec3::Z * 10.0).into(), fov: 70.0 } });
		register(name, label, freeze(0.0, &[]));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Faint particles stacked: white additive squares (Brightness 1) whose opacity is 0.5 to 3.5 of 255
/// (columns), 1, 10 and 40 of them at one place (rows), over a black wall and a lit gray one, frozen
/// at age 0 (does Studio keep an opacity's fraction of a 255th, or cut it off?).
pub fn particle_faint_stack() -> Experiment {
	let name = "particle-faint-stack";
	let mut model = ModelBuilder::new("ParticleFaintStack");
	// opacities in 255ths
	let levels = [0.5f32, 0.9, 0.99, 1.01, 1.5, 1.99, 2.01, 3.5];
	let stacks = [1u32, 10, 40];
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut counts = Vec::new();
	let mut runs = Vec::new();
	for (block, (label, shade)) in [("black", 0u8), ("gray", 128)].iter().enumerate() {
		let center = Vec3::new(block as f32 * 60.0, 0.0, 0.0);
		for (row, stack) in stacks.iter().enumerate() {
			for (column, level) in levels.iter().enumerate() {
				let opacity = level / 255.0;
				let set: Props = vec![
					("Size", constant(1.0)),
					("Transparency", constant(1.0 - opacity)),
					("LightEmission", 1.0f32.into()),
				];
				let emitter = format!("{label}-n{stack}-l{level}");
				counts.push((emitter.clone(), *stack));
				add_emitter(&mut model, &emitter, Affine3A::from_translation(center + Vec3::new((3.5 - column as f32) * 3.0, (1.0 - row as f32) * 3.0, 0.0)), &set);
			}
		}
		model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 6.0)), size: Vec3::new(50.0, 40.0, 1.0), color: [*shade; 3], ..Default::default() });
		runs.push(Run { label: (*label).into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -45.0)).into(), target: center.into(), fov: 25.0 } });
	}
	for run in &runs {
		register(name, &run.label, freeze(0.0, &counts));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Overdraw: K faint white additive particles (opacity 1.01 / 255, Size 60: two of the target's steps
/// each, particle-quantize) stacked over the whole view, K = 50, 150, 300 and 600, frozen at age 0
/// (each covers the view once: does Studio draw them all, past ror's 128 viewports?).
pub fn particle_overdraw() -> Experiment {
	let name = "particle-overdraw";
	let mut model = ModelBuilder::new("ParticleOverdraw");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut counts = Vec::new();
	let mut runs = Vec::new();
	for (index, stack) in [50u32, 150, 300, 600].iter().enumerate() {
		let center = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		let set: Props = vec![("Size", constant(60.0)), ("Transparency", constant(1.0 - 1.01 / 255.0)), ("LightEmission", 1.0f32.into())];
		let emitter = format!("n{stack}");
		counts.push((emitter.clone(), *stack));
		add_emitter(&mut model, &emitter, Affine3A::from_translation(center), &set);
		model.part(&PartSpec { name: format!("Wall-{stack}"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 40.0)), size: Vec3::new(150.0, 150.0, 1.0), color: [0, 0, 0], cast_shadow: false, ..Default::default() });
		runs.push(Run { label: emitter, environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -45.0)).into(), target: center.into(), fov: 25.0 } });
	}
	for run in &runs {
		register(name, &run.label, freeze(0.0, &counts));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// What a faint additive particle adds, per particle: stacks of 1, 10 and 50 white-square particles
/// (Brightness 1, LightEmission 1) at opacity 1.01 and 2.01 / 255 (counted as 1 and 2 / 255) in grays
/// 0.2 to 1, so each adds 0.28 to 2.8 / 255 of light in the √ target, over black, frozen at age 0
/// (does the target keep what one particle adds below 1 / 255?).
pub fn particle_quantize() -> Experiment {
	let name = "particle-quantize";
	let mut model = ModelBuilder::new("ParticleQuantize");
	let grays = [0.2f32, 0.36, 0.5, 0.72, 1.0];
	let opacities = [1.01f32, 2.01];
	let stacks = [1u32, 10, 50];
	// (stack, opacity, Brightness): the stacks above at Brightness 1, then 50 at Brightness 4 and 0.25
	let rows: Vec<(u32, f32, f32)> = stacks.iter().flat_map(|stack| opacities.iter().map(move |opacity| (*stack, *opacity, 1.0f32))).chain([(50, 1.01, 4.0), (50, 2.01, 4.0), (50, 2.01, 0.25), (50, 6.01, 0.25)]).collect();
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut counts = Vec::new();
	let mut probes = Vec::new();
	for (row, (stack, opacity, brightness)) in rows.into_iter().enumerate() {
		for (column, gray) in grays.iter().enumerate() {
			let set: Props = vec![("Size", constant(1.2)), ("Transparency", constant(1.0 - opacity / 255.0)), ("LightEmission", 1.0f32.into()), ("Color", solid([*gray; 3])), ("Brightness", brightness.into())];
			let emitter = format!("n{stack}-o{opacity}-g{gray}-b{brightness}");
			counts.push((emitter.clone(), stack));
			let position = Vec3::new((2.0 - column as f32) * 2.5, (4.5 - row as f32) * 2.0, 0.0);
			add_emitter(&mut model, &emitter, Affine3A::from_translation(position), &set);
			probes.push(super::Probe { label: emitter, position: position.into(), inputs: serde_json::json!({"stack": stack, "opacity": opacity, "gray": gray, "brightness": brightness}) });
		}
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)), size: Vec3::new(60.0, 40.0, 1.0), color: [0, 0, 0], cast_shadow: false, ..Default::default() });
	let runs = vec![Run { label: "black".into(), environment: flat, camera: CameraSpec::LookAt { eye: [0.0, -4.0, -75.0], target: [0.0, -4.0, 0.0], fov: 25.0 } }];
	register(name, "black", freeze(0.0, &counts));
	Experiment { name: name.into(), model, probes, runs }
}


/// How many particles Studio draws at once: small white additive squares (Size 0.25, Transparency 0.5)
/// spread over a 24 x 16 box's volume facing the camera, K = 500 to 8,000 emitted at once from one
/// emitter and from ten (K / 10 each), frozen at age 0 (does the light grow with K all the way?).
pub fn particle_budget() -> Experiment {
	let name = "particle-budget";
	let mut model = ModelBuilder::new("ParticleBudget");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut counts = Vec::new();
	let mut runs = Vec::new();
	let set: Props = vec![("Size", constant(0.25)), ("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into())];
	for (index, (total, emitters)) in [500u32, 1000, 2000, 4000, 8000].iter().flat_map(|total| [(*total, 1u32), (*total, 10)]).enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let label = format!("k{total}-e{emitters}");
		for emitter in 0..emitters {
			let emitter_name = format!("{label}-{emitter}");
			counts.push((emitter_name.clone(), total / emitters));
			add_part_emitter(&mut model, &emitter_name, Affine3A::from_translation(center), Vec3::new(24.0, 16.0, 0.2), &set);
		}
		model.part(&PartSpec { name: format!("Wall-{label}"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 30.0)), size: Vec3::new(80.0, 80.0, 1.0), color: [0, 0, 0], cast_shadow: false, ..Default::default() });
		runs.push(Run { label: label.clone(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -45.0)).into(), target: center.into(), fov: 25.0 } });
	}
	for run in &runs {
		register(name, &run.label, freeze(0.0, &counts));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// How far beside the view an emitter still runs: emitters 1, 5, 10 and 20 studs past the view's left
/// edge (rows) sending particles across it at 5 studs a second for 3 s (15 studs; the last row at 10
/// studs a second, 30), Rate 10, watched for 3 s from an empty start (which of them does Studio update?).
pub fn particle_offscreen_reach() -> Experiment {
	let name = "particle-offscreen-reach";
	let mut model = ModelBuilder::new("ParticleOffscreenReach");
	// the view's half width at the emitters' depth: 25 studs off at 30 degrees, 1644 x 1116
	let half = 25.0 * (15.0f32.to_radians()).tan() * 1644.0 / 1116.0;
	let cases: [(&str, f32, f32); 5] = [("p1", 1.0, 5.0), ("p5", 5.0, 5.0), ("p10", 10.0, 5.0), ("p20", 20.0, 5.0), ("p20-fast", 20.0, 10.0)];
	for (row, (label, past, speed)) in cases.iter().enumerate() {
		let set: Props = vec![
			("Rate", 10.0f32.into()),
			("Lifetime", NumberRange::new(3.0, 3.0).into()),
			("Speed", fixed(*speed)),
			("Size", constant(0.15)),
			("LightEmission", 1.0f32.into()),
			("EmissionDirection", Enum::from_u32(3).into()),
		];
		add_emitter(&mut model, label, Affine3A::from_translation(Vec3::new(half + past, 4.0 - row as f32 * 2.0, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let luau = "local emitters = {}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.Enabled = false
		e.TimeScale = 1
		e:Clear()
		table.insert(emitters, e)
	end
end
task.wait(0.3)
for _, e in emitters do
	e.Enabled = true
end
task.wait(3)
for _, e in emitters do
	e.TimeScale = 0
end
return #emitters"
		.to_string();
	let runs = vec![Run { label: "watched".into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -25.0], target: [0.0, 0.0, 0.0], fov: 30.0 } }];
	register(name, "watched", Setup { luau, bursts: Vec::new(), counts: Vec::new(), seeds: 0 });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Particles near the camera: a white additive square (Transparency 0.5) 0.3 to 16 studs in front of
/// the camera, its Size a share of that distance so that it covers the same part of the view, frozen at
/// age 0 (does Studio fade what comes close to the camera?).
pub fn particle_near() -> Experiment {
	let name = "particle-near";
	let mut model = ModelBuilder::new("ParticleNear");
	let distances = [0.3f32, 0.6, 1.0, 2.0, 4.0, 8.0, 16.0];
	let mut runs = Vec::new();
	for (index, distance) in distances.iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let set: Props = vec![("Size", constant(0.06 * distance)), ("Transparency", constant(0.5)), ("LightEmission", 1.0f32.into())];
		add_emitter(&mut model, &format!("d{distance}"), Affine3A::from_translation(center), &set);
		model.part(&PartSpec { name: format!("Wall{index}"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 30.0)), size: Vec3::new(60.0, 60.0, 1.0), color: [0, 0, 0], ..Default::default() });
		let label = format!("d{distance}");
		runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: (center - Vec3::new(0.0, 0.0, *distance)).into(), target: center.into(), fov: 30.0 } });
		register(name, &label, freeze(0.0, &[]));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Negative Drag: single particles sent sideways at 4 studs a second with Drag 0, -0.5, -1 and -2 (and
/// one pulled up at 4 studs a second² with Drag -2), frozen 0.5 and 1 s after the emit (does the speed
/// double every 1 / |Drag| seconds, or does a negative Drag count as none?).
pub fn particle_drag_negative() -> Experiment {
	let name = "particle-drag-negative";
	let mut model = ModelBuilder::new("ParticleDragNegative");
	let cases: [(&str, f32, f32); 5] = [("d0", 0.0, 0.0), ("dm0.5", -0.5, 0.0), ("dm1", -1.0, 0.0), ("dm2", -2.0, 0.0), ("dm2-pull", -2.0, 4.0)];
	for (row, (label, drag, pull)) in cases.iter().enumerate() {
		let set: Props = vec![
			("Size", constant(0.12)),
			("LightEmission", 1.0f32.into()),
			("Speed", fixed(if *pull > 0.0 { 0.0 } else { 4.0 })),
			("EmissionDirection", Enum::from_u32(0).into()),
			("Drag", (*drag).into()),
			("Acceleration", Variant::Vector3(rbx_dom_weak::types::Vector3::new(0.0, *pull, 0.0))),
		];
		add_emitter(&mut model, label, Affine3A::from_translation(Vec3::new(-6.0, 4.0 - row as f32 * 2.0, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let mut runs = Vec::new();
	for age in [0.5f32, 1.0] {
		let label = format!("age{age}");
		runs.push(Run { label: label.clone(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0, 0.0, 0.0], fov: 30.0 } });
		register(name, &label, freeze(age, &[]));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Emitters that run off screen: Rate 10 columns rising at 2 studs a second (Lifetime 2; one at
/// TimeScale 0.2) and a burst of 20, started while the camera looks the other way, the camera turned
/// back onto them after 3 s and the particles frozen `delay` seconds later (`onscreen`: watched all
/// along). Does Studio emit and age what it does not draw (a full column), keep what it emitted at
/// age 0 (a heap at the emitter), or emit nothing (an empty column filling from the bottom)?
pub fn particle_offscreen() -> Experiment {
	let name = "particle-offscreen";
	let mut model = ModelBuilder::new("ParticleOffscreen");
	let rising = |time_scale: f32| -> Props {
		vec![
			("Rate", 10.0f32.into()),
			("Lifetime", NumberRange::new(2.0, 2.0).into()),
			("Speed", fixed(2.0)),
			("Size", constant(0.15)),
			("LightEmission", 1.0f32.into()),
			("TimeScale", time_scale.into()),
		]
	};
	add_emitter(&mut model, "rate", Affine3A::from_translation(Vec3::new(3.0, -2.0, 0.0)), &rising(1.0));
	add_emitter(&mut model, "slow", Affine3A::from_translation(Vec3::new(0.0, -2.0, 0.0)), &rising(0.2));
	let burst: Props = vec![("Lifetime", NumberRange::new(10.0, 10.0).into()), ("Speed", NumberRange::new(0.5, 1.0).into()), ("Size", constant(0.15)), ("LightEmission", 1.0f32.into())];
	add_emitter(&mut model, "burst", Affine3A::from_translation(Vec3::new(-3.0, -2.0, 0.0)), &burst);
	// beside the view, 2 studs past its left edge, sending its particles across it (is an emitter whose
	// particles could reach the view updated, or only one whose source is in it?)
	let mut edge = rising(1.0);
	edge.push(("EmissionDirection", Enum::from_u32(3).into()));
	edge.push(("Speed", fixed(5.0)));
	edge.push(("Lifetime", NumberRange::new(3.0, 3.0).into()));
	add_emitter(&mut model, "edge", Affine3A::from_translation(Vec3::new(11.9, 2.0, 0.0)), &edge);
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let mut runs = Vec::new();
	// each run's steps: start (enable the columns, emit the burst), away / back (the camera), a wait
	let cases: [(&str, &str); 7] = [
		("onscreen", "start() wait(3) wait(0.1)"),
		("offscreen-0.1", "away() start() wait(3) back() wait(0.1)"),
		("offscreen-0.5", "away() start() wait(3) back() wait(0.5)"),
		("offscreen-1.5", "away() start() wait(3) back() wait(1.5)"),
		("late-0.5", "away() wait(3) back() start() wait(0.5)"),
		// watched for 1.5 s, unseen for 1 s, frozen as soon as it is back
		("away-between", "start() wait(1.5) away() wait(1) back() wait(0.05)"),
		("watched-2.55", "start() wait(2.55)"),
	];
	for (label, steps) in cases {
		let steps = steps.replace(") ", ")\n").replace("wait(", "task.wait(");
		let luau = format!(
			"local camera = workspace.CurrentCamera
local home = camera.CFrame
local emitters = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.Enabled = false
		e.TimeScale = if e.Name == \"slow\" then 0.2 else 1
		e:Clear()
		emitters[e.Name] = e
	end
end
local function start()
	emitters.rate.Enabled = true
	emitters.slow.Enabled = true
	emitters.edge.Enabled = true
	emitters.burst:Emit(20)
end
local function away()
	camera.CFrame = home * CFrame.Angles(0, math.pi, 0)
end
local function back()
	camera.CFrame = home
end
task.wait(0.3)
{steps}
for _, e in emitters do
	e.TimeScale = 0
end
return 3"
		);
		runs.push(Run { label: label.into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -25.0], target: [0.0, 0.0, 0.0], fov: 30.0 } });
		register(name, label, Setup { luau, bursts: Vec::new(), counts: Vec::new(), seeds: 0 });
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// TimeScale and the steady state: emitters at Rate 2, Lifetime 1 and Speed 3 straight up with
/// TimeScale 1, 0.5 and 0.2, live: does TimeScale slow the emission with the particles (2 alive in a
/// 3-stud column at every TimeScale) or not (4 and 10 alive, closer together)?
pub fn particle_timescale() -> Experiment {
	let name = "particle-timescale";
	let mut model = ModelBuilder::new("ParticleTimeScale");
	for (index, time_scale) in [1.0f32, 0.5, 0.2].iter().enumerate() {
		let set: Props = vec![
			("Rate", 2.0f32.into()),
			("Lifetime", NumberRange::new(1.0, 1.0).into()),
			("Speed", fixed(3.0)),
			("Size", constant(0.2)),
			("LightEmission", 1.0f32.into()),
			("TimeScale", (*time_scale).into()),
		];
		add_emitter(&mut model, &format!("timescale{time_scale}"), Affine3A::from_translation(Vec3::new((index as f32 - 1.0) * 4.0, -1.5, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let runs = vec![Run { label: "live".into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -30.0], target: [0.0, 0.0, 0.0], fov: 25.0 } }];
	register(name, "live", steady(6.0, 8));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Live against frozen: dense streams (Rate 60, Lifetime 1, spread, faded over their life) of additive
/// and blended white squares, at Brightness 1 and 4, in their steady state; `particle-held-live`
/// captures a burst of frames as they play, `particle-held-frozen` freezes them (TimeScale 0) first.
/// Does Studio draw the particles of a held emitter as it draws them playing?
pub fn particle_held(frozen: bool) -> Experiment {
	let name = if frozen { "particle-held-frozen" } else { "particle-held-live" };
	let mut model = ModelBuilder::new("ParticleHeld");
	for (index, (emission, brightness)) in [(1.0f32, 1.0f32), (1.0, 4.0), (0.0, 1.0), (0.0, 4.0)].iter().enumerate() {
		let set: Props = vec![
			("Rate", 60.0f32.into()),
			("Lifetime", NumberRange::new(1.0, 1.0).into()),
			("Speed", fixed(3.0)),
			("SpreadAngle", rbx_dom_weak::types::Vector2::new(25.0, 25.0).into()),
			("Size", constant(0.8)),
			("Transparency", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.6, 0.0), NumberSequenceKeypoint::new(1.0, 1.0, 0.0)] }.into()),
			("Color", solid([0.9, 0.5, 1.0])),
			("LightEmission", (*emission).into()),
			("Brightness", (*brightness).into()),
		];
		add_emitter(&mut model, &format!("e{emission}-b{brightness}"), Affine3A::from_translation(Vec3::new((index as f32 - 1.5) * 6.0, -2.0, 0.0)), &set);
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [40, 40, 60], ..Default::default() });
	let runs = vec![Run { label: "held".into(), environment: dark(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0, 0.0, 0.0], fov: 30.0 } }];
	let mut setup = steady(2.0, 16);
	if frozen {
		setup.luau = setup.luau.replace("return #emitters", "for _, e in emitters do\n\te.TimeScale = 0\nend\ntask.wait(0.3)\nreturn #emitters");
	}
	register(name, "held", setup);
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// Soft particles when big: red particles of Size 6 at 1.5, 3 and 6 studs in front of a white block
/// (does the fade still span the whole Size?), frozen at age 0.
pub fn particle_soft_big() -> Experiment {
	let name = "particle-soft-big";
	let mut model = ModelBuilder::new("ParticleSoftBig");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for (index, gap) in [1.5f32, 3.0, 6.0].iter().enumerate() {
		let label = format!("size6-gap{gap}");
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		model.part(&PartSpec { name: label.clone(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(30.0, 30.0, 1.0), color: [255, 255, 255], material: 272, cast_shadow: false, ..Default::default() });
		let set: Props = vec![("Size", constant(6.0)), ("Color", solid([1.0, 0.0, 0.0]))];
		add_emitter(&mut model, &format!("{label}-particle"), Affine3A::from_translation(center + Vec3::new(0.0, 0.0, -gap)), &set);
		runs.push(Run { label: label.clone(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -80.0)).into(), target: center.into(), fov: 20.0 } });
		register(name, &label, freeze(0.0, &[]));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(60.0, 0.0, 30.0)), size: Vec3::new(300.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Soft particles and size: red particles of Size 2 at 0.5 and 1 stud and of Size 0.25 at 0.1 and
/// 0.2 studs in front of a white block (does the fade scale with the particle's size?), frozen at
/// age 0.
pub fn particle_soft_size() -> Experiment {
	let name = "particle-soft-size";
	let mut model = ModelBuilder::new("ParticleSoftSize");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	// (label, Size, gap)
	let cases: [(&str, f32, f32); 4] = [("size2-gap0.5", 2.0, 0.5), ("size2-gap1", 2.0, 1.0), ("size0.25-gap0.1", 0.25, 0.1), ("size0.25-gap0.2", 0.25, 0.2)];
	let mut runs = Vec::new();
	for (index, (label, size, gap)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 20.0, 0.0, 0.0);
		model.part(&PartSpec { name: label.into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(8.0, 8.0, 1.0), color: [255, 255, 255], material: 272, cast_shadow: false, ..Default::default() });
		let set: Props = vec![("Size", constant(size)), ("Color", solid([1.0, 0.0, 0.0]))];
		add_emitter(&mut model, &format!("{label}-particle"), Affine3A::from_translation(center + Vec3::new(0.0, 0.0, -gap)), &set);
		runs.push(Run { label: label.into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -40.0)).into(), target: center.into(), fov: 20.0 } });
		register(name, label, freeze(0.0, &[]));
	}
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(30.0, 0.0, 12.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}


/// A real model's particles at their steady state (`particle-model:<file>` or
/// `particle-model:<file>|<instance path>` for a part of a place, e.g. an NPC with an aura): the
/// default lighting and sky (a place's own sky need not load in the capture Studio), the default
/// orbit view and a closer one; Studio's mean over a burst of frames against ror's mean over seeded
/// snapshots. Named after the model: `particle-model-<last two names of the path>`.
pub fn particle_model(spec: &str) -> anyhow::Result<Experiment> {
	let (path, select) = match spec.split_once('|') {
		Some((path, select)) => (path, Some(select)),
		None => (spec, None),
	};
	let names: Vec<String> = match select {
		Some(select) => select.replace("[\"", ".").replace("\"]", ".").split('.').filter(|name| !name.is_empty()).map(str::to_string).collect(),
		None => vec![std::path::Path::new(path).file_stem().unwrap_or_default().to_string_lossy().into_owned()],
	};
	let label: String = names[names.len().saturating_sub(2)..].join("-").to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
	let name = format!("particle-model-{label}");
	let name = name.as_str();
	let mut document = crate::scene::load::read_file(std::path::Path::new(path))?;
	if let Some(select) = select {
		document.select(select)?;
	}
	let scene = crate::scene::extract::extract(&document);
	let longest = scene.particles.iter().map(|emitter| emitter.lifetime.0.max(emitter.lifetime.1) / emitter.time_scale.max(0.05)).fold(0.0f32, f32::max);
	let environment = Environment::default();
	let mut model = ModelBuilder::new("ParticleModel");
	let roots = document.roots.clone();
	model.dom = document.dom;
	let container = model.dom.insert(model.dom.root_ref(), InstanceBuilder::new("Model").with_name("ParticleModel"));
	for root in roots {
		model.dom.transfer_within(root, container);
	}
	model.model = container;
	let runs = vec![
		Run { label: "orbit".into(), environment: environment.clone(), camera: CameraSpec::Orbit { yaw: 30.0, pitch: 20.0, distance: None, padding: 0.08, target: None, fov: 50.0 } },
		Run { label: "close".into(), environment, camera: CameraSpec::Orbit { yaw: -20.0, pitch: 10.0, distance: None, padding: 0.3, target: None, fov: 30.0 } },
	];
	for run in &runs {
		register(name, &run.label, steady(longest.min(20.0) + 0.5, 16));
	}
	Ok(Experiment { name: name.into(), model, probes: Vec::new(), runs })
}

/// A scene's emitters in their steady state seen from its own camera (`particle-view:<file>|<instance
/// path>|<eye x,y,z>|<target x,y,z>|<fov>`, e.g. a cutscene record's scene.rbxm and a camera of its
/// track): every emitter under the path switched on at its own TimeScale, over black (a black wall
/// 3,000 studs past the target), in the dark; Studio's mean over a burst of frames against ror's mean
/// over seeded snapshots; a sixth field keeps only the emitters of those names (comma-separated, empty:
/// all) and names the experiment after them, a seventh lays the file's Lighting children at those paths
/// (comma-separated: post effects, an Atmosphere, a Sky) over the dark. Named
/// `particle-view-<last two names of the path>[-<kept>][-lit]`.
pub fn particle_view(spec: &str) -> anyhow::Result<Experiment> {
	let fields: Vec<&str> = spec.split('|').collect();
	anyhow::ensure!((5..=7).contains(&fields.len()), "particle-view:<file>|<instance path>|<eye x,y,z>|<target x,y,z>|<fov>[|<emitter names>[|<Lighting child paths>]]");
	let kept: Option<Vec<&str>> = fields.get(5).filter(|names| !names.trim().is_empty()).map(|names| names.split(',').map(str::trim).collect());
	let mut environment = dark();
	if let Some(paths) = fields.get(6) {
		for path in paths.split(',').map(str::trim).filter(|path| !path.is_empty()) {
			// Lighting's own properties as Name=value (a colour as r/g/b, 0-1)
			if let Some((property, value)) = path.split_once('=') {
				let numbers: Vec<f32> = value.split('/').map(|part| part.trim().parse::<f32>()).collect::<Result<_, _>>()?;
				let color = |numbers: &[f32]| if numbers.len() == 3 { Vec3::new(numbers[0], numbers[1], numbers[2]) } else { Vec3::splat(numbers[0]) };
				match property.trim() {
					"ClockTime" => environment.clock_time = numbers[0],
					"Brightness" => environment.brightness = numbers[0],
					"GeographicLatitude" => environment.geographic_latitude = numbers[0],
					"EnvironmentDiffuseScale" => environment.environment_diffuse_scale = numbers[0],
					"EnvironmentSpecularScale" => environment.environment_specular_scale = numbers[0],
					"ExposureCompensation" => environment.exposure_compensation = numbers[0],
					"Ambient" => environment.ambient = color(&numbers),
					"OutdoorAmbient" => environment.outdoor_ambient = color(&numbers),
					"ColorShift_Top" => environment.color_shift_top = color(&numbers),
					"ColorShift_Bottom" => environment.color_shift_bottom = color(&numbers),
					other => anyhow::bail!("particle-view: no Lighting property {other:?}"),
				}
				continue;
			}
			let mut lighting = crate::scene::load::read_file(std::path::Path::new(fields[0]))?;
			lighting.select(path)?;
			let child = lighting.dom.get_by_ref(lighting.roots[0]).ok_or_else(|| anyhow::anyhow!("broken DOM"))?;
			environment.apply_child(child);
		}
	}
	let vector = |text: &str| -> anyhow::Result<Vec3> {
		let values: Vec<f32> = text.split(',').map(|value| value.trim().parse::<f32>()).collect::<Result<_, _>>()?;
		anyhow::ensure!(values.len() == 3, "{text:?} is not x,y,z");
		Ok(Vec3::new(values[0], values[1], values[2]))
	};
	let (eye, target, fov) = (vector(fields[2])?, vector(fields[3])?, fields[4].trim().parse::<f32>()?);
	let names: Vec<String> = fields[1].replace("[\"", ".").replace("\"]", ".").split('.').filter(|name| !name.is_empty()).map(str::to_string).collect();
	let mut label: String = names[names.len().saturating_sub(2)..].join("-");
	if let Some(kept) = &kept {
		label = format!("{label}-{}", kept.join("-"));
	}
	if fields.len() == 7 {
		label.push_str("-lit");
	}
	let label: String = label.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
	let name = format!("particle-view-{label}");
	let mut document = crate::scene::load::read_file(std::path::Path::new(fields[0]))?;
	document.select(fields[1])?;
	let mut longest = 0.0f32;
	for referent in document.dom.descendants_of(document.dom.root_ref()).map(|instance| instance.referent()).collect::<Vec<_>>() {
		let Some(instance) = document.dom.get_by_ref_mut(referent) else { continue };
		if instance.class == "ParticleEmitter" {
			if kept.as_ref().is_some_and(|kept| !kept.contains(&instance.name.as_str())) {
				document.dom.destroy(referent);
				continue;
			}
			instance.properties.insert("Enabled".into(), true.into());
		}
	}
	let scene = crate::scene::extract::extract(&document);
	for emitter in &scene.particles {
		longest = longest.max(emitter.lifetime.0.max(emitter.lifetime.1) / emitter.time_scale.max(0.05));
	}
	let mut model = ModelBuilder::new("ParticleView");
	let roots = document.roots.clone();
	model.dom = document.dom;
	let container = model.dom.insert(model.dom.root_ref(), InstanceBuilder::new("Model").with_name("ParticleView"));
	for root in roots {
		model.dom.transfer_within(root, container);
	}
	model.model = container;
	let ahead = (target - eye).normalize();
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_rotation_translation(glam::Quat::from_rotation_arc(Vec3::Z, ahead), target + ahead * 3000.0), size: Vec3::new(2048.0, 2048.0, 1.0), color: [0, 0, 0], cast_shadow: false, ..Default::default() });
	let runs = vec![Run { label: "view".into(), environment, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov } }];
	let luau = format!(
		"local emitters = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"ParticleEmitter\") then
		e.Enabled = true
		e:Clear()
		table.insert(emitters, e)
	end
end
task.wait({})
return #emitters",
		longest.min(30.0) + 0.5
	);
	register(&name, "view", Setup { luau, bursts: Vec::new(), counts: Vec::new(), seeds: 64 });
	Ok(Experiment { name, model, probes: Vec::new(), runs })
}

/// How particles face the camera: Squash at several values, VelocityParallel moving diagonally on
/// screen (and with a Rotation), VelocityPerpendicular moving sideways and up seen obliquely,
/// FacingCameraWorldUp from above, and a FacingCamera particle far off the view's centre (turned to
/// the screen or to the camera?), all with the arrow texture, frozen at age 0.
pub fn particle_orient() -> Experiment {
	let name = "particle-orient";
	let mut model = ModelBuilder::new("ParticleOrient");
	let arrow = || -> (&'static str, Variant) { ("Texture", ContentId::from(ARROW).into()) };
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let front = Vec3::new(0.0, 0.0, -40.0);
	// (label, attachment rotation, properties, camera offset from the emitter, camera target offset)
	let diagonal = Affine3A::from_rotation_z(-std::f32::consts::FRAC_PI_4);
	let sideways = Affine3A::from_rotation_z(-std::f32::consts::FRAC_PI_2);
	let cases: Vec<(&str, Affine3A, Props, Vec3, Vec3)> = vec![
		("squash-neg", Affine3A::IDENTITY, vec![arrow(), ("Squash", constant(-0.5))], front, Vec3::ZERO),
		("squash-1", Affine3A::IDENTITY, vec![arrow(), ("Squash", constant(1.0))], front, Vec3::ZERO),
		("squash-2", Affine3A::IDENTITY, vec![arrow(), ("Squash", constant(2.0))], front, Vec3::ZERO),
		("rot90", Affine3A::IDENTITY, vec![arrow(), ("Rotation", fixed(90.0))], front, Vec3::ZERO),
		("parallel-diagonal", diagonal, vec![arrow(), ("Orientation", Enum::from_u32(2).into()), ("Speed", fixed(10.0))], front, Vec3::ZERO),
		("parallel-rot45", Affine3A::IDENTITY, vec![arrow(), ("Orientation", Enum::from_u32(2).into()), ("Speed", fixed(10.0)), ("Rotation", fixed(45.0))], front, Vec3::ZERO),
		("perpendicular-side", sideways, vec![arrow(), ("Orientation", Enum::from_u32(3).into()), ("Speed", fixed(10.0))], Vec3::new(-30.0, 10.0, -25.0), Vec3::ZERO),
		("perpendicular-up", Affine3A::IDENTITY, vec![arrow(), ("Orientation", Enum::from_u32(3).into()), ("Speed", fixed(10.0))], Vec3::new(10.0, 35.0, -20.0), Vec3::ZERO),
		("perpendicular-rot45", Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2), vec![arrow(), ("Orientation", Enum::from_u32(3).into()), ("Speed", fixed(10.0)), ("Rotation", fixed(45.0))], front, Vec3::ZERO),
		("worldup-above", Affine3A::IDENTITY, vec![arrow(), ("Orientation", Enum::from_u32(1).into())], Vec3::new(0.0, 30.0, -25.0), Vec3::ZERO),
		("facing-above", Affine3A::IDENTITY, vec![arrow()], Vec3::new(0.0, 30.0, -25.0), Vec3::ZERO),
		("facing-offcentre", Affine3A::IDENTITY, vec![arrow()], Vec3::new(0.0, 0.0, -15.0), Vec3::new(-9.0, 0.0, 0.0)),
	];
	let centers: Vec<Vec3> = (0..cases.len()).map(|index| Vec3::new(index as f32 * 30.0, 0.0, 0.0)).collect();
	let mut runs = Vec::new();
	for ((label, rotation, set, eye, target), center) in cases.into_iter().zip(&centers) {
		add_emitter(&mut model, label, Affine3A::from_translation(*center) * rotation, &set);
		runs.push(Run { label: label.into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (*center + eye).into(), target: (*center + target).into(), fov: if label == "facing-offcentre" { 70.0 } else { 20.0 } } });
		register(name, label, freeze(0.0, &[]));
	}
	backdrop(&mut model, &centers);
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}
