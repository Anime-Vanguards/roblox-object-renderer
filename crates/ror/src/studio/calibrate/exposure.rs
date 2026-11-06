//! Long exposures (notes/render.md "Playback", "Long exposures"): an effect averaged over its timeline,
//! as a camera's open shutter sums the light. Studio's particles are random, so a frame of an effect
//! cannot match ror's pixel for pixel even where ror's rules are right; averaged over many plays and
//! every frame of each, chance cancels and what stays is systematic: where the effect puts its light,
//! how much, for how long and in what colour.
//!
//! `exposure[(<option>=<value>;...)]:<timeline experiment>` takes the experiment's first run (its
//! playback, camera and lighting; e.g. `vfx-attack:<bundle>|auto:<effect path>`)
//! and plays it in the capture Studio `plays` times from 0 to `end`, capturing every frame it can with
//! its index (the timeline marker); ror draws the playback at each of those frames' times with Studio's
//! frame times, with a random seed of its own per play. Each side's frames are averaged in linear light
//! (the tone curve inverted) per time window, the plays alternating between two halves (Studio's two
//! halves against each other are the noise floor), and so is the scene without the effect (Studio:
//! plays with its ParticleEmitters, Trails and Beams removed; ror: its render without them at every
//! frame). Options: `plays` (default [`PLAYS`]), `end` (seconds; default where ror's playback stops
//! drawing particles and trails, at most [`END_CAP`]), `windows` (the windows' edges in seconds,
//! comma-separated), `keep` / `drop` (ParticleEmitters, Trails and Beams by Name or class, or by the
//! dotted path their parent's ends with: only those stay / those go), `bare` (Studio's plays without the effect, 2), `warm` (plays before the counted
//! ones, so Studio's textures are in at full resolution, 1) and `tag` (a suffix for the
//! experiment's name). `ROR_RERENDER=1` draws ror's side again against the stored Studio averages.
//! tools/analysis/exposure_metrics.py measures them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use rbx_dom_weak::types::Ref;
use serde::{Deserialize, Serialize};

use super::timeline::{self, Captured, Marker, Timeline};
use super::{Experiment, Run};
use crate::output;
use crate::render::{Background, Renderer, ViewRequest};
use crate::scene;
use crate::studio::CaptureStudio;
use crate::studio::compare::CaptureSession;

/// Plays per exposure by default, two halves of 16: two Studio exposures of 8 plays each differ in
/// what the effect adds by 1.6 (a slash) and 3.3 (a sphere attack) of 255 RMS over the whole
/// exposure, of 16 plays 1.0 and 2.5 (notes/render.md "Long exposures").
const PLAYS: u32 = 32;

/// The longest default exposure: effects still drawing then (emitters running from the file, or a long
/// tail) are cut there.
const END_CAP: f32 = 2.5;

// Studio's default tone curve (shaders/resolve.wgsl `tonemap`, tools/analysis/tone.py; notes/render.md
// "Colour pipeline and tone curve"): the rational fit up to x = 2.1313, above it a shoulder
// approaching its ceiling as a power of the distance, past 1 (255 of 255) from x = 12.67.
const TONE_TOE: f32 = 0.00329;
const TONE_SCALE: f32 = 1.0098;
const TONE_SHOULDER_X: f32 = 2.1313;
const TONE_SHOULDER_START: f32 = 0.795199;
const TONE_CEILING: f32 = 1.00999;
const TONE_SHOULDER_K: f32 = 0.32012;
const TONE_SHOULDER_POWER: f32 = 2.07968;

fn tone(x: f32) -> f32 {
	let x = x.max(0.0);
	if x > TONE_SHOULDER_X {
		return TONE_CEILING - (TONE_CEILING - TONE_SHOULDER_START) * (1.0 + TONE_SHOULDER_K * (x - TONE_SHOULDER_X)).powf(-TONE_SHOULDER_POWER);
	}
	let y = x * TONE_SCALE;
	(y * (2.725891 * y + TONE_TOE)) / (y * (2.307040 * y + 1.951868) + 1.0)
}

/// The displayed value (0-1) of a tone curve input: its square root, clipped at 1.
fn display(x: f32) -> f32 {
	tone(x).sqrt().min(1.0)
}

/// The tone curve's input a displayed value (0-1) shows; 1 (255, every input from 10.7 up) reads as
/// where the curve reaches 1, x = 12.67 (tone.py's `radiance(255)`).
fn linear(shown: f64) -> f32 {
	let t = (shown * shown).min(1.0);
	if t > TONE_SHOULDER_START as f64 {
		let (start, ceiling) = (TONE_SHOULDER_START as f64, TONE_CEILING as f64);
		let distance = ((ceiling - t) / (ceiling - start)).powf(-1.0 / TONE_SHOULDER_POWER as f64) - 1.0;
		return (TONE_SHOULDER_X as f64 + distance / TONE_SHOULDER_K as f64) as f32;
	}
	// T = y (a y + toe) / (y (b y + c) + 1): (a - T b) y² + (toe - T c) y - T = 0
	let (a, b, c, toe) = (2.725891f64, 2.307040f64, 1.951868f64, TONE_TOE as f64);
	let (qa, qb) = (a - t * b, toe - t * c);
	let y = (-qb + (qb * qb + 4.0 * qa * t).sqrt()) / (2.0 * qa);
	(y / TONE_SCALE as f64) as f32
}

