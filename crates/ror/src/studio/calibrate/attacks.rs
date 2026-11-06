//! Effect runs (notes/render.md "Playback"): effects taken from a bundle (a model file of effects, kept
//! out of Git) and played in the capture Studio and in ror from one playback, at a few moments.
//! `vfx-attack:<bundle file>|auto:<effect path>` plays any effect of a bundle by its emit attributes
//! (`auto`); `ROR_BASELINE=playback` draws what ror drew before playbacks (each emitter's steady
//! state, no trails).

use anyhow::{Context, Result, bail};
use glam::{Affine3A, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::timeline::{Timeline, register};
use super::{Experiment, Run};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, cframe};
use crate::scene::playback::{Action, Emit, Motion, MotionKey, Playback};
use crate::scene::props;

pub(super) const ROOT: &str = "VfxAttack";

/// The first instance at a dotted path from the DOM's root.
pub(super) fn find(dom: &WeakDom, path: &str) -> Result<Ref> {
	let mut current = dom.root_ref();
	for segment in crate::scene::load::split_path(path)? {
		let instance = dom.get_by_ref(current).context("broken DOM")?;
		current = instance.children().iter().copied().find(|&child| dom.get_by_ref(child).is_some_and(|child| child.name == segment)).with_context(|| format!("no {segment:?} in {path}"))?;
	}
	Ok(current)
}

pub(super) fn descendants(dom: &WeakDom, referent: Ref) -> Vec<Ref> {
	let mut list = Vec::new();
	let mut stack = vec![referent];
	while let Some(current) = stack.pop() {
		list.push(current);
		if let Some(instance) = dom.get_by_ref(current) {
			stack.extend(instance.children().iter().copied());
		}
	}
	list
}

/// The centre of the parts' centres under an instance.
pub(super) fn center(dom: &WeakDom, referent: Ref) -> Vec3 {
	let points: Vec<Vec3> = descendants(dom, referent).into_iter().filter_map(|child| dom.get_by_ref(child)).filter(|child| props::is_a(child, "BasePart")).filter_map(|part| props::cframe(part, "CFrame")).map(|cframe| Vec3::from(cframe.translation)).collect();
	if points.is_empty() {
		return Vec3::ZERO;
	}
	let (min, max) = points.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(min, max), point| (min.min(*point), max.max(*point)));
	(min + max) * 0.5
}

/// A bundle opened as an experiment's model: pieces are moved into `VfxAttack`.
pub(super) struct Bundle {
	pub(super) model: ModelBuilder,
}

