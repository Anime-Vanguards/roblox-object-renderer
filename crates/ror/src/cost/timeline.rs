//! Multi-frame timeline costing (`ror cost --duration ...` / `--times ...`): steps one loaded
//! document through a playback's moments, reusing decoded meshes and (when `--memory` is requested)
//! cached texture headers across frames, and writes `timeline.json` (`ror-cost-timeline/1`) plus a
//! concise `analysis.txt`.
//!
//! By default, the timeline populates `Playback::viewer` from `--camera-track` (or the fixed
//! camera) so emitters out of the camera's view stand still (neither emitting nor ageing, matching
//! Roblox's client rule). Pass `--all-emitters-running` for the worst case where every emitter runs
//! regardless of camera view; `timeline.json` and `analysis.txt` record which rule ran.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use glam::Vec3;
use rbx_dom_weak::types::Ref;
use serde::{Deserialize, Serialize};

use super::agent;
use super::attribute::{self, Owners, Work};
use super::ids::InstanceIds;
use super::memory::{self, MemoryCollector, MemoryReport, TextureCache};
use super::meshes::{MeshStats, Meshes};
use super::pixels;
use super::prices::{self, Preset, Price, Quality};
use super::{Analysis, CostOptions, CostReport, SceneCounts};
use crate::budget::GpuGate;
use crate::job::NamedView;
use crate::render::Renderer;
use crate::render::camera::CameraSpec;
use crate::scene::load::Document;
use crate::scene::{Playback, Scene};
use crate::video::CameraKey;

pub const TIMELINE_SCHEMA: &str = "ror-cost-timeline/1";
const MAX_TIMELINE_FRAMES: usize = 7200;
const TOP_INSTANCES: usize = 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimelineOptions {
	/// Start time in seconds (default 0).
	pub start: f32,
	/// Explicit duration in seconds (None when `auto_duration` or `times` is used).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration: Option<f32>,
	/// Derive duration from `Playback::natural_end` (`--duration auto`).
	#[serde(default)]
	pub auto_duration: bool,
	/// Sampling rate in frames per second (default 10).
	pub fps: f32,
	/// Explicit list of sample moments in seconds (`--times`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub times: Option<Vec<f32>>,
	/// Moving camera sampled at each frame moment (`--camera-track`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub camera_track: Option<Vec<CameraKey>>,
	/// Keep a leading engine-default `(0, 20, 20)` camera key at `t = 0` in `camera_track`.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub keep_default_camera: bool,
	/// Worst-case mode (`--all-emitters-running`): every emitter emits and ages even when out of
	/// the camera's view. Default `false` pauses emitters out of view (`Playback::viewer`),
	/// matching Roblox's client rule.
	#[serde(default)]
	pub all_emitters_running: bool,
}

/// Which offscreen emitter simulation rule ran for this timeline.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OffscreenEmittersReport {
	/// `"paused-out-of-view"` (default, matching Roblox client) or `"all-running"` (`--all-emitters-running`).
	pub rule: &'static str,
	/// Source of the viewer frustum: `"camera-track"`, `"fixed-camera"`, `"playback-viewer"`, or `"disabled-by-flag"`.
	pub source: &'static str,
	/// Plain-language summary of what ran.
	pub note: String,
}

/// Where the timeline's end time / sample window came from.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationSource {
	/// `"auto"`, `"explicit-duration"`, or `"explicit-times"`.
	pub mode: &'static str,
	/// Specific origin: `"actions"`, `"tweens"`, `"infinite-tween"`, `"motion"`,
	/// `"looping-emitters"`, `"looping-emitters-and-playback"`, `"fallback-empty"`,
	/// `"explicit-duration"`, or `"explicit-times"`.
	pub source: &'static str,
	/// Human-readable description printed in `analysis.txt`.
	pub detail: String,
	/// True when `--duration auto` found no playback events or looping emitters and used the 2 s fallback.
	pub fallback: bool,
	/// True when the playback contains an infinite-repeat tween (`repeatCount < 0`, capped to 1 cycle).
	pub has_infinite_tween: bool,
	/// Playback `prewarm` in seconds (`0` means file-enabled emitters start empty at `t=0`).
	pub prewarm: f64,
}

/// Climb vs steady-state breakdown when one or more continuous emitters are still running at the
/// end of the playback.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopingReport {
	/// Number of continuous emitters still enabled past the playback's last event.
	pub emitters_still_running: usize,
	/// Paths of continuous emitters still enabled at the end (capped to 20).
	pub emitter_paths: Vec<String>,
	/// True when file-enabled looping emitters start empty at `t=0` (`prewarm == 0`).
	pub file_enabled_start_empty: bool,
	/// Earliest time in seconds when all still-running emitters reach steady state.
	pub steady_state_at: f64,
	/// True when at least one sampled frame lies at or after `steady_state_at`.
	pub steady_state_reached: bool,
	/// Number of sampled frames during the initial climb (`t < steady_state_at`).
	pub climb_frames: usize,
	/// Number of sampled frames at steady state (`t >= steady_state_at`).
	pub steady_frames: usize,
	/// Summary over climb frames only (`None` when the run starts at/after `steady_state_at`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub climb: Option<TimelineSummary>,
	/// Summary over steady-state frames only (`None` when the run ended before `steady_state_at`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub steady_state: Option<TimelineSummary>,
	/// Non-empty warning when the run ended before steady state was reached.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub warning: Option<String>,
}

/// One budget check (either worst frame or steady/overall mean) against [`agent::Budget`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineBudgetCheck {
	pub draws: f64,
	pub triangles: f64,
	pub ms: f64,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub over: Vec<String>,
}

/// Budget verdict over a timeline: checked against both the worst sampled frame and the steady
/// (or climb-only / overall) mean.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineBudget {
	pub limits: agent::Budget,
	pub over: bool,
	pub frames_over: usize,
	pub frame_count: usize,
	pub worst_frame: TimelineBudgetCheck,
	/// Which mean was checked: `"steadyMean"`, `"climbMean"`, or `"mean"`.
	pub mean_kind: &'static str,
	pub mean: TimelineBudgetCheck,
}

/// Validates that `options` and `tl` contain no unsupported single-frame/cost-loop flags and that
/// explicit `--times` moments are finite, non-negative, and in non-decreasing order.
pub fn validate_options(options: &CostOptions, tl: &TimelineOptions, view_count: usize) -> Result<()> {
	if (tl.duration.is_some() || tl.auto_duration) && tl.times.as_deref().is_some_and(|ts| !ts.is_empty()) {
		bail!("--duration and --times cannot be combined; pass one or the other");
	}
	if let Some(ts) = &tl.times {
		if ts.is_empty() || ts.iter().any(|t| !t.is_finite() || *t < 0.0) {
			bail!("--times must be a non-empty list of finite non-negative seconds");
		}
		for pair in ts.windows(2) {
			if pair[1] < pair[0] {
				bail!("--times must be in non-decreasing order (got {} after {})", pair[1], pair[0]);
			}
		}
	}
	if options.edits.is_some() || options.look || options.export.is_some() {
		bail!("--edits (--look, --export) is not supported on timelines (--duration/--times); run single-moment cost without --duration/--times");
	}
	if options.items {
		bail!("--items is not supported on timelines (--duration/--times); run single-moment cost without --duration/--times");
	}
	if options.heat {
		bail!("--heat is not supported on timelines (--duration/--times); run single-moment cost without --duration/--times");
	}
	if options.grid.is_some() {
		bail!("--grid is not supported on timelines (--duration/--times); run single-moment cost without --duration/--times");
	}
	if options.all_emitters {
		bail!("--all-emitters (view fullest-list limit) is not supported on timelines (--duration/--times); pass --all-emitters-running for worst-case offscreen emitter simulation");
	}
	let has_track = tl.camera_track.as_deref().is_some_and(|t| !t.is_empty());
	if view_count > 1 && !has_track {
		bail!("multi-view options (--paths, --sweep, --cameras, or multiple --views/--camera-from views) are not supported on timelines (--duration/--times); pass a single camera or --camera-track");
	}
	Ok(())
}

/// One sampled moment along the timeline.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameCost {
	pub frame: usize,
	pub time: f64,
	pub draws: u64,
	pub triangles: u64,
	pub parts: u64,
	pub emitters: u64,
	pub particles: u64,
	pub particle_fill: f64,
	pub beams: u64,
	pub trails: u64,
	pub blended: f64,
	pub blended_viewports: f64,
	pub gpu_units: f64,
	pub cpu_ms: f64,
	/// Calibrated GPU ms, or `null` on `--preset desktop` where GPU ms is uncalibrated.
	pub gpu_ms: Option<f64>,
	pub ms: f64,
}

/// Distribution and integral statistics of one metric across all sampled frames.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSummary {
	pub mean: f64,
	pub peak: f64,
	pub peak_time: f64,
	pub peak_frame: usize,
	pub p50: f64,
	pub p95: f64,
	pub p99: f64,
	pub top1_pct_mean: f64,
	pub integral: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSummary {
	pub draws: MetricSummary,
	pub triangles: MetricSummary,
	pub particles: MetricSummary,
	pub particle_fill: MetricSummary,
	pub blended_viewports: MetricSummary,
	pub gpu_units: MetricSummary,
	pub cpu_ms: MetricSummary,
	/// `null` on `--preset desktop` where GPU ms is uncalibrated.
	pub gpu_ms: Option<MetricSummary>,
	pub ms: MetricSummary,
}

/// Deterministic summary of one costly instance across the timeline.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineInstance {
	pub id: String,
	pub class: String,
	pub name: String,
	pub frames: usize,
	pub mean_ms: f64,
	pub peak_ms: f64,
	pub peak_time: f64,
	pub peak_frame: usize,
	pub mean_gpu_units: f64,
	pub peak_gpu_units: f64,
	pub mean_draws: f64,
	pub peak_draws: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineReport {
	pub schema: &'static str,
	pub input: String,
	pub start: f64,
	pub duration: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fps: Option<f64>,
	pub frame_count: usize,
	pub preset: Preset,
	pub quality: Quality,
	pub technology: u32,
	pub prices: Vec<Price>,
	pub fidelity: serde_json::Value,
	pub unmodeled: Vec<serde_json::Value>,
	pub duration_source: DurationSource,
	pub offscreen_emitters: OffscreenEmittersReport,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub looping: Option<LoopingReport>,
	pub pixels_counted: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub memory: Option<MemoryReport>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub budget: Option<TimelineBudget>,
	pub summary: TimelineSummary,
	pub top_instances: Vec<TimelineInstance>,
	pub frames: Vec<FrameCost>,
}

fn round4(value: f64) -> f64 {
	(value * 1e4).round() / 1e4
}

fn round6(value: f64) -> f64 {
	(value * 1e6).round() / 1e6
}

/// The camera track's camera at `time`: the latest key at or before `time + 1e-4`, else the first
/// (matches `video.rs::track_camera`).
pub fn track_camera(track: &[CameraKey], time: f32) -> CameraSpec {
	let key = track.iter().filter(|key| key.at <= time + 1e-4).max_by(|a, b| a.at.total_cmp(&b.at)).unwrap_or(&track[0]);
	CameraSpec::CFrame { components: key.cframe, fov: key.fov, near: None }
}

/// Builds the rounded sample moments `(times, start, duration, fps_opt)` using a precomputed
/// [`crate::scene::playback::NaturalEnd`].
pub fn sample_times_with_info(opts: &TimelineOptions, info: &crate::scene::playback::NaturalEnd) -> (Vec<f64>, f64, f64, Option<f64>) {
	if let Some(explicit) = &opts.times
		&& !explicit.is_empty()
	{
		let times: Vec<f64> = explicit.iter().take(MAX_TIMELINE_FRAMES).map(|&t| round6(f64::from(t))).collect();
		let start = times[0];
		let end = times.iter().copied().fold(start, f64::max);
		return (times, start, round6((end - start).max(0.0)), None);
	}
	let fps = if opts.fps.is_finite() && opts.fps > 0.0 { f64::from(opts.fps) } else { 10.0 };
	let start = if opts.start.is_finite() && opts.start >= 0.0 { f64::from(opts.start) } else { 0.0 };
	let dur = if opts.auto_duration {
		let natural = f64::from(info.seconds);
		let min_span = if !info.looping_emitters.is_empty() { 1.0 } else { 1.0 / fps };
		(natural - start).max(min_span)
	} else {
		f64::from(opts.duration.unwrap_or(2.0)).max(0.0)
	};
	let count = ((dur * fps).round() as usize).clamp(1, MAX_TIMELINE_FRAMES);
	let times: Vec<f64> = (0..count).map(|i| round6(start + i as f64 / fps)).collect();
	(times, round6(start), round6(dur), Some(round6(fps)))
}

/// Builds the rounded sample moments `(times, start, duration, fps_opt)`.
pub fn sample_times(opts: &TimelineOptions, playback: &Playback, document: &Document) -> (Vec<f64>, f64, f64, Option<f64>) {
	let info = playback.natural_end_info(document);
	sample_times_with_info(opts, &info)
}

