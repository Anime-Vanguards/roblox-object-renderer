//! Effect sweeps (notes/render.md "Playback", "Sweeps"): many bundle effects, each played as
//! the generic runner plays one (`attacks::auto_play`), a few at a time in one view. A batch is
//! uploaded once and played twice in the capture Studio (the first play uncaptured, so its textures
//! are in at full resolution, timeline.rs), the frames nearest every moment kept from the second;
//! ror draws each kept frame's playback at its time with Studio's frame times, again from other
//! random streams (how far apart two draws of random particles are) and without particles, trails
//! and beams (what the effects add).
//!
//! `vfx-sweep:<spec file>` reads a JSON [`Spec`]: `effects` batched here by size, `per` to a view,
//! side by side in a grid facing the camera, each cell framed as the generic run frames one effect;
//! or `batches` given whole (effects where `at` puts them, overlapping or apart, seen from a given
//! camera or from one fitted to them, nearer or farther by `scale`). Lighting: `default`, `night`,
//! `dark` or a place file's (its clouds left out), `+retro` adding a ColorGradingEffect with the
//! Retro tonemapper. Each batch writes out/calibrate/vfx-sweep-<spec>/<batch>/: `cells.json` (each
//! effect's image rectangle and bounds), `model.rbxm`, `playback.json`, `camera.json`, `assets.json`
//! (what the capture Studio's ContentProvider failed to load, by cell: such effects are not compared),
//! each side's view without the batch (`studio-sky.png`, Studio's with the model out of the world, and
//! `ror-sky.png`), and per moment `t<moment>/` with studio.png, studio.json, ror.png, ror-s<k>.png
//! (other random streams), parts.png (ror without particles, trails and beams) and, with `bare`,
//! studio-bare.png (Studio's play of the batch without its emitters, trails and beams); then
//! `done.json`, so a later run skips it (`ROR_RERENDER=1` draws ror's side again from the kept
//! Studio frames, `ROR_SWEEP_ASSETS=1` checks their assets alone). `tools/vfx_sweep.py` measures and
//! prunes them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glam::Vec3;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::attacks::{self, Bundle, ROOT, VIEW};
use super::timeline::{self, Captured, Marker, Timeline};
use super::{Experiment, Run};
use crate::output;
use crate::render::camera::{Camera, CameraSpec};
use crate::render::{Background, Renderer, ViewRequest};
use crate::scene::build::ModelBuilder;
use crate::scene::environment::{PostEffect, Sky};
use crate::scene::playback::Playback;
use crate::scene::{self, Environment};
use crate::studio::CaptureStudio;
use crate::studio::compare::CaptureSession;

/// The generic run's moments after the first burst.
fn default_moments() -> Vec<f32> {
	vec![0.1, 0.3, 0.6]
}

fn default_per() -> usize {
	4
}

fn default_seeds() -> u32 {
	1
}

/// One bundle effect in a sweep.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectSpec {
	/// The bundle file and the effect's dotted path in it.
	pub bundle: String,
	pub effect: String,
	/// Flies the generic run's arc (default: an effect with trails does).
	#[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
	pub moving: Option<bool>,
	/// Where its parts' centre goes in a given batch (default 4 studs above the origin).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub at: Option<[f32; 3]>,
	/// Seconds after the batch's first burst its own first burst comes.
	#[serde(default)]
	pub delay: f32,
	/// Its parts' centre as high above `at` as it stood above its bundle's origin (the ground of the
	/// place it was made in; 4 studs for one made more than 40 studs off it): an effect on the ground
	/// of a batch with one stays on it.
	#[serde(default)]
	pub keep_height: bool,
	/// Only the ParticleEmitters, Trails and Beams of these names stay (which of an effect's parts
	/// draws what): the effect framed as a whole. `name#k` keeps the k-th of that name in the file's
	/// order (from 0), for emitters sharing one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub only: Option<Vec<String>>,
	/// Instances of these classes or names go (its Decals, a Highlight: what the rest draws alone).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub strip: Option<Vec<String>>,
	/// Properties set on its instances once `only` and `strip` are done: one property of the effect
	/// itself varied.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub set: Vec<SetSpec>,
}

/// A property set on every instance of a class or name (`*`: all) under an effect: a number, a bool
/// or three numbers, of the type the instance already has (a Float32, an Enum's value, a Color3 or
/// Color3uint8, a Vector3).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SetSpec {
	#[serde(rename = "match")]
	pub matches: String,
	pub property: String,
	pub value: serde_json::Value,
}

/// `value` as the type of the property's `current` value (a number alone is a Float32).
fn variant(current: Option<&rbx_dom_weak::types::Variant>, value: &serde_json::Value) -> Option<rbx_dom_weak::types::Variant> {
	use rbx_dom_weak::types::{Color3, Color3uint8, Enum, Variant, Vector3};

	let number = value.as_f64();
	let triple = value.as_array().filter(|items| items.len() == 3).map(|items| items.iter().map(|item| item.as_f64().unwrap_or(0.0) as f32).collect::<Vec<f32>>());
	Some(match current {
		Some(Variant::Float32(_)) => Variant::Float32(number? as f32),
		Some(Variant::Float64(_)) => Variant::Float64(number?),
		Some(Variant::Int32(_)) => Variant::Int32(number? as i32),
		Some(Variant::Enum(_)) => Variant::Enum(Enum::from_u32(number? as u32)),
		Some(Variant::Bool(_)) => Variant::Bool(value.as_bool()?),
		Some(Variant::Color3(_)) => triple.map(|t| Variant::Color3(Color3::new(t[0], t[1], t[2])))?,
		Some(Variant::Color3uint8(_)) => triple.map(|t| Variant::Color3uint8(Color3uint8::new(t[0] as u8, t[1] as u8, t[2] as u8)))?,
		Some(Variant::Vector3(_)) => triple.map(|t| Variant::Vector3(Vector3::new(t[0], t[1], t[2])))?,
		None if value.is_boolean() => Variant::Bool(value.as_bool()?),
		None => Variant::Float32(number? as f32),
		Some(_) => return None,
	})
}

/// Applies an effect's `set` to its copy at `root`.
fn apply_sets(model: &mut ModelBuilder, root: rbx_dom_weak::types::Ref, effect: &EffectSpec) -> Result<()> {
	for set in &effect.set {
		for referent in attacks::descendants(&model.dom, root) {
			let Some(instance) = model.dom.get_by_ref(referent) else {
				continue;
			};
			if set.matches != "*" && set.matches != instance.class && set.matches != instance.name {
				continue;
			}
			let value = variant(instance.properties.get(&rbx_dom_weak::ustr(&set.property)), &set.value).with_context(|| format!("{}: cannot set {} to {}", instance.name, set.property, set.value))?;
			model.set(referent, &set.property, value);
		}
	}
	Ok(())
}

