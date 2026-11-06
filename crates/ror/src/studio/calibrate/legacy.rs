//! Legacy effects (notes/render.md "Legacy effects"): Fire, Smoke, Sparkles, Explosion and the
//! ForceField instance, Studio's built-in particle effects. Their particles are random: runs freeze
//! them (TimeScale 0) or compare Studio's mean over a burst of frames against ror's mean over
//! seeded snapshots.

use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, Ref, Variant};

use super::particles::{Setup, register};
use super::timeline::{self, Timeline};
use super::{Experiment, Run, dark};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::playback::{Action, Motion, MotionKey, Playback};

/// Experiments of live effects: a burst of Studio frames (count, ms apart) for their mean.
pub fn burst(name: &str) -> Option<(u32, u64)> {
	match name {
		"legacy-survey" => Some((16, 150)),
		"legacy-forcefield-look" => Some((8, 250)),
		"legacy-forcefield-where" => Some((6, 250)),
		"legacy-fire-mean" | "legacy-fire-life" | "legacy-fire-hide" | "legacy-sparkles-mean" | "legacy-sparkles-panel" | "legacy-forcefield-mean" | "legacy-forcefield-panel" => Some((24, 150)),
		"legacy-smoke-mean" => Some((24, 300)),
		_ => None,
	}
}

/// An invisible anchor part of `size` at `at` (the effect's parent).
fn anchor(model: &mut ModelBuilder, name: &str, at: Affine3A, size: Vec3) -> Ref {
	model.part(&PartSpec { name: name.into(), cframe: at, size, transparency: 1.0, cast_shadow: false, ..Default::default() })
}

/// An effect instance of `class` with `set` on top of its defaults.
fn effect(class: &str, set: &[(&str, Variant)]) -> InstanceBuilder {
	let mut builder = InstanceBuilder::new(class).with_name(class);
	for (property, value) in set {
		builder = builder.with_property(*property, value.clone());
	}
	builder
}

fn color(c: [f32; 3]) -> Variant {
	Color3::new(c[0], c[1], c[2]).into()
}

/// A black wall far behind everything and a black floor far below.
fn wall(model: &mut ModelBuilder, center: Vec3) {
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 40.0)), size: Vec3::new(4000.0, 600.0, 1.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(center + Vec3::new(0.0, -30.0, 0.0)), size: Vec3::new(4000.0, 1.0, 600.0), color: [0, 0, 0], ..Default::default() });
}

/// Studio: the Fire, Smoke and Sparkles under the instance named `only` (all without one) enabled
/// and running (TimeScale 1) for `wait` seconds, the others disabled, then every one frozen
/// (TimeScale 0) unless `live`; with `only`, only its ForceFields Visible; an Explosion at
/// `explosion` (position, BlastRadius) made first.
fn running(only: Option<&str>, wait: f32, live: bool, explosion: Option<(Vec3, f32)>) -> Setup {
	let explode = match explosion {
		Some((at, radius)) => format!(
			"local explosion = Instance.new(\"Explosion\")
explosion.Position = Vector3.new({}, {}, {})
explosion.BlastRadius = {radius}
explosion.BlastPressure = 0
explosion.DestroyJointRadiusPercent = 0
explosion.Parent = workspace.RorCapture
table.insert(effects, explosion)
",
			at.x, at.y, at.z
		),
		None => String::new(),
	};
	let freeze = if live { String::new() } else { "for _, e in effects do\n\te.TimeScale = 0\nend\n".to_string() };
	let only = only.map(|name| format!("{name:?}")).unwrap_or_else(|| "nil".into());
	let luau = format!(
		"local only = {only}
local effects = {{}}
for _, e in workspace.RorCapture:GetDescendants() do
	if e:IsA(\"Fire\") or e:IsA(\"Smoke\") or e:IsA(\"Sparkles\") then
		local mine = only == nil
		local ancestor = e.Parent
		while not mine and ancestor and ancestor ~= workspace do
			mine = ancestor.Name == only
			ancestor = ancestor.Parent
		end
		e.Enabled = mine
		e.TimeScale = 1
		table.insert(effects, e)
	elseif e:IsA(\"ForceField\") and only ~= nil then
		local mine = false
		local ancestor = e.Parent
		while not mine and ancestor and ancestor ~= workspace do
			mine = ancestor.Name == only
			ancestor = ancestor.Parent
		end
		e.Visible = mine
	end
end
{explode}task.wait({wait})
{freeze}return #effects"
	);
	Setup { luau, bursts: Vec::new(), counts: Vec::new(), seeds: 0 }
}

/// A first look: each class at its defaults (Fire, Smoke, Sparkles on a tiny invisible part; a
/// ForceField on a gray block and on a model of two; an Explosion made by the setup), alone in
/// front of a black wall, live (a burst of frames), and Explosions frozen at a few ages.
pub fn legacy_survey() -> Experiment {
	let name = "legacy-survey";
	let mut model = ModelBuilder::new("LegacySurvey");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let center = |index: usize| Vec3::new(index as f32 * 60.0, 0.0, 0.0);
	let camera = |at: Vec3, distance: f32| CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 0.0, -distance)).into(), target: at.into(), fov: 40.0 };
	let mut runs = Vec::new();
	// (label, class, properties, environment)
	let cases: Vec<(&str, &str, Vec<(&str, Variant)>, Environment)> = vec![
		("fire", "Fire", vec![], dark()),
		("smoke", "Smoke", vec![], flat.clone()),
		("smoke-dark", "Smoke", vec![], dark()),
		("sparkles", "Sparkles", vec![], dark()),
		("fire-colors", "Fire", vec![("Color", color([0.0, 0.5, 1.0])), ("SecondaryColor", color([1.0, 0.0, 0.0]))], dark()),
	];
	let mut index = 0;
	for (label, class, set, environment) in cases {
		let at = center(index);
		let part = anchor(&mut model, label, Affine3A::from_translation(at), Vec3::splat(0.2));
		model.child(part, effect(class, &set));
		runs.push(Run { label: label.into(), environment, camera: camera(at + Vec3::new(0.0, 3.0, 0.0), 30.0) });
		register(name, label, running(None, 3.0, true, None));
		index += 1;
	}
	// a ForceField on a visible block, and on a model of two blocks
	let at = center(index);
	let block = model.part(&PartSpec { name: "forcefield-block".into(), cframe: Affine3A::from_translation(at), size: Vec3::splat(4.0), color: [128, 128, 128], cast_shadow: false, ..Default::default() });
	model.child(block, effect("ForceField", &[]));
	runs.push(Run { label: "forcefield-block".into(), environment: flat.clone(), camera: camera(at, 25.0) });
	register(name, "forcefield-block", running(None, 1.0, true, None));
	index += 1;
	let at = center(index);
	let group = model.child(model.model, InstanceBuilder::new("Model").with_name("forcefield-model"));
	for (offset, size) in [(Vec3::new(-2.0, 0.0, 0.0), Vec3::new(2.0, 4.0, 2.0)), (Vec3::new(2.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 2.0))] {
		let part = model.part(&PartSpec { name: "Block".into(), cframe: Affine3A::from_translation(at + offset), size, color: [128, 128, 128], cast_shadow: false, ..Default::default() });
		model.dom.transfer_within(part, group);
	}
	model.child(group, effect("ForceField", &[]));
	runs.push(Run { label: "forcefield-model".into(), environment: flat.clone(), camera: camera(at, 25.0) });
	register(name, "forcefield-model", running(None, 1.0, true, None));
	index += 1;
	// Explosions made by the setup, frozen at a few ages
	for age in [0.05f32, 0.2, 0.5, 1.0] {
		let at = center(index);
		let label = format!("explosion-{age}");
		runs.push(Run { label: label.clone(), environment: dark(), camera: camera(at, 40.0) });
		register(name, &label, running(None, age, false, Some((at, 4.0))));
		index += 1;
	}
	wall(&mut model, center(index / 2));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// One effect case of a look experiment: its label, the effect's class and properties, its
