//! Authoring lints: what an effect does that reads wrong in a game whatever the renderer makes of
//! it, read from the scenes a render or a video draws and reported in their summaries.
//!
//! [`Motion`] flags ParticleEmitters whose particles fly with neither Drag nor Acceleration: at a
//! constant velocity, as if weightless, a defect a reviewer spots first. Hand-made effects seldom do
//! it: of the 57,141 emitters faster than 8 studs/s in one game's attack effects, 4.7 % have neither
//! (counted with the same thresholds).
//!
//! [`Effects`] gathers what a video's effects are made of, for the facts a VFX review checks: fades
//! that are plain linear ramps ([`Fades`]), emitters of one form repeating a texture or mixing
//! blends ([`Restraint`]), clouds that stop dead and hang ([`Frozen`]), images Roblox refuses to
//! everyone ([`Refused`]), beams drawn static and straight ([`Rigid`]), and each effect's emitters,
//! textures, beams and trails ([`Counts`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rbx_dom_weak::Instance;
use rbx_dom_weak::types::Ref;
use serde::Serialize;

use crate::scene::load::Document;
use crate::scene::playback::Tween;
use crate::scene::{Beam, ParticleEmitter, Playback, Scene, props};

/// Particles faster than this (studs a second, Speed's larger end) fly; slower ones (auras, rings,
/// ground marks) may drift at a constant speed.
pub const FLYING_SPEED: f32 = 8.0;

/// An Acceleration axis under this (studs a second²) is none.
pub const NO_ACCELERATION: f32 = 0.5;

/// The most weightless emitters a summary names.
const NAMED: usize = 8;

/// The flying ParticleEmitters with Drag 0 and every Acceleration axis under 0.5: a summary's
/// `weightless`. A negative Drag is not weightless: Roblox grows the speed exponentially with it.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Weightless {
	/// Weightless emitters.
	pub count: usize,
	/// Emitters that emit in what is drawn (a render's moment, a video's frames) and fly.
	pub flying: usize,
	/// `count / flying`, to four places (0 when none flies).
	pub share: f64,
	/// The weightless by path, up to eight paths, those that put out the most particles first.
	pub emitters: Vec<WeightlessEmitter>,
	/// The summary's line (`ror render` and `ror video` print it to stderr), when any is weightless.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub line: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WeightlessEmitter {
	/// Dotted from the file's root.
	pub path: String,
	/// The weightless emitters at the path (an effect's copies of one emitter).
	pub copies: usize,
	/// Speed's larger end in studs a second, to two places, as a playback's sets and tweens leave it
	/// (the largest of the copies, and over a video of its frames while they flew weightless).
	pub speed: f64,
	/// Particles they emitted by the moment drawn (over a video, the most by any frame), or without a
	/// playback those their steady state holds (Rate times mean Lifetime).
	pub particles: usize,
}

/// What the drawn scenes' ParticleEmitters emit, each by its path and its place among emitters of
/// that path (the legacy effects' systems aside).
#[derive(Default)]
pub struct Motion {
	seen: HashMap<(String, usize), Seen>,
}

#[derive(Default)]
struct Seen {
	flying: bool,
	weightless: bool,
	speed: f32,
	particles: usize,
}

impl Motion {
	/// Adds one drawn scene: a render's, or each of a video's frames. Emitters that emit nothing in it
	/// (disabled and never emitted, or not yet) do not count.
	pub fn add(&mut self, scene: &Scene) {
		let mut places: HashMap<&str, usize> = HashMap::new();
		for emitter in scene.particles.iter().filter(|emitter| emitter.mount.is_none()) {
			let place = places.entry(emitter.path.as_str()).or_default();
			let key = (emitter.path.clone(), *place);
			*place += 1;
			let particles = emitted(emitter);
			if particles == 0 {
				continue;
			}
			let seen = self.seen.entry(key).or_default();
			seen.particles = seen.particles.max(particles);
			let speed = emitter.speed.0.abs().max(emitter.speed.1.abs());
			if speed > FLYING_SPEED {
				seen.flying = true;
				if emitter.drag == 0.0 && emitter.acceleration.abs().max_element() < NO_ACCELERATION {
					seen.weightless = true;
					seen.speed = seen.speed.max(speed);
				}
			}
		}
	}

	/// The weightless emitters of every scene added.
	pub fn weightless(&self) -> Weightless {
		let flying = self.seen.values().filter(|seen| seen.flying).count();
		let mut paths: HashMap<&str, WeightlessEmitter> = HashMap::new();
		for ((path, _), seen) in self.seen.iter().filter(|(_, seen)| seen.weightless) {
			let entry = paths.entry(path.as_str()).or_insert_with(|| WeightlessEmitter { path: path.clone(), ..Default::default() });
			entry.copies += 1;
			entry.speed = entry.speed.max((f64::from(seen.speed) * 100.0).round() / 100.0);
			entry.particles += seen.particles;
		}
		let count = paths.values().map(|entry| entry.copies).sum();
		let mut emitters: Vec<WeightlessEmitter> = paths.into_values().collect();
		emitters.sort_by(|a, b| b.particles.cmp(&a.particles).then(b.speed.total_cmp(&a.speed)).then_with(|| a.path.cmp(&b.path)));
		emitters.truncate(NAMED);
		let share = if flying > 0 { (count as f64 / flying as f64 * 10_000.0).round() / 10_000.0 } else { 0.0 };
		let line = (count > 0).then(|| line(count, flying, &emitters));
		Weightless { count, flying, share, emitters, line }
	}
}

/// Particles an emitter puts out in what is drawn: a playback's emitted by the moment drawn, else
/// those of its steady state (enabled with a Rate) and its calibration bursts.
fn emitted(emitter: &ParticleEmitter) -> usize {
	if let Some(births) = &emitter.births {
		return births.len();
	}
	let bursts: usize = emitter.bursts.iter().map(|&(_, count)| count as usize).sum();
	let steady = if emitter.enabled && emitter.rate > 0.0 { (emitter.rate * (emitter.lifetime.0 + emitter.lifetime.1) * 0.5).ceil().max(1.0) as usize } else { 0 };
	bursts + steady
}

/// "motion: 3 of 21 flying emitters are weightless (no Drag or Acceleration): A.Sparks x2 (40 studs/s), ..."
fn line(count: usize, flying: usize, named: &[WeightlessEmitter]) -> String {
	let studs = |speed: f64| {
		let text = format!("{speed:.1}");
		text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
	};
	let mut paths: Vec<String> = named.iter().map(|entry| format!("{}{} ({} studs/s)", entry.path, if entry.copies > 1 { format!(" x{}", entry.copies) } else { String::new() }, studs(entry.speed))).collect();
	let unnamed = count - named.iter().map(|entry| entry.copies).sum::<usize>();
	if unnamed > 0 {
		paths.push(format!("and {unnamed} more"));
	}
	let (noun, verb) = if flying == 1 { ("emitter", "is") } else if count == 1 { ("emitters", "is") } else { ("emitters", "are") };
	format!("motion: {count} of {flying} flying {noun} {verb} weightless (no Drag or Acceleration): {}", paths.join(", "))
}

/// A ramp's keys may stray from the straight line between its ends by this share of its rise and
/// still be linear.
pub const LINEAR_TOLERANCE: f32 = 0.02;

/// A tween's stretch shorter than this (seconds) is a cut, not a fade: an eased curve recorded every
/// frame (a sim's recording) turns in stretches of one or two frames, each a straight line.
pub const SHORTEST_FADE: f32 = 0.1;

/// Drag at or above this stops particles dead within a fraction of a second, and those living
/// [`FROZEN_LIFETIME`] seconds or more (Lifetime's larger end) then hang where they stopped.
pub const FROZEN_DRAG: f32 = 15.0;
pub const FROZEN_LIFETIME: f32 = 1.0;

/// LightEmission values of one form further apart than this (or of both signs) blend unlike each
/// other.
pub const BLEND_SPREAD: f32 = 1.0;

/// The most instances a summary line names.
const LINE_NAMES: usize = 4;

/// What a summary's `lint` holds besides `weightless`: the clip's fades, restraint, frozen clouds and
/// refused images.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lint {
	pub fades: Fades,
	pub restraint: Restraint,
	pub frozen: Vec<Frozen>,
	pub refused: Vec<Refused>,
	pub rigid: Rigid,
}

/// Beams drawn rigidly: TextureSpeed 0 and CurveSize0 and CurveSize1 0 in every frame they draw, a
/// static straight streak. One game's hand-made attack beams scroll (91 % of 6,104 have TextureSpeed
/// other than 0) or curve (57 %); 9 % do neither. Trails are not counted: they move with their
/// attachments.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Rigid {
	/// Rigid beams (each copy of a path once).
	pub count: usize,
	/// Beams that draw in some frame (some Transparency under 1 and a width), the share's whole.
	pub beams: usize,
	pub entries: Vec<RigidBeam>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RigidBeam {
	/// Dotted from the file's root.
	pub path: String,
	/// The rigid beams at the path (an effect's copies of one beam).
	pub copies: usize,
	/// TextureSpeed and whether a CurveSize is set, as drawn (0 and false: what makes it rigid).
	pub texture_speed: f64,
	pub curved: bool,
}