/// What `only` and `strip` take out of an effect's copy.
fn doomed(dom: &rbx_dom_weak::WeakDom, root: rbx_dom_weak::types::Ref, effect: &EffectSpec) -> Vec<rbx_dom_weak::types::Ref> {
	fn walk(dom: &rbx_dom_weak::WeakDom, referent: rbx_dom_weak::types::Ref, effect: &EffectSpec, seen: &mut HashMap<String, usize>, doomed: &mut Vec<rbx_dom_weak::types::Ref>) {
		let Some(instance) = dom.get_by_ref(referent) else {
			return;
		};
		if effect.strip.as_ref().is_some_and(|strip| strip.iter().any(|name| *name == instance.class || *name == instance.name)) {
			doomed.push(referent);
			return;
		}
		if let Some(only) = &effect.only
			&& matches!(instance.class.as_str(), "ParticleEmitter" | "Trail" | "Beam")
		{
			let index = seen.entry(instance.name.clone()).or_insert(0);
			let numbered = format!("{}#{index}", instance.name);
			*index += 1;
			if !only.iter().any(|name| *name == instance.name || *name == numbered) {
				doomed.push(referent);
			}
		}
		for &child in instance.children() {
			walk(dom, child, effect, seen, doomed);
		}
	}
	let mut doomed = Vec::new();
	walk(dom, root, effect, &mut HashMap::new(), &mut doomed);
	doomed
}

/// A camera fitted to what a batch draws: from `yaw` / `pitch` degrees (the generic run's 35 and 20),
/// through `fov`, the distance times `scale` (above 1 farther, below 1 nearer, into the effect).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSpec {
	#[serde(default)]
	pub yaw: Option<f32>,
	#[serde(default)]
	pub pitch: Option<f32>,
	#[serde(default)]
	pub fov: Option<f32>,
	#[serde(default)]
	pub padding: Option<f32>,
	#[serde(default)]
	pub scale: Option<f32>,
}

/// Effects played together in one view.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSpec {
	pub name: String,
	pub effects: Vec<EffectSpec>,
	/// Side by side in a grid facing the camera (default: where `at` puts them).
	#[serde(default)]
	pub grid: bool,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub camera: Option<CameraSpec>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub view: Option<ViewSpec>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub lighting: Option<String>,
	/// A SmoothPlastic ground of this colour under the batch, its top at y = 0 (each side's view
	/// without the effects keeps it).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub ground: Option<[u8; 3]>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
	/// Effects batched by size and by whether they fly, `per` to a view (in a grid).
	#[serde(default)]
	pub effects: Vec<EffectSpec>,
	#[serde(default = "default_per")]
	pub per: usize,
	#[serde(default)]
	pub batches: Vec<BatchSpec>,
	/// Seconds after each batch's first burst.
	#[serde(default = "default_moments")]
	pub moments: Vec<f32>,
	/// ror's draws from other random streams per moment.
	#[serde(default = "default_seeds")]
	pub seeds: u32,
	#[serde(default)]
	pub lighting: Option<String>,
	#[serde(default)]
	pub view: Option<ViewSpec>,
	/// Studio plays each batch once more without its emitters, trails and beams.
	#[serde(default)]
	pub bare: bool,
}

fn sweeps() -> &'static Mutex<HashMap<String, Spec>> {
	static SWEEPS: OnceLock<Mutex<HashMap<String, Spec>>> = OnceLock::new();
	SWEEPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The sweep of an experiment, if it is one.
pub fn get(experiment: &str) -> Option<Spec> {
	sweeps().lock().ok()?.get(experiment).cloned()
}

/// `vfx-sweep:<spec file>`: the experiment (its runs are the batches, made in [`run`]).
pub fn experiment(path: &str) -> Result<Experiment> {
	let text = std::fs::read_to_string(path).with_context(|| format!("vfx-sweep: reading {path}"))?;
	let spec: Spec = serde_json::from_str(&text).with_context(|| format!("vfx-sweep: {path}"))?;
	let stem = Path::new(path).file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default();
	let name = format!("vfx-sweep-{}", attacks::slug(&stem));
	if let Ok(mut sweeps) = sweeps().lock() {
		sweeps.insert(name.clone(), spec);
	}
	Ok(Experiment { name, model: ModelBuilder::new(ROOT), probes: Vec::new(), runs: Vec::<Run>::new() })
}

/// An effect measured alone: its drawn bounds about its parts' centre (`drawn` false when ror's
/// playback draws nothing then: the bounds of its parts and attachments instead), its first burst and
/// whether it flies.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measured {
	low: [f32; 3],
	high: [f32; 3],
	drawn: bool,
	first_burst: f32,
	moving: bool,
}

impl Measured {
	fn size(&self) -> f32 {
		(Vec3::from(self.high) - Vec3::from(self.low)).length()
	}
}

/// Where the generic run puts an effect's parts' centre.
const HOME: Vec3 = Vec3::new(0.0, 4.0, 0.0);

/// The name of a batch's ground part (`BatchSpec::ground`).
const GROUND: &str = "Ground";

/// Bounds of an effect's parts at rest, invisible ones too (an effect ror's playback draws nothing of:
/// what Studio draws there is what the sweep is after).
fn rest_bounds(bundle: &Bundle, effect: &str) -> Result<(Vec3, Vec3)> {
	let dom = &bundle.model.dom;
	let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
	for referent in attacks::descendants(dom, attacks::find(dom, effect)?) {
		let Some(part) = dom.get_by_ref(referent).filter(|instance| scene::props::is_a(instance, "BasePart")) else {
			continue;
		};
		let Some(cframe) = scene::props::cframe(part, "CFrame") else {
			continue;
		};
		let extent = Vec3::from(cframe.matrix3.abs() * glam::Vec3A::from(scene::props::vec3(part, "Size", Vec3::ONE) * 0.5));
		let center = Vec3::from(cframe.translation);
		min = min.min(center - extent);
		max = max.max(center + extent);
	}
	if min.x > max.x {
		bail!("{effect}: no parts");
	}
	let center = (min + max) * 0.5;
	let half = ((max - min) * 0.5).max(Vec3::splat(3.0));
	Ok((center - half, center + half))
}