impl Bundle {
	pub(super) fn open(path: &str) -> Result<Self> {
		let document = crate::scene::load::read_file(std::path::Path::new(path))?;
		let mut dom = document.dom;
		let model = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name(ROOT));
		Ok(Self { model: ModelBuilder { dom, model } })
	}

	/// Moves the instance at `path` into the experiment's model, its parts centred at `at` (models
	/// under it pivot at their parts' centre, unturned, without a PrimaryPart: Studio's GetPivot and
	/// ror agree); returns its path in the model.
	pub(super) fn take(&mut self, path: &str, at: Vec3) -> Result<String> {
		let referent = find(&self.model.dom, path)?;
		self.model.dom.transfer_within(referent, self.model.model);
		self.place(referent, at);
		let name = self.model.dom.get_by_ref(referent).map(|instance| instance.name.clone()).unwrap_or_default();
		Ok(format!("{ROOT}.{name}"))
	}

	/// Moves the instance at `path` of another bundle into a folder `folder` of the experiment's
	/// model, placed as [`Bundle::take`] places it; returns its path in the model.
	pub(super) fn adopt(&mut self, other: &mut Bundle, path: &str, folder: &str, at: Vec3) -> Result<String> {
		let referent = find(&other.model.dom, path)?;
		let parent = self.model.dom.insert(self.model.model, InstanceBuilder::new("Folder").with_name(folder));
		other.model.dom.transfer(referent, &mut self.model.dom, parent);
		self.place(referent, at);
		let name = self.model.dom.get_by_ref(referent).map(|instance| instance.name.clone()).unwrap_or_default();
		Ok(format!("{ROOT}.{folder}.{name}"))
	}

	/// Moves an instance's parts so that their centre is at `at`; models under it pivot there.
	fn place(&mut self, referent: Ref, at: Vec3) {
		let shift = at - center(&self.model.dom, referent);
		for child in descendants(&self.model.dom, referent) {
			let Some(instance) = self.model.dom.get_by_ref(child) else {
				continue;
			};
			if props::is_a(instance, "BasePart")
				&& let Some(cframe) = props::cframe(instance, "CFrame")
			{
				self.model.set(child, "CFrame", crate::scene::build::cframe(Affine3A::from_translation(shift) * cframe));
			}
		}
		for child in descendants(&self.model.dom, referent) {
			if self.model.dom.get_by_ref(child).is_some_and(|instance| instance.class == "Model") {
				let pivot = center(&self.model.dom, child);
				self.model.set(child, "WorldPivotData", Variant::OptionalCFrame(Some(cframe(Affine3A::from_translation(pivot)))));
				self.model.set(child, "PrimaryPart", Variant::Ref(Ref::none()));
			}
		}
	}

	/// The world CFrame of a part (or a model's pivot) in the model.
	pub(super) fn cframe(&self, path: &str) -> Result<Affine3A> {
		let referent = find(&self.model.dom, path)?;
		let instance = self.model.dom.get_by_ref(referent).context("broken DOM")?;
		props::cframe(instance, "CFrame").or_else(|| props::cframe(instance, "WorldPivotData")).with_context(|| format!("{path} has no CFrame"))
	}

}


/// Every emitter at or under `target` by its attributes, as the runs were captured ("delayed": the
/// Studio runner's timing, not the "attributes" mode's; both sides play the same timeline).
pub(super) fn emit(at: f32, target: &str) -> Action {
	Action { at, target: target.into(), emit: Some(Emit::Mode("delayed".into())), ..Default::default() }
}

pub(super) fn switch(at: f32, target: &str, enabled: bool, class: Option<&str>) -> Action {
	Action { at, target: target.into(), enabled: Some(enabled), class: class.map(str::to_string), ..Default::default() }
}


/// An attack: the bundle's pieces in a model, the playback (its `time` set per moment), the moments
/// and the camera.
struct Attack {
	model: ModelBuilder,
	playback: Playback,
	moments: Vec<f32>,
	camera: CameraSpec,
}


/// Studio's capture viewport in physical pixels (`CaptureSession`), the shape a generic run frames.
pub(super) const VIEW: (u32, u32) = (1644, 1116);

/// The scene ror draws for `playback` at its moment, as a timeline run draws it (60 frames a second).
pub(super) fn scene_at(bytes: &[u8], playback: &Playback) -> Result<crate::scene::Scene> {
	let mut document = crate::scene::load::read_bytes(bytes)?;
	let resolved = playback.resolve(&document)?;
	resolved.apply_state(&mut document)?;
	let mut scene = crate::scene::extract::extract(&document);
	resolved.apply_history(&document, &mut scene);
	Ok(scene)
}