/// Fades that are a plain linear ramp: an emitter's, trail's or beam's Transparency (a trail's
/// WidthScale) of two keypoints or several on one line, without envelopes, not constant; a playback's
/// tweens of Transparency, Width0 / Width1, ImageTransparency or Size of Beams, Trails, Decals and
/// GUI images whose keys are linear in time ([`tween_fades`]).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Fades {
	pub count: usize,
	pub entries: Vec<Fade>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fade {
	/// Dotted from the file's root.
	pub path: String,
	pub property: String,
	pub kind: FadeKind,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum FadeKind {
	/// A NumberSequence over each particle's or point's life.
	Sequence,
	/// A playback's tween over the clip.
	Tween,
}

/// Within one form (the emitters of one parent Attachment or Part): one texture on several
/// emitters, and LightEmissions that blend unlike each other.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Restraint {
	pub duplicate_textures: Vec<DuplicateTexture>,
	pub mixed_blends: Vec<MixedBlend>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DuplicateTexture {
	/// The form: its parent's path (twice, as `parent` and as `path`, which every lint entry has).
	pub parent: String,
	pub path: String,
	/// The texture (`rbxassetid://<id>` for asset ids).
	pub texture: String,
	/// Each emitter using it (a copy of one path once per copy).
	pub emitters: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MixedBlend {
	/// As [`DuplicateTexture`]'s.
	pub parent: String,
	pub path: String,
	/// The form's LightEmission values, lowest first, to three places.
	pub light_emissions: Vec<f64>,
	pub emitters: Vec<String>,
}

/// An emitter whose particles stop dead and hang: Drag 15 or more, living a second or more.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Frozen {
	pub path: String,
	/// The largest Drag and Lifetime's larger end it emitted with, to two places.
	pub drag: f64,
	pub lifetime: f64,
}

/// An image Roblox refuses to every account and place (Studio draws nothing for it either), and the
/// instances drawing it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Refused {
	/// The first instance drawing it ("" when none drawn could be named), as every lint entry has a
	/// path; all of them in `paths`.
	pub path: String,
	/// `rbxassetid://<id>` for asset ids.
	pub asset: String,
	/// `moderated`, `archived` or `unreviewed`.
	pub reason: String,
	pub paths: Vec<String>,
}

/// A summary's `counts`.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Counts {
	pub effects: Vec<EffectCount>,
}

/// One effect over the clip, each instance counted once (the video's census): its emitters (the
/// legacy effects' systems among them), their distinct textures, its beams and trails.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EffectCount {
	pub root: String,
	pub emitters: usize,
	pub textures: usize,
	pub beams: usize,
	pub trails: usize,
}

/// Where a clip's effects hang: each child of the loaded root (a model's top-level instance, a
/// place's Workspace, the selected instance) is one effect, and so is each child of a `World` in it
/// (a sim recording's world).
#[derive(Clone, Debug, Default)]
pub struct EffectRoots {
	/// Each holder's lineage (`Document::lineage`: its path and its ancestors'), the deepest first.
	holders: Vec<Vec<String>>,
}

impl EffectRoots {
	pub fn of(document: &Document) -> Self {
		let mut holders: Vec<Ref> = Vec::new();
		for &root in &document.roots {
			// a map drawn around the model (`--map`) holds none of its effects
			if document.dom.get_by_ref(root).is_some_and(|instance| instance.parent() == document.dom.root_ref() && document.setting.as_deref() == Some(instance.name.as_str())) {
				continue;
			}
			let holder = if document.is_place { document.dom.get_by_ref(root).map_or(root, Instance::parent) } else { root };
			if !holders.contains(&holder) {
				holders.push(holder);
			}
		}
		let mut lineages = Vec::new();
		for holder in holders {
			let Some(instance) = document.dom.get_by_ref(holder) else {
				continue;
			};
			if let Some(&world) = instance.children().iter().find(|&&child| document.dom.get_by_ref(child).is_some_and(|child| child.name == "World")) {
				lineages.push(document.lineage(world));
			}
			lineages.push(document.lineage(holder));
		}
		lineages.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
		lineages.dedup();
		Self { holders: lineages }
	}

	/// The effect an instance is in, from its lineage (its path last): the child of its holder it is
	/// at or under (outside every holder, the second of its lineage, its first two names).
	pub fn root_of(&self, lineage: &[String]) -> String {
		for holder in &self.holders {
			if lineage.len() > holder.len() && lineage.starts_with(holder) {
				return lineage[holder.len()].clone();
			}
		}
		lineage.get(1).or(lineage.first()).cloned().unwrap_or_default()
	}

	/// The effect `path` is in, reading the path back (for a path whose lineage is not known).
	pub fn root(&self, path: &str) -> String {
		match path_lineage(path) {
			Some(lineage) => self.root_of(&lineage),
			None => path.to_string(),
		}
	}
}

/// A path's lineage read back from the path ([`crate::scene::load::split_path`]): what `Document::lineage`
/// gives for the instance at it, when its paths are written alike.
fn path_lineage(path: &str) -> Option<Vec<String>> {
	let segments = crate::scene::load::split_path(path).ok()?;
	let mut joined = String::new();
	Some(segments.iter().map(|name| crate::scene::load::push_segment(&mut joined, name).clone()).collect())
}

/// An image's URI as one spelling: `rbxassetid://<id>` for every way of writing an asset id.
fn canonical(uri: &str) -> String {
	ror_assets::ContentId::parse(uri).map(|id| id.canonical()).unwrap_or_else(|_| uri.trim().to_string())
}

/// What a clip's effects are made of, from the scenes it draws (each of a video's frames): every
/// ParticleEmitter, Beam and Trail once, known by its path and its place among those of that path (as
/// [`Motion`] knows emitters), and which instances draw each image.
#[derive(Default)]
pub struct Effects {
	emitters: HashMap<String, Vec<EmitterSeen>>,
	beams: HashMap<String, Vec<BeamSeen>>,
	trails: HashMap<String, Vec<TrailSeen>>,
	images: HashMap<String, BTreeSet<String>>,
	/// Each path's lineage as the scenes give it (`Scene::lineage`): what holds it, by the instances.
	lineage: HashMap<String, Vec<String>>,
}

#[derive(Clone, Default)]
struct EmitterSeen {
	/// A legacy effect's system (Fire, Smoke, Sparkles, an Explosion).
	legacy: bool,
	/// Its texture (first seen emitting, else first seen) and LightEmission alike.
	texture: Option<String>,
	light_emission: f32,
	/// Emitted in some frame; then the largest Drag and Lifetime it emitted with, and whether its
	/// Transparency was a linear ramp then.
	emits: bool,
	drag: f32,
	lifetime: f32,
	linear: bool,
}

#[derive(Clone, Default)]
struct BeamSeen {
	/// Its Transparency was a linear ramp in some frame.
	linear: bool,
	/// Drew in some frame ([`shows`]); then whether it scrolled or curved in any frame it drew, and
	/// its TextureSpeed and curve when it first drew.
	drawn: bool,
	moves: bool,
	texture_speed: f32,
	curved: bool,
}

/// A beam that shows in a frame: some Transparency under 1 and a width at either end.
fn shows(beam: &Beam) -> bool {
	(beam.width0 != 0.0 || beam.width1 != 0.0) && beam.transparency.iter().any(|&(_, value, _)| value < 1.0)
}

#[derive(Clone, Default)]
struct TrailSeen {
	/// Drew a ribbon (two points or more) in some frame; then whether its Transparency and WidthScale
	/// were linear ramps.
	drawn: bool,
	transparency: bool,
	width: bool,
}

/// The entry of the next instance of `path` in a frame (its place among those of that path), and
/// whether it is seen for the first time.
fn place_of<'a, 's, T: Default + Clone>(map: &'a mut HashMap<String, Vec<T>>, places: &mut HashMap<&'s str, usize>, path: &'s str) -> (&'a mut T, bool) {
	let next = places.entry(path).or_default();
	let place = *next;
	*next += 1;
	if !map.contains_key(path) {
		map.insert(path.to_owned(), Vec::new());
	}
	let seen = map.get_mut(path).expect("inserted above");
	let new = seen.len() <= place;
	if new {
		seen.resize(place + 1, T::default());
	}
	(&mut seen[place], new)
}

/// Notes that `path` draws the image `uri`.
fn note(images: &mut HashMap<String, BTreeSet<String>>, uri: &str, path: &str) {
	match images.get_mut(uri) {
		Some(paths) => {
			if !paths.contains(path) {
				paths.insert(path.to_owned());
			}
		}
		None => {
			images.insert(uri.to_owned(), BTreeSet::from([path.to_owned()]));
		}
	}
}