fn measure(spec: &EffectSpec, moments: &[f32]) -> Result<Measured> {
	let mut bundle = Bundle::open(&spec.bundle)?;
	let effect = bundle.take(&spec.effect, HOME)?;
	let play = attacks::auto_play(&bundle, &effect, spec.moving, 0.0)?;
	let playback = Playback { actions: play.actions, motion: play.motion, ..Default::default() };
	let times: Vec<f32> = moments.iter().map(|moment| moment + play.first_burst).collect();
	let bytes = bundle.model.to_rbxm()?;
	let (low, high, drawn) = match attacks::drawn_bounds(&bytes, &playback, &times) {
		Ok((low, high)) => (low, high, true),
		Err(_) => {
			let (low, high) = rest_bounds(&bundle, &effect)?;
			(low, high, false)
		}
	};
	Ok(Measured { low: (low - HOME).to_array(), high: (high - HOME).to_array(), drawn, first_burst: play.first_burst, moving: play.moving })
}

/// A key for an effect in the measurement cache.
fn effect_key(spec: &EffectSpec) -> String {
	format!("{}|{}|{:?}", spec.bundle, spec.effect, spec.moving)
}

/// Every effect's measurement, cached in `<out>/effects.json` (a rerun or a resumed sweep measures
/// only new effects); the measuring threads are few, each holding one bundle.
fn measure_all(effects: &[EffectSpec], moments: &[f32], out: &Path) -> Result<HashMap<String, Result<Measured, String>>> {
	let path = out.join("effects.json");
	let mut known: HashMap<String, Result<Measured, String>> = std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
	let missing: Vec<&EffectSpec> = effects.iter().filter(|spec| !known.contains_key(&effect_key(spec))).collect();
	if !missing.is_empty() {
		eprintln!("ror: vfx-sweep: measuring {} effects", missing.len());
		let pool = rayon::ThreadPoolBuilder::new().num_threads(6).build()?;
		let results: Vec<(String, Result<Measured, String>)> = pool.install(|| missing.par_iter().map(|spec| (effect_key(spec), measure(spec, moments).map_err(|error| format!("{error:#}")))).collect());
		known.extend(results);
		output::write_atomic(&path, serde_json::to_string(&known)?.as_bytes())?;
	}
	Ok(known)
}

/// Effects batched for grids: those that fly apart from the rest, by size, `per` to a batch, a batch
/// never spanning more than 2.5 times in size (a small effect beside a large one would draw tiny).
fn plan(spec: &Spec, out: &Path) -> Result<Vec<BatchSpec>> {
	let mut batches = spec.batches.clone();
	if spec.effects.is_empty() {
		return Ok(batches);
	}
	let measured = measure_all(&spec.effects, &spec.moments, out)?;
	let mut sized: Vec<(bool, f32, &EffectSpec)> = Vec::new();
	let mut failed = Vec::new();
	for effect in &spec.effects {
		match measured.get(&effect_key(effect)) {
			Some(Ok(measure)) => sized.push((measure.moving, measure.size(), effect)),
			Some(Err(error)) => failed.push(serde_json::json!({ "bundle": effect.bundle, "effect": effect.effect, "error": error })),
			None => {}
		}
	}
	output::write_atomic(&out.join("unmeasured.json"), serde_json::to_string_pretty(&failed)?.as_bytes())?;
	sized.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
	let mut current: Vec<(bool, f32, &EffectSpec)> = Vec::new();
	let mut groups = Vec::new();
	for item in sized {
		if let Some(first) = current.first()
			&& (current.len() >= spec.per.max(1) || first.0 != item.0 || item.1 > first.1.max(1.0) * 2.5)
		{
			groups.push(std::mem::take(&mut current));
		}
		current.push(item);
	}
	if !current.is_empty() {
		groups.push(current);
	}
	let mut grids = Vec::new();
	for group in groups {
		let ids: Vec<String> = group.iter().map(|(_, _, effect)| effect_key(effect)).collect();
		let hash = ids.join("\n").bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3));
		grids.push(BatchSpec { name: format!("b{:08x}", hash as u32), effects: group.into_iter().map(|(_, _, effect)| effect.clone()).collect(), grid: true, camera: None, view: spec.view.clone(), lighting: None, ground: None });
	}
	// in the order of their names (hashes): any share of a sweep run so far is a fair sample of it
	grids.sort_by(|a, b| a.name.cmp(&b.name));
	batches.extend(grids);
	Ok(batches)
}

/// The lighting of a sweep: `default`, `night` (ClockTime 0, no stars, sun or moon), `dark` (no light),
/// or a place file's Lighting; `+retro` adds the ColorGradingEffect's Retro tonemapper. Terrain's
/// clouds drift in Studio and ror does not draw them, so they are left out (as `ror compare` does).
fn lighting(name: Option<&str>) -> Result<Environment> {
	let text = name.unwrap_or("default").trim();
	let (base, retro) = match text.strip_suffix("+retro") {
		Some(base) => (base.trim(), true),
		None => (text, false),
	};
	let mut environment = match base {
		"" | "default" => Environment::default(),
		"dark" => super::dark(),
		"night" => Environment { clock_time: 0.0, sky: Some(Sky { star_count: 0, celestial_bodies_shown: false, ..Sky::default() }), ..Environment::default() },
		place => {
			// a place is read once (a large one is 30 MB)
			static PLACES: OnceLock<Mutex<HashMap<String, Environment>>> = OnceLock::new();
			let places = PLACES.get_or_init(|| Mutex::new(HashMap::new()));
			let known = places.lock().ok().and_then(|places| places.get(place).cloned());
			match known {
				Some(environment) => environment,
				None => {
					let document = scene::load::read_file(Path::new(place)).with_context(|| format!("vfx-sweep: the lighting place {place}"))?;
					let environment = scene::extract::environment(&document).with_context(|| format!("{place} has no Lighting"))?;
					if let Ok(mut places) = places.lock() {
						places.insert(place.to_string(), environment.clone());
					}
					environment
				}
			}
		}
	};
	environment.clouds = None;
	if retro {
		environment.effects.retain(|effect| !matches!(effect, PostEffect::ColorGrading { .. }));
		environment.effects.push(PostEffect::ColorGrading { tonemapper: 1 });
	}
	Ok(environment)
}

/// A batch's effect in its model.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cell {
	bundle: String,
	effect: String,
	/// Its path in the model.
	path: String,
	at: [f32; 3],
	/// What ror's playback draws of it (world), or its parts' bounds when that is nothing.
	low: [f32; 3],
	high: [f32; 3],
	drawn: bool,
	moving: bool,
	/// When its play starts and its first burst comes, seconds into the batch's timeline.
	start: f32,
	first_burst: f32,
	/// Its share of the image (x0, y0, x1, y1): its grid cell, or its bounds projected.
	rect: [u32; 4],
	/// Its bounds projected.
	bounds_rect: [u32; 4],
}