/// parent's CFrame (relative to the case's centre) and size, the environment and the camera's
/// offset from the centre (looking at the centre `target` up).
struct Case {
	label: &'static str,
	class: &'static str,
	set: Vec<(&'static str, Variant)>,
	parent: Affine3A,
	size: Vec3,
	environment: Environment,
	eye: Vec3,
	target: f32,
	fov: f32,
}

impl Case {
	fn new(label: &'static str, class: &'static str, set: Vec<(&'static str, Variant)>, environment: &Environment) -> Self {
		Self { label, class, set, parent: Affine3A::IDENTITY, size: Vec3::splat(0.2), environment: environment.clone(), eye: Vec3::new(0.0, 0.0, -30.0), target: 3.0, fov: 40.0 }
	}
}

/// Every case at its own centre 100 studs apart, frozen after `wait` seconds alone.
fn look(name: &str, root: &str, cases: Vec<Case>, wait: f32) -> Experiment {
	look_with(name, root, cases, wait, false)
}

/// As `look`, or live (`live`: Studio's burst of frames against ror's mean over 12 seeded snapshots).
fn look_with(name: &str, root: &str, cases: Vec<Case>, wait: f32, live: bool) -> Experiment {
	let mut model = ModelBuilder::new(root);
	let mut runs = Vec::new();
	let count = cases.len();
	for (index, case) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = anchor(&mut model, case.label, Affine3A::from_translation(center) * case.parent, case.size);
		model.child(part, effect(case.class, &case.set));
		let target = center + Vec3::new(0.0, case.target, 0.0);
		runs.push(Run { label: case.label.into(), environment: case.environment, camera: CameraSpec::LookAt { eye: (target + case.eye).into(), target: target.into(), fov: case.fov } });
		let mut setup = running(Some(case.label), wait, live, None);
		if live {
			setup.seeds = 12;
		}
		register(name, case.label, setup);
	}
	wall(&mut model, Vec3::new(count as f32 * 50.0, 0.0, 0.0));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Fire's anatomy, frozen at its steady state on black: the defaults from the front and above,
/// Size and Heat across their ranges, white and black colours (which colour goes where), turned
/// and big parents (does it rise along the world or the part, spawn over the part?), on an
/// attachment, and under light (is it lit?).
pub fn legacy_fire_look() -> Experiment {
	let lit = Environment { brightness: 2.0, ambient: Vec3::splat(0.5), ..dark() };
	let white = || color([1.0, 1.0, 1.0]);
	let black = || color([0.0, 0.0, 0.0]);
	let mut cases = vec![
		Case::new("default", "Fire", vec![], &dark()),
		Case { eye: Vec3::new(0.0, 30.0, -0.1), target: 0.0, ..Case::new("default-top", "Fire", vec![], &dark()) },
		Case { eye: Vec3::new(0.0, 0.0, -20.0), target: 2.0, ..Case::new("size2", "Fire", vec![("Size", 2.0f32.into())], &dark()) },
		Case { eye: Vec3::new(0.0, 0.0, -60.0), target: 6.0, ..Case::new("size10", "Fire", vec![("Size", 10.0f32.into())], &dark()) },
		Case { eye: Vec3::new(0.0, 0.0, -150.0), target: 15.0, ..Case::new("size30", "Fire", vec![("Size", 30.0f32.into())], &dark()) },
		Case::new("heat0", "Fire", vec![("Heat", 0.0f32.into())], &dark()),
		Case { eye: Vec3::new(0.0, 0.0, -45.0), target: 6.0, ..Case::new("heat25", "Fire", vec![("Heat", 25.0f32.into())], &dark()) },
		Case { target: -2.0, ..Case::new("heat-10", "Fire", vec![("Heat", (-10.0f32).into())], &dark()) },
		Case::new("white", "Fire", vec![("Color", white()), ("SecondaryColor", white())], &dark()),
		Case::new("secondary-black", "Fire", vec![("Color", white()), ("SecondaryColor", black())], &dark()),
		Case::new("primary-black", "Fire", vec![("Color", black()), ("SecondaryColor", white())], &dark()),
		Case { parent: Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2), ..Case::new("turned", "Fire", vec![], &dark()) },
		Case { parent: Affine3A::from_rotation_z(std::f32::consts::PI), ..Case::new("upside-down", "Fire", vec![], &dark()) },
		Case { size: Vec3::new(12.0, 1.0, 12.0), eye: Vec3::new(0.0, 15.0, -30.0), ..Case::new("big-part", "Fire", vec![], &dark()) },
		Case::new("lit", "Fire", vec![], &lit),
	];
	// on an attachment 2 studs above its part (built below)
	cases.push(Case::new("attachment", "Fire", vec![], &dark()));
	let mut experiment = look("legacy-fire-look", "LegacyFireLook", cases, 3.0);
	attach(&mut experiment.model, "attachment", Vec3::new(0.0, 2.0, 0.0));
	experiment
}

/// Moves the effect of the case `label` onto an Attachment at `offset` in its part.
fn attach(model: &mut ModelBuilder, label: &str, offset: Vec3) {
	let Some(part) = model.dom.descendants().find(|instance| instance.name == label && instance.class == "Part").map(|instance| instance.referent()) else {
		return;
	};
	let effects: Vec<Ref> = model.dom.get_by_ref(part).map(|part| part.children().to_vec()).unwrap_or_default();
	let attachment = model.child(part, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(offset))));
	for effect in effects {
		model.dom.transfer_within(effect, attachment);
	}
}

/// Sparkles' anatomy, frozen on black: the default from the front and above, white, black and red,
/// a turned and a big parent, on an attachment, under light.
pub fn legacy_sparkles_look() -> Experiment {
	let lit = Environment { brightness: 2.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut cases = vec![
		Case::new("default", "Sparkles", vec![], &dark()),
		Case { eye: Vec3::new(0.0, 30.0, -0.1), target: 0.0, ..Case::new("default-top", "Sparkles", vec![], &dark()) },
		Case::new("white", "Sparkles", vec![("SparkleColor", color([1.0, 1.0, 1.0]))], &dark()),
		Case::new("black", "Sparkles", vec![("SparkleColor", color([0.0, 0.0, 0.0]))], &dark()),
		Case::new("red", "Sparkles", vec![("SparkleColor", color([1.0, 0.0, 0.0]))], &dark()),
		Case { parent: Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2), ..Case::new("turned", "Sparkles", vec![], &dark()) },
		Case { size: Vec3::new(12.0, 1.0, 12.0), eye: Vec3::new(0.0, 15.0, -30.0), ..Case::new("big-part", "Sparkles", vec![], &dark()) },
		Case::new("lit", "Sparkles", vec![], &lit),
	];
	cases.push(Case::new("attachment", "Sparkles", vec![], &dark()));
	let mut experiment = look("legacy-sparkles-look", "LegacySparklesLook", cases, 3.0);
	attach(&mut experiment.model, "attachment", Vec3::new(0.0, 2.0, 0.0));
	experiment
}

/// Smoke's anatomy, frozen, lit by flat ambient over black: the default from the front and above,
/// Size, RiseVelocity and Opacity across their ranges, a red one, a turned and a big parent, in the
/// dark and in the sun.
pub fn legacy_smoke_look() -> Experiment {
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let sun = Environment { brightness: 2.0, ..dark() };
	let cases = vec![
		Case::new("default", "Smoke", vec![], &flat),
		Case { eye: Vec3::new(0.0, 30.0, -0.1), target: 0.0, ..Case::new("default-top", "Smoke", vec![], &flat) },
		Case { eye: Vec3::new(0.0, 0.0, -60.0), target: 6.0, ..Case::new("size5", "Smoke", vec![("Size", 5.0f32.into())], &flat) },
		Case { eye: Vec3::new(0.0, 0.0, -150.0), target: 15.0, ..Case::new("size20", "Smoke", vec![("Size", 20.0f32.into())], &flat) },
		Case::new("rise0", "Smoke", vec![("RiseVelocity", 0.0f32.into())], &flat),
		Case { eye: Vec3::new(0.0, 0.0, -60.0), target: 12.0, ..Case::new("rise10", "Smoke", vec![("RiseVelocity", 10.0f32.into())], &flat) },
		Case { target: -4.0, ..Case::new("rise-5", "Smoke", vec![("RiseVelocity", (-5.0f32).into())], &flat) },
		Case::new("opacity1", "Smoke", vec![("Opacity", 1.0f32.into())], &flat),
		Case::new("opacity0.1", "Smoke", vec![("Opacity", 0.1f32.into())], &flat),
		Case::new("red", "Smoke", vec![("Color", color([1.0, 0.0, 0.0]))], &flat),
		Case { parent: Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2), ..Case::new("turned", "Smoke", vec![], &flat) },
		Case { size: Vec3::new(12.0, 1.0, 12.0), eye: Vec3::new(0.0, 15.0, -30.0), ..Case::new("big-part", "Smoke", vec![], &flat) },
		Case::new("sun", "Smoke", vec![], &sun),
	];
	look("legacy-smoke-look", "LegacySmokeLook", cases, 5.0)
}

/// A Model of gray blocks (offset from `center`, size) named `name`, with a ForceField and the
/// PrimaryPart or WorldPivot given; returns the model.
fn forcefield_model(model: &mut ModelBuilder, name: &str, center: Vec3, blocks: &[(Vec3, Vec3)], primary: Option<usize>, pivot: Option<Vec3>, set: &[(&str, Variant)]) -> Ref {
	let group = model.child(model.model, InstanceBuilder::new("Model").with_name(name));
	let mut parts = Vec::new();
	for (offset, size) in blocks {
		let part = model.part(&PartSpec { name: "Block".into(), cframe: Affine3A::from_translation(center + *offset), size: *size, color: [128, 128, 128], cast_shadow: false, ..Default::default() });
		model.dom.transfer_within(part, group);
		parts.push(part);
	}
	if let Some(index) = primary {
		model.set(group, "PrimaryPart", Variant::Ref(parts[index]));
	}
	if let Some(pivot) = pivot {
		model.set(group, "WorldPivotData", Variant::OptionalCFrame(Some(crate::scene::build::cframe(Affine3A::from_translation(pivot)))));
	}
	model.child(group, effect("ForceField", set));
	group
}