impl Effects {
	/// Adds one drawn scene: a render's, or each of a video's frames.
	pub fn add(&mut self, scene: &Scene) {
		for (path, lineage) in &scene.lineage {
			if !self.lineage.contains_key(path) {
				self.lineage.insert(path.clone(), lineage.clone());
			}
		}
		let mut places = HashMap::new();
		for emitter in &scene.particles {
			if let Some(texture) = &emitter.texture {
				note(&mut self.images, texture, &emitter.path);
			}
			let (seen, new) = place_of(&mut self.emitters, &mut places, &emitter.path);
			if new {
				*seen = EmitterSeen { legacy: emitter.mount.is_some(), texture: emitter.texture.clone(), light_emission: emitter.light_emission, ..Default::default() };
			}
			if seen.legacy || emitted(emitter) == 0 {
				continue;
			}
			if !seen.emits {
				seen.emits = true;
				seen.texture = emitter.texture.clone();
				seen.light_emission = emitter.light_emission;
			}
			seen.drag = seen.drag.max(emitter.drag);
			seen.lifetime = seen.lifetime.max(emitter.lifetime.0.max(emitter.lifetime.1));
			seen.linear |= linear_sequence(&emitter.transparency);
		}
		let mut places = HashMap::new();
		for beam in &scene.beams {
			if let Some(texture) = &beam.texture {
				note(&mut self.images, texture, &beam.path);
			}
			let (seen, _) = place_of(&mut self.beams, &mut places, &beam.path);
			seen.linear |= linear_sequence(&beam.transparency);
			if shows(beam) {
				let curved = beam.curve_size0 != 0.0 || beam.curve_size1 != 0.0;
				if !seen.drawn {
					seen.drawn = true;
					seen.texture_speed = beam.texture_speed;
					seen.curved = curved;
				}
				seen.moves |= beam.texture_speed != 0.0 || curved;
			}
		}
		let mut places = HashMap::new();
		for trail in &scene.trails {
			if let Some(texture) = &trail.texture {
				note(&mut self.images, texture, &trail.path);
			}
			let (seen, _) = place_of(&mut self.trails, &mut places, &trail.path);
			if trail.points.len() >= 2 {
				seen.drawn = true;
				seen.transparency |= linear_sequence(&trail.transparency);
				seen.width |= linear_sequence(&trail.width_scale);
			}
		}
		for part in &scene.parts {
			let appearance = part.surface_appearance.iter().flat_map(|appearance| [&appearance.color_map, &appearance.normal_map, &appearance.metalness_map, &appearance.roughness_map, &appearance.emissive_mask]);
			for uri in part.texture.iter().chain(appearance.flatten()).chain(part.decals.iter().map(|decal| &decal.texture)) {
				note(&mut self.images, uri, &part.path);
			}
		}
	}

	/// The lineage of `path`: as the scenes gave it, else read back from the path.
	fn lineage_of(&self, path: &str) -> Vec<String> {
		match self.lineage.get(path) {
			Some(lineage) => lineage.clone(),
			None => path_lineage(path).unwrap_or_default(),
		}
	}

	/// The path of what holds `path` ("" at the top).
	fn parent_of(&self, path: &str) -> String {
		let mut lineage = self.lineage_of(path);
		lineage.pop();
		lineage.pop().unwrap_or_default()
	}

	/// The effect `path` is in.
	fn root_of(&self, roots: &EffectRoots, path: &str) -> String {
		match self.lineage.get(path) {
			Some(lineage) => roots.root_of(lineage),
			None => roots.root(path),
		}
	}

	/// Every emitter by (path, place), in order.
	fn each_emitter(&self) -> impl Iterator<Item = (&str, &EmitterSeen)> {
		let mut paths: Vec<&String> = self.emitters.keys().collect();
		paths.sort();
		paths.into_iter().flat_map(|path| self.emitters[path].iter().map(move |seen| (path.as_str(), seen)))
	}

	/// The lints over every scene added: with `tweens`, a playback's linear fades ([`tween_fades`]),
	/// and the images the renderer reported missing (`stats.missing`), those Roblox refuses to
	/// everyone named with the instances drawing them.
	pub fn lint(&self, tweens: &[Fade], missing: &[String]) -> Lint {
		// emitters that emitted, the legacy effects aside
		let emitting: Vec<(&str, &EmitterSeen)> = self.each_emitter().filter(|(_, seen)| seen.emits && !seen.legacy).collect();

		let mut fades: BTreeSet<Fade> = tweens.iter().cloned().collect();
		let sequence = |path: &str, property: &str| Fade { path: path.to_string(), property: property.to_string(), kind: FadeKind::Sequence };
		fades.extend(emitting.iter().filter(|(_, seen)| seen.linear).map(|(path, _)| sequence(path, "Transparency")));
		for (path, beams) in &self.beams {
			if beams.iter().any(|seen| seen.linear) {
				fades.insert(sequence(path, "Transparency"));
			}
		}
		for (path, trails) in &self.trails {
			if trails.iter().any(|seen| seen.drawn && seen.transparency) {
				fades.insert(sequence(path, "Transparency"));
			}
			if trails.iter().any(|seen| seen.drawn && seen.width) {
				fades.insert(sequence(path, "WidthScale"));
			}
		}
		let entries: Vec<Fade> = fades.into_iter().collect();

		let mut forms: BTreeMap<String, Vec<(&str, &EmitterSeen)>> = BTreeMap::new();
		for &(path, seen) in &emitting {
			forms.entry(self.parent_of(path)).or_default().push((path, seen));
		}
		let mut restraint = Restraint::default();
		for (parent, emitters) in forms.iter().filter(|(_, emitters)| emitters.len() >= 2) {
			let mut textures: BTreeMap<String, Vec<String>> = BTreeMap::new();
			for (path, seen) in emitters {
				if let Some(texture) = &seen.texture {
					textures.entry(canonical(texture)).or_default().push(path.to_string());
				}
			}
			restraint.duplicate_textures.extend(textures.into_iter().filter(|(_, emitters)| emitters.len() >= 2).map(|(texture, emitters)| DuplicateTexture { parent: parent.clone(), path: parent.clone(), texture, emitters }));
			let (low, high) = emitters.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), (_, seen)| (low.min(seen.light_emission), high.max(seen.light_emission)));
			if high - low > BLEND_SPREAD || (high > 0.0 && low < 0.0) {
				let mut values: Vec<f64> = emitters.iter().map(|(_, seen)| round(seen.light_emission, 1000.0)).collect();
				values.sort_by(f64::total_cmp);
				values.dedup();
				restraint.mixed_blends.push(MixedBlend { parent: parent.clone(), path: parent.clone(), light_emissions: values, emitters: emitters.iter().map(|(path, _)| path.to_string()).collect() });
			}
		}

		let mut frozen: BTreeMap<&str, (f32, f32)> = BTreeMap::new();
		for &(path, seen) in emitting.iter().filter(|(_, seen)| seen.drag >= FROZEN_DRAG && seen.lifetime >= FROZEN_LIFETIME) {
			let most = frozen.entry(path).or_default();
			*most = (most.0.max(seen.drag), most.1.max(seen.lifetime));
		}
		let frozen = frozen.into_iter().map(|(path, (drag, lifetime))| Frozen { path: path.to_string(), drag: round(drag, 100.0), lifetime: round(lifetime, 100.0) }).collect();

		Lint { fades: Fades { count: entries.len(), entries }, restraint, frozen, refused: self.refused(missing), rigid: self.rigid() }
	}

	/// The beams that drew rigidly over every scene added, and how many drew.
	fn rigid(&self) -> Rigid {
		let mut rigid = Rigid::default();
		let mut paths: BTreeMap<&str, RigidBeam> = BTreeMap::new();
		for (path, beams) in &self.beams {
			for seen in beams.iter().filter(|seen| seen.drawn) {
				rigid.beams += 1;
				if seen.moves {
					continue;
				}
				rigid.count += 1;
				paths.entry(path).or_insert_with(|| RigidBeam { path: path.clone(), copies: 0, texture_speed: round(seen.texture_speed, 1000.0), curved: seen.curved }).copies += 1;
			}
		}
		rigid.entries = paths.into_values().collect();
		rigid
	}

	/// The images among `missing` that Roblox refuses to everyone, with the instances drawing them.
	fn refused(&self, missing: &[String]) -> Vec<Refused> {
		let refused: BTreeMap<String, &'static str> = missing.iter().filter_map(|message| refusal(message)).collect();
		if refused.is_empty() {
			return Vec::new();
		}
		let mut users: HashMap<String, BTreeSet<&str>> = HashMap::new();
		for (uri, paths) in &self.images {
			users.entry(canonical(uri)).or_default().extend(paths.iter().map(String::as_str));
		}
		refused
			.into_iter()
			.map(|(asset, reason)| {
				let paths: Vec<String> = users.get(&asset).map(|paths| paths.iter().map(|path| path.to_string()).collect()).unwrap_or_default();
				Refused { path: paths.first().cloned().unwrap_or_default(), asset, reason: reason.to_string(), paths }
			})
			.collect()
	}

	/// Each effect's emitters, textures, beams and trails, every instance seen counted once.
	pub fn counts(&self, roots: &EffectRoots) -> Counts {
		#[derive(Default)]
		struct Tally {
			emitters: usize,
			textures: BTreeSet<String>,
			beams: usize,
			trails: usize,
		}
		let mut effects: BTreeMap<String, Tally> = BTreeMap::new();
		for (path, seen) in self.each_emitter() {
			let tally = effects.entry(self.root_of(roots, path)).or_default();
			tally.emitters += 1;
			tally.textures.extend(seen.texture.as_deref().map(canonical));
		}
		for (path, beams) in &self.beams {
			effects.entry(self.root_of(roots, path)).or_default().beams += beams.len();
		}
		for (path, trails) in &self.trails {
			effects.entry(self.root_of(roots, path)).or_default().trails += trails.len();
		}
		Counts { effects: effects.into_iter().map(|(root, tally)| EffectCount { root, emitters: tally.emitters, textures: tally.textures.len(), beams: tally.beams, trails: tally.trails }).collect() }
	}
}