/// A batch made ready: its model, timeline, camera, lighting and moments.
struct Batch {
	model: Vec<u8>,
	playback: Playback,
	cells: Vec<Cell>,
	camera: Camera,
	/// The lighting's name (`lighting`) and what it is.
	lighting: Option<String>,
	environment: Environment,
	moments: Vec<f32>,
}

/// Where a world point shows in the image, if in front of the camera.
fn project(camera: &Camera, point: Vec3) -> Option<[f32; 2]> {
	let clip = camera.view_projection(VIEW.0 as f32 / VIEW.1 as f32) * point.extend(1.0);
	if clip.w <= 0.0 {
		return None;
	}
	let ndc = clip.truncate() / clip.w;
	Some([(ndc.x * 0.5 + 0.5) * VIEW.0 as f32, (0.5 - ndc.y * 0.5) * VIEW.1 as f32])
}

/// The image rectangle of points, clipped to the image (all of it when one is behind the camera).
fn rect_of(camera: &Camera, points: &[Vec3]) -> [u32; 4] {
	let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
	for point in points {
		let Some([x, y]) = project(camera, *point) else {
			return [0, 0, VIEW.0, VIEW.1];
		};
		(x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
	}
	let clamp = |value: f32, limit: u32| value.clamp(0.0, limit as f32) as u32;
	[clamp(x0.floor(), VIEW.0), clamp(y0.floor(), VIEW.1), clamp(x1.ceil(), VIEW.0), clamp(y1.ceil(), VIEW.1)]
}

fn corners(low: Vec3, high: Vec3) -> Vec<Vec3> {
	(0..8).map(|index| Vec3::new(if index & 1 == 0 { low.x } else { high.x }, if index & 2 == 0 { low.y } else { high.y }, if index & 4 == 0 { low.z } else { high.z })).collect()
}

/// The camera looking at `target` from `yaw` / `pitch` degrees, `distance` away.
fn orbit(target: Vec3, yaw: f32, pitch: f32, distance: f32, fov: f32) -> Camera {
	let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
	let direction = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()).normalize();
	CameraSpec::LookAt { eye: (target + direction * distance).to_array(), target: target.to_array(), fov }.place(None, VIEW.0, VIEW.1)
}

/// Builds a batch: its effects taken from their bundles into one model where the layout puts them,
/// each played as the generic run plays it, their first bursts together (each a `delay` later).
fn build(spec: &Spec, batch: &BatchSpec, measured: &[Measured]) -> Result<Batch> {
	let view = batch.view.clone().or_else(|| spec.view.clone()).unwrap_or_default();
	let (yaw, pitch, fov, padding) = (view.yaw.unwrap_or(35.0), view.pitch.unwrap_or(20.0), view.fov.unwrap_or(50.0), view.padding.unwrap_or(0.08));
	let aspect = VIEW.0 as f32 / VIEW.1 as f32;
	let count = batch.effects.len();
	let center = Vec3::new(0.0, 4.0, 0.0);
	let (columns, rows) = match count {
		0 | 1 => (1, 1),
		2 => (2, 1),
		3 | 4 => (2, 2),
		5 | 6 => (3, 2),
		_ => (3, 3),
	};
	// grid cells in the plane facing the camera, as wide and tall as the widest and tallest effect's
	// bounds show from there, the grid shaped as the view
	let probe = orbit(center, yaw, pitch, 1.0, fov);
	let (right, up) = (Vec3::from(probe.world.matrix3.x_axis), Vec3::from(probe.world.matrix3.y_axis));
	let shown = |measure: &Measured, axis: Vec3| -> f32 { ((Vec3::from(measure.high) - Vec3::from(measure.low)) * 0.5).dot(axis.abs()) * 2.0 * 1.05 };
	let mut cell_width = measured.iter().map(|measure| shown(measure, right)).fold(1.0f32, f32::max);
	let mut cell_height = measured.iter().map(|measure| shown(measure, up)).fold(1.0f32, f32::max);
	if columns as f32 * cell_width < aspect * rows as f32 * cell_height {
		cell_width = aspect * rows as f32 * cell_height / columns as f32;
	} else {
		cell_height = columns as f32 * cell_width / (aspect * rows as f32);
	}
	let cell_center = |index: usize| -> Vec3 {
		let (column, row) = (index % columns, index / columns);
		center + right * ((column as f32 - (columns as f32 - 1.0) * 0.5) * cell_width) + up * (((rows as f32 - 1.0) * 0.5 - row as f32) * cell_height)
	};
	// the distance that fits every effect's bounds in its cell, as an orbit fits a box's corners
	let grid_camera = {
		let direction = (probe.eye - center).normalize();
		let tan_y = (fov.to_radians() * 0.5).tan() * (1.0 - padding);
		let tan_x = tan_y * aspect;
		let mut needed: f32 = 0.5;
		for (index, measure) in measured.iter().enumerate() {
			let middle = (Vec3::from(measure.low) + Vec3::from(measure.high)) * 0.5;
			for corner in corners(Vec3::from(measure.low), Vec3::from(measure.high)) {
				let offset = cell_center(index) + corner - middle - center;
				let along = offset.dot(direction);
				needed = needed.max(offset.dot(right).abs() / tan_x + along).max(offset.dot(up).abs() / tan_y + along);
			}
		}
		orbit(center, yaw, pitch, needed * view.scale.unwrap_or(1.0), fov)
	};
	let first = measured.iter().zip(&batch.effects).map(|(measure, effect)| measure.first_burst - effect.delay).fold(0.0f32, f32::max);
	let mut model: Option<Bundle> = None;
	let mut cells = Vec::new();
	let mut actions = Vec::new();
	let mut motion = Vec::new();
	for (index, (effect, measure)) in batch.effects.iter().zip(measured).enumerate() {
		let middle = (Vec3::from(measure.low) + Vec3::from(measure.high)) * 0.5;
		let mut at = if batch.grid { cell_center(index) - middle } else { effect.at.map(Vec3::from).unwrap_or(HOME) };
		let mut source = Bundle::open(&effect.bundle)?;
		if effect.keep_height {
			// as high above the ground as it was made; an effect made far above or below its place's
			// ground (a cloud anchor's, at hundreds of studs) stands where the generic run puts one
			let height = attacks::center(&source.model.dom, attacks::find(&source.model.dom, &effect.effect)?).y;
			at.y += if (-5.0..=40.0).contains(&height) { height } else { HOME.y };
		}
		let host = model.get_or_insert_with(|| Bundle { model: ModelBuilder::new(ROOT) });
		let path = host.adopt(&mut source, &effect.effect, &format!("C{index}"), at)?;
		if effect.only.is_some() || effect.strip.is_some() {
			let doomed = doomed(&host.model.dom, attacks::find(&host.model.dom, &path)?, effect);
			for referent in doomed {
				if host.model.dom.get_by_ref(referent).is_some() {
					host.model.dom.destroy(referent);
				}
			}
		}
		let root = attacks::find(&host.model.dom, &path)?;
		apply_sets(&mut host.model, root, effect)?;
		let start = first - measure.first_burst + effect.delay;
		let play = attacks::auto_play(host, &path, effect.moving, start)?;
		actions.extend(play.actions);
		motion.extend(play.motion);
		let (low, high) = (Vec3::from(measure.low) + at, Vec3::from(measure.high) + at);
		cells.push(Cell {
			bundle: effect.bundle.clone(),
			effect: effect.effect.clone(),
			path,
			at: at.to_array(),
			low: low.to_array(),
			high: high.to_array(),
			drawn: measure.drawn,
			moving: play.moving,
			start,
			first_burst: start + measure.first_burst,
			rect: [0; 4],
			bounds_rect: [0; 4],
		});
	}
	let mut model = model.context("vfx-sweep: a batch without effects")?.model;
	if let Some(color) = batch.ground {
		model.part(&crate::scene::build::PartSpec { name: GROUND.into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -1.0, 0.0)), size: Vec3::new(2048.0, 2.0, 2048.0), color, material: 272, ..Default::default() });
	}
	let camera = if batch.grid {
		grid_camera
	} else if let Some(camera) = &batch.camera {
		camera.place(None, VIEW.0, VIEW.1)
	} else {
		let (low, high) = cells.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(low, high), cell| (low.min(Vec3::from(cell.low)), high.max(Vec3::from(cell.high))));
		let fitted = CameraSpec::Orbit { yaw, pitch, distance: None, padding, target: None, fov }.place(Some((low, high)), VIEW.0, VIEW.1);
		let target = (low + high) * 0.5;
		orbit(target, yaw, pitch, fitted.eye.distance(target) * view.scale.unwrap_or(1.0), fov)
	};
	for (index, cell) in cells.iter_mut().enumerate() {
		cell.bounds_rect = rect_of(&camera, &corners(Vec3::from(cell.low), Vec3::from(cell.high)));
		cell.rect = if batch.grid {
			let middle = cell_center(index);
			rect_of(&camera, &[middle - right * cell_width * 0.5 - up * cell_height * 0.5, middle + right * cell_width * 0.5 + up * cell_height * 0.5])
		} else {
			cell.bounds_rect
		};
	}
	let moments = spec.moments.iter().map(|moment| ((first + moment) * 1000.0).round() / 1000.0).collect();
	let name = batch.lighting.clone().or_else(|| spec.lighting.clone());
	let environment = lighting(name.as_deref())?;
	Ok(Batch { model: model.to_rbxm()?, playback: Playback { actions, motion, ..Default::default() }, cells, camera, lighting: name, environment, moments })
}