/// Where ror's playback of a model draws at `moments`: its particles (on each axis the 4th to 96th
/// percentile of their centres, grown by their median half size), the visible parts, the trails'
/// points and the beams' ends; at least 6 studs across.
pub(super) fn drawn_bounds(bytes: &[u8], playback: &Playback, moments: &[f32]) -> Result<(Vec3, Vec3)> {
	let mut centres: Vec<Vec3> = Vec::new();
	let mut sizes: Vec<f32> = Vec::new();
	let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
	let mut include = |low: Vec3, high: Vec3| {
		min = min.min(low);
		max = max.max(high);
	};
	for &moment in moments {
		let scene = scene_at(bytes, &Playback { time: moment, ..playback.clone() })?;
		for emitter in &scene.particles {
			for particle in crate::render::particles::simulate(emitter) {
				if particle.opacity > 0.02 && particle.size > 0.0 {
					centres.push(particle.position);
					sizes.push(particle.size * (1.0 + particle.squash.abs()));
				}
			}
		}
		for part in scene.parts.iter().filter(|part| part.transparency < 0.98) {
			let extent = Vec3::from(part.cframe.matrix3.abs() * glam::Vec3A::from(part.size * 0.5));
			let center = Vec3::from(part.cframe.translation);
			include(center - extent, center + extent);
		}
		for point in scene.trails.iter().flat_map(|trail| trail.points.iter()) {
			include(point.position0.min(point.position1), point.position0.max(point.position1));
		}
		for beam in &scene.beams {
			let (a, b) = (Vec3::from(beam.attachment0.translation), Vec3::from(beam.attachment1.translation));
			include(a.min(b), a.max(b));
		}
	}
	if !centres.is_empty() {
		sizes.sort_by(f32::total_cmp);
		let size = sizes[sizes.len() / 2];
		for axis in 0..3 {
			let mut values: Vec<f32> = centres.iter().map(|centre| centre[axis]).collect();
			values.sort_by(f32::total_cmp);
			let at = |share: f32| values[((values.len() - 1) as f32 * share).round() as usize];
			let (mut low, mut high) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
			low[axis] = at(0.04) - size;
			high[axis] = at(0.96) + size;
			include(low, high);
		}
	}
	if min.x > max.x || min.y > max.y || min.z > max.z {
		bail!("auto: the effect draws nothing at {moments:?} s");
	}
	let center = (min + max) * 0.5;
	let half = ((max - min) * 0.5).max(Vec3::splat(3.0));
	Ok((center - half, center + half))
}

/// A number attribute of an instance.
pub(super) fn attribute(instance: &rbx_dom_weak::Instance, name: &str) -> Option<f32> {
	match instance.properties.get(&rbx_dom_weak::ustr("Attributes")) {
		Some(Variant::Attributes(attributes)) => match attributes.get(name)? {
			Variant::Float64(value) => Some(*value as f32),
			Variant::Float32(value) => Some(*value),
			Variant::Int32(value) => Some(*value as f32),
			_ => None,
		},
		_ => None,
	}
}