/// "1 beam", "2 beams"
fn counted(count: usize, noun: &str) -> String {
	format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn round(value: f32, scale: f64) -> f64 {
	(f64::from(value) * scale).round() / scale
}

/// An asset Roblox refuses to everyone in a missing asset's message, and why: the renderer writes
/// ror_assets' `<asset>: HTTP <status>, <reason>` (AssetError::Http) inside its own words.
fn refusal(message: &str) -> Option<(String, &'static str)> {
	use ror_assets::RefusalReason;
	for (reason, label) in [(RefusalReason::Moderated, "moderated"), (RefusalReason::Archived, "archived"), (RefusalReason::Unreviewed, "unreviewed")] {
		let Some(end) = message.find(&format!(", {reason}")) else {
			continue;
		};
		let Some((before, status)) = message[..end].rsplit_once(": HTTP ") else {
			continue;
		};
		if status.is_empty() || !status.bytes().all(|byte| byte.is_ascii_digit()) {
			continue;
		}
		let asset = before.rsplit_once(": ").map_or(before, |(_, asset)| asset);
		return Some((canonical(asset), label));
	}
	None
}

/// A NumberSequence (time, value, envelope) that is a plain linear ramp: two keypoints or more on one
/// line (within 2 % of its range), no envelope, not constant.
pub fn linear_sequence(keys: &[(f32, f32, f32)]) -> bool {
	let (Some(&(t0, v0, _)), Some(&(t1, v1, _))) = (keys.first(), keys.last()) else {
		return false;
	};
	if keys.len() < 2 || t1 <= t0 || keys.iter().any(|key| key.2.abs() > 1e-6) {
		return false;
	}
	let (low, high) = keys.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), key| (low.min(key.1), high.max(key.1)));
	let range = high - low;
	range > 1e-4 && keys.iter().all(|&(t, v, _)| (v - (v0 + (v1 - v0) * (t - t0) / (t1 - t0))).abs() <= LINEAR_TOLERANCE * range)
}

/// What a fade may tween on an instance: Transparency of a Beam, Trail or Decal (a Texture), Width0
/// and Width1 of a Beam, ImageTransparency and Size of an ImageLabel or ImageButton.
fn fades_by(instance: &Instance, property: &str) -> bool {
	match property {
		"Transparency" => ["Beam", "Trail", "Decal"].iter().any(|class| props::is_a(instance, class)),
		"Width0" | "Width1" => props::is_a(instance, "Beam"),
		"ImageTransparency" | "Size" => props::is_a(instance, "ImageLabel") || props::is_a(instance, "ImageButton"),
		_ => false,
	}
}

/// A playback's tweens that fade linearly ([`Fades`]): a tween with keys of which a stretch moving
/// one way, at least [`SHORTEST_FADE`] long, is eased Linear and keeps within 2 % of the straight
/// line between its ends (holds before and after aside), or a TweenService tween eased Linear; each
/// instance it tweens once, by its path in `document` as loaded.
pub fn tween_fades(document: &Document, playback: &Playback) -> Vec<Fade> {
	let mut fades = BTreeSet::new();
	for tween in playback.tweens.iter().filter(|tween| linear_tween(tween)) {
		let Ok(targets) = crate::scene::playback::resolve(document, &tween.target) else {
			continue;
		};
		let instances: Vec<Ref> = match &tween.class {
			None => targets,
			Some(class) => targets.into_iter().flat_map(|target| descendants(document, target)).filter(|&referent| document.dom.get_by_ref(referent).is_some_and(|instance| props::is_a(instance, class))).collect(),
		};
		for referent in instances {
			if document.dom.get_by_ref(referent).is_some_and(|instance| fades_by(instance, &tween.property)) {
				fades.insert(Fade { path: document.path_of(referent), property: tween.property.clone(), kind: FadeKind::Tween });
			}
		}
	}
	fades.into_iter().collect()
}

fn descendants(document: &Document, referent: Ref) -> Vec<Ref> {
	let mut list = Vec::new();
	let mut stack = vec![referent];
	while let Some(current) = stack.pop() {
		list.push(current);
		if let Some(instance) = document.dom.get_by_ref(current) {
			stack.extend(instance.children().iter().copied());
		}
	}
	list
}

/// Whether a tween fades linearly ([`tween_fades`]), whatever it tweens.
fn linear_tween(tween: &Tween) -> bool {
	if tween.keys.is_empty() {
		return tween.to.is_some() && tween.easing.as_deref() == Some("Linear") && tween.duration >= SHORTEST_FADE;
	}
	let mut keys: Vec<&crate::scene::playback::TweenKey> = tween.keys.iter().collect();
	keys.sort_by(|a, b| a.at.total_cmp(&b.at));
	let Some(values) = keys.iter().map(|key| scalars(&key.value)).collect::<Option<Vec<Vec<f32>>>>() else {
		return false;
	};
	let width = values[0].len();
	if width == 0 || values.iter().any(|value| value.len() != width) {
		return false;
	}
	let spread = |component: usize| {
		let (low, high) = values.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), value| (low.min(value[component]), high.max(value[component])));
		high - low
	};
	// the component that moves the most (a UDim2's scale, a vector's longest axis)
	let component = (0..width).max_by(|&a, &b| spread(a).total_cmp(&spread(b))).unwrap_or(0);
	let range = spread(component);
	if range <= 1e-6 {
		return false;
	}
	let points: Vec<(f32, f32)> = keys.iter().zip(&values).map(|(key, value)| (key.at, value[component])).collect();
	let eased_linear: Vec<bool> = keys.iter().map(|key| key.easing.as_deref().is_none_or(|easing| easing == "Linear")).collect();
	let direction = |index: usize| {
		let rise = points[index + 1].1 - points[index].1;
		if rise.abs() <= range * 1e-4 { 0 } else { rise.signum() as i32 }
	};
	let mut start = 0;
	while start + 1 < points.len() {
		let way = direction(start);
		if way == 0 {
			start += 1;
			continue;
		}
		let mut end = start + 1;
		while end + 1 < points.len() && direction(end) == way {
			end += 1;
		}
		if linear_stretch(&points[start..=end], &eased_linear[start + 1..=end]) {
			return true;
		}
		start = end;
	}
	false
}

/// A stretch of keys moving one way is a linear fade: long enough, every segment eased Linear, every
/// key within 2 % of the rise from the straight line between its ends.
fn linear_stretch(points: &[(f32, f32)], eased_linear: &[bool]) -> bool {
	let ((t0, v0), (t1, v1)) = (points[0], points[points.len() - 1]);
	let rise = v1 - v0;
	t1 - t0 >= SHORTEST_FADE && eased_linear.iter().all(|&linear| linear) && points.iter().all(|&(t, v)| (v - (v0 + rise * (t - t0) / (t1 - t0))).abs() <= LINEAR_TOLERANCE * rise.abs())
}

/// A tween key's value as numbers: a number, a vector or colour, a UDim2's four, or a NumberSequence
/// holding one value throughout; None for anything else.
fn scalars(value: &serde_json::Value) -> Option<Vec<f32>> {
	use serde_json::Value;
	match value {
		Value::Number(number) => Some(vec![number.as_f64()? as f32]),
		Value::Array(items) if items.iter().all(Value::is_number) => items.iter().map(|item| item.as_f64().map(|value| value as f32)).collect(),
		Value::Array(items) => {
			let values: Vec<f32> = items.iter().map(|key| key.get(1).and_then(Value::as_f64).map(|value| value as f32)).collect::<Option<_>>()?;
			let first = *values.first()?;
			values.iter().all(|value| (value - first).abs() <= 1e-6).then(|| vec![first])
		}
		_ => None,
	}
}

impl Lint {
	/// The lines a summary prints for its lints (none for a clean clip).
	pub fn lines(&self) -> Vec<String> {
		let mut lines = Vec::new();
		let named = |names: Vec<String>| {
			let count = names.len();
			let mut shown: Vec<String> = names.into_iter().take(LINE_NAMES).collect();
			if count > LINE_NAMES {
				shown.push(format!("and {} more", count - LINE_NAMES));
			}
			shown.join(", ")
		};
		if self.fades.count > 0 {
			let names = self.fades.entries.iter().map(|fade| format!("{} {} ({})", fade.path, fade.property, if fade.kind == FadeKind::Sequence { "sequence" } else { "tween" })).collect();
			lines.push(format!("fades: {} linear: {}", self.fades.count, named(names)));
		}
		let Restraint { duplicate_textures, mixed_blends } = &self.restraint;
		if !duplicate_textures.is_empty() || !mixed_blends.is_empty() {
			let mut names: Vec<String> = duplicate_textures.iter().map(|entry| format!("{} has {} on {} emitters", entry.parent, entry.texture, entry.emitters.len())).collect();
			names.extend(mixed_blends.iter().map(|entry| format!("{} mixes LightEmission {}", entry.parent, entry.light_emissions.iter().map(|value| value.to_string()).collect::<Vec<_>>().join(" / "))));
			lines.push(format!("restraint: {} and {} in a form: {}", counted(duplicate_textures.len(), "repeated texture"), counted(mixed_blends.len(), "mixed blend"), named(names)));
		}
		if !self.frozen.is_empty() {
			let names = self.frozen.iter().map(|entry| format!("{} (Drag {}, {} s)", entry.path, entry.drag, entry.lifetime)).collect();
			let verbs = if self.frozen.len() == 1 { "stops dead and hangs" } else { "stop dead and hang" };
			lines.push(format!("frozen: {} {verbs} (Drag {FROZEN_DRAG}+, living {FROZEN_LIFETIME} s+): {}", counted(self.frozen.len(), "emitter"), named(names)));
		}
		if !self.refused.is_empty() {
			let names = self.refused.iter().map(|entry| format!("{} ({}, {})", entry.asset, entry.reason, counted(entry.paths.len(), "instance"))).collect();
			lines.push(format!("refused: {} Roblox refuses to everyone, drawn by nothing: {}", counted(self.refused.len(), "image"), named(names)));
		}
		if self.rigid.count > 0 {
			let names = self.rigid.entries.iter().map(|entry| if entry.copies > 1 { format!("{} x{}", entry.path, entry.copies) } else { entry.path.clone() }).collect();
			let verb = if self.rigid.count == 1 { "neither scrolls nor curves" } else { "neither scroll nor curve" };
			lines.push(format!("rigid: {} of {} {verb} (TextureSpeed 0, no CurveSize): {}", self.rigid.count, counted(self.rigid.beams, "beam"), named(names)));
		}
		lines
	}
}