/// A batch as it was played: its model, playback and camera from its directory (`ROR_RERENDER`: ror's
/// side drawn again exactly where Studio's frames were captured, whatever later builds measure).
fn played(spec: &Spec, directory: &Path) -> Result<Batch> {
	let model = std::fs::read(directory.join("model.rbxm")).context("ROR_RERENDER: the batch's model.rbxm")?;
	let playback = timeline::stored_playback(&directory.join("playback.json"))?;
	let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(directory.join("camera.json"))?)?;
	let camera: CameraSpec = serde_json::from_value(record["camera"].clone())?;
	let moments: Vec<f32> = serde_json::from_value(record["moments"].clone())?;
	let name = record["lighting"].as_str().map(str::to_string).or_else(|| spec.lighting.clone());
	let environment = lighting(name.as_deref())?;
	Ok(Batch { model, playback, cells: Vec::new(), camera: camera.place(None, VIEW.0, VIEW.1), lighting: name, environment, moments })
}

/// How long Studio has after the warm-up play before the captured one (as timeline.rs).
const WARM_SETTLE_MS: u64 = 1000;

/// Before the warm-up play every emitter of the batch's model emits a few particles that stay on
/// screen, frozen part way into their life, this long: Studio streams a texture's full resolution only
/// for what it draws a while, and particles living 0.14-0.2 s do not stay long enough in one play
/// (an effect's balls, drawn from a blurred copy of their flipbook sheets after one
/// warm-up play, the same white star as ror's once loaded).
const PRELOAD_SECONDS: f32 = 1.5;

/// The preload (`PRELOAD_SECONDS`): every ParticleEmitter of the uploaded model (`RorCapture`, before
/// the runner clones it) emits 3, is frozen at a third of its shortest life (at most 0.15 s), then
/// cleared, its Enabled and TimeScale as they were.
fn preload_script() -> String {
	format!(
		r#"local current = workspace:FindFirstChild("RorCapture")
if not current then return "no model" end
_G.__ror_preload = {{ done = false }}
task.spawn(function()
	local emitters = {{}}
	for _, instance in current:GetDescendants() do
		if instance:IsA("ParticleEmitter") then
			table.insert(emitters, {{ instance, instance.TimeScale, instance.Enabled }})
			instance.Enabled = false
			instance.TimeScale = 1
			instance:Emit(3)
			task.delay(math.min(instance.Lifetime.Min / 3, 0.15), function() instance.TimeScale = 0 end)
		end
	end
	task.wait({PRELOAD_SECONDS})
	for _, entry in emitters do
		entry[1]:Clear()
		entry[1].TimeScale = entry[2]
		entry[1].Enabled = entry[3]
	end
	_G.__ror_preload.done = true
end)
return "started""#
	)
}

/// How long the asset check waits for the capture Studio's ContentProvider.
const ASSET_SECONDS: f32 = 30.0;

/// The asset check: ContentProvider:PreloadAsync over each cell of the uploaded model (`RorCapture`),
/// every content id it reports anything but Success for, as "<id> <status>" by the cell's name. The
/// capture account cannot fetch some assets that ror draws (one round: 8 of an effect's 16 emitter
/// textures), and an effect missing them in Studio cannot be compared.
fn asset_script() -> String {
	format!(
		r#"local current = workspace:FindFirstChild("RorCapture")
if not current then return "no model" end
_G.__ror_assets = {{ done = false, failed = {{}} }}
task.spawn(function()
	local provider = game:GetService("ContentProvider")
	for _, model in current:GetChildren() do
		for _, cell in model:GetChildren() do
			if cell.Name ~= {GROUND:?} then
				local failed = {{}}
				provider:PreloadAsync({{ cell }}, function(content, status)
					if status ~= Enum.AssetFetchStatus.Success then
						table.insert(failed, content .. " " .. status.Name)
					end
				end)
				_G.__ror_assets.failed[cell.Name] = failed
			end
		end
	end
	_G.__ror_assets.done = true
end)
return "started""#
	)
}