/// Every display level's tone curve input.
pub fn levels() -> [f32; 256] {
	std::array::from_fn(|level| linear(level as f64 / 255.0))
}

#[derive(Clone, Debug)]
pub struct Exposure {
	/// The inner experiment's playback (its time the end) and the end.
	pub timeline: Timeline,
	pub plays: u32,
	pub bare_plays: u32,
	/// Plays before the counted ones (their textures in at full resolution).
	pub warm_plays: u32,
	/// Play k counts in group k mod `groups` (2: the halves A and B).
	pub groups: u32,
	/// The windows' edges in seconds, increasing, between 0 and the end.
	pub edges: Vec<f32>,
	/// When ror's playback first and last draws particles or trails.
	pub span: (f32, f32),
	pub inner: String,
}

fn exposures() -> &'static Mutex<HashMap<String, Exposure>> {
	static EXPOSURES: OnceLock<Mutex<HashMap<String, Exposure>>> = OnceLock::new();
	EXPOSURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The exposure of an experiment, if it is one.
pub fn get(experiment: &str) -> Option<Exposure> {
	exposures().lock().ok()?.get(experiment).cloned()
}

/// The scene ror draws for `playback` at its moment (60 frames a second).
fn scene_at(bytes: &[u8], playback: &scene::playback::Playback) -> Result<scene::Scene> {
	let mut document = scene::load::read_bytes(bytes)?;
	let resolved = playback.resolve(&document)?;
	resolved.apply_state(&mut document)?;
	let mut scene = scene::extract::extract(&document);
	resolved.apply_history(&document, &mut scene);
	Ok(scene)
}

/// When ror's playback first and last shows its effect, sampled every 1/30 s up to [`END_CAP`]: a
/// trail with points, or particles amounting to 2 % of their most (opacity times squared size, summed;
/// a slash keeps faint specks to 2.4 s that its averages do not show past 1.7 s). Beams and parts
/// stay drawn: they do not end an effect.
fn drawn_span(bytes: &[u8], playback: &scene::playback::Playback) -> Result<(f32, f32)> {
	let mut samples: Vec<(f32, f32, bool)> = Vec::new();
	for step in 0..=(END_CAP * 30.0) as u32 {
		let time = step as f32 / 30.0;
		let scene = scene_at(bytes, &scene::playback::Playback { time, frames: None, ..playback.clone() })?;
		let amount: f32 = scene.particles.iter().flat_map(|emitter| crate::render::particles::simulate(emitter)).map(|particle| particle.opacity.clamp(0.0, 1.0) * particle.size * particle.size).sum();
		let trails = scene.trails.iter().any(|trail| trail.points.len() >= 2);
		samples.push((time, amount, trails));
	}
	let most = samples.iter().map(|&(_, amount, _)| amount).fold(0.0f32, f32::max);
	let shown: Vec<f32> = samples.iter().filter(|&&(_, amount, trails)| trails || (amount > 0.0 && amount >= 0.02 * most)).map(|&(time, _, _)| time).collect();
	match (shown.first(), shown.last()) {
		(Some(&first), Some(&last)) => Ok((first, last)),
		_ => bail!("exposure: the effect draws no particles or trails"),
	}
}

/// The windows' default edges: 0.1, 0.25, 0.5, 0.8, 1.2 and 1.7 s after the effect first draws, those
/// at least 0.05 s inside the exposure.
fn default_edges(first: f32, end: f32) -> Vec<f32> {
	[0.1f32, 0.25, 0.5, 0.8, 1.2, 1.7].iter().map(|offset| ((first + offset) * 1000.0).round() / 1000.0).filter(|&edge| edge > 0.05 && edge < end - 0.05).collect()
}

/// A path or list as an experiment name's part: lower-case letters and digits, other runs as one dash.
fn slug(text: &str) -> String {
	let mut label = String::new();
	for c in text.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }) {
		if c != '-' || !label.ends_with('-') {
			label.push(c);
		}
	}
	label.trim_matches('-').to_string()
}