impl Counts {
	/// "effects: Slam.Impact (40 emitters, 12 textures, 2 beams), ..."
	pub fn line(&self) -> Option<String> {
		if self.effects.is_empty() {
			return None;
		}
		let mut names: Vec<String> = self
			.effects
			.iter()
			.take(LINE_NAMES * 2)
			.map(|effect| {
				let mut parts = vec![counted(effect.emitters, "emitter"), counted(effect.textures, "texture")];
				for (count, noun) in [(effect.beams, "beam"), (effect.trails, "trail")] {
					if count > 0 {
						parts.push(counted(count, noun));
					}
				}
				format!("{} ({})", effect.root, parts.join(", "))
			})
			.collect();
		if self.effects.len() > LINE_NAMES * 2 {
			names.push(format!("and {} more", self.effects.len() - LINE_NAMES * 2));
		}
		Some(format!("effects: {}", names.join(", ")))
	}
}

#[cfg(test)]
mod tests {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{NumberRange, Vector3};

	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use crate::scene::playback::{Action, Emit};
	use crate::scene::{Playback, extract};

	/// An emitter under the part: Speed (constant), Drag, Acceleration, Enabled.
	fn emitter(model: &mut ModelBuilder, part: rbx_dom_weak::types::Ref, name: &str, speed: f32, drag: f32, acceleration: [f32; 3], enabled: bool) {
		let builder = InstanceBuilder::new("ParticleEmitter")
			.with_name(name)
			.with_property("Speed", NumberRange::new(speed * 0.5, speed))
			.with_property("Drag", drag)
			.with_property("Acceleration", Vector3::new(acceleration[0], acceleration[1], acceleration[2]))
			.with_property("Enabled", enabled)
			.with_property("Rate", 20.0f32)
			.with_property("Lifetime", NumberRange::new(1.0, 1.0));
		model.child(part, builder);
	}

	fn drawn(model: ModelBuilder, playback: Option<&Playback>) -> Scene {
		let mut document = model.into_document();
		let Some(playback) = playback else {
			return extract::extract(&document);
		};
		let resolved = playback.resolve(&document).unwrap();
		resolved.apply_state(&mut document).unwrap();
		let mut scene = extract::extract(&document);
		resolved.apply_history(&document, &mut scene);
		scene
	}

	fn effect() -> ModelBuilder {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		emitter(&mut model, part, "Sparks", 40.0, 0.0, [0.0, 0.0, 0.0], true);
		emitter(&mut model, part, "Dragged", 30.0, 6.0, [0.0, 0.0, 0.0], true);
		emitter(&mut model, part, "Falling", 25.0, 0.0, [0.0, -40.0, 0.0], true);
		emitter(&mut model, part, "Drift", 4.0, 0.0, [0.0, 0.0, 0.0], true);
		emitter(&mut model, part, "Dormant", 50.0, 0.0, [0.0, 0.2, 0.0], false);
		model.child(part, InstanceBuilder::new("Sparkles"));
		model
	}

	/// Emitters that fly with neither Drag nor Acceleration are weightless; slow, dragged, falling and
	/// dormant ones and the legacy effects are not
	#[test]
	fn flying_emitters_without_drag_or_acceleration_are_weightless() {
		let mut motion = Motion::default();
		motion.add(&drawn(effect(), None));
		let report = motion.weightless();
		assert_eq!((report.count, report.flying, report.share), (1, 3, 0.3333));
		assert_eq!(report.emitters, vec![WeightlessEmitter { path: "Effect.Core.Sparks".into(), copies: 1, speed: 40.0, particles: 20 }]);
		assert_eq!(report.line.as_deref(), Some("motion: 1 of 3 flying emitters is weightless (no Drag or Acceleration): Effect.Core.Sparks (40 studs/s)"));
	}

	/// A negative Drag speeds particles up (Roblox: "velocities grow exponentially"): accelerating,
	/// not weightless
	#[test]
	fn negative_drag_accelerates_and_is_not_weightless() {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		emitter(&mut model, part, "Pulled", 20.0, -3.0, [0.0, 0.0, 0.0], true);
		emitter(&mut model, part, "Sparks", 40.0, 0.0, [0.0, 0.0, 0.0], true);
		let mut motion = Motion::default();
		motion.add(&drawn(model, None));
		let report = motion.weightless();
		assert_eq!((report.count, report.flying), (1, 2));
		assert_eq!(report.emitters[0].path, "Effect.Core.Sparks");
	}

	/// A playback's emits wake a dormant emitter, its sets change what flies, and the emitters are
	/// named by the particles they put out
	#[test]
	fn a_playback_decides_what_emits_and_how_it_moves() {
		let playback = Playback {
			time: 0.5,
			actions: vec![
				Action { target: "Effect.Core.Dormant".into(), emit: Some(Emit::Count(30)), ..Default::default() },
				Action { at: 0.1, target: "Effect.Core.Dragged".into(), set: [("Drag".to_string(), serde_json::json!(0))].into(), ..Default::default() },
				Action { at: 0.1, target: "Effect.Core.Drift".into(), set: [("Speed".to_string(), serde_json::json!(12.5))].into(), ..Default::default() },
				Action { at: 0.1, target: "Effect.Core.Sparks".into(), set: [("Acceleration".to_string(), serde_json::json!([0, -30, 0]))].into(), ..Default::default() },
			],
			..Default::default()
		};
		let mut motion = Motion::default();
		motion.add(&drawn(effect(), Some(&playback)));
		let report = motion.weightless();
		let named: Vec<(&str, f64)> = report.emitters.iter().map(|emitter| (emitter.path.as_str(), emitter.speed)).collect();
		assert_eq!(named, vec![("Effect.Core.Dormant", 50.0), ("Effect.Core.Dragged", 30.0), ("Effect.Core.Drift", 12.5)]);
		assert_eq!((report.count, report.flying), (3, 5));
		assert!(report.emitters[0].particles == 30 && report.emitters[1].particles > 0);
		assert!(report.line.unwrap().contains("Effect.Core.Drift (12.5 studs/s)"));

		// an emitter disabled and never emitted, or emitting after the moment drawn, puts out nothing
		let later = Playback { time: 0.5, actions: vec![Action { at: 0.8, target: "Effect.Core.Dormant".into(), emit: Some(Emit::Count(30)), ..Default::default() }], ..Default::default() };
		let mut motion = Motion::default();
		motion.add(&drawn(effect(), Some(&later)));
		assert_eq!((motion.weightless().count, motion.weightless().flying), (1, 3));
	}

	/// Over a video's frames an emitter counts once, weightless if it ever flew so
	#[test]
	fn frames_add_up_by_emitter() {
		let mut motion = Motion::default();
		for time in [0.2, 0.4, 0.6] {
			let playback = Playback { time, actions: vec![Action { at: 0.3, target: "Effect.Core.Falling".into(), set: [("Acceleration".to_string(), serde_json::json!([0, 0, 0]))].into(), ..Default::default() }], ..Default::default() };
			motion.add(&drawn(effect(), Some(&playback)));
		}
		let report = motion.weightless();
		assert_eq!((report.count, report.flying), (2, 3));
		// as many particles each (one Rate, one start): the faster first
		assert_eq!(report.emitters.iter().map(|emitter| (emitter.path.as_str(), emitter.speed)).collect::<Vec<_>>(), vec![("Effect.Core.Sparks", 40.0), ("Effect.Core.Falling", 25.0)]);
		assert_eq!(report.emitters[0].particles, report.emitters[1].particles);
		assert_eq!(Motion::default().weightless(), Weightless::default());
	}