/// Runs the asset check on the model in the capture Studio: the failures by cell name (`assets.json`).
fn asset_failures(studio: &mut CaptureStudio) -> Result<serde_json::Value> {
	let result = studio.luau(&asset_script()).context("checking the batch's assets")?;
	if !result.contains("started") {
		bail!("the asset check did not start: {result}");
	}
	let started = Instant::now();
	while started.elapsed() < Duration::from_secs_f32(ASSET_SECONDS) {
		std::thread::sleep(Duration::from_millis(250));
		let result = studio.luau("if not (_G.__ror_assets and _G.__ror_assets.done) then return 'running' end return game:GetService('HttpService'):JSONEncode(_G.__ror_assets.failed)")?;
		if !result.contains("running") {
			return serde_json::from_str(result.trim()).with_context(|| format!("the asset check's result: {result}"));
		}
	}
	bail!("the asset check did not finish in {ASSET_SECONDS} s")
}

/// Writes a batch's `assets.json`: its asset check's failures, or the check's own error (`error`).
fn check_assets(studio: &mut CaptureStudio, directory: &Path) {
	let found = asset_failures(studio).unwrap_or_else(|error| {
		eprintln!("ror: vfx-sweep {}: {error:#}", directory.display());
		serde_json::json!({ "error": format!("{error:#}") })
	});
	if let Err(error) = serde_json::to_string_pretty(&found).map_err(anyhow::Error::from).and_then(|text| output::write_atomic(&directory.join("assets.json"), text.as_bytes())) {
		eprintln!("ror: vfx-sweep {}: writing assets.json: {error:#}", directory.display());
	}
}

/// `ROR_SWEEP_ASSETS=1`: the asset check alone (`assets.json`) of every batch played so far, its model
/// uploaded again without a play or a draw.
fn assets_only(out: &Path, only: &[String]) -> Result<()> {
	let mut directories: Vec<std::path::PathBuf> = std::fs::read_dir(out)?.filter_map(|entry| entry.ok()).map(|entry| entry.path()).filter(|path| path.join("done.json").exists()).collect();
	directories.sort();
	let mut studio = CaptureStudio::connect()?;
	for directory in directories {
		let name = directory.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
		if !only.is_empty() && !only.contains(&name) {
			continue;
		}
		let started = Instant::now();
		let result = std::fs::read(directory.join("model.rbxm")).map_err(anyhow::Error::from).and_then(|bytes| {
			let mut session = CaptureSession::start_settled(&mut studio, &bytes, 0)?;
			let studio = session.studio.as_deref_mut().context("vfx-sweep: no Studio")?;
			check_assets(studio, &directory);
			Ok(())
		});
		match result {
			Ok(()) => eprintln!("ror: vfx-sweep {name}: assets checked in {:.1} s", started.elapsed().as_secs_f32()),
			Err(error) => eprintln!("ror: vfx-sweep {name}: {error:#}"),
		}
	}
	Ok(())
}

/// Batches drawn by one renderer before a fresh one replaces it: its caches keep every texture and
/// mesh of every batch otherwise (up to 4 GB of textures and 2 GB of meshes, and the material bind
/// groups that hold textures past their eviction; one sweep process grew to 6.8 GB of GPU memory
/// over 180 batches; with a fresh one every 6, up to 1.3 GB).
const BATCHES_PER_RENDERER: usize = 3;

/// Plays a batch's timeline in the capture Studio and keeps the frame nearest each moment: the
/// frames' pixels (the marker blanked) and times. With `warm`, it plays once uncaptured first.
fn capture(studio: &mut CaptureStudio, camera: &Camera, timeline: &Timeline, moments: &[f32], script: &str, warm: bool) -> Result<Vec<(Vec<u8>, Captured)>> {
	let (width, height) = VIEW;
	let marker = Marker::new(camera, width, height);
	timeline::session_camera(studio, camera)?;
	let window = studio.window()?;
	let scale = window.dpi as f32 / 96.0;
	let (logical_width, logical_height) = studio.viewport()?;
	let viewport = crate::studio::capture::find_viewport(window.hwnd, (logical_width * scale).round() as i32, (logical_height * scale).round() as i32).context("the capture Studio's viewport window")?;
	let wait_done = |studio: &mut CaptureStudio| -> Result<()> {
		let started = Instant::now();
		while started.elapsed() < Duration::from_secs_f32(timeline.duration + 3.0) {
			std::thread::sleep(Duration::from_millis(200));
			if studio.luau("return if _G.__ror_timeline and _G.__ror_timeline.done then 'done' else 'running'")?.contains("done") {
				break;
			}
		}
		Ok(())
	};
	if warm {
		if studio.luau(&preload_script()).context("preloading the particles' textures")?.contains("started") {
			let started = Instant::now();
			while started.elapsed() < Duration::from_secs_f32(PRELOAD_SECONDS + 3.0) {
				std::thread::sleep(Duration::from_millis(200));
				if studio.luau("return if _G.__ror_preload and _G.__ror_preload.done then 'done' else 'running'")?.contains("done") {
					break;
				}
			}
		}
		let result = studio.luau(script).context("starting the timeline (warm-up)")?;
		if !result.contains("started") {
			bail!("the timeline did not start: {result}");
		}
		wait_done(studio)?;
		std::thread::sleep(Duration::from_millis(WARM_SETTLE_MS));
	}
	let result = studio.luau(script).context("starting the timeline")?;
	if !result.contains("started") {
		bail!("the timeline did not start: {result}");
	}
	// frames as they come, the few nearest each moment kept by their index at the frame rate seen
	let start = Instant::now();
	let mut kept: Vec<Vec<(u32, Vec<u8>)>> = vec![Vec::new(); moments.len()];
	let mut seen = 0;
	let mut first: Option<(u32, Instant)> = None;
	let mut rate = 59.0f32;
	while start.elapsed() < Duration::from_secs_f32(timeline.duration + 1.5) {
		let (w, h, rgba) = crate::studio::capture::capture(viewport.hwnd)?;
		if (w, h) != (width, height) {
			bail!("captured {w}x{h}, expected {width}x{height}");
		}
		let Some(index) = marker.decode(&rgba, width) else {
			if seen > 0 && start.elapsed() > Duration::from_secs_f32(timeline.duration * 0.5 + 0.5) {
				break;
			}
			continue;
		};
		seen += 1;
		match first {
			None => first = Some((index, Instant::now())),
			Some((first_index, at)) if index > first_index + 10 => rate = (index - first_index) as f32 / at.elapsed().as_secs_f32().max(1e-3),
			_ => {}
		}
		for (slot, moment) in moments.iter().enumerate() {
			let wanted = (moment * rate).round() as i64;
			let reach = (6.0 * (rate / 59.0).max(1.0)).round() as i64;
			let frames = &mut kept[slot];
			if (index as i64 - wanted).abs() > reach || frames.iter().any(|(kept, _)| *kept == index) {
				continue;
			}
			frames.push((index, rgba.clone()));
			// at most 4 frames a moment in memory: the nearest
			if frames.len() > 4
				&& let Some(farthest) = (0..frames.len()).max_by_key(|&slot| (frames[slot].0 as i64 - wanted).abs())
			{
				frames.swap_remove(farthest);
			}
		}
	}
	wait_done(studio)?;
	let times_text = studio.luau("local timeline = _G.__ror_timeline local parts = {} for index, time in timeline.times do parts[index] = string.format('%.6f', time) end return (timeline.done and 'done ' or 'running ') .. table.concat(parts, ',')")?;
	let times: Vec<f32> = times_text.trim().split_once(' ').map(|(_, list)| list).unwrap_or_default().split(',').filter_map(|value| value.trim().parse().ok()).collect();
	let mut frames = Vec::new();
	for (frames_kept, moment) in kept.into_iter().zip(moments) {
		let best = frames_kept.into_iter().filter(|(index, _)| (*index as usize) < times.len()).min_by(|a, b| (times[a.0 as usize] - moment).abs().total_cmp(&(times[b.0 as usize] - moment).abs()));
		let Some((index, mut rgba)) = best else {
			bail!("no frame of the timeline near {moment} s was captured ({seen} marked frames, {} frame times)", times.len());
		};
		timeline::blank(&mut rgba, width, marker.rect);
		frames.push((rgba, Captured { time: times[index as usize], frames: times[..=index as usize].to_vec() }));
	}
	Ok(frames)
}