/// `exposure[(<option>=<value>;...)]:<timeline experiment>` (the module's comment).
pub fn experiment(spec: &str) -> Result<Experiment> {
	let usage = "exposure[(plays=<n>;end=<s>;windows=<s>,...;keep=<names>;drop=<names>;bare=<n>;warm=<n>;groups=<n>;tag=<label>)]:<timeline experiment>";
	let rest = spec.strip_prefix("exposure").context(usage)?;
	let (options, inner_spec) = match rest.strip_prefix('(') {
		Some(rest) => {
			let (options, inner) = rest.split_once("):").context(usage)?;
			(options, inner)
		}
		None => ("", rest.strip_prefix(':').context(usage)?),
	};
	let mut options: HashMap<&str, &str> = options.split(';').filter(|option| !option.trim().is_empty()).map(|option| option.split_once('=').map(|(key, value)| (key.trim(), value.trim())).with_context(|| format!("exposure: {option:?} is not <option>=<value>"))).collect::<Result<_>>()?;
	let number = |options: &mut HashMap<&str, &str>, key: &str| -> Result<Option<f32>> { options.remove(key).map(|value| value.parse::<f32>().with_context(|| format!("exposure: {key}={value}"))).transpose() };
	let plays = number(&mut options, "plays")?.map_or(PLAYS, |plays| plays.round().max(2.0) as u32);
	let bare_plays = number(&mut options, "bare")?.map_or(2, |plays| plays.round().max(0.0) as u32);
	let warm_plays = number(&mut options, "warm")?.map_or(1, |plays| plays.round().max(1.0) as u32);
	let groups = number(&mut options, "groups")?.map_or(2, |groups| groups.round().clamp(2.0, 8.0) as u32);
	let end = number(&mut options, "end")?;
	let edges = options.remove("windows").map(|list| list.split(',').map(|edge| edge.trim().parse::<f32>().with_context(|| format!("exposure: the window edge {edge:?}"))).collect::<Result<Vec<_>>>()).transpose()?;
	let names = |list: Option<&str>| -> Vec<String> { list.map(|list| list.split(',').map(|name| name.trim().to_string()).filter(|name| !name.is_empty()).collect()).unwrap_or_default() };
	let keep = names(options.remove("keep"));
	let drop = names(options.remove("drop"));
	let tag = options.remove("tag").map(slug);
	if let Some(unknown) = options.keys().next() {
		bail!("exposure: unknown option {unknown:?} ({usage})");
	}
	let mut inner = super::experiment(inner_spec)?;
	let first_run = inner.runs.first().context("exposure: the experiment has no runs")?;
	let run = Run { label: "exposure".into(), environment: first_run.environment.clone(), camera: first_run.camera.clone() };
	let inner_timeline = timeline::get(&inner.name, &first_run.label).with_context(|| format!("exposure: {} is not a timeline experiment", inner.name))?;
	// only the effects kept (or without those dropped): which part of an effect draws what
	if !keep.is_empty() || !drop.is_empty() {
		let model = &mut inner.model;
		let mut doomed: Vec<Ref> = Vec::new();
		// each instance with its parent's dotted path from the model
		let mut stack = vec![(model.model, String::new())];
		while let Some((referent, parent)) = stack.pop() {
			let Some(instance) = model.dom.get_by_ref(referent) else {
				continue;
			};
			let here = if parent.is_empty() { instance.name.clone() } else { format!("{parent}.{}", instance.name) };
			stack.extend(instance.children().iter().map(|&child| (child, here.clone())));
			if !matches!(instance.class.as_str(), "ParticleEmitter" | "Trail" | "Beam") {
				continue;
			}
			// a Name or class, or a dotted path its parent's ends with (`Blackhole.FX`: the emitters on
			// that attachment, where an effect's names repeat from attachment to attachment)
			let named = |list: &[String]| list.iter().any(|name| *name == instance.name || *name == instance.class.as_str() || (name.contains('.') && (parent == *name || parent.ends_with(&format!(".{name}")))));
			if (!keep.is_empty() && !named(&keep)) || named(&drop) {
				doomed.push(referent);
			}
		}
		for referent in doomed {
			model.dom.destroy(referent);
		}
	}
	let bytes = inner.model.to_rbxm()?;
	let span = drawn_span(&bytes, &inner_timeline.playback)?;
	let end = end.unwrap_or_else(|| ((span.1 + 0.1).min(END_CAP).max(span.0 + 0.3) * 100.0).round() / 100.0);
	let edges = edges.unwrap_or_else(|| default_edges(span.0, end));
	if edges.windows(2).any(|pair| pair[1] <= pair[0]) || edges.iter().any(|&edge| edge <= 0.0 || edge >= end) {
		bail!("exposure: the window edges {edges:?} must increase between 0 and the end ({end} s)");
	}
	let mut name = format!("exposure-{}", inner.name);
	if !keep.is_empty() {
		name.push_str(&format!("-keep-{}", slug(&keep.join("-"))));
	}
	if !drop.is_empty() {
		name.push_str(&format!("-drop-{}", slug(&drop.join("-"))));
	}
	if let Some(tag) = tag {
		name.push_str(&format!("-{tag}"));
	}
	let playback = scene::playback::Playback { time: end, frames: None, ..inner_timeline.playback };
	eprintln!("ror: {name}: draws from {:.3} to {:.3} s; exposure to {end} s, windows at {edges:?}, {plays} plays", span.0, span.1);
	let exposure = Exposure { timeline: Timeline { playback, duration: end }, plays, bare_plays, warm_plays, groups, edges, span, inner: inner.name.clone() };
	if let Ok(mut exposures) = exposures().lock() {
		exposures.insert(name.clone(), exposure);
	}
	Ok(Experiment { name, model: inner.model, probes: Vec::new(), runs: vec![run] })
}

/// Linear light summed per time window: (sum of each pixel's RGB, frames).
pub struct Sums {
	pixels: usize,
	windows: Vec<(Vec<f32>, u32)>,
}

impl Sums {
	pub fn new(windows: usize, pixels: usize) -> Self {
		Self { pixels, windows: (0..windows).map(|_| (Vec::new(), 0)).collect() }
	}

	pub fn add(&mut self, window: usize, rgba: &[u8], levels: &[f32; 256]) {
		let (sum, count) = &mut self.windows[window];
		if sum.is_empty() {
			*sum = vec![0.0; self.pixels * 3];
		}
		// in parallel: the capture loop waits for it
		sum.par_chunks_mut(3 * 4096).zip(rgba.par_chunks(4 * 4096)).for_each(|(totals, pixels)| {
			for (total, pixel) in totals.chunks_exact_mut(3).zip(pixels.chunks_exact(4)) {
				total[0] += levels[pixel[0] as usize];
				total[1] += levels[pixel[1] as usize];
				total[2] += levels[pixel[2] as usize];
			}
		});
		*count += 1;
	}