/// A generic run of any bundle effect, `auto:<effect path>[|move|still][|only:<names>][|bare][|nolights][|<moment>,...]`,
/// played much as effect scripts play effects by their emit attributes (notes/render.md "Playback"):
/// at 0 every emitter emits by its attributes as the runs were captured ("delayed": EmitCount, 5
/// without one, after EmitDelay, then enabled for EmitDuration), emitters enabled in the file run
/// from the first frame, and the trails and beams are switched on. Trails draw nothing at rest, so an
/// effect with trails moves (`move`, `still` keeps it put): 24 studs across the view and back down
/// over an arc in 0.5 s. The moments are 0.1, 0.3 and 0.6 s after the first burst (the smallest
/// EmitDelay) by default, and the camera frames what ror's playback draws at them from 35 degrees
/// round and 20 above. It is not the game's choreography: Studio and ror play the same timeline, so
/// the runs measure the rendering. `nolights` takes the effect's PointLights, SpotLights and
/// SurfaceLights out (what they light).
fn auto(path: &str, spec: &str) -> Result<(String, Attack)> {
	let mut options = spec.split('|');
	let effect_path = options.next().filter(|path| !path.is_empty()).context("auto:<effect path>[|move|still][|only:<names>][|bare][|nolights][|<moments>]")?;
	let mut moving = None;
	let mut moments = None;
	let mut only: Option<Vec<String>> = None;
	let mut bare = false;
	let mut lights = true;
	for option in options {
		match option.trim() {
			"move" => moving = Some(true),
			"still" => moving = Some(false),
			"bare" => bare = true,
			"nolights" => lights = false,
			names if names.starts_with("only:") => only = Some(names["only:".len()..].split(',').map(|name| name.trim().to_string()).collect()),
			list => moments = Some(list.split(',').map(|moment| moment.trim().parse::<f32>()).collect::<Result<Vec<_>, _>>().with_context(|| format!("auto: {list:?} is not a list of moments"))?),
		}
	}
	let mut bundle = Bundle::open(path)?;
	let center = Vec3::new(0.0, 4.0, 0.0);
	let effect = bundle.take(effect_path, center)?;
	// played and framed as the whole effect, so a run of some of its emitters lines up with the whole
	// one's (its first burst, its trails' flight, the camera)
	let play = auto_play(&bundle, &effect, moving, 0.0)?;
	let whole = bundle.model.to_rbxm()?;
	let referent = find(&bundle.model.dom, &effect)?;
	// `only`: the emitters, trails and beams of these names alone (the parts move as they do)
	if let Some(names) = &only {
		let dropped: Vec<rbx_dom_weak::types::Ref> = descendants(&bundle.model.dom, referent)
			.into_iter()
			.filter(|child| bundle.model.dom.get_by_ref(*child).is_some_and(|instance| matches!(instance.class.as_str(), "ParticleEmitter" | "Trail" | "Beam") && !names.iter().any(|name| *name == instance.name)))
			.collect();
		for child in dropped {
			bundle.model.dom.destroy(child);
		}
	}
	// `bare`: the effect's parts unseen (their particles, trails and beams alone)
	if bare {
		let parts: Vec<rbx_dom_weak::types::Ref> = descendants(&bundle.model.dom, referent).into_iter().filter(|child| bundle.model.dom.get_by_ref(*child).is_some_and(|instance| props::is_a(instance, "BasePart"))).collect();
		for part in parts {
			bundle.model.set(part, "Transparency", 1.0f32);
		}
	}
	if !lights {
		let dropped: Vec<rbx_dom_weak::types::Ref> = descendants(&bundle.model.dom, referent).into_iter().filter(|child| bundle.model.dom.get_by_ref(*child).is_some_and(|instance| matches!(instance.class.as_str(), "PointLight" | "SpotLight" | "SurfaceLight"))).collect();
		for child in dropped {
			bundle.model.dom.destroy(child);
		}
	}
	let moments = moments.unwrap_or_else(|| [0.1f32, 0.3, 0.6].iter().map(|moment| ((moment + play.first_burst) * 1000.0).round() / 1000.0).collect());
	let playback = Playback { actions: play.actions, motion: play.motion, ..Default::default() };
	let mut name = format!("auto-{}", slug(effect_path));
	if let Some(names) = &only {
		name = format!("{name}-only-{}", slug(&names.join("-")));
	}
	if bare {
		name.push_str("-bare");
	}
	if !lights {
		name.push_str("-nolights");
	}
	match moving {
		Some(true) => name.push_str("-move"),
		Some(false) => name.push_str("-still"),
		None => {}
	}
	let camera = pinned_camera(&format!("vfx-attack-{name}"), &format!("{path}|auto:{spec}"), || {
		let (low, high) = drawn_bounds(&whole, &playback, &moments)?;
		let fov = 50.0;
		let placed = CameraSpec::Orbit { yaw: 35.0, pitch: 20.0, distance: None, padding: 0.08, target: None, fov }.place(Some((low, high)), VIEW.0, VIEW.1);
		Ok(CameraSpec::LookAt { eye: placed.eye.to_array(), target: ((low + high) * 0.5).to_array(), fov })
	})?;
	Ok((name, Attack { model: bundle.model, playback, moments, camera }))
}

/// What the generic run plays for an effect (`auto`), from `start` seconds on.
pub(super) struct AutoPlay {
	pub(super) actions: Vec<Action>,
	pub(super) motion: Vec<Motion>,
	/// Seconds from `start` to its first burst (the smallest EmitDelay of its emitters).
	pub(super) first_burst: f32,
	pub(super) moving: bool,
}