/// The ForceField instance, live (a burst of frames): around Studio's R15 and R6 rigs, around models
/// with a PrimaryPart, a WorldPivot, wide and tiny, under a part, in the dark, and not Visible.
pub fn legacy_forcefield_look() -> anyhow::Result<Experiment> {
	let name = "legacy-forcefield-look";
	let mut model = ModelBuilder::new("LegacyForceFieldLook");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let center = |index: usize| Vec3::new(index as f32 * 100.0, 0.0, 0.0);
	let camera = |at: Vec3, distance: f32| CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 0.0, -distance)).into(), target: at.into(), fov: 40.0 };
	let mut runs = Vec::new();
	let add = |runs: &mut Vec<Run>, label: &str, at: Vec3, distance: f32, environment: &Environment| {
		runs.push(Run { label: label.into(), environment: environment.clone(), camera: camera(at, distance) });
		register(name, label, running(None, 1.0, true, None));
	};
	for (index, r15) in [(0usize, true), (1, false)] {
		let rig = super::characters::studio_rig(r15)?;
		let label = if r15 { "r15" } else { "r6" };
		let copy = super::characters::add_rig(&mut model, &rig, label, center(index), &super::characters::Outfit::default())?;
		model.child(copy, effect("ForceField", &[]));
		add(&mut runs, label, center(index), 20.0, &flat);
	}
	let two = [(Vec3::new(-2.0, 0.0, 0.0), Vec3::new(2.0, 4.0, 2.0)), (Vec3::new(2.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 2.0))];
	forcefield_model(&mut model, "primary", center(2), &two, Some(1), None, &[]);
	add(&mut runs, "primary", center(2), 25.0, &flat);
	forcefield_model(&mut model, "pivot", center(3), &two, None, Some(center(3) + Vec3::new(3.0, 3.0, 0.0)), &[]);
	add(&mut runs, "pivot", center(3), 25.0, &flat);
	forcefield_model(&mut model, "wide", center(4), &[(Vec3::ZERO, Vec3::new(20.0, 2.0, 2.0))], Some(0), None, &[]);
	add(&mut runs, "wide", center(4), 40.0, &flat);
	forcefield_model(&mut model, "tall", center(5), &[(Vec3::ZERO, Vec3::new(2.0, 12.0, 2.0))], Some(0), None, &[]);
	add(&mut runs, "tall", center(5), 40.0, &flat);
	forcefield_model(&mut model, "tiny", center(6), &[(Vec3::ZERO, Vec3::splat(0.5))], Some(0), None, &[]);
	add(&mut runs, "tiny", center(6), 15.0, &flat);
	forcefield_model(&mut model, "huge", center(7), &[(Vec3::ZERO, Vec3::splat(20.0))], Some(0), None, &[]);
	add(&mut runs, "huge", center(7), 60.0, &flat);
	let part = model.part(&PartSpec { name: "part".into(), cframe: Affine3A::from_translation(center(8)), size: Vec3::splat(4.0), color: [128, 128, 128], cast_shadow: false, ..Default::default() });
	model.child(part, effect("ForceField", &[]));
	add(&mut runs, "part", center(8), 25.0, &flat);
	let rig = super::characters::studio_rig(true)?;
	let copy = super::characters::add_rig(&mut model, &rig, "r15-dark", center(9), &super::characters::Outfit::default())?;
	model.child(copy, effect("ForceField", &[]));
	add(&mut runs, "r15-dark", center(9), 20.0, &dark());
	forcefield_model(&mut model, "invisible", center(10), &two, Some(1), None, &[("Visible", false.into())]);
	add(&mut runs, "invisible", center(10), 25.0, &flat);
	wall(&mut model, center(5));
	Ok(Experiment { name: name.into(), model, probes: Vec::new(), runs })
}

/// A Model with a Humanoid and the named blocks (name, offset from `center`, size), a ForceField
/// and its first block as PrimaryPart.
fn humanoid_model(model: &mut ModelBuilder, name: &str, center: Vec3, blocks: &[(&str, Vec3, Vec3)]) -> Ref {
	let group = model.child(model.model, InstanceBuilder::new("Model").with_name(name));
	let mut first = None;
	for (part_name, offset, size) in blocks {
		let part = model.part(&PartSpec { name: (*part_name).into(), cframe: Affine3A::from_translation(center + *offset), size: *size, color: [128, 128, 128], cast_shadow: false, ..Default::default() });
		model.dom.transfer_within(part, group);
		first.get_or_insert(part);
	}
	if let Some(first) = first {
		model.set(group, "PrimaryPart", Variant::Ref(first));
	}
	model.child(group, InstanceBuilder::new("Humanoid").with_name("Humanoid"));
	model.child(group, effect("ForceField", &[]));
	group
}

/// Where the ForceField draws, one at a time (the others not Visible), live: a model without a
/// Humanoid and a part (seen where they are and at the world's origin), Humanoid models with only a
/// Torso, only a HumanoidRootPart, a big root part, a big other part, and Studio's rigs.
pub fn legacy_forcefield_where() -> anyhow::Result<Experiment> {
	let name = "legacy-forcefield-where";
	let mut model = ModelBuilder::new("LegacyForceFieldWhere");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let center = |index: usize| Vec3::new(index as f32 * 100.0 + 200.0, 0.0, 0.0);
	let camera = |at: Vec3, distance: f32| CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 0.0, -distance)).into(), target: at.into(), fov: 40.0 };
	let mut runs = Vec::new();
	let add = |runs: &mut Vec<Run>, label: &str, only: &str, at: Vec3, distance: f32| {
		runs.push(Run { label: label.into(), environment: flat.clone(), camera: camera(at, distance) });
		register(name, label, running(Some(only), 3.0, true, None));
	};
	add(&mut runs, "origin-none", "nothing", Vec3::ZERO, 25.0);
	let two = [(Vec3::new(-2.0, 0.0, 0.0), Vec3::new(2.0, 4.0, 2.0)), (Vec3::new(2.0, 1.0, 0.0), Vec3::new(2.0, 2.0, 2.0))];
	forcefield_model(&mut model, "model", center(0), &two, Some(1), None, &[]);
	add(&mut runs, "origin-model", "model", Vec3::ZERO, 25.0);
	add(&mut runs, "at-model", "model", center(0), 25.0);
	let part = model.part(&PartSpec { name: "part".into(), cframe: Affine3A::from_translation(center(1)), size: Vec3::splat(4.0), color: [128, 128, 128], cast_shadow: false, ..Default::default() });
	model.child(part, effect("ForceField", &[]));
	add(&mut runs, "origin-part", "part", Vec3::ZERO, 25.0);
	add(&mut runs, "at-part", "part", center(1), 25.0);
	humanoid_model(&mut model, "torso-only", center(2), &[("Torso", Vec3::ZERO, Vec3::new(2.0, 2.0, 1.0)), ("Head", Vec3::new(0.0, 1.5, 0.0), Vec3::new(2.0, 1.0, 1.0))]);
	add(&mut runs, "torso-only", "torso-only", center(2), 25.0);
	add(&mut runs, "origin-torso-only", "torso-only", Vec3::ZERO, 25.0);
	humanoid_model(&mut model, "root-only", center(3), &[("HumanoidRootPart", Vec3::ZERO, Vec3::new(2.0, 2.0, 1.0))]);
	add(&mut runs, "root-only", "root-only", center(3), 25.0);
	humanoid_model(&mut model, "big-root", center(4), &[("HumanoidRootPart", Vec3::ZERO, Vec3::new(6.0, 6.0, 3.0))]);
	add(&mut runs, "big-root", "big-root", center(4), 35.0);
	humanoid_model(&mut model, "big-other", center(5), &[("HumanoidRootPart", Vec3::ZERO, Vec3::new(2.0, 2.0, 1.0)), ("Torso", Vec3::new(5.0, 3.0, 0.0), Vec3::new(4.0, 12.0, 2.0))]);
	add(&mut runs, "big-other", "big-other", center(5), 35.0);
	for (index, r15) in [(6usize, false), (7, true)] {
		let rig = super::characters::studio_rig(r15)?;
		let label = if r15 { "r15" } else { "r6" };
		let copy = super::characters::add_rig(&mut model, &rig, label, center(index), &super::characters::Outfit::default())?;
		model.child(copy, effect("ForceField", &[]));
		add(&mut runs, label, label, center(index), 20.0);
	}
	wall(&mut model, center(4));
	Ok(Experiment { name: name.into(), model, probes: Vec::new(), runs })
}

/// The ForceField instance's look, live (Studio's mean over a burst of frames against ror's mean over
/// seeded snapshots), in the dark: around a Humanoid model's invisible Torso (only the shield shows)
/// from the front at exposure 0 and -3, from above and from the side at -3, and around Studio's R6
/// and R15 rigs at 0.
pub fn legacy_forcefield_mean() -> anyhow::Result<Experiment> {
	let name = "legacy-forcefield-mean";
	let mut model = ModelBuilder::new("LegacyForceFieldMean");
	let center = |index: usize| Vec3::new(index as f32 * 100.0 + 200.0, 0.0, 0.0);
	let dim3 = Environment { exposure_compensation: -3.0, ..dark() };
	let mut runs = Vec::new();
	let add = |runs: &mut Vec<Run>, label: &str, only: &str, eye: Vec3, at: Vec3, environment: &Environment| {
		runs.push(Run { label: label.into(), environment: environment.clone(), camera: CameraSpec::LookAt { eye: (at + eye).into(), target: at.into(), fov: 40.0 } });
		let mut setup = running(Some(only), 2.0, true, None);
		setup.seeds = 12;
		register(name, label, setup);
	};
	let shield = humanoid_model(&mut model, "shield", center(0), &[("Torso", Vec3::ZERO, Vec3::new(2.0, 2.0, 1.0))]);
	if let Some(torso) = model.dom.get_by_ref(shield).and_then(|group| group.children().iter().copied().find(|child| model.dom.get_by_ref(*child).is_some_and(|part| part.name == "Torso"))) {
		model.set(torso, "Transparency", 1.0f32);
	}
	add(&mut runs, "front", "shield", Vec3::new(0.0, 0.0, -20.0), center(0), &dark());
	add(&mut runs, "front-ev-3", "shield", Vec3::new(0.0, 0.0, -20.0), center(0), &dim3);
	add(&mut runs, "top-ev-3", "shield", Vec3::new(0.0, 20.0, -0.1), center(0), &dim3);
	add(&mut runs, "side-ev-3", "shield", Vec3::new(-20.0, 0.0, 0.0), center(0), &dim3);
	for (index, r15) in [(1usize, false), (2, true)] {
		let rig = super::characters::studio_rig(r15)?;
		let label = if r15 { "r15" } else { "r6" };
		let copy = super::characters::add_rig(&mut model, &rig, label, center(index), &super::characters::Outfit::default())?;
		model.child(copy, effect("ForceField", &[]));
		add(&mut runs, label, label, Vec3::new(0.0, 0.0, -20.0), center(index), &dark());
	}
	wall(&mut model, center(1));
	Ok(Experiment { name: name.into(), model, probes: Vec::new(), runs })
}