	/// Each window's mean, as a 16-bit image of the displayed values (`<prefix>-w<window>.png`), and
	/// its frame count.
	pub fn write(&self, dir: &Path, prefix: &str, width: u32, height: u32) -> Result<Vec<u32>> {
		for (window, (sum, count)) in self.windows.iter().enumerate() {
			if *count == 0 {
				continue;
			}
			let mean: Vec<f32> = sum.iter().map(|total| total / *count as f32).collect();
			write_png16(&dir.join(format!("{prefix}-w{window}.png")), width, height, &mean)?;
		}
		Ok(self.windows.iter().map(|(_, count)| *count).collect())
	}
}

/// A mean in linear light as a 16-bit RGB PNG of its displayed values.
fn write_png16(path: &Path, width: u32, height: u32, linear: &[f32]) -> Result<()> {
	let mut data = Vec::with_capacity(linear.len() * 2);
	for &x in linear {
		data.extend_from_slice(&((display(x) * 65535.0).round().clamp(0.0, 65535.0) as u16).to_be_bytes());
	}
	let mut bytes = Vec::with_capacity(data.len() / 2);
	{
		let mut encoder = png::Encoder::new(&mut bytes, width, height);
		encoder.set_color(png::ColorType::Rgb);
		encoder.set_depth(png::BitDepth::Sixteen);
		encoder.set_compression(png::Compression::Fast);
		encoder.set_filter(png::Filter::Up);
		let mut writer = encoder.write_header().context("PNG header")?;
		writer.write_image_data(&data).context("PNG data")?;
	}
	output::write_atomic(path, &bytes)
}

/// A [`write_png16`] image back in linear light.
fn read_png16(path: &Path) -> Result<(u32, u32, Vec<f32>)> {
	let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
	let decoder = png::Decoder::new(std::io::BufReader::new(file));
	let mut reader = decoder.read_info().context("PNG header")?;
	let mut buffer = vec![0; reader.output_buffer_size().context("PNG size")?];
	let info = reader.next_frame(&mut buffer).context("PNG data")?;
	if info.bit_depth != png::BitDepth::Sixteen || info.color_type != png::ColorType::Rgb {
		bail!("{} is not a 16-bit RGB exposure", path.display());
	}
	let values = buffer[..info.buffer_size()].chunks_exact(2).map(|pair| linear(u16::from_be_bytes([pair[0], pair[1]]) as f64 / 65535.0)).collect();
	Ok((info.width, info.height, values))
}

/// A mean in linear light as displayed 8-bit RGBA.
fn displayed(linear: &[f32]) -> Vec<u8> {
	linear.chunks_exact(3).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]].map(|x| (display(x) * 255.0).round() as u8).into_iter().chain([255u8])).collect()
}

/// One play of the exposure: its seed for ror, the frames of Studio's that were captured (their
/// indices and the window each counts in) and Studio's frame times.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Play {
	seed: u64,
	indices: Vec<u32>,
	windows: Vec<u8>,
	times: Vec<f32>,
}

/// The plays of an exposure, kept beside its averages (`plays.json`) so that ror's side can be drawn
/// again without Studio. Play k counts in group k mod `groups`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
	groups: u32,
	plays: Vec<Play>,
	bare: Vec<Play>,
	/// The model's assets the capture Studio could not fetch, each with the instances using it (None:
	/// not probed, or the Studio fetched none of them).
	#[serde(default)]
	studio_failures: Option<Vec<(String, Vec<String>)>>,
}

/// The model's asset URIs (its Content properties: textures, meshes, SurfaceAppearance maps) with the
/// instances using each (class and Name), Studio's built-in `rbxasset://` content aside.
fn asset_uris(document: &scene::load::Document) -> Vec<(String, Vec<String>)> {
	let mut uris: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
	for instance in document.dom.descendants() {
		for value in instance.properties.values() {
			let uri = match value {
				rbx_dom_weak::types::Variant::ContentId(value) => value.as_str().trim().to_string(),
				rbx_dom_weak::types::Variant::Content(value) => value.as_uri().unwrap_or_default().trim().to_string(),
				_ => continue,
			};
			if uri.is_empty() || uri.starts_with("rbxasset://") {
				continue;
			}
			let user = format!("{} {}", instance.class, instance.name);
			let users = uris.entry(uri).or_default();
			if !users.contains(&user) {
				users.push(user);
			}
		}
	}
	uris.into_iter().collect()
}

/// The assets the capture Studio cannot fetch (ContentProvider's status), each with the instances
/// using it: an emitter, trail, beam or decal whose texture fails draws nothing in Studio, where ror,
/// fetching with its own account, draws it, a capture artefact rather than a rule. Asked right after
/// the model is uploaded and preloaded (`CaptureSession::start`): a Studio that has not loaded an
/// asset answers from its fetch alone, and one whose session cannot fetch fails everything it has
/// not cached (one session's Studio at 14:00 failed all 108 of an effect's assets, which the captures had
/// drawn), so a probe that fails every asset records nothing (None).
#[cfg(windows)]
fn studio_failures(studio: &mut CaptureStudio, uris: &[(String, Vec<String>)]) -> Result<Option<Vec<(String, Vec<String>)>>> {
	if uris.is_empty() {
		return Ok(Some(Vec::new()));
	}
	let list = uris.iter().map(|(uri, _)| format!("{uri:?}")).collect::<Vec<_>>().join(", ");
	let text = studio.luau(&format!(
		"local failed = {{}}\ngame:GetService(\"ContentProvider\"):PreloadAsync({{{list}}}, function(id, status)\n\tif status ~= Enum.AssetFetchStatus.Success then table.insert(failed, id) end\nend)\nreturn table.concat(failed, \"\\n\")"
	))?;
	let digits = |uri: &str| uri.chars().filter(char::is_ascii_digit).collect::<String>();
	let failures: Vec<(String, Vec<String>)> = text
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.map(|failed| {
			let users = uris.iter().find(|(uri, _)| uri == failed || (!digits(failed).is_empty() && digits(uri) == digits(failed))).map(|(_, users)| users.clone()).unwrap_or_default();
			(failed.to_string(), users)
		})
		.collect();
	if uris.len() > 1 && failures.len() >= uris.len() {
		return Ok(None);
	}
	Ok(Some(failures))
}