/// The generic run's playback of the effect at `effect` (its path in the model) from `start` on:
/// every emitter emits by its attributes, the trails and beams switch on, and an effect with trails
/// (or `moving`) flies 24 studs across and back down over an arc in 0.5 s (`auto`).
pub(super) fn auto_play(bundle: &Bundle, effect: &str, moving: Option<bool>, start: f32) -> Result<AutoPlay> {
	let referent = find(&bundle.model.dom, effect)?;
	let members: Vec<&rbx_dom_weak::Instance> = descendants(&bundle.model.dom, referent).into_iter().filter_map(|child| bundle.model.dom.get_by_ref(child)).collect();
	let has_trails = members.iter().any(|child| child.class == "Trail");
	let first_burst = members.iter().filter(|child| child.class == "ParticleEmitter").map(|emitter| attribute(emitter, "EmitDelay").unwrap_or(0.0).max(0.0)).fold(f32::INFINITY, f32::min);
	let actions = vec![emit(start, effect), switch(start, effect, true, Some("Trail")), switch(start, effect, true, Some("Beam"))];
	let moving = moving.unwrap_or(has_trails);
	let mut motion = Vec::new();
	if moving {
		let rest = Vec3::from(bundle.cframe(effect)?.translation);
		let keys = (0..=10)
			.map(|step| {
				let alpha = step as f32 / 10.0;
				let at = rest + Vec3::new(-12.0 + 24.0 * alpha, 4.0 * (alpha * std::f32::consts::PI).sin(), 0.0);
				MotionKey { at: start + alpha * 0.5, cframe: at.to_array().to_vec(), ..Default::default() }
			})
			.collect();
		motion.push(Motion { target: effect.to_string(), keys, relative: false, tagged: None, physics_from: None });
	}
	Ok(AutoPlay { actions, motion, first_burst: if first_burst.is_finite() { first_burst } else { 0.0 }, moving })
}

/// A path as an experiment name's part: lower-case letters and digits, other runs as one dash.
pub(super) fn slug(text: &str) -> String {
	let mut label = String::new();
	for c in text.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }) {
		if c != '-' || !label.ends_with('-') {
			label.push(c);
		}
	}
	label.trim_matches('-').to_string()
}

/// The camera of a run framed from ror's own drawing (`frame`): the one chosen when the experiment
/// first ran for the same spec, kept in its output's camera.json, so that a later build (or a
/// baseline's, with ROR_RERENDER) draws from the camera Studio captured.
fn pinned_camera(experiment: &str, spec: &str, frame: impl FnOnce() -> Result<CameraSpec>) -> Result<CameraSpec> {
	let path = crate::output::repo_dir().join("out").join("calibrate").join(experiment).join("camera.json");
	if let Ok(text) = std::fs::read_to_string(&path)
		&& let Ok(record) = serde_json::from_str::<serde_json::Value>(&text)
		&& record["spec"] == spec
		&& let Ok(camera) = serde_json::from_value::<CameraSpec>(record["camera"].clone())
	{
		return Ok(camera);
	}
	let camera = frame()?;
	crate::output::write_atomic(&path, serde_json::to_string_pretty(&serde_json::json!({ "spec": spec, "camera": camera }))?.as_bytes())?;
	eprintln!("ror: {experiment}: camera {}", serde_json::to_string(&camera)?);
	Ok(camera)
}

