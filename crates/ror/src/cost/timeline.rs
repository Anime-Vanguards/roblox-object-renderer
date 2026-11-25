//! Multi-frame timeline costing (`ror cost --duration ...` / `--times ...`): steps one loaded
//! document through a playback's moments, reusing decoded meshes and (when `--memory` is requested)
//! cached texture headers across frames, and writes `timeline.json` (`ror-cost-timeline/1`) plus a
//! concise `analysis.txt`.
//!
//! Each frame matches a single-moment `ror cost --playback ... --time <t>` run using the same
//! camera (never copying `--camera-track` into `Playback::viewer`, which would turn on offscreen
//! emitter culling absent from single-moment runs).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use glam::Vec3;
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
	pub pixels_counted: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub memory: Option<MemoryReport>,
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

/// Builds the rounded sample moments `(times, start, duration, fps_opt)`.
pub fn sample_times(opts: &TimelineOptions, playback: &Playback, document: &Document) -> (Vec<f64>, f64, f64, Option<f64>) {
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
		let natural = f64::from(playback.natural_end(document));
		(natural - start).max(1.0 / fps)
	} else {
		f64::from(opts.duration.unwrap_or(2.0)).max(0.0)
	};
	let count = ((dur * fps).round() as usize).clamp(1, MAX_TIMELINE_FRAMES);
	let times: Vec<f64> = (0..count).map(|i| round6(start + i as f64 / fps)).collect();
	(times, round6(start), round6(dur), Some(round6(fps)))
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
	let owned_playback = playback.cloned().unwrap_or_default();
	let (times, start, duration, fps_opt) = sample_times(tl, &owned_playback, document);
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
	let has_track = tl.camera_track.as_deref().is_some_and(|t| !t.is_empty());

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

			let mut spec = if has_track {
				track_camera(tl.camera_track.as_deref().unwrap_or(&[]), t_f32)
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

	let uniform_dt = fps_opt.map(|fps| 1.0 / fps);
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
		Some(summarize(&gpu_s, &times, uniform_dt))
	};

	let summary = TimelineSummary {
		draws: summarize(&draws_s, &times, uniform_dt),
		triangles: summarize(&tris_s, &times, uniform_dt),
		particles: summarize(&particles_s, &times, uniform_dt),
		particle_fill: summarize(&pfill_s, &times, uniform_dt),
		blended_viewports: summarize(&bvp_s, &times, uniform_dt),
		gpu_units: summarize(&gpuu_s, &times, uniform_dt),
		cpu_ms: summarize(&cpu_s, &times, uniform_dt),
		gpu_ms: gpu_ms_summary,
		ms: summarize(&ms_s, &times, uniform_dt),
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
	top_instances.truncate(TOP_INSTANCES);

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
		fidelity: prices::fidelity(&preset, options.pixels),
		unmodeled: prices::unmodeled(&preset, &quality),
		pixels_counted: options.pixels,
		memory: memory.clone(),
		summary,
		top_instances,
		frames,
	};

	crate::output::write_json_atomic(&out.join("timeline.json"), &report, true)?;
	let text_lines = format_lines(&report, desktop);
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
		over_budget: 0,
	})
}

fn format_lines(report: &TimelineReport, desktop: bool) -> Vec<String> {
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
		"cost timeline ({TIMELINE_SCHEMA}): {}, {} frames ({span}), preset {} ({}x{}, {quality_label}{})",
		report.input,
		report.frame_count,
		report.preset.name,
		report.preset.width,
		report.preset.height,
		if report.quality.sun_shadows { ", sun shadows" } else { ", no sun shadows" }
	));
	let s = &report.summary;
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
	if let Some(memory) = &report.memory {
		lines.push(memory::summary_line(memory, !desktop));
	}
	if !report.top_instances.is_empty() {
		lines.push("peak costing instances across timeline:".into());
		for inst in report.top_instances.iter().take(8) {
			lines.push(format!(
				"  peak {:>7.2} ms (t={:.3}s, mean {:>6.2} ms, peak {:>5.2} gpuUnits, peak {:>4.0} draws)  {}",
				inst.peak_ms, inst.peak_time, inst.mean_ms, inst.peak_gpu_units, inst.peak_draws, inst.id
			));
		}
	}
	lines
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
		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.6), auto_duration: false, fps: 10.0, times: None, camera_track: Some(track.clone()) };
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

		let tl_opts = TimelineOptions { start: 0.0, duration: Some(0.3), auto_duration: false, fps: 10.0, times: None, camera_track: None };
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
}