	/// Copies of one emitter (same name, same parent) are named once, with how many
	#[test]
	fn copies_share_a_path() {
		let mut model = effect();
		let part = model.dom.get_by_ref(model.model).unwrap().children()[0];
		emitter(&mut model, part, "Sparks", 33.333, 0.0, [0.0, 0.0, 0.0], true);
		let mut motion = Motion::default();
		motion.add(&drawn(model, None));
		let report = motion.weightless();
		assert_eq!((report.count, report.flying, report.share), (2, 4, 0.5));
		assert_eq!(report.emitters, vec![WeightlessEmitter { path: "Effect.Core.Sparks".into(), copies: 2, speed: 40.0, particles: 40 }]);
		assert!(report.line.unwrap().ends_with("2 of 4 flying emitters are weightless (no Drag or Acceleration): Effect.Core.Sparks x2 (40 studs/s)"));
	}

	use rbx_dom_weak::types::{ContentId, NumberSequence, NumberSequenceKeypoint, Variant};

	use crate::scene::playback::TweenKey;

	fn keys(keys: &[(f32, f32, f32)]) -> Variant {
		Variant::NumberSequence(NumberSequence { keypoints: keys.iter().map(|&(time, value, envelope)| NumberSequenceKeypoint::new(time, value, envelope)).collect() })
	}

	fn content(uri: &str) -> Variant {
		Variant::ContentId(ContentId::from(uri.to_string()))
	}

	/// A ParticleEmitter under `parent` emitting 20 a second for a second, with these properties.
	fn emitter_with(model: &mut ModelBuilder, parent: Ref, name: &str, properties: &[(&str, Variant)]) {
		let mut builder = InstanceBuilder::new("ParticleEmitter").with_name(name).with_property("Rate", 20.0f32).with_property("Lifetime", NumberRange::new(1.0, 1.0));
		for (property, value) in properties {
			builder = builder.with_property(*property, value.clone());
		}
		model.child(parent, builder);
	}

	/// A Beam on the part between two attachments, with these properties.
	fn beam_with(model: &mut ModelBuilder, part: Ref, name: &str, properties: &[(&str, Variant)]) {
		let a0 = model.child(part, InstanceBuilder::new("Attachment").with_name(format!("{name}0")));
		let a1 = model.child(part, InstanceBuilder::new("Attachment").with_name(format!("{name}1")).with_property("CFrame", crate::scene::build::cframe(glam::Affine3A::from_translation(glam::Vec3::new(4.0, 0.0, 0.0)))));
		let mut builder = InstanceBuilder::new("Beam").with_name(name).with_property("Attachment0", Variant::Ref(a0)).with_property("Attachment1", Variant::Ref(a1));
		for (property, value) in properties {
			builder = builder.with_property(*property, value.clone());
		}
		model.child(part, builder);
	}

	fn lint_of(model: ModelBuilder) -> Lint {
		let mut effects = Effects::default();
		effects.add(&drawn(model, None));
		effects.lint(&[], &[])
	}