/// A group's letter in file names (a, b, c...).
fn letter(group: usize) -> char {
	(b'a' + group as u8) as char
}

/// Seconds the runner waits before its timeline starts (the copy's wait in the world).
const LEAD: f32 = 0.8;

/// The timeline runner, made to return at once: its timeline starts [`LEAD`] s later in a task of its
/// own, so that every frame from the first is captured.
fn deferred(script: &str) -> Result<String> {
	let wait = format!("task.wait({LEAD})\n");
	if script.matches(&wait).count() != 1 || !script.ends_with("\nreturn \"started\"") {
		bail!("exposure: the timeline runner has changed (its wait and return)");
	}
	let script = script.replacen(&wait, &format!("task.spawn(function()\n{wait}"), 1);
	Ok(format!("{}\nend)\nreturn \"started\"", script.trim_end_matches("\nreturn \"started\"")))
}

/// The runner without the effect: the fresh copy's ParticleEmitters, Trails and Beams removed.
fn bare(script: &str) -> Result<String> {
	let anchor = "root.Parent = workspace\n";
	if !script.contains(anchor) || script.contains("explosions") {
		bail!("exposure: no bare runner for this model (legacy effects)");
	}
	Ok(script.replacen(
		anchor,
		"root.Parent = workspace\nfor _, instance in root:GetDescendants() do\n\tif instance:IsA(\"ParticleEmitter\") or instance:IsA(\"Trail\") or instance:IsA(\"Beam\") then instance:Destroy() end\nend\n",
		1,
	))
}

/// A play's frames: the captured indices, each one's window, Studio's frame times, and how long the
/// runner's start, the capture and the times' read took (seconds).
struct Played {
	indices: Vec<u32>,
	windows: Vec<u8>,
	times: Vec<f32>,
	timings: [f32; 3],
}

/// Plays a runner once in the capture Studio, capturing every frame it can while the timeline marker
/// is lit: each new frame (its window and pixels, the marker blanked) goes to `frame` as it comes.
/// The window is the frame's time by the wall clock from the first marked frame: Studio's clock
/// (os.clock) runs with it whatever Studio's frame rate, which the capture itself lowers by up to a
/// fifth and unevenly (a slash: 47-58 frames a second from play to play), so frame indices would
/// not keep the windows' edges; the time is known exactly only after the play.
#[cfg(windows)]
fn play(studio: &mut CaptureStudio, viewport: isize, marker: &Marker, script: &str, size: (u32, u32), edges: &[f32], end: f32, mut frame: impl FnMut(usize, &[u8])) -> Result<Played> {
	let (width, height) = size;
	let started = Instant::now();
	let result = studio.luau(script).context("starting the exposure's timeline")?;
	if !result.contains("started") {
		bail!("the exposure's timeline did not start: {result}");
	}
	let call = started.elapsed().as_secs_f32();
	let mut indices: Vec<u32> = Vec::new();
	let mut windows: Vec<u8> = Vec::new();
	let mut first: Option<(u32, Instant)> = None;
	let mut last_seen = Instant::now();
	while started.elapsed() < Duration::from_secs_f32(end + LEAD + 4.0) {
		let (w, h, mut rgba) = crate::studio::capture::capture(viewport)?;
		if (w, h) != (width, height) {
			bail!("captured {w}x{h}, expected {width}x{height}");
		}
		let Some(index) = plated(marker, &rgba, width).then(|| marker.decode(&rgba, width)).flatten() else {
			if !indices.is_empty() && last_seen.elapsed() > Duration::from_millis(400) {
				break;
			}
			continue;
		};
		let now = Instant::now();
		last_seen = now;
		// frames in order from one of the first few (the capture starts before the timeline); a jump far
		// ahead would be a misread marker
		if indices.last().map_or(index > 30, |&last| index <= last || index > last + 40) {
			continue;
		}
		// the first marked frame is one of the first few: its time at 60 a second
		let (first_index, first_at) = *first.get_or_insert((index, now));
		let time = first_index as f32 / 60.0 + now.duration_since(first_at).as_secs_f32();
		let window = edges.iter().filter(|&&edge| time >= edge).count();
		indices.push(index);
		windows.push(window as u8);
		timeline::blank(&mut rgba, width, marker.rect);
		frame(window, &rgba);
	}
	let captured = started.elapsed().as_secs_f32() - call;
	let text = studio.luau("local timeline = _G.__ror_timeline local parts = {} for index, time in timeline.times do parts[index] = string.format('%.6f', time) end return (timeline.done and 'done ' or 'running ') .. table.concat(parts, ',')")?;
	let times: Vec<f32> = text.trim().split_once(' ').map(|(_, list)| list).unwrap_or_default().split(',').filter_map(|value| value.trim().parse().ok()).collect();
	if let Some(&beyond) = indices.iter().find(|&&index| index as usize >= times.len()) {
		bail!("exposure: frame {beyond} captured but Studio recorded {} frame times", times.len());
	}
	let read = started.elapsed().as_secs_f32() - call - captured;
	Ok(Played { indices, windows, times, timings: [call, captured, read] })
}