fn nearest_rank_percentile(sorted: &[f64], pct: f64) -> f64 {
	let n = sorted.len();
	if n == 0 {
		return 0.0;
	}
	let idx = ((pct / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1;
	sorted[idx]
}

fn top_1pct_mean(values: &[f64]) -> f64 {
	if values.is_empty() {
		return 0.0;
	}
	let mut desc = values.to_vec();
	desc.sort_by(|a, b| b.total_cmp(a));
	let k = ((0.01 * desc.len() as f64).ceil() as usize).max(1).min(desc.len());
	desc[..k].iter().sum::<f64>() / k as f64
}

/// Computes mean, peak (with earliest peakFrame/peakTime), nearest-rank p50/p95/p99, top-1% mean,
/// and time integral (`sum * dt` for uniform `fps`, interval-weighted sum for explicit `--times`).
pub fn summarize(series: &[f64], times: &[f64], uniform_dt: Option<f64>) -> MetricSummary {
	let n = series.len();
	if n == 0 {
		return MetricSummary {
			mean: 0.0,
			peak: 0.0,
			peak_time: 0.0,
			peak_frame: 0,
			p50: 0.0,
			p95: 0.0,
			p99: 0.0,
			top1_pct_mean: 0.0,
			integral: 0.0,
		};
	}
	let sum: f64 = series.iter().sum();
	let mean = sum / n as f64;
	let mut peak = series[0];
	let mut peak_frame = 0usize;
	for (i, &v) in series.iter().enumerate().skip(1) {
		if v > peak {
			peak = v;
			peak_frame = i;
		}
	}
	let peak_time = times.get(peak_frame).copied().unwrap_or(0.0);
	let mut sorted = series.to_vec();
	sorted.sort_by(|a, b| a.total_cmp(b));
	let p50 = nearest_rank_percentile(&sorted, 50.0);
	let p95 = nearest_rank_percentile(&sorted, 95.0);
	let p99 = nearest_rank_percentile(&sorted, 99.0);
	let top1 = top_1pct_mean(series);
	let integral = match uniform_dt {
		Some(dt) => sum * dt,
		None => {
			if n < 2 {
				0.0
			} else {
				let mut total = 0.0;
				for i in 0..n {
					let step = if i + 1 < n {
						(times[i + 1] - times[i]).max(0.0)
					} else {
						(times[i] - times[i - 1]).max(0.0)
					};
					total += series[i] * step;
				}
				total
			}
		}
	};
	MetricSummary {
		mean: round6(mean),
		peak: round6(peak),
		peak_time: round6(peak_time),
		peak_frame,
		p50: round6(p50),
		p95: round6(p95),
		p99: round6(p99),
		top1_pct_mean: round6(top1),
		integral: round6(integral),
	}
}

fn summarize_frames(frames: &[FrameCost], uniform_dt: Option<f64>, desktop: bool) -> TimelineSummary {
	let times: Vec<f64> = frames.iter().map(|f| f.time).collect();
	let draws_s: Vec<f64> = frames.iter().map(|f| f.draws as f64).collect();
	let tris_s: Vec<f64> = frames.iter().map(|f| f.triangles as f64).collect();
	let particles_s: Vec<f64> = frames.iter().map(|f| f.particles as f64).collect();
	let pfill_s: Vec<f64> = frames.iter().map(|f| f.particle_fill).collect();
	let bvp_s: Vec<f64> = frames.iter().map(|f| f.blended_viewports).collect();
	let gpuu_s: Vec<f64> = frames.iter().map(|f| f.gpu_units).collect();
	let cpu_s: Vec<f64> = frames.iter().map(|f| f.cpu_ms).collect();
	let ms_s: Vec<f64> = frames.iter().map(|f| f.ms).collect();
	let gpu_ms_summary = if desktop || frames.iter().any(|f| f.gpu_ms.is_none()) {
		None
	} else {
		let gpu_s: Vec<f64> = frames.iter().map(|f| f.gpu_ms.unwrap_or(0.0)).collect();
		let mut s = summarize(&gpu_s, &times, uniform_dt);
		s.peak_frame = frames.get(s.peak_frame).map_or(s.peak_frame, |f| f.frame);
		Some(s)
	};
	let remap = |mut s: MetricSummary| -> MetricSummary {
		if let Some(f) = frames.get(s.peak_frame) {
			s.peak_frame = f.frame;
		}
		s
	};
	TimelineSummary {
		draws: remap(summarize(&draws_s, &times, uniform_dt)),
		triangles: remap(summarize(&tris_s, &times, uniform_dt)),
		particles: remap(summarize(&particles_s, &times, uniform_dt)),
		particle_fill: remap(summarize(&pfill_s, &times, uniform_dt)),
		blended_viewports: remap(summarize(&bvp_s, &times, uniform_dt)),
		gpu_units: remap(summarize(&gpuu_s, &times, uniform_dt)),
		cpu_ms: remap(summarize(&cpu_s, &times, uniform_dt)),
		gpu_ms: gpu_ms_summary,
		ms: remap(summarize(&ms_s, &times, uniform_dt)),
	}
}

#[derive(Default)]
struct InstAcc {
	class: String,
	name: String,
	frames: usize,
	sum_ms: f64,
	peak_ms: f64,
	peak_time: f64,
	peak_frame: usize,
	sum_gpu_units: f64,
	peak_gpu_units: f64,
	sum_draws: f64,
	peak_draws: f64,
}

/// Runs multi-frame timeline costing on `document` (restoring it after every frame), writing
/// `timeline.json` and `analysis.txt` into `out` and returning a summary [`CostReport`].
#[allow(clippy::too_many_arguments)]
pub fn run(
	renderer: &Renderer,
	document: &mut Document,
	playback: Option<&Playback>,
	options: &CostOptions,
	tl: &TimelineOptions,
	views: &[NamedView],
	frame_box: Option<[[f32; 3]; 2]>,
	focus: Option<&str>,
	out: &Path,
	gpu: Option<&GpuGate>,
	input: &str,
	lent_lighting: bool,
	lend: &dyn Fn(&mut Scene) -> Result<()>,
) -> Result<CostReport> {
	validate_options(options, tl, views.len())?;
	let budget_spec = options.budget.as_deref().map(agent::Budget::parse).transpose()?;

	struct ExplainAcc {
		id: String,
		root: Ref,
		uid: Option<String>,
		class: Option<String>,
		frames: Vec<serde_json::Value>,
		sum: Work,
		peak_ms: f64,
		peak_gpu_units: f64,
		peak_draws: f64,
		peak_triangles: f64,
		peak_blended: f64,
		peak_time: f64,
		peak_frame: usize,
		leaves: BTreeMap<String, Work>,
	}
	let mut explain_accs: Vec<ExplainAcc> = Vec::new();
	if !options.explain.is_empty() {
		let base_ids = InstanceIds::new(document);
		for id in &options.explain {
			let root = base_ids.require(id)?;
			explain_accs.push(ExplainAcc {
				id: id.clone(),
				root,
				uid: base_ids.uid(root).map(str::to_string),
				class: base_ids.info(root).map(|info| info.1.clone()),
				frames: Vec::new(),
				sum: Work::default(),
				peak_ms: 0.0,
				peak_gpu_units: 0.0,
				peak_draws: 0.0,
				peak_triangles: 0.0,
				peak_blended: 0.0,
				peak_time: 0.0,
				peak_frame: 0,
				leaves: BTreeMap::new(),
			});
		}
	}

	let mut owned_playback = playback.cloned().unwrap_or_default();
	let natural_info = owned_playback.natural_end_info(document);
	let (times, start, duration, fps_opt) = sample_times_with_info(tl, &natural_info);
	let prewarm = if owned_playback.prewarm.is_finite() { round6(f64::from(owned_playback.prewarm.max(0.0))) } else { 0.0 };
	let duration_source = if tl.times.as_deref().is_some_and(|ts| !ts.is_empty()) {
		DurationSource {
			mode: "explicit-times",
			source: "explicit-times",
			detail: format!("explicit --times ({} moments)", times.len()),
			fallback: false,
			has_infinite_tween: natural_info.has_infinite_tween,
			prewarm,
		}
	} else if tl.auto_duration {
		DurationSource {
			mode: "auto",
			source: natural_info.source,
			detail: natural_info.detail.clone(),
			fallback: natural_info.fallback,
			has_infinite_tween: natural_info.has_infinite_tween,
			prewarm,
		}
	} else {
		DurationSource {
			mode: "explicit-duration",
			source: "explicit-duration",
			detail: format!("explicit --duration {duration:.2}s"),
			fallback: false,
			has_infinite_tween: natural_info.has_infinite_tween,
			prewarm,
		}
	};
	let (view_w, view_h) = views.first().map(|v| (v.view.width, v.view.height)).unwrap_or((1920, 1080));
	let dummy_cam = CameraSpec::LookAt { eye: [0.0, 0.0, 10.0], target: [0.0; 3], fov: 70.0 }.place(None, view_w.max(1), view_h.max(1));
	let preset = agent::preset_of(options, &[(dummy_cam, view_w, view_h)])?;
	let quality = prices::Quality::of_scale(options.quality(), options.client_quality, preset.phone);
	let desktop = preset.uses_desktop_pricing();
	let viewport_pixels = (preset.width.max(1) as f64) * (preset.height.max(1) as f64);
	let (analysis_w, analysis_h) = preset.analysis_size();
	let base_camera = views
		.first()
		.map(|v| v.view.camera.clone())
		.unwrap_or(CameraSpec::Orbit { yaw: 0.0, pitch: 20.0, distance: None, padding: 0.08, target: None, fov: 70.0 });
	let normalized_track = tl
		.camera_track
		.as_ref()
		.filter(|track| !track.is_empty())
		.map(|track| crate::video::normalize_camera_track(track.clone(), tl.keep_default_camera).0);
	let has_track = normalized_track.is_some();
	let viewer_aspect = preset.width.max(1) as f32 / preset.height.max(1) as f32;

	let offscreen_emitters = if tl.all_emitters_running {
		owned_playback.viewer.clear();
		OffscreenEmittersReport {
			rule: "all-running",
			source: "disabled-by-flag",
			note: "worst-case (--all-emitters-running): every emitter emits and ages even out of the camera's view; live Roblox clients pause emitters out of view".into(),
		}
	} else if !owned_playback.viewer.is_empty() {
		owned_playback.viewer.sort_by(|a, b| a.at.total_cmp(&b.at));
		OffscreenEmittersReport {
			rule: "paused-out-of-view",
			source: "playback-viewer",
			note: "emitters out of the playback viewer's frustum stand still (neither emit nor age, matching Roblox client; pass --all-emitters-running for worst case)".into(),
		}
	} else if let Some(track) = normalized_track.as_deref() {
		let mut keys: Vec<crate::scene::playback::ViewerKey> = track
			.iter()
			.map(|key| crate::scene::playback::ViewerKey { at: key.at, cframe: key.cframe, fov: key.fov, aspect: viewer_aspect })
			.collect();
		keys.sort_by(|a, b| a.at.total_cmp(&b.at));
		owned_playback.viewer = keys;
		OffscreenEmittersReport {
			rule: "paused-out-of-view",
			source: "camera-track",
			note: "emitters out of the camera track's view stand still (neither emit nor age, matching Roblox client; pass --all-emitters-running for worst case)".into(),
		}
	} else {
		let fixed_cam = match &base_camera {
			CameraSpec::CFrame { .. } | CameraSpec::LookAt { .. } => base_camera.place(None, preset.width.max(1), preset.height.max(1)),
			CameraSpec::Orbit { .. } => {
				let init_resolved = owned_playback.resolve_at(document, times[0] as f32)?;
				let init_undo = init_resolved.apply_state(document)?;
				let cam_res = (|| -> Result<crate::render::camera::Camera> {
					let mut scene0 = crate::scene::extract::extract(document);
					lend(&mut scene0)?;
					let mut spec = base_camera.clone();
					let framed = match (frame_box, focus) {
						(Some([min, max]), _) => Some((Vec3::from(min).min(Vec3::from(max)), Vec3::from(min).max(Vec3::from(max)))),
						(None, Some(focus_path)) => Some(scene0.bounds_at(focus_path).with_context(|| format!("focus {focus_path:?}: no part or effect is at or under it"))?),
						(None, None) => scene0.framing_subject().and_then(|subject| crate::video::subject_box(renderer, &subject)),
					};
					if let Some(bounds) = framed {
						spec = spec.framing(bounds, preset.width.max(1), preset.height.max(1));
					}
					let subject = scene0.framing_subject();
					let bounds = renderer.framing_bounds(subject.as_ref().unwrap_or(&scene0));
					Ok(spec.place(bounds, preset.width.max(1), preset.height.max(1)))
				})();
				init_undo.restore(document);
				cam_res?
			}
		};
		owned_playback.viewer = vec![crate::scene::playback::ViewerKey {
			at: 0.0,
			cframe: fixed_cam.cframe_components(),
			fov: fixed_cam.fov,
			aspect: viewer_aspect,
		}];
		OffscreenEmittersReport {
			rule: "paused-out-of-view",
			source: "fixed-camera",
			note: "emitters out of the camera's view stand still (neither emit nor age, matching Roblox client; pass --all-emitters-running for worst case)".into(),
		}
	};

	let mut resolved = owned_playback.resolve_at(document, times[0] as f32)?;
	let mut mesh_cache: HashMap<String, Result<Arc<MeshStats>, String>> = HashMap::new();
	let mut mem_collector = options.memory.then(MemoryCollector::default);
	let mut inst_acc: BTreeMap<String, InstAcc> = BTreeMap::new();
	let mut frames = Vec::with_capacity(times.len());
	let mut technology = options.technology.unwrap_or(5);
	let mut warnings: Vec<String> = Vec::new();
	let mut first_scene_counts = SceneCounts::default();

	for (frame_idx, &t) in times.iter().enumerate() {
		let t_f32 = t as f32;
		if !resolved.at(t_f32) {
			resolved = owned_playback.resolve_at(document, t_f32)?;
		}
		let undo = resolved.apply_state(document)?;
		let step = (|| -> Result<()> {
			let mut scene = crate::scene::extract::extract(document);
			resolved.apply_history(document, &mut scene);
			lend(&mut scene)?;

			let mut spec = if let Some(track) = normalized_track.as_deref() {
				track_camera(track, t_f32)
			} else {
				base_camera.clone()
			};
			if !has_track {
				let framed = match (frame_box, focus) {
					(Some([min, max]), _) => Some((Vec3::from(min).min(Vec3::from(max)), Vec3::from(min).max(Vec3::from(max)))),
					(None, Some(focus_path)) => Some(scene.bounds_at(focus_path).with_context(|| format!("focus {focus_path:?}: no part or effect is at or under it"))?),
					(None, None) => scene.framing_subject().and_then(|subject| crate::video::subject_box(renderer, &subject)),
				};
				if let Some(bounds) = framed {
					spec = spec.framing(bounds, preset.width.max(1), preset.height.max(1));
				}
			}
			let bounds = match &spec {
				CameraSpec::CFrame { .. } | CameraSpec::LookAt { .. } => None,
				_ => {
					let subject = scene.framing_subject();
					renderer.framing_bounds(subject.as_ref().unwrap_or(&scene))
				}
			};
			let camera = spec.place(bounds, preset.width.max(1), preset.height.max(1));
			let aspect = preset.width.max(1) as f32 / preset.height.max(1) as f32;

			let meshes = Meshes::load_cached(&scene.parts, &renderer.assets, &mut mesh_cache);
			let analysis = Analysis::new_with_meshes(&scene, document, meshes, options, &HashMap::new());
			if frame_idx == 0 {
				technology = analysis.technology;
				first_scene_counts = analysis.scene_counts();
			}
			for w in &analysis.warnings {
				if !warnings.contains(w) {
					warnings.push(w.clone());
				}
			}
			let frame_name = format!("frame-{frame_idx:04}");
			let view_cost = analysis.view(&frame_name, &camera, aspect, options);
			let owners = Owners::new(document, &scene, &analysis, lent_lighting);
			let ids = InstanceIds::new(document);
			let counted = if options.pixels {
				let req = crate::render::ViewRequest {
					width: analysis_w,
					height: analysis_h,
					camera: camera.spec(),
					msaa: 1,
					bloom: false,
					layers: true,
					lod: (!options.full_detail).then_some(crate::lod::Lod { quality: quality.level, cull: true }),
					..Default::default()
				};
				let shadows = attribute::sun_shadows(&scene, &quality);
				let (images, _) = renderer.render_gated(&scene, std::slice::from_ref(&req), gpu)?;
				let drawn: Vec<usize> = view_cost.drawn.iter().map(|(item, _, _)| *item as usize).collect();
				images.first().and_then(|image| image.layers.as_ref()).map(|layers| pixels::view(&scene, &analysis, &drawn, &camera, layers, &quality, shadows))
			} else {
				None
			};
			let view_work = attribute::view(&scene, &analysis, &owners, &ids, &view_cost, &camera, counted.as_ref(), &preset, &quality);
			let total = view_work.total.rounded();

			frames.push(FrameCost {
				frame: frame_idx,
				time: t,
				draws: view_cost.total.draws,
				triangles: view_cost.total.triangles,
				parts: view_cost.visible.parts,
				emitters: view_cost.visible.emitters,
				particles: view_cost.visible.particles,
				particle_fill: round4(view_cost.visible.particle_fill),
				beams: view_cost.visible.beams,
				trails: view_cost.visible.trails,
				blended: total.blended,
				blended_viewports: round4(total.blended / viewport_pixels),
				gpu_units: total.gpu_units,
				cpu_ms: total.cpu_ms,
				gpu_ms: total.gpu_ms,
				ms: total.ms,
			});

			// Aggregate per-instance cost deterministically keyed by instance id path.
			let mut frame_inst: BTreeMap<String, (Work, String, String)> = BTreeMap::new();
			for (&referent, work) in &view_work.by_instance {
				let id = if referent.is_none() {
					attribute::SHARED.to_string()
				} else {
					ids.id(referent).map(str::to_string).unwrap_or_else(|| attribute::SHARED.to_string())
				};
				let info = ids.info(referent);
				let class = info.map(|i| i.1.clone()).unwrap_or_default();
				let name = info.map(|i| i.2.clone()).unwrap_or_else(|| id.clone());
				let entry = frame_inst.entry(id).or_insert_with(|| (Work::default(), class, name));
				entry.0.add(work);
			}
			for (id, (work, class, name)) in frame_inst {
				let w = work.rounded();
				let entry = inst_acc.entry(id).or_insert_with(|| InstAcc { class, name, ..Default::default() });
				entry.frames += 1;
				entry.sum_ms += w.ms;
				entry.sum_gpu_units += w.gpu_units;
				entry.sum_draws += w.draws;
				let better_peak = entry.frames == 1
					|| w.ms > entry.peak_ms
					|| (desktop && w.ms == entry.peak_ms && w.gpu_units > entry.peak_gpu_units);
				if better_peak {
					entry.peak_ms = w.ms;
					entry.peak_time = t;
					entry.peak_frame = frame_idx;
				}
				if w.gpu_units > entry.peak_gpu_units {
					entry.peak_gpu_units = w.gpu_units;
				}
				if w.draws > entry.peak_draws {
					entry.peak_draws = w.draws;
				}
			}

			for acc in &mut explain_accs {
				let mut frame_work = Work::default();
				let mut ordered: Vec<(String, &Work)> = view_work
					.by_instance
					.iter()
					.filter(|&(referent, _)| {
						let mut cur = *referent;
						while !cur.is_none() {
							if cur == acc.root {
								return true;
							}
							cur = document.dom.get_by_ref(cur).map(|inst| inst.parent()).unwrap_or_else(Ref::none);
						}
						false
					})
					.map(|(referent, work)| (ids.id(*referent).unwrap_or_default().to_string(), work))
					.collect();
				ordered.sort_by(|a, b| a.0.cmp(&b.0));
				for (leaf, work) in ordered {
					frame_work.add(work);
					acc.leaves.entry(leaf).or_default().add(work);
				}
				let fw = frame_work.rounded();
				acc.sum.add(&frame_work);
				let better_peak = acc.frames.is_empty()
					|| fw.ms > acc.peak_ms
					|| (desktop && fw.ms == acc.peak_ms && fw.gpu_units > acc.peak_gpu_units);
				if better_peak {
					acc.peak_ms = fw.ms;
					acc.peak_time = t;
					acc.peak_frame = frame_idx;
				}
				acc.peak_gpu_units = acc.peak_gpu_units.max(fw.gpu_units);
				acc.peak_draws = acc.peak_draws.max(fw.draws);
				acc.peak_triangles = acc.peak_triangles.max(fw.triangles);
				acc.peak_blended = acc.peak_blended.max(fw.blended);
				acc.frames.push(serde_json::json!({ "frame": frame_idx, "time": t, "work": fw }));
			}

			if let Some(collector) = mem_collector.as_mut() {
				collector.add_scene(&scene, &renderer.assets);
			}
			Ok(())
		})();
		undo.restore(document);
		step?;
	}

	let memory = mem_collector.map(|collector| {
		let loaded_meshes: HashMap<String, Arc<MeshStats>> = mesh_cache.into_iter().filter_map(|(k, v)| v.ok().map(|s| (k, s))).collect();
		let mut tex_cache = TextureCache::default();
		collector.finish(&loaded_meshes, &renderer.assets, &mut tex_cache)
	});
	if let Some(mem) = &memory {
		for w in memory::missing_texture_warnings(mem) {
			if !warnings.contains(&w) {
				warnings.push(w);
			}
		}
	}

	let uniform_dt = fps_opt.map(|fps| 1.0 / fps);
	let summary = summarize_frames(&frames, uniform_dt, desktop);

	let looping = if !natural_info.looping_emitters.is_empty() {
		let steady_at = round6(f64::from(natural_info.steady_state_at.unwrap_or(0.0)));
		let file_enabled_start_empty = prewarm <= 0.0 && natural_info.looping_emitters.iter().any(|e| e.file_enabled);
		let climb_slice: Vec<FrameCost> = frames.iter().filter(|f| f.time < steady_at - 1e-4).cloned().collect();
		let steady_slice: Vec<FrameCost> = frames.iter().filter(|f| f.time >= steady_at - 1e-4).cloned().collect();
		let climb_frames = climb_slice.len();
		let steady_frames = steady_slice.len();
		let steady_state_reached = steady_frames > 0;
		let climb = (!climb_slice.is_empty()).then(|| summarize_frames(&climb_slice, uniform_dt, desktop));
		let steady_state = (!steady_slice.is_empty()).then(|| summarize_frames(&steady_slice, uniform_dt, desktop));
		let warning = if !steady_state_reached {
			let last_t = frames.last().map(|f| f.time).unwrap_or(start);
			let suggested = round4((steady_at + 1.0 - start).max(steady_at + 1.0));
			Some(format!(
				"steady state NOT reached (looping emitters climb until t={steady_at:.2}s, last sampled frame t={last_t:.2}s{}); reported mean reflects climb-only particles — re-run with --duration auto or --duration >= {suggested:.2} (or --start {steady_at:.2})",
				if file_enabled_start_empty { "; file-enabled emitters start empty at t=0" } else { "" }
			))
		} else {
			None
		};
		Some(LoopingReport {
			emitters_still_running: natural_info.looping_emitters.len(),
			emitter_paths: natural_info.looping_emitters.iter().take(TOP_INSTANCES).map(|e| e.path.clone()).collect(),
			file_enabled_start_empty,
			steady_state_at: steady_at,
			steady_state_reached,
			climb_frames,
			steady_frames,
			climb,
			steady_state,
			warning,
		})
	} else {
		None
	};

	let n_frames = frames.len().max(1) as f64;
	let mut top_instances: Vec<TimelineInstance> = inst_acc
		.into_iter()
		.filter(|(id, _)| id != attribute::SHARED)
		.map(|(id, acc)| TimelineInstance {
			id,
			class: acc.class,
			name: acc.name,
			frames: acc.frames,
			mean_ms: round4(acc.sum_ms / n_frames),
			peak_ms: round4(acc.peak_ms),
			peak_time: round6(acc.peak_time),
			peak_frame: acc.peak_frame,
			mean_gpu_units: round4(acc.sum_gpu_units / n_frames),
			peak_gpu_units: round4(acc.peak_gpu_units),
			mean_draws: round4(acc.sum_draws / n_frames),
			peak_draws: round4(acc.peak_draws),
		})
		.collect();
	top_instances.sort_by(|a, b| {
		let ord = b.peak_ms.total_cmp(&a.peak_ms);
		let ord = if desktop { ord.then_with(|| b.peak_gpu_units.total_cmp(&a.peak_gpu_units)) } else { ord };
		ord.then_with(|| b.mean_ms.total_cmp(&a.mean_ms)).then_with(|| a.id.cmp(&b.id))
	});
	let top_limit = options.top.unwrap_or(TOP_INSTANCES);
	top_instances.truncate(top_limit);

	let budget = budget_spec.map(|limits| {
		let frames_over = frames.iter().filter(|f| !limits.check(f.draws, f.triangles, f.ms).is_empty()).count();
		let worst_draws = frames.iter().map(|f| f.draws).max().unwrap_or(0);
		let worst_tris = frames.iter().map(|f| f.triangles).max().unwrap_or(0);
		let worst_ms = summary.ms.peak;
		let worst_over = limits.check(worst_draws, worst_tris, worst_ms);
		let (mean_kind, mean_sum) = match &looping {
			Some(l) => match &l.steady_state {
				Some(steady) => ("steadyMean", steady),
				None => ("climbMean", &summary),
			},
			None => ("mean", &summary),
		};
		let mean_over = limits.check_f64(mean_sum.draws.mean, mean_sum.triangles.mean, mean_sum.ms.mean);
		let over = !worst_over.is_empty() || !mean_over.is_empty() || frames_over > 0;
		TimelineBudget {
			limits,
			over,
			frames_over,
			frame_count: frames.len(),
			worst_frame: TimelineBudgetCheck {
				draws: worst_draws as f64,
				triangles: worst_tris as f64,
				ms: worst_ms,
				over: worst_over,
			},
			mean_kind,
			mean: TimelineBudgetCheck {
				draws: mean_sum.draws.mean,
				triangles: mean_sum.triangles.mean,
				ms: mean_sum.ms.mean,
				over: mean_over,
			},
		}
	});
	let over_budget = budget.as_ref().map_or(0, |b| if b.over { b.frames_over.max(1) } else { 0 });

	let mut fidelity = prices::fidelity(&preset, options.pixels);
	if let Some(map) = fidelity.as_object_mut() {
		map.insert("durationSource".into(), serde_json::json!(duration_source.source));
		map.insert("offscreenEmitters".into(), serde_json::json!(offscreen_emitters.rule));
		map.insert("offscreenEmittersSource".into(), serde_json::json!(offscreen_emitters.source));
		if let Some(loop_rep) = &looping {
			map.insert("steadyStateReached".into(), serde_json::json!(loop_rep.steady_state_reached));
		}
	}
	let mut unmodeled = prices::unmodeled(&preset, &quality);
	if tl.all_emitters_running {
		unmodeled.push(serde_json::json!({
			"kind": "offscreenEmittersAllRunning",
			"scope": "particles,draws,gpu",
			"reason": "--all-emitters-running forced every emitter to emit and age even out of the camera's view; live Roblox clients pause emitters out of view.",
			"settleWith": "Omit --all-emitters-running to pause emitters out of the camera's view.",
		}));
	}
	if let Some(loop_rep) = &looping
		&& !loop_rep.steady_state_reached
	{
		unmodeled.push(serde_json::json!({
			"kind": "steadyStateNotReached",
			"scope": "particles,draws,gpu,ms",
			"reason": loop_rep.warning.clone().unwrap_or_else(|| "timeline ended before looping emitters reached steady state".into()),
			"settleWith": format!("Re-run with --duration auto or --duration >= {:.2} (or --start {:.2}).", loop_rep.steady_state_at + 1.0, loop_rep.steady_state_at),
		}));
	}

	let report = TimelineReport {
		schema: TIMELINE_SCHEMA,
		input: input.to_string(),
		start,
		duration,
		fps: fps_opt,
		frame_count: frames.len(),
		preset: preset.clone(),
		quality: quality.clone(),
		technology,
		prices: prices::table_for(&preset),
		fidelity,
		unmodeled,
		duration_source,
		offscreen_emitters,
		looping,
		pixels_counted: options.pixels,
		memory: memory.clone(),
		budget,
		summary,
		top_instances,
		frames,
	};

	crate::output::write_json_atomic(&out.join("timeline.json"), &report, true)?;
	let mut text_lines = format_lines(&report, desktop, options.top.unwrap_or(8), options.fail_over_budget);
	for acc in explain_accs {
		let mean = acc.sum.scaled(1.0 / n_frames).rounded();
		let mut ranked: Vec<(String, Work)> = acc
			.leaves
			.into_iter()
			.map(|(leaf, work)| (leaf, work.scaled(1.0 / n_frames).rounded()))
			.collect();
		ranked.sort_by(|a, b| {
			let ord = b.1.ms.total_cmp(&a.1.ms);
			let ord = if desktop { ord.then_with(|| b.1.gpu_units.total_cmp(&a.1.gpu_units)) } else { ord };
			ord.then_with(|| a.0.cmp(&b.0))
		});
		let explained = serde_json::json!({
			"schema": "ror-cost-timeline-explain/1",
			"id": acc.id,
			"uid": acc.uid,
			"class": acc.class,
			"frameCount": acc.frames.len(),
			"summary": {
				"mean": mean,
				"peakMs": round4(acc.peak_ms),
				"peakTime": round6(acc.peak_time),
				"peakFrame": acc.peak_frame,
				"peakGpuUnits": round4(acc.peak_gpu_units),
				"peakDraws": round4(acc.peak_draws),
				"peakTriangles": round4(acc.peak_triangles),
				"peakBlended": round4(acc.peak_blended),
			},
			"frames": acc.frames,
			"costliest": ranked.iter().take(50).map(|(leaf, work)| serde_json::json!({ "id": leaf, "mean": work })).collect::<Vec<_>>(),
		});
		let explain_file = format!("explain-{}.json", agent::file_name(&acc.id));
		crate::output::write_json_atomic(&out.join(&explain_file), &explained, true)?;
		text_lines.push(format!(
			"explain {} (timeline, {} frames; {explain_file}): mean {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}), peak {:.1} draws, peak {:.2} gpuUnits",
			acc.id,
			report.frame_count,
			mean.ms,
			acc.peak_ms,
			acc.peak_time,
			acc.peak_frame,
			acc.peak_draws,
			acc.peak_gpu_units
		));
		for (leaf, work) in ranked.iter().take(5) {
			text_lines.push(format!(
				"  mean {:>6.2} ms  {:.2} draws {:.0} tris {:.2} gpuUnits  {leaf}",
				work.ms, work.draws, work.triangles, work.gpu_units
			));
		}
	}
	crate::output::write_atomic(&out.join("analysis.txt"), (text_lines.join("\n") + "\n").as_bytes())?;

	Ok(CostReport {
		technology,
		quality: quality.level,
		occlusion: !options.no_occlusion,
		mesh_streaming: None,
		scene: first_scene_counts,
		memory,
		views: Vec::new(),
		opportunities: Vec::new(),
		lines: Vec::new(),
		warnings,
		agent: text_lines,
		over_budget,
	})
}

fn format_lines(report: &TimelineReport, desktop: bool, top_shown: usize, fail_over_budget: bool) -> Vec<String> {
	let mut lines = Vec::new();
	let quality_label = match report.quality.client_level {
		Some(bar) => format!("quality {} (client bar {bar}, {:.0} studs)", report.quality.level, report.quality.view_distance),
		None => format!("quality {}", report.quality.level),
	};
	let span = match report.fps {
		Some(fps) => format!("{:.2}..+{:.2}s @ {:.1} fps", report.start, report.duration, fps),
		None => format!("{:.2}..+{:.2}s (explicit times)", report.start, report.duration),
	};
	lines.push(format!(
		"cost timeline ({TIMELINE_SCHEMA}): {}, {} frames ({span}; {}: {}), preset {} ({}x{}, {quality_label}{})",
		report.input,
		report.frame_count,
		report.duration_source.mode,
		report.duration_source.detail,
		report.preset.name,
		report.preset.width,
		report.preset.height,
		if report.quality.sun_shadows { ", sun shadows" } else { ", no sun shadows" }
	));
	let s = &report.summary;
	if let Some(looping) = &report.looping {
		if let Some(steady) = &looping.steady_state {
			if !desktop {
				lines.push(format!(
					"steady-state frame time (t>={:.2}s, {} frames): mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {})",
					looping.steady_state_at, looping.steady_frames, steady.ms.mean, steady.ms.p95, steady.ms.peak, steady.ms.peak_time, steady.ms.peak_frame
				));
			} else {
				lines.push(format!(
					"steady-state desktop CPU frame time (t>={:.2}s, {} frames): mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); GPU uncalibrated (mean {:.2} / peak {:.2} gpuUnits, gpuMs: null)",
					looping.steady_state_at, looping.steady_frames, steady.ms.mean, steady.ms.p95, steady.ms.peak, steady.ms.peak_time, steady.ms.peak_frame, steady.gpu_units.mean, steady.gpu_units.peak
				));
				lines.push("fidelity: desktop CPU ms is estimated from one reference PC ratio (1/2.224); desktop GPU ms is uncalibrated (gpuMs = null)".into());
			}
			lines.push(format!(
				"steady-state counts: draws mean {:.1} / peak {:.0} (t={:.3}s); triangles mean {:.0}k / peak {:.0}k; particles mean {:.1} / peak {:.0}; blended mean {:.2} / peak {:.2} vp",
				steady.draws.mean,
				steady.draws.peak,
				steady.draws.peak_time,
				steady.triangles.mean / 1000.0,
				steady.triangles.peak / 1000.0,
				steady.particles.mean,
				steady.particles.peak,
				steady.blended_viewports.mean,
				steady.blended_viewports.peak
			));
			if let Some(climb) = &looping.climb {
				let start_note = if looping.file_enabled_start_empty {
					format!("; file-enabled emitters start empty at t=0, prewarm {:.2}s", report.duration_source.prewarm)
				} else {
					String::new()
				};
				lines.push(format!(
					"climb (t<{:.2}s, {} frames{start_note}): mean {:.2} ms, peak {:.2} ms; particles mean {:.1} / peak {:.0}; blended mean {:.2} vp",
					looping.steady_state_at, looping.climb_frames, climb.ms.mean, climb.ms.peak, climb.particles.mean, climb.particles.peak, climb.blended_viewports.mean
				));
			} else {
				lines.push(format!(
					"climb: none (run starts at t={:.2}s >= steady state t={:.2}s)",
					report.start, looping.steady_state_at
				));
			}
			lines.push(format!(
				"whole run ({} frames): mean {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); particles mean {:.1} / peak {:.0} (endless emitters: steady-state mean is the headline figure)",
				report.frame_count, s.ms.mean, s.ms.peak, s.ms.peak_time, s.ms.peak_frame, s.particles.mean, s.particles.peak
			));
		} else {
			if let Some(warn) = &looping.warning {
				lines.push(format!("WARNING: {warn}"));
			}
			if !desktop {
				lines.push(format!(
					"frame time (climb-only, before steady state): mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {})",
					s.ms.mean, s.ms.p95, s.ms.peak, s.ms.peak_time, s.ms.peak_frame
				));
			} else {
				lines.push(format!(
					"desktop CPU frame time (climb-only, before steady state): mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); GPU uncalibrated (mean {:.2} / peak {:.2} gpuUnits, gpuMs: null)",
					s.ms.mean, s.ms.p95, s.ms.peak, s.ms.peak_time, s.ms.peak_frame, s.gpu_units.mean, s.gpu_units.peak
				));
				lines.push("fidelity: desktop CPU ms is estimated from one reference PC ratio (1/2.224); desktop GPU ms is uncalibrated (gpuMs = null)".into());
			}
			lines.push(format!(
				"counts (climb-only, before steady state): draws mean {:.1} / peak {:.0} (t={:.3}s); triangles mean {:.0}k / peak {:.0}k; particles mean {:.1} / peak {:.0}; blended mean {:.2} / peak {:.2} vp",
				s.draws.mean, s.draws.peak, s.draws.peak_time, s.triangles.mean / 1000.0, s.triangles.peak / 1000.0, s.particles.mean, s.particles.peak, s.blended_viewports.mean, s.blended_viewports.peak
			));
		}
		let shown: Vec<&str> = looping.emitter_paths.iter().take(6).map(String::as_str).collect();
		let more = looping.emitters_still_running.saturating_sub(shown.len());
		let suffix = if more > 0 { format!(" (+{more} more)") } else { String::new() };
		lines.push(format!(
			"looping emitters still running at end ({}): {}{suffix}",
			looping.emitters_still_running,
			shown.join(", ")
		));
	} else {
		if !desktop {
			lines.push(format!(
				"frame time: mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); integral {:.2} ms*s",
				s.ms.mean, s.ms.p95, s.ms.peak, s.ms.peak_time, s.ms.peak_frame, s.ms.integral
			));
		} else {
			lines.push(format!(
				"desktop CPU frame time: mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); GPU uncalibrated (mean {:.2} / peak {:.2} gpuUnits, gpuMs: null)",
				s.ms.mean, s.ms.p95, s.ms.peak, s.ms.peak_time, s.ms.peak_frame, s.gpu_units.mean, s.gpu_units.peak
			));
			lines.push("fidelity: desktop CPU ms is estimated from one reference PC ratio (1/2.224); desktop GPU ms is uncalibrated (gpuMs = null)".into());
		}
		lines.push(format!(
			"counts: draws mean {:.1} / peak {:.0} (t={:.3}s); triangles mean {:.0}k / peak {:.0}k; particles mean {:.1} / peak {:.0}; blended mean {:.2} / peak {:.2} vp",
			s.draws.mean, s.draws.peak, s.draws.peak_time, s.triangles.mean / 1000.0, s.triangles.peak / 1000.0, s.particles.mean, s.particles.peak, s.blended_viewports.mean, s.blended_viewports.peak
		));
	}
	lines.push(format!(
		"offscreen emitters: {} ({}; {})",
		report.offscreen_emitters.rule,
		report.offscreen_emitters.source,
		if report.offscreen_emitters.rule == "all-running" {
			"worst-case flag active: emitters out of view still emit and age, unlike Roblox client"
		} else {
			"emitters out of view neither emit nor age, matching Roblox client; --all-emitters-running for worst case"
		}
	));
	if let Some(memory) = &report.memory {
		lines.push(memory::summary_line(memory, !desktop));
		for warning in memory::missing_texture_warnings(memory) {
			lines.push(format!("warning: {warning}"));
		}
	}
	if let Some(budget) = &report.budget {
		let verdict = if budget.over { "OVER" } else { "PASS" };
		lines.push(format!(
			"budget {} ({} draws, {}k triangles, {:.2} ms): {verdict} ({} of {} frames over)",
			budget.limits.name,
			budget.limits.draws,
			budget.limits.triangles / 1000,
			budget.limits.ms,
			budget.frames_over,
			budget.frame_count
		));
		let worst_v = if budget.worst_frame.over.is_empty() {
			"PASS".to_string()
		} else {
			format!("OVER ({})", budget.worst_frame.over.join(", "))
		};
		lines.push(format!(
			"  worst frame: {worst_v} [{:.0} draws, {:.0}k tris, {:.2} ms]",
			budget.worst_frame.draws,
			budget.worst_frame.triangles / 1000.0,
			budget.worst_frame.ms
		));
		let mean_label = match budget.mean_kind {
			"steadyMean" => format!(
				"steady mean (t>={:.2}s)",
				report.looping.as_ref().map_or(0.0, |l| l.steady_state_at)
			),
			"climbMean" => "climb-only mean (before steady state)".to_string(),
			_ => "mean".to_string(),
		};
		let mean_v = if budget.mean.over.is_empty() {
			"PASS".to_string()
		} else {
			format!("OVER ({})", budget.mean.over.join(", "))
		};
		lines.push(format!(
			"  {mean_label}: {mean_v} [{:.1} draws, {:.0}k tris, {:.2} ms]",
			budget.mean.draws,
			budget.mean.triangles / 1000.0,
			budget.mean.ms
		));
		if fail_over_budget && budget.over {
			lines.push("over budget: timeline failed --fail-over-budget".into());
		}
	}
	if !report.top_instances.is_empty() && top_shown > 0 {
		lines.push("peak costing instances across timeline:".into());
		for inst in report.top_instances.iter().take(top_shown) {
			lines.push(format!(
				"  peak {:>7.2} ms (t={:.3}s, mean {:>6.2} ms, peak {:>5.2} gpuUnits, peak {:>4.0} draws)  {}",
				inst.peak_ms, inst.peak_time, inst.mean_ms, inst.peak_gpu_units, inst.peak_draws, inst.id
			));
		}
	}
	lines
}

/// Compares a timeline report JSON against an earlier timeline run's `timeline.json` in `before_dir`.
pub fn compare_timelines(after: &serde_json::Value, before_dir: &Path) -> Result<Vec<String>> {
	let file = before_dir.join("timeline.json");
	let before: serde_json::Value = serde_json::from_slice(
		&std::fs::read(&file).with_context(|| format!("--compare: reading {} (timeline --compare expects an earlier timeline output folder)", file.display()))?,
	)?;
	if before.get("schema").and_then(|v| v.as_str()) != Some(TIMELINE_SCHEMA) {
		bail!("--compare: {} is not a {TIMELINE_SCHEMA} file", file.display());
	}
	let mut lines = vec![format!(
		"compared timeline with {} ({} -> {} frames):",
		before_dir.display(),
		before["frameCount"].as_u64().unwrap_or(0),
		after["frameCount"].as_u64().unwrap_or(0)
	)];
	let b_preset = before.pointer("/preset/name").and_then(|v| v.as_str()).unwrap_or("?");
	let a_preset = after.pointer("/preset/name").and_then(|v| v.as_str()).unwrap_or("?");
	if b_preset != a_preset {
		lines.push(format!("  warning: preset changed ({b_preset} -> {a_preset})"));
	}
	let b_off = before.pointer("/offscreenEmitters/rule").and_then(|v| v.as_str()).unwrap_or("?");
	let a_off = after.pointer("/offscreenEmitters/rule").and_then(|v| v.as_str()).unwrap_or("?");
	if b_off != a_off {
		lines.push(format!("  warning: offscreen emitters rule changed ({b_off} -> {a_off})"));
	}
	let num = |root: &serde_json::Value, ptr: &str| root.pointer(ptr).and_then(|v| v.as_f64()).unwrap_or(0.0);
	let delta = |base: &serde_json::Value, curr: &serde_json::Value, ptr: &str, label: &str, digits: usize| -> String {
		let (a, b) = (num(base, ptr), num(curr, ptr));
		let pct = if a.abs() > 1e-9 { format!(" ({:+.1}%)", 100.0 * (b - a) / a) } else { String::new() };
		format!("{label} {a:.digits$} -> {b:.digits$}{pct}")
	};
	let compare_summary = |lines: &mut Vec<String>, label: &str, b_sum: &serde_json::Value, a_sum: &serde_json::Value, show_integral: bool| {
		let mut ms_line = format!(
			"  {label} ms: {}, {}, {}",
			delta(b_sum, a_sum, "/ms/mean", "mean", 2),
			delta(b_sum, a_sum, "/ms/p95", "p95", 2),
			delta(b_sum, a_sum, "/ms/peak", "peak", 2),
		);
		if show_integral {
			ms_line.push_str(&format!(", {}", delta(b_sum, a_sum, "/ms/integral", "integral", 2)));
		}
		lines.push(ms_line);
		lines.push(format!(
			"  {label} counts: {}, {}, {}, {}, {}",
			delta(b_sum, a_sum, "/draws/mean", "draws mean", 1),
			delta(b_sum, a_sum, "/draws/peak", "peak", 0),
			delta(b_sum, a_sum, "/triangles/mean", "tris mean", 0),
			delta(b_sum, a_sum, "/particles/mean", "particles mean", 1),
			delta(b_sum, a_sum, "/blendedViewports/mean", "blended vp", 2),
		));
		lines.push(format!(
			"  {label} gpuUnits: {}, {}",
			delta(b_sum, a_sum, "/gpuUnits/mean", "mean", 2),
			delta(b_sum, a_sum, "/gpuUnits/peak", "peak", 2),
		));
	};
	if let (Some(b_st), Some(a_st)) = (before.pointer("/looping/steadyState"), after.pointer("/looping/steadyState"))
		&& b_st.is_object()
		&& a_st.is_object()
	{
		compare_summary(&mut lines, "steady-state", b_st, a_st, false);
	}
	compare_summary(&mut lines, "whole-run", &before["summary"], &after["summary"], true);
	if let (Some(b_mem), Some(a_mem)) = (before.get("memory"), after.get("memory"))
		&& b_mem.is_object()
		&& a_mem.is_object()
	{
		let mb = |root: &serde_json::Value, ptr: &str| num(root, ptr) / (1024.0 * 1024.0);
		let desktop = a_preset == "desktop";
		let (field, tag) = if desktop { ("/totalGpuBytes/desktopBc", "BCn") } else { ("/totalGpuBytes/mobileEtc2", "ETC2") };
		let (ba, aa) = (mb(b_mem, field), mb(a_mem, field));
		let pct = if ba.abs() > 1e-9 { format!(" ({:+.1}%)", 100.0 * (aa - ba) / ba) } else { String::new() };
		let b_partial = b_mem.get("partial").and_then(|v| v.as_bool()).unwrap_or(false);
		let a_partial = a_mem.get("partial").and_then(|v| v.as_bool()).unwrap_or(false);
		let caveat = if b_partial || a_partial {
			format!(" [PARTIAL: before partial={b_partial}, after partial={a_partial}; missing assets excluded]")
		} else {
			String::new()
		};
		lines.push(format!("  memory ({tag} est GPU upper bound): {ba:.2} -> {aa:.2} MB{pct}{caveat}"));
	}
	Ok(lines)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::gpu::Gpu;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use crate::scene::playback::{Action, Emit, Motion, MotionKey, Tween};
	use glam::Affine3A;
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::NumberRange;

	fn make_test_renderer(state: &Path) -> Renderer {
		let gpu = Arc::new(Gpu::new().expect("a GPU or software adapter"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(state) });
		Renderer::new(gpu, assets).expect("renderer")
	}

	fn look_at_cframe(eye: Vec3, target: Vec3) -> [f32; 12] {
		let back = (eye - target).normalize();
		let right = Vec3::Y.cross(back).normalize();
		let up = back.cross(right);
		[eye.x, eye.y, eye.z, right.x, up.x, back.x, right.y, up.y, back.y, right.z, up.z, back.z]
	}

	fn build_dynamic_scene() -> (Document, Playback) {
		let mut model = ModelBuilder::new("Root");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -2.0, 0.0)), size: Vec3::new(20.0, 1.0, 20.0), ..Default::default() });
		let mover = model.part(&PartSpec { name: "Mover".into(), cframe: Affine3A::from_translation(Vec3::new(-6.0, 2.0, 0.0)), size: Vec3::splat(2.0), ..Default::default() });
		let a0 = model.child(mover, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(Vec3::new(0.0, -1.0, 0.0)))));
		let a1 = model.child(mover, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_translation(Vec3::new(0.0, 1.0, 0.0)))));
		model.child(
			mover,
			InstanceBuilder::new("Trail")
				.with_name("Ribbon")
				.with_property("Attachment0", a0)
				.with_property("Attachment1", a1)
				.with_property("Lifetime", 0.5f32)
				.with_property("MinLength", 0.1f32)
				.with_property("Enabled", true),
		);
		model.child(
			mover,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Burst")
				.with_property("Rate", 0.0f32)
				.with_property("Lifetime", NumberRange::new(0.5, 0.5))
				.with_property("Speed", NumberRange::new(2.0, 2.0))
				.with_property("Enabled", false),
		);
		model.child(
			mover,
			InstanceBuilder::new("Beam")
				.with_name("Arc")
				.with_property("Attachment0", a0)
				.with_property("Attachment1", a1)
				.with_property("Segments", 6i32)
				.with_property("Enabled", false),
		);
		model.part(&PartSpec { name: "FadeBox".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, -4.0)), size: Vec3::splat(3.0), transparency: 1.0, ..Default::default() });

		let playback = Playback {
			actions: vec![
				Action { at: 0.1, target: "Root.Mover.Burst".into(), emit: Some(Emit::Count(15)), ..Default::default() },
				Action { at: 0.2, target: "Root.Mover.Arc".into(), enabled: Some(true), ..Default::default() },
				Action { at: 0.5, target: "Root.Mover.Arc".into(), enabled: Some(false), ..Default::default() },
			],
			tweens: vec![Tween { target: "Root.FadeBox".into(), property: "Transparency".into(), at: 0.1, duration: 0.3, to: Some(serde_json::json!(0.0)), easing: Some("Linear".into()), ..Default::default() }],
			motion: vec![Motion {
				target: "Root.Mover".into(),
				keys: vec![
					MotionKey { at: 0.0, cframe: vec![-6.0, 2.0, 0.0], ..Default::default() },
					MotionKey { at: 0.6, cframe: vec![6.0, 2.0, 0.0], ..Default::default() },
				],
				..Default::default()
			}],
			..Default::default()
		};
		(model.into_document(), playback)
	}

	#[test]
	fn timeline_matches_single_moment_across_burst_beam_trail_tween_and_camera_track() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-parity-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));
		let (mut doc, playback) = build_dynamic_scene();

		let cf0 = look_at_cframe(Vec3::new(0.0, 8.0, 22.0), Vec3::new(0.0, 2.0, 0.0));
		let cf1 = look_at_cframe(Vec3::new(5.0, 10.0, 18.0), Vec3::new(0.0, 2.0, 0.0));
		let track = vec![CameraKey { at: 0.0, cframe: cf0, fov: 70.0 }, CameraKey { at: 0.3, cframe: cf1, fov: 70.0 }];
		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.6), auto_duration: false, fps: 10.0, times: None, camera_track: Some(track.clone()), keep_default_camera: false, all_emitters_running: false };
		let cost_opts = CostOptions { attribute: true, pixels: true, preset: Some("phone".into()), no_images: true, ..Default::default() };
		let views = vec![NamedView { name: "render".into(), view: crate::render::ViewRequest { width: 2532, height: 1170, ..Default::default() } }];

		let out0 = temp.join("tl0");
		let out1 = temp.join("tl1");
		std::fs::create_dir_all(&out0).unwrap();
		std::fs::create_dir_all(&out1).unwrap();
		run(&renderer, &mut doc, Some(&playback), &cost_opts, &tl_opts, &views, None, None, &out0, None, "test.rbxm", false, &|_| Ok(())).unwrap();
		run(&renderer, &mut doc, Some(&playback), &cost_opts, &tl_opts, &views, None, None, &out1, None, "test.rbxm", false, &|_| Ok(())).unwrap();

		// Determinism: byte-identical timeline.json and analysis.txt across repeats.
		assert_eq!(std::fs::read(out0.join("timeline.json")).unwrap(), std::fs::read(out1.join("timeline.json")).unwrap());
		assert_eq!(std::fs::read(out0.join("analysis.txt")).unwrap(), std::fs::read(out1.join("analysis.txt")).unwrap());

		let tl_json: serde_json::Value = serde_json::from_slice(&std::fs::read(out0.join("timeline.json")).unwrap()).unwrap();
		assert!(tl_json.get("memory").is_none(), "memory must be omitted without --memory");
		let frames = tl_json["frames"].as_array().unwrap();
		assert_eq!(frames.len(), 6);
		assert_eq!(frames[0]["particles"].as_u64().unwrap(), 0);
		assert_eq!(frames[3]["particles"].as_u64().unwrap(), 15);
		assert_eq!(frames[3]["beams"].as_u64().unwrap(), 1);
		assert_eq!(frames[3]["trails"].as_u64().unwrap(), 1);

		// Compare frames 0, 3, 5 against single-moment `cost::run`.
		for (fi, cf) in [(0usize, cf0), (3, cf1), (5, cf1)] {
			let t = frames[fi]["time"].as_f64().unwrap() as f32;
			let (mut single_doc, mut single_pb) = build_dynamic_scene();
			single_pb.time = t;
			let resolved = single_pb.resolve(&single_doc).unwrap();
			resolved.apply_state(&mut single_doc).unwrap();
			let mut scene = crate::scene::extract::extract(&single_doc);
			resolved.apply_history(&single_doc, &mut scene);
			let spec = CameraSpec::CFrame { components: cf, fov: 70.0, near: None };
			let cam = spec.place(None, 2532, 1170);
			let sviews = vec![NamedView { name: "render".into(), view: crate::render::ViewRequest { width: 2532, height: 1170, camera: spec, ..Default::default() } }];
			let sout = temp.join(format!("single_{fi}"));
			std::fs::create_dir_all(&sout).unwrap();
			let sreport = crate::cost::run(&renderer, &scene, &single_doc, &cost_opts, &sviews, &[(cam, 2532, 1170)], &sout, None, None, "test.rbxm", (false, false)).unwrap();
			let sa: serde_json::Value = serde_json::from_slice(&std::fs::read(sout.join("analysis.json")).unwrap()).unwrap();
			let sw = &sa["views"][0]["work"];
			let sv = &sreport.views[0];
			assert_eq!(frames[fi]["draws"].as_u64().unwrap(), sv.total.draws, "frame {fi} draws");
			assert_eq!(frames[fi]["triangles"].as_u64().unwrap(), sv.total.triangles, "frame {fi} tris");
			assert_eq!(frames[fi]["particles"].as_u64().unwrap(), sv.visible.particles, "frame {fi} particles");
			assert_eq!(frames[fi]["beams"].as_u64().unwrap(), sv.visible.beams, "frame {fi} beams");
			assert_eq!(frames[fi]["trails"].as_u64().unwrap(), sv.visible.trails, "frame {fi} trails");
			assert!((frames[fi]["blended"].as_f64().unwrap() - sw["blended"].as_f64().unwrap()).abs() <= 1e-3, "frame {fi} blended");
			assert!((frames[fi]["gpuUnits"].as_f64().unwrap() - sw["gpuUnits"].as_f64().unwrap()).abs() <= 1e-4, "frame {fi} gpuUnits");
			assert!((frames[fi]["cpuMs"].as_f64().unwrap() - sw["cpuMs"].as_f64().unwrap()).abs() <= 1e-4, "frame {fi} cpuMs");
			assert!((frames[fi]["gpuMs"].as_f64().unwrap() - sw["gpuMs"].as_f64().unwrap()).abs() <= 1e-4, "frame {fi} gpuMs");
			assert!((frames[fi]["ms"].as_f64().unwrap() - sw["ms"].as_f64().unwrap()).abs() <= 1e-4, "frame {fi} ms");
		}
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_auto_duration_respects_tail_and_caps_infinite_loops() {
		let (doc, pb) = build_dynamic_scene();
		// Burst emit at 0.1 + 0.5 lifetime = 0.6; motion ends at 0.6; Beam ends at 0.5 -> natural_end == 0.6.
		let end = pb.natural_end(&doc);
		assert!((end - 0.6).abs() < 1e-4, "expected 0.6, got {end}");

		// Empty playback falls back to 2.0 s.
		let empty = Playback::default();
		assert!((empty.natural_end(&doc) - 2.0).abs() < 1e-4);

		// Infinite tween repeatCount = -1 is bounded to 1 cycle and capped at 120 s.
		let inf = Playback {
			tweens: vec![Tween { target: "Root.FadeBox".into(), property: "Transparency".into(), at: 0.0, duration: 1.5, repeat_count: -1, reverses: true, to: Some(serde_json::json!(0.0)), ..Default::default() }],
			..Default::default()
		};
		assert!((inf.natural_end(&doc) - 3.0).abs() < 1e-4);
	}

	#[test]
	fn timeline_desktop_refuses_gpu_ms_and_honest_memory_is_opt_in() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-desk-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));
		let (mut doc, playback) = build_dynamic_scene();

		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.3), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 1920,
				height: 1080,
				camera: CameraSpec::LookAt { eye: [0.0, 8.0, 22.0], target: [0.0, 2.0, 0.0], fov: 70.0 },
				..Default::default()
			},
		}];

		// Desktop without --memory: gpuMs is null, no "phone" wording, memory omitted.
		let out_desk = temp.join("desk");
		std::fs::create_dir_all(&out_desk).unwrap();
		let desk_opts = CostOptions { attribute: true, pixels: false, preset: Some("desktop".into()), no_images: true, memory: false, ..Default::default() };
		run(&renderer, &mut doc, Some(&playback), &desk_opts, &tl_opts, &views, None, None, &out_desk, None, "test.rbxm", false, &|_| Ok(())).unwrap();
		let tl_str = std::fs::read_to_string(out_desk.join("timeline.json")).unwrap();
		let txt_str = std::fs::read_to_string(out_desk.join("analysis.txt")).unwrap();
		let tl_val: serde_json::Value = serde_json::from_str(&tl_str).unwrap();
		assert!(tl_val.get("memory").is_none());
		assert!(tl_val["summary"]["gpuMs"].is_null());
		assert!(tl_val["frames"].as_array().unwrap().iter().all(|f| f["gpuMs"].is_null()));
		assert!(!txt_str.to_ascii_lowercase().contains("phone"), "desktop analysis.txt must not mention phone:\n{txt_str}");

		// Desktop with --memory: honest upper-bound-estimate memory block present, no ASTC fields.
		let out_mem = temp.join("desk_mem");
		std::fs::create_dir_all(&out_mem).unwrap();
		let mem_opts = CostOptions { memory: true, ..desk_opts };
		run(&renderer, &mut doc, Some(&playback), &mem_opts, &tl_opts, &views, None, None, &out_mem, None, "test.rbxm", false, &|_| Ok(())).unwrap();
		let mem_str = std::fs::read_to_string(out_mem.join("timeline.json")).unwrap();
		let mem_val: serde_json::Value = serde_json::from_str(&mem_str).unwrap();
		assert_eq!(mem_val["memory"]["kind"].as_str(), Some("upper-bound-estimate"));
		let tex_obj = mem_val["memory"]["textures"].as_object().unwrap();
		assert!(tex_obj.contains_key("mobileEtc2MipBytes"));
		assert!(!tex_obj.keys().any(|k| k.to_ascii_lowercase().contains("astc")));
		let tot_obj = mem_val["memory"]["totalGpuBytes"].as_object().unwrap();
		assert!(tot_obj.contains_key("mobileEtc2"));
		assert!(!tot_obj.keys().any(|k| k.to_ascii_lowercase().contains("astc")));
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_pauses_offscreen_emitters_until_seen_and_flag_opts_into_all_running() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-offscreen-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));

		let mut model = ModelBuilder::new("Offscreen");
		let front = model.part(&PartSpec { name: "FrontHost".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, -20.0)), size: Vec3::ONE, transparency: 1.0, ..Default::default() });
		model.child(
			front,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("FrontStream")
				.with_property("Rate", 30.0f32)
				.with_property("Lifetime", NumberRange::new(1.0, 1.0))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", true),
		);
		let behind = model.part(&PartSpec { name: "BehindHost".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, 470.0)), size: Vec3::ONE, transparency: 1.0, ..Default::default() });
		model.child(
			behind,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("BehindStream")
				.with_property("Rate", 50.0f32)
				.with_property("Lifetime", NumberRange::new(1.0, 1.0))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", true),
		);
		let mut doc = model.into_document();

		let cf_front = look_at_cframe(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, 2.0, -20.0));
		let cf_behind = look_at_cframe(Vec3::new(0.0, 2.0, 0.0), Vec3::new(0.0, 2.0, 470.0));
		let track = vec![CameraKey { at: 0.0, cframe: cf_front, fov: 60.0 }, CameraKey { at: 2.0, cframe: cf_behind, fov: 60.0 }];
		let views = vec![NamedView { name: "render".into(), view: crate::render::ViewRequest { width: 1280, height: 720, ..Default::default() } }];
		let cost_opts = CostOptions { attribute: true, pixels: false, no_images: true, ..Default::default() };

		// Default: emitters out of the camera's view stand still; BehindStream starts from 0 at t=2.0.
		let out_default = temp.join("default");
		std::fs::create_dir_all(&out_default).unwrap();
		let tl_default = TimelineOptions { start: 0.0, duration: Some(3.2), auto_duration: false, fps: 10.0, times: None, camera_track: Some(track.clone()), keep_default_camera: false, all_emitters_running: false };
		run(&renderer, &mut doc, None, &cost_opts, &tl_default, &views, None, None, &out_default, None, "offscreen.rbxmx", false, &|_| Ok(())).unwrap();
		let j_def: serde_json::Value = serde_json::from_slice(&std::fs::read(out_default.join("timeline.json")).unwrap()).unwrap();
		let t_def = std::fs::read_to_string(out_default.join("analysis.txt")).unwrap();
		assert_eq!(j_def["offscreenEmitters"]["rule"], "paused-out-of-view");
		assert_eq!(j_def["offscreenEmitters"]["source"], "camera-track");
		assert_eq!(j_def["fidelity"]["offscreenEmitters"], "paused-out-of-view");
		assert!(t_def.contains("offscreen emitters: paused-out-of-view (camera-track"), "{t_def}");
		let f_def = j_def["frames"].as_array().unwrap();
		let p = |i: usize| f_def[i]["particles"].as_u64().unwrap();
		assert!((29..=31).contains(&p(19)), "expected FrontStream steady ~30 at t=1.9, got {}", p(19));
		assert_eq!(p(20), 0, "BehindStream must start at 0 on first frame seen (t=2.0), got {}", p(20));
		assert!((4..=6).contains(&p(21)), "BehindStream must ramp (~5 at t=2.1), got {}", p(21));
		assert!((23..=27).contains(&p(25)), "BehindStream must ramp (~25 at t=2.5), got {}", p(25));
		assert!((48..=52).contains(&p(30)), "BehindStream must reach steady ~50 at t=3.0, got {}", p(30));

		// Opt-in worst case: --all-emitters-running keeps BehindStream full on the first frame seen.
		let out_all = temp.join("all_running");
		std::fs::create_dir_all(&out_all).unwrap();
		let tl_all = TimelineOptions { all_emitters_running: true, ..tl_default };
		run(&renderer, &mut doc, None, &cost_opts, &tl_all, &views, None, None, &out_all, None, "offscreen.rbxmx", false, &|_| Ok(())).unwrap();
		let j_all: serde_json::Value = serde_json::from_slice(&std::fs::read(out_all.join("timeline.json")).unwrap()).unwrap();
		let t_all = std::fs::read_to_string(out_all.join("analysis.txt")).unwrap();
		assert_eq!(j_all["offscreenEmitters"]["rule"], "all-running");
		assert_eq!(j_all["offscreenEmitters"]["source"], "disabled-by-flag");
		assert_eq!(j_all["fidelity"]["offscreenEmitters"], "all-running");
		assert!(j_all["unmodeled"].as_array().unwrap().iter().any(|u| u["kind"] == "offscreenEmittersAllRunning"));
		assert!(t_all.contains("offscreen emitters: all-running"), "{t_all}");
		let f_all = j_all["frames"].as_array().unwrap();
		assert!((48..=52).contains(&f_all[20]["particles"].as_u64().unwrap()), "worst-case flag should have BehindStream full at t=2.0, got {}", f_all[20]["particles"]);

		// Real-engine 10 s away check: Rate 2, Speed 5, Lifetime 10 -> 0, 1, 2, 3, 4, 5 at +0..+2.5 s.
		let mut stream_model = ModelBuilder::new("Stream");
		let source = stream_model.part(&PartSpec { name: "Source".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 25.0, 0.0)), size: Vec3::ONE, transparency: 1.0, ..Default::default() });
		stream_model.child(
			source,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Column")
				.with_property("Rate", 2.0f32)
				.with_property("Lifetime", NumberRange::new(10.0, 10.0))
				.with_property("Speed", NumberRange::new(5.0, 5.0))
				.with_property("Enabled", true),
		);
		let mut stream_doc = stream_model.into_document();
		let eye = Vec3::new(-60.0, 25.0, 0.0);
		let target = Vec3::new(0.0, 25.0, 0.0);
		let stream_track = vec![
			CameraKey { at: 0.0, cframe: look_at_cframe(eye, eye + (eye - target)), fov: 60.0 },
			CameraKey { at: 10.0, cframe: look_at_cframe(eye, target), fov: 60.0 },
		];
		let out_stream = temp.join("stream");
		std::fs::create_dir_all(&out_stream).unwrap();
		let tl_stream = TimelineOptions {
			start: 0.0,
			duration: None,
			auto_duration: false,
			fps: 10.0,
			times: Some(vec![10.0, 10.5, 11.0, 11.5, 12.0, 12.5]),
			camera_track: Some(stream_track),
			keep_default_camera: false,
			all_emitters_running: false,
		};
		run(&renderer, &mut stream_doc, None, &cost_opts, &tl_stream, &views, None, None, &out_stream, None, "stream.rbxmx", false, &|_| Ok(())).unwrap();
		let j_stream: serde_json::Value = serde_json::from_slice(&std::fs::read(out_stream.join("timeline.json")).unwrap()).unwrap();
		let counts: Vec<u64> = j_stream["frames"].as_array().unwrap().iter().map(|f| f["particles"].as_u64().unwrap()).collect();
		assert_eq!(counts, vec![0, 1, 2, 3, 4, 5], "10 s unseen emitter must ramp 0..5 at 0.5 s steps");
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_auto_reports_provenance_detects_looping_and_separates_climb_from_steady() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-looping-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));

		// Endless file-enabled emitter (Rate 20, Lifetime 1.5 -> steady ~30 at t>=1.5) + short burst (ends 1.3).
		let mut model = ModelBuilder::new("Looping");
		let host = model.part(&PartSpec { name: "Host".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, 0.0)), size: Vec3::ONE, transparency: 1.0, ..Default::default() });
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Endless")
				.with_property("Rate", 20.0f32)
				.with_property("Lifetime", NumberRange::new(1.5, 1.5))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", true),
		);
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("ShortBurst")
				.with_property("Rate", 0.0f32)
				.with_property("Lifetime", NumberRange::new(0.8, 0.8))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", false),
		);
		let mut doc = model.into_document();
		let pb_with_burst = Playback {
			actions: vec![Action { at: 0.5, target: "Looping.Host.ShortBurst".into(), emit: Some(Emit::Count(10)), ..Default::default() }],
			..Default::default()
		};

		// Provenance check on Playback::natural_end_info:
		let info_burst = pb_with_burst.natural_end_info(&doc);
		assert_eq!(info_burst.source, "looping-emitters-and-playback");
		assert!(!info_burst.fallback);
		assert_eq!(info_burst.looping_emitters.len(), 1);
		assert_eq!(info_burst.looping_emitters[0].path, "Looping.Host.Endless");
		assert!((info_burst.steady_state_at.unwrap() - 1.5).abs() < 1e-4);
		assert!((info_burst.seconds - 2.5).abs() < 1e-4, "auto must extend past 1.3s burst to 1.5+1.0=2.5s, got {}", info_burst.seconds);

		// Empty playback on a doc with ONLY looping emitter -> source = "looping-emitters" (not fallback-empty).
		let info_only_loop = Playback::default().natural_end_info(&doc);
		assert_eq!(info_only_loop.source, "looping-emitters");
		assert!(!info_only_loop.fallback);
		assert!((info_only_loop.seconds - 2.5).abs() < 1e-4);

		// Disabling Endless at t=0.5 removes it from looping_emitters (finite tail 0.5+1.5=2.0).
		let pb_disabled = Playback {
			actions: vec![Action { at: 0.5, target: "Looping.Host.Endless".into(), enabled: Some(false), ..Default::default() }],
			..Default::default()
		};
		let info_disabled = pb_disabled.natural_end_info(&doc);
		assert!(info_disabled.looping_emitters.is_empty(), "disabled emitter must not be flagged looping");
		assert_eq!(info_disabled.source, "actions");
		assert!((info_disabled.seconds - 2.0).abs() < 1e-4);

		// Truly empty scene with no actions and no looping emitters -> fallback-empty 2.0s.
		let (dyn_doc, _) = build_dynamic_scene();
		let info_empty = Playback::default().natural_end_info(&dyn_doc);
		assert_eq!(info_empty.source, "fallback-empty");
		assert!(info_empty.fallback);
		assert!((info_empty.seconds - 2.0).abs() < 1e-4);

		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 1280,
				height: 720,
				camera: CameraSpec::LookAt { eye: [0.0, 2.0, 15.0], target: [0.0, 2.0, 0.0], fov: 60.0 },
				..Default::default()
			},
		}];
		let cost_opts = CostOptions { attribute: true, pixels: false, no_images: true, ..Default::default() };

		// 1) --duration auto on endless + burst: reaches steady state, separates climb vs steady (~30 particles headline).
		let out_auto = temp.join("auto");
		std::fs::create_dir_all(&out_auto).unwrap();
		let tl_auto = TimelineOptions { start: 0.0, duration: None, auto_duration: true, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		run(&renderer, &mut doc, Some(&pb_with_burst), &cost_opts, &tl_auto, &views, None, None, &out_auto, None, "looping.rbxmx", false, &|_| Ok(())).unwrap();
		let j_auto: serde_json::Value = serde_json::from_slice(&std::fs::read(out_auto.join("timeline.json")).unwrap()).unwrap();
		let t_auto = std::fs::read_to_string(out_auto.join("analysis.txt")).unwrap();
		assert_eq!(j_auto["durationSource"]["mode"], "auto");
		assert_eq!(j_auto["durationSource"]["source"], "looping-emitters-and-playback");
		assert_eq!(j_auto["looping"]["emittersStillRunning"], 1);
		assert_eq!(j_auto["looping"]["steadyStateReached"], true);
		assert_eq!(j_auto["looping"]["fileEnabledStartEmpty"], true);
		assert_eq!(j_auto["looping"]["climbFrames"], 15);
		assert_eq!(j_auto["looping"]["steadyFrames"], 10);
		let steady_p = j_auto["looping"]["steadyState"]["particles"]["mean"].as_f64().unwrap();
		let climb_p = j_auto["looping"]["climb"]["particles"]["mean"].as_f64().unwrap();
		assert!((28.0..=32.0).contains(&steady_p), "steady particles mean should be ~30, got {steady_p}");
		assert!(climb_p < steady_p - 5.0, "climb particles mean ({climb_p}) must be clearly below steady ({steady_p})");
		assert!(t_auto.contains("steady-state frame time (t>=1.50s, 10 frames):"), "{t_auto}");
		assert!(t_auto.contains("climb (t<1.50s, 15 frames; file-enabled emitters start empty at t=0"), "{t_auto}");
		assert!(t_auto.contains("looping emitters still running at end (1): Looping.Host.Endless"), "{t_auto}");
		assert!(!t_auto.contains("WARNING:"), "auto run reached steady state and should not warn:\n{t_auto}");

		// 2) Short explicit --duration 1.0 (last sample t=0.9 < 1.5): steady state NOT reached + clear WARNING line.
		let out_short = temp.join("short");
		std::fs::create_dir_all(&out_short).unwrap();
		let tl_short = TimelineOptions { start: 0.0, duration: Some(1.0), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		run(&renderer, &mut doc, None, &cost_opts, &tl_short, &views, None, None, &out_short, None, "looping.rbxmx", false, &|_| Ok(())).unwrap();
		let j_short: serde_json::Value = serde_json::from_slice(&std::fs::read(out_short.join("timeline.json")).unwrap()).unwrap();
		let t_short = std::fs::read_to_string(out_short.join("analysis.txt")).unwrap();
		assert_eq!(j_short["durationSource"]["mode"], "explicit-duration");
		assert_eq!(j_short["looping"]["steadyStateReached"], false);
		assert_eq!(j_short["looping"]["steadyFrames"], 0);
		assert!(j_short["looping"]["steadyState"].is_null());
		assert!(j_short["looping"]["warning"].as_str().unwrap().contains("steady state NOT reached"));
		assert!(j_short["unmodeled"].as_array().unwrap().iter().any(|u| u["kind"] == "steadyStateNotReached"));
		assert!(t_short.contains("WARNING: steady state NOT reached"), "short run must warn clearly:\n{t_short}");
		assert!(t_short.contains("frame time (climb-only, before steady state):"), "{t_short}");

		// 3) --start 1.5 --duration 1.0: skips climb completely, starts directly in steady state.
		let out_start = temp.join("start_steady");
		std::fs::create_dir_all(&out_start).unwrap();
		let tl_start = TimelineOptions { start: 1.5, duration: Some(1.0), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		run(&renderer, &mut doc, None, &cost_opts, &tl_start, &views, None, None, &out_start, None, "looping.rbxmx", false, &|_| Ok(())).unwrap();
		let j_start: serde_json::Value = serde_json::from_slice(&std::fs::read(out_start.join("timeline.json")).unwrap()).unwrap();
		assert_eq!(j_start["looping"]["steadyStateReached"], true);
		assert_eq!(j_start["looping"]["climbFrames"], 0);
		assert_eq!(j_start["looping"]["steadyFrames"], 10);
		assert!(j_start["looping"]["climb"].is_null());
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_budget_checks_worst_frame_and_steady_mean_and_reports_over_budget() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-budget-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));

		// Looping emitter + short burst so worst frame > steady mean.
		let mut model = ModelBuilder::new("BudgetFx");
		let host = model.part(&PartSpec { name: "Host".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 2.0, 0.0)), size: Vec3::ONE, transparency: 1.0, ..Default::default() });
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Endless")
				.with_property("Rate", 20.0f32)
				.with_property("Lifetime", NumberRange::new(1.0, 1.0))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", true),
		);
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Burst")
				.with_property("Rate", 0.0f32)
				.with_property("Lifetime", NumberRange::new(0.3, 0.3))
				.with_property("Speed", NumberRange::new(1.0, 1.0))
				.with_property("Enabled", false),
		);
		let mut doc = model.into_document();
		let pb = Playback {
			actions: vec![Action { at: 0.2, target: "BudgetFx.Host.Burst".into(), emit: Some(Emit::Count(10)), ..Default::default() }],
			..Default::default()
		};
		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 1280,
				height: 720,
				camera: CameraSpec::LookAt { eye: [0.0, 2.0, 15.0], target: [0.0, 2.0, 0.0], fov: 60.0 },
				..Default::default()
			},
		}];
		let tl_opts = TimelineOptions { start: 0.0, duration: None, auto_duration: true, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };

		// 1) Generous budget passes cleanly (over_budget == 0).
		let out_pass = temp.join("pass");
		std::fs::create_dir_all(&out_pass).unwrap();
		let pass_opts = CostOptions { attribute: true, pixels: false, no_images: true, budget: Some("draws=500,triangles=500k,ms=50".into()), fail_over_budget: true, ..Default::default() };
		let rep_pass = run(&renderer, &mut doc, Some(&pb), &pass_opts, &tl_opts, &views, None, None, &out_pass, None, "budget.rbxmx", false, &|_| Ok(())).unwrap();
		assert_eq!(rep_pass.over_budget, 0);
		let j_pass: serde_json::Value = serde_json::from_slice(&std::fs::read(out_pass.join("timeline.json")).unwrap()).unwrap();
		let t_pass = std::fs::read_to_string(out_pass.join("analysis.txt")).unwrap();
		assert_eq!(j_pass["budget"]["over"], false);
		assert_eq!(j_pass["budget"]["meanKind"], "steadyMean");
		assert!(t_pass.contains("budget custom") && t_pass.contains(": PASS"));
		assert!(t_pass.contains("worst frame: PASS"));
		assert!(t_pass.contains("steady mean (t>=1.00s): PASS"));

		// 2) Tight budget (draws=1) fails BOTH worst frame and steady mean -> over_budget > 0.
		let out_fail = temp.join("fail");
		std::fs::create_dir_all(&out_fail).unwrap();
		let fail_opts = CostOptions { attribute: true, pixels: false, no_images: true, budget: Some("draws=1".into()), fail_over_budget: true, ..Default::default() };
		let rep_fail = run(&renderer, &mut doc, Some(&pb), &fail_opts, &tl_opts, &views, None, None, &out_fail, None, "budget.rbxmx", false, &|_| Ok(())).unwrap();
		assert!(rep_fail.over_budget > 0, "over-budget timeline must set CostReport.over_budget > 0");
		let j_fail: serde_json::Value = serde_json::from_slice(&std::fs::read(out_fail.join("timeline.json")).unwrap()).unwrap();
		let t_fail = std::fs::read_to_string(out_fail.join("analysis.txt")).unwrap();
		assert_eq!(j_fail["budget"]["over"], true);
		assert_eq!(j_fail["budget"]["meanKind"], "steadyMean");
		assert!(!j_fail["budget"]["worstFrame"]["over"].as_array().unwrap().is_empty());
		assert!(!j_fail["budget"]["mean"]["over"].as_array().unwrap().is_empty());
		assert!(t_fail.contains("worst frame: OVER"));
		assert!(t_fail.contains("steady mean (t>=1.00s): OVER"));
		assert!(t_fail.contains("over budget: timeline failed --fail-over-budget"));

		// 3) Budget between steady draws and burst worst-frame draws fails worst frame ONLY.
		let steady_draws = j_pass["looping"]["steadyState"]["draws"]["mean"].as_f64().unwrap();
		let peak_draws = j_pass["summary"]["draws"]["peak"].as_f64().unwrap();
		assert!(peak_draws > steady_draws, "burst must lift worst-frame draws ({peak_draws}) above steady mean ({steady_draws})");
		let mid = steady_draws.ceil() as u64;
		let out_worst = temp.join("worst_only");
		std::fs::create_dir_all(&out_worst).unwrap();
		let worst_opts = CostOptions { attribute: true, pixels: false, no_images: true, budget: Some(format!("draws={mid}")), fail_over_budget: true, ..Default::default() };
		let rep_worst = run(&renderer, &mut doc, Some(&pb), &worst_opts, &tl_opts, &views, None, None, &out_worst, None, "budget.rbxmx", false, &|_| Ok(())).unwrap();
		assert!(rep_worst.over_budget > 0);
		let j_worst: serde_json::Value = serde_json::from_slice(&std::fs::read(out_worst.join("timeline.json")).unwrap()).unwrap();
		assert!(!j_worst["budget"]["worstFrame"]["over"].as_array().unwrap().is_empty());
		assert!(j_worst["budget"]["mean"].get("over").is_none());

		// 4) Short duration before steady state labels budget mean check as climbMean.
		let out_climb = temp.join("climb_mean");
		std::fs::create_dir_all(&out_climb).unwrap();
		let tl_short = TimelineOptions { start: 0.0, duration: Some(0.5), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		run(&renderer, &mut doc, None, &fail_opts, &tl_short, &views, None, None, &out_climb, None, "budget.rbxmx", false, &|_| Ok(())).unwrap();
		let j_climb: serde_json::Value = serde_json::from_slice(&std::fs::read(out_climb.join("timeline.json")).unwrap()).unwrap();
		let t_climb = std::fs::read_to_string(out_climb.join("analysis.txt")).unwrap();
		assert_eq!(j_climb["budget"]["meanKind"], "climbMean");
		assert!(t_climb.contains("climb-only mean (before steady state): OVER"));
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_explain_compare_top_and_refusals_work_end_to_end() {
		let temp = std::env::temp_dir().join(format!("ror-timeline-flags-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));
		let (mut doc, pb) = build_dynamic_scene();

		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 1280,
				height: 720,
				camera: CameraSpec::LookAt { eye: [0.0, 8.0, 22.0], target: [0.0, 2.0, 0.0], fov: 70.0 },
				..Default::default()
			},
		}];
		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.5), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };

		// --explain + --top honoured.
		let out_a = temp.join("before");
		std::fs::create_dir_all(&out_a).unwrap();
		let opts_a = CostOptions { attribute: true, pixels: false, no_images: true, top: Some(1), explain: vec!["Root.Mover".into()], ..Default::default() };
		run(&renderer, &mut doc, Some(&pb), &opts_a, &tl_opts, &views, None, None, &out_a, None, "test.rbxm", false, &|_| Ok(())).unwrap();
		let j_a: serde_json::Value = serde_json::from_slice(&std::fs::read(out_a.join("timeline.json")).unwrap()).unwrap();
		let t_a = std::fs::read_to_string(out_a.join("analysis.txt")).unwrap();
		assert_eq!(j_a["topInstances"].as_array().unwrap().len(), 1, "--top 1 must cap topInstances");
		let exp: serde_json::Value = serde_json::from_slice(&std::fs::read(out_a.join("explain-Root.Mover.json")).unwrap()).unwrap();
		assert_eq!(exp["schema"], "ror-cost-timeline-explain/1");
		assert_eq!(exp["id"], "Root.Mover");
		assert_eq!(exp["frameCount"], 5);
		assert!(exp["summary"]["peakMs"].as_f64().unwrap() > 0.0);
		assert!(t_a.contains("explain Root.Mover (timeline, 5 frames; explain-Root.Mover.json):"), "{t_a}");

		// Invalid --explain ID errors cleanly instead of silently ignoring.
		let out_bad = temp.join("bad_explain");
		std::fs::create_dir_all(&out_bad).unwrap();
		let opts_bad = CostOptions { attribute: true, pixels: false, no_images: true, explain: vec!["Root.DoesNotExist".into()], ..Default::default() };
		assert!(run(&renderer, &mut doc, Some(&pb), &opts_bad, &tl_opts, &views, None, None, &out_bad, None, "test.rbxm", false, &|_| Ok(())).is_err());

		// --compare compares two timeline runs and warns on rule/preset changes.
		let out_b = temp.join("after");
		std::fs::create_dir_all(&out_b).unwrap();
		let mut pb_smaller = pb.clone();
		pb_smaller.actions[0].emit = Some(Emit::Count(5));
		let tl_b = TimelineOptions { all_emitters_running: true, ..tl_opts.clone() };
		let opts_b = CostOptions { attribute: true, pixels: false, no_images: true, ..Default::default() };
		run(&renderer, &mut doc, Some(&pb_smaller), &opts_b, &tl_b, &views, None, None, &out_b, None, "test.rbxm", false, &|_| Ok(())).unwrap();
		let j_b: serde_json::Value = serde_json::from_slice(&std::fs::read(out_b.join("timeline.json")).unwrap()).unwrap();
		let cmp_lines = compare_timelines(&j_b, &out_a).unwrap();
		let cmp_text = cmp_lines.join("\n");
		assert!(cmp_text.contains("compared timeline with"), "{cmp_text}");
		assert!(cmp_text.contains("offscreen emitters rule changed (paused-out-of-view -> all-running)"), "{cmp_text}");
		assert!(cmp_text.contains("particles mean"), "{cmp_text}");

		// validate_options refuses every unsupported cost-loop / multi-view flag.
		let edits = crate::cost::edits::EditsFile::read(r#"{"edits":[{"op":"hide","targets":["Root.Floor"]}]}"#).unwrap();
		let base_opts = CostOptions::default();
		let cases: Vec<(CostOptions, TimelineOptions, usize, &str)> = vec![
			(CostOptions { edits: Some(edits), ..base_opts.clone() }, tl_opts.clone(), 1, "--edits"),
			(CostOptions { look: true, ..base_opts.clone() }, tl_opts.clone(), 1, "--edits"),
			(CostOptions { export: Some(temp.join("kit")), ..base_opts.clone() }, tl_opts.clone(), 1, "--edits"),
			(CostOptions { items: true, ..base_opts.clone() }, tl_opts.clone(), 1, "--items"),
			(CostOptions { heat: true, ..base_opts.clone() }, tl_opts.clone(), 1, "--heat"),
			(CostOptions { grid: Some(20.0), ..base_opts.clone() }, tl_opts.clone(), 1, "--grid"),
			(CostOptions { all_emitters: true, ..base_opts.clone() }, tl_opts.clone(), 1, "--all-emitters"),
			(base_opts.clone(), tl_opts.clone(), 4, "multi-view"),
			(base_opts, TimelineOptions { times: Some(vec![0.0, 0.5]), ..tl_opts }, 1, "--duration and --times"),
		];
		for (o, t, vc, expected) in cases {
			let err = validate_options(&o, &t, vc).expect_err(&format!("expected refusal containing {expected}"));
			assert!(err.to_string().contains(expected), "expected {expected} in {err}");
		}
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn timeline_summary_figures_match_exact_formulas_in_json_and_text() {
		// Part A: closed-form check of every MetricSummary field for uniform dt, explicit times, tie-breaking, and >100-frame top1PctMean.
		let series = [10.0, 40.0, 20.0, 40.0, 90.0];
		let uniform_times = [0.0, 0.1, 0.2, 0.3, 0.4];
		let u = summarize(&series, &uniform_times, Some(0.1));
		assert!((u.mean - 40.0).abs() < 1e-6, "mean expected 40.0, got {}", u.mean);
		assert!((u.peak - 90.0).abs() < 1e-6);
		assert!((u.peak_time - 0.4).abs() < 1e-6);
		assert_eq!(u.peak_frame, 4);
		assert!((u.p50 - 40.0).abs() < 1e-6);
		assert!((u.p95 - 90.0).abs() < 1e-6);
		assert!((u.p99 - 90.0).abs() < 1e-6);
		assert!((u.top1_pct_mean - 90.0).abs() < 1e-6);
		assert!((u.integral - 20.0).abs() < 1e-6, "uniform integral expected 20.0, got {}", u.integral);

		let uneven_times = [0.0, 0.2, 0.5, 1.0, 1.5];
		let nv = summarize(&series, &uneven_times, None);
		// 10*0.2 + 40*0.3 + 20*0.5 + 40*0.5 + 90*0.5 = 89.0
		assert!((nv.integral - 89.0).abs() < 1e-6, "explicit-times integral expected 89.0, got {}", nv.integral);
		assert!((nv.peak_time - 1.5).abs() < 1e-6);

		// Tie-breaking: earliest peak frame wins.
		let tied = summarize(&[5.0, 20.0, 20.0, 10.0], &[0.0, 0.1, 0.2, 0.3], Some(0.1));
		assert_eq!(tied.peak_frame, 1);
		assert!((tied.peak_time - 0.1).abs() < 1e-6);

		// 101 samples: ceil(0.01 * 101) = 2 -> top1_pct_mean averages the top two (100 and 200 -> 150).
		let mut big = vec![1.0; 99];
		big.push(100.0);
		big.push(200.0);
		let big_times: Vec<f64> = (0..101).map(|i| i as f64 * 0.1).collect();
		let bs = summarize(&big, &big_times, Some(0.1));
		assert!((bs.top1_pct_mean - 150.0).abs() < 1e-6, "expected 150.0, got {}", bs.top1_pct_mean);
		assert!((bs.p99 - 100.0).abs() < 1e-6, "p99 expected 100.0, got {}", bs.p99);
		assert!((bs.mean - (399.0 / 101.0)).abs() < 1e-5, "mean expected {}, got {}", 399.0 / 101.0, bs.mean);

		// Part B: end-to-end timeline::run verification of EVERY metric in timeline.json["summary"] and EVERY printed figure in analysis.txt.
		let temp = std::env::temp_dir().join(format!("ror-timeline-summary-exact-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));
		let (mut doc, pb) = build_dynamic_scene();
		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 2532,
				height: 1170,
				camera: CameraSpec::LookAt { eye: [0.0, 8.0, 22.0], target: [0.0, 2.0, 0.0], fov: 70.0 },
				..Default::default()
			},
		}];
		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.6), auto_duration: false, fps: 10.0, times: None, camera_track: None, keep_default_camera: false, all_emitters_running: false };
		let cost_opts = CostOptions { attribute: true, pixels: true, preset: Some("phone".into()), no_images: true, ..Default::default() };
		let out = temp.join("out");
		std::fs::create_dir_all(&out).unwrap();
		run(&renderer, &mut doc, Some(&pb), &cost_opts, &tl_opts, &views, None, None, &out, None, "test.rbxm", false, &|_| Ok(())).unwrap();

		let j: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("timeline.json")).unwrap()).unwrap();
		let txt = std::fs::read_to_string(out.join("analysis.txt")).unwrap();
		let frames = j["frames"].as_array().unwrap();
		let n = frames.len();
		assert_eq!(n, 6);
		let times: Vec<f64> = frames.iter().map(|f| f["time"].as_f64().unwrap()).collect();

		for metric in ["draws", "triangles", "particles", "particleFill", "blendedViewports", "gpuUnits", "cpuMs", "gpuMs", "ms"] {
			let vals: Vec<f64> = frames.iter().map(|f| f[metric].as_f64().unwrap()).collect();
			let sum: f64 = vals.iter().sum();
			let exp_mean = sum / n as f64;
			let (exp_peak_idx, exp_peak) = vals.iter().enumerate().fold((0usize, vals[0]), |(bi, bv), (i, &v)| if v > bv { (i, v) } else { (bi, bv) });
			let exp_peak_time = times[exp_peak_idx];
			let mut sorted = vals.clone();
			sorted.sort_by(|a, b| a.total_cmp(b));
			let rank = |p: f64| sorted[((p / 100.0 * n as f64).ceil() as usize).clamp(1, n) - 1];
			let exp_p50 = rank(50.0);
			let exp_p95 = rank(95.0);
			let exp_p99 = rank(99.0);
			let exp_top1 = sorted[n - 1];
			let exp_int = sum * 0.1;

			let got = &j["summary"][metric];
			assert!((got["mean"].as_f64().unwrap() - exp_mean).abs() <= 1e-5, "{metric} mean mismatch: got {} exp {exp_mean}", got["mean"]);
			assert!((got["peak"].as_f64().unwrap() - exp_peak).abs() <= 1e-5, "{metric} peak mismatch");
			assert_eq!(got["peakFrame"].as_u64().unwrap() as usize, exp_peak_idx, "{metric} peakFrame mismatch");
			assert!((got["peakTime"].as_f64().unwrap() - exp_peak_time).abs() <= 1e-5, "{metric} peakTime mismatch");
			assert!((got["p50"].as_f64().unwrap() - exp_p50).abs() <= 1e-5, "{metric} p50 mismatch");
			assert!((got["p95"].as_f64().unwrap() - exp_p95).abs() <= 1e-5, "{metric} p95 mismatch");
			assert!((got["p99"].as_f64().unwrap() - exp_p99).abs() <= 1e-5, "{metric} p99 mismatch");
			assert!((got["top1PctMean"].as_f64().unwrap() - exp_top1).abs() <= 1e-5, "{metric} top1PctMean mismatch");
			assert!((got["integral"].as_f64().unwrap() - exp_int).abs() <= 1e-5, "{metric} integral mismatch");
		}

		// Verify every figure printed in analysis.txt matches the exact recomputed summary.
		let s = &j["summary"];
		let expected_ft = format!(
			"frame time: mean {:.2} ms, p95 {:.2} ms, peak {:.2} ms at t={:.3}s (frame {}); integral {:.2} ms*s",
			s["ms"]["mean"].as_f64().unwrap(),
			s["ms"]["p95"].as_f64().unwrap(),
			s["ms"]["peak"].as_f64().unwrap(),
			s["ms"]["peakTime"].as_f64().unwrap(),
			s["ms"]["peakFrame"].as_u64().unwrap(),
			s["ms"]["integral"].as_f64().unwrap(),
		);
		assert!(txt.contains(&expected_ft), "analysis.txt missing exact frame time line:\nexpected: {expected_ft}\ngot:\n{txt}");
		let expected_counts = format!(
			"counts: draws mean {:.1} / peak {:.0} (t={:.3}s); triangles mean {:.0}k / peak {:.0}k; particles mean {:.1} / peak {:.0}; blended mean {:.2} / peak {:.2} vp",
			s["draws"]["mean"].as_f64().unwrap(),
			s["draws"]["peak"].as_f64().unwrap(),
			s["draws"]["peakTime"].as_f64().unwrap(),
			s["triangles"]["mean"].as_f64().unwrap() / 1000.0,
			s["triangles"]["peak"].as_f64().unwrap() / 1000.0,
			s["particles"]["mean"].as_f64().unwrap(),
			s["particles"]["peak"].as_f64().unwrap(),
			s["blendedViewports"]["mean"].as_f64().unwrap(),
			s["blendedViewports"]["peak"].as_f64().unwrap(),
		);
		assert!(txt.contains(&expected_counts), "analysis.txt missing exact counts line:\nexpected: {expected_counts}\ngot:\n{txt}");
		let _ = std::fs::remove_dir_all(&temp);
	}

	#[test]
	fn natural_end_includes_emitter_lifetime_tail_beyond_motion_and_tweens() {
		let mut model = ModelBuilder::new("TailTest");
		let host = model.part(&PartSpec { name: "Host".into(), size: Vec3::ONE, ..Default::default() });
		let a0 = model.child(host, InstanceBuilder::new("Attachment").with_name("A0"));
		let a1 = model.child(host, InstanceBuilder::new("Attachment").with_name("A1"));
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("Burst")
				.with_property("Rate", 0.0f32)
				.with_property("Lifetime", NumberRange::new(1.3, 1.3))
				.with_property("Enabled", false),
		);
		let mut attrs = rbx_dom_weak::types::Attributes::new();
		attrs.insert("EmitDelay".into(), rbx_dom_weak::types::Variant::Float32(0.2));
		attrs.insert("EmitDuration".into(), rbx_dom_weak::types::Variant::Float32(0.3));
		attrs.insert("EmitCount".into(), rbx_dom_weak::types::Variant::Int32(8));
		model.child(
			host,
			InstanceBuilder::new("ParticleEmitter")
				.with_name("AttrEmitter")
				.with_property("Rate", 0.0f32)
				.with_property("Lifetime", NumberRange::new(1.1, 1.1))
				.with_property("Enabled", false)
				.with_property("Attributes", attrs),
		);
		model.child(
			host,
			InstanceBuilder::new("Trail")
				.with_name("TailTrail")
				.with_property("Attachment0", a0)
				.with_property("Attachment1", a1)
				.with_property("Lifetime", 0.9f32)
				.with_property("Enabled", false),
		);
		let doc = model.into_document();

		// Motion ends at 0.20, tween ends at 0.25 — strictly shorter than every lifetime tail below.
		let base_pb = Playback {
			tweens: vec![Tween { target: "TailTest.Host".into(), property: "Transparency".into(), at: 0.05, duration: 0.2, to: Some(serde_json::json!(0.5)), ..Default::default() }],
			motion: vec![Motion {
				target: "TailTest.Host".into(),
				keys: vec![
					MotionKey { at: 0.0, cframe: vec![0.0, 0.0, 0.0], ..Default::default() },
					MotionKey { at: 0.2, cframe: vec![2.0, 0.0, 0.0], ..Default::default() },
				],
				..Default::default()
			}],
			..Default::default()
		};

		// 1) Burst at 0.1 + 1.3 lifetime = 1.4 (dropping +life yields 0.25).
		let mut pb_burst = base_pb.clone();
		pb_burst.actions.push(Action { at: 0.1, target: "TailTest.Host.Burst".into(), emit: Some(Emit::Count(10)), ..Default::default() });
		let end_burst = pb_burst.natural_end(&doc);
		assert!((end_burst - 1.4).abs() < 1e-4, "burst tail must be 0.1 + 1.3 = 1.4, got {end_burst}");

		// 2) Attribute emit at 0.1 + 0.2 delay + 0.3 duration + 1.1 lifetime = 1.7 (dropping +life yields 0.6).
		let mut pb_attr = base_pb.clone();
		pb_attr.actions.push(Action { at: 0.1, target: "TailTest.Host.AttrEmitter".into(), emit: Some(Emit::Mode("attributes".into())), ..Default::default() });
		let end_attr = pb_attr.natural_end(&doc);
		assert!((end_attr - 1.7).abs() < 1e-4, "attribute emit tail must be 0.1 + 0.2 + 0.3 + 1.1 = 1.7, got {end_attr}");

		// 3) Trail enabled at 0.1 + 0.9 lifetime = 1.0 (dropping +life yields 0.25).
		let mut pb_trail = base_pb;
		pb_trail.actions.push(Action { at: 0.1, target: "TailTest.Host.TailTrail".into(), enabled: Some(true), ..Default::default() });
		let end_trail = pb_trail.natural_end(&doc);
		assert!((end_trail - 1.0).abs() < 1e-4, "trail tail must be 0.1 + 0.9 = 1.0, got {end_trail}");
	}

	#[test]
	fn timeline_refuses_out_of_order_times_and_names_missing_textures() {
		let base = CostOptions::default();
		let bad = TimelineOptions { times: Some(vec![2.0, 0.5, 1.0]), ..Default::default() };
		let err = validate_options(&base, &bad, 1).expect_err("out-of-order --times must be refused");
		assert!(err.to_string().contains("non-decreasing order"), "got: {err}");

		let dup_ok = TimelineOptions { times: Some(vec![0.0, 0.5, 0.5, 1.0]), ..Default::default() };
		assert!(validate_options(&base, &dup_ok, 1).is_ok(), "non-decreasing times with duplicate must be accepted");

		// Timeline run with --memory prints missing texture ID on analysis.txt and CostReport.warnings.
		let temp = std::env::temp_dir().join(format!("ror-timeline-missing-tex-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&temp);
		std::fs::create_dir_all(&temp).unwrap();
		let renderer = make_test_renderer(&temp.join("cache"));
		let mut model = ModelBuilder::new("MissingTexTl");
		let wall = model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(6.0, 6.0, 1.0), ..Default::default() });
		model.child(
			wall,
			InstanceBuilder::new("Decal")
				.with_property("Texture", rbx_dom_weak::types::ContentId::from("rbxassetid://99999992"))
				.with_property("Face", rbx_dom_weak::types::Enum::from_u32(5)),
		);
		let mut doc = model.into_document();
		let views = vec![NamedView {
			name: "render".into(),
			view: crate::render::ViewRequest {
				width: 640,
				height: 360,
				camera: CameraSpec::LookAt { eye: [0.0, 0.0, 15.0], target: [0.0; 3], fov: 60.0 },
				..Default::default()
			},
		}];
		let tl_opts = TimelineOptions { times: Some(vec![0.0, 0.5]), ..Default::default() };
		let out = temp.join("out");
		std::fs::create_dir_all(&out).unwrap();
		let rep = run(&renderer, &mut doc, None, &CostOptions { attribute: true, pixels: false, no_images: true, memory: true, ..Default::default() }, &tl_opts, &views, None, None, &out, None, "tex.rbxmx", false, &|_| Ok(())).unwrap();
		let txt = std::fs::read_to_string(out.join("analysis.txt")).unwrap();
		assert!(rep.warnings.iter().any(|w| w.contains("rbxassetid://99999992")), "{:?}", rep.warnings);
		assert!(txt.contains("warning: texture not loaded (bytes uncounted): rbxassetid://99999992"), "{txt}");
		let _ = std::fs::remove_dir_all(&temp);
	}
}