/// One emitter of a bundle in isolation, `vfx-emitter:<bundle file>|<emitter path>|<moment>|<variants>`
/// (notes/render.md "Particles"): a copy per variant in a row along X, each on its own invisible
/// anchor (on an Attachment, or on a part of the original part's size where the original emits
/// from a part), `label` or `label:Property=value,...` setting properties at 0 (numbers, booleans,
/// `[x y z]` vectors and colours; `Enabled=true` switches the copy on, its Rate running), every
/// copy emitting `count` particles at 0 (`#<count>` after the moment, 1 by default, 0 for none),
/// frozen with TimeScale 0 at the moment, drawn 0.1 s later over a black
/// wall in the dark (`|sky`: the default sky). One property varied against the original shows what
/// it does in Studio.
pub fn vfx_emitter(spec: &str) -> Result<Experiment> {
	let parts: Vec<&str> = spec.split('|').collect();
	let [path, emitter_path, moment, variants, rest @ ..] = parts.as_slice() else {
		bail!("vfx-emitter:<bundle file>|<emitter path>|<moment>[#<count>]|<label>[:Property=value,...];...[|sky][|turned]");
	};
	let (moment, count) = match moment.split_once('#') {
		Some((moment, count)) => (moment.trim().parse::<f32>()?, count.trim().parse::<u32>()?),
		None => (moment.trim().parse::<f32>()?, 1),
	};
	let sky = rest.contains(&"sky");
	// `turned`: the copies turned as the original is in the world (its attachment's turn in its part's)
	let turned = rest.contains(&"turned");
	let mut bundle = Bundle::open(path)?;
	let original = find(&bundle.model.dom, emitter_path)?;
	let (properties, parent_size, rotation) = {
		let instance = bundle.model.dom.get_by_ref(original).context("broken DOM")?;
		if instance.class != "ParticleEmitter" {
			bail!("vfx-emitter: {emitter_path} is a {}, not a ParticleEmitter", instance.class);
		}
		let parent = bundle.model.dom.get_by_ref(instance.parent()).context("the emitter's parent")?;
		let size = props::is_a(parent, "BasePart").then(|| props::vec3(parent, "Size", Vec3::ONE));
		let own = props::cframe(parent, "CFrame").unwrap_or(Affine3A::IDENTITY).matrix3;
		let rotation = match bundle.model.dom.get_by_ref(parent.parent()).and_then(|part| props::cframe(part, "CFrame")) {
			Some(part) if size.is_none() => part.matrix3 * own,
			_ => own,
		};
		(instance.properties.clone(), size, if turned { rotation } else { glam::Mat3A::IDENTITY })
	};
	let variants: Vec<(String, Vec<(String, serde_json::Value)>)> = variants
		.split(';')
		.filter(|variant| !variant.trim().is_empty())
		.map(|variant| {
			let (label, sets) = variant.split_once(':').unwrap_or((variant, ""));
			let sets = sets
				.split(',')
				.filter(|set| !set.trim().is_empty())
				.map(|set| {
					let (name, value) = set.split_once('=').with_context(|| format!("vfx-emitter: {set:?} is not Property=value"))?;
					let value = value.trim();
					let json = if value.starts_with('[') { value.replace(' ', ",") } else { value.to_string() };
					Ok((name.trim().to_string(), serde_json::from_str(&json).with_context(|| format!("vfx-emitter: the value {value:?}"))?))
				})
				.collect::<Result<Vec<_>>>()?;
			Ok((label.trim().to_string(), sets))
		})
		.collect::<Result<_>>()?;
	let size_estimate = |properties: &rbx_dom_weak::UstrMap<Variant>| match properties.get(&rbx_dom_weak::ustr("Size")) {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.iter().map(|key| key.value + key.envelope.abs()).fold(0.5f32, f32::max),
		_ => 1.0,
	};
	let spacing = (size_estimate(&properties) * 2.5).max(parent_size.map(|size| size.max_element() * 1.5).unwrap_or(0.0)).max(3.0);
	let mut actions = Vec::new();
	for (index, (label, sets)) in variants.iter().enumerate() {
		let at = Vec3::new((index as f32 - (variants.len() as f32 - 1.0) * 0.5) * spacing, 4.0, 0.0);
		let part_turn = if parent_size.is_some() { rotation } else { glam::Mat3A::IDENTITY };
		let anchor = bundle.model.part(&crate::scene::build::PartSpec { name: label.clone(), cframe: Affine3A { matrix3: part_turn, translation: at.into() }, size: parent_size.unwrap_or(Vec3::splat(0.2)), transparency: 1.0, cast_shadow: false, ..Default::default() });
		let holder = match parent_size {
			Some(_) => anchor,
			None => bundle.model.child(anchor, InstanceBuilder::new("Attachment").with_name("At").with_property("CFrame", cframe(Affine3A { matrix3: rotation, translation: glam::Vec3A::ZERO }))),
		};
		let mut builder = InstanceBuilder::new("ParticleEmitter").with_name("Emitter");
		for (name, value) in &properties {
			if name.as_str() != "Name" {
				builder = builder.with_property(name.as_str(), value.clone());
			}
		}
		bundle.model.child(holder, builder.with_property("Enabled", false));
		let target = format!("{ROOT}.{label}");
		let emitter = if parent_size.is_some() { format!("{target}.Emitter") } else { format!("{target}.At.Emitter") };
		// `Enabled=true` switches the copy on at 0 (its Rate runs), the rest are set
		let (switches, sets): (Vec<_>, Vec<_>) = sets.iter().cloned().partition(|(name, _)| name == "Enabled");
		if !sets.is_empty() {
			actions.push(Action { at: 0.0, target: emitter.clone(), set: sets.into_iter().collect(), ..Default::default() });
		}
		if let Some((_, value)) = switches.last() {
			actions.push(Action { at: 0.0, target: emitter, enabled: value.as_bool(), ..Default::default() });
		}
		if count > 0 {
			actions.push(Action { at: 0.0, target, emit: Some(Emit::Count(count)), ..Default::default() });
		}
	}
	actions.push(Action { at: moment, target: ROOT.into(), time_scale: Some(0.0), ..Default::default() });
	let playback = Playback { time: moment + 0.1, actions, ..Default::default() };
	// named by the emitter's last two path segments and the variants' labels
	let labels: Vec<&str> = variants.iter().map(|(label, _)| label.as_str()).collect();
	let name = format!("vfx-emitter-{}-{}{}", slug(&emitter_path.rsplit('.').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("-")), slug(&labels.join("-")), if turned { "-turned" } else { "" });
	let camera = pinned_camera(&name, spec, || {
		let (low, high) = drawn_bounds(&bundle.model.to_rbxm()?, &playback, &[moment + 0.1])?;
		let fov = 40.0;
		let placed = CameraSpec::Orbit { yaw: 0.0, pitch: 0.0, distance: None, padding: 0.06, target: None, fov }.place(Some((low, high)), VIEW.0, VIEW.1);
		Ok(CameraSpec::LookAt { eye: placed.eye.to_array(), target: ((low + high) * 0.5).to_array(), fov })
	})?;
	if !sky {
		bundle.model.part(&crate::scene::build::PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 4.0, 150.0)), size: Vec3::new(2048.0, 2048.0, 1.0), color: [0, 0, 0], ..Default::default() });
	}
	let run = format!("t{moment}");
	register(&name, &run, Timeline { playback, duration: moment + 0.35 });
	let environment = if sky { Environment::default() } else { super::dark() };
	Ok(Experiment { name, model: bundle.model, probes: Vec::new(), runs: vec![Run { label: run, environment, camera }] })
}