/// Explosions frozen at moments after they go off (timeline runs: an Explosion in the model goes off
/// at the first frame, as it enters the world), in the dark: a row of three 32 studs apart with
/// BlastRadius `radius` (no pressure, no joints broken), seen from 100 studs (or `distance`), a run per
/// (moment, exposure, frozen there or not).
fn explosion_runs(name: &str, root: &str, radius: f32, distance: f32, runs: &[(f32, f32, bool)]) -> Experiment {
	let mut model = ModelBuilder::new(root);
	for index in 0..3 {
		let at = Vec3::new((index as f32 - 1.0) * 32.0 * distance / 100.0, 0.0, 0.0);
		let explosion = InstanceBuilder::new("Explosion")
			.with_name(format!("Explosion{index}"))
			.with_property("Position", Variant::Vector3(rbx_dom_weak::types::Vector3::new(at.x, at.y, at.z)))
			.with_property("BlastRadius", radius)
			.with_property("BlastPressure", 0.0f32)
			.with_property("DestroyJointRadiusPercent", 0.0f32);
		model.child(model.model, explosion);
	}
	wall(&mut model, Vec3::ZERO);
	let mut experiment_runs = Vec::new();
	for &(moment, exposure, frozen) in runs {
		let label = if frozen { format!("t{moment}-ev{exposure}") } else { format!("live{moment}-ev{exposure}") };
		experiment_runs.push(Run { label: label.clone(), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -distance], target: [0.0, 0.0, 0.0], fov: 40.0 } });
		let playback = if frozen { Playback { time: moment + 0.1, actions: vec![freeze(moment, root)], ..Default::default() } } else { Playback { time: moment, ..Default::default() } };
		timeline::register(name, &label, Timeline { playback, duration: moment + 0.35 });
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs: experiment_runs }
}

/// An Explosion's life (BlastRadius 4), frozen (TimeScale 0) at 0.03 to 3 s at exposure -3 and a few
/// moments at 0. Frozen, Studio draws more than the explosion was: from 1.3 s on the same fresh
/// fireball (frozen at whatever moment), at 0.6 s 18 times the light legacy-explosion-end shows live.
pub fn legacy_explosion_ages() -> Experiment {
	let mut runs: Vec<(f32, f32, bool)> = [0.03f32, 0.08, 0.15, 0.25, 0.4, 0.6, 0.8, 1.0, 1.3, 1.7, 2.2, 3.0].iter().map(|&moment| (moment, -3.0, true)).collect();
	runs.extend([0.1f32, 0.3, 0.6, 1.0].iter().map(|&moment| (moment, 0.0, true)));
	explosion_runs("legacy-explosion-ages", "LegacyExplosionAges", 4.0, 100.0, &runs)
}

/// An Explosion's life (BlastRadius 4) as it plays, not frozen: the frame at 0.03 to 0.6 s at
/// exposure -3, and a few at 0.
pub fn legacy_explosion_live() -> Experiment {
	let mut runs: Vec<(f32, f32, bool)> = [0.03f32, 0.06, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.6].iter().map(|&moment| (moment, -3.0, false)).collect();
	runs.extend([0.05f32, 0.15, 0.3].iter().map(|&moment| (moment, 0.0, false)));
	explosion_runs("legacy-explosion-live", "LegacyExplosionLive", 4.0, 100.0, &runs)
}

/// How BlastRadius scales an Explosion (`legacy-explosion-radius1`, `-radius16`), not frozen: the frame
/// at 0.05, 0.15 and 0.3 s at exposure -3, seen from as far as its size needs.
pub fn legacy_explosion_radius(suffix: &str) -> Option<Experiment> {
	let (radius, distance) = match suffix {
		"radius1" => (1.0, 50.0),
		"radius16" => (16.0, 250.0),
		_ => return None,
	};
	let runs: Vec<(f32, f32, bool)> = [0.05f32, 0.15, 0.3].iter().map(|&moment| (moment, -3.0, false)).collect();
	Some(explosion_runs(&format!("legacy-explosion-{suffix}"), "LegacyExplosionRadius", radius, distance, &runs))
}

/// An Explosion (BlastRadius 4) not frozen, at exposure -3: its frame at 0.6, 1.1, 1.4, 1.8 and
/// 2.5 s (when is it gone? frozen after its end, Studio shows it going off again).
pub fn legacy_explosion_end() -> Experiment {
	let mut experiment = explosion_runs("legacy-explosion-end", "LegacyExplosionEnd", 4.0, 100.0, &[]);
	for moment in [0.6f32, 1.1, 1.4, 1.8, 2.5] {
		let label = format!("live{moment}");
		experiment.runs.push(Run { label: label.clone(), environment: Environment { exposure_compensation: -3.0, ..dark() }, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -100.0], target: [0.0, 0.0, 0.0], fov: 40.0 } });
		timeline::register("legacy-explosion-end", &label, Timeline { playback: Playback { time: moment, ..Default::default() }, duration: moment + 0.25 });
	}
	experiment
}

/// A gray Neon panel (x = 0.5 at exposure 0, about a daytime sky) of `size` at `at`.
fn bright_panel(model: &mut ModelBuilder, at: Vec3, size: Vec3) {
	model.part(&PartSpec { name: "Panel".into(), cframe: Affine3A::from_translation(at), size, color: [96, 96, 96], material: 288, cast_shadow: false, ..Default::default() });
}

/// The ForceField over a bright background, live: the shield around an invisible Torso in front of
/// a gray Neon panel 12 studs behind, at exposure 0 (does its light add in linear light or in the
/// √ space, as legacy-sparkles-panel's stars do?).
pub fn legacy_forcefield_panel() -> Experiment {
	let name = "legacy-forcefield-panel";
	let mut model = ModelBuilder::new("LegacyForceFieldPanel");
	let at = Vec3::new(200.0, 0.0, 0.0);
	let shield = humanoid_model(&mut model, "shield", at, &[("Torso", Vec3::ZERO, Vec3::new(2.0, 2.0, 1.0))]);
	if let Some(torso) = model.dom.get_by_ref(shield).and_then(|group| group.children().iter().copied().find(|child| model.dom.get_by_ref(*child).is_some_and(|part| part.name == "Torso"))) {
		model.set(torso, "Transparency", 1.0f32);
	}
	bright_panel(&mut model, at + Vec3::new(0.0, 0.0, 12.0), Vec3::new(40.0, 30.0, 0.2));
	wall(&mut model, at);
	let runs = vec![Run { label: "front".into(), environment: dark(), camera: CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 0.0, -20.0)).into(), target: at.into(), fov: 40.0 } }];
	let mut setup = running(Some("shield"), 2.0, true, None);
	setup.seeds = 12;
	register(name, "front", setup);
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Explosions over a bright background (legacy-explosion-live's row in front of a gray Neon panel 30
/// studs behind), their frames at 0.05, 0.15 and 0.3 s at exposure -3.
pub fn legacy_explosion_panel() -> Experiment {
	let runs: Vec<(f32, f32, bool)> = [0.05f32, 0.15, 0.3].iter().map(|&moment| (moment, -3.0, false)).collect();
	let mut experiment = explosion_runs("legacy-explosion-panel", "LegacyExplosionPanel", 4.0, 100.0, &runs);
	bright_panel(&mut experiment.model, Vec3::new(0.0, 0.0, 30.0), Vec3::new(160.0, 90.0, 0.2));
	experiment
}

fn switch(at: f32, target: &str, enabled: bool) -> Action {
	Action { at, target: target.into(), enabled: Some(enabled), ..Default::default() }
}

fn freeze(at: f32, target: &str) -> Action {
	Action { at, target: target.into(), time_scale: Some(0.0), ..Default::default() }
}

/// Cohort runs: a row of `count` effects of `class` with `set` 12 studs apart (`spacing`), a fifth
/// of `distance` under the view's centre, each run emitting from 0 to its window (seconds) and
/// frozen at its moment (a timeline run each), seen from the front `distance` studs away under
/// `environment`: their particles' rise, spread, size and light with age, and how many a window
/// emits.
struct Cohorts {
	class: &'static str,
	set: Vec<(&'static str, Variant)>,
	count: usize,
	spacing: f32,
	distance: f32,
	environment: Environment,
	/// (label, the windows it emits in, moment frozen)
	runs: Vec<(String, Vec<(f32, f32)>, f32)>,
}

impl Cohorts {
	fn new(class: &'static str, set: Vec<(&'static str, Variant)>, environment: Environment) -> Self {
		Self { class, set, count: 6, spacing: 12.0, distance: 60.0, environment, runs: Vec::new() }
	}

	/// A run per age, all emitting for `window`.
	fn ages(mut self, window: f32, ages: &[f32]) -> Self {
		self.runs.extend(ages.iter().map(|&age| (format!("age{age}"), vec![(0.0, window)], age)));
		self
	}

	/// A run per window, each frozen `after` seconds after its window closes.
	fn windows(mut self, windows: &[f32], after: f32) -> Self {
		self.runs.extend(windows.iter().map(|&window| (format!("window{window}"), vec![(0.0, window)], window + after)));
		self
	}

	/// A run emitting in the windows (from, to) given, frozen at `moment`.
	fn run(mut self, label: &str, windows: &[(f32, f32)], moment: f32) -> Self {
		self.runs.push((label.into(), windows.to_vec(), moment));
		self
	}

	fn experiment(self, name: &str, root: &str) -> Experiment {
		let mut model = ModelBuilder::new(root);
		for index in 0..self.count {
			let at = Vec3::new((index as f32 - (self.count as f32 - 1.0) * 0.5) * self.spacing, -0.2 * self.distance, 0.0);
			let part = anchor(&mut model, &format!("{}{index}", self.class), Affine3A::from_translation(at), Vec3::splat(0.2));
			let mut properties = self.set.clone();
			properties.push(("Enabled", false.into()));
			model.child(part, effect(self.class, &properties));
		}
		wall(&mut model, Vec3::ZERO);
		let mut runs = Vec::new();
		for (label, windows, moment) in self.runs {
			runs.push(Run { label: label.clone(), environment: self.environment.clone(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -self.distance], target: [0.0, 0.0, 0.0], fov: 40.0 } });
			let mut actions: Vec<Action> = windows.iter().flat_map(|&(from, to)| [switch(from, root, true), switch(to, root, false)]).collect();
			actions.push(freeze(moment, root));
			let playback = Playback { time: moment + 0.1, actions, ..Default::default() };
			timeline::register(name, &label, Timeline { playback, duration: moment + 0.35 });
		}
		Experiment { name: name.into(), model, probes: Vec::new(), runs }
	}
}