	/// Two keypoints, or several on one line, without envelopes and not constant, are linear fades;
	/// curves, envelopes, constants and emitters that never emit are not
	#[test]
	fn linear_sequences_are_fades() {
		assert!(linear_sequence(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]));
		assert!(linear_sequence(&[(0.0, 0.2, 0.0), (0.5, 0.6, 0.0), (1.0, 1.0, 0.0)]));
		assert!(linear_sequence(&[(0.0, 1.0, 0.0), (0.3, 0.715, 0.0), (1.0, 0.0, 0.0)]), "within 2 % of the line");
		assert!(!linear_sequence(&[(0.0, 1.0, 0.0), (0.3, 0.675, 0.0), (1.0, 0.0, 0.0)]), "2.5 % off");
		assert!(!linear_sequence(&[(0.0, 0.0, 0.0), (0.2, 0.8, 0.0), (1.0, 1.0, 0.0)]), "eased");
		assert!(!linear_sequence(&[(0.0, 0.0, 0.0), (0.5, 1.0, 0.0), (1.0, 0.0, 0.0)]), "a hump");
		assert!(!linear_sequence(&[(0.0, 0.0, 0.1), (1.0, 1.0, 0.1)]), "an envelope");
		assert!(!linear_sequence(&[(0.0, 0.5, 0.0), (1.0, 0.5, 0.0)]), "constant");
		assert!(!linear_sequence(&[(0.0, 0.5, 0.0)]));

		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		let ramp = keys(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]);
		emitter_with(&mut model, part, "Ramp", &[("Transparency", ramp.clone())]);
		emitter_with(&mut model, part, "Eased", &[("Transparency", keys(&[(0.0, 0.0, 0.0), (0.2, 0.8, 0.0), (1.0, 1.0, 0.0)]))]);
		emitter_with(&mut model, part, "Plain", &[]);
		emitter_with(&mut model, part, "Dormant", &[("Transparency", ramp.clone()), ("Enabled", Variant::Bool(false))]);
		beam_with(&mut model, part, "Ray", &[("Transparency", ramp.clone())]);
		beam_with(&mut model, part, "Glow", &[("Transparency", keys(&[(0.0, 1.0, 0.0), (0.5, 0.0, 0.0), (1.0, 1.0, 0.0)]))]);
		let lint = lint_of(model);
		let named: Vec<(&str, &str, FadeKind)> = lint.fades.entries.iter().map(|fade| (fade.path.as_str(), fade.property.as_str(), fade.kind)).collect();
		assert_eq!(named, vec![("Effect.Core.Ramp", "Transparency", FadeKind::Sequence), ("Effect.Core.Ray", "Transparency", FadeKind::Sequence)]);
		assert_eq!(lint.fades.count, 2);
		assert_eq!(lint.lines()[0], "fades: 2 linear: Effect.Core.Ramp Transparency (sequence), Effect.Core.Ray Transparency (sequence)");
	}

	/// A trail fades by its points' life: a linear Transparency or WidthScale counts once it draws
	#[test]
	fn trails_fade_once_they_draw() {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Blade".into(), transparency: 1.0, ..Default::default() });
		let a0 = model.child(part, InstanceBuilder::new("Attachment").with_name("Top"));
		let a1 = model.child(part, InstanceBuilder::new("Attachment").with_name("Bottom"));
		let ramp = keys(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.0)]);
		model.child(part, InstanceBuilder::new("Trail").with_name("Swing").with_property("Attachment0", Variant::Ref(a0)).with_property("Attachment1", Variant::Ref(a1)).with_property("Transparency", ramp.clone()).with_property("WidthScale", ramp));
		let mut scene = drawn(model, None);
		assert_eq!(scene.trails.len(), 1);
		let mut effects = Effects::default();
		effects.add(&scene);
		assert_eq!(effects.lint(&[], &[]).fades.count, 0, "a trail that never moved draws nothing");
		let point = crate::scene::trails::TrailPoint { time: 0.0, position0: glam::Vec3::ZERO, position1: glam::Vec3::Y, starts_strip: false };
		scene.trails[0].points = vec![point, crate::scene::trails::TrailPoint { time: 0.1, position0: glam::Vec3::X, ..point }];
		effects.add(&scene);
		let properties: Vec<String> = effects.lint(&[], &[]).fades.entries.into_iter().map(|fade| fade.property).collect();
		assert_eq!(properties, ["Transparency", "WidthScale"]);
	}

	fn tween(target: &str, property: &str, keys: &[(f32, serde_json::Value)]) -> Tween {
		Tween { target: target.into(), property: property.into(), keys: keys.iter().map(|(at, value)| TweenKey { at: *at, value: value.clone(), ..Default::default() }).collect(), ..Default::default() }
	}

	/// Every frame of a tween recorded (as a sim records one): `value(t)` from `start` to `end`.
	fn recorded(start: f32, end: f32, value: impl Fn(f32) -> serde_json::Value) -> Vec<(f32, serde_json::Value)> {
		let frames = ((end - start) * 60.0).round() as usize;
		(0..=frames).map(|frame| start + frame as f32 / 60.0).map(|at| (at, value(at))).collect()
	}

	/// Tweens of a fade's properties on Beams, Decals and GUI images fade linearly when a stretch of
	/// them is a straight line in time (Linear keys, or recorded frame by frame on one line), or a
	/// TweenService tween is eased Linear; curves, short stretches and other instances do not
	#[test]
	fn tweens_that_fade_linearly() {
		use serde_json::json;
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		beam_with(&mut model, part, "Ray", &[]);
		beam_with(&mut model, part, "Arc", &[]);
		model.child(part, InstanceBuilder::new("Decal").with_name("Scorch"));
		model.child(part, InstanceBuilder::new("Decal").with_name("Crack"));
		model.child(part, InstanceBuilder::new("Texture").with_name("Tiles"));
		let gui = model.child(part, InstanceBuilder::new("BillboardGui").with_name("Flash"));
		model.child(gui, InstanceBuilder::new("ImageLabel").with_name("Star"));
		model.child(gui, InstanceBuilder::new("ImageLabel").with_name("Ring"));
		model.child(gui, InstanceBuilder::new("Frame").with_name("Back"));
		let document = model.into_document();

		let quad_out = |t: f32| 1.0 - (1.0 - t) * (1.0 - t);
		let playback = Playback {
			tweens: vec![
				// two keys, Linear by default: a linear fade
				tween("Effect.Core.Ray", "Width0", &[(0.0, json!(4)), (0.3, json!(0))]),
				// eased by its end key
				Tween { keys: vec![TweenKey { at: 0.0, value: json!(4), ..Default::default() }, TweenKey { at: 0.3, value: json!(0), easing: Some("Quad".into()), ..Default::default() }], ..tween("Effect.Core.Arc", "Width1", &[]) },
				// recorded: held, then a straight ramp from 0.55 s
				tween("Effect.Core.Flash.Star", "ImageTransparency", &recorded(0.1, 1.0, |t| json!(if t < 0.55 { 1.0 } else { 1.0 - (t - 0.55) / 0.45 }))),
				// recorded UDim2 sizes eased Quad Out: a curve
				tween("Effect.Core.Flash.Ring", "Size", &recorded(0.0, 0.5, |t| json!([quad_out(t / 0.5), 0, quad_out(t / 0.5), 0]))),
				// a recorded flicker: straight one-frame stretches, none long enough
				tween("Effect.Core.Flash.Ring", "ImageTransparency", &recorded(0.0, 0.5, |t| json!(((t * 60.0).round() as i32 % 2) as f32 * 0.5))),
				// TweenService tweens: Linear fades, the default Quad does not
				Tween { to: Some(json!(1)), easing: Some("Linear".into()), duration: 0.4, ..tween("Effect.Core.Scorch", "Transparency", &[]) },
				Tween { to: Some(json!(1)), duration: 0.4, ..tween("Effect.Core.Crack", "Transparency", &[]) },
				// not a fade's property, nor an instance a fade tweens
				tween("Effect.Core.Flash.Star", "Rotation", &[(0.0, json!(0)), (1.0, json!(90))]),
				tween("Effect.Core.Flash.Back", "Size", &[(0.0, json!([0, 0, 0, 0])), (1.0, json!([1, 0, 1, 0]))]),
				tween("Effect.Core", "Transparency", &[(0.0, json!(0)), (1.0, json!(1))]),
				// a class at or under the target: each Decal (a Texture is one)
				Tween { class: Some("Decal".into()), ..tween("Effect.Core", "Transparency", &[(0.0, json!(1)), (0.5, json!(0))]) },
			],
			..Default::default()
		};
		let fades: Vec<(String, String)> = tween_fades(&document, &playback).into_iter().map(|fade| (fade.path, fade.property)).collect();
		let expected = [("Effect.Core.Crack", "Transparency"), ("Effect.Core.Flash.Star", "ImageTransparency"), ("Effect.Core.Ray", "Width0"), ("Effect.Core.Scorch", "Transparency"), ("Effect.Core.Tiles", "Transparency")];
		assert_eq!(fades, expected.map(|(path, property)| (path.to_string(), property.to_string())));
		assert!(tween_fades(&document, &Playback { tweens: vec![tween("Effect.Missing", "Transparency", &[(0.0, json!(0)), (1.0, json!(1))])], ..Default::default() }).is_empty());
	}

	/// Within one form: a texture on two emitters (however its id is written), and LightEmissions of
	/// both signs or more than 1 apart; another form's same texture and 0..1 blends are fine
	#[test]
	fn restraint_within_a_form() {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		let burst = model.child(part, InstanceBuilder::new("Attachment").with_name("Burst"));
		let glow = model.child(part, InstanceBuilder::new("Attachment").with_name("Glow"));
		emitter_with(&mut model, burst, "Sparks", &[("Texture", content("rbxassetid://5")), ("LightEmission", Variant::Float32(1.0))]);
		emitter_with(&mut model, burst, "Flecks", &[("Texture", content("http://www.roblox.com/asset/?id=5")), ("LightEmission", Variant::Float32(0.0))]);
		emitter_with(&mut model, burst, "Smoke", &[("Texture", content("rbxassetid://6")), ("LightEmission", Variant::Float32(-0.5))]);
		emitter_with(&mut model, glow, "Core", &[("Texture", content("rbxassetid://5")), ("LightEmission", Variant::Float32(0.0))]);
		emitter_with(&mut model, glow, "Halo", &[("Texture", content("rbxassetid://7")), ("LightEmission", Variant::Float32(1.0))]);
		emitter_with(&mut model, glow, "Unlit", &[("Texture", content("rbxassetid://7")), ("Enabled", Variant::Bool(false))]);
		let Restraint { duplicate_textures, mixed_blends } = lint_of(model).restraint;
		assert_eq!(duplicate_textures, vec![DuplicateTexture { parent: "Effect.Core.Burst".into(), path: "Effect.Core.Burst".into(), texture: "rbxassetid://5".into(), emitters: vec!["Effect.Core.Burst.Flecks".into(), "Effect.Core.Burst.Sparks".into()] }]);
		assert_eq!(mixed_blends.len(), 1);
		assert_eq!((mixed_blends[0].parent.as_str(), mixed_blends[0].light_emissions.as_slice(), mixed_blends[0].emitters.len()), ("Effect.Core.Burst", [-0.5, 0.0, 1.0].as_slice(), 3));

		// values more than 1 apart, of one sign
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		emitter_with(&mut model, part, "A", &[("Texture", content("rbxassetid://8")), ("LightEmission", Variant::Float32(0.0))]);
		emitter_with(&mut model, part, "B", &[("Texture", content("rbxassetid://9")), ("LightEmission", Variant::Float32(1.5))]);
		let lint = lint_of(model);
		assert_eq!(lint.restraint.mixed_blends[0].light_emissions, [0.0, 1.5]);
		assert_eq!(lint.lines(), ["restraint: 0 repeated textures and 1 mixed blend in a form: Effect.Core mixes LightEmission 0 / 1.5"]);
	}

	/// Drag 15 or more with particles living a second or more stops dead and hangs
	#[test]
	fn frozen_clouds() {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		let drag = |value: f32| ("Drag", Variant::Float32(value));
		let lifetime = |min: f32, max: f32| ("Lifetime", Variant::NumberRange(NumberRange::new(min, max)));
		emitter_with(&mut model, part, "Smoke", &[drag(20.0), lifetime(0.5, 2.0)]);
		emitter_with(&mut model, part, "Puff", &[drag(20.0), lifetime(0.5, 0.9)]);
		emitter_with(&mut model, part, "Dust", &[drag(10.0), lifetime(3.0, 3.0)]);
		emitter_with(&mut model, part, "Held", &[drag(30.0), lifetime(3.0, 3.0), ("Enabled", Variant::Bool(false))]);
		let lint = lint_of(model);
		assert_eq!(lint.frozen, vec![Frozen { path: "Effect.Core.Smoke".into(), drag: 20.0, lifetime: 2.0 }]);
		assert_eq!(lint.lines().last().unwrap(), "frozen: 1 emitter stops dead and hangs (Drag 15+, living 1 s+): Effect.Core.Smoke (Drag 20, 2 s)");
	}

	/// Beams with TextureSpeed 0 and no CurveSize in every frame they draw are rigid, each copy
	/// counted; beams that scroll or curve (in any frame), beams that never show and trails are not
	#[test]
	fn rigid_beams() {
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), transparency: 1.0, ..Default::default() });
		let still = ("TextureSpeed", Variant::Float32(0.0));
		beam_with(&mut model, part, "Streak", &[still.clone()]);
		beam_with(&mut model, part, "Streak", &[still.clone()]);
		beam_with(&mut model, part, "Scroll", &[]);
		beam_with(&mut model, part, "Arc", &[still.clone(), ("CurveSize1", Variant::Float32(-2.0))]);
		beam_with(&mut model, part, "Later", &[still.clone()]);
		beam_with(&mut model, part, "Hidden", &[still.clone(), ("Transparency", keys(&[(0.0, 1.0, 0.0), (1.0, 1.0, 0.0)]))]);
		beam_with(&mut model, part, "Thin", &[still.clone(), ("Width0", Variant::Float32(0.0)), ("Width1", Variant::Float32(0.0))]);
		let a0 = model.child(part, InstanceBuilder::new("Attachment").with_name("Top"));
		let a1 = model.child(part, InstanceBuilder::new("Attachment").with_name("Bottom"));
		model.child(part, InstanceBuilder::new("Trail").with_name("Swing").with_property("Attachment0", Variant::Ref(a0)).with_property("Attachment1", Variant::Ref(a1)));
		let mut scene = drawn(model, None);
		assert_eq!((scene.beams.len(), scene.trails.len()), (7, 1));
		let point = crate::scene::trails::TrailPoint { time: 0.0, position0: glam::Vec3::ZERO, position1: glam::Vec3::Y, starts_strip: false };
		scene.trails[0].points = vec![point, crate::scene::trails::TrailPoint { time: 0.1, position0: glam::Vec3::X, ..point }];
		let mut effects = Effects::default();
		effects.add(&scene);
		// the next frame, a playback sets Later's TextureSpeed
		scene.beams.iter_mut().filter(|beam| beam.path == "Effect.Core.Later").for_each(|beam| beam.texture_speed = 0.5);
		effects.add(&scene);
		let lint = effects.lint(&[], &[]);
		assert_eq!((lint.rigid.count, lint.rigid.beams), (2, 5));
		assert_eq!(lint.rigid.entries, vec![RigidBeam { path: "Effect.Core.Streak".into(), copies: 2, texture_speed: 0.0, curved: false }]);
		assert_eq!(lint.lines().last().unwrap(), "rigid: 2 of 5 beams neither scroll nor curve (TextureSpeed 0, no CurveSize): Effect.Core.Streak x2");
		let json = serde_json::to_value(&lint.rigid).unwrap();
		assert_eq!(json["entries"][0], serde_json::json!({ "path": "Effect.Core.Streak", "copies": 2, "textureSpeed": 0.0, "curved": false }));
	}

	/// Images Roblox refuses to everyone, read from the renderer's missing assets, with every instance
	/// drawing them; one refused to this account alone, or missing otherwise, is not
	#[test]
	fn refused_images_and_what_draws_them() {
		use ror_assets::{AssetError, RefusalReason};
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec { name: "Core".into(), ..Default::default() });
		emitter_with(&mut model, part, "Sparks", &[("Texture", content("rbxassetid://19"))]);
		emitter_with(&mut model, part, "Embers", &[("Texture", content("http://www.roblox.com/asset/?id=19"))]);
		model.child(part, InstanceBuilder::new("Decal").with_name("Scorch").with_property("Texture", content("rbxassetid://22")));
		let mut effects = Effects::default();
		effects.add(&drawn(model, None));
		let error = |id: u64, reason| AssetError::Http { uri: format!("rbxassetid://{id}"), status: 403, reason: Some(reason) }.to_string();
		let missing = [
			format!("Effect.Core.Sparks: {} (its particles are not drawn)", error(19, RefusalReason::Moderated)),
			format!("Effect.Core: {} (its decal is not drawn)", error(22, RefusalReason::Archived)),
			format!("Effect.Core.Other: {} (not drawn)", error(23, RefusalReason::NotAuthorized)),
			"Effect.Core: rbxassetid://24 not found".to_string(),
		];
		let lint = effects.lint(&[], &missing);
		assert_eq!(
			lint.refused,
			vec![
				Refused { path: "Effect.Core.Embers".into(), asset: "rbxassetid://19".into(), reason: "moderated".into(), paths: vec!["Effect.Core.Embers".into(), "Effect.Core.Sparks".into()] },
				Refused { path: "Effect.Core".into(), asset: "rbxassetid://22".into(), reason: "archived".into(), paths: vec!["Effect.Core".into()] },
			]
		);
		assert_eq!(lint.lines().last().unwrap(), "refused: 2 images Roblox refuses to everyone, drawn by nothing: rbxassetid://19 (moderated, 2 instances), rbxassetid://22 (archived, 1 instance)");
		assert_eq!(refusal(&format!("x: {}", error(18, RefusalReason::Unreviewed))), Some(("rbxassetid://18".to_string(), "unreviewed")));
		// the labels are ror_assets' own names for the reasons
		for (reason, label) in [(RefusalReason::Moderated, "moderated"), (RefusalReason::Archived, "archived"), (RefusalReason::Unreviewed, "unreviewed")] {
			assert_eq!(serde_json::to_value(&reason).unwrap(), label);
		}
	}

	/// Each child of the loaded root is an effect, and so is each child of a sim's World; emitters
	/// count as the census counts them (each present once, copies apart), textures once each
	#[test]
	fn effects_counted_by_root() {
		let mut model = ModelBuilder::new("Sim");
		let world = model.child(model.model, InstanceBuilder::new("Folder").with_name("World"));
		let burst = model.child(world, InstanceBuilder::new("Part").with_name("Burst").with_property("Transparency", 1.0f32));
		let ring = model.child(world, InstanceBuilder::new("Part").with_name("Ring").with_property("Transparency", 1.0f32));
		let floor = model.part(&PartSpec { name: "Floor".into(), ..Default::default() });
		for (name, texture) in [("Sparks", "rbxassetid://1"), ("Sparks", "rbxassetid://1"), ("Glow", "rbxassetid://2"), ("Dark", "rbxassetid://3")] {
			emitter_with(&mut model, burst, name, &[("Texture", content(texture)), ("Enabled", Variant::Bool(name != "Dark"))]);
		}
		beam_with(&mut model, ring, "Arc", &[]);
		emitter_with(&mut model, floor, "Dust", &[]);
		let document = crate::scene::load::read_bytes(&model.to_rbxm().unwrap()).unwrap();
		let roots = EffectRoots::of(&document);
		assert_eq!([roots.root("Sim.World.Burst.Sparks"), roots.root("Sim.Floor.Dust"), roots.root("Sim.World")], ["Sim.World.Burst", "Sim.Floor", "Sim.World"]);
		let mut effects = Effects::default();
		effects.add(&extract::extract(&document));
		let counts = effects.counts(&roots);
		let rows: Vec<(&str, usize, usize, usize)> = counts.effects.iter().map(|effect| (effect.root.as_str(), effect.emitters, effect.textures, effect.beams)).collect();
		// Dust draws ParticleEmitter's default texture
		assert_eq!(rows, vec![("Sim.Floor", 1, 1, 0), ("Sim.World.Burst", 4, 3, 0), ("Sim.World.Ring", 0, 0, 1)]);
		assert_eq!(counts.line().unwrap(), "effects: Sim.Floor (1 emitter, 1 texture), Sim.World.Burst (4 emitters, 3 textures), Sim.World.Ring (0 emitters, 0 textures, 1 beam)");
	}

	/// Names a dot, a bracket or an edge space would split (an effect's `Exp4 [5]`, which recordings
	/// carry) are quoted steps of a path that read back to the same names; forms and effects group by
	/// the instances holding each emitter, so siblings named alike under other parents stay apart
	#[test]
	fn bracketed_names_group_by_what_holds_them() {
		use crate::scene::load::{path_segment, split_path};
		assert_eq!([path_segment("Exp4 [5]"), path_segment("a.b"), path_segment(" pad"), path_segment(r#"v1."hi""#), path_segment(r#"say "hi""#)], [r#"["Exp4 [5]"]"#, r#"["a.b"]"#, r#"[" pad"]"#, r#"['v1."hi"']"#, r#"say "hi""#]);

		let mut model = ModelBuilder::new("Sim");
		let world = model.child(model.model, InstanceBuilder::new("Folder").with_name("World"));
		let impact = model.child(world, InstanceBuilder::new("Part").with_name("Impact").with_property("Transparency", 1.0f32));
		let fx = model.child(impact, InstanceBuilder::new("Attachment").with_name("FX03"));
		let main = model.child(impact, InstanceBuilder::new("Attachment").with_name("Main.FX"));
		let bracketed = model.child(world, InstanceBuilder::new("Part").with_name("Ring [2]").with_property("Transparency", 1.0f32));
		for (parent, name, texture) in [(fx, "Exp4 [5]", "rbxassetid://1"), (fx, "Dots [40]", "rbxassetid://2"), (fx, "Exp9 [24]", "rbxassetid://2"), (main, "Exp4 [5]", "rbxassetid://1"), (main, "Glow", "rbxassetid://3"), (bracketed, "Spin", "rbxassetid://4")] {
			emitter_with(&mut model, parent, name, &[("Texture", content(texture))]);
		}
		let document = crate::scene::load::read_bytes(&model.to_rbxm().unwrap()).unwrap();
		let scene = extract::extract(&document);
		let mut paths: Vec<&str> = scene.particles.iter().map(|emitter| emitter.path.as_str()).collect();
		paths.sort();
		assert_eq!(paths, [r#"Sim.World.Impact.FX03["Dots [40]"]"#, r#"Sim.World.Impact.FX03["Exp4 [5]"]"#, r#"Sim.World.Impact.FX03["Exp9 [24]"]"#, r#"Sim.World.Impact["Main.FX"].Glow"#, r#"Sim.World.Impact["Main.FX"]["Exp4 [5]"]"#, r#"Sim.World["Ring [2]"].Spin"#]);
		assert_eq!(split_path(r#"Sim.World.Impact["Main.FX"]["Exp4 [5]"]"#).unwrap(), ["Sim", "World", "Impact", "Main.FX", "Exp4 [5]"]);
		assert_eq!(scene.lineage[r#"Sim.World["Ring [2]"].Spin"#], ["Sim", "Sim.World", r#"Sim.World["Ring [2]"]"#, r#"Sim.World["Ring [2]"].Spin"#]);

		let mut effects = Effects::default();
		effects.add(&scene);
		let counts = effects.counts(&EffectRoots::of(&document));
		let rows: Vec<(&str, usize, usize)> = counts.effects.iter().map(|effect| (effect.root.as_str(), effect.emitters, effect.textures)).collect();
		assert_eq!(rows, vec![("Sim.World.Impact", 5, 3), (r#"Sim.World["Ring [2]"]"#, 1, 1)]);
		// the one texture laid twice within a form is FX03's; "Exp4 [5]" under two parents is two forms
		let lint = effects.lint(&[], &[]);
		let repeats: Vec<(&str, &str, Vec<&str>)> = lint.restraint.duplicate_textures.iter().map(|entry| (entry.parent.as_str(), entry.texture.as_str(), entry.emitters.iter().map(String::as_str).collect())).collect();
		assert_eq!(repeats, vec![("Sim.World.Impact.FX03", "rbxassetid://2", vec![r#"Sim.World.Impact.FX03["Dots [40]"]"#, r#"Sim.World.Impact.FX03["Exp9 [24]"]"#])]);
		// a path with no lineage (a scene built without one) is read back alike
		let roots = EffectRoots::of(&document);
		assert_eq!([roots.root(r#"Sim.World.Impact["Main.FX"]["Exp4 [5]"]"#), roots.root(r#"Sim.World["Ring [2]"].Spin"#)], ["Sim.World.Impact", r#"Sim.World["Ring [2]"]"#]);
	}
}