/// Whether a frame shows the marker's black plate (dark between every two tiles): the runner returns
/// before its marker is drawn, and a frame from before it shows the scenery in that corner, which the
/// tiles' test can read as lit (frame 2047 over a bright sky).
fn plated(marker: &Marker, rgba: &[u8], width: u32) -> bool {
	marker.tiles.windows(2).all(|pair| {
		let (x, y) = (((pair[0].0[0] + pair[1].0[0]) * 0.5) as i32, pair[0].0[1] as i32);
		(-1..=1).all(|dy| {
			(-1..=1).all(|dx| {
				let index = (((y + dy).max(0) as u32 * width + (x + dx).max(0) as u32) * 4) as usize;
				rgba.get(index..index + 3).is_some_and(|pixel| pixel.iter().all(|&value| value < 40))
			})
		})
	})
}

/// Studio's frames a second over a play.
fn rate(times: &[f32]) -> f32 {
	match (times.first(), times.last()) {
		(Some(first), Some(last)) if times.len() > 10 && last > first => (times.len() - 1) as f32 / (last - first),
		_ => 60.0,
	}
}

/// The frames of `plays` (those of one group, or all) counted per window.
fn counts(plays: &[Play], groups: u32, group: Option<usize>, windows: usize) -> Vec<u32> {
	let mut counts = vec![0u32; windows];
	for (index, play) in plays.iter().enumerate() {
		if group.is_some_and(|group| index % groups as usize != group) {
			continue;
		}
		for &window in &play.windows {
			counts[window as usize] += 1;
		}
	}
	counts
}