/// Fire cohorts: red Color, green SecondaryColor (which particles take which), Size 5, Heat 9,
/// emitting for 0.05 s.
pub fn legacy_fire_cohort() -> Experiment {
	let set = vec![("Color", color([1.0, 0.0, 0.0])), ("SecondaryColor", color([0.0, 1.0, 0.0]))];
	Cohorts::new("Fire", set, dark()).ages(0.05, &[0.05, 0.15, 0.3, 0.5, 0.75, 1.0, 1.5, 2.0]).experiment("legacy-fire-cohort", "LegacyFireCohort")
}

/// Exposure -5 (below the tone curve's shoulder and the bloom threshold even where particles
/// overlap): light reads linearly.
fn dim() -> Environment {
	Environment { exposure_compensation: -5.0, ..dark() }
}

/// Fire's flame alone (white Color, black SecondaryColor) at exposure -5: cohorts emitting for
/// 0.05 s frozen at 0.05 to 1.6 s, and windows of 1 to 12 frames frozen 0.35 s after them (a step
/// of light per particle: the rate).
pub fn legacy_fire_flame() -> Experiment {
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let frames: Vec<f32> = (1..=12).map(|k| k as f32 / 60.0 - 0.004).collect();
	Cohorts::new("Fire", set, dim())
		.ages(0.05, &[0.05, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.6, 0.8, 1.0, 1.2, 1.4, 1.6])
		.windows(&frames, 0.35)
		.experiment("legacy-fire-flame", "LegacyFireFlame")
}

/// Fire's secondary particles alone (black Color, white SecondaryColor) at exposure -5: cohorts
/// emitting for 0.5 s frozen at 0.5 to 2.2 s, and windows of 0.05 to 0.5 s frozen 0.2 s after them.
pub fn legacy_fire_secondary() -> Experiment {
	let set = vec![("Color", color([0.0, 0.0, 0.0])), ("SecondaryColor", color([1.0, 1.0, 1.0]))];
	Cohorts::new("Fire", set, dim())
		.ages(0.5, &[0.5, 0.6, 0.7, 0.8, 1.0, 1.2, 1.5, 1.8, 2.2])
		.windows(&[0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45, 0.5], 0.2)
		.experiment("legacy-fire-secondary", "LegacyFireSecondary")
}

/// How Heat and Size scale Fire's flame (white, exposure -5): cohorts of 0.05 s at Heat 25, 0 and
/// -10 and at Size 2 and 10 (`legacy-fire-heat25` ...), frozen at 0.3, 0.6 and 1 s.
pub fn legacy_fire_scale(suffix: &str) -> Option<Experiment> {
	let (extra, distance, spacing) = match suffix {
		"heat25" => (("Heat", Variant::from(25.0f32)), 60.0, 12.0),
		"heat0" => (("Heat", Variant::from(0.0f32)), 60.0, 12.0),
		"heat-10" => (("Heat", Variant::from(-10.0f32)), 60.0, 12.0),
		"size2" => (("Size", Variant::from(2.0f32)), 30.0, 6.0),
		"size10" => (("Size", Variant::from(10.0f32)), 120.0, 24.0),
		_ => return None,
	};
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0])), extra];
	let mut cohorts = Cohorts::new("Fire", set, dim()).ages(0.05, &[0.3, 0.6, 1.0]);
	cohorts.distance = distance;
	cohorts.spacing = spacing;
	Some(cohorts.experiment(&format!("legacy-fire-{suffix}"), "LegacyFireScale"))
}

/// Fire's parts by colour, frozen at its steady state: red Color, green SecondaryColor (which
/// particles take which), at Heat 9, 25 and 0 and Size 2, 5 and 10, and cohorts emitting for
/// 0.25 s (more of the rarer particles).
pub fn legacy_fire_parts() -> Experiment {
	let colours = || vec![("Color", color([1.0, 0.0, 0.0])), ("SecondaryColor", color([0.0, 1.0, 0.0]))];
	let with = |extra: Vec<(&'static str, Variant)>| {
		let mut set = colours();
		set.extend(extra);
		set
	};
	let cases = vec![
		Case::new("heat9", "Fire", colours(), &dark()),
		Case { eye: Vec3::new(0.0, 0.0, -45.0), target: 6.0, ..Case::new("heat25", "Fire", with(vec![("Heat", 25.0f32.into())]), &dark()) },
		Case { target: 0.0, ..Case::new("heat0", "Fire", with(vec![("Heat", 0.0f32.into())]), &dark()) },
		Case { eye: Vec3::new(0.0, 0.0, -15.0), target: 1.2, ..Case::new("size2", "Fire", with(vec![("Size", 2.0f32.into())]), &dark()) },
		Case { eye: Vec3::new(0.0, 0.0, -60.0), target: 6.0, ..Case::new("size10", "Fire", with(vec![("Size", 10.0f32.into())]), &dark()) },
	];
	look("legacy-fire-parts", "LegacyFireParts", cases, 3.0)
}

/// Fire cohorts emitting for 0.25 s (red Color, green SecondaryColor), frozen at 0.3 to 1.5 s.
pub fn legacy_fire_cohort_long() -> Experiment {
	let set = vec![("Color", color([1.0, 0.0, 0.0])), ("SecondaryColor", color([0.0, 1.0, 0.0]))];
	Cohorts::new("Fire", set, dark()).ages(0.25, &[0.3, 0.6, 1.0, 1.5]).experiment("legacy-fire-cohort-long", "LegacyFireCohortLong")
}

/// Fire's steady state frozen at exposure -5 (its total light over the light a cohort's particle
/// gives over its life: the rate): white flames alone (black SecondaryColor) and secondary particles
/// alone (black Color), four snapshots each.
pub fn legacy_fire_steady() -> Experiment {
	let flame = || vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let secondary = || vec![("Color", color([0.0, 0.0, 0.0])), ("SecondaryColor", color([1.0, 1.0, 1.0]))];
	let mut cases = Vec::new();
	for (labels, set) in [(["flame1", "flame2", "flame3", "flame4"], flame()), (["secondary1", "secondary2", "secondary3", "secondary4"], secondary())] {
		for label in labels {
			cases.push(Case { eye: Vec3::new(0.0, 0.0, -60.0), target: 2.0, ..Case::new(label, "Fire", set.clone(), &dim()) });
		}
	}
	look("legacy-fire-steady", "LegacyFireSteady", cases, 3.0)
}

/// Fire's flame (white, black SecondaryColor, exposure -5) growing from its first frame: enabled at
/// 0 and never disabled, frozen at 0.1 to 3 s (does it emit at a rate or up to a count?).
pub fn legacy_fire_growth() -> Experiment {
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	Cohorts::new("Fire", set, dim()).ages(10.0, &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.8, 1.0, 1.25, 1.5, 2.0, 3.0]).experiment("legacy-fire-growth", "LegacyFireGrowth")
}

/// Fire's flame (white, exposure -5) after its steady state: enabled for 2 s then off, frozen 0 to
/// 1 s later (the ages its steady state holds), and enabled again at 2.5 s for 1 to 8 frames,
/// frozen 0.35 s after (does a fire enabled again emit a particle each frame, as a new one does?).
pub fn legacy_fire_after() -> Experiment {
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let mut cohorts = Cohorts::new("Fire", set, dim());
	for after in [0.0f32, 0.1, 0.2, 0.3, 0.5, 0.8, 1.0] {
		cohorts = cohorts.run(&format!("off{after}"), &[(0.0, 2.0)], 2.0 + after);
	}
	for frames in [1u32, 2, 3, 4, 6, 8] {
		let to = 2.5 + frames as f32 / 60.0 - 0.004;
		cohorts = cohorts.run(&format!("again{frames}"), &[(0.0, 2.0), (2.5, to)], to + 0.35);
	}
	cohorts.experiment("legacy-fire-after", "LegacyFireAfter")
}