/// `vfx-attack:<bundle file>|auto:<effect path>` plays any effect of the bundle (`auto`): its runs at
/// its moments, Studio's frame against ror's playback at that frame's time.
pub fn vfx_attack(spec: &str) -> Result<Experiment> {
	let (path, full_name) = spec.split_once('|').context("vfx-attack:<bundle file>|<attack>")?;
	if let Some(effect) = full_name.strip_prefix("auto:") {
		let (name, attack) = auto(path, effect)?;
		return Ok(attack_runs(format!("vfx-attack-{name}"), attack));
	}
	anyhow::bail!("vfx-attack:<bundle file>|auto:<effect path>[|...]: {full_name:?} is not an auto run")
}

/// An attack's runs at its moments, each a timeline run: Studio's frame against ror's playback at
/// that frame's time.
fn attack_runs(experiment: String, attack: Attack) -> Experiment {
	let mut runs = Vec::new();
	for moment in &attack.moments {
		let label = format!("t{moment}");
		runs.push(Run { label: label.clone(), environment: Environment::default(), camera: attack.camera.clone() });
		let playback = Playback { time: *moment, ..attack.playback.clone() };
		register(&experiment, &label, Timeline { playback, duration: moment + 0.3 });
	}
	Experiment { name: experiment, model: attack.model, probes: Vec::new(), runs }
}