/// Runs an exposure (`calibrate::run` hands exposures here): Studio's plays (or, with
/// `ROR_RERENDER=1`, its stored averages and frame times), ror's drawing of every captured frame, the
/// averages per window and the whole exposure's images.
#[cfg(windows)]
pub fn run(renderer: &Renderer, experiment: Experiment, exposure: &Exposure, out: &Path) -> Result<()> {
	let run = experiment.runs.first().context("exposure: no run")?;
	let dir = out.join(&run.label);
	std::fs::create_dir_all(&dir)?;
	let rerender = std::env::var_os("ROR_RERENDER").is_some();
	let mut bytes = experiment.model.to_rbxm()?;
	let mut exposure = exposure.clone();
	// `ROR_RERENDER`: the model, playback, end and windows Studio played, kept beside its averages (a
	// later build's experiment could make them otherwise: an effect's beam ends 0.1 s sooner
	// in main 246e325's playback, one window fewer)
	if rerender {
		if let Ok(text) = std::fs::read_to_string(dir.join("exposure.json")) {
			let summary: serde_json::Value = serde_json::from_str(&text).context("the exposure's exposure.json")?;
			if let (Some(end), Some(edges)) = (summary["end"].as_f64(), summary["edges"].as_array()) {
				exposure.timeline.duration = end as f32;
				exposure.edges = edges.iter().filter_map(|edge| edge.as_f64()).map(|edge| edge as f32).collect();
			}
			exposure.warm_plays = summary["warmPlays"].as_u64().map_or(1, |plays| plays as u32);
		}
		if let Ok(stored) = std::fs::read(out.join("model.rbxm")) {
			if stored != bytes {
				eprintln!("ror: {}: drawing the model Studio played (this build makes another)", experiment.name);
			}
			bytes = stored;
		}
		if dir.join("playback.json").exists() {
			let stored = super::timeline::stored_playback(&dir.join("playback.json")).context("the exposure's playback.json")?;
			if stored != exposure.timeline.playback {
				eprintln!("ror: {}: drawing the playback Studio played (this build makes another)", experiment.name);
			}
			exposure.timeline.playback = stored;
		}
	} else {
		output::write_atomic(&out.join("model.rbxm"), &bytes)?;
		// the playback both sides play (examples/exposure_groups.rs draws ror's side by groups of emitters)
		output::write_atomic(&dir.join("playback.json"), serde_json::to_string_pretty(&exposure.timeline.playback)?.as_bytes())?;
	}
	let exposure = &exposure;
	let bounds = scene::extract::extract(&scene::load::read_bytes(&bytes)?).framing_bounds();
	let mut studio = if rerender { None } else { Some(CaptureStudio::connect()?) };
	let mut session = match studio.as_mut() {
		Some(studio) => CaptureSession::start(studio, &bytes)?,
		None => CaptureSession::offline(&bytes, ""),
	};
	session.set_environment(&run.environment)?;
	let (width, height) = (session.width, session.height);
	let pixels = (width * height) as usize;
	let camera = run.camera.place(bounds, width, height);
	let marker = Marker::new(&camera, width, height);
	let levels = levels();
	let windows = exposure.edges.len() + 1;
	let record_path = dir.join("plays.json");
	let record: Record = match session.studio.as_deref_mut() {
		None => serde_json::from_str(&std::fs::read_to_string(&record_path).context("ROR_RERENDER: the exposure's plays.json")?)?,
		Some(studio) => {
			let key = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3));
			let script = deferred(&timeline::runner(&exposure.timeline, &scene::load::read_bytes(&bytes)?, key, &marker, run.environment.exposure_compensation)?)?;
			let bare_script = if exposure.bare_plays > 0 { bare(&script)? } else { String::new() };
			let studio_failures = studio_failures(studio, &asset_uris(&scene::load::read_bytes(&bytes)?))?;
			match &studio_failures {
				None => eprintln!("ror: {}: the capture Studio fetched none of the model's assets (its session cannot fetch?)", experiment.name),
				Some(failures) if !failures.is_empty() => eprintln!("ror: {}: {} of the model's assets fail to load in the capture Studio ({})", experiment.name, failures.len(), failures.iter().flat_map(|(_, users)| users.iter().cloned()).collect::<Vec<_>>().join(", ")),
				Some(_) => {}
			}
			timeline::session_camera(studio, &camera)?;
			let window = studio.window()?;
			let scale = window.dpi as f32 / 96.0;
			let (logical_width, logical_height) = studio.viewport()?;
			let viewport = crate::studio::capture::find_viewport(window.hwnd, (logical_width * scale).round() as i32, (logical_height * scale).round() as i32).context("the capture Studio's viewport window")?.hwnd;
			let (end, edges) = (exposure.timeline.duration, exposure.edges.as_slice());
			let log = |label: &str, played: &Played| {
				// the windows by Studio's own frame times: how many frames the wall clock put in another
				let moved = played.indices.iter().zip(&played.windows).filter(|&(&index, &window)| edges.iter().filter(|&&edge| played.times[index as usize] >= edge).count() != window as usize).count();
				eprintln!(
					"ror: {}: {label}: {} of {} frames at {:.1} a second, {moved} in a neighbouring window by Studio's times; start {:.2} s, capture {:.2} s, times {:.2} s",
					experiment.name,
					played.indices.len(),
					played.times.len(),
					rate(&played.times),
					played.timings[0],
					played.timings[1],
					played.timings[2]
				);
			};
			// a warm-up play, captured as the others are, so that its textures are in at full resolution
			// (particle-streaming)
			for index in 0..exposure.warm_plays {
				let warm = play(studio, viewport, &marker, &script, (width, height), edges, end, |_, _| {})?;
				log(&format!("warm-up {index}"), &warm);
			}
			let mut sums: Vec<Sums> = (0..exposure.groups).map(|_| Sums::new(windows, pixels)).collect();
			let mut plays = Vec::new();
			for index in 0..exposure.plays {
				let group = &mut sums[(index % exposure.groups) as usize];
				let played = play(studio, viewport, &marker, &script, (width, height), edges, end, |window, rgba| group.add(window, rgba, &levels))?;
				log(&format!("play {index}"), &played);
				plays.push(Play { seed: (index as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03), indices: played.indices, windows: played.windows, times: played.times });
			}
			let mut bare_sums = Sums::new(windows, pixels);
			let mut bare_plays = Vec::new();
			for index in 0..exposure.bare_plays {
				let played = play(studio, viewport, &marker, &bare_script, (width, height), edges, end, |window, rgba| bare_sums.add(window, rgba, &levels))?;
				log(&format!("bare play {index}"), &played);
				bare_plays.push(Play { seed: 0, indices: played.indices, windows: played.windows, times: played.times });
			}
			for (group, sums) in sums.iter().enumerate() {
				sums.write(&dir, &format!("studio-{}", letter(group)), width, height)?;
			}
			bare_sums.write(&dir, "studio-bare", width, height)?;
			let record = Record { groups: exposure.groups, plays, bare: bare_plays, studio_failures };
			output::write_atomic(&record_path, serde_json::to_string(&record)?.as_bytes())?;
			record
		}
	};
	// ror: every captured frame at its time with Studio's frame times, a seed per play, in the window
	// Studio's frame counted in, and without the effect (particles, trails and beams)
	let started = Instant::now();
	let request = ViewRequest { width, height, camera: camera.spec(), background: Background::Sky, ..Default::default() };
	let mut sums: Vec<Sums> = (0..record.groups).map(|_| Sums::new(windows, pixels)).collect();
	let mut bare_sums = Sums::new(windows, pixels);
	let mut drawn = 0;
	for (index, play) in record.plays.iter().enumerate() {
		for (&frame, &window) in play.indices.iter().zip(&play.windows) {
			let captured = Captured { time: play.times[frame as usize], frames: play.times[..=frame as usize].to_vec() };
			let mut scene = timeline::scene(&bytes, &exposure.timeline, &captured)?;
			scene.environment = run.environment.clone();
			scene.materials.use_2022 = session.materials_2022;
			for emitter in &mut scene.particles {
				emitter.seed ^= play.seed;
			}
			let (images, _) = renderer.render(&scene, std::slice::from_ref(&request))?;
			let mut image = images.into_iter().next().context("no image")?;
			timeline::blank(&mut image.rgba, width, marker.rect);
			sums[index % record.groups as usize].add(window as usize, &image.rgba, &levels);
			scene.particles.clear();
			scene.trails.clear();
			scene.beams.clear();
			let (images, _) = renderer.render(&scene, std::slice::from_ref(&request))?;
			let mut image = images.into_iter().next().context("no image")?;
			timeline::blank(&mut image.rgba, width, marker.rect);
			bare_sums.add(window as usize, &image.rgba, &levels);
			drawn += 1;
		}
	}
	eprintln!("ror: {}: drew {drawn} frames twice in {:.1} s", experiment.name, started.elapsed().as_secs_f32());
	for (group, sums) in sums.iter().enumerate() {
		sums.write(&dir, &format!("ror-{}", letter(group)), width, height)?;
	}
	bare_sums.write(&dir, "ror-bare", width, height)?;
	drop(sums);
	drop(bare_sums);
	// the whole exposure: each window's mean weighed by its frames
	let combine = |files: &[(String, Vec<u32>)]| -> Result<Vec<f32>> {
		let mut sum = vec![0f32; pixels * 3];
		let mut total = 0u32;
		for (prefix, counts) in files {
			for (window, &frames) in counts.iter().enumerate() {
				if frames == 0 {
					continue;
				}
				let (_, _, mean) = read_png16(&dir.join(format!("{prefix}-w{window}.png")))?;
				for (sum, value) in sum.iter_mut().zip(&mean) {
					*sum += value * frames as f32;
				}
				total += frames;
			}
		}
		Ok(sum.into_iter().map(|value| value / total.max(1) as f32).collect())
	};
	let group_counts: Vec<Vec<u32>> = (0..record.groups as usize).map(|group| counts(&record.plays, record.groups, Some(group), windows)).collect();
	let side = |name: &str| -> Vec<(String, Vec<u32>)> { group_counts.iter().enumerate().map(|(group, counts)| (format!("{name}-{}", letter(group)), counts.clone())).collect() };
	let studio_mean = combine(&side("studio"))?;
	let ror_mean = combine(&side("ror"))?;
	crate::studio::compare::write_comparison(&dir, width, height, &displayed(&studio_mean), &displayed(&ror_mean), crate::studio::compare::derived_images())?;
	output::write_png(&dir.join("background.png"), width, height, &displayed(&combine(&[("ror-bare".into(), counts(&record.plays, record.groups, None, windows))])?), true)?;
	let studio_bare_counts = counts(&record.bare, 1, None, windows);
	if studio_bare_counts.iter().any(|&frames| frames > 0) {
		output::write_png(&dir.join("studio-bare.png"), width, height, &displayed(&combine(&[("studio-bare".into(), studio_bare_counts.clone())])?), true)?;
	}
	// what each window spans by Studio's frame times
	let spans: Vec<[f32; 2]> = (0..windows)
		.map(|window| {
			let times = record.plays.iter().flat_map(|play| play.indices.iter().zip(&play.windows).filter(|&(_, &frame_window)| frame_window as usize == window).map(|(&frame, _)| play.times[frame as usize]));
			times.fold([f32::INFINITY, f32::NEG_INFINITY], |[low, high], time| [low.min(time), high.max(time)])
		})
		.collect();
	let summary = serde_json::json!({
		"inner": exposure.inner,
		"camera": camera.cframe_components(),
		"fov": camera.fov,
		"end": exposure.timeline.duration,
		"edges": exposure.edges,
		"drawn": exposure.span,
		"windowTimes": spans,
		"groups": record.groups,
		"plays": record.plays.len(),
		"warmPlays": exposure.warm_plays,
		"studioFailures": record.studio_failures,
		"framesPerPlay": record.plays.iter().map(|play| play.indices.len()).collect::<Vec<_>>(),
		"playRates": record.plays.iter().map(|play| rate(&play.times)).collect::<Vec<_>>(),
		"counts": group_counts,
		"studioBare": studio_bare_counts,
		"white": linear(1.0),
	});
	output::write_atomic(&dir.join("exposure.json"), serde_json::to_string_pretty(&summary)?.as_bytes())?;
	output::write_atomic(&out.join("samples.json"), b"[]")?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn linear_inverts_the_tone_curve() {
		for x in [0.0f32, 0.001, 0.02, 0.3, 1.0, 2.0, 2.2, 4.0, 7.0, 10.0] {
			let back = linear(display(x) as f64);
			assert!((back - x).abs() < 1e-3 * x.max(1.0), "{x} -> {back}");
		}
		// a white surface under a Brightness-1 sun shows 184; 255 reads as where the curve reaches 1
		assert_eq!((display(1.0) * 255.0).round(), 184.0);
		assert!((linear(1.0) - 12.67).abs() < 0.01, "{}", linear(1.0));
		assert!((tone(linear(1.0)) - 1.0).abs() < 1e-4);
	}

	#[test]
	fn deferred_runner_returns_at_once() {
		let script = "local a = 1\ntask.wait(0.8)\nlocal b = 2\nreturn \"started\"";
		assert_eq!(deferred(script).unwrap(), "local a = 1\ntask.spawn(function()\ntask.wait(0.8)\nlocal b = 2\nend)\nreturn \"started\"");
	}

	#[test]
	fn frames_counted_per_group_and_window() {
		let play = |windows: Vec<u8>| Play { seed: 0, indices: (0..windows.len() as u32).collect(), windows, times: Vec::new() };
		let plays = [play(vec![0, 0, 1]), play(vec![1, 2]), play(vec![0])];
		assert_eq!(counts(&plays, 2, Some(0), 3), vec![3, 1, 0]);
		assert_eq!(counts(&plays, 2, Some(1), 3), vec![0, 1, 1]);
		assert_eq!(counts(&plays, 2, None, 3), vec![3, 2, 1]);
	}
}