/// A moving Fire (white flame, black SecondaryColor, exposure -5): its part slides along X at
/// `speed` studs a second from the start, the fire enabled throughout, frozen at 1.6 and 2.6 s; the
/// particles stay where they were emitted, so the trail's light along X is each age's light (the
/// steady state's, and the first frames' after enabling).
pub fn legacy_fire_moving() -> Experiment {
	let name = "legacy-fire-moving";
	let root = "LegacyFireMoving";
	let mut model = ModelBuilder::new(root);
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let mut runs = Vec::new();
	for (index, speed) in [10.0f32, 20.0].into_iter().enumerate() {
		let label = format!("speed{speed}");
		let start = Vec3::new(-0.5 * speed * 2.6 + index as f32 * 200.0, -6.0, 0.0);
		let part = anchor(&mut model, &label, Affine3A::from_translation(start), Vec3::splat(0.2));
		model.child(part, effect("Fire", &set));
		let motion = vec![Motion {
			target: format!("{root}.{label}"),
			keys: vec![MotionKey { at: 0.0, cframe: vec![0.0, 0.0, 0.0], ..Default::default() }, MotionKey { at: 3.0, cframe: vec![speed * 3.0, 0.0, 0.0], ..Default::default() }],
			relative: true,
			tagged: None,
			physics_from: None,
		}];
		let center = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		for moment in [1.6f32, 2.6] {
			let run = format!("{label}-at{moment}");
			let distance = if speed > 15.0 { 70.0 } else { 45.0 };
			runs.push(Run { label: run.clone(), environment: dim(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -distance)).into(), target: center.into(), fov: 40.0 } });
			let playback = Playback { time: moment + 0.1, motion: motion.clone(), actions: vec![freeze(moment, root)], ..Default::default() };
			timeline::register(name, &run, Timeline { playback, duration: moment + 0.35 });
		}
	}
	wall(&mut model, Vec3::new(100.0, 0.0, 0.0));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// How Fire blends (white flame, black SecondaryColor, exposure -5, frozen steady states): 1, 2 and
/// 4 fires in one part (does light add?), and a fire 1 stud in front of a gray Neon panel's edge
/// (half over the panel, half over black: does it cover what is behind it?).
pub fn legacy_fire_blend() -> Experiment {
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let mut model = ModelBuilder::new("LegacyFireBlend");
	let mut runs = Vec::new();
	let camera = |at: Vec3| CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 1.0, -30.0)).into(), target: (at + Vec3::new(0.0, 1.0, 0.0)).into(), fov: 30.0 };
	for (index, count) in [1usize, 2, 4].into_iter().enumerate() {
		let label = format!("count{count}");
		let at = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = anchor(&mut model, &label, Affine3A::from_translation(at), Vec3::splat(0.2));
		for _ in 0..count {
			model.child(part, effect("Fire", &set));
		}
		runs.push(Run { label: label.clone(), environment: dim(), camera: camera(at) });
		register("legacy-fire-blend", &label, running(Some(&label), 3.0, false, None));
	}
	// a gray Neon panel behind the fire's right half (x = 25.75 · 0.5^4 = 1.6 at exposure 0)
	let at = Vec3::new(300.0, 0.0, 0.0);
	let part = anchor(&mut model, "panel", Affine3A::from_translation(at), Vec3::splat(0.2));
	model.child(part, effect("Fire", &set));
	model.part(&PartSpec { name: "Panel".into(), cframe: Affine3A::from_translation(at + Vec3::new(-5.0, 0.0, 1.0)), size: Vec3::new(10.0, 12.0, 0.2), color: [255, 255, 255], material: 288, cast_shadow: false, ..Default::default() });
	runs.push(Run { label: "panel".into(), environment: dim(), camera: camera(at) });
	register("legacy-fire-blend", "panel", running(Some("panel"), 3.0, false, None));
	wall(&mut model, Vec3::new(150.0, 0.0, 0.0));
	Experiment { name: "legacy-fire-blend".into(), model, probes: Vec::new(), runs }
}

/// Fire's motion by Size and Heat (white flame, black SecondaryColor, exposure -5): a row of fires
/// at Heat -10, 0, 5, 9, 18 and 25 per Size (2, 3, 5, 10), each emitting for 0.05 s, frozen at
/// 0.3, 0.6 and 1 s.
pub fn legacy_fire_grid() -> Experiment {
	let name = "legacy-fire-grid";
	let root = "LegacyFireGrid";
	let mut model = ModelBuilder::new(root);
	let heats = [-10.0f32, 0.0, 5.0, 9.0, 18.0, 25.0];
	let mut runs = Vec::new();
	for (row, size) in [2.0f32, 3.0, 5.0, 10.0].into_iter().enumerate() {
		let scale = size / 5.0;
		let center = Vec3::new(row as f32 * 300.0, 0.0, 0.0);
		let spacing = 12.0 * scale.max(0.6);
		let distance = 60.0 * scale.max(0.6);
		for (column, heat) in heats.iter().enumerate() {
			let at = center + Vec3::new((column as f32 - 2.5) * spacing, -0.2 * distance, 0.0);
			let part = anchor(&mut model, &format!("size{size}-heat{heat}"), Affine3A::from_translation(at), Vec3::splat(0.2));
			let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0])), ("Size", size.into()), ("Heat", (*heat).into()), ("Enabled", false.into())];
			model.child(part, effect("Fire", &set));
		}
		for age in [0.3f32, 0.6, 1.0] {
			let label = format!("size{size}-age{age}");
			runs.push(Run { label: label.clone(), environment: dim(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -distance)).into(), target: center.into(), fov: 40.0 } });
			let playback = Playback { time: age + 0.1, actions: vec![switch(0.0, root, true), switch(0.05, root, false), freeze(age, root)], ..Default::default() };
			timeline::register(name, &label, Timeline { playback, duration: age + 0.35 });
		}
	}
	wall(&mut model, Vec3::new(450.0, 0.0, 0.0));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Smoke and Sparkles stacking (how their particles blend): windows of 1 to 12 frames frozen 0.3 s
/// after them, white Smoke under flat ambient light and white Sparkles at exposure -3, and cohorts
/// emitting for 0.05 s frozen at 0.1 to 3 s.
pub fn legacy_stack(class: &str) -> Option<Experiment> {
	let frames: Vec<f32> = (1..=12).map(|k| k as f32 / 60.0 - 0.004).collect();
	let (set, environment, ages, name) = match class {
		"smoke" => (vec![("Color", color([1.0, 1.0, 1.0]))], Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() }, vec![0.1f32, 0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0], "legacy-smoke-stack"),
		"sparkles" => (vec![("SparkleColor", color([1.0, 1.0, 1.0]))], Environment { exposure_compensation: -3.0, ..dark() }, vec![0.1f32, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0], "legacy-sparkles-stack"),
		_ => return None,
	};
	let class_name = if class == "smoke" { "Smoke" } else { "Sparkles" };
	Some(Cohorts::new(class_name, set, environment).windows(&frames, 0.3).ages(0.05, &ages).experiment(name, "LegacyStack"))
}

/// What in Fire covers what is behind it (exposure -5, frozen steady states 1 stud in front of a
/// white Neon panel): a black fire (Color and SecondaryColor black: only what covers shows), black
/// flames with white SecondaryColor, white flames with black SecondaryColor, and a white fire.
pub fn legacy_fire_cover() -> Experiment {
	let mut model = ModelBuilder::new("LegacyFireCover");
	let mut runs = Vec::new();
	let cases = [("black", [0.0f32; 3], [0.0f32; 3]), ("secondary-white", [0.0; 3], [1.0; 3]), ("flame-white", [1.0; 3], [0.0; 3]), ("white", [1.0; 3], [1.0; 3])];
	for (index, (label, flame, secondary)) in cases.into_iter().enumerate() {
		let at = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = anchor(&mut model, label, Affine3A::from_translation(at), Vec3::splat(0.2));
		model.child(part, effect("Fire", &[("Color", color(flame)), ("SecondaryColor", color(secondary))]));
		model.part(&PartSpec { name: format!("{label}-panel"), cframe: Affine3A::from_translation(at + Vec3::new(0.0, 2.0, 1.0)), size: Vec3::new(14.0, 14.0, 0.2), color: [255, 255, 255], material: 288, cast_shadow: false, ..Default::default() });
		runs.push(Run { label: label.into(), environment: dim(), camera: CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 1.0, -30.0)).into(), target: (at + Vec3::new(0.0, 1.0, 0.0)).into(), fov: 30.0 } });
		register("legacy-fire-cover", label, running(Some(label), 3.0, false, None));
	}
	wall(&mut model, Vec3::new(150.0, 0.0, 0.0));
	Experiment { name: "legacy-fire-cover".into(), model, probes: Vec::new(), runs }
}

/// Fire's look, live (Studio's mean over a burst of frames against ror's mean over seeded
/// snapshots): the default and the catalog items' settings (Fiery Horns Size 3 Heat 8, the Flaming
/// Mohawk Size 3 Heat 5 and a dark SecondaryColor, the Torch Size 2 Heat 18 and red), at exposure 0
/// and -3, from the front.
pub fn legacy_fire_mean() -> Experiment {
	let close = |case: Case| Case { eye: Vec3::new(0.0, 0.0, -25.0), target: 2.5, ..case };
	let cases = vec![
		close(Case::new("default", "Fire", vec![], &dark())),
		close(Case::new("default-ev-3", "Fire", vec![], &Environment { exposure_compensation: -3.0, ..dark() })),
		close(Case::new("horns", "Fire", vec![("Size", 3.0f32.into()), ("Heat", 8.0f32.into())], &dark())),
		close(Case::new("mohawk", "Fire", vec![("Size", 3.0f32.into()), ("Heat", 5.0f32.into()), ("SecondaryColor", color([0.41568628, 0.17254902, 0.050980393]))], &dark())),
		close(Case::new("torch", "Fire", vec![("Size", 2.0f32.into()), ("Heat", 18.0f32.into()), ("Color", color([1.0, 0.7137255, 0.4])), ("SecondaryColor", color([1.0, 0.0, 0.0]))], &dark())),
		close(Case::new("lit", "Fire", vec![], &Environment { brightness: 2.0, ambient: Vec3::splat(0.5), ..dark() })),
	];
	look_with("legacy-fire-mean", "LegacyFireMean", cases, 3.0, true)
}