/// The seed change of ror's `k`th other draw of random particles.
fn reseed(scene: &mut scene::Scene, k: u32) {
	for emitter in &mut scene.particles {
		emitter.seed ^= 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64);
	}
}

/// Plays and draws one batch into `directory`.
fn run_batch(renderer: &Renderer, studio: Option<&mut CaptureStudio>, spec: &Spec, batch: &Batch, directory: &Path) -> Result<()> {
	let bytes = batch.model.as_slice();
	if studio.is_some() {
		output::write_atomic(&directory.join("model.rbxm"), bytes)?;
		output::write_atomic(&directory.join("cells.json"), serde_json::to_string_pretty(&batch.cells)?.as_bytes())?;
		output::write_atomic(&directory.join("playback.json"), serde_json::to_string_pretty(&batch.playback)?.as_bytes())?;
		output::write_atomic(&directory.join("camera.json"), serde_json::to_string_pretty(&serde_json::json!({ "camera": batch.camera.spec(), "moments": batch.moments, "lighting": batch.lighting }))?.as_bytes())?;
	}
	let (width, height) = VIEW;
	let last = batch.moments.iter().copied().fold(0.0f32, f32::max);
	let timeline = Timeline { playback: Playback { time: last, ..batch.playback.clone() }, duration: last + 0.3 };
	let labels: Vec<String> = spec.moments.iter().map(|moment| format!("t{moment}")).collect();
	let mut materials_2022 = true;
	let frames: Vec<(Vec<u8>, Captured)> = match studio {
		Some(studio) => {
			let document = scene::load::read_bytes(bytes)?;
			let key = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3));
			let marker = Marker::new(&batch.camera, width, height);
			let script = timeline::runner(&timeline, &document, key, &marker, batch.environment.exposure_compensation)?;
			let mut session = CaptureSession::start_settled(studio, bytes, 500)?;
			materials_2022 = session.materials_2022;
			session.set_environment(&batch.environment)?;
			let studio = session.studio.as_deref_mut().context("vfx-sweep: no Studio")?;
			check_assets(studio, directory);
			// each side's view without the effects, what they are measured against: Studio's with the
			// model out of the world
			timeline::session_camera(studio, &batch.camera)?;
			studio.luau(&format!("local moved = {{}}
for _, model in workspace.RorCapture:GetChildren() do
	for _, child in model:GetChildren() do
		if child.Name ~= {GROUND:?} then
			table.insert(moved, {{child, child.Parent}})
			child.Parent = game:GetService(\"ReplicatedStorage\")
		end
	end
end
_G.__ror_sky_moved = moved
return #moved"))?;
			std::thread::sleep(Duration::from_millis(700));
			let (_, _, mut sky) = studio.capture_viewport()?;
			studio.luau("for _, item in _G.__ror_sky_moved do item[1].Parent = item[2] end _G.__ror_sky_moved = nil return 0")?;
			timeline::blank(&mut sky, width, Marker::new(&batch.camera, width, height).rect);
			output::write_png(&directory.join("studio-sky.png"), width, height, &sky, true)?;
			let frames = capture(studio, &batch.camera, &timeline, &batch.moments, &script, true)?;
			// Studio's play without the effects: the parts where they are at each moment, the sky
			if spec.bare {
				studio.luau("for _, instance in _G.__ror_pristine:GetDescendants() do if instance:IsA(\"ParticleEmitter\") or instance:IsA(\"Trail\") or instance:IsA(\"Beam\") then instance:Destroy() end end return 0")?;
				let bare = capture(studio, &batch.camera, &timeline, &batch.moments, &script, false)?;
				for (label, (rgba, _)) in labels.iter().zip(&bare) {
					output::write_png(&directory.join(label).join("studio-bare.png"), width, height, rgba, true)?;
				}
			}
			for (label, (rgba, captured)) in labels.iter().zip(&frames) {
				output::write_png(&directory.join(label).join("studio.png"), width, height, rgba, true)?;
				output::write_atomic(&directory.join(label).join("studio.json"), serde_json::to_string(captured)?.as_bytes())?;
			}
			frames
		}
		None => {
			let mut frames = Vec::new();
			for label in &labels {
				let (_, _, rgba) = output::decode_png(&directory.join(label).join("studio.png")).with_context(|| format!("ROR_RERENDER: {}", directory.join(label).display()))?;
				let captured: Captured = serde_json::from_str(&std::fs::read_to_string(directory.join(label).join("studio.json"))?)?;
				frames.push((rgba, captured));
			}
			frames
		}
	};
	let request = ViewRequest { width, height, camera: batch.camera.spec(), background: Background::Sky, ..Default::default() };
	let marker = timeline::marker_rect(&batch.camera, width, height);
	{
		let ground = scene::extract::extract(&scene::load::read_bytes(bytes)?).parts.into_iter().filter(|part| part.path == format!("{ROOT}.{GROUND}")).collect();
		let empty = scene::Scene { parts: ground, environment: batch.environment.clone(), ..Default::default() };
		let (images, _) = renderer.render(&empty, std::slice::from_ref(&request))?;
		let mut image = images.into_iter().next().context("no image")?;
		timeline::blank(&mut image.rgba, width, marker);
		output::write_png(&directory.join("ror-sky.png"), width, height, &image.rgba, true)?;
	}
	let mut drawn = Vec::new();
	for (label, (_, captured)) in labels.iter().zip(&frames) {
		let mut scene = timeline::scene(bytes, &timeline, captured)?;
		scene.environment = batch.environment.clone();
		scene.materials.use_2022 = materials_2022;
		let render = |scene: &scene::Scene, name: &str| -> Result<()> {
			let (images, stats) = renderer.render(scene, std::slice::from_ref(&request))?;
			for missing in &stats.missing {
				eprintln!("ror: vfx-sweep {}/{label}: missing {missing}", directory.display());
			}
			let mut image = images.into_iter().next().context("no image")?;
			timeline::blank(&mut image.rgba, width, marker);
			output::write_png(&directory.join(label).join(name), width, height, &image.rgba, true)
		};
		render(&scene, "ror.png")?;
		for k in 1..=spec.seeds {
			let mut other = scene.clone();
			reseed(&mut other, k);
			render(&other, &format!("ror-s{k}.png"))?;
		}
		let mut bare = scene.clone();
		bare.particles.clear();
		bare.trails.clear();
		bare.beams.clear();
		render(&bare, "parts.png")?;
		drawn.push(serde_json::json!({ "label": label, "time": captured.time, "frames": captured.frames.len(), "emitters": scene.particles.len(), "particles": scene.particles.iter().map(|emitter| crate::render::particles::simulate(emitter).len()).sum::<usize>(), "trails": scene.trails.len(), "beams": scene.beams.len() }));
	}
	output::write_atomic(&directory.join("done.json"), serde_json::to_string_pretty(&serde_json::json!({ "runs": drawn, "materials2022": materials_2022 }))?.as_bytes())?;
	Ok(())
}

/// Runs a sweep: each batch not done yet (or every one again for `ROR_RERENDER=1`, ror's side alone).
pub fn run(renderer: &Renderer, spec: &Spec, out: &Path) -> Result<()> {
	std::fs::create_dir_all(out)?;
	// `ROR_SWEEP_BATCHES=<name>,...`: only those batches
	let only: Vec<String> = std::env::var("ROR_SWEEP_BATCHES").map(|list| list.split(',').map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).collect()).unwrap_or_default();
	// a renderer of the sweep's own, replaced every few batches (`BATCHES_PER_RENDERER`)
	let fresh = || Renderer::new(renderer.gpu.clone(), renderer.assets.clone());
	if std::env::var_os("ROR_RERENDER").is_some() {
		return rerender(&fresh, spec, out, &only);
	}
	if std::env::var_os("ROR_SWEEP_ASSETS").is_some() {
		return assets_only(out, &only);
	}
	let batches = plan(spec, out)?;
	let measured = if spec.effects.is_empty() && batches.is_empty() { HashMap::new() } else { measure_all(&batches.iter().flat_map(|batch| batch.effects.clone()).collect::<Vec<_>>(), &spec.moments, out)? };
	let mut studio = Some(CaptureStudio::connect()?);
	let mut failures = 0;
	let mut drawing: Option<Renderer> = None;
	let mut drawn = 0;
	for (number, batch) in batches.iter().enumerate() {
		if !only.is_empty() && !only.contains(&batch.name) {
			continue;
		}
		let directory = out.join(&batch.name);
		if directory.join("done.json").exists() {
			continue;
		}
		if drawn % BATCHES_PER_RENDERER == 0 {
			// the old one's GPU memory goes before the new one takes any
			drop(drawing.take());
			drawing = Some(fresh()?);
		}
		drawn += 1;
		let renderer = drawing.as_ref().context("vfx-sweep: no renderer")?;
		let started = Instant::now();
		let result = (|| -> Result<()> {
			let measures = batch
				.effects
				.iter()
				.map(|effect| match measured.get(&effect_key(effect)) {
					Some(Ok(measure)) => Ok(measure.clone()),
					Some(Err(error)) => bail!("{}: {error}", effect.effect),
					None => bail!("{}: not measured", effect.effect),
				})
				.collect::<Result<Vec<_>>>()?;
			let built = build(spec, batch, &measures)?;
			run_batch(renderer, studio.as_mut(), spec, &built, &directory)
		})();
		match result {
			Ok(()) => {
				failures = 0;
				let _ = std::fs::remove_file(directory.join("failed.json"));
				eprintln!("ror: vfx-sweep {} ({}/{}): {} effects in {:.1} s", batch.name, number + 1, batches.len(), batch.effects.len(), started.elapsed().as_secs_f32());
			}
			Err(error) => {
				eprintln!("ror: vfx-sweep {}: {error:#}", batch.name);
				output::write_atomic(&directory.join("failed.json"), serde_json::to_string_pretty(&serde_json::json!({ "error": format!("{error:#}"), "effects": batch.effects }))?.as_bytes())?;
				failures += 1;
				if failures >= 3 {
					bail!("vfx-sweep: three batches failed in a row (the last: {error:#})");
				}
			}
		}
	}
	Ok(())
}