/// How long Fire's flame lives by Size and Heat: steady white flames (black SecondaryColor: the
/// wisps add nothing) at exposure -3 (under the bloom's threshold), Size 2, 3, 5 and 10 at Heat 9, 18
/// and 25, live (Studio's mean over a burst of frames against ror's over seeded snapshots), each
/// framed to its plume (legacy-fire-mean's torch, Size 2 at Heat 18, stood 1.27x as tall in ror).
pub fn legacy_fire_life() -> Experiment {
	let set = |size: f32, heat: f32| vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0])), ("Size", size.into()), ("Heat", heat.into())];
	let environment = Environment { exposure_compensation: -3.0, ..dark() };
	let case = |label: &'static str, size: f32, heat: f32, distance: f32, target: f32| Case { eye: Vec3::new(0.0, 0.0, -distance), target, ..Case::new(label, "Fire", set(size, heat), &environment) };
	let cases = vec![
		case("size2-heat9", 2.0, 9.0, 20.0, 2.0),
		case("size2-heat18", 2.0, 18.0, 25.0, 2.5),
		case("size2-heat25", 2.0, 25.0, 30.0, 3.5),
		case("size3-heat18", 3.0, 18.0, 28.0, 3.0),
		case("size5-heat9", 5.0, 9.0, 25.0, 2.5),
		case("size5-heat18", 5.0, 18.0, 35.0, 4.0),
		case("size5-heat25", 5.0, 25.0, 45.0, 6.0),
		case("size10-heat25", 10.0, 25.0, 60.0, 7.0),
	];
	look_with("legacy-fire-life", "LegacyFireLife", cases, 3.0, true)
}

/// How much Fire's flame covers by Size and Heat (does a flame with more Heat for its Size give less
/// light by covering less, or by shining less?): black flames (Color and SecondaryColor black: no
/// light, only cover) 3 studs in front of a white Neon panel at exposure -5, at the default Size 5
/// and Heat 9, Size 2 at Heat 9, 18 and 25, and Size 5 at Heat 25, live (Studio's mean over a burst
/// of frames against ror's over seeded snapshots), framed as legacy-fire-life's.
pub fn legacy_fire_hide() -> Experiment {
	let name = "legacy-fire-hide";
	let mut model = ModelBuilder::new("LegacyFireHide");
	let mut runs = Vec::new();
	let cases = [("size5-heat9", 5.0f32, 9.0f32, 25.0f32, 2.5f32), ("size2-heat9", 2.0, 9.0, 20.0, 2.0), ("size2-heat18", 2.0, 18.0, 25.0, 2.5), ("size2-heat25", 2.0, 25.0, 30.0, 3.5), ("size5-heat25", 5.0, 25.0, 45.0, 6.0)];
	for (index, (label, size, heat, distance, height)) in cases.into_iter().enumerate() {
		let at = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = anchor(&mut model, label, Affine3A::from_translation(at), Vec3::splat(0.2));
		model.child(part, effect("Fire", &[("Color", color([0.0; 3])), ("SecondaryColor", color([0.0; 3])), ("Size", size.into()), ("Heat", heat.into())]));
		model.part(&PartSpec { name: format!("{label}-panel"), cframe: Affine3A::from_translation(at + Vec3::new(0.0, height, 3.0)), size: Vec3::new(40.0, 40.0, 0.2), color: [255, 255, 255], material: 288, cast_shadow: false, ..Default::default() });
		let target = at + Vec3::new(0.0, height, 0.0);
		runs.push(Run { label: label.into(), environment: dim(), camera: CameraSpec::LookAt { eye: (target + Vec3::new(0.0, 0.0, -distance)).into(), target: target.into(), fov: 40.0 } });
		let mut setup = running(Some(label), 3.0, true, None);
		setup.seeds = 12;
		register(name, label, setup);
	}
	wall(&mut model, Vec3::new(250.0, 0.0, 0.0));
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Fires in separate parts (white flame, black SecondaryColor, exposure -5, frozen steady states):
/// one alone, two at the same place, two 2 studs apart in depth (does the front one cover the back
/// one's light?), and two side by side 1 stud apart.
pub fn legacy_fire_pair() -> Experiment {
	let set = vec![("Color", color([1.0, 1.0, 1.0])), ("SecondaryColor", color([0.0, 0.0, 0.0]))];
	let mut model = ModelBuilder::new("LegacyFirePair");
	let mut runs = Vec::new();
	let cases: [(&str, &[Vec3]); 4] = [("one", &[Vec3::ZERO]), ("same", &[Vec3::ZERO, Vec3::ZERO]), ("depth", &[Vec3::ZERO, Vec3::new(0.0, 0.0, 2.0)]), ("side", &[Vec3::new(-0.5, 0.0, 0.0), Vec3::new(0.5, 0.0, 0.0)])];
	for (index, (label, offsets)) in cases.into_iter().enumerate() {
		let at = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let group = model.child(model.model, InstanceBuilder::new("Model").with_name(label));
		for (number, offset) in offsets.iter().enumerate() {
			let part = anchor(&mut model, &format!("{label}{number}"), Affine3A::from_translation(at + *offset), Vec3::splat(0.2));
			model.dom.transfer_within(part, group);
			model.child(part, effect("Fire", &set));
		}
		runs.push(Run { label: label.into(), environment: dim(), camera: CameraSpec::LookAt { eye: (at + Vec3::new(0.0, 1.0, -30.0)).into(), target: (at + Vec3::new(0.0, 1.0, 0.0)).into(), fov: 30.0 } });
		register("legacy-fire-pair", label, running(Some(label), 3.0, false, None));
	}
	wall(&mut model, Vec3::new(150.0, 0.0, 0.0));
	Experiment { name: "legacy-fire-pair".into(), model, probes: Vec::new(), runs }
}

/// Sparkles' look, live (Studio's mean over a burst of frames against ror's mean over seeded
/// snapshots): the default purple at exposure 0 and -3, white and black (what does not take the
/// colour?) at -3, from the front and from above, and the catalog items' colours.
pub fn legacy_sparkles_mean() -> Experiment {
	let dim3 = Environment { exposure_compensation: -3.0, ..dark() };
	let close = |case: Case| Case { eye: Vec3::new(0.0, 0.0, -25.0), target: 1.5, ..case };
	let cases = vec![
		close(Case::new("default", "Sparkles", vec![], &dark())),
		close(Case::new("default-ev-3", "Sparkles", vec![], &dim3)),
		close(Case::new("white-ev-3", "Sparkles", vec![("SparkleColor", color([1.0, 1.0, 1.0]))], &dim3)),
		close(Case::new("black-ev-3", "Sparkles", vec![("SparkleColor", color([0.0, 0.0, 0.0]))], &dim3)),
		Case { eye: Vec3::new(0.0, 25.0, -0.1), target: 0.0, ..Case::new("top-ev-3", "Sparkles", vec![], &dim3) },
		close(Case::new("golden", "Sparkles", vec![("SparkleColor", color([1.0, 0.80784315, 0.03137255]))], &dark())),
		close(Case::new("wings", "Sparkles", vec![("SparkleColor", color([0.8784314, 0.99215686, 1.0]))], &dark())),
	];
	look_with("legacy-sparkles-mean", "LegacySparklesMean", cases, 3.0, true)
}

/// Sparkles live over a bright background (Studio's mean over a burst of frames against ror's mean
/// over seeded snapshots): a gray Neon panel (x = 0.5 at exposure 0, about a daytime sky) 12 studs
/// behind, the default purple, golden and white. In linear light a star adds the same light as over
/// black; blending in the √ space it adds 2 √(panel · star) more.
pub fn legacy_sparkles_panel() -> Experiment {
	let close = |case: Case| Case { eye: Vec3::new(0.0, 0.0, -25.0), target: 1.5, ..case };
	let cases = vec![
		close(Case::new("default", "Sparkles", vec![], &dark())),
		close(Case::new("golden", "Sparkles", vec![("SparkleColor", color([1.0, 0.6666667, 0.0]))], &dark())),
		close(Case::new("white", "Sparkles", vec![("SparkleColor", color([1.0, 1.0, 1.0]))], &dark())),
	];
	let mut experiment = look_with("legacy-sparkles-panel", "LegacySparklesPanel", cases, 3.0, true);
	for index in 0..3 {
		let at = Vec3::new(index as f32 * 100.0, 1.5, 12.0);
		experiment.model.part(&PartSpec { name: "Panel".into(), cframe: Affine3A::from_translation(at), size: Vec3::new(40.0, 30.0, 0.2), color: [96, 96, 96], material: 288, cast_shadow: false, ..Default::default() });
	}
	experiment
}

/// Smoke's look, live (Studio's mean over a burst of frames 0.3 s apart against ror's mean over
/// seeded snapshots), lit by flat ambient light over black: the default (Size 1, Opacity 0.5,
/// RiseVelocity 1), Size 5 and 20, RiseVelocity 0 and 5, Opacity 1, a red one, the Cursed Flames'
/// settings (Size 0.1, Opacity 0.1, RiseVelocity 0.1, dark purple) and the default under the sun.
pub fn legacy_smoke_mean() -> Experiment {
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let sun = Environment { brightness: 2.0, ..dark() };
	let view = |distance: f32, target: f32| (Vec3::new(0.0, 0.0, -distance), target);
	let case = |label: &'static str, set: Vec<(&'static str, Variant)>, (eye, target): (Vec3, f32), environment: &Environment| Case { eye, target, ..Case::new(label, "Smoke", set, environment) };
	let cases = vec![
		case("default", vec![], view(30.0, 2.0), &flat),
		case("size5", vec![("Size", 5.0f32.into())], view(50.0, 4.0), &flat),
		case("size20", vec![("Size", 20.0f32.into())], view(120.0, 10.0), &flat),
		case("rise0", vec![("RiseVelocity", 0.0f32.into())], view(30.0, 0.0), &flat),
		case("rise5", vec![("RiseVelocity", 5.0f32.into())], view(50.0, 8.0), &flat),
		case("opacity1", vec![("Opacity", 1.0f32.into())], view(30.0, 2.0), &flat),
		case("red", vec![("Color", color([1.0, 0.0, 0.0]))], view(30.0, 2.0), &flat),
		case("cursed", vec![("Size", 0.1f32.into()), ("Opacity", 0.1f32.into()), ("RiseVelocity", 0.1f32.into()), ("Color", color([0.09411766, 0.0, 0.14117648]))], view(20.0, 1.0), &flat),
		case("sun", vec![], view(30.0, 2.0), &sun),
	];
	look_with("legacy-smoke-mean", "LegacySmokeMean", cases, 8.0, true)
}

/// Smoke cohorts under flat ambient light (white, the defaults otherwise): emitting for 0.3 to 2 s,
/// frozen 0.5 s after (how fast it emits), and emitting for 1 s, frozen at 1 to 9 s (each puff's
/// growth, rise and fade).
pub fn legacy_smoke_cohort() -> Experiment {
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut cohorts = Cohorts::new("Smoke", vec![("Color", color([1.0, 1.0, 1.0]))], flat).windows(&[0.3, 0.6, 1.0, 1.5, 2.0], 0.5).ages(1.0, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 9.0]);
	cohorts.count = 4;
	cohorts.spacing = 25.0;
	cohorts.distance = 90.0;
	cohorts.experiment("legacy-smoke-cohort", "LegacySmokeCohort")
}

/// One Smoke puff (enabled for 0.2 s: it emits its first 0.155 s after, legacy-smoke-stack) under flat
/// ambient light at exposure +2 (faint puffs above the 8-bit floor), frozen at 0.3 to 5.5 s (its
/// light, size and rise over age), and windows of 0.2 to 1.2 s all frozen at 2.5 s (a step of light
/// per puff: when each is born).
pub fn legacy_smoke_single() -> Experiment {
	let bright = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), exposure_compensation: 2.0, ..dark() };
	let mut cohorts = Cohorts::new("Smoke", vec![("Color", color([1.0, 1.0, 1.0]))], bright).ages(0.2, &[0.3, 0.6, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 5.5]);
	for window in [0.3f32, 0.35, 0.4, 0.45, 0.5, 0.55, 0.6, 0.7, 0.8, 1.0, 1.2] {
		cohorts = cohorts.run(&format!("window{window}"), &[(0.0, window)], 2.5);
	}
	cohorts.count = 4;
	cohorts.spacing = 25.0;
	cohorts.distance = 90.0;
	cohorts.experiment("legacy-smoke-single", "LegacySmokeSingle")
}

/// What Smoke covers: black Smoke 25 studs in front of a gray Neon panel (x = 0.1 at exposure 0;
/// that far, puffs do not fade as they near it, as soft particles do) under flat ambient light at
/// exposure +2, as legacy-smoke-single's white puffs: one puff frozen at 1, 2.5 and 3.5 s, a second's
/// puffs frozen at 2.5 s, and the steady state (emitting from 0, frozen at 7 s). The panel shows
/// through 1 - a of a puff whose white light is a (linear light) or (1 - a)² of one whose white light
/// is a² (Roblox's √ space).
pub fn legacy_smoke_cover() -> Experiment {
	let bright = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), exposure_compensation: 2.0, ..dark() };
	let mut cohorts = Cohorts::new("Smoke", vec![("Color", color([0.0, 0.0, 0.0]))], bright).ages(0.2, &[1.0, 2.5, 3.5]);
	cohorts = cohorts.run("window1", &[(0.0, 1.0)], 2.5).run("steady", &[(0.0, 7.0)], 7.0);
	cohorts.count = 4;
	cohorts.spacing = 25.0;
	cohorts.distance = 90.0;
	let mut experiment = cohorts.experiment("legacy-smoke-cover", "LegacySmokeCover");
	smoke_panel(&mut experiment.model);
	experiment
}

/// legacy-smoke-cover's gray Neon panel, 25 studs behind the row of emitters.
fn smoke_panel(model: &mut ModelBuilder) {
	model.part(&PartSpec { name: "Panel".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -10.0, 25.0)), size: Vec3::new(140.0, 50.0, 0.2), color: [64, 64, 64], material: 288, cast_shadow: false, ..Default::default() });
}

/// How Smoke's cover builds up (black Smoke over legacy-smoke-cover's panel, exposure +2): emitting
/// from 0 and frozen at 2 to 6 s, and puffs of one second born at 0 and at 3 s, both seen 2 s after
/// (do later puffs cover as the first ones do?).
pub fn legacy_smoke_late() -> Experiment {
	let bright = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), exposure_compensation: 2.0, ..dark() };
	let mut cohorts = Cohorts::new("Smoke", vec![("Color", color([0.0, 0.0, 0.0]))], bright);
	for moment in [2.0f32, 3.0, 4.0, 5.0, 6.0] {
		cohorts = cohorts.run(&format!("steady{moment}"), &[(0.0, moment)], moment);
	}
	cohorts = cohorts.run("early", &[(0.0, 1.0)], 3.0).run("late", &[(3.0, 4.0)], 6.0);
	cohorts.count = 4;
	cohorts.spacing = 25.0;
	cohorts.distance = 90.0;
	let mut experiment = cohorts.experiment("legacy-smoke-late", "LegacySmokeLate");
	smoke_panel(&mut experiment.model);
	experiment
}

/// White Smoke's steady state over the dark wall under flat ambient light, as a timeline (emitting
/// from 0, frozen at 6 s) at exposure 0 and +2: legacy-smoke-late's cover in light.
pub fn legacy_smoke_white() -> Experiment {
	let flat = |exposure_compensation: f32| Environment { brightness: 0.0, ambient: Vec3::splat(0.5), exposure_compensation, ..dark() };
	let mut cohorts = Cohorts::new("Smoke", vec![("Color", color([1.0, 1.0, 1.0]))], flat(0.0)).run("steady", &[(0.0, 6.0)], 6.0);
	cohorts.count = 4;
	cohorts.spacing = 25.0;
	cohorts.distance = 90.0;
	let mut experiment = cohorts.experiment("legacy-smoke-white", "LegacySmokeWhite");
	if let Some(timeline) = timeline::get("legacy-smoke-white", "steady") {
		timeline::register("legacy-smoke-white", "steady-ev2", timeline);
	}
	experiment.runs.push(Run { label: "steady-ev2".into(), environment: flat(2.0), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -90.0], target: [0.0, 0.0, 0.0], fov: 40.0 } });
	experiment
}

/// Do legacy effects thin far off, as a ParticleEmitter's Rate does (notes/render.md "Particles":
/// none from 1,000 studs)? Fire (Size 25, Heat 25) and Smoke (Size 30) on tiny parts 100 to 1,300
/// studs from the camera along rays fanned across the view, running for 3 s and frozen; the Fire in
/// darkness, the Smoke under flat ambient light (a run each).
pub fn legacy_far() -> Experiment {
	let name = "legacy-far";
	let mut model = ModelBuilder::new("LegacyFar");
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let distances = [100.0f32, 300.0, 500.0, 700.0, 900.0, 1100.0, 1300.0];
	let mut runs = Vec::new();
	for (class, set, environment, origin) in [
		("Fire", vec![("Size", Variant::Float32(25.0)), ("Heat", Variant::Float32(25.0))], dark(), Vec3::ZERO),
		("Smoke", vec![("Size", Variant::Float32(30.0)), ("RiseVelocity", Variant::Float32(5.0))], flat, Vec3::new(5000.0, 0.0, 0.0)),
	] {
		for (column, distance) in distances.iter().enumerate() {
			let at = origin + Vec3::new((3.0 - column as f32) * 0.12, -0.05, 1.0).normalize() * *distance;
			let part = anchor(&mut model, &format!("{class}-{distance}"), Affine3A::from_translation(at), Vec3::splat(0.2));
			model.child(part, effect(class, &set));
		}
		let label = class.to_lowercase();
		runs.push(Run { label: label.clone(), environment, camera: CameraSpec::LookAt { eye: origin.into(), target: (origin + Vec3::Z).into(), fov: 40.0 } });
		register(name, &label, running(None, 3.0, false, None));
	}
	Experiment { name: name.into(), model, probes: Vec::new(), runs }
}

/// Fire's thinning curve (`legacy-far`: Size 25 Fires thin more than a ParticleEmitter's Rate between
/// 500 and 900 studs, their overlapping flames hiding how much): Size 8 Fires (less overlap, their
/// light nearer their particles' count) every 100 studs from 200 to 1,000, and at 50 and 100 for
/// reference, in darkness, running for 3 s and frozen.
pub fn legacy_far_fire() -> Experiment {
	let name = "legacy-far-fire";
	let mut model = ModelBuilder::new("LegacyFarFire");
	let distances = [50.0f32, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0, 1000.0];
	for (column, distance) in distances.iter().enumerate() {
		let at = Vec3::new((5.0 - column as f32) * 0.07, -0.05, 1.0).normalize() * *distance;
		let part = anchor(&mut model, &format!("fire-{distance}"), Affine3A::from_translation(at), Vec3::splat(0.2));
		model.child(part, effect("Fire", &[("Size", Variant::Float32(8.0)), ("Heat", Variant::Float32(25.0))]));
	}
	register(name, "fire", running(None, 3.0, false, None));
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, 1.0], fov: 40.0 };
	Experiment { name: name.into(), model, probes: Vec::new(), runs: vec![Run { label: "fire".into(), environment: dark(), camera }] }
}