/// `ROR_RERENDER=1`: ror's side of every batch played so far (each directory with a `done.json`),
/// from the model, playback, camera and lighting it was played with.
fn rerender(fresh: &dyn Fn() -> Result<Renderer>, spec: &Spec, out: &Path, only: &[String]) -> Result<()> {
	let mut directories: Vec<std::path::PathBuf> = std::fs::read_dir(out)?.filter_map(|entry| entry.ok()).map(|entry| entry.path()).filter(|path| path.join("done.json").exists()).collect();
	directories.sort();
	let mut drawing: Option<Renderer> = None;
	let mut drawn = 0;
	for directory in directories {
		let name = directory.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
		if !only.is_empty() && !only.contains(&name) {
			continue;
		}
		if drawn % BATCHES_PER_RENDERER == 0 {
			// the old one's GPU memory goes before the new one takes any
			drop(drawing.take());
			drawing = Some(fresh()?);
		}
		drawn += 1;
		let renderer = drawing.as_ref().context("vfx-sweep: no renderer")?;
		let started = Instant::now();
		match played(spec, &directory).and_then(|batch| run_batch(renderer, None, spec, &batch, &directory)) {
			Ok(()) => eprintln!("ror: vfx-sweep {name}: drawn again in {:.1} s", started.elapsed().as_secs_f32()),
			Err(error) => eprintln!("ror: vfx-sweep {name}: {error:#}"),
		}
	}
	Ok(())
}
