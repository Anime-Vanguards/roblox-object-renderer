//! Calibration experiments: a fixture model captured in the capture Studio under controlled
//! lighting, with pixel values sampled at known world points (and the same points in ror's render)
//! written to `out/calibrate/<experiment>/samples.json` for fitting (tools/analysis).

mod attacks;
mod bloom;
mod characters;
mod environment;
pub mod exposure;
mod geometry;
mod guis;
mod legacy;
mod lightgrid;
mod lights;
mod particles;
mod post;
mod scroll;
mod shadows;
mod sweep;
pub mod timeline;
mod trails;

use std::path::Path;

use anyhow::{Context, Result, bail};
use glam::Vec3;
use serde::Serialize;

use super::CaptureStudio;
use super::compare::{CaptureSession, derived_images, write_comparison};
use crate::output;
use crate::render::camera::{Camera, CameraSpec};
use crate::render::environment::sun_direction;
use crate::render::{Background, Renderer, ViewRequest};
use crate::scene::build::{ModelBuilder, PartSpec, facing};
use crate::scene::{self, Environment};

#[derive(Clone, Debug, Serialize)]
pub struct Probe {
	pub label: String,
	pub position: [f32; 3],
	/// Free-form inputs for the fit (albedo, n·l, material, ...).
	pub inputs: serde_json::Value,
}

pub struct Run {
	pub label: String,
	pub environment: Environment,
	pub camera: CameraSpec,
}

pub struct Experiment {
	pub name: String,
	pub model: ModelBuilder,
	pub probes: Vec<Probe>,
	pub runs: Vec<Run>,
}

#[derive(Serialize)]
struct Sample {
	probe: String,
	pixel: [f32; 2],
	studio: [f32; 3],
	ror: [f32; 3],
	inputs: serde_json::Value,
}

#[derive(Serialize)]
struct RunResult {
	label: String,
	environment: serde_json::Value,
	camera: [f32; 12],
	fov: f32,
	samples: Vec<Sample>,
}

fn environment_json(environment: &Environment) -> serde_json::Value {
	serde_json::json!({
		"brightness": environment.brightness,
		"ambient": environment.ambient.to_array(),
		"outdoorAmbient": environment.outdoor_ambient.to_array(),
		"clockTime": environment.clock_time,
		"latitude": environment.geographic_latitude,
		"environmentDiffuseScale": environment.environment_diffuse_scale,
		"environmentSpecularScale": environment.environment_specular_scale,
		"exposureCompensation": environment.exposure_compensation,
		"globalShadows": environment.global_shadows,
		"colorShiftTop": environment.color_shift_top.to_array(),
		"colorShiftBottom": environment.color_shift_bottom.to_array(),
		"lightingStyle": environment.lighting_style,
		"fog": [environment.fog_start, environment.fog_end],
		"fogColor": environment.fog_color.to_array(),
		"atmosphere": environment.atmosphere.map(|atmosphere| serde_json::json!({
			"density": atmosphere.density,
			"offset": atmosphere.offset,
			"color": atmosphere.color.to_array(),
			"decay": atmosphere.decay.to_array(),
			"glare": atmosphere.glare,
			"haze": atmosphere.haze,
		})),
		"sky": environment.sky.as_ref().map(|sky| serde_json::json!({
			"skybox": sky.skybox,
			"orientation": sky.skybox_orientation.to_array(),
			"celestialBodiesShown": sky.celestial_bodies_shown,
			"sunAngularSize": sky.sun_angular_size,
			"moonAngularSize": sky.moon_angular_size,
			"starCount": sky.star_count,
		})),
		"effects": environment.effects.iter().map(|effect| format!("{effect:?}")).collect::<Vec<_>>(),
		"clouds": environment.clouds.map(|clouds| serde_json::json!({
			"cover": clouds.cover,
			"density": clouds.density,
			"color": clouds.color.to_array(),
		})),
	})
}

/// Mean RGB over a (2r+1)² window around a pixel.
fn sample(rgba: &[u8], width: u32, height: u32, x: f32, y: f32, radius: i32) -> [f32; 3] {
	let (cx, cy) = (x.floor() as i32, y.floor() as i32);
	let mut sum = [0f32; 3];
	let mut count = 0f32;
	for dy in -radius..=radius {
		for dx in -radius..=radius {
			let (px, py) = (cx + dx, cy + dy);
			if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
				continue;
			}
			let index = ((py as u32 * width + px as u32) * 4) as usize;
			for channel in 0..3 {
				sum[channel] += rgba[index + channel] as f32;
			}
			count += 1.0;
		}
	}
	sum.map(|value| value / count.max(1.0))
}

fn project(camera: &Camera, width: u32, height: u32, point: Vec3) -> Option<[f32; 2]> {
	let clip = camera.view_projection(width as f32 / height as f32) * point.extend(1.0);
	if clip.w <= 0.0 {
		return None;
	}
	let ndc = clip.truncate() / clip.w;
	Some([(ndc.x * 0.5 + 0.5) * width as f32, (0.5 - ndc.y * 0.5) * height as f32])
}

/// Luau an experiment runs in the capture Studio once its model is loaded (e.g. moving its
/// MaterialVariants into MaterialService): a StringValue of this name in the model, so the capture
/// cache keys on it.
const SETUP_SCRIPT: &str = "RorSetup";

fn setup_script(model: &ModelBuilder) -> Option<String> {
	let root = model.dom.get_by_ref(model.model)?;
	root.children().iter().filter_map(|child| model.dom.get_by_ref(*child)).find(|child| child.class == "StringValue" && child.name == SETUP_SCRIPT).and_then(|child| {
		match child.properties.get(&rbx_dom_weak::ustr("Value")) {
			Some(rbx_dom_weak::types::Variant::String(code)) => Some(code.clone()),
			_ => None,
		}
	})
}

#[cfg(windows)]
pub fn run(renderer: &Renderer, experiment: Experiment, out: &Path) -> Result<()> {
	// long exposures play and average a whole timeline (exposure.rs)
	if let Some(exposure) = exposure::get(&experiment.name) {
		return exposure::run(renderer, experiment, &exposure, out);
	}
	// effect sweeps play their batches themselves (sweep.rs)
	if let Some(spec) = sweep::get(&experiment.name) {
		return sweep::run(renderer, &spec, out);
	}
	let bytes = experiment.model.to_rbxm()?;
	let setup = setup_script(&experiment.model);
	// the fixture as captured, for re-rendering or inspecting it offline
	output::write_atomic(&out.join("model.rbxm"), &bytes)?;
	let document = experiment.model.into_document();
	let mut base_scene = scene::extract::extract(&document);
	let bounds = base_scene.framing_bounds();
	// ROR_RERENDER=1: ror's side alone, against each run's studio.png from its last capture
	let rerender = std::env::var_os("ROR_RERENDER").is_some();
	let mut studio = match super::compare::offline_studio() {
		None if !rerender => Some(CaptureStudio::connect()?),
		_ => None,
	};
	let mut session = match studio.as_mut() {
		Some(studio) => CaptureSession::start(studio, &bytes)?,
		None => CaptureSession::offline(&bytes, &super::compare::offline_studio().unwrap_or_default()),
	};
	base_scene.materials.use_2022 = session.materials_2022;
	// offline the setup is part of the model the capture cache keys on
	if let Some(code) = &setup
		&& let Some(studio) = session.studio.as_mut()
	{
		let result = studio.luau(code).context("running the experiment's setup Luau")?;
		eprintln!("ror: calibrate {}: setup {}", experiment.name, result.trim());
	}
	let (width, height) = (session.width, session.height);
	let mut results = Vec::new();
	// `ROR_CALIBRATE_FILTER=a,b`: only the runs whose label contains one of these
	let filter: Vec<String> = std::env::var("ROR_CALIBRATE_FILTER").map(|text| text.split(',').map(str::to_string).filter(|part| !part.is_empty()).collect()).unwrap_or_default();
	for run in &experiment.runs {
		if !filter.is_empty() && !filter.iter().any(|part| run.label.contains(part.as_str())) {
			continue;
		}
		session.set_environment(&run.environment)?;
		let camera = run.camera.place(bounds, width, height);
		// timeline runs play in Studio frame by frame; ror draws the frame Studio showed
		if let Some(timeline) = timeline::get(&experiment.name, &run.label) {
			let directory = out.join(&run.label);
			// the playback both sides play (its time and frames are the captured frame's)
			output::write_atomic(&directory.join("playback.json"), serde_json::to_string_pretty(&timeline.playback)?.as_bytes())?;
			let (captured, record) = if rerender {
				let (_, _, rgba) = output::decode_png(&directory.join("studio.png")).with_context(|| format!("ROR_RERENDER: reading {}", directory.join("studio.png").display()))?;
				let record = std::fs::read_to_string(directory.join("studio.json")).context("ROR_RERENDER: the run's studio.json")?;
				(rgba, serde_json::from_str(&record)?)
			} else {
				let document = scene::load::read_bytes(&bytes)?;
				let key = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ *byte as u64).wrapping_mul(0x100_0000_01b3));
				timeline::capture(&mut session, &camera, &timeline, &document, key, run.environment.exposure_compensation)?
			};
			// ROR_BASELINE=playback: the model as authored, what ror drew before playbacks
			let mut scene = if baseline_has("playback") { scene::extract::extract(&scene::load::read_bytes(&bytes)?) } else { timeline::scene(&bytes, &timeline, &record)? };
			// ROR_BASELINE=scroll: beams at rest, as before they scrolled
			if baseline_has("scroll") {
				for beam in &mut scene.beams {
					beam.texture_offset = 0.0;
				}
			}
			scene.environment = run.environment.clone();
			baseline(&mut scene.environment);
			scene.materials.use_2022 = session.materials_2022;
			let request = ViewRequest { width, height, camera: camera.spec(), background: Background::Sky, ..Default::default() };
			let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request))?;
			for missing in &stats.missing {
				eprintln!("ror: calibrate {}/{}: missing {missing}", experiment.name, run.label);
			}
			let mut image = images.into_iter().next().context("no image")?;
			timeline::blank(&mut image.rgba, width, timeline::marker_rect(&camera, width, height));
			write_comparison(&directory, width, height, &captured, &image.rgba, derived_images())?;
			// the same without particles, trails and beams: what the effects add (vfx_metrics.py)
			let mut bare = scene.clone();
			bare.particles.clear();
			bare.trails.clear();
			bare.beams.clear();
			let (images, _) = renderer.render(&bare, std::slice::from_ref(&request))?;
			if let Some(mut background) = images.into_iter().next() {
				timeline::blank(&mut background.rgba, width, timeline::marker_rect(&camera, width, height));
				output::write_png(&directory.join("background.png"), width, height, &background.rgba, true)?;
			}
			output::write_atomic(&directory.join("studio.json"), serde_json::to_string(&record)?.as_bytes())?;
			eprintln!("ror: calibrate {}/{}: frame at {:.4} s ({} frames)", experiment.name, run.label, record.time, record.frames.len());
			results.push(RunResult { label: run.label.clone(), environment: environment_json(&run.environment), camera: camera.cframe_components(), fov: camera.fov, samples: Vec::new() });
			continue;
		}
		// particle runs emit and freeze their particles first (ror gets the same as bursts), with the
		// camera already on them: Studio does not age particles it does not see
		let setup = particles::setup(&experiment.name, &run.label);
		if let Some(setup) = &setup {
			session.key_setup(&setup.luau);
			// a cached capture needs no setup, a burst of frames always does (offline neither runs)
			if session.studio.is_some() && (burst(&experiment.name).is_some() || !session.cached(&camera)) {
				session.set_camera(&camera)?;
				if let Some(studio) = session.studio.as_mut() {
					studio.luau(&setup.luau).context("running the capture setup")?;
				}
			}
		}
		let (captured, attempts) = if rerender {
			let path = out.join(&run.label).join("studio.png");
			let (_, _, rgba) = output::decode_png(&path).with_context(|| format!("ROR_RERENDER: reading {}", path.display()))?;
			(rgba, 0)
		} else {
			session.capture(&camera)?
		};
		let mut scene = base_scene.clone();
		scene.environment = run.environment.clone();
		baseline(&mut scene.environment);
		if let Some(setup) = &setup {
			setup.apply(&mut scene);
		}
		// the camera Studio captured from, exactly (ror's own framing widens the bounds by what
		// meshes draw)
		let request = ViewRequest { width, height, camera: camera.spec(), background: Background::Sky, ..Default::default() };
		let (images, stats) = renderer.render(&scene, std::slice::from_ref(&request))?;
		for missing in &stats.missing {
			eprintln!("ror: calibrate {}/{}: missing {missing}", experiment.name, run.label);
		}
		for warning in &stats.warnings {
			eprintln!("ror: calibrate {}/{}: {warning}", experiment.name, run.label);
		}
		let image = images.into_iter().next().context("no image")?;
		write_comparison(&out.join(&run.label), width, height, &captured, &image.rgba, derived_images())?;
		// `ROR_CALIBRATE_SOURCE=1`: what ror's bloom reads, the exposed radiance x without the tone
		// curve, as √(x / 16) and √(x / 2) (`linear16.png`, `linear2.png`; tools/analysis/bloom_fit.py)
		if std::env::var_os("ROR_CALIBRATE_SOURCE").is_some() {
			for scale in [16u32, 2] {
				let request = ViewRequest { linear_scale: Some(1.0 / scale as f32), ..request.clone() };
				let (images, _) = renderer.render(&scene, std::slice::from_ref(&request))?;
				let image = images.into_iter().next().context("no image")?;
				output::write_png(&out.join(&run.label).join(format!("linear{scale}.png")), width, height, &image.rgba, true)?;
			}
		}
		// random particles: ror's mean over several snapshots, against Studio's burst of frames
		if let Some(setup) = &setup {
			setup.mean_renders(renderer, &scene, &request, &out.join(&run.label))?;
		}
		// offline the frames of the last run with a Studio stay
		if let Some((count, interval)) = burst(&experiment.name)
			&& session.studio.is_some()
		{
			let frames = session.capture_frames(&camera, count, interval)?;
			for (index, (_, frame)) in frames.iter().enumerate() {
				output::write_png(&out.join(&run.label).join(format!("frame{index}.png")), width, height, frame, true)?;
			}
			let times: Vec<f64> = frames.iter().map(|(time, _)| *time).collect();
			output::write_atomic(&out.join(&run.label).join("frames.json"), serde_json::to_string(&times)?.as_bytes())?;
		}
		let mut samples = Vec::new();
		for probe in &experiment.probes {
			let Some(pixel) = project(&camera, width, height, Vec3::from(probe.position)) else {
				continue;
			};
			samples.push(Sample {
				probe: probe.label.clone(),
				pixel,
				studio: sample(&captured, width, height, pixel[0], pixel[1], 3),
				ror: sample(&image.rgba, width, height, pixel[0], pixel[1], 3),
				inputs: probe.inputs.clone(),
			});
		}
		eprintln!("ror: calibrate {}/{}: {} captures, {} samples", experiment.name, run.label, attempts, samples.len());
		results.push(RunResult {
			label: run.label.clone(),
			environment: environment_json(&run.environment),
			camera: camera.cframe_components(),
			fov: camera.fov,
			samples,
		});
	}
	output::write_atomic(&out.join("samples.json"), serde_json::to_string_pretty(&results)?.as_bytes())?;
	Ok(())
}

/// Whether `ROR_BASELINE` names `part` (`playback`: timeline runs draw the model as authored, each
/// emitter's steady state, no trails or motion, as ror did before playbacks; `scroll`: beams at rest).
fn baseline_has(part: &str) -> bool {
	std::env::var("ROR_BASELINE").is_ok_and(|list| list.split(',').any(|item| item.trim() == part))
}

/// `ROR_BASELINE=atmosphere,effects`: ror's side of a run leaves those parts of the environment out
/// (what ror drew before it rendered them), for before / after numbers with `ROR_RERENDER=1`.
fn baseline(environment: &mut Environment) {
	let Ok(list) = std::env::var("ROR_BASELINE") else {
		return;
	};
	for item in list.split(',').map(str::trim) {
		match item {
			"atmosphere" => environment.atmosphere = None,
			"effects" => environment.effects.clear(),
			"playback" | "scroll" => {}
			_ => eprintln!("ror: ROR_BASELINE: unknown part {item:?} (atmosphere, effects, playback, scroll)"),
		}
	}
}

/// Experiments whose subject may animate: every run also captures `count` frames `interval` ms
/// apart (`<run>/frame<k>.png`), the time course the settled capture cannot show.
fn burst(name: &str) -> Option<(u32, u64)> {
	match name {
		"forcefield-textures" | "forcefield-pattern" | "forcefield-specialmesh" => Some((12, 300)),
		"forcefield-time" => Some((260, 20)),
		name if name.starts_with("legacy-") => legacy::burst(name),
		_ => particles::burst(name),
	}
}

/// Lighting with only the sun: no ambient, no sky light, no shadows.
pub fn sun_only(brightness: f32) -> Environment {
	Environment {
		brightness,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// Tiles facing the sun (gray ramps, colours) and tiles tilted away from it (n·l falloff), seen
/// from far away through a narrow field of view so every tile has the same view direction (and so
/// the same specular).
pub fn sun_board(runs: &[(f32, f32)]) -> Experiment {
	let environment = sun_only(1.0);
	let sun = sun_direction(environment.clock_time, environment.geographic_latitude);
	let u = sun.cross(Vec3::Y).normalize();
	let v = u.cross(sun).normalize();
	let mut model = ModelBuilder::new("SunBoard");
	let mut probes = Vec::new();
	let tile = 2.0;
	let pitch = 2.5;
	let add = |model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, row: f32, column: f32, normal: Vec3, color: [u8; 3], inputs: serde_json::Value| {
		let center = u * (column - 7.5) * pitch + v * (2.0 - row) * pitch;
		let spec = PartSpec { name: label.clone(), cframe: facing(center, normal), size: Vec3::new(tile, tile, 0.2), color, material: 272, ..Default::default() };
		model.part(&spec);
		probes.push(Probe { label, position: (center + normal * 0.1).into(), inputs });
	};
	for index in 0..16 {
		let gray = (index * 17) as u8;
		add(&mut model, &mut probes, format!("gray{gray}"), 0.0, index as f32, sun, [gray; 3], serde_json::json!({"color": [gray, gray, gray], "ndotl": 1.0}));
	}
	let darks: [u8; 16] = [2, 4, 6, 8, 10, 12, 14, 20, 25, 30, 40, 50, 60, 75, 95, 110];
	for (index, gray) in darks.iter().enumerate() {
		add(&mut model, &mut probes, format!("dark{gray}"), 1.0, index as f32, sun, [*gray; 3], serde_json::json!({"color": [gray, gray, gray], "ndotl": 1.0}));
	}
	let colors: [[u8; 3]; 16] = [
		[255, 0, 0],
		[0, 255, 0],
		[0, 0, 255],
		[255, 255, 0],
		[0, 255, 255],
		[255, 0, 255],
		[128, 0, 0],
		[0, 128, 0],
		[0, 0, 128],
		[163, 162, 165],
		[196, 40, 28],
		[13, 105, 172],
		[75, 151, 75],
		[245, 205, 48],
		[27, 42, 53],
		[242, 243, 243],
	];
	for (index, color) in colors.iter().enumerate() {
		add(&mut model, &mut probes, format!("color{index}"), 2.0, index as f32, sun, *color, serde_json::json!({"color": color, "ndotl": 1.0}));
	}
	// tilted diagonally (towards u + v): faces stay visible from the camera and no tile mirrors the
	// sun into it
	let diagonal = (u + v).normalize();
	for index in 0..16 {
		let angle = (index as f32 * 6.0).min(89.0).to_radians();
		let normal = (sun * angle.cos() + diagonal * angle.sin()).normalize();
		add(
			&mut model,
			&mut probes,
			format!("tilt{index}"),
			3.0,
			index as f32,
			normal,
			[255; 3],
			serde_json::json!({"color": [255, 255, 255], "ndotl": normal.dot(sun)}),
		);
	}
	let view_direction = (sun * 50f32.to_radians().cos() + v * 50f32.to_radians().sin()).normalize();
	let yaw = view_direction.x.atan2(-view_direction.z).to_degrees();
	let pitch_angle = view_direction.y.asin().to_degrees();
	let camera = CameraSpec::Orbit { yaw, pitch: pitch_angle, distance: Some(190.0), padding: 0.0, target: Some([0.0, 0.0, 0.0]), fov: 10.0 };
	let runs = runs
		.iter()
		.map(|&(brightness, exposure)| Run {
			label: if exposure == 0.0 { format!("b{brightness}") } else { format!("b{brightness}-ev{exposure}") },
			environment: Environment { exposure_compensation: exposure, ..sun_only(brightness) },
			camera: camera.clone(),
		})
		.collect();
	Experiment { name: "sun-board".into(), model, probes, runs }
}

/// Gray cubes lit only by Ambient / OutdoorAmbient (sun at Brightness 0, moon below the horizon,
/// no sky light), seen from two opposite diagonals so all six face directions are measured.
pub fn ambient_cubes() -> Experiment {
	let mut model = ModelBuilder::new("AmbientCubes");
	let mut probes = Vec::new();
	let grays: [u8; 6] = [255, 204, 153, 102, 51, 0];
	let normals = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	for (index, gray) in grays.iter().enumerate() {
		let center = Vec3::new((index as f32 - 2.5) * 4.0, 0.0, 0.0);
		let spec = PartSpec { name: format!("cube{gray}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(2.0), color: [*gray; 3], material: 272, ..Default::default() };
		model.part(&spec);
		for normal in normals {
			probes.push(Probe {
				label: format!("cube{gray}_{}", ["px", "py", "pz", "nx", "ny", "nz"][normals.iter().position(|n| *n == normal).unwrap_or(0)]),
				position: (center + normal * 1.0).into(),
				inputs: serde_json::json!({"color": [gray, gray, gray], "normal": normal.to_array()}),
			});
		}
	}
	let dark = |ambient: f32, outdoor: f32| Environment {
		brightness: 0.0,
		ambient: Vec3::splat(ambient),
		outdoor_ambient: Vec3::splat(outdoor),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let views = [("a", 35.0f32, 30.0f32), ("b", 215.0, -30.0)];
	let mut runs = Vec::new();
	for (view, yaw, pitch) in views {
		let camera = CameraSpec::Orbit { yaw, pitch, distance: Some(170.0), padding: 0.0, target: Some([0.0, 0.0, 0.0]), fov: 10.0 };
		for level in [0.1f32, 0.25, 0.5, 0.75, 1.0] {
			runs.push(Run { label: format!("{view}-ambient{level}"), environment: dark(level, 0.0), camera: camera.clone() });
			runs.push(Run { label: format!("{view}-outdoor{level}"), environment: dark(0.0, level), camera: camera.clone() });
		}
		runs.push(Run { label: format!("{view}-both0.5"), environment: dark(0.5, 0.5), camera: camera.clone() });
		runs.push(Run {
			label: format!("{view}-ambient-red"),
			environment: Environment { ambient: Vec3::new(0.8, 0.2, 0.0), ..dark(0.0, 0.0) },
			camera: camera.clone(),
		});
		runs.push(Run {
			label: format!("{view}-outdoor-red"),
			environment: Environment { outdoor_ambient: Vec3::new(0.8, 0.2, 0.0), ..dark(0.0, 0.0) },
			camera: camera.clone(),
		});
	}
	Experiment { name: "ambient-cubes".into(), model, probes, runs }
}

/// Neon tiles (three sizes, gray levels and colours) spaced apart in front of a black, unlit
/// backdrop: their own values and their bloom halos.
pub fn neon_rows() -> Experiment {
	let mut model = ModelBuilder::new("NeonRows");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)),
		size: Vec3::new(400.0, 200.0, 1.0),
		color: [0, 0, 0],
		material: 272,
		..Default::default()
	});
	let colors: [[u8; 3]; 8] = [[255, 255, 255], [200, 200, 200], [150, 150, 150], [100, 100, 100], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 170, 0]];
	for (row, size) in [(0, 1.0f32), (1, 2.0), (2, 4.0)] {
		for (column, color) in colors.iter().enumerate() {
			let center = Vec3::new((column as f32 - 3.5) * 12.0, (1.0 - row as f32) * 12.0, 0.0);
			let label = format!("neon{size}_{column}");
			model.part(&PartSpec {
				name: label.clone(),
				cframe: glam::Affine3A::from_translation(center),
				size: Vec3::new(size, size, 0.2),
				color: *color,
				material: 288,
				cast_shadow: false,
				..Default::default()
			});
			probes.push(Probe { label, position: (center - Vec3::Z * 0.1).into(), inputs: serde_json::json!({"color": color, "size": size}) });
		}
	}
	let dark = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	// in front of the row (on its -Z side), looking +Z
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -52.0], target: [0.0, 0.0, 0.0], fov: 70.0 };
	let runs = vec![
		Run { label: "dark".into(), environment: dark.clone(), camera: camera.clone() },
		Run { label: "sun3".into(), environment: Environment { brightness: 3.0, ..dark.clone() }, camera: camera.clone() },
		Run { label: "ambient1".into(), environment: Environment { ambient: Vec3::ONE, ..dark.clone() }, camera: camera.clone() },
		Run { label: "dark-ev-2".into(), environment: Environment { exposure_compensation: -2.0, ..dark.clone() }, camera: camera.clone() },
		Run { label: "dark-ev2".into(), environment: Environment { exposure_compensation: 2.0, ..dark }, camera },
	];
	Experiment { name: "neon-rows".into(), model, probes, runs }
}

/// Part shapes up close (Ball, Cylinder, Wedge, CornerWedge, a block for reference, SpecialMesh
/// shapes) in front of a black backdrop, lit by the sun and a flat ambient, from several sides:
/// tessellation, orientation and SpecialMesh scaling.
pub fn shapes_close() -> Experiment {
	let mut model = ModelBuilder::new("ShapesClose");
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -6.0, 0.0)),
		size: Vec3::new(200.0, 1.0, 200.0),
		color: [0, 0, 0],
		material: 272,
		..Default::default()
	});
	let place = |x: f32, z: f32| glam::Affine3A::from_translation(Vec3::new(x, 0.0, z));
	let shapes: [(&str, &'static str, Option<u32>, [u8; 3], f32, f32); 6] = [
		("Ball", "Part", Some(0), [200, 60, 60], -9.0, 0.0),
		("Cylinder", "Part", Some(2), [60, 200, 60], -3.0, 0.0),
		("Wedge", "WedgePart", None, [60, 60, 200], 3.0, 0.0),
		("CornerWedge", "CornerWedgePart", None, [200, 200, 60], 9.0, 0.0),
		("Block", "Part", Some(1), [200, 200, 200], -6.0, 7.0),
		("TallCylinder", "Part", Some(2), [60, 200, 200], 6.0, 7.0),
	];
	for (name, class, shape, color, x, z) in shapes {
		let size = if name == "TallCylinder" { Vec3::new(5.0, 3.0, 3.0) } else { Vec3::splat(4.0) };
		model.part(&PartSpec { class, name: name.into(), cframe: place(x, z), size, color, material: 272, shape, ..Default::default() });
	}
	let head = model.part(&PartSpec { name: "Head".into(), cframe: place(0.0, 7.0), size: Vec3::new(2.0, 1.0, 1.0), color: [230, 180, 140], material: 272, ..Default::default() });
	model.child(
		head,
		rbx_dom_weak::InstanceBuilder::new("SpecialMesh")
			.with_name("Mesh")
			.with_property("MeshType", rbx_dom_weak::types::Enum::from_u32(0))
			.with_property("Scale", rbx_dom_weak::types::Vector3::new(1.25, 1.25, 1.25)),
	);
	let lit = Environment {
		brightness: 2.0,
		ambient: Vec3::splat(0.35),
		outdoor_ambient: Vec3::splat(0.35),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut runs = Vec::new();
	for (label, yaw, pitch) in [("front", 0.0f32, 15.0f32), ("right", 90.0, 15.0), ("back", 180.0, 15.0), ("left", 270.0, 15.0), ("top", 0.0, 80.0)] {
		runs.push(Run {
			label: label.into(),
			environment: lit.clone(),
			camera: CameraSpec::Orbit { yaw, pitch, distance: Some(30.0), padding: 0.0, target: Some([0.0, 0.0, 3.0]), fov: 50.0 },
		});
	}
	Experiment { name: "shapes-close".into(), model, probes: Vec::new(), runs }
}

/// SpecialMesh sizing: Head / Sphere / Cylinder / Torso / Brick / Wedge meshes on parts of different
/// Size and Scale, one unique flat colour each (ambient light only), seen near-orthographically from
/// the front and the top; `tools/analysis/specialmesh_sizes.py` measures each colour's extent.
pub fn specialmesh_sizes() -> Experiment {
	let mut model = ModelBuilder::new("SpecialMeshSizes");
	let mut probes = Vec::new();
	// (mesh type, part size, mesh scale)
	let cases: [(u32, [f32; 3], [f32; 3]); 16] = [
		(0, [2.0, 1.0, 1.0], [1.25, 1.25, 1.25]),
		(0, [2.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(0, [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(0, [2.0, 2.0, 2.0], [1.0, 1.0, 1.0]),
		(0, [4.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(0, [1.0, 2.0, 1.0], [1.0, 1.0, 1.0]),
		(0, [1.0, 1.0, 3.0], [1.0, 1.0, 1.0]),
		(0, [2.0, 1.0, 1.0], [2.0, 1.0, 1.0]),
		(3, [2.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(3, [1.0, 2.0, 3.0], [1.0, 1.0, 1.0]),
		(4, [2.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(4, [1.0, 2.0, 3.0], [1.0, 1.0, 1.0]),
		(1, [2.0, 2.0, 1.0], [1.0, 1.0, 1.0]),
		(1, [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
		(6, [1.0, 2.0, 3.0], [1.0, 1.0, 1.0]),
		(2, [1.0, 2.0, 3.0], [1.0, 1.0, 1.0]),
	];
	for (index, (mesh_type, size, scale)) in cases.iter().enumerate() {
		// a 4 x 4 grid on a diagonal: separate both from the front (x, y) and from the top (x, z)
		let column = (index % 4) as f32;
		let row = (index / 4) as f32;
		let center = Vec3::new((1.5 - column) * 7.0, (row - 1.5) * 6.0, (row - 1.5) * 6.0);
		// distinct colours: index in two channels, constant third
		let color = [40 + (index as u8 % 4) * 60, 40 + (index as u8 / 4) * 60, 200];
		let label = format!("case{index}");
		let part = model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::from(*size), color, material: 272, ..Default::default() });
		model.child(
			part,
			rbx_dom_weak::InstanceBuilder::new("SpecialMesh")
				.with_name("Mesh")
				.with_property("MeshType", rbx_dom_weak::types::Enum::from_u32(*mesh_type))
				.with_property("Scale", rbx_dom_weak::types::Vector3::new(scale[0], scale[1], scale[2])),
		);
		probes.push(Probe { label, position: center.into(), inputs: serde_json::json!({"meshType": mesh_type, "size": size, "scale": scale, "color": color}) });
	}
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::splat(0.7),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let runs = vec![
		Run { label: "front".into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -200.0], target: [0.0, 0.0, 0.0], fov: 10.0 } },
		Run { label: "top".into(), environment: flat, camera: CameraSpec::LookAt { eye: [0.0, 200.0, 0.001], target: [0.0, 0.0, 0.0], fov: 10.0 } },
	];
	Experiment { name: "specialmesh-sizes".into(), model, probes, runs }
}

/// Tessellation: one big Ball or Cylinder at a time, flat-lit, seen exactly along an axis through
/// a narrow field of view; the silhouette's corners give the segment counts
/// (`tools/analysis/tessellation.py`).
pub fn tessellation(shape: u32) -> Experiment {
	let mut model = ModelBuilder::new("Tessellation");
	model.part(&PartSpec { name: "Shape".into(), size: Vec3::splat(10.0), color: [255, 255, 255], material: 272, shape: Some(shape), ..Default::default() });
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.6),
		outdoor_ambient: Vec3::splat(0.6),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let views: [(&str, [f32; 3]); 3] = [("x", [120.0, 0.0, 0.0]), ("y", [0.0, 120.0, 0.0001]), ("z", [0.0, 0.0, -120.0])];
	let runs = views
		.iter()
		.map(|(label, eye)| Run { label: (*label).into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: *eye, target: [0.0, 0.0, 0.0], fov: 6.0 } })
		.collect();
	Experiment { name: if shape == 0 { "tessellation-ball".into() } else { "tessellation-cylinder".into() }, model, probes: Vec::new(), runs }
}

/// Sun shadows: a wall casting a straight shadow edge onto a white floor; probes cross the edge at
/// several distances along it, for several ShadowSoftness values.
/// Shadows where parts touch (the R15 compare: ror's filter darkens the feet's sunlit tops beside
/// the legs, Studio does not): under the sun alone with shadows on, a white floor with a gray block
/// standing on it, a pole standing on a slab (a leg on a foot) and a plate floating 0.3 studs above
/// the floor; probe lines across the lit floor and faces at the contacts (sun side) and across the
/// shadows' start at the bases (far side), at ShadowSoftness 0.2 (the default) and 1.
pub fn shadow_contact() -> Experiment {
	let sun = sun_direction(Environment::default().clock_time, Environment::default().geographic_latitude);
	let mut model = ModelBuilder::new("ShadowContact");
	let part = |model: &mut ModelBuilder, name: &str, at: Vec3, size: Vec3, color: u8| {
		model.part(&PartSpec { name: name.into(), cframe: glam::Affine3A::from_translation(at), size, color: [color; 3], material: 272, ..Default::default() });
	};
	part(&mut model, "Floor", Vec3::new(0.0, -0.5, 0.0), Vec3::new(80.0, 1.0, 80.0), 255);
	part(&mut model, "Block", Vec3::new(0.0, 2.0, 0.0), Vec3::splat(4.0), 160);
	part(&mut model, "Slab", Vec3::new(8.0, 0.15, 0.0), Vec3::new(1.2, 0.3, 1.2), 200);
	part(&mut model, "Pole", Vec3::new(8.0, 2.8, 0.0), Vec3::new(0.5, 5.0, 0.5), 160);
	part(&mut model, "Plate", Vec3::new(-8.0, 0.4, 0.0), Vec3::new(4.0, 0.2, 4.0), 160);
	let mut probes = Vec::new();
	let mut line = |label: &str, start: Vec3, step: Vec3, count: usize| {
		for index in 0..count {
			let distance = 0.02 + index as f32 * 0.04;
			probes.push(Probe { label: format!("{label}_{distance:.2}"), position: (start + step * distance).into(), inputs: serde_json::json!({"line": label, "distance": distance}) });
		}
	};
	// sun side (the sun is towards -X, +Y, -Z): the floor in front of the block and the slab's top in
	// front of the pole, both lit; the block's front face above the floor
	line("floor-front", Vec3::new(0.0, 0.001, -2.0), Vec3::NEG_Z, 40);
	line("floor-left", Vec3::new(-2.0, 0.001, 0.0), Vec3::NEG_X, 40);
	line("block-front", Vec3::new(0.0, 0.0, -2.001), Vec3::Y, 40);
	line("slab-front", Vec3::new(8.0, 0.301, -0.25), Vec3::NEG_Z, 9);
	line("slab-left", Vec3::new(7.75, 0.301, 0.0), Vec3::NEG_X, 9);
	// far side: where the shadows start, and under the plate
	line("floor-right", Vec3::new(2.0, 0.001, 0.0), Vec3::X, 60);
	line("floor-back", Vec3::new(0.0, 0.001, 2.0), Vec3::Z, 60);
	line("slab-right", Vec3::new(8.25, 0.301, 0.0), Vec3::X, 9);
	line("under-plate", Vec3::new(-10.5, 0.001, 0.0), Vec3::X, 100);
	let sun_only_shadows = |softness: f32| Environment { global_shadows: true, shadow_softness: softness, ..sun_only(2.0) };
	let target = Vec3::new(0.0, 1.0, 0.0);
	let views = [("sun", (Vec3::new(sun.x, 0.0, sun.z).normalize() + Vec3::new(0.0, 1.0, 0.0)).normalize()), ("far", Vec3::new(0.55, 0.75, 0.37).normalize())];
	let mut runs = Vec::new();
	for softness in [0.2f32, 1.0] {
		for (view, direction) in views {
			let camera = CameraSpec::LookAt { eye: (target + direction * 45.0).into(), target: target.into(), fov: 30.0 };
			runs.push(Run { label: format!("{view}-s{softness}"), environment: sun_only_shadows(softness), camera });
		}
	}
	Experiment { name: "shadow-contact".into(), model, probes, runs }
}

pub fn shadow_edge() -> Experiment {
	let base = Environment {
		brightness: 2.0,
		ambient: Vec3::splat(0.3),
		outdoor_ambient: Vec3::splat(0.3),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: true,
		..Environment::default()
	};
	let sun = sun_direction(base.clock_time, base.geographic_latitude);
	let mut model = ModelBuilder::new("ShadowEdge");
	model.part(&PartSpec {
		name: "Floor".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)),
		size: Vec3::new(80.0, 1.0, 80.0),
		color: [255, 255, 255],
		material: 272,
		..Default::default()
	});
	let height = 8.0;
	model.part(&PartSpec {
		name: "Wall".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, height * 0.5, 0.0)),
		size: Vec3::new(1.0, height, 30.0),
		color: [120, 120, 120],
		material: 272,
		..Default::default()
	});
	// the wall's top far edge (x = 0.5, y = height) projects along -sun onto the floor
	let edge_x = 0.5 + height * (-sun.x) / sun.y;
	let mut probes = Vec::new();
	for z in [-8.0f32, -4.0, 0.0, 4.0] {
		for step in -30..=30 {
			let offset = step as f32 * 0.05;
			probes.push(Probe {
				label: format!("z{z}_d{offset:.2}"),
				position: [edge_x + offset, 0.001, z],
				inputs: serde_json::json!({"z": z, "offset": offset}),
			});
		}
	}
	let camera = CameraSpec::LookAt { eye: [edge_x + 6.0, 14.0, -18.0], target: [edge_x, 0.0, -2.0], fov: 40.0 };
	let runs = [0.0f32, 0.2, 0.5, 1.0]
		.iter()
		.map(|&softness| Run { label: format!("soft{softness}"), environment: Environment { shadow_softness: softness, ..base.clone() }, camera: camera.clone() })
		.collect();
	Experiment { name: "shadow-edge".into(), model, probes, runs }
}

/// The darkness every bloom / neon experiment starts from: no sun, ambient or sky light.
fn dark() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// Bloom: small gray Neon sources whose emission spans the bloom threshold (Neon emits
/// x = 25 · c⁴ for a gray c, notes/render.md), and white sources of growing size, far apart on a
/// black backdrop; at three exposures.
pub fn bloom_probe() -> Experiment {
	let mut model = ModelBuilder::new("BloomProbe");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)),
		size: Vec3::new(400.0, 200.0, 1.0),
		color: [0, 0, 0],
		material: 272,
		..Default::default()
	});
	let mut tile = |model: &mut ModelBuilder, label: String, center: Vec3, size: f32, gray: u8, emission: f32| {
		model.part(&PartSpec {
			name: label.clone(),
			cframe: glam::Affine3A::from_translation(center),
			size: Vec3::new(size, size, 0.2),
			color: [gray; 3],
			material: 288,
			cast_shadow: false,
			..Default::default()
		});
		probes.push(Probe { label, position: (center - Vec3::Z * 0.1).into(), inputs: serde_json::json!({"gray": gray, "size": size, "emission": emission}) });
	};
	let emissions = [2.0f32, 3.0, 4.0, 5.0, 6.0, 8.0, 12.0, 25.0];
	for (column, emission) in emissions.iter().enumerate() {
		let gray = ((emission / 25.0).powf(0.25) * 255.0).round().min(255.0) as u8;
		tile(&mut model, format!("level{column}"), Vec3::new((column as f32 - 3.5) * 12.0, 8.0, 0.0), 1.0, gray, *emission);
	}
	for (column, size) in [0.5f32, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0].iter().enumerate() {
		tile(&mut model, format!("size{column}"), Vec3::new((column as f32 - 3.0) * 13.0, -8.0, 0.0), *size, 255, 25.0);
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -52.0], target: [0.0, 0.0, 0.0], fov: 70.0 };
	let runs = [0.0f32, -1.0, 1.0]
		.iter()
		.map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera.clone() })
		.collect();
	Experiment { name: "bloom-probe".into(), model, probes, runs }
}

/// Sun shadows in depth: walls of four heights (shadow edges at four distances from their
/// occluder), eight ShadowSoftness values, and two cameras framing the same area from 35 and 100
/// studs (a view-dependent shadow resolution would show as different widths).
pub fn shadow_steps() -> Experiment {
	let base = Environment {
		brightness: 2.0,
		ambient: Vec3::splat(0.3),
		outdoor_ambient: Vec3::splat(0.3),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: true,
		..Environment::default()
	};
	let sun = sun_direction(base.clock_time, base.geographic_latitude);
	let mut model = ModelBuilder::new("ShadowSteps");
	model.part(&PartSpec {
		name: "Floor".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)),
		size: Vec3::new(160.0, 1.0, 160.0),
		color: [255, 255, 255],
		material: 272,
		..Default::default()
	});
	let mut probes = Vec::new();
	for (index, height) in [1.0f32, 2.0, 4.0, 8.0].iter().enumerate() {
		let center_z = -22.0 + index as f32 * 14.0;
		model.part(&PartSpec {
			name: format!("Wall{height}"),
			cframe: glam::Affine3A::from_translation(Vec3::new(0.0, height * 0.5, center_z)),
			size: Vec3::new(1.0, *height, 10.0),
			color: [120, 120, 120],
			material: 272,
			..Default::default()
		});
		// the shadow of the wall top's far edge, at the shadow of the wall's middle
		let edge_x = 0.5 + height * (-sun.x) / sun.y;
		let z = center_z + height * (-sun.z) / sun.y;
		for step in -40..=40 {
			let offset = step as f32 * 0.04;
			probes.push(Probe {
				label: format!("h{height}_d{offset:.2}"),
				position: [edge_x + offset, 0.001, z],
				inputs: serde_json::json!({"height": height, "offset": offset}),
			});
		}
	}
	let target = Vec3::new(3.5, 0.0, 0.0);
	let mut runs = Vec::new();
	for (camera_label, distance, fov) in [("near", 35.0f32, 70.0f32), ("far", 100.0, 26.0)] {
		let direction = Vec3::new(0.45, 0.75, -0.48).normalize();
		let eye = target + direction * distance;
		let camera = CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov };
		for softness in [0.0f32, 0.05, 0.1, 0.2, 0.3, 0.5, 0.75, 1.0] {
			runs.push(Run { label: format!("{camera_label}-soft{softness}"), environment: Environment { shadow_softness: softness, ..base.clone() }, camera: camera.clone() });
		}
	}
	Experiment { name: "shadow-steps".into(), model, probes, runs }
}

/// Sky light: gray cubes and spheres lit only by the default sky (sun at Brightness 0, no Ambient /
/// OutdoorAmbient), EnvironmentDiffuseScale and EnvironmentSpecularScale swept, seen from two
/// opposite diagonals so every cube face direction is measured.
pub fn sky_light() -> Experiment {
	let mut model = ModelBuilder::new("SkyLight");
	let mut probes = Vec::new();
	let grays: [u8; 3] = [255, 160, 0];
	let normals = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	for (index, gray) in grays.iter().enumerate() {
		let center = Vec3::new((index as f32 - 1.0) * 5.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("cube{gray}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(3.0), color: [*gray; 3], material: 272, ..Default::default() });
		for (face, normal) in normals.iter().enumerate() {
			probes.push(Probe {
				label: format!("cube{gray}_{}", ["px", "py", "pz", "nx", "ny", "nz"][face]),
				position: (center + *normal * 1.5).into(),
				inputs: serde_json::json!({"color": [gray, gray, gray], "normal": normal.to_array()}),
			});
		}
		let sphere_center = Vec3::new((index as f32 - 1.0) * 5.0, 5.0, 0.0);
		model.part(&PartSpec { name: format!("ball{gray}"), cframe: glam::Affine3A::from_translation(sphere_center), size: Vec3::splat(3.0), color: [*gray; 3], material: 272, shape: Some(0), ..Default::default() });
	}
	let sky_only = |diffuse: f32, specular: f32| Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: diffuse,
		environment_specular_scale: specular,
		global_shadows: false,
		..Environment::default()
	};
	let mut runs = Vec::new();
	for (view, yaw, pitch) in [("a", 35.0f32, 30.0f32), ("b", 215.0, -30.0)] {
		let camera = CameraSpec::Orbit { yaw, pitch, distance: Some(60.0), padding: 0.0, target: Some([0.0, 2.5, 0.0]), fov: 20.0 };
		for (diffuse, specular) in [(1.0f32, 0.0f32), (0.5, 0.0), (0.25, 0.0), (0.0, 1.0), (0.0, 0.5), (1.0, 1.0)] {
			runs.push(Run { label: format!("{view}-d{diffuse}-s{specular}"), environment: sky_only(diffuse, specular), camera: camera.clone() });
		}
	}
	Experiment { name: "sky-light".into(), model, probes, runs }
}

/// How Studio turns a sky into sky light (sky-light: its light is flatter than the default sky's
/// cosine irradiance, the directional terms ~0.6 of ror's): a white and a gray SmoothPlastic ball lit
/// only by the sky (EnvironmentDiffuseScale 1) under skies that are all dark, all white, white in one
/// image only, and the default sky without (`plain`) and with (`none`: no Sky instance) the sun and
/// moon, each from four diagonals; `none-check` repeats `none` last from a hair further away, to
/// show whether the sky light had settled.
pub fn sky_kernel() -> Experiment {
	let mut model = ModelBuilder::new("SkyKernel");
	for (index, gray) in [255u8, 160].iter().enumerate() {
		let center = Vec3::new(index as f32 * 5.0 - 5.0, 5.0, 0.0);
		model.part(&PartSpec { name: format!("ball{gray}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(3.0), color: [*gray; 3], material: 272, shape: Some(0), ..Default::default() });
	}
	let sky_only = |sky: Option<scene::environment::Sky>| Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 1.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		sky,
		..Environment::default()
	};
	let faces = |white: Option<usize>| {
		let skybox = std::array::from_fn(|face| if white == Some(face) || white == Some(6) { SKY_WHITE.to_string() } else { SKY_DARK.to_string() });
		Some(scene::environment::Sky { skybox, celestial_bodies_shown: false, ..Default::default() })
	};
	let mut skies = vec![("none", None), ("plain", Some(default_sky())), ("dark", faces(None)), ("white", faces(Some(6)))];
	for (face, label) in ["bk", "dn", "ft", "lf", "rt", "up"].iter().enumerate() {
		skies.push((label, faces(Some(face))));
	}
	let views = [("a", 35.0f32, 30.0f32), ("b", 215.0, -30.0), ("c", 125.0, 30.0), ("d", 305.0, -30.0)];
	let camera = |yaw: f32, pitch: f32, distance: f32| CameraSpec::Orbit { yaw, pitch, distance: Some(distance), padding: 0.0, target: Some([-2.5, 5.0, 0.0]), fov: 15.0 };
	let mut runs = Vec::new();
	for (label, sky) in &skies {
		for (view, yaw, pitch) in views {
			runs.push(Run { label: format!("{label}-{view}"), environment: sky_only(sky.clone()), camera: camera(yaw, pitch, 40.0) });
		}
	}
	for (view, yaw, pitch) in views {
		runs.push(Run { label: format!("none-check-{view}"), environment: sky_only(None), camera: camera(yaw, pitch, 40.01) });
	}
	Experiment { name: "sky-kernel".into(), model, probes: Vec::new(), runs }
}

/// How the sun and the sky light combine: the sky-light cubes with the sun on (Brightness 0, 1, 2,
/// 4) and the sky light on or off, so the sun's share of the environment lighting shows.
pub fn sun_sky_mix() -> Experiment {
	let mut experiment = sky_light();
	experiment.name = "sun-sky-mix".into();
	let mix = |brightness: f32, diffuse: f32, specular: f32| Environment {
		brightness,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: diffuse,
		environment_specular_scale: specular,
		global_shadows: false,
		..Environment::default()
	};
	let mut runs = Vec::new();
	for (view, yaw, pitch) in [("a", 35.0f32, 30.0f32), ("b", 215.0, -30.0)] {
		let camera = CameraSpec::Orbit { yaw, pitch, distance: Some(60.0), padding: 0.0, target: Some([0.0, 2.5, 0.0]), fov: 20.0 };
		for brightness in [0.0f32, 1.0, 2.0, 4.0] {
			for (diffuse, specular) in [(0.0f32, 0.0f32), (1.0, 0.0), (1.0, 1.0)] {
				runs.push(Run { label: format!("{view}-b{brightness}-d{diffuse}-s{specular}"), environment: mix(brightness, diffuse, specular), camera: camera.clone() });
			}
		}
	}
	experiment.runs = runs;
	experiment
}

/// A model file under each lighting term alone (sun, flat ambient, sky diffuse, sky specular) and
/// all together, from the default orbit camera: which term a model's mismatch comes from.
pub fn decompose(path: &std::path::Path) -> Result<Experiment> {
	let document = crate::scene::load::read_file(path)?;
	let mut model = ModelBuilder::new("Decompose");
	model.dom = document.dom;
	let roots = document.roots.clone();
	// re-root the loaded model's instances under a fresh Model in its own DOM
	let container = model.dom.insert(model.dom.root_ref(), rbx_dom_weak::InstanceBuilder::new("Model").with_name("Decompose"));
	for root in roots {
		model.dom.transfer_within(root, container);
	}
	model.model = container;
	let off = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		..Environment::default()
	};
	let base = Environment::default();
	let camera = CameraSpec::default();
	let runs = vec![
		Run { label: "sun".into(), environment: Environment { brightness: base.brightness, ..off.clone() }, camera: camera.clone() },
		Run { label: "ambient".into(), environment: Environment { ambient: base.ambient, outdoor_ambient: base.outdoor_ambient, ..off.clone() }, camera: camera.clone() },
		Run { label: "sky-diffuse".into(), environment: Environment { environment_diffuse_scale: 1.0, ..off.clone() }, camera: camera.clone() },
		Run { label: "sky-specular".into(), environment: Environment { environment_specular_scale: 1.0, ..off.clone() }, camera: camera.clone() },
		Run { label: "all".into(), environment: base, camera },
	];
	Ok(Experiment { name: "decompose".into(), model, probes: Vec::new(), runs })
}

/// `decompose` from a fixed camera (the parity close-ups): `eye` looking along `look` (as
/// tools/parity.py places it, towards eye + 10 look) through `fov`; and the sun with the sky specular
/// (metals are metallic only with the sky specular on: their sun term is that run's less sky-specular's).
pub fn decompose_view(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> Result<Experiment> {
	let mut experiment = decompose(path)?;
	experiment.name = "decompose-view".into();
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov };
	for run in &mut experiment.runs {
		run.camera = camera.clone();
	}
	let base = Environment::default();
	let sun_specular = Environment { ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, ..base };
	experiment.runs.push(Run { label: "sun-specular".into(), environment: sun_specular, camera });
	Ok(experiment)
}

/// `decompose-view`'s sky specular alone in the settings the sphere experiments use instead: the
/// close-ups' metal panels read their sky specular 8-17 % brighter in Studio (GlobalShadows on, no Sky
/// instance) where `ibl-grey`'s spheres wearing the same SurfaceAppearances match (GlobalShadows off, a
/// Sky without its sun and moon). Runs `gs<0|1>-<none|sky|bodies>`: GlobalShadows off or on; no Sky,
/// the default Sky without its sun and moon, or with them. They move Studio's sky specular by 1 % at
/// most: the panels' excess is their view depth ("Specular fade", `ibl-distance`).
pub fn sky_context(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> Result<Experiment> {
	let mut experiment = decompose(path)?;
	experiment.name = "sky-context".into();
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov };
	let bodies = scene::environment::Sky::default();
	experiment.runs = [("none", None), ("sky", Some(default_sky())), ("bodies", Some(bodies))]
		.into_iter()
		.flat_map(|(name, sky)| {
			let camera = camera.clone();
			[false, true].map(move |global_shadows| Run {
				label: format!("gs{}-{name}", u8::from(global_shadows)),
				environment: Environment { sky: sky.clone(), global_shadows, ..sky_specular_only(1.0) },
				camera: camera.clone(),
			})
		})
		.collect();
	Ok(experiment)
}

/// Beam properties an experiment case sets.
type BeamProperties = Vec<(&'static str, rbx_dom_weak::types::Variant)>;

/// A beam for the beam experiments: between two attachments (CFrames relative to an invisible
/// anchor part at `origin`), white, opaque, unlit, untextured and still unless `set` overrides it.
fn add_beam(model: &mut ModelBuilder, name: &str, origin: Vec3, a0: glam::Affine3A, a1: glam::Affine3A, set: &[(&str, rbx_dom_weak::types::Variant)]) {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	let anchor = model.part(&PartSpec { name: name.into(), cframe: glam::Affine3A::from_translation(origin), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
	let attachment0 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", crate::scene::build::cframe(a0)));
	let attachment1 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", crate::scene::build::cframe(a1)));
	let white = Color3::new(1.0, 1.0, 1.0);
	let mut beam = InstanceBuilder::new("Beam")
		.with_name("Beam")
		.with_property("Attachment0", Variant::Ref(attachment0))
		.with_property("Attachment1", Variant::Ref(attachment1))
		.with_property("Color", ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, white), ColorSequenceKeypoint::new(1.0, white)] })
		.with_property("Transparency", NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 0.0, 0.0)] })
		.with_property("Brightness", 1.0f32)
		.with_property("LightEmission", 0.0f32)
		.with_property("LightInfluence", 0.0f32)
		.with_property("TextureSpeed", 0.0f32)
		.with_property("Width0", 1.0f32)
		.with_property("Width1", 1.0f32)
		.with_property("Segments", 10i32);
	for (property, value) in set {
		beam = beam.with_property(*property, value.clone());
	}
	model.child(anchor, beam);
}

/// Black floor and back wall (unlit in the dark environment) behind the beam experiments' views.
fn beam_backdrop(model: &mut ModelBuilder, center: Vec3, length: f32) {
	model.part(&PartSpec { name: "Floor".into(), cframe: glam::Affine3A::from_translation(center + Vec3::new(0.0, -4.0, 0.0)), size: Vec3::new(length, 1.0, 200.0), color: [0, 0, 0], ..Default::default() });
	model.part(&PartSpec { name: "Wall".into(), cframe: glam::Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 14.0)), size: Vec3::new(length, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
}

/// Beam geometry: where the curve runs (CurveSize along which attachment axis), Segments, the
/// width's interpolation and the ribbon's orientation (attachment axes, twist, FaceCamera), and
/// what Studio draws for a disabled beam or one missing an attachment; white, unlit, on black,
/// seen from above, the front and obliquely.
pub fn beam_shape() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{Ref, Variant};

	let mut model = ModelBuilder::new("BeamShape");
	let at = |x: f32, rotation: Quat| Affine3A::from_rotation_translation(rotation, Vec3::new(x, 0.0, 0.0));
	let none = Quat::IDENTITY;
	// the attachments' X axes turned to +Z (across the beam), so CurveSize bends it in the XZ plane
	let across = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
	let roll = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
	// (label, attachment0, attachment1, properties)
	let cases: Vec<(&str, Affine3A, Affine3A, BeamProperties)> = vec![
		("straight", at(-4.0, none), at(4.0, none), vec![("Width0", 1.0f32.into()), ("Width1", 3.0f32.into())]),
		("roll90", at(-4.0, roll), at(4.0, roll), vec![("Width0", 1.0f32.into()), ("Width1", 3.0f32.into())]),
		("twist", at(-4.0, none), at(4.0, roll), vec![("Width0", 2.0f32.into()), ("Width1", 2.0f32.into())]),
		("yaw30", at(-4.0, Quat::from_rotation_y(30f32.to_radians())), at(4.0, Quat::from_rotation_y(30f32.to_radians())), vec![("Width0", 1.0f32.into()), ("Width1", 3.0f32.into())]),
		("curve", at(-4.0, across), at(4.0, across), vec![("CurveSize0", 4.0f32.into()), ("CurveSize1", (-2.0f32).into()), ("Segments", 4i32.into()), ("Width0", 0.6f32.into()), ("Width1", 0.6f32.into())]),
		("curve-s", at(-4.0, across), at(4.0, across), vec![("CurveSize0", 4.0f32.into()), ("CurveSize1", 3.0f32.into()), ("Width0", 1.0f32.into()), ("Width1", 1.0f32.into())]),
		("pitch", at(-4.0, Quat::from_rotation_z(30f32.to_radians())), at(4.0, none), vec![("CurveSize0", 4.0f32.into()), ("Segments", 5i32.into())]),
		("facecamera", at(-4.0, none), at(4.0, none), vec![("FaceCamera", true.into()), ("Width0", 1.0f32.into()), ("Width1", 3.0f32.into())]),
		("disabled", at(-4.0, none), at(4.0, none), vec![("Enabled", false.into())]),
		("no-attachment1", at(-4.0, none), at(4.0, none), vec![("Attachment1", Variant::Ref(Ref::none()))]),
	];
	let mut runs = Vec::new();
	for (index, (label, a0, a1, set)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		add_beam(&mut model, label, center, a0, a1, &set);
		let views: [(&str, Vec3); 3] = [("top", Vec3::new(0.0, 60.0, -0.05)), ("front", Vec3::new(0.0, 0.0, -60.0)), ("oblique", Vec3::new(25.0, 35.0, -40.0))];
		for (view, offset) in views {
			runs.push(Run { label: format!("{label}-{view}"), environment: dark(), camera: CameraSpec::LookAt { eye: (center + offset).into(), target: center.into(), fov: 20.0 } });
		}
	}
	beam_backdrop(&mut model, Vec3::new(180.0, 0.0, 0.0), 500.0);
	Experiment { name: "beam-shape".into(), model, probes: Vec::new(), runs }
}

/// Beam textures: Studio's `blockUpperLeft.png` (an opaque white top-left quarter) on straight and
/// curved beams in every TextureMode and several TextureLengths, Wrap / Static beams shifted along
/// their line (where the pattern is anchored), FaceCamera and flipped attachments (which way the
/// image runs), and `grid4.png` (opaque grays: how texels decode); unlit, on black, from the front.
pub fn beam_texture() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{ContentId, Enum, Variant};

	const BLOCK: &str = "rbxasset://textures/blockUpperLeft.png";
	const GRID: &str = "rbxasset://textures/grid4.png";
	// asymmetric both ways (an arrow pointer, its tip at the image centre), and a 48 x 64 triangle
	// pointing up: which image axis runs along the beam
	const ARROW: &str = "rbxasset://textures/ArrowCursor.png";
	const TRIANGLE: &str = "rbxasset://textures/triangle.png";
	let mut model = ModelBuilder::new("BeamTexture");
	let at = |x: f32, rotation: Quat| Affine3A::from_rotation_translation(rotation, Vec3::new(x, 0.0, 0.0));
	let none = Quat::IDENTITY;
	let across = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
	let flip = Quat::from_rotation_x(std::f32::consts::PI);
	let texture = |uri: &str, mode: u32, length: f32| -> Vec<(&'static str, Variant)> {
		vec![("Texture", ContentId::from(uri).into()), ("TextureMode", Enum::from_u32(mode).into()), ("TextureLength", length.into()), ("Width0", 2.0f32.into()), ("Width1", 2.0f32.into())]
	};
	let with = |mut base: Vec<(&'static str, Variant)>, extra: Vec<(&'static str, Variant)>| {
		base.extend(extra);
		base
	};
	// (label, attachment0, attachment1, properties, views: front / back / top)
	let cases: Vec<(&str, Affine3A, Affine3A, BeamProperties, &[&str])> = vec![
		("stretch-1", at(-4.0, none), at(4.0, none), texture(BLOCK, 0, 1.0), &["front", "back"]),
		("stretch-3", at(-4.0, none), at(4.0, none), texture(BLOCK, 0, 3.0), &["front"]),
		("stretch-0.375", at(-4.0, none), at(4.0, none), texture(BLOCK, 0, 0.375), &["front"]),
		("wrap-3", at(-4.0, none), at(3.0, none), texture(BLOCK, 1, 3.0), &["front"]),
		("wrap-3-shift", at(-3.0, none), at(4.0, none), texture(BLOCK, 1, 3.0), &["front"]),
		("static-3", at(-4.0, none), at(3.0, none), texture(BLOCK, 2, 3.0), &["front"]),
		("static-3-shift", at(-3.0, none), at(4.0, none), texture(BLOCK, 2, 3.0), &["front"]),
		("static-3-long", at(-4.0, none), at(4.5, none), texture(BLOCK, 2, 3.0), &["front"]),
		("stretch-curve", at(-4.0, across), at(4.0, across), with(texture(BLOCK, 0, 4.0), vec![("CurveSize0", 4.0f32.into()), ("CurveSize1", 3.0f32.into()), ("Segments", 20i32.into())]), &["front", "top"]),
		("wrap-curve", at(-4.0, across), at(4.0, across), with(texture(BLOCK, 1, 2.0), vec![("CurveSize0", 4.0f32.into()), ("CurveSize1", 3.0f32.into()), ("Segments", 20i32.into())]), &["front", "top"]),
		("flip", at(-4.0, flip), at(4.0, flip), texture(BLOCK, 0, 1.0), &["front"]),
		("facecamera", at(-4.0, none), at(4.0, none), with(texture(BLOCK, 0, 1.0), vec![("FaceCamera", true.into())]), &["front", "back", "top"]),
		("taper", at(-4.0, none), at(4.0, none), with(texture(BLOCK, 0, 2.0), vec![("Width0", 0.5f32.into()), ("Width1", 3.0f32.into())]), &["front"]),
		("grid", at(-4.0, none), at(4.0, none), texture(GRID, 0, 1.0), &["front"]),
		("arrow", at(-4.0, none), at(4.0, none), with(texture(ARROW, 0, 1.0), vec![("Width0", 6.0f32.into()), ("Width1", 6.0f32.into())]), &["front"]),
		("triangle", at(-4.0, none), at(4.0, none), with(texture(TRIANGLE, 0, 1.0), vec![("Width0", 6.0f32.into()), ("Width1", 6.0f32.into())]), &["front"]),
	];
	let mut runs = Vec::new();
	for (index, (label, a0, a1, set, views)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		add_beam(&mut model, label, center, a0, a1, &set);
		for view in views {
			// the back views stand between the beam and the wall, with a wider field of view
			let (offset, fov) = match *view {
				"back" => (Vec3::new(0.0, 0.0, 12.0), 60.0),
				"top" => (Vec3::new(0.0, 60.0, -0.05), 20.0),
				_ => (Vec3::new(0.0, 0.0, -60.0), 20.0),
			};
			runs.push(Run { label: format!("{label}-{view}"), environment: dark(), camera: CameraSpec::LookAt { eye: (center + offset).into(), target: center.into(), fov } });
		}
	}
	beam_backdrop(&mut model, Vec3::new(260.0, 0.0, 0.0), 700.0);
	// a second wall behind the back views (and behind the front views' cameras)
	model.part(&PartSpec { name: "BackWall".into(), cframe: glam::Affine3A::from_translation(Vec3::new(260.0, 0.0, -80.0)), size: Vec3::new(700.0, 200.0, 1.0), color: [0, 0, 0], ..Default::default() });
	Experiment { name: "beam-texture".into(), model, probes: Vec::new(), runs }
}

/// Beam shading: Brightness and colours (the emission scale and decode, bloom), Transparency and
/// LightEmission over a backdrop that is black on the left and white on the right (the blend), Color
/// and Transparency sequences at several Segments (per-vertex sampling, along the arc), and
/// LightInfluence under flat ambient and under the sun from both faces.
pub fn beam_color() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamColor");
	let at = |x: f32, rotation: Quat| Affine3A::from_rotation_translation(rotation, Vec3::new(x, 0.0, 0.0));
	let none = Quat::IDENTITY;
	let across = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
	let colors = |keys: &[(f32, [f32; 3])]| -> Variant { ColorSequence { keypoints: keys.iter().map(|(time, c)| ColorSequenceKeypoint::new(*time, Color3::new(c[0], c[1], c[2]))).collect() }.into() };
	let numbers = |keys: &[(f32, f32)]| -> Variant { NumberSequence { keypoints: keys.iter().map(|(time, value)| NumberSequenceKeypoint::new(*time, *value, 0.0)).collect() }.into() };
	let solid = |c: [f32; 3]| colors(&[(0.0, c), (1.0, c)]);
	let constant = |value: f32| numbers(&[(0.0, value), (1.0, value)]);
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let sun = Environment { brightness: 2.0, ..dark() };
	let white = [1.0, 1.0, 1.0];
	let orange = [1.0, 0.28235295, 0.0];
	// (label, properties, environment, curved)
	let mut cases: Vec<(String, BeamProperties, Environment, bool)> = Vec::new();
	for brightness in [0.25f32, 0.5, 1.0, 2.0, 3.0, 5.0] {
		cases.push((format!("white-b{brightness}"), vec![("Brightness", brightness.into())], flat.clone(), false));
	}
	cases.push(("orange-b1".into(), vec![("Color", solid(orange))], flat.clone(), false));
	cases.push(("orange-b3".into(), vec![("Color", solid(orange)), ("Brightness", 3.0f32.into())], flat.clone(), false));
	cases.push(("gray-b1".into(), vec![("Color", solid([0.5, 0.5, 0.5]))], flat.clone(), false));
	cases.push(("mixed-b1".into(), vec![("Color", solid([1.0, 0.6, 0.2]))], flat.clone(), false));
	for (transparency, emission) in [(0.5f32, 0.0f32), (0.0, 1.0), (0.5, 1.0), (0.5, 0.5), (0.25, 0.22), (0.0, 0.5), (0.8, 0.0)] {
		cases.push((format!("t{transparency}-e{emission}"), vec![("Transparency", constant(transparency)), ("LightEmission", emission.into())], flat.clone(), false));
	}
	cases.push(("orange-b3-e1".into(), vec![("Color", solid(orange)), ("Brightness", 3.0f32.into()), ("LightEmission", 1.0f32.into())], flat.clone(), false));
	for segments in [1i32, 2, 10] {
		cases.push((format!("gradient-s{segments}"), vec![("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 0.0, 1.0])])), ("Segments", segments.into())], flat.clone(), false));
	}
	cases.push(("gradient3-s2".into(), vec![("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (0.3, [0.0, 1.0, 0.0]), (1.0, [0.0, 0.0, 1.0])])), ("Segments", 2i32.into())], flat.clone(), false));
	cases.push(("gradient3-s10".into(), vec![("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (0.3, [0.0, 1.0, 0.0]), (1.0, [0.0, 0.0, 1.0])])), ("Segments", 10i32.into())], flat.clone(), false));
	for segments in [2i32, 10] {
		cases.push((format!("fade-s{segments}"), vec![("Transparency", numbers(&[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)])), ("Segments", segments.into())], flat.clone(), false));
	}
	cases.push(("curve-gradient".into(), vec![("Color", colors(&[(0.0, [1.0, 0.0, 0.0]), (1.0, [0.0, 1.0, 0.0])])), ("CurveSize0", 4.0f32.into()), ("CurveSize1", 3.0f32.into()), ("Segments", 20i32.into())], flat.clone(), true));
	cases.push(("lit-ambient".into(), vec![("LightInfluence", 1.0f32.into())], flat.clone(), false));
	cases.push(("half-lit-ambient".into(), vec![("LightInfluence", 0.5f32.into())], flat.clone(), false));
	cases.push(("lit-sun".into(), vec![("LightInfluence", 1.0f32.into())], sun.clone(), false));
	cases.push(("lit-sun-b3".into(), vec![("LightInfluence", 1.0f32.into()), ("Brightness", 3.0f32.into())], sun.clone(), false));
	let mut runs = Vec::new();
	for (index, (label, mut set, environment, curved)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		set.insert(0, ("Width0", 2.0f32.into()));
		set.insert(1, ("Width1", 2.0f32.into()));
		set.insert(2, ("Color", solid(white)));
		let (a0, a1) = if curved { (at(-4.0, across), at(4.0, across)) } else { (at(-4.0, none), at(4.0, none)) };
		add_beam(&mut model, &label, center, a0, a1, &set);
		// behind the beam: black on the left (+X) half, white on the right
		for (side, color) in [(1.0f32, [0u8, 0, 0]), (-1.0, [255, 255, 255])] {
			model.part(&PartSpec {
				name: format!("{label}-backdrop"),
				cframe: glam::Affine3A::from_translation(center + Vec3::new(side * 3.0, 0.0, 3.0)),
				size: Vec3::new(6.0, 5.0, 0.5),
				color,
				cast_shadow: false,
				..Default::default()
			});
		}
		let front = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -60.0)).into(), target: center.into(), fov: 20.0 };
		if curved {
			runs.push(Run { label: format!("{label}-top"), environment: environment.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 60.0, -0.05)).into(), target: center.into(), fov: 20.0 } });
		}
		if label.starts_with("lit-sun") {
			// the other face: from behind, between the beam and its backdrop
			runs.push(Run { label: format!("{label}-back"), environment: environment.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, 2.5)).into(), target: center.into(), fov: 120.0 } });
		}
		runs.push(Run { label: format!("{label}-front"), environment, camera: front });
	}
	beam_backdrop(&mut model, Vec3::new(500.0, 0.0, 0.0), 1100.0);
	Experiment { name: "beam-color".into(), model, probes: Vec::new(), runs }
}

/// Beam emission against Brightness: white, orange and gray beams at Brightness 0.5-10 in a row,
/// read at ExposureCompensation -3 and -2 (below clipping and the bloom threshold) and at 0, next to
/// Neon tiles of known emission; LightInfluence 0.25-0.75 under flat ambient and the sun.
pub fn beam_brightness() -> Experiment {
	use glam::Affine3A;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamBrightness");
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	let mut probes = Vec::new();
	let brightness = [0.5f32, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 6.0, 10.0];
	// rows of vertical beams 1.5 studs wide, 4 apart: white, orange, gray
	for (row, (name, color)) in [("white", [1.0f32, 1.0, 1.0]), ("orange", [1.0, 0.28235295, 0.0]), ("gray", [0.5, 0.5, 0.5])].iter().enumerate() {
		for (column, b) in brightness.iter().enumerate() {
			let x = (column as f32 - 5.0) * 4.0;
			let y = (1.0 - row as f32) * 7.0;
			let label = format!("{name}-b{b}");
			let up = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
			// along Y: the attachments' X axes up, their Y axes (the width) along -X
			add_beam(
				&mut model,
				&label,
				Vec3::new(x, y, 0.0),
				Affine3A::from_rotation_translation(up, Vec3::new(0.0, -2.5, 0.0)),
				Affine3A::from_rotation_translation(up, Vec3::new(0.0, 2.5, 0.0)),
				&[("Color", solid(*color)), ("Brightness", (*b).into()), ("Width0", 1.5f32.into()), ("Width1", 1.5f32.into())],
			);
			probes.push(Probe { label, position: [x, y, 0.0], inputs: serde_json::json!({"color": color, "brightness": b}) });
		}
	}
	// Neon references (x = 25 c^4): gray levels
	for (column, gray) in [100u8, 130, 160, 190, 220, 255].iter().enumerate() {
		let center = Vec3::new((column as f32 - 2.5) * 4.0, -14.0, 0.0);
		let label = format!("neon{gray}");
		model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(1.5, 3.0, 0.2), color: [*gray; 3], material: 288, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label, position: (center - Vec3::Z * 0.1).into(), inputs: serde_json::json!({"neon": gray}) });
	}
	// LightInfluence between 0 and 1 (Brightness 1 and 2), lit by the environment of the run
	for (column, (influence, b)) in [(0.25f32, 1.0f32), (0.5, 1.0), (0.75, 1.0), (0.5, 2.0), (1.0, 1.0), (0.0, 1.0)].iter().enumerate() {
		let x = (column as f32 - 2.5) * 4.0;
		let label = format!("li{influence}-b{b}");
		let up = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
		add_beam(
			&mut model,
			&label,
			Vec3::new(x, -21.0, 0.0),
			Affine3A::from_rotation_translation(up, Vec3::new(0.0, -2.5, 0.0)),
			Affine3A::from_rotation_translation(up, Vec3::new(0.0, 2.5, 0.0)),
			&[("LightInfluence", (*influence).into()), ("Brightness", (*b).into()), ("Width0", 1.5f32.into()), ("Width1", 1.5f32.into())],
		);
		probes.push(Probe { label, position: [x, -21.0, 0.0], inputs: serde_json::json!({"lightInfluence": influence, "brightness": b}) });
	}
	model.part(&PartSpec { name: "Backdrop".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -5.0, 8.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let camera = CameraSpec::LookAt { eye: [0.0, -5.0, -75.0], target: [0.0, -5.0, 0.0], fov: 35.0 };
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for exposure in [-3.0f32, -2.0, 0.0] {
		runs.push(Run { label: format!("flat-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..flat.clone() }, camera: camera.clone() });
	}
	runs.push(Run { label: "sun-ev-2".into(), environment: Environment { brightness: 2.0, exposure_compensation: -2.0, ..dark() }, camera: camera.clone() });
	runs.push(Run { label: "default-ev-2".into(), environment: Environment { exposure_compensation: -2.0, global_shadows: false, ..Environment::default() }, camera });
	Experiment { name: "beam-brightness".into(), model, probes, runs }
}

/// LightInfluence at Brightness away from 1 (a cutscene's slash: a blue (0.16, 0.16, 1) beam
/// of Brightness 15 at LightInfluence 0.7): white and blue beams, LightInfluence 0 to 1 against
/// Brightness 0 to 15, unlit (LightEmission 0) on black, in the dark, under flat ambient and under the sun.
pub fn beam_influence() -> Experiment {
	use glam::Affine3A;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamInfluence");
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	let mut probes = Vec::new();
	let influences = [0.0f32, 0.1, 0.25, 0.5, 0.7, 0.9, 1.0];
	let brightness = [0.0f32, 0.5, 1.0, 3.0, 15.0];
	let up = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
	for (set, (name, color)) in [("white", [1.0f32, 1.0, 1.0]), ("blue", [0.16, 0.16, 1.0])].iter().enumerate() {
		for (row, b) in brightness.iter().enumerate() {
			for (column, influence) in influences.iter().enumerate() {
				let x = (column as f32 - 3.0) * 3.0 + (set as f32 - 0.5) * 24.0;
				let y = (2.0 - row as f32) * 4.0;
				let label = format!("{name}-b{b}-li{influence}");
				add_beam(
					&mut model,
					&label,
					Vec3::new(x, y, 0.0),
					Affine3A::from_rotation_translation(up, Vec3::new(0.0, -1.5, 0.0)),
					Affine3A::from_rotation_translation(up, Vec3::new(0.0, 1.5, 0.0)),
					&[("Color", solid(*color)), ("Brightness", (*b).into()), ("LightInfluence", (*influence).into()), ("Width0", 1.5f32.into()), ("Width1", 1.5f32.into())],
				);
				probes.push(Probe { label, position: [x, y, 0.0], inputs: serde_json::json!({"color": color, "brightness": b, "lightInfluence": influence}) });
			}
		}
	}
	model.part(&PartSpec { name: "Backdrop".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 8.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -70.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let runs = vec![
		Run { label: "dark".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: camera.clone() },
		Run { label: "flat".into(), environment: Environment { ambient: Vec3::splat(0.5), exposure_compensation: -2.0, ..dark() }, camera: camera.clone() },
		Run { label: "sun".into(), environment: Environment { brightness: 2.0, exposure_compensation: -2.0, ..dark() }, camera },
	];
	Experiment { name: "beam-influence".into(), model, probes, runs }
}

/// Beams tied in distance (a cutscene stacks three to six beams on one pair of
/// attachments): two or three opaque beams of different colours on the same attachments, in both
/// orders in the file and under one part or two, of equal and unequal widths, with equal ZOffsets and
/// ZOffsets 0.001 apart, FaceCamera and not; which colour Studio shows on top.
pub fn beam_ties() -> Experiment {
	use glam::Affine3A;
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamTies");
	let mut probes = Vec::new();
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	let opaque: Variant = NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, 0.0, 0.0), NumberSequenceKeypoint::new(1.0, 0.0, 0.0)] }.into();
	let colors = [("red", [1.0f32, 0.0, 0.0]), ("green", [0.0, 1.0, 0.0]), ("blue", [0.0, 0.0, 1.0])];
	// (label, beams as (colour index, width, ZOffset, LightEmission), FaceCamera, each beam under its own anchor part)
	type Case = (&'static str, Vec<(usize, f32, f32, f32)>, bool, bool);
	let cases: Vec<Case> = vec![
		("rg", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0)], false, false),
		("gr", vec![(1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, false),
		("rgb", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0), (2, 1.5, 0.0, 0.0)], false, false),
		("bgr", vec![(2, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, false),
		("r-wide-g", vec![(0, 2.0, 0.0, 0.0), (1, 1.0, 0.0, 0.0)], false, false),
		("g-wide-r", vec![(1, 2.0, 0.0, 0.0), (0, 1.0, 0.0, 0.0)], false, false),
		("r-narrow-g", vec![(0, 1.0, 0.0, 0.0), (1, 2.0, 0.0, 0.0)], false, false),
		("rg-face", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0)], true, false),
		("gr-face", vec![(1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], true, false),
		("rg-parts", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0)], false, true),
		("gr-parts", vec![(1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, true),
		("r-z-g", vec![(0, 1.5, 0.001, 0.0), (1, 1.5, 0.0, 0.0)], false, false),
		("g-z-r", vec![(1, 1.5, 0.001, 0.0), (0, 1.5, 0.0, 0.0)], false, false),
		("rg-half", vec![(0, 1.5, 0.0, 0.5), (1, 1.5, 0.0, 0.5)], false, false),
		("gr-half", vec![(1, 1.5, 0.0, 0.5), (0, 1.5, 0.0, 0.5)], false, false),
		("rg-2", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0)], false, false),
		("gr-2", vec![(1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, false),
		("rg-parts-2", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 0.0)], false, true),
		("gr-parts-2", vec![(1, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, true),
		("rb-parts", vec![(0, 1.5, 0.0, 0.0), (2, 1.5, 0.0, 0.0)], false, true),
		("br-parts", vec![(2, 1.5, 0.0, 0.0), (0, 1.5, 0.0, 0.0)], false, true),
		("rg-add", vec![(0, 1.5, 0.0, 1.0), (1, 1.5, 0.0, 1.0)], false, false),
		("rg-mixed", vec![(0, 1.5, 0.0, 0.0), (1, 1.5, 0.0, 1.0)], false, false),
		("gr-mixed", vec![(1, 1.5, 0.0, 1.0), (0, 1.5, 0.0, 0.0)], false, false),
		("rg-half-2", vec![(0, 1.5, 0.0, 0.5), (1, 1.5, 0.0, 0.5)], false, false),
		("bg-half", vec![(2, 1.5, 0.0, 0.5), (1, 1.5, 0.0, 0.5)], false, false),
		("gb-half", vec![(1, 1.5, 0.0, 0.5), (2, 1.5, 0.0, 0.5)], false, false),
		("r-narrow-g-2", vec![(0, 1.4, 0.0, 0.0), (1, 1.6, 0.0, 0.0)], false, false),
		("r-narrow-g-face", vec![(0, 1.0, 0.0, 0.0), (1, 2.0, 0.0, 0.0)], true, false),
		("r-narrow-g-parts", vec![(0, 1.0, 0.0, 0.0), (1, 2.0, 0.0, 0.0)], false, true),
		("g-narrow-r-parts", vec![(1, 1.0, 0.0, 0.0), (0, 2.0, 0.0, 0.0)], false, true),
	];
	let up = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
	for (index, (label, beams, face, parts)) in cases.iter().enumerate() {
		let x = (index % 8) as f32 * 4.0 - 14.0;
		let y = 9.0 - (index / 8) as f32 * 6.0;
		let mut shared = None;
		for (order, (color, width, z_offset, emission)) in beams.iter().enumerate() {
			let anchor = match shared {
				Some(anchor) if !*parts => anchor,
				_ => {
					let anchor = model.part(&PartSpec { name: format!("{label}-{order}"), cframe: Affine3A::from_translation(Vec3::new(x, y, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
					shared = Some(anchor);
					anchor
				}
			};
			let a0 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A0").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_rotation_translation(up, Vec3::new(0.0, -2.5, 0.0)))));
			let a1 = model.child(anchor, InstanceBuilder::new("Attachment").with_name("A1").with_property("CFrame", crate::scene::build::cframe(Affine3A::from_rotation_translation(up, Vec3::new(0.0, 2.5, 0.0)))));
			model.child(
				anchor,
				InstanceBuilder::new("Beam")
					.with_name(colors[*color].0)
					.with_property("Attachment0", Variant::Ref(a0))
					.with_property("Attachment1", Variant::Ref(a1))
					.with_property("Color", solid(colors[*color].1))
					.with_property("Transparency", opaque.clone())
					.with_property("Brightness", 1.0f32)
					.with_property("LightEmission", *emission)
					.with_property("LightInfluence", 0.0f32)
					.with_property("TextureSpeed", 0.0f32)
					.with_property("Width0", *width)
					.with_property("Width1", *width)
					.with_property("ZOffset", *z_offset)
					.with_property("FaceCamera", *face)
					.with_property("Segments", 10i32),
			);
		}
		let order: Vec<&str> = beams.iter().map(|beam| colors[beam.0].0).collect();
		probes.push(Probe { label: (*label).into(), position: [x, y, 0.0], inputs: serde_json::json!({"order": order}) });
	}
	model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 8.0)), size: Vec3::new(200.0, 100.0, 1.0), color: [0, 0, 0], ..Default::default() });
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let oblique = CameraSpec::LookAt { eye: [8.0, 5.0, -38.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let runs = vec![Run { label: "front".into(), environment: dark(), camera }, Run { label: "oblique".into(), environment: dark(), camera: oblique }];
	Experiment { name: "beam-ties".into(), model, probes, runs }
}

/// How Studio evaluates Color and Transparency sequences: long beams (30 studs, 300 segments, so the
/// curve's vertices sample the sequence finely) whose keypoints zigzag between extremes at uneven
/// times; the profile along each beam shows where the corners land and how sharp they stay.
pub fn beam_sequence() -> Experiment {
	use glam::Affine3A;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamSequence");
	let times = [0.0f32, 0.13, 0.21, 0.37, 0.5, 0.61, 0.77, 0.9, 1.0];
	let transparency: Variant = NumberSequence { keypoints: times.iter().enumerate().map(|(index, time)| NumberSequenceKeypoint::new(*time, if index % 2 == 0 { 1.0 } else { 0.0 }, 0.0)).collect() }.into();
	let red: Variant = ColorSequence {
		keypoints: times.iter().enumerate().map(|(index, time)| ColorSequenceKeypoint::new(*time, if index % 2 == 0 { Color3::new(0.0, 0.0, 1.0) } else { Color3::new(1.0, 0.0, 1.0) })).collect(),
	}
	.into();
	// a single sharp opaque spike at 0.5 and one at 0.3, and a 3-keypoint ramp
	let spikes: Variant = NumberSequence {
		keypoints: vec![
			NumberSequenceKeypoint::new(0.0, 1.0, 0.0),
			NumberSequenceKeypoint::new(0.25, 1.0, 0.0),
			NumberSequenceKeypoint::new(0.3, 0.0, 0.0),
			NumberSequenceKeypoint::new(0.35, 1.0, 0.0),
			NumberSequenceKeypoint::new(0.45, 1.0, 0.0),
			NumberSequenceKeypoint::new(0.5, 0.0, 0.0),
			NumberSequenceKeypoint::new(0.55, 1.0, 0.0),
			NumberSequenceKeypoint::new(1.0, 1.0, 0.0),
		],
	}
	.into();
	let cases: [(&str, Vec<(&str, Variant)>); 3] = [("transparency", vec![("Transparency", transparency)]), ("color", vec![("Color", red)]), ("spikes", vec![("Transparency", spikes)])];
	let mut runs = Vec::new();
	for (index, (label, mut set)) in cases.into_iter().enumerate() {
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		set.extend([("Segments", 300i32.into()), ("Width0", 2.0f32.into()), ("Width1", 2.0f32.into())]);
		add_beam(&mut model, label, center, Affine3A::from_translation(Vec3::new(-15.0, 0.0, 0.0)), Affine3A::from_translation(Vec3::new(15.0, 0.0, 0.0)), &set);
		runs.push(Run { label: label.into(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -60.0)).into(), target: center.into(), fov: 25.0 } });
	}
	beam_backdrop(&mut model, Vec3::new(60.0, 0.0, 0.0), 300.0);
	Experiment { name: "beam-sequence".into(), model, probes: Vec::new(), runs }
}

/// Beams among other geometry: a half-transparent red part in front of, behind and crossing a beam
/// (which one Studio draws last), two half-transparent beams crossing, additive beams over each
/// other, and ZOffset pulling a beam out from behind an opaque block or pushing one behind it.
pub fn beam_order() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, NumberSequence, NumberSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamOrder");
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	let constant = |value: f32| -> Variant { NumberSequence { keypoints: vec![NumberSequenceKeypoint::new(0.0, value, 0.0), NumberSequenceKeypoint::new(1.0, value, 0.0)] }.into() };
	let at = |x: f32, z: f32| Affine3A::from_translation(Vec3::new(x, 0.0, z));
	let wide = |set: &[(&'static str, Variant)]| -> Vec<(&'static str, Variant)> {
		let mut all = vec![("Width0", 3.0f32.into()), ("Width1", 3.0f32.into())];
		all.extend(set.iter().cloned());
		all
	};
	let red_glass = |model: &mut ModelBuilder, name: &str, cframe: Affine3A, size: Vec3| {
		model.part(&PartSpec { name: name.into(), cframe, size, color: [255, 0, 0], transparency: 0.5, cast_shadow: false, ..Default::default() });
	};
	let mut runs = Vec::new();
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut view = |label: &str, center: Vec3, offset: Vec3| {
		runs.push(Run { label: label.into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + offset).into(), target: center.into(), fov: 20.0 } });
	};
	let front = Vec3::new(0.0, 0.0, -40.0);
	// (0) the part 2 studs in front of the beam, (1) 2 behind
	for (index, (label, z)) in [("part-front", -2.0f32), ("part-behind", 2.0)].iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		add_beam(&mut model, label, center, at(-4.0, 0.0), at(4.0, 0.0), &wide(&[]));
		red_glass(&mut model, &format!("{label}-glass"), Affine3A::from_translation(center + Vec3::new(0.0, 0.0, *z)), Vec3::new(3.0, 4.0, 0.2));
		view(label, center, front);
	}
	// (2) a thin part through the beam, its centre 1 stud nearer the camera; (3) 1 stud farther
	for (index, (label, z)) in [("crossing-near", -1.0f32), ("crossing-far", 1.0)].iter().enumerate() {
		let center = Vec3::new((index + 2) as f32 * 30.0, 0.0, 0.0);
		add_beam(&mut model, label, center, at(-4.0, 0.0), at(4.0, 0.0), &wide(&[]));
		red_glass(&mut model, &format!("{label}-glass"), Affine3A::from_translation(center + Vec3::new(0.0, 0.0, *z)), Vec3::new(0.4, 4.0, 6.0));
		view(label, center, Vec3::new(10.0, 0.0, -40.0));
	}
	// (4) two half-transparent beams crossing in an X, the blue one's middle nearer the camera
	{
		let center = Vec3::new(120.0, 0.0, 0.0);
		add_beam(&mut model, "beams-a", center, at(-4.0, 1.0), at(4.0, 1.0), &wide(&[("Color", solid([1.0, 0.2, 0.2])), ("Transparency", constant(0.5))]));
		let turn = Quat::from_rotation_y(0.6);
		add_beam(&mut model, "beams-b", center, Affine3A::from_rotation_translation(turn, turn * Vec3::new(-4.0, 0.0, 0.0) + Vec3::new(0.0, 0.0, -0.5)), Affine3A::from_rotation_translation(turn, turn * Vec3::new(4.0, 0.0, 0.0) + Vec3::new(0.0, 0.0, -0.5)), &wide(&[("Color", solid([0.2, 0.2, 1.0])), ("Transparency", constant(0.5))]));
		view("beams-crossing", center, front);
	}
	// (5) an additive beam over a half-transparent one and over a transparent part
	{
		let center = Vec3::new(150.0, 0.0, 0.0);
		add_beam(&mut model, "glow-a", center, at(-4.0, 0.0), at(4.0, 0.0), &wide(&[("Color", solid([1.0, 0.5, 0.1])), ("LightEmission", 1.0f32.into()), ("Transparency", constant(0.3))]));
		add_beam(&mut model, "glow-b", center, at(-4.0, 1.0), at(4.0, 1.0), &[("Width0", 1.5f32.into()), ("Width1", 6.0f32.into()), ("Color", solid([0.2, 0.4, 1.0])), ("Transparency", constant(0.5))]);
		red_glass(&mut model, "glow-glass", Affine3A::from_translation(center + Vec3::new(2.0, 0.0, -1.0)), Vec3::new(2.0, 4.0, 0.2));
		view("glow-stack", center, front);
	}
	// (6) ZOffset: an opaque gray block, a beam 2 studs behind it pulled forward 3, a beam 1 stud in
	// front pushed back 2, and one 1 stud behind pulled 0.5 (still hidden)
	{
		let center = Vec3::new(180.0, 0.0, 0.0);
		model.part(&PartSpec { name: "Block".into(), cframe: Affine3A::from_translation(center), size: Vec3::new(10.0, 8.0, 0.5), color: [128, 128, 128], cast_shadow: false, ..Default::default() });
		add_beam(&mut model, "pulled", center + Vec3::new(0.0, 2.5, 0.0), at(-4.0, 2.0), at(4.0, 2.0), &[("ZOffset", 3.0f32.into()), ("Color", solid([1.0, 1.0, 0.0]))]);
		add_beam(&mut model, "pushed", center, at(-4.0, -1.0), at(4.0, -1.0), &[("ZOffset", (-2.0f32).into()), ("Color", solid([0.0, 1.0, 1.0]))]);
		add_beam(&mut model, "short", center + Vec3::new(0.0, -2.5, 0.0), at(-4.0, 1.0), at(4.0, 1.0), &[("ZOffset", 0.5f32.into()), ("Color", solid([1.0, 0.0, 1.0]))]);
		add_beam(&mut model, "front", center + Vec3::new(0.0, 5.0, 0.0), at(-4.0, -1.0), at(4.0, -1.0), &[("ZOffset", 0.0f32.into())]);
		view("zoffset", center + Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, -40.0));
		view("zoffset-oblique", center + Vec3::new(0.0, 1.0, 0.0), Vec3::new(20.0, 6.0, -35.0));
	}
	beam_backdrop(&mut model, Vec3::new(90.0, 0.0, 0.0), 300.0);
	Experiment { name: "beam-order".into(), model, probes: Vec::new(), runs }
}

/// Which point a beam sorts by among transparent parts: U-shaped beams bulging 3 studs towards the
/// camera (attachments' midpoint at z 0, bounding box centre at -1.5, curve midpoint at -3) with a
/// half-transparent part at z -0.75 or -2.25 in front of the bulge; and the same with additive
/// beams, whose order matters only against alpha-blended geometry.
pub fn beam_sort() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamSort");
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	// the attachments' X axes turned to -Z (towards the camera), so CurveSize 4 / -4 bulges the beam
	// that way by 3 studs
	let toward = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
	let at = |x: f32| Affine3A::from_rotation_translation(toward, Vec3::new(x, 0.0, 0.0));
	let flat = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let mut runs = Vec::new();
	for (index, (label, z, emission)) in [("u-075", -0.75f32, 0.0f32), ("u-225", -2.25, 0.0), ("u-075-additive", -0.75, 1.0), ("u-225-additive", -2.25, 1.0)].iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		add_beam(
			&mut model,
			label,
			center,
			at(-4.0),
			at(4.0),
			&[("CurveSize0", 4.0f32.into()), ("CurveSize1", (-4.0f32).into()), ("Width0", 3.0f32.into()), ("Width1", 3.0f32.into()), ("Color", solid([1.0, 1.0, 1.0])), ("LightEmission", (*emission).into())],
		);
		model.part(&PartSpec { name: format!("{label}-glass"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.0, *z)), size: Vec3::new(3.0, 4.0, 0.2), color: [255, 0, 0], transparency: 0.5, cast_shadow: false, ..Default::default() });
		runs.push(Run { label: (*label).into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -60.0)).into(), target: center.into(), fov: 20.0 } });
	}
	// beams running away from the camera (Attachment0 at z -3, Attachment1 at +3, or reversed), seen
	// from above and in front, with a glass panel at z -1.5 or +1.5 across them
	let along = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
	for (index, (label, z0, glass)) in [("near0-glass-near", -3.0f32, -1.5f32), ("near0-glass-far", -3.0, 1.5), ("far0-glass-near", 3.0, -1.5), ("far0-glass-far", 3.0, 1.5)].iter().enumerate() {
		let center = Vec3::new(120.0 + index as f32 * 30.0, 0.0, 0.0);
		// the ribbon lies flat (the attachments' Y axes along X), so it shows from above
		let flat_ribbon = along * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
		add_beam(
			&mut model,
			label,
			center,
			Affine3A::from_rotation_translation(flat_ribbon, Vec3::new(0.0, 0.0, *z0)),
			Affine3A::from_rotation_translation(flat_ribbon, Vec3::new(0.0, 0.0, -*z0)),
			&[("Width0", 2.0f32.into()), ("Width1", 2.0f32.into()), ("Color", solid([1.0, 1.0, 1.0]))],
		);
		model.part(&PartSpec { name: format!("{label}-glass"), cframe: Affine3A::from_translation(center + Vec3::new(0.0, 0.5, *glass)), size: Vec3::new(4.0, 1.0, 0.2), color: [255, 0, 0], transparency: 0.5, cast_shadow: false, ..Default::default() });
		runs.push(Run { label: (*label).into(), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 30.0, -30.0)).into(), target: center.into(), fov: 20.0 } });
	}
	beam_backdrop(&mut model, Vec3::new(105.0, 0.0, 0.0), 300.0);
	Experiment { name: "beam-sort".into(), model, probes: Vec::new(), runs }
}

/// Odd cases: a twist whose rotation axis is not the beam (Attachment1 turned 120 degrees about the
/// diagonal: slerping the Y axes or the whole rotation part ways), and beams in fog (alpha and
/// additive) against the same beams out of it.
pub fn beam_misc() -> Experiment {
	use glam::{Affine3A, Quat};
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint, Variant};

	let mut model = ModelBuilder::new("BeamMisc");
	let solid = |c: [f32; 3]| -> Variant { ColorSequence { keypoints: vec![ColorSequenceKeypoint::new(0.0, Color3::new(c[0], c[1], c[2])), ColorSequenceKeypoint::new(1.0, Color3::new(c[0], c[1], c[2]))] }.into() };
	let at = |x: f32, rotation: Quat| Affine3A::from_rotation_translation(rotation, Vec3::new(x, 0.0, 0.0));
	let diagonal = Quat::from_axis_angle(Vec3::ONE.normalize(), 120f32.to_radians());
	let mut runs = Vec::new();
	add_beam(&mut model, "twist-diagonal", Vec3::ZERO, at(-4.0, Quat::IDENTITY), at(4.0, diagonal), &[("Width0", 2.0f32.into()), ("Width1", 2.0f32.into()), ("Segments", 20i32.into())]);
	for (view, offset) in [("front", Vec3::new(0.0, 0.0, -60.0)), ("top", Vec3::new(0.0, 60.0, -0.05)), ("oblique", Vec3::new(25.0, 35.0, -40.0))] {
		runs.push(Run { label: format!("twist-diagonal-{view}"), environment: dark(), camera: CameraSpec::LookAt { eye: offset.into(), target: [0.0, 0.0, 0.0], fov: 20.0 } });
	}
	// fog: FogEnd 120 from 0, the beams 60 studs away (half fogged), gray fog, flat ambient light
	let fog = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), fog_color: Vec3::new(0.4, 0.6, 0.9), fog_start: 0.0, fog_end: 120.0, ..dark() };
	let clear = Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..dark() };
	let center = Vec3::new(60.0, 0.0, 0.0);
	add_beam(&mut model, "fog-alpha", center + Vec3::new(0.0, 2.0, 0.0), at(-4.0, Quat::IDENTITY), at(4.0, Quat::IDENTITY), &[("Width0", 2.0f32.into()), ("Width1", 2.0f32.into()), ("Color", solid([1.0, 0.5, 0.2]))]);
	add_beam(&mut model, "fog-additive", center + Vec3::new(0.0, -2.0, 0.0), at(-4.0, Quat::IDENTITY), at(4.0, Quat::IDENTITY), &[("Width0", 2.0f32.into()), ("Width1", 2.0f32.into()), ("Color", solid([1.0, 0.5, 0.2])), ("LightEmission", 1.0f32.into())]);
	model.part(&PartSpec { name: "FogBlock".into(), cframe: Affine3A::from_translation(center + Vec3::new(6.0, 0.0, 0.0)), size: Vec3::new(2.0, 6.0, 0.5), color: [255, 255, 255], cast_shadow: false, ..Default::default() });
	for (label, environment) in [("fog", fog), ("clear", clear)] {
		runs.push(Run { label: label.into(), environment, camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -60.0)).into(), target: center.into(), fov: 20.0 } });
	}
	beam_backdrop(&mut model, Vec3::new(30.0, 0.0, 0.0), 200.0);
	Experiment { name: "beam-misc".into(), model, probes: Vec::new(), runs }
}

/// Neon emission per channel: tiles of many colours (varied channel ratios and levels) on a black
/// backdrop at ExposureCompensation -3 and -2, below both saturation and the bloom threshold.
pub fn neon_colors() -> Experiment {
	let mut model = ModelBuilder::new("NeonColors");
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)),
		size: Vec3::new(400.0, 200.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		..Default::default()
	});
	let mut colors: Vec<[u8; 3]> = Vec::new();
	for max in [255u8, 200, 150] {
		for second in [1.0f32, 0.8, 0.6, 0.4, 0.25, 0.1, 0.0] {
			let value = (max as f32 * second).round() as u8;
			colors.push([max, value, (value as f32 * 0.5).round() as u8]);
		}
	}
	colors.extend([[255, 239, 20], [255, 170, 0], [100, 150, 255], [60, 255, 120], [128, 128, 128], [255, 255, 255], [180, 90, 255]]);
	let mut probes = Vec::new();
	for (index, color) in colors.iter().enumerate() {
		let column = (index % 7) as f32;
		let row = (index / 7) as f32;
		let center = Vec3::new((3.0 - column) * 5.0, (1.5 - row) * 5.0, 0.0);
		let label = format!("neon{index}");
		model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(3.0, 3.0, 0.2), color: *color, material: 288, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label, position: (center - Vec3::Z * 0.1).into(), inputs: serde_json::json!({"color": color}) });
	}
	let runs = [-3.0f32, -2.0]
		.iter()
		.map(|&exposure| Run {
			label: format!("ev{exposure}"),
			environment: Environment { exposure_compensation: exposure, ..dark() },
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0, -2.5, 0.0], fov: 50.0 },
		})
		.collect();
	Experiment { name: "neon-colors".into(), model, probes, runs }
}

/// Transparent Neon (and SmoothPlastic for reference): blocks of three colours at six
/// transparencies in front of a backdrop that is black on the left and white on the right, so the
/// blend (alpha, additive, ...) and its view dependence (front face head-on, top face grazing)
/// can be read off.
pub fn neon_transparency() -> Experiment {
	let mut model = ModelBuilder::new("NeonTransparency");
	let mut probes = Vec::new();
	// black Neon on the left (x = 0), lit gray SmoothPlastic on the right (a known, moderate x)
	for (side, color, material) in [(-1.0f32, [0u8, 0, 0], 288u32), (1.0, [160, 160, 160], 272)] {
		model.part(&PartSpec {
			name: format!("Backdrop{side}"),
			cframe: glam::Affine3A::from_translation(Vec3::new(side * 25.0, 0.0, 8.0)),
			size: Vec3::new(50.0, 60.0, 1.0),
			color,
			material,
			..Default::default()
		});
	}
	let transparencies = [0.0f32, 0.3, 0.5, 0.7, 0.85, 0.95];
	let colors: [(&str, [u8; 3], u32); 4] = [("yellow", [255, 239, 20], 288), ("red", [255, 0, 0], 288), ("white", [255, 255, 255], 288), ("plastic", [255, 239, 20], 272)];
	for (row, (name, color, material)) in colors.iter().enumerate() {
		for (column, transparency) in transparencies.iter().enumerate() {
			for side in [-1.0f32, 1.0] {
				let center = Vec3::new(side * (3.0 + column as f32 * 3.6), (1.5 - row as f32) * 5.0, 0.0);
				let label = format!("{name}_t{transparency}_{}", if side < 0.0 { "black" } else { "white" });
				model.part(&PartSpec {
					name: label.clone(),
					cframe: glam::Affine3A::from_translation(center),
					size: Vec3::splat(2.0),
					color: *color,
					material: *material,
					transparency: *transparency,
					cast_shadow: false,
					..Default::default()
				});
				probes.push(Probe {
					label: format!("{label}_front"),
					position: (center - Vec3::Z * 1.0).into(),
					inputs: serde_json::json!({"color": color, "material": material, "transparency": transparency, "backdrop": if side < 0.0 { 0 } else { 160 }, "face": "front"}),
				});
				probes.push(Probe {
					label: format!("{label}_top"),
					position: (center + Vec3::Y * 1.0).into(),
					inputs: serde_json::json!({"color": color, "material": material, "transparency": transparency, "backdrop": if side < 0.0 { 0 } else { 160 }, "face": "top"}),
				});
			}
		}
	}
	let lit = Environment {
		brightness: 2.0,
		ambient: Vec3::splat(0.3),
		outdoor_ambient: Vec3::splat(0.3),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let runs = vec![
		Run { label: "front".into(), environment: lit.clone(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -60.0], target: [0.0, 0.0, 0.0], fov: 45.0 } },
		Run { label: "above".into(), environment: lit, camera: CameraSpec::LookAt { eye: [0.0, 32.0, -52.0], target: [0.0, 0.0, 0.0], fov: 45.0 } },
	];
	Experiment { name: "neon-transparency".into(), model, probes, runs }
}

/// The default sky alone: the camera at the origin looking along each axis (and diagonals) with a
/// 90° field of view, at noon and in the default afternoon; the skybox's face layout, orientation
/// and brightness.
pub fn sky_faces() -> Experiment {
	// one tiny far-away part so the model is not empty (it is never in view)
	let mut model = ModelBuilder::new("SkyFaces");
	model.part(&PartSpec { name: "Anchor".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -500.0, 0.0)), size: Vec3::splat(0.2), ..Default::default() });
	let views: [(&str, [f32; 3]); 8] = [
		("front", [0.0, 0.0, -1.0]),
		("back", [0.0, 0.0, 1.0]),
		("right", [1.0, 0.0, 0.0]),
		("left", [-1.0, 0.0, 0.0]),
		("up", [0.0, 1.0, 0.0001]),
		("down", [0.0, -1.0, 0.0001]),
		("front-right-up", [1.0, 0.7, -1.0]),
		("back-left-down", [-1.0, -0.7, 1.0]),
	];
	let mut runs = Vec::new();
	for (label, direction) in views {
		runs.push(Run {
			label: label.into(),
			environment: Environment::default(),
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: direction, fov: 90.0 },
		});
	}
	Experiment { name: "sky-faces".into(), model, probes: Vec::new(), runs }
}

/// The sun's specular on SmoothPlastic and other untextured materials: big spheres (black isolates
/// the specular lobe, gray / white / red show it with diffuse), sun only, seen from three angles to
/// the sun so the highlight sits at different view angles.
pub fn specular_spheres() -> Experiment {
	let environment = sun_only(2.0);
	let sun = sun_direction(environment.clock_time, environment.geographic_latitude);
	let materials: [(&str, u32, [u8; 3]); 8] = [
		("black", 272, [0, 0, 0]),
		("gray", 272, [128, 128, 128]),
		("white", 272, [255, 255, 255]),
		("red", 272, [200, 30, 30]),
		("plastic", 256, [128, 128, 128]),
		("metal", 1088, [128, 128, 128]),
		("glass", 1568, [128, 128, 128]),
		("foil", 1072, [128, 128, 128]),
	];
	// cameras at 10, 40 and 80 degrees from the sun (rotated about the vertical); each run gets its
	// own row of spheres across its view, so the experiment builds the model per view
	let mut model = ModelBuilder::new("SpecularSpheres");
	let mut runs = Vec::new();
	for (row, angle) in [10.0f32, 40.0, 80.0].iter().enumerate() {
		let direction = (glam::Quat::from_rotation_y(angle.to_radians()) * sun).normalize();
		let across = direction.cross(Vec3::Y).normalize();
		let origin = Vec3::new(0.0, row as f32 * 40.0, 0.0);
		for (index, (name, material, color)) in materials.iter().enumerate() {
			let center = origin + across * ((index as f32 - 3.5) * 6.0);
			model.part(&PartSpec { name: format!("{name}{row}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(5.0), color: *color, material: *material, shape: Some(0), ..Default::default() });
		}
		let eye = origin + direction * 120.0;
		runs.push(Run { label: format!("view{angle}"), environment: environment.clone(), camera: CameraSpec::LookAt { eye: eye.into(), target: origin.into(), fov: 26.0 } });
	}
	Experiment { name: "specular-spheres".into(), model, probes: Vec::new(), runs }
}

/// Every textured material on a white 36 x 36 stud slab seen straight down, under flat ambient
/// light (the albedo texture alone) and a low sun from +X (the normal map's relief): which texture
/// pack each material uses, and how it tiles.
pub fn material_swatches() -> Experiment {
	let materials: [(&str, u32); 41] = [
		("Plastic", 256),
		("SmoothPlastic", 272),
		("Wood", 512),
		("WoodPlanks", 528),
		("Marble", 784),
		("Basalt", 788),
		("Slate", 800),
		("CrackedLava", 804),
		("Concrete", 816),
		("Limestone", 820),
		("Granite", 832),
		("Pavement", 836),
		("Brick", 848),
		("Pebble", 864),
		("Cobblestone", 880),
		("Rock", 896),
		("Sandstone", 912),
		("CorrodedMetal", 1040),
		("DiamondPlate", 1056),
		("Foil", 1072),
		("Metal", 1088),
		("Grass", 1280),
		("LeafyGrass", 1284),
		("Sand", 1296),
		("Fabric", 1312),
		("Snow", 1328),
		("Mud", 1344),
		("Ground", 1360),
		("Asphalt", 1376),
		("Salt", 1392),
		("Ice", 1536),
		("Glacier", 1552),
		("Glass", 1568),
		("Cardboard", 2304),
		("Carpet", 2305),
		("CeramicTiles", 2306),
		("ClayRoofTiles", 2307),
		("RoofShingles", 2308),
		("Leather", 2309),
		("Plaster", 2310),
		("Rubber", 2311),
	];
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	// ClockTime 8: the sun 27 degrees above the horizon, from +X
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let mut model = ModelBuilder::new("MaterialSwatches");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: [255, 255, 255], material: *material, ..Default::default() });
		// straight down (a hair off vertical so the view has an up direction: +Z is up in the image)
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 60.0, -0.05)).into(), target: center.into(), fov: 20.0 };
		runs.push(Run { label: format!("{name}-flat"), environment: flat.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("{name}-sun"), environment: low_sun.clone(), camera });
	}
	Experiment { name: "material-swatches".into(), model, probes: Vec::new(), runs }
}

/// Surface types (Studs, Inlet, Glue, ...): white Plastic blocks with one type on top each; Studs on
/// other materials and colours; on a side face, a rotated block, odd sizes and positions, small
/// parts and the other part shapes. Flat ambient light (the texture's effect on the albedo) and a
/// low sun from either side (baked shading or a normal map), each block seen head-on.
pub fn surfaces() -> Experiment {
	use rbx_dom_weak::types::Enum;

	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	// ClockTime 8 / 16: the sun 27 degrees above the horizon, from +X / -X
	let sun_east = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let sun_west = Environment { clock_time: 16.0, ..sun_only(2.0) };
	let mut model = ModelBuilder::new("Surfaces");
	let mut runs = Vec::new();
	let top_view = |center: Vec3, height: f32| CameraSpec::LookAt { eye: (center + Vec3::new(0.0, height, -0.05)).into(), target: center.into(), fov: 20.0 };
	let block = |model: &mut ModelBuilder, name: &str, cframe: glam::Affine3A, size: Vec3, color: [u8; 3], material: u32, class: &'static str, shape: Option<u32>, faces: &[(&str, u32)]| {
		let part = model.part(&PartSpec { class, name: name.into(), cframe, size, color, material, shape, ..Default::default() });
		for (face, surface) in faces {
			model.set(part, face, Enum::from_u32(*surface));
		}
		part
	};

	// every type on top of a white Plastic block
	let types: [(&str, u32); 9] = [("Glue", 1), ("Weld", 2), ("Studs", 3), ("Inlet", 4), ("Universal", 5), ("Hinge", 6), ("Motor", 7), ("SteppingMotor", 8), ("SmoothNoOutlines", 10)];
	for (index, (name, surface)) in types.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		block(&mut model, name, glam::Affine3A::from_translation(center), Vec3::new(8.0, 2.0, 8.0), [255, 255, 255], 256, "Part", None, &[("TopSurface", *surface)]);
		let camera = top_view(center + Vec3::Y, 40.0);
		runs.push(Run { label: format!("type-{name}-flat"), environment: flat.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("type-{name}-east"), environment: sun_east.clone(), camera: camera.clone() });
		if *surface <= 5 {
			runs.push(Run { label: format!("type-{name}-west"), environment: sun_west.clone(), camera });
		}
	}

	// Studs on other materials
	let materials: [(&str, u32); 9] = [("SmoothPlastic", 272), ("Wood", 512), ("Slate", 800), ("Metal", 1088), ("Foil", 1072), ("Fabric", 1312), ("Neon", 288), ("Glass", 1568), ("ForceField", 1584)];
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 40.0);
		block(&mut model, name, glam::Affine3A::from_translation(center), Vec3::new(8.0, 2.0, 8.0), [255, 255, 255], *material, "Part", None, &[("TopSurface", 3)]);
		let camera = top_view(center + Vec3::Y, 40.0);
		runs.push(Run { label: format!("material-{name}-flat"), environment: flat.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("material-{name}-east"), environment: sun_east.clone(), camera });
	}

	// Studs and Inlets on colours
	let colors: [(&str, [u8; 3]); 4] = [("black", [0, 0, 0]), ("gray", [128, 128, 128]), ("red", [255, 0, 0]), ("blue", [13, 105, 172])];
	for (index, (name, color)) in colors.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 80.0);
		block(&mut model, name, glam::Affine3A::from_translation(center), Vec3::new(8.0, 2.0, 8.0), *color, 256, "Part", None, &[("TopSurface", 5)]);
		let camera = top_view(center + Vec3::Y, 40.0);
		runs.push(Run { label: format!("color-{name}-flat"), environment: flat.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("color-{name}-east"), environment: sun_east.clone(), camera });
	}

	// mapping: odd size and position, a rotated block, every face, small parts, other shapes
	let row = 120.0;
	let odd = Vec3::new(0.5, 0.0, row + 0.25);
	block(&mut model, "Odd", glam::Affine3A::from_translation(odd), Vec3::new(5.0, 2.0, 3.0), [255, 255, 255], 256, "Part", None, &[("TopSurface", 3)]);
	runs.push(Run { label: "map-odd-flat".into(), environment: flat.clone(), camera: top_view(odd + Vec3::Y, 30.0) });
	let odd2 = Vec3::new(40.3, 0.0, row - 0.4);
	block(&mut model, "Odd2", glam::Affine3A::from_translation(odd2), Vec3::new(7.4, 2.0, 6.6), [255, 255, 255], 256, "Part", None, &[("TopSurface", 5)]);
	runs.push(Run { label: "map-odd2-flat".into(), environment: flat.clone(), camera: top_view(odd2 + Vec3::Y, 30.0) });
	let rotated = Vec3::new(80.0, 0.0, row);
	block(&mut model, "Rotated", glam::Affine3A::from_translation(rotated) * glam::Affine3A::from_rotation_y(37f32.to_radians()), Vec3::new(8.0, 2.0, 6.0), [255, 255, 255], 256, "Part", None, &[("TopSurface", 5)]);
	runs.push(Run { label: "map-rotated-flat".into(), environment: flat.clone(), camera: top_view(rotated + Vec3::Y, 40.0) });
	runs.push(Run { label: "map-rotated-east".into(), environment: sun_east.clone(), camera: top_view(rotated + Vec3::Y, 40.0) });
	// every face Universal (asymmetric), seen from each side
	let cube = Vec3::new(120.0, 0.0, row);
	let all: Vec<(&str, u32)> = ["TopSurface", "BottomSurface", "FrontSurface", "BackSurface", "LeftSurface", "RightSurface"].iter().map(|face| (*face, 5)).collect();
	block(&mut model, "Cube", glam::Affine3A::from_translation(cube), Vec3::new(8.0, 6.0, 4.0), [255, 255, 255], 256, "Part", None, &all);
	for (side, offset) in [("front", Vec3::new(0.0, 0.0, -40.0)), ("back", Vec3::new(0.0, 0.0, 40.0)), ("left", Vec3::new(-40.0, 0.0, 0.0)), ("right", Vec3::new(40.0, 0.0, 0.0)), ("bottom", Vec3::new(0.0, -40.0, 0.05))] {
		runs.push(Run { label: format!("map-cube-{side}-flat"), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (cube + offset).into(), target: cube.into(), fov: 20.0 } });
	}
	runs.push(Run { label: "map-cube-iso-east".into(), environment: sun_east.clone(), camera: CameraSpec::LookAt { eye: (cube + Vec3::new(-25.0, 25.0, -25.0)).into(), target: cube.into(), fov: 25.0 } });
	// small parts: Studs on 1 x 1, 2 x 2 and 1 x 0.4 x 1 tops, and a 4 x 1 x 2 brick with the class defaults
	let small = Vec3::new(160.0, 0.0, row);
	for (index, size) in [Vec3::new(1.0, 1.0, 1.0), Vec3::new(2.0, 1.0, 2.0), Vec3::new(1.0, 0.4, 1.0), Vec3::new(3.0, 0.2, 3.0)].iter().enumerate() {
		block(&mut model, &format!("Small{index}"), glam::Affine3A::from_translation(small + Vec3::new(index as f32 * 4.0 - 6.0, 0.0, 0.0)), *size, [255, 255, 255], 256, "Part", None, &[("TopSurface", 3)]);
	}
	model.part(&PartSpec { name: "Defaults".into(), cframe: glam::Affine3A::from_translation(small + Vec3::new(0.0, 0.0, 5.0)), size: Vec3::new(4.0, 1.0, 2.0), color: [255, 255, 255], material: 256, smooth: false, ..Default::default() });
	runs.push(Run { label: "map-small-flat".into(), environment: flat.clone(), camera: top_view(small + Vec3::new(0.0, 1.0, 2.5), 40.0) });
	// a wedge, a corner wedge and a cylinder with every face Studs, and a truss
	let shapes = Vec3::new(200.0, 0.0, row);
	let studs: Vec<(&str, u32)> = ["TopSurface", "BottomSurface", "FrontSurface", "BackSurface", "LeftSurface", "RightSurface"].iter().map(|face| (*face, 3)).collect();
	block(&mut model, "Wedge", glam::Affine3A::from_translation(shapes + Vec3::new(-8.0, 0.0, 0.0)), Vec3::new(4.0, 4.0, 6.0), [255, 255, 255], 256, "WedgePart", None, &studs);
	block(&mut model, "CornerWedge", glam::Affine3A::from_translation(shapes), Vec3::new(4.0, 4.0, 4.0), [255, 255, 255], 256, "CornerWedgePart", None, &studs);
	block(&mut model, "Cylinder", glam::Affine3A::from_translation(shapes + Vec3::new(8.0, 0.0, 0.0)), Vec3::new(4.0, 4.0, 4.0), [255, 255, 255], 256, "Part", Some(2), &studs);
	for (side, offset) in [("iso", Vec3::new(-20.0, 20.0, -20.0)), ("iso2", Vec3::new(20.0, 20.0, 20.0))] {
		runs.push(Run { label: format!("map-shapes-{side}-flat"), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (shapes + offset).into(), target: shapes.into(), fov: 40.0 } });
	}
	Experiment { name: "surfaces".into(), model, probes: Vec::new(), runs }
}

/// Which surface property draws where on the other shapes: rows of wedges, corner wedges, cylinders
/// and balls with Studs on one face each (Top, Bottom, Front, Back, Left, Right), seen from two
/// opposite corners; and Hinge / Motor / SteppingMotor on a block's front, seen from the side.
pub fn surfaces_shapes() -> Experiment {
	use rbx_dom_weak::types::Enum;

	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("SurfaceShapes");
	let mut runs = Vec::new();
	let faces = ["TopSurface", "BottomSurface", "FrontSurface", "BackSurface", "LeftSurface", "RightSurface"];
	let shapes: [(&str, &'static str, Option<u32>); 4] = [("wedge", "WedgePart", None), ("cornerwedge", "CornerWedgePart", None), ("cylinder", "Part", Some(2)), ("ball", "Part", Some(0))];
	for (row, (name, class, shape)) in shapes.iter().enumerate() {
		let z = row as f32 * 40.0;
		for (index, face) in faces.iter().enumerate() {
			let center = Vec3::new(index as f32 * 10.0, 0.0, z);
			let part = model.part(&PartSpec { class, name: format!("{name}-{face}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(6.0), color: [255, 255, 255], material: 256, shape: *shape, ..Default::default() });
			model.set(part, face, Enum::from_u32(3));
		}
		let middle = Vec3::new(25.0, 0.0, z);
		for (side, offset) in [("a", Vec3::new(-30.0, 30.0, -30.0)), ("b", Vec3::new(30.0, 30.0, 30.0)), ("c", Vec3::new(-30.0, -30.0, 30.0))] {
			runs.push(Run { label: format!("{name}-{side}"), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (middle + offset).into(), target: middle.into(), fov: 55.0 } });
		}
	}
	// the joint surfaces on a block's front, seen from the front and from the side
	let joints: [(&str, u32); 3] = [("Hinge", 6), ("Motor", 7), ("SteppingMotor", 8)];
	for (index, (name, surface)) in joints.iter().enumerate() {
		let center = Vec3::new(index as f32 * 20.0, 0.0, 200.0);
		let part = model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(6.0, 6.0, 2.0), color: [255, 255, 255], material: 256, ..Default::default() });
		model.set(part, "FrontSurface", Enum::from_u32(*surface));
		runs.push(Run { label: format!("joint-{name}-front"), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -20.0)).into(), target: center.into(), fov: 20.0 } });
		runs.push(Run { label: format!("joint-{name}-side"), environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(14.0, 3.0, -14.0)).into(), target: (center + Vec3::new(0.0, 0.0, -1.0)).into(), fov: 15.0 } });
		runs.push(Run { label: format!("joint-{name}-sun"), environment: Environment { clock_time: 8.0, ..sun_only(2.0) }, camera: CameraSpec::LookAt { eye: (center + Vec3::new(14.0, 3.0, -14.0)).into(), target: (center + Vec3::new(0.0, 0.0, -1.0)).into(), fov: 15.0 } });
	}
	Experiment { name: "surfaces-shapes".into(), model, probes: Vec::new(), runs }
}


/// Highlights: the default one (red fill at 0.5, white outline) on a block, outline only (green,
/// black), fill only (blue at 0.25), on a ball half behind a wall with DepthMode Occluded and
/// AlwaysOnTop, on a two-part model (the union's silhouette) and through Adornee, under flat ambient
/// light; seen head-on at two distances (outline width in pixels) and from above.
pub fn highlights() -> Experiment {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, Enum};

	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("Highlights");
	let root = model.model;
	let mut runs = Vec::new();
	let highlight = |fill: [f32; 3], fill_transparency: f32, outline: [f32; 3], outline_transparency: f32, depth_mode: u32| {
		InstanceBuilder::new("Highlight")
			.with_property("FillColor", Color3::new(fill[0], fill[1], fill[2]))
			.with_property("FillTransparency", fill_transparency)
			.with_property("OutlineColor", Color3::new(outline[0], outline[1], outline[2]))
			.with_property("OutlineTransparency", outline_transparency)
			.with_property("DepthMode", Enum::from_u32(depth_mode))
	};
	let gray = [128, 128, 128];
	let cases: [(&str, [f32; 3], f32, [f32; 3], f32); 5] = [
		("default", [1.0, 0.0, 0.0], 0.5, [1.0, 1.0, 1.0], 0.0),
		("outline-green", [1.0, 0.0, 0.0], 1.0, [0.0, 1.0, 0.0], 0.0),
		("outline-black", [1.0, 0.0, 0.0], 1.0, [0.0, 0.0, 0.0], 0.0),
		("fill-blue", [0.0, 0.0, 1.0], 0.25, [1.0, 1.0, 1.0], 1.0),
		("half", [1.0, 1.0, 0.0], 0.75, [1.0, 0.0, 1.0], 0.5),
	];
	for (index, (name, fill, fill_transparency, outline, outline_transparency)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 3.0, 0.0);
		let part = model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(6.0), color: gray, material: 272, ..Default::default() });
		model.child(part, highlight(*fill, *fill_transparency, *outline, *outline_transparency, 0));
		for (label, distance) in [("near", 20.0), ("far", 60.0)] {
			let camera = CameraSpec::LookAt { eye: (center + Vec3::new(-0.3 * distance, 0.4 * distance, -distance)).into(), target: center.into(), fov: 30.0 };
			runs.push(Run { label: format!("{name}-{label}"), environment: flat.clone(), camera });
		}
	}
	// a ball half behind a wall: Occluded and AlwaysOnTop
	for (index, (name, depth_mode)) in [("occluded", 1u32), ("ontop", 0)].iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 3.0, 40.0);
		let ball = model.part(&PartSpec { name: format!("ball-{name}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(6.0), color: gray, material: 272, shape: Some(0), ..Default::default() });
		model.child(ball, highlight([1.0, 0.0, 0.0], 0.5, [1.0, 1.0, 1.0], 0.0, *depth_mode));
		model.part(&PartSpec { name: format!("wall-{name}"), cframe: glam::Affine3A::from_translation(center + Vec3::new(2.5, 0.0, -6.0)), size: Vec3::new(5.0, 10.0, 1.0), color: [60, 90, 160], material: 272, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 2.0, -25.0)).into(), target: center.into(), fov: 30.0 };
		runs.push(Run { label: format!("ball-{name}"), environment: flat.clone(), camera });
	}
	// a two-part model (an L) under one Highlight, and a Highlight elsewhere pointing at a part
	let group = model.child(root, InstanceBuilder::new("Model").with_name("Group"));
	let center = Vec3::new(60.0, 3.0, 40.0);
	for (offset, size) in [(Vec3::ZERO, Vec3::new(8.0, 3.0, 3.0)), (Vec3::new(2.5, 4.0, 0.0), Vec3::new(3.0, 5.0, 3.0))] {
		let part = model.part(&PartSpec { name: "Piece".into(), cframe: glam::Affine3A::from_translation(center + offset), size, color: gray, material: 272, ..Default::default() });
		model.dom.transfer_within(part, group);
	}
	model.child(group, highlight([0.0, 1.0, 1.0], 0.5, [0.0, 0.0, 0.0], 0.0, 0));
	let camera = CameraSpec::LookAt { eye: (center + Vec3::new(-8.0, 8.0, -25.0)).into(), target: (center + Vec3::new(0.0, 2.0, 0.0)).into(), fov: 30.0 };
	runs.push(Run { label: "model".into(), environment: flat.clone(), camera });
	let target = model.part(&PartSpec { name: "Adorned".into(), cframe: glam::Affine3A::from_translation(Vec3::new(90.0, 3.0, 40.0)), size: Vec3::splat(6.0), color: gray, material: 272, ..Default::default() });
	let adorning = model.child(root, highlight([1.0, 0.5, 0.0], 0.5, [1.0, 1.0, 1.0], 0.0, 0).with_name("AdornHighlight"));
	model.set(adorning, "Adornee", target);
	let camera = CameraSpec::LookAt { eye: Vec3::new(90.0 - 6.0, 11.0, 40.0 - 20.0).into(), target: Vec3::new(90.0, 3.0, 40.0).into(), fov: 30.0 };
	runs.push(Run { label: "adornee".into(), environment: flat.clone(), camera });
	// from above, the default one
	let camera = CameraSpec::LookAt { eye: Vec3::new(0.0, 40.0, -0.05).into(), target: Vec3::new(0.0, 3.0, 0.0).into(), fov: 30.0 };
	runs.push(Run { label: "default-top".into(), environment: flat, camera });
	Experiment { name: "highlights".into(), model, probes: Vec::new(), runs }
}


/// Anti-aliasing: black SmoothPlastic bars at 3, 17, 30 and 45 degrees and a black ball in front of a
/// white SmoothPlastic wall, flat ambient light only (black 0, the wall one level), seen head-on: the
/// edge pixels' values give the sample count and the space the samples are averaged in.
pub fn aa_edges() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("AaEdges");
	// the wall far behind the bars: Studio's ambient occlusion darkens it within ~1 stud of a bar
	// 1 stud in front (aa-edges at gap 1)
	model.part(&PartSpec { name: "Wall".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 30.0)), size: Vec3::new(200.0, 140.0, 1.0), color: [255, 255, 255], material: 272, cast_shadow: false, ..Default::default() });
	for (index, angle) in [3.0f32, 17.0, 30.0, 45.0].iter().enumerate() {
		let center = Vec3::new(-12.0 + index as f32 * 8.0, 0.0, 0.0);
		let cframe = glam::Affine3A::from_translation(center) * glam::Affine3A::from_rotation_z(angle.to_radians());
		model.part(&PartSpec { name: format!("Bar{angle}"), cframe, size: Vec3::new(2.0, 14.0, 1.0), color: [0, 0, 0], material: 272, cast_shadow: false, ..Default::default() });
	}
	model.part(&PartSpec { name: "Ball".into(), cframe: glam::Affine3A::from_translation(Vec3::new(-14.0, 10.0, 0.0)), size: Vec3::splat(5.0), color: [0, 0, 0], material: 272, shape: Some(0), cast_shadow: false, ..Default::default() });
	let camera = CameraSpec::LookAt { eye: Vec3::new(-2.0, 2.0, -50.0).into(), target: Vec3::new(-2.0, 2.0, 0.0).into(), fov: 40.0 };
	let runs = vec![Run { label: "flat".into(), environment: flat, camera }];
	Experiment { name: "aa-edges-far".into(), model, probes: Vec::new(), runs }
}

/// Roblox's light grid (sky and outdoor ambient occluded by parts): white floors, each half under
/// a roof at 2, 4, 8 and 16 studs, and white cubes of 1, 2, 4 and 8 studs standing alone; seen from
/// above-front under OutdoorAmbient alone, Ambient alone, the sky's diffuse alone and the sun alone.
pub fn lightgrid() -> Experiment {
	let off = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: true,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("LightGrid");
	for (index, height) in [2.0f32, 4.0, 8.0, 16.0].iter().enumerate() {
		let x = index as f32 * 60.0;
		model.part(&PartSpec { name: format!("Floor{height}"), cframe: glam::Affine3A::from_translation(Vec3::new(x, -0.5, 0.0)), size: Vec3::new(40.0, 1.0, 40.0), color: [255, 255, 255], material: 272, ..Default::default() });
		model.part(&PartSpec { name: format!("Roof{height}"), cframe: glam::Affine3A::from_translation(Vec3::new(x, height + 0.5, 10.0)), size: Vec3::new(40.0, 1.0, 20.0), color: [255, 255, 255], material: 272, ..Default::default() });
	}
	for (index, size) in [1.0f32, 2.0, 4.0, 8.0].iter().enumerate() {
		let x = index as f32 * 30.0;
		model.part(&PartSpec { name: format!("Cube{size}"), cframe: glam::Affine3A::from_translation(Vec3::new(x, size * 0.5, 120.0)), size: Vec3::splat(*size), color: [255, 255, 255], material: 272, ..Default::default() });
	}
	let environments = [
		("outdoor", Environment { outdoor_ambient: Vec3::splat(0.7), ..off.clone() }),
		("ambient", Environment { ambient: Vec3::splat(0.7), ..off.clone() }),
		("sky", Environment { environment_diffuse_scale: 1.0, ..off.clone() }),
		("sun", Environment { brightness: 2.0, ..off.clone() }),
	];
	let mut runs = Vec::new();
	for (label, environment) in &environments {
		for (index, height) in [2.0f32, 4.0, 8.0, 16.0].iter().enumerate() {
			// from the open side, looking in under the roof
			let x = index as f32 * 60.0;
			let camera = CameraSpec::LookAt { eye: Vec3::new(x, 30.0, -45.0).into(), target: Vec3::new(x, 0.0, 2.0).into(), fov: 50.0 };
			runs.push(Run { label: format!("roof{height}-{label}"), environment: environment.clone(), camera });
		}
		let camera = CameraSpec::LookAt { eye: Vec3::new(45.0, 25.0, 80.0).into(), target: Vec3::new(45.0, 2.0, 120.0).into(), fov: 50.0 };
		runs.push(Run { label: format!("cubes-{label}"), environment: environment.clone(), camera });
	}
	Experiment { name: "lightgrid".into(), model, probes: Vec::new(), runs }
}


/// The tone curve on bright saturated colours: Neon panels filling the view, one per colour, with
/// a BloomEffect of Intensity 0 (no halo) and no other light; per channel 25.75 c⁴ of emission,
/// some channels far above 1 while others stay below (a trophy room's Neon ceiling showed blue 20 of
/// 255 higher in Studio than ror's per-channel curve).
pub fn neon_slabs() -> Experiment {
	use crate::scene::environment::PostEffect;

	let colors: [(&str, [u8; 3]); 10] = [
		("red", [255, 0, 0]),
		("orange", [255, 128, 0]),
		("gold", [191, 158, 84]),
		("yellow", [255, 255, 64]),
		("blue", [0, 0, 255]),
		("lilac", [128, 128, 255]),
		("cyan", [0, 255, 255]),
		("dim-gold", [96, 79, 42]),
		("mid-gold", [143, 118, 63]),
		("white", [200, 200, 200]),
	];
	let dark = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }],
		..Environment::default()
	};
	let mut model = ModelBuilder::new("NeonSlabs");
	let mut runs = Vec::new();
	let mut probes = Vec::new();
	for (index, (name, color)) in colors.iter().enumerate() {
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(40.0, 30.0, 1.0), color: *color, material: 288, cast_shadow: false, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -20.0)).into(), target: center.into(), fov: 40.0 };
		runs.push(Run { label: (*name).into(), environment: dark.clone(), camera: camera.clone() });
		// and with Studio's built-in bloom (no BloomEffect)
		if matches!(*name, "gold" | "white" | "red") {
			runs.push(Run { label: format!("{name}-bloom"), environment: Environment { effects: Vec::new(), ..dark.clone() }, camera });
		}
		probes.push(Probe { label: (*name).into(), position: (center + Vec3::new(0.0, 0.0, -0.5)).into(), inputs: serde_json::json!({"color": color}) });
	}
	Experiment { name: "neon-slabs".into(), model, probes, runs }
}

/// The built-in bloom inside large Neon areas: panels filling the view whose dominant channel is
/// far above the bloom threshold and the others below 1, so the halo's share of the weaker channels
/// shows (neon-slabs gold: blue 171 in Studio, 153 in ror).
pub fn neon_slabs_bloom() -> Experiment {
	let colors: [(&str, [u8; 3]); 9] = [
		("r255-g96-b48", [255, 96, 48]),
		("r255-g128-b64", [255, 128, 64]),
		("r255-g40-b40", [255, 40, 40]),
		("r200-g100-b50", [200, 100, 50]),
		("r160-g80-b40", [160, 80, 40]),
		("r130-g65-b33", [130, 65, 33]),
		("b255-r64-g64", [64, 64, 255]),
		("g255-r96-b48", [96, 255, 48]),
		("r255-g0-b0", [255, 0, 0]),
	];
	let dark = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("NeonSlabsBloom");
	let mut runs = Vec::new();
	let mut probes = Vec::new();
	for (index, (name, color)) in colors.iter().enumerate() {
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(40.0, 30.0, 1.0), color: *color, material: 288, cast_shadow: false, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -20.0)).into(), target: center.into(), fov: 40.0 };
		runs.push(Run { label: (*name).into(), environment: dark.clone(), camera });
		probes.push(Probe { label: (*name).into(), position: (center + Vec3::new(0.0, 0.0, -0.5)).into(), inputs: serde_json::json!({"color": color}) });
	}
	Experiment { name: "neon-slabs-bloom".into(), model, probes, runs }
}


/// Neon by viewing angle (a character close-up: Studio's domed Neon goggle lenses show their saturated
/// colour in a ring towards the rim, ror's are flat): Neon balls of four colours and Neon tiles
/// turned 0-75 degrees from the camera, in the dark with a BloomEffect of Intensity 0, at
/// ExposureCompensation 0 and -3 (nothing clips at -3).
pub fn neon_angles() -> Experiment {
	use crate::scene::environment::PostEffect;

	let colors: [(&str, [u8; 3]); 4] = [("goggles", [186, 119, 97]), ("white", [200, 200, 200]), ("gray", [120, 120, 120]), ("orange", [255, 128, 0])];
	let angles = [0.0f32, 30.0, 45.0, 60.0, 75.0];
	let mut model = ModelBuilder::new("NeonAngles");
	let mut probes = Vec::new();
	for (index, (name, color)) in colors.iter().enumerate() {
		let center = Vec3::new(index as f32 * 6.0 - 9.0, 4.0, 0.0);
		model.part(&PartSpec { name: format!("ball-{name}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(4.0), color: *color, material: 288, cast_shadow: false, shape: Some(0), ..Default::default() });
	}
	for (row, (name, color)) in [colors[0], colors[2]].iter().enumerate() {
		for (index, angle) in angles.iter().enumerate() {
			let center = Vec3::new(index as f32 * 5.0 - 10.0, -2.0 - row as f32 * 5.0, 0.0);
			let cframe = glam::Affine3A::from_translation(center) * glam::Affine3A::from_rotation_y(angle.to_radians());
			model.part(&PartSpec { name: format!("tile-{name}-{angle}"), cframe, size: Vec3::new(3.0, 3.0, 0.4), color: *color, material: 288, cast_shadow: false, ..Default::default() });
			let normal = glam::Quat::from_rotation_y(angle.to_radians()) * Vec3::NEG_Z;
			probes.push(Probe { label: format!("tile-{name}-{angle}"), position: (center + normal * 0.2).into(), inputs: serde_json::json!({"color": color, "angle": angle}) });
		}
	}
	let dark = |exposure: f32| Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		exposure_compensation: exposure,
		effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }],
		..Environment::default()
	};
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -45.0], target: [0.0, 0.0, 0.0], fov: 30.0 };
	let runs = vec![
		Run { label: "ev0".into(), environment: dark(0.0), camera: camera.clone() },
		Run { label: "ev-3".into(), environment: dark(-3.0), camera },
	];
	Experiment { name: "neon-angles".into(), model, probes, runs }
}

/// The tone curve's top end: gray Neon panels filling the view at 25.75 c⁴ from 1.6 to 25.75, with a
/// BloomEffect of Intensity 0 (no halo) and no other light (neon-slabs: ror's fitted curve reads 3-4
/// of 255 high near x = 4 and Studio tops out at 253).
pub fn neon_tone() -> Experiment {
	use crate::scene::environment::PostEffect;

	let levels: [u8; 12] = [128, 143, 150, 158, 163, 168, 174, 180, 189, 199, 217, 255];
	let dark = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }],
		..Environment::default()
	};
	let mut model = ModelBuilder::new("NeonTone");
	let mut runs = Vec::new();
	let mut probes = Vec::new();
	for (index, level) in levels.iter().enumerate() {
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("gray{level}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(40.0, 30.0, 1.0), color: [*level; 3], material: 288, cast_shadow: false, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -20.0)).into(), target: center.into(), fov: 40.0 };
		runs.push(Run { label: format!("gray{level}"), environment: dark.clone(), camera });
		probes.push(Probe { label: format!("gray{level}"), position: (center + Vec3::new(0.0, 0.0, -0.5)).into(), inputs: serde_json::json!({"level": level}) });
	}
	Experiment { name: "neon-tone".into(), model, probes, runs }
}

/// The tone curve's top end on lit surfaces (not Neon): white and gray SmoothPlastic tiles facing the
/// sun at Brightness 2 to 12, with a BloomEffect of Intensity 0 and no other light (does the 253
/// ceiling neon-tone shows hold for lit surfaces, and is Neon's 25.75 c⁴ right up there?).
pub fn tone_top() -> Experiment {
	use crate::scene::environment::PostEffect;

	let base = Environment {
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }],
		..Environment::default()
	};
	let sun = sun_direction(base.clock_time, base.geographic_latitude);
	let mut model = ModelBuilder::new("ToneTop");
	let mut probes = Vec::new();
	for (index, (name, level)) in [("white", 255u8), ("gray", 181), ("dark", 128)].iter().enumerate() {
		let up = sun.cross(Vec3::Y).normalize();
		let center = up * (index as f32 * 12.0 - 12.0);
		model.part(&PartSpec { name: (*name).into(), cframe: facing(center, sun), size: Vec3::new(10.0, 10.0, 1.0), color: [*level; 3], material: 272, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label: (*name).into(), position: (center + sun * 0.6).into(), inputs: serde_json::json!({"level": level}) });
	}
	let camera = CameraSpec::LookAt { eye: (sun * 190.0).into(), target: Vec3::ZERO.into(), fov: 12.0 };
	let runs = [2.0f32, 3.0, 4.0, 5.0, 6.0, 8.0, 12.0].iter().map(|brightness| Run { label: format!("b{brightness}"), environment: Environment { brightness: *brightness, ..base.clone() }, camera: camera.clone() }).collect();
	Experiment { name: "tone-top".into(), model, probes, runs }
}

/// The bloom's widest component: Neon squares of 2, 6 and 12 studs (red 255, 96, 48 and white) in the
/// dark, the halo profile out to ~600 px (neon-slabs-bloom: inside large Neon areas Studio's glow is
/// 1.8 x the emission per channel, ror's two Gaussians 1.16).
pub fn bloom_wide() -> Experiment {
	let dark = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("BloomWide");
	// a black backdrop so the sky does not add light
	model.part(&PartSpec { name: "Backdrop".into(), cframe: glam::Affine3A::from_translation(Vec3::new(100.0, 0.0, 30.0)), size: Vec3::new(600.0, 400.0, 1.0), color: [0, 0, 0], material: 272, cast_shadow: false, ..Default::default() });
	let mut runs = Vec::new();
	for (index, (name, size, color)) in [("red2", 2.0f32, [255u8, 96, 48]), ("red6", 6.0, [255, 96, 48]), ("red12", 12.0, [255, 96, 48]), ("white6", 6.0, [255, 255, 255])].iter().enumerate() {
		let center = Vec3::new(index as f32 * 70.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(*size, *size, 0.5), color: *color, material: 288, cast_shadow: false, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -60.0)).into(), target: center.into(), fov: 40.0 };
		runs.push(Run { label: (*name).into(), environment: dark.clone(), camera });
	}
	Experiment { name: "bloom-wide".into(), model, probes: Vec::new(), runs }
}

/// The material texture's phase on blocks inside a Humanoid's model: white Brick cubes (not named as
/// limbs) moved by whole and fractional studs along each axis, turned, and of another size, each seen
/// head-on from -Z under flat light. Run twice (`ROR_RECAPTURE=1`): does the phase follow the part's
/// position, or is it drawn per part and load?
pub fn humanoid_phase() -> Experiment {
	use rbx_dom_weak::InstanceBuilder;

	let mut model = ModelBuilder::new("HumanoidPhase");
	let root = model.model;
	model.child(root, InstanceBuilder::new("Humanoid"));
	let mut runs = Vec::new();
	let cases: [(&str, Vec3, f32, Vec3); 10] = [
		("a", Vec3::ZERO, 0.0, Vec3::splat(8.0)),
		("x1", Vec3::new(1.0, 0.0, 0.0), 0.0, Vec3::splat(8.0)),
		("x2.5", Vec3::new(2.5, 0.0, 0.0), 0.0, Vec3::splat(8.0)),
		("x8", Vec3::new(8.0, 0.0, 0.0), 0.0, Vec3::splat(8.0)),
		("y1", Vec3::new(0.0, 1.0, 0.0), 0.0, Vec3::splat(8.0)),
		("y2.5", Vec3::new(0.0, 2.5, 0.0), 0.0, Vec3::splat(8.0)),
		("z1", Vec3::new(0.0, 0.0, 1.0), 0.0, Vec3::splat(8.0)),
		("turned", Vec3::ZERO, 90.0, Vec3::splat(8.0)),
		("wide", Vec3::ZERO, 0.0, Vec3::new(12.0, 8.0, 8.0)),
		("same", Vec3::ZERO, 0.0, Vec3::splat(8.0)),
	];
	for (index, (label, offset, turn, size)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0) + *offset;
		let cframe = glam::Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(turn.to_radians()), center);
		model.part(&PartSpec { name: format!("Cube-{label}"), cframe, size: *size, color: [255, 255, 255], material: 848, ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -24.0)).into(), target: center.into(), fov: 30.0 };
		runs.push(Run { label: (*label).into(), environment: flat_ambient(), camera });
	}
	Experiment { name: "humanoid-phase".into(), model, probes: Vec::new(), runs }
}

/// How material textures map onto parts: white Brick (a regular, oriented pattern) on cubes at
/// grid-aligned and odd positions, rotated, and of uneven size, on the other part shapes and on a
/// MeshPart (Studio's head mesh), plus Grass (Organic) cubes at two positions; flat ambient light,
/// every face head-on.
pub fn material_mapping() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("MaterialMapping");
	let mut runs = Vec::new();
	let axes: [(&str, Vec3); 6] = [("px", Vec3::X), ("nx", Vec3::NEG_X), ("py", Vec3::Y), ("ny", Vec3::NEG_Y), ("pz", Vec3::Z), ("nz", Vec3::NEG_Z)];
	let mut view = |label: String, center: Vec3, direction: Vec3, distance: f32| {
		// a hair off the vertical for views along Y so the camera has an up direction (+Z, -Z)
		let direction = if direction.y.abs() > 0.99 { (direction + Vec3::new(0.0, 0.0, -0.001 * direction.y.signum())).normalize() } else { direction.normalize() };
		runs.push(Run { label, environment: flat.clone(), camera: CameraSpec::LookAt { eye: (center + direction * distance).into(), target: center.into(), fov: 20.0 } });
	};
	let brick = 848;
	let part = |model: &mut ModelBuilder, name: &str, class: &'static str, cframe: glam::Affine3A, size: Vec3, material: u32, shape: Option<u32>| {
		model.part(&PartSpec { class, name: name.into(), cframe, size, color: [255, 255, 255], material, shape, ..Default::default() })
	};
	let cube = Vec3::splat(16.0);
	// grid-aligned cube, every face
	let a = Vec3::ZERO;
	part(&mut model, "CubeA", "Part", glam::Affine3A::from_translation(a), cube, brick, None);
	for (name, axis) in axes {
		view(format!("cubeA-{name}"), a, axis, 70.0);
	}
	// off-grid position: world or part space?
	let b = Vec3::new(203.3, 5.1, 1.7);
	part(&mut model, "CubeB", "Part", glam::Affine3A::from_translation(b), cube, brick, None);
	view("cubeB-py".into(), b, Vec3::Y, 70.0);
	view("cubeB-nz".into(), b, Vec3::NEG_Z, 70.0);
	// rotated 30 degrees about Y
	let c = Vec3::new(400.0, 0.0, 0.0);
	let rotation = glam::Quat::from_rotation_y(30f32.to_radians());
	part(&mut model, "CubeC", "Part", glam::Affine3A::from_rotation_translation(rotation, c), cube, brick, None);
	view("cubeC-py".into(), c, Vec3::Y, 70.0);
	view("cubeC-nz".into(), c, rotation * Vec3::NEG_Z, 70.0);
	// uneven size: where the pattern starts on each face
	let d = Vec3::new(600.0, 0.0, 0.0);
	part(&mut model, "BoxD", "Part", glam::Affine3A::from_translation(d), Vec3::new(13.0, 9.0, 11.0), brick, None);
	for (name, axis) in axes {
		view(format!("boxD-{name}"), d, axis, 70.0);
	}
	// the other shapes
	let e = Vec3::new(800.0, 0.0, 0.0);
	part(&mut model, "Ball", "Part", glam::Affine3A::from_translation(e), cube, brick, Some(0));
	for (name, axis) in [("py", Vec3::Y), ("nz", Vec3::NEG_Z), ("px", Vec3::X), ("iso", Vec3::new(1.0, 0.8, -1.0))] {
		view(format!("ball-{name}"), e, axis, 70.0);
	}
	let f = Vec3::new(1000.0, 0.0, 0.0);
	part(&mut model, "Cylinder", "Part", glam::Affine3A::from_translation(f), cube, brick, Some(2));
	for (name, axis) in [("px", Vec3::X), ("nz", Vec3::NEG_Z), ("py", Vec3::Y), ("iso", Vec3::new(1.0, 0.8, -1.0))] {
		view(format!("cylinder-{name}"), f, axis, 70.0);
	}
	let g = Vec3::new(1200.0, 0.0, 0.0);
	part(&mut model, "Wedge", "WedgePart", glam::Affine3A::from_translation(g), cube, brick, None);
	for (name, axis) in [("nz", Vec3::NEG_Z), ("py", Vec3::Y), ("px", Vec3::X), ("slope", Vec3::new(0.0, 1.0, -1.0)), ("iso", Vec3::new(1.0, 0.8, -1.0))] {
		view(format!("wedge-{name}"), g, axis, 70.0);
	}
	let h = Vec3::new(1400.0, 0.0, 0.0);
	part(&mut model, "CornerWedge", "CornerWedgePart", glam::Affine3A::from_translation(h), cube, brick, None);
	for (name, axis) in [("iso", Vec3::new(1.0, 0.8, -1.0)), ("py", Vec3::Y), ("nz", Vec3::NEG_Z)] {
		view(format!("cornerwedge-{name}"), h, axis, 70.0);
	}
	// a MeshPart: Studio's classic head mesh (UV-mapped) at 16 studs
	let m = Vec3::new(1600.0, 0.0, 0.0);
	let mesh = part(&mut model, "MeshHead", "MeshPart", glam::Affine3A::from_translation(m), cube, brick, None);
	model.set(mesh, "MeshId", rbx_dom_weak::types::ContentId::from("rbxasset://avatar/heads/head.mesh"));
	model.set(mesh, "InitialSize", rbx_dom_weak::types::Vector3::new(1.197849, 1.2024246, 1.197849));
	for (name, axis) in [("nz", Vec3::NEG_Z), ("px", Vec3::X), ("py", Vec3::Y)] {
		view(format!("meshhead-{name}"), m, axis, 70.0);
	}
	// Organic (Grass): the same cube at a grid-aligned and an odd position
	for (name, center) in [("grassA", Vec3::new(1800.0, 0.0, 0.0)), ("grassB", Vec3::new(2003.3, 5.1, 1.7))] {
		part(&mut model, name, "Part", glam::Affine3A::from_translation(center), cube, 1280, None);
		view(format!("{name}-py"), center, Vec3::Y, 70.0);
		view(format!("{name}-nz"), center, Vec3::NEG_Z, 70.0);
	}
	Experiment { name: "material-mapping".into(), model, probes: Vec::new(), runs }
}


/// Transparent Neon discs like a selection ring under characters (a yellow 0.1 x 5.1 x 5.1
/// cylinder at Transparency 0.85, whose rim glows in Studio): the same disc as a cylinder and as a
/// block, at several transparencies, and in SmoothPlastic, seen from 10 and 45 degrees above.
pub fn neon_discs() -> Experiment {
	let mut model = ModelBuilder::new("NeonDiscs");
	// lying flat: the cylinder's axis (X) turned up
	let flat = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
	// (label, shape: 2 cylinder / 1 block, material, transparency)
	let cases: [(&str, u32, u32, f32); 7] = [
		("cylinder-085", 2, 288, 0.85),
		("cylinder-050", 2, 288, 0.5),
		("cylinder-095", 2, 288, 0.95),
		("cylinder-000", 2, 288, 0.0),
		("block-085", 1, 288, 0.85),
		("plastic-085", 2, 272, 0.85),
		("cylinder-085-thick", 2, 288, 0.85),
	];
	let mut runs = Vec::new();
	for (index, (label, shape, material, transparency)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 20.0, 0.0, 0.0);
		let size = if label.ends_with("thick") { Vec3::new(1.0, 5.1, 5.1) } else { Vec3::new(0.1, 5.1, 5.1) };
		model.part(&PartSpec {
			name: (*label).into(),
			cframe: glam::Affine3A::from_rotation_translation(flat, center),
			size,
			color: [255, 239, 20],
			material: *material,
			shape: Some(*shape),
			transparency: *transparency,
			cast_shadow: false,
			..Default::default()
		});
		for (view, pitch) in [("low", 10.0f32), ("high", 45.0)] {
			let direction = Vec3::new(0.0, pitch.to_radians().sin(), -pitch.to_radians().cos());
			runs.push(Run {
				label: format!("{label}-{view}"),
				environment: Environment::default(),
				camera: CameraSpec::LookAt { eye: (center + direction * 14.0).into(), target: center.into(), fov: 35.0 },
			});
		}
	}
	Experiment { name: "neon-discs".into(), model, probes: Vec::new(), runs }
}

/// How much diffuse light metals keep: Metal and Plastic slabs (white) under a low sun with the sky
/// specular at several EnvironmentSpecularScale values (no sky diffuse, no ambient), seen straight
/// down (the sky specular then mirrors the zenith).
pub fn metal_diffuse() -> Experiment {
	let mut model = ModelBuilder::new("MetalDiffuse");
	for (index, (name, material)) in [("metal", 1088u32), ("plastic", 256), ("foil", 1072), ("smooth", 272)].iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0 - 60.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(30.0, 2.0, 30.0), color: [255, 255, 255], material: *material, ..Default::default() });
	}
	let mut runs = Vec::new();
	let camera = CameraSpec::LookAt { eye: [0.0, 200.0, -0.05], target: [0.0, 0.0, 0.0], fov: 45.0 };
	for scale in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
		let environment = Environment { clock_time: 8.0, environment_specular_scale: scale, global_shadows: false, ..sun_only(2.0) };
		runs.push(Run { label: format!("specular{scale}"), environment, camera: camera.clone() });
	}
	// the flat ambient and the sky diffuse alone, with and without the sky specular
	for scale in [0.0f32, 1.0] {
		let ambient = Environment { brightness: 0.0, ambient: Vec3::splat(0.7), environment_specular_scale: scale, global_shadows: false, ..sun_only(0.0) };
		runs.push(Run { label: format!("ambient-specular{scale}"), environment: ambient, camera: camera.clone() });
		let sky = Environment { brightness: 0.0, environment_diffuse_scale: 1.0, environment_specular_scale: scale, global_shadows: false, ..sun_only(0.0) };
		runs.push(Run { label: format!("sky-specular{scale}"), environment: sky, camera: camera.clone() });
	}
	Experiment { name: "metal-diffuse".into(), model, probes: Vec::new(), runs }
}

/// What Studio reflects: white Metal and Foil balls and a black SmoothPlastic ball lit only by the
/// sky specular (no sun, ambient or sky diffuse), seen from the horizon and from above; the ball's
/// picture is the prefiltered environment times the BRDF's scale.
pub fn sky_reflection() -> Experiment {
	let mut model = ModelBuilder::new("SkyReflection");
	for (index, (name, material, color)) in [("metal", 1088u32, [255u8; 3]), ("foil", 1072, [255; 3]), ("black", 272, [0; 3]), ("white", 272, [255; 3])].iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0 - 45.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(20.0), color: *color, material: *material, shape: Some(0), ..Default::default() });
	}
	let environment = Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 1.0,
		global_shadows: false,
		..Environment::default()
	};
	let runs = vec![
		Run { label: "horizon".into(), environment: environment.clone(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -140.0], target: [0.0, 0.0, 0.0], fov: 30.0 } },
		Run { label: "above".into(), environment, camera: CameraSpec::LookAt { eye: [0.0, 100.0, -100.0], target: [0.0, 0.0, 0.0], fov: 30.0 } },
	];
	Experiment { name: "sky-reflection".into(), model, probes: Vec::new(), runs }
}


/// Per-material texture offsets: every regularly tiled material on a white 16 x 1 x 16 slab at a
/// grid-aligned position and at an odd, rotated one, seen straight down under flat ambient light.
pub fn material_offsets() -> Experiment {
	let materials: [(&str, u32); 21] = [
		("WoodPlanks", 528),
		("Marble", 784),
		("CrackedLava", 804),
		("Pavement", 836),
		("Brick", 848),
		("Pebble", 864),
		("Cobblestone", 880),
		("DiamondPlate", 1056),
		("Foil", 1072),
		("Metal", 1088),
		("Fabric", 1312),
		("Salt", 1392),
		("Glass", 1568),
		("Cardboard", 2304),
		("Carpet", 2305),
		("CeramicTiles", 2306),
		("ClayRoofTiles", 2307),
		("RoofShingles", 2308),
		("Leather", 2309),
		("Plaster", 2310),
		("Rubber", 2311),
	];
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut model = ModelBuilder::new("MaterialOffsets");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		for (variant, offset, angle) in [("a", Vec3::ZERO, 0.0f32), ("b", Vec3::new(37.3, 5.1, 11.7), 90.0)] {
			let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0) + offset;
			let rotation = glam::Quat::from_rotation_y(angle.to_radians());
			model.part(&PartSpec {
				name: format!("{name}-{variant}"),
				cframe: glam::Affine3A::from_rotation_translation(rotation, center),
				size: Vec3::new(16.0, 1.0, 16.0),
				color: [255, 255, 255],
				material: *material,
				..Default::default()
			});
			runs.push(Run {
				label: format!("{name}-{variant}"),
				environment: flat.clone(),
				camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 40.0, -0.05)).into(), target: center.into(), fov: 20.0 },
			});
		}
	}
	Experiment { name: "material-offsets".into(), model, probes: Vec::new(), runs }
}

/// Material textures on MeshParts versus Parts: Plastic, Brick and Slate (strong normal maps) on
/// Studio's head mesh as a MeshPart and on a block of the same size, under a low sun (relief) and
/// flat ambient (albedo), seen from the front.
pub fn meshpart_materials() -> Experiment {
	use rbx_dom_weak::types::{ContentId, Vector3};

	let mut model = ModelBuilder::new("MeshPartMaterials");
	let mut runs = Vec::new();
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	// ClockTime 7.5: a low sun from +X, grazing the front faces
	let low_sun = Environment { clock_time: 7.5, global_shadows: false, ..sun_only(2.0) };
	for (index, (name, material)) in [("Plastic", 256u32), ("Brick", 848), ("Slate", 800)].iter().enumerate() {
		let center = Vec3::new(index as f32 * 30.0, 0.0, 0.0);
		let mesh = model.part(&PartSpec {
			class: "MeshPart",
			name: format!("{name}-mesh"),
			cframe: glam::Affine3A::from_translation(center + Vec3::new(-6.0, 0.0, 0.0)),
			size: Vec3::splat(8.0),
			color: [200, 200, 200],
			material: *material,
			..Default::default()
		});
		model.set(mesh, "MeshId", ContentId::from("rbxasset://avatar/heads/head.mesh"));
		model.set(mesh, "InitialSize", Vector3::new(1.197849, 1.2024246, 1.197849));
		model.part(&PartSpec {
			name: format!("{name}-block"),
			cframe: glam::Affine3A::from_translation(center + Vec3::new(6.0, 0.0, 0.0)),
			size: Vec3::splat(8.0),
			color: [200, 200, 200],
			material: *material,
			..Default::default()
		});
		let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -45.0)).into(), target: center.into(), fov: 30.0 };
		runs.push(Run { label: format!("{name}-sun"), environment: low_sun.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("{name}-flat"), environment: flat.clone(), camera });
	}
	Experiment { name: "meshpart-materials".into(), model, probes: Vec::new(), runs }
}

/// Where a material's texture origin and tile cells sit (the pre-2022 materials do not start at the
/// face corner): white slabs of regular (Pebble, Pavement, CrackedLava, Wood, Fabric) and
/// cell-tiled (Cobblestone, Brick) materials at the swatch position, moved by an odd offset, smaller,
/// and turned 90 degrees, plus a cube's side faces; flat ambient light, the swatches' top-down
/// camera.
pub fn material_placement() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let materials: [(&str, u32); 7] = [("Pebble", 864), ("Pavement", 836), ("CrackedLava", 804), ("Wood", 512), ("Fabric", 1312), ("Cobblestone", 880), ("Brick", 848)];
	let placements: [(&str, Vec3, Vec3, f32); 4] = [
		("a", Vec3::ZERO, Vec3::new(36.0, 2.0, 36.0), 0.0),
		("moved", Vec3::new(3.3, 0.0, 101.7), Vec3::new(36.0, 2.0, 36.0), 0.0),
		("small", Vec3::new(0.0, 0.0, 200.0), Vec3::new(30.0, 2.0, 26.0), 0.0),
		("turned", Vec3::new(0.0, 0.0, 300.0), Vec3::new(36.0, 2.0, 36.0), 90.0),
	];
	let mut model = ModelBuilder::new("MaterialPlacement");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let base = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		for (label, offset, size, turn) in placements {
			let center = base + offset;
			let cframe = glam::Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(turn.to_radians()), center);
			model.part(&PartSpec { name: format!("{name}-{label}"), cframe, size, color: [255, 255, 255], material: *material, ..Default::default() });
			// 59 studs above the top face, as material-swatches
			let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, size.y * 0.5 + 59.0, -0.05)).into(), target: (center + Vec3::new(0.0, size.y * 0.5, 0.0)).into(), fov: 20.0 };
			runs.push(Run { label: format!("{name}-{label}"), environment: flat.clone(), camera });
		}
		// a 24-stud cube: its +X and -Z faces head-on
		let center = base + Vec3::new(0.0, 0.0, 400.0);
		model.part(&PartSpec { name: format!("{name}-cube"), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(24.0), color: [255, 255, 255], material: *material, ..Default::default() });
		for (label, direction) in [("px", Vec3::X), ("nz", Vec3::NEG_Z)] {
			let camera = CameraSpec::LookAt { eye: (center + direction * 71.0).into(), target: center.into(), fov: 20.0 };
			runs.push(Run { label: format!("{name}-cube-{label}"), environment: flat.clone(), camera });
		}
	}
	Experiment { name: "material-placement".into(), model, probes: Vec::new(), runs }
}

/// Whether a material's texture scale depends on the viewing distance: big white slabs seen straight
/// down from 8 to 128 studs (30 degree field of view), flat ambient light.
pub fn material_distance() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let materials: [(&str, u32); 6] = [("Slate", 800), ("Concrete", 816), ("CorrodedMetal", 1040), ("Granite", 832), ("Sand", 1296), ("Cobblestone", 880)];
	let mut model = ModelBuilder::new("MaterialDistance");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(150.0, 2.0, 150.0), color: [255, 255, 255], material: *material, ..Default::default() });
		for distance in [8.0f32, 16.0, 32.0, 64.0, 128.0] {
			let target = center + Vec3::Y;
			let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, distance, -0.001 * distance)).into(), target: target.into(), fov: 30.0 };
			runs.push(Run { label: format!("{name}-{distance}"), environment: flat.clone(), camera });
		}
	}
	Experiment { name: "material-distance".into(), model, probes: Vec::new(), runs }
}

/// The blend from a pre-2022 material's near texture scale to its far one: big slabs straight down
/// at 10-160 studs through a 30 degree field of view, and at 20, 40, 80 studs through 15 and 60
/// degrees (does the blend follow depth or the screen footprint?); flat ambient light, plus Slate
/// under a low sun (its normal map).
pub fn material_far() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let materials: [(&str, u32); 4] = [("Granite", 832), ("Slate", 800), ("CorrodedMetal", 1040), ("Concrete", 816)];
	let mut views: Vec<(f32, f32)> = [10.0, 14.0, 20.0, 28.0, 40.0, 56.0, 80.0, 113.0, 160.0].iter().map(|&distance| (distance, 30.0)).collect();
	for fov in [15.0, 60.0] {
		for distance in [20.0, 40.0, 80.0] {
			views.push((distance, fov));
		}
	}
	let mut model = ModelBuilder::new("MaterialFar");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 500.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(400.0, 2.0, 400.0), color: [255, 255, 255], material: *material, ..Default::default() });
		let target = center + Vec3::Y;
		for &(distance, fov) in &views {
			let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, distance, -0.001 * distance)).into(), target: target.into(), fov };
			runs.push(Run { label: format!("{name}-d{distance}-f{fov}"), environment: flat.clone(), camera: camera.clone() });
			if *name == "Slate" && fov == 30.0 && [10.0, 40.0, 160.0].contains(&distance) {
				runs.push(Run { label: format!("{name}-d{distance}-f{fov}-sun"), environment: low_sun.clone(), camera });
			}
		}
	}
	Experiment { name: "material-far".into(), model, probes: Vec::new(), runs }
}

/// Every textured material's near and far texture scales: big slabs straight down from 12, 30, 60
/// and 120 studs (30 degree field of view) under flat ambient light, and from 12 and 120 under a
/// low sun (the normal map's scale).
pub fn material_depths() -> Experiment {
	let flat = Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let materials: [(&str, u32); 39] = [
		("Plastic", 256),
		("Wood", 512),
		("WoodPlanks", 528),
		("Marble", 784),
		("Basalt", 788),
		("Slate", 800),
		("CrackedLava", 804),
		("Concrete", 816),
		("Limestone", 820),
		("Granite", 832),
		("Pavement", 836),
		("Brick", 848),
		("Pebble", 864),
		("Cobblestone", 880),
		("Rock", 896),
		("Sandstone", 912),
		("CorrodedMetal", 1040),
		("DiamondPlate", 1056),
		("Foil", 1072),
		("Metal", 1088),
		("Grass", 1280),
		("LeafyGrass", 1284),
		("Sand", 1296),
		("Fabric", 1312),
		("Snow", 1328),
		("Mud", 1344),
		("Ground", 1360),
		("Asphalt", 1376),
		("Salt", 1392),
		("Ice", 1536),
		("Glacier", 1552),
		("Glass", 1568),
		("Cardboard", 2304),
		("Carpet", 2305),
		("CeramicTiles", 2306),
		("ClayRoofTiles", 2307),
		("RoofShingles", 2308),
		("Leather", 2309),
		("Rubber", 2311),
	];
	let mut model = ModelBuilder::new("MaterialDepths");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 400.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(300.0, 2.0, 300.0), color: [255, 255, 255], material: *material, ..Default::default() });
		let target = center + Vec3::Y;
		for (distance, environment, suffix) in [(12.0f32, &flat, ""), (30.0, &flat, ""), (60.0, &flat, ""), (120.0, &flat, ""), (12.0, &low_sun, "-sun"), (120.0, &low_sun, "-sun")] {
			let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, distance, -0.001 * distance)).into(), target: target.into(), fov: 30.0 };
			runs.push(Run { label: format!("{name}-d{distance}{suffix}"), environment: environment.clone(), camera });
		}
	}
	Experiment { name: "material-depths".into(), model, probes: Vec::new(), runs }
}

/// Glass at max graphics: rows of 5-stud Glass panes (and SmoothPlastic for reference) at
/// Transparency 0 to 0.9, white, red and blue, 3 studs in front of a backdrop of coloured stripes,
/// seen head-on under flat light, under the default lighting (sun, sky light, reflections) and under
/// the sky specular alone; and CrackedLava slabs (white, black, red) in darkness (their emission
/// alone) and under flat light.
pub fn material_glass() -> Experiment {
	let transparencies = [0.0f32, 0.3, 0.5, 0.7, 0.9];
	let rows: [(&str, u32, [u8; 3]); 4] = [("glass-white", 1568, [255, 255, 255]), ("glass-red", 1568, [255, 0, 0]), ("glass-blue", 1568, [13, 105, 172]), ("plastic-white", 272, [255, 255, 255])];
	let stripes: [[u8; 3]; 6] = [[0, 0, 0], [255, 255, 255], [200, 30, 30], [30, 160, 60], [30, 60, 200], [128, 128, 128]];
	let flat = flat_ambient();
	let sky_only = Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 1.0, global_shadows: false, ..Environment::default() };
	let mut model = ModelBuilder::new("MaterialGlass");
	let mut runs = Vec::new();
	for (row, (label, material, color)) in rows.iter().enumerate() {
		let base = Vec3::new(0.0, row as f32 * 100.0, 0.0);
		// the backdrop: vertical stripes 1 stud wide across the row, 3 studs behind the panes
		for stripe in 0..40 {
			let x = stripe as f32 - 19.5;
			model.part(&PartSpec {
				name: format!("{label}-stripe{stripe}"),
				cframe: glam::Affine3A::from_translation(base + Vec3::new(x, 0.0, 3.5)),
				size: Vec3::new(1.0, 12.0, 1.0),
				color: stripes[stripe % stripes.len()],
				material: 272,
				..Default::default()
			});
		}
		for (index, transparency) in transparencies.iter().enumerate() {
			let x = (index as f32 - 2.0) * 7.0;
			model.part(&PartSpec {
				name: format!("{label}-t{transparency}"),
				cframe: glam::Affine3A::from_translation(base + Vec3::new(x, 0.0, 0.0)),
				size: Vec3::new(5.0, 5.0, 0.5),
				color: *color,
				material: *material,
				transparency: *transparency,
				..Default::default()
			});
		}
		let camera = CameraSpec::LookAt { eye: (base + Vec3::new(0.0, 0.0, -80.0)).into(), target: base.into(), fov: 28.0 };
		runs.push(Run { label: format!("{label}-flat"), environment: flat.clone(), camera: camera.clone() });
		runs.push(Run { label: format!("{label}-default"), environment: Environment::default(), camera: camera.clone() });
		runs.push(Run { label: format!("{label}-sky"), environment: sky_only.clone(), camera });
	}
	let dark = Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, global_shadows: false, ..Environment::default() };
	for (index, (label, color)) in [("white", [255u8, 255, 255]), ("black", [0, 0, 0]), ("red", [255, 0, 0])].iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 1000.0, 0.0);
		model.part(&PartSpec { name: format!("CrackedLava-{label}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: *color, material: 804, ..Default::default() });
		runs.push(Run { label: format!("CrackedLava-{label}-dark"), environment: dark.clone(), camera: top_view(center + Vec3::Y) });
		runs.push(Run { label: format!("CrackedLava-{label}-flat"), environment: flat.clone(), camera: top_view(center + Vec3::Y) });
	}
	Experiment { name: "material-glass".into(), model, probes: Vec::new(), runs }
}

/// How the part colour applies to built-in colour maps with an alpha channel (Brick's mortar,
/// CrackedLava's cracks, CorrodedMetal's rust, ...; every 2022 pack says `alphamode` 2) and two
/// without (Concrete, Fabric): black, gray and blue slabs seen straight down under flat light like
/// material-swatches (whose slabs are white), and Brick's colour map as a MaterialVariant's on the
/// same colours.
pub fn material_tint() -> Experiment {
	let materials: [(&str, u32); 16] = [
		("Brick", 848),
		("Carpet", 2305),
		("CeramicTiles", 2306),
		("WoodPlanks", 528),
		("CrackedLava", 804),
		("CorrodedMetal", 1040),
		("Pavement", 836),
		("Pebble", 864),
		("LeafyGrass", 1284),
		("Grass", 1280),
		("Asphalt", 1376),
		("Basalt", 788),
		("Sand", 1296),
		("Marble", 784),
		("Concrete", 816),
		("Fabric", 1312),
	];
	let colors: [(&str, [u8; 3]); 3] = [("black", [0, 0, 0]), ("gray", [128, 128, 128]), ("blue", [13, 105, 172])];
	let mut model = ModelBuilder::new("MaterialTint");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		for (row, (color_name, color)) in colors.iter().enumerate() {
			let center = Vec3::new(index as f32 * 100.0, 0.0, row as f32 * 100.0);
			model.part(&PartSpec { name: format!("{name}-{color_name}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: *color, material: *material, ..Default::default() });
			runs.push(Run { label: format!("{name}-{color_name}"), environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
		}
	}
	// Brick's colour map (alpha in the mortar) as a Regular MaterialVariant's
	variant(&mut model, "RorOrganicTint", &[("ColorMap", "rbxassetid://105907702570155")], 8.0, false);
	for (row, (color_name, color)) in colors.iter().enumerate() {
		let center = Vec3::new(materials.len() as f32 * 100.0, 0.0, row as f32 * 100.0);
		let part = model.part(&PartSpec { name: format!("Variant-{color_name}"), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: *color, material: 256, ..Default::default() });
		model.set(part, "MaterialVariantSerialized", "RorOrganicTint".to_string());
		runs.push(Run { label: format!("Variant-{color_name}"), environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
	}
	with_setup(&mut model, VARIANT_SETUP);
	Experiment { name: "material-tint".into(), model, probes: Vec::new(), runs }
}

/// Built-in material textures from close up to far away: white 120-stud slabs seen straight down
/// from 6 to 96 studs above (30 degree field of view: 347 to 22 px per stud) under flat light (the
/// colour maps: their sharpness and contrast with distance; Granite's far tiling, Sand) and under a
/// low sun from +X (the normal maps' relief).
pub fn material_zoom() -> Experiment {
	let colour: [(&str, u32); 12] = [
		("Brick", 848),
		("Carpet", 2305),
		("CeramicTiles", 2306),
		("Cardboard", 2304),
		("Rubber", 2311),
		("RoofShingles", 2308),
		("Plaster", 2310),
		("Fabric", 1312),
		("WoodPlanks", 528),
		("Metal", 1088),
		("Granite", 832),
		("Sand", 1296),
	];
	let relief: [(&str, u32); 6] = [("Plastic", 256), ("Foil", 1072), ("DiamondPlate", 1056), ("Brick", 848), ("Fabric", 1312), ("Metal", 1088)];
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let distances = [6.0f32, 12.0, 17.0, 24.0, 34.0, 48.0, 96.0];
	let mut model = ModelBuilder::new("MaterialZoom");
	let mut runs = Vec::new();
	let mut slabs: Vec<(&str, u32)> = colour.to_vec();
	for entry in relief {
		if !slabs.contains(&entry) {
			slabs.push(entry);
		}
	}
	for (index, (name, material)) in slabs.iter().enumerate() {
		let center = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(120.0, 2.0, 120.0), color: [255, 255, 255], material: *material, ..Default::default() });
		let target = center + Vec3::Y;
		for distance in distances {
			let camera = CameraSpec::LookAt { eye: (target + Vec3::new(0.0, distance, -0.001 * distance)).into(), target: target.into(), fov: 30.0 };
			if colour.iter().any(|(colour_name, _)| colour_name == name) {
				runs.push(Run { label: format!("{name}-d{distance}"), environment: flat_ambient(), camera: camera.clone() });
			}
			if relief.iter().any(|(relief_name, _)| relief_name == name) {
				runs.push(Run { label: format!("{name}-d{distance}-sun"), environment: low_sun.clone(), camera });
			}
		}
	}
	Experiment { name: "material-zoom".into(), model, probes: Vec::new(), runs }
}

/// Where the texture of Foil and DiamondPlate (normal maps only), Leather, Plaster and Fabric starts
/// on a part: white 16-stud slabs at a grid position, moved, wider, turned 90 degrees and raised,
/// seen straight down from 30 studs, under a low sun (Foil, DiamondPlate, Fabric) and flat light
/// (Leather, Plaster, Fabric). Run twice (`ROR_RECAPTURE=1`): an offset drawn at random per part
/// changes between loads.
pub fn material_phase() -> Experiment {
	let materials: [(&str, u32, bool, bool); 5] = [("Foil", 1072, false, true), ("DiamondPlate", 1056, false, true), ("Leather", 2309, true, false), ("Plaster", 2310, true, false), ("Fabric", 1312, true, true)];
	let placements: [(&str, Vec3, Vec3, f32); 5] = [
		("a", Vec3::ZERO, Vec3::new(16.0, 2.0, 16.0), 0.0),
		("moved", Vec3::new(3.3, 0.0, 45.7), Vec3::new(16.0, 2.0, 16.0), 0.0),
		("wide", Vec3::new(0.0, 0.0, 90.0), Vec3::new(24.0, 2.0, 12.0), 0.0),
		("turned", Vec3::new(0.0, 0.0, 135.0), Vec3::new(16.0, 2.0, 16.0), 90.0),
		("raised", Vec3::new(0.0, 7.3, 180.0), Vec3::new(16.0, 2.0, 16.0), 0.0),
	];
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let mut model = ModelBuilder::new("MaterialPhase");
	let mut runs = Vec::new();
	for (index, (name, material, flat, sun)) in materials.iter().enumerate() {
		for (label, offset, size, turn) in placements {
			let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0) + offset;
			let cframe = glam::Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(turn.to_radians()), center);
			model.part(&PartSpec { name: format!("{name}-{label}"), cframe, size, color: [255, 255, 255], material: *material, ..Default::default() });
			let top = center + Vec3::Y * size.y * 0.5;
			let camera = CameraSpec::LookAt { eye: (top + Vec3::new(0.0, 30.0, -0.05)).into(), target: top.into(), fov: 20.0 };
			if *flat {
				runs.push(Run { label: format!("{name}-{label}"), environment: flat_ambient(), camera: camera.clone() });
			}
			if *sun {
				runs.push(Run { label: format!("{name}-{label}-sun"), environment: low_sun.clone(), camera });
			}
		}
	}
	Experiment { name: "material-phase".into(), model, probes: Vec::new(), runs }
}

/// ForceField fixtures stand in front of two unlit backdrops: black Neon on the screen's right
/// (world -X, seen from -Z) and gray-100 Neon on the left (x = 25 · (100/255)⁴ = 0.59).
fn forcefield_backdrops(model: &mut ModelBuilder, center: Vec3, width: f32, height: f32) {
	for (side, gray) in [(-1.0f32, 0u8), (1.0, 100)] {
		model.part(&PartSpec {
			name: format!("Backdrop{gray}"),
			cframe: glam::Affine3A::from_translation(center + Vec3::new(side * width * 0.25, 0.0, 8.0)),
			size: Vec3::new(width * 0.5, height, 1.0),
			color: [gray; 3],
			material: 288,
			cast_shadow: false,
			..Default::default()
		});
	}
}

/// Lit like a default place (sun, flat ambient, sky light), without shadows.
fn forcefield_lit() -> Environment {
	Environment {
		brightness: 3.0,
		ambient: Vec3::splat(0.5),
		outdoor_ambient: Vec3::splat(0.5),
		environment_diffuse_scale: 1.0,
		environment_specular_scale: 1.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// ForceField parts without textures over the black and the gray backdrop: balls and blocks of six
/// colours, transparency sweeps (negative too) on white balls and on a ship model's green, dark at
/// three exposures, lit (is it unlit?), from the front and from above; and big balls up close for
/// the rim's angular profile.
pub fn forcefield_basics() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldBasics");
	let mut probes = Vec::new();
	forcefield_backdrops(&mut model, Vec3::ZERO, 80.0, 60.0);
	let colors: [(&str, [u8; 3]); 6] = [("white", [255, 255, 255]), ("gray", [128, 128, 128]), ("red", [255, 0, 0]), ("green", [0, 255, 0]), ("blue", [0, 0, 255]), ("sage", [140, 177, 121])];
	let transparencies = [-2.0f32, -1.0, -0.5, 0.0, 0.25, 0.5, 0.75, 0.9];
	let add = |model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, center: Vec3, ball: bool, size: f32, color: [u8; 3], transparency: f32, backdrop: u8| {
		model.part(&PartSpec {
			name: label.clone(),
			cframe: glam::Affine3A::from_translation(center),
			size: Vec3::splat(size),
			color,
			material: 1584,
			shape: Some(if ball { 0 } else { 1 }),
			transparency,
			cast_shadow: false,
			..Default::default()
		});
		probes.push(Probe {
			label,
			position: (center - Vec3::Z * size * 0.5).into(),
			inputs: serde_json::json!({"color": color, "transparency": transparency, "shape": if ball { "ball" } else { "block" }, "backdrop": backdrop, "size": size, "center": center.to_array()}),
		});
	};
	for (side, backdrop) in [(-1.0f32, 0u8), (1.0, 100)] {
		let name = if backdrop == 0 { "black" } else { "gray" };
		for (index, (color_name, color)) in colors.iter().enumerate() {
			let x = side * (3.0 + index as f32 * 5.0);
			add(&mut model, &mut probes, format!("ball_{color_name}_{name}"), Vec3::new(x, 15.0, 0.0), true, 3.5, *color, 0.0, backdrop);
			add(&mut model, &mut probes, format!("block_{color_name}_{name}"), Vec3::new(x, 9.0, 0.0), false, 3.0, *color, 0.0, backdrop);
		}
		for (index, transparency) in transparencies.iter().enumerate() {
			let x = side * (3.0 + index as f32 * 4.4);
			add(&mut model, &mut probes, format!("ball_white_t{transparency}_{name}"), Vec3::new(x, 2.5, 0.0), true, 3.5, [255; 3], *transparency, backdrop);
			add(&mut model, &mut probes, format!("block_sage_t{transparency}_{name}"), Vec3::new(x, -3.5, 0.0), false, 3.0, [140, 177, 121], *transparency, backdrop);
			add(&mut model, &mut probes, format!("ball_sage_t{transparency}_{name}"), Vec3::new(x, -9.5, 0.0), true, 3.5, [140, 177, 121], *transparency, backdrop);
		}
	}
	// big balls for the rim profile, out of the main view
	let rim = Vec3::new(300.0, 0.0, 0.0);
	forcefield_backdrops(&mut model, rim, 80.0, 40.0);
	for (side, backdrop) in [(-1.0f32, 0u8), (1.0, 100)] {
		let name = if backdrop == 0 { "black" } else { "gray" };
		for (index, (label, color, transparency)) in [("white", [255u8; 3], 0.0f32), ("sage", [140, 177, 121], 0.0), ("white-t-1", [255; 3], -1.0)].iter().enumerate() {
			let center = rim + Vec3::new(side * (7.0 + index as f32 * 11.5), 0.0, 0.0);
			add(&mut model, &mut probes, format!("rim_{label}_{name}"), center, true, 10.0, *color, *transparency, backdrop);
		}
	}
	let front = CameraSpec::LookAt { eye: [0.0, 2.75, -72.0], target: [0.0, 2.75, 0.0], fov: 40.0 };
	let elevation = 40f32.to_radians();
	let above = CameraSpec::LookAt { eye: [0.0, 2.75 + 72.0 * elevation.sin(), -72.0 * elevation.cos()], target: [0.0, 2.75, 0.0], fov: 40.0 };
	let close = CameraSpec::LookAt { eye: [rim.x, 0.0, -62.0], target: rim.into(), fov: 40.0 };
	let runs = vec![
		Run { label: "dark".into(), environment: dark(), camera: front.clone() },
		Run { label: "dark-ev2".into(), environment: Environment { exposure_compensation: 2.0, ..dark() }, camera: front.clone() },
		Run { label: "dark-ev-2".into(), environment: Environment { exposure_compensation: -2.0, ..dark() }, camera: front.clone() },
		Run { label: "lit".into(), environment: forcefield_lit(), camera: front },
		Run { label: "above".into(), environment: dark(), camera: above },
		Run { label: "rim".into(), environment: dark(), camera: close.clone() },
		Run { label: "rim-ev2".into(), environment: Environment { exposure_compensation: 2.0, ..dark() }, camera: close.clone() },
		Run { label: "rim-lit".into(), environment: forcefield_lit(), camera: close },
	];
	Experiment { name: "forcefield-basics".into(), model, probes, runs }
}

/// Identical ForceField parts (a ship model's green at Transparency 0, balls and blocks) on a grid
/// over the black backdrop, plus a big slab: does the material vary between parts, across a
/// surface, with the screen position (a shifted camera) or over time (bursts)?
pub fn forcefield_pattern() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldPattern");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 8.0)),
		size: Vec3::new(120.0, 80.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		cast_shadow: false,
		..Default::default()
	});
	for row in 0..5 {
		for column in 0..5 {
			for (ball, x0) in [(true, -26.0f32), (false, 4.0)] {
				let center = Vec3::new(x0 + column as f32 * 5.5, 11.0 - row as f32 * 5.5, 0.0);
				let label = format!("{}_{row}_{column}", if ball { "ball" } else { "block" });
				model.part(&PartSpec {
					name: label.clone(),
					cframe: glam::Affine3A::from_translation(center),
					size: Vec3::splat(3.5),
					color: [140, 177, 121],
					material: 1584,
					shape: Some(if ball { 0 } else { 1 }),
					cast_shadow: false,
					..Default::default()
				});
				probes.push(Probe { label, position: (center - Vec3::Z * 1.75).into(), inputs: serde_json::json!({"center": center.to_array(), "size": 3.5, "ball": ball}) });
			}
		}
	}
	model.part(&PartSpec {
		name: "Slab".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -22.0, 0.0)),
		size: Vec3::new(56.0, 14.0, 1.0),
		color: [140, 177, 121],
		material: 1584,
		cast_shadow: false,
		..Default::default()
	});
	let view = |dx: f32| CameraSpec::LookAt { eye: [dx, -2.0, -80.0], target: [dx, -2.0, 0.0], fov: 40.0 };
	let bright = Environment { exposure_compensation: 2.0, ..dark() };
	let runs = vec![
		Run { label: "front".into(), environment: bright.clone(), camera: view(0.0) },
		Run { label: "shifted".into(), environment: bright.clone(), camera: view(7.0) },
		Run { label: "near".into(), environment: bright, camera: CameraSpec::LookAt { eye: [0.0, -2.0, -50.0], target: [0.0, -2.0, 0.0], fov: 65.0 } },
	];
	Experiment { name: "forcefield-pattern".into(), model, probes, runs }
}

/// Studio's flat quad (1024 x 1024 in the XY plane, normal +Z, uv 0..1 with v down) and smooth
/// sphere: meshes whose uv layout is known.
const QUAD_MESH: (&str, [f32; 3]) = ("rbxasset://avatar/compositing/CompositQuad.mesh", [1024.0, 1024.0, 0.000_166_830_75]);



/// Places a CompositQuad part so that Studio, which draws the mesh with its origin (a corner) at the
/// part position, shows the quad centred on `center`, turned by `yaw` degrees from facing -Z.
fn quad_cframe(center: Vec3, size: f32, yaw: f32) -> glam::Affine3A {
	let rotation = glam::Quat::from_rotation_y(std::f32::consts::PI + yaw.to_radians());
	glam::Affine3A::from_rotation_translation(rotation, center - rotation * Vec3::new(size * 0.5, size * 0.5, 0.0))
}


/// How a single ForceField layer blends: long flat quads (white, a ship model's green and red, at
/// several transparencies) across six backdrop stripes of known radiance (SmoothPlastic grays
/// under a flat ambient of 1, x = 1.98 · c², and a Neon stripe, x = 25 · 0.6⁴), head-on.
pub fn forcefield_blend() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldBlend");
	let mut probes = Vec::new();
	let stripes: [(u8, u32); 6] = [(0, 288), (64, 272), (128, 272), (191, 272), (255, 272), (153, 288)];
	for (index, (gray, material)) in stripes.iter().enumerate() {
		let x = 15.0 - index as f32 * 6.0;
		model.part(&PartSpec {
			name: format!("Stripe{index}"),
			cframe: glam::Affine3A::from_translation(Vec3::new(x, 0.0, 8.0)),
			size: Vec3::new(6.0, 40.0, 1.0),
			color: [*gray; 3],
			material: *material,
			cast_shadow: false,
			..Default::default()
		});
	}
	let rows: [(&str, [u8; 3], f32); 7] = [
		("white", [255; 3], 0.0),
		("white-t-0.5", [255; 3], -0.5),
		("white-t0.5", [255; 3], 0.5),
		("sage", [140, 177, 121], 0.0),
		("sage-t-1", [140, 177, 121], -1.0),
		("red", [255, 0, 0], 0.0),
		("white-t-2", [255; 3], -2.0),
	];
	for (row, (name, color, transparency)) in rows.iter().enumerate() {
		let center = Vec3::new(0.0, 12.0 - row as f32 * 4.0, 0.0);
		let rotation = glam::Quat::from_rotation_y(std::f32::consts::PI);
		let size = Vec3::new(36.0, 2.5, 0.05);
		let cframe = glam::Affine3A::from_rotation_translation(rotation, center - rotation * Vec3::new(size.x * 0.5, size.y * 0.5, 0.0));
		let part = model.part(&PartSpec { class: "MeshPart", name: (*name).into(), cframe, size, color: *color, material: 1584, transparency: *transparency, cast_shadow: false, ..Default::default() });
		model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from(QUAD_MESH.0));
		model.set(part, "InitialSize", rbx_dom_weak::types::Vector3::new(QUAD_MESH.1[0], QUAD_MESH.1[1], QUAD_MESH.1[2]));
		for (index, (gray, material)) in stripes.iter().enumerate() {
			// on the quads' plane, in line (from the camera 60 studs away) with the stripe's centre
			// on the backdrop's face 67.5 studs away
			let x = (15.0 - index as f32 * 6.0) * 60.0 / 67.5;
			let background = if *material == 288 { 25.0 * (*gray as f32 / 255.0).powi(4) } else { 1.98 * (*gray as f32 / 255.0).powi(2) };
			probes.push(Probe {
				label: format!("{name}_{index}"),
				position: [x, center.y, 0.0],
				inputs: serde_json::json!({"color": color, "transparency": transparency, "background": background, "stripe": index}),
			});
			probes.push(Probe { label: format!("bg{row}_{index}"), position: [x, center.y + 2.0, 0.0], inputs: serde_json::json!({"background": background, "stripe": index}) });
		}
	}
	let flat = Environment { ambient: Vec3::ONE, ..dark() };
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -60.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	// the dark end (black and darkest stripes) is read at +2 and +3 EV, the bright end at 0 and -1
	let runs = [0.0f32, -1.0, 2.0, 3.0]
		.iter()
		.map(|&exposure| Run { label: format!("flat-ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..flat.clone() }, camera: camera.clone() })
		.collect();
	Experiment { name: "forcefield-blend".into(), model, probes, runs }
}

/// The rim of a single ForceField layer: white quads turned so that the camera sees them at n·v
/// 0.03-0.5 (each turned about its own vertical through its centre), at Transparency 0, 0.5 and
/// -1, over the black backdrop.
pub fn forcefield_angles() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldAngles");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)),
		size: Vec3::new(120.0, 80.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		cast_shadow: false,
		..Default::default()
	});
	let eye = Vec3::new(0.0, 0.0, -55.0);
	let targets = [0.03f32, 0.06, 0.09, 0.12, 0.15, 0.18, 0.22, 0.26, 0.3, 0.35, 0.4, 0.5];
	for (block, transparency) in [0.0f32, 0.5, -1.0].iter().enumerate() {
		for (index, target) in targets.iter().enumerate() {
			let column = (index % 6) as f32;
			let row = (block * 2 + index / 6) as f32;
			let center = Vec3::new(13.75 - column * 5.5, 13.75 - row * 5.5, 0.0);
			// the quad's normal is (-sin yaw, 0, -cos yaw); turn it so that n · (eye - center) = target
			let d = (eye - center).normalize();
			let horizontal = (d.x * d.x + d.z * d.z).sqrt();
			let phi = d.x.atan2(d.z);
			let yaw = phi + (-target / horizontal).clamp(-1.0, 1.0).acos();
			let label = format!("t{transparency}_{target}");
			let part = model.part(&PartSpec { class: "MeshPart", name: label.clone(), cframe: quad_cframe(center, 4.0, yaw.to_degrees()), size: Vec3::new(4.0, 4.0, 0.05), color: [255; 3], material: 1584, transparency: *transparency, cast_shadow: false, ..Default::default() });
			model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from(QUAD_MESH.0));
			model.set(part, "InitialSize", rbx_dom_weak::types::Vector3::new(QUAD_MESH.1[0], QUAD_MESH.1[1], QUAD_MESH.1[2]));
			probes.push(Probe { label, position: center.into(), inputs: serde_json::json!({"center": center.to_array(), "yaw": yaw.to_degrees(), "target": target, "transparency": transparency}) });
		}
	}
	let camera = CameraSpec::LookAt { eye: eye.into(), target: [0.0, 0.0, 0.0], fov: 40.0 };
	let runs = [0.0f32, -1.0, -2.0].iter().map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera.clone() }).collect();
	Experiment { name: "forcefield-angles".into(), model, probes, runs }
}

/// Do ForceField parts cast sun shadows? ForceField blocks at Transparency -1, 0, 0.5 and 0.9 and
/// a SmoothPlastic block for reference above a white floor, sun on.
pub fn forcefield_shadow() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldShadow");
	model.part(&PartSpec {
		name: "Floor".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)),
		size: Vec3::new(70.0, 1.0, 40.0),
		color: [255, 255, 255],
		material: 272,
		..Default::default()
	});
	for (index, (material, transparency)) in [(272u32, 0.0f32), (1584, 0.0), (1584, -1.0), (1584, 0.5), (1584, 0.9)].iter().enumerate() {
		model.part(&PartSpec {
			name: format!("Block{index}"),
			cframe: glam::Affine3A::from_translation(Vec3::new(-20.0 + index as f32 * 10.0, 5.0, 0.0)),
			size: Vec3::new(4.0, 4.0, 4.0),
			color: [140, 177, 121],
			material: *material,
			transparency: *transparency,
			..Default::default()
		});
	}
	let lit = Environment { brightness: 3.0, ambient: Vec3::splat(0.3), outdoor_ambient: Vec3::splat(0.3), environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, global_shadows: true, ..Environment::default() };
	let runs = vec![Run { label: "sun".into(), environment: lit, camera: CameraSpec::LookAt { eye: [0.0, 40.0, -45.0], target: [0.0, 0.0, 0.0], fov: 40.0 } }];
	Experiment { name: "forcefield-shadow".into(), model, probes: Vec::new(), runs }
}

/// Does ForceField react to the surface behind it? Flat quads tilted away from a backdrop: each
/// touches it along its bottom edge and stands 1 stud off at the top, so one image covers gaps of
/// 0-1 stud (black and gray Neon backdrops; white and a ship model's green at Transparency 0, 0.5
/// and -0.5), head-on and (for the gap's direction) from 45° to the side.
pub fn forcefield_depth() -> Experiment {
	let rise = 1.0f32;
	let mut model = ModelBuilder::new("ForceFieldDepth");
	let mut probes = Vec::new();
	for (side, gray) in [(-1.0f32, 0u8), (1.0, 100)] {
		model.part(&PartSpec {
			name: format!("Backdrop{gray}"),
			cframe: glam::Affine3A::from_translation(Vec3::new(side * 22.0, 0.0, 0.5)),
			size: Vec3::new(44.0, 30.0, 1.0),
			color: [gray; 3],
			material: 288,
			cast_shadow: false,
			..Default::default()
		});
	}
	let (width, length) = (7.0f32, 9.0f32);
	let tilt = (rise / length).asin();
	let rotation = glam::Quat::from_rotation_x(-tilt) * glam::Quat::from_rotation_y(std::f32::consts::PI);
	let rows: [(&str, [u8; 3], f32); 4] = [("white", [255; 3], 0.0), ("sage", [140, 177, 121], 0.0), ("sage-t-0.5", [140, 177, 121], -0.5), ("white-t0.5", [255; 3], 0.5)];
	for (side, backdrop) in [(-1.0f32, 0u8), (1.0, 100)] {
		for (index, (name, color, transparency)) in rows.iter().enumerate() {
			let x = side * (5.0 + index as f32 * 9.5);
			// the quad's corner (mesh origin) at the bottom edge, just off the backdrop's face (z = 0)
			let corner = Vec3::new(x + width * 0.5, -4.0, -0.02);
			let label = format!("{name}_{}", if backdrop == 0 { "black" } else { "gray" });
			let part = model.part(&PartSpec {
				class: "MeshPart",
				name: label.clone(),
				cframe: glam::Affine3A::from_rotation_translation(rotation, corner),
				size: Vec3::new(width, length, 0.05),
				color: *color,
				material: 1584,
				transparency: *transparency,
				cast_shadow: false,
				..Default::default()
			});
			model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from(QUAD_MESH.0));
			model.set(part, "InitialSize", rbx_dom_weak::types::Vector3::new(QUAD_MESH.1[0], QUAD_MESH.1[1], QUAD_MESH.1[2]));
			probes.push(Probe { label, position: corner.into(), inputs: serde_json::json!({"corner": corner.to_array(), "width": width, "length": length, "tilt": tilt, "color": color, "transparency": transparency, "backdrop": backdrop}) });
		}
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -75.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let mut runs: Vec<Run> = [0.0f32, -1.0].iter().map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera.clone() }).collect();
	// 45° to the side of the gray backdrop's quads, looking at their middle
	let target = Vec3::new(19.25, 0.0, 0.0);
	let eye = target + Vec3::new(-1.0, 0.0, -1.0).normalize() * 40.0;
	runs.push(Run { label: "side".into(), environment: dark(), camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 40.0 } });
	Experiment { name: "forcefield-depth".into(), model, probes, runs }
}

/// Rim and contact glow together: small white quads standing steeply off a black backdrop (seen
/// at n·v 0.2-0.35 near the contact line), up close: do the two glows add or does the larger win?
pub fn forcefield_contact() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldContact");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 0.5)),
		size: Vec3::new(60.0, 40.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		cast_shadow: false,
		..Default::default()
	});
	let (width, length) = (2.5f32, 3.0f32);
	for (index, (degrees, transparency)) in [(70.0f32, 0.0f32), (76.0, 0.0), (70.0, 0.5), (76.0, 0.5)].iter().enumerate() {
		let tilt = degrees.to_radians();
		let rotation = glam::Quat::from_rotation_x(-tilt) * glam::Quat::from_rotation_y(std::f32::consts::PI);
		let x = 4.5 - index as f32 * 3.0;
		let corner = Vec3::new(x + width * 0.5, -0.6, -0.02);
		let label = format!("tilt{degrees}_t{transparency}");
		let part = model.part(&PartSpec { class: "MeshPart", name: label.clone(), cframe: glam::Affine3A::from_rotation_translation(rotation, corner), size: Vec3::new(width, length, 0.05), color: [255; 3], material: 1584, transparency: *transparency, cast_shadow: false, ..Default::default() });
		model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from(QUAD_MESH.0));
		model.set(part, "InitialSize", rbx_dom_weak::types::Vector3::new(QUAD_MESH.1[0], QUAD_MESH.1[1], QUAD_MESH.1[2]));
		probes.push(Probe { label, position: corner.into(), inputs: serde_json::json!({"corner": corner.to_array(), "width": width, "length": length, "tilt": tilt, "color": [255, 255, 255], "transparency": transparency, "backdrop": 0}) });
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -14.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let runs = [0.0f32, -1.0].iter().map(|&exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: exposure, ..dark() }, camera: camera.clone() }).collect();
	Experiment { name: "forcefield-contact".into(), model, probes, runs }
}

/// A ship model's ForceField over Neon: its green quads at Transparency -0.5 parallel to a Neon panel
/// at gaps of 0.03-3 studs (and at 0.5 at 0.03 and 1), head-on, in the dark; the panel black, gray 153
/// and that green (140, 177, 121), each its own run. Does the contact glow hold over a bright
/// panel, and does the quad blend over it as over a dark one?
pub fn forcefield_neon() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldNeon");
	let green = [140u8, 177, 121];
	let panels: [(&str, [u8; 3]); 3] = [("black", [0; 3]), ("gray", [153; 3]), ("green", green)];
	let quads: [(f32, f32); 8] = [(0.03, -0.5), (0.1, -0.5), (0.2, -0.5), (0.3, -0.5), (1.0, -0.5), (3.0, -0.5), (0.03, 0.5), (1.0, 0.5)];
	let mut runs = Vec::new();
	for (row, (label, color)) in panels.iter().enumerate() {
		let center = Vec3::new(0.0, row as f32 * 40.0, 0.0);
		model.part(&PartSpec { name: format!("{label}-panel"), cframe: glam::Affine3A::from_translation(center + Vec3::new(0.0, 0.0, 0.5)), size: Vec3::new(26.0, 6.0, 1.0), color: *color, material: 288, cast_shadow: false, ..Default::default() });
		for (index, (gap, transparency)) in quads.iter().enumerate() {
			let at = center + Vec3::new(-9.1 + index as f32 * 2.6, 0.0, -gap);
			let part = model.part(&PartSpec { class: "MeshPart", name: format!("{label}-gap{gap}-t{transparency}"), cframe: quad_cframe(at, 2.0, 0.0), size: Vec3::new(2.0, 2.0, 0.001), color: green, material: 1584, transparency: *transparency, cast_shadow: false, ..Default::default() });
			model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from(QUAD_MESH.0));
			model.set(part, "InitialSize", rbx_dom_weak::types::Vector3::new(QUAD_MESH.1[0], QUAD_MESH.1[1], QUAD_MESH.1[2]));
		}
		runs.push(Run { label: (*label).into(), environment: dark(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -22.0)).into(), target: center.into(), fov: 40.0 } });
	}
	Experiment { name: "forcefield-neon".into(), model, probes: Vec::new(), runs }
}


/// Small identical ForceField balls stepped half a stud apart along X at several heights and
/// depths, and along Y: what decides whether a closed part shows one, two or three layers
/// (forcefield-geometry)?
pub fn forcefield_levels() -> Experiment {
	let mut model = ModelBuilder::new("ForceFieldLevels");
	let mut probes = Vec::new();
	model.part(&PartSpec {
		name: "Backdrop".into(),
		cframe: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 12.0)),
		size: Vec3::new(120.0, 80.0, 1.0),
		color: [0, 0, 0],
		material: 288,
		cast_shadow: false,
		..Default::default()
	});
	let ball = |model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: String, center: Vec3| {
		model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(0.4), color: [255; 3], material: 1584, shape: Some(0), cast_shadow: false, ..Default::default() });
		probes.push(Probe { label, position: (center - Vec3::Z * 0.2).into(), inputs: serde_json::json!({"center": center.to_array()}) });
	};
	for (row, (y, z)) in [(3.0f32, 0.0f32), (2.0, 0.0), (1.0, 2.0), (0.0, -2.0), (-1.0, 0.3)].iter().enumerate() {
		for step in 0..25 {
			let x = -6.0 + step as f32 * 0.5;
			ball(&mut model, &mut probes, format!("row{row}_{step}"), Vec3::new(x, *y, *z));
		}
	}
	for step in 0..17 {
		ball(&mut model, &mut probes, format!("column_{step}"), Vec3::new(-7.5, -4.0 + step as f32 * 0.5, 0.0));
	}
	let runs = vec![Run {
		label: "ev2".into(),
		environment: Environment { exposure_compensation: 2.0, ..dark() },
		camera: CameraSpec::LookAt { eye: [-0.75, 0.0, -24.0], target: [-0.75, 0.0, 0.0], fov: 40.0 },
	}];
	Experiment { name: "forcefield-levels".into(), model, probes, runs }
}


/// Flat ambient light alone (Ambient 0.7): material colour maps show their albedo.
fn flat_ambient() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// Straight down onto a top face from 59 studs (20° field of view: 53.64 px per stud at a 1116 px
/// tall viewport, +Z up and -X right in the image, like material-swatches).
fn top_view(top_center: Vec3) -> CameraSpec {
	CameraSpec::LookAt { eye: (top_center + Vec3::new(0.0, 59.0, -0.05)).into(), target: top_center.into(), fov: 20.0 }
}

/// Organic pattern anchoring: identical white CorrodedMetal slabs (36 x 2 x 36) 1024 studs apart,
/// some shifted by a stud or less, raised, turned, widened, and two slabs side by side, each seen
/// straight down under flat light. Do the pattern's regions and offsets follow the world or the part?
pub fn organic_anchor() -> Experiment {
	let mut model = ModelBuilder::new("OrganicAnchor");
	let mut runs = Vec::new();
	let slab = Vec3::new(36.0, 2.0, 36.0);
	let cases: [(&str, Vec3, f32, Vec3); 8] = [
		("ref", Vec3::ZERO, 0.0, slab),
		("x1024", Vec3::new(1024.0, 0.0, 0.0), 0.0, slab),
		("x2049", Vec3::new(2049.0, 0.0, 0.0), 0.0, slab),
		("x3072.5-z2", Vec3::new(3072.5, 0.0, 2.0), 0.0, slab),
		("x4096-y4", Vec3::new(4096.0, 4.0, 0.0), 0.0, slab),
		("x5120-yaw90", Vec3::new(5120.0, 0.0, 0.0), 90.0, slab),
		("x6144-wide", Vec3::new(6144.0, 0.0, 0.0), 0.0, Vec3::new(40.0, 2.0, 36.0)),
		("x8192-z1024", Vec3::new(8192.0, 0.0, 1024.0), 0.0, slab),
	];
	for (label, center, yaw, size) in cases {
		let cframe = glam::Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(yaw.to_radians()), center);
		model.part(&PartSpec { name: label.into(), cframe, size, color: [255, 255, 255], material: 1040, ..Default::default() });
		runs.push(Run { label: label.into(), environment: flat_ambient(), camera: top_view(center + Vec3::Y * size.y * 0.5) });
	}
	// two slabs meeting at x = 7168: does a region continue across the seam?
	for (label, x) in [("pairL", 7168.0 + 18.0), ("pairR", 7168.0 - 18.0)] {
		model.part(&PartSpec { name: label.into(), cframe: glam::Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)), size: slab, color: [255, 255, 255], material: 1040, ..Default::default() });
	}
	runs.push(Run { label: "pair".into(), environment: flat_ambient(), camera: top_view(Vec3::new(7168.0, 1.0, 0.0)) });
	Experiment { name: "organic-anchor".into(), model, probes: Vec::new(), runs }
}

/// Setup Luau: the model's MaterialVariants named `RorOrganic*` move into MaterialService (Studio
/// ignores them in the Workspace), replacing earlier runs' ones, and the parts naming them are
/// reassigned (parts loaded before their variant existed keep the base material otherwise).
const VARIANT_SETUP: &str = r#"local MaterialService = game:GetService("MaterialService")
for _, old in MaterialService:GetChildren() do
	if old:IsA("MaterialVariant") and string.sub(old.Name, 1, 10) == "RorOrganic" then
		old:Destroy()
	end
end
local moved = {}
for _, variant in workspace.RorCapture:GetDescendants() do
	if variant:IsA("MaterialVariant") and string.sub(variant.Name, 1, 10) == "RorOrganic" then
		table.insert(moved, variant)
	end
end
for _, variant in moved do
	variant.Parent = MaterialService
end
-- their maps load before the capture (else the parts show the base material); assigned twice, a
-- few seconds apart, as a variant whose maps arrive late may otherwise not draw
game:GetService("ContentProvider"):PreloadAsync(moved)
local parts = 0
local function assign()
	parts = 0
	for _, part in workspace.RorCapture:GetDescendants() do
		if part:IsA("BasePart") and string.sub(part.MaterialVariant, 1, 10) == "RorOrganic" then
			local name = part.MaterialVariant
			part.MaterialVariant = ""
			part.MaterialVariant = name
			parts += 1
		end
	end
end
assign()
task.wait(3)
assign()
return #moved .. " variants, " .. parts .. " parts"
"#;

fn with_setup(model: &mut ModelBuilder, code: &str) {
	let root = model.model;
	model.child(root, rbx_dom_weak::InstanceBuilder::new("StringValue").with_name(SETUP_SCRIPT).with_property("Value", code.to_string()));
}

/// A Plastic MaterialVariant with the given maps (property, asset), carried in the model
/// (`VARIANT_SETUP` moves it).
fn variant(model: &mut ModelBuilder, name: &str, maps: &[(&str, &str)], studs_per_tile: f32, organic: bool) {
	let root = model.model;
	let mut builder = rbx_dom_weak::InstanceBuilder::new("MaterialVariant")
		.with_name(name)
		.with_property("BaseMaterial", rbx_dom_weak::types::Enum::from_u32(256))
		.with_property("MaterialPattern", rbx_dom_weak::types::Enum::from_u32(organic as u32))
		.with_property("StudsPerTile", studs_per_tile);
	for (property, asset) in maps {
		builder = builder.with_property(*property, rbx_dom_weak::types::ContentId::from(*asset));
	}
	model.child(root, builder);
}

/// CorrodedMetal's maps (pack 122577965407434). The colour map has high contrast and no internal
/// repeat, so 1-stud patches locate themselves in it.
const CORRODED_METAL: [(&str, &str); 4] = [
	("ColorMap", "rbxassetid://93432967062797"),
	("NormalMap", "rbxassetid://79556365947166"),
	("MetalnessMap", "rbxassetid://136935150640534"),
	("RoughnessMap", "rbxassetid://119971623184180"),
];

/// The Organic pattern on MaterialVariants (CorrodedMetal's colour map): white 36 x 36 slabs 100
/// studs apart, Organic at 8, 4, 16 and 2 studs per tile and Regular at 8, each seen straight down
/// under flat light.
pub fn organic_variant() -> Experiment {
	let mut model = ModelBuilder::new("OrganicVariant");
	let mut runs = Vec::new();
	let cases: [(&str, f32, bool); 5] = [("organic8", 8.0, true), ("organic4", 4.0, true), ("organic16", 16.0, true), ("organic2", 2.0, true), ("regular8", 8.0, false)];
	for (index, (label, studs_per_tile, organic)) in cases.iter().enumerate() {
		let variant_name = format!("RorOrganic{index}");
		variant(&mut model, &variant_name, &CORRODED_METAL[..1], *studs_per_tile, *organic);
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = model.part(&PartSpec { name: (*label).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: [255, 255, 255], material: 256, ..Default::default() });
		model.set(part, "MaterialVariantSerialized", variant_name);
		runs.push(Run { label: (*label).into(), environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
	}
	with_setup(&mut model, VARIANT_SETUP);
	Experiment { name: "organic-variant".into(), model, probes: Vec::new(), runs }
}

/// The Organic pattern with every map (CorrodedMetal's pack as an Organic MaterialVariant, 8 studs
/// per tile): a slab under flat light and under a low sun (the normal map), and the other shapes'
/// faces head-on (a cube's sides, a ball, a cylinder, a wedge's slope).
pub fn organic_maps() -> Experiment {
	let mut model = ModelBuilder::new("OrganicMaps");
	let mut runs = Vec::new();
	variant(&mut model, "RorOrganicMaps", &CORRODED_METAL, 8.0, true);
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	let add = |model: &mut ModelBuilder, name: &str, class: &'static str, center: Vec3, size: Vec3, shape: Option<u32>| {
		let part = model.part(&PartSpec { class, name: name.into(), cframe: glam::Affine3A::from_translation(center), size, color: [255, 255, 255], material: 256, shape, ..Default::default() });
		model.set(part, "MaterialVariantSerialized", "RorOrganicMaps".to_string());
	};
	let slab = Vec3::ZERO;
	add(&mut model, "Slab", "Part", slab, Vec3::new(36.0, 2.0, 36.0), None);
	runs.push(Run { label: "slab-flat".into(), environment: flat_ambient(), camera: top_view(slab + Vec3::Y) });
	runs.push(Run { label: "slab-sun".into(), environment: low_sun, camera: top_view(slab + Vec3::Y) });
	let mut view = |label: &str, center: Vec3, direction: Vec3| {
		// a hair off the vertical for views along Y so the camera has an up direction
		let direction = if direction.y.abs() > 0.99 { (direction + Vec3::new(0.0, 0.0, -0.001 * direction.y.signum())).normalize() } else { direction.normalize() };
		runs.push(Run { label: label.into(), environment: flat_ambient(), camera: CameraSpec::LookAt { eye: (center + direction * 70.0).into(), target: center.into(), fov: 20.0 } });
	};
	let cube = Vec3::new(200.0, 0.0, 0.0);
	add(&mut model, "Cube", "Part", cube, Vec3::splat(16.0), None);
	for (label, axis) in [("cube-px", Vec3::X), ("cube-nz", Vec3::NEG_Z), ("cube-ny", Vec3::NEG_Y), ("cube-iso", Vec3::new(1.0, 0.8, -1.0))] {
		view(label, cube, axis);
	}
	let ball = Vec3::new(400.0, 0.0, 0.0);
	add(&mut model, "Ball", "Part", ball, Vec3::splat(16.0), Some(0));
	view("ball-py", ball, Vec3::Y);
	view("ball-nz", ball, Vec3::NEG_Z);
	let cylinder = Vec3::new(600.0, 0.0, 0.0);
	add(&mut model, "Cylinder", "Part", cylinder, Vec3::splat(16.0), Some(2));
	view("cylinder-nz", cylinder, Vec3::NEG_Z);
	view("cylinder-px", cylinder, Vec3::X);
	let wedge = Vec3::new(800.0, 0.0, 0.0);
	add(&mut model, "Wedge", "WedgePart", wedge, Vec3::splat(16.0), None);
	view("wedge-slope", wedge, Vec3::new(0.0, 1.0, -1.0));
	// a flatter, longer wedge: how the slope's length counts (8 up, 24 along Z)
	let long_wedge = Vec3::new(1000.0, 0.0, 0.0);
	add(&mut model, "LongWedge", "WedgePart", long_wedge, Vec3::new(16.0, 8.0, 24.0), None);
	view("longwedge-slope", long_wedge, Vec3::new(0.0, 24.0, -8.0));
	let corner = Vec3::new(1200.0, 0.0, 0.0);
	add(&mut model, "CornerWedge", "CornerWedgePart", corner, Vec3::splat(16.0), None);
	view("cornerwedge-a", corner, Vec3::new(0.0, 1.0, 1.0));
	view("cornerwedge-b", corner, Vec3::new(-1.0, 1.0, 0.0));
	// the ball again with a Regular variant of the same maps, for reference
	variant(&mut model, "RorOrganicMapsRegular", &CORRODED_METAL, 8.0, false);
	let regular_ball = Vec3::new(1400.0, 0.0, 0.0);
	let part = model.part(&PartSpec { name: "RegularBall".into(), cframe: glam::Affine3A::from_translation(regular_ball), size: Vec3::splat(16.0), color: [255, 255, 255], material: 256, shape: Some(0), ..Default::default() });
	model.set(part, "MaterialVariantSerialized", "RorOrganicMapsRegular".to_string());
	view("regularball-py", regular_ball, Vec3::Y);
	with_setup(&mut model, VARIANT_SETUP);
	Experiment { name: "organic-maps".into(), model, probes: Vec::new(), runs }
}

/// Normal maps under the Organic pattern without metal: Sandstone's and Slate's packs as Organic
/// MaterialVariants (8 studs per tile) on slabs, under flat light and a low sun. (Grass's and Rock's
/// maps do not draw as a variant's: Studio shows plain Plastic.)
pub fn organic_sun() -> Experiment {
	let mut model = ModelBuilder::new("OrganicSun");
	let mut runs = Vec::new();
	let packs: [(&str, [(&str, &str); 3]); 2] = [
		("sandstone", [("ColorMap", "rbxassetid://75311569459836"), ("NormalMap", "rbxassetid://86625778869912"), ("RoughnessMap", "rbxassetid://93308437897556")]),
		("slate", [("ColorMap", "rbxassetid://87694454713126"), ("NormalMap", "rbxassetid://78438403538623"), ("RoughnessMap", "rbxassetid://111124347732838")]),
	];
	let low_sun = Environment { clock_time: 8.0, ..sun_only(2.0) };
	for (index, (label, maps)) in packs.iter().enumerate() {
		let name = format!("RorOrganicSun{index}");
		variant(&mut model, &name, maps, 8.0, true);
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let part = model.part(&PartSpec { name: (*label).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: [255, 255, 255], material: 256, ..Default::default() });
		model.set(part, "MaterialVariantSerialized", name);
		runs.push(Run { label: format!("{label}-flat"), environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
		runs.push(Run { label: format!("{label}-sun"), environment: low_sun.clone(), camera: top_view(center + Vec3::Y) });
	}
	with_setup(&mut model, VARIANT_SETUP);
	Experiment { name: "organic-sun".into(), model, probes: Vec::new(), runs }
}

/// Organic wedge slopes of different lengths (the Organic maps' variant), each seen head-on: how the
/// slope's texture coordinate scales with the wedge's size.
pub fn organic_slopes() -> Experiment {
	let mut model = ModelBuilder::new("OrganicSlopes");
	let mut runs = Vec::new();
	variant(&mut model, "RorOrganicMaps", &CORRODED_METAL, 8.0, true);
	let sizes: [(&str, Vec3); 4] = [("w16x4x4", Vec3::new(16.0, 4.0, 4.0)), ("w16x30x10", Vec3::new(16.0, 30.0, 10.0)), ("w16x8x8", Vec3::new(16.0, 8.0, 8.0)), ("w16x6x40", Vec3::new(16.0, 6.0, 40.0))];
	for (index, (label, size)) in sizes.iter().enumerate() {
		let center = Vec3::new(index as f32 * 200.0, 0.0, 0.0);
		let part = model.part(&PartSpec { class: "WedgePart", name: (*label).into(), cframe: glam::Affine3A::from_translation(center), size: *size, color: [255, 255, 255], material: 256, ..Default::default() });
		model.set(part, "MaterialVariantSerialized", "RorOrganicMaps".to_string());
		// the slope's outward normal, (0, z, -y) normalised
		let normal = Vec3::new(0.0, size.z, -size.y).normalize();
		let distance = 30.0 + size.y.max(size.z);
		runs.push(Run { label: (*label).into(), environment: flat_ambient(), camera: CameraSpec::LookAt { eye: (center + normal * distance).into(), target: center.into(), fov: 20.0 } });
	}
	with_setup(&mut model, VARIANT_SETUP);
	Experiment { name: "organic-slopes".into(), model, probes: Vec::new(), runs }
}

/// Setup Luau for `organic-reset`: property changes on loaded parts.
const RESET_SETUP: &str = r#"local model = workspace.RorCapture:FindFirstChild("OrganicReset")
local toggled = model:FindFirstChild("toggled")
toggled.Material = Enum.Material.Plastic
toggled.Material = Enum.Material.CorrodedMetal
local recolored = model:FindFirstChild("recolored")
recolored.Color = Color3.new(1, 1, 0.98)
recolored.Color = Color3.new(1, 1, 1)
local moved = model:FindFirstChild("moved")
moved.Position += Vector3.new(0, 1, 0)
moved.Position -= Vector3.new(0, 1, 0)
local cloned = model:FindFirstChild("cloned")
local copy = cloned:Clone()
cloned:Destroy()
copy.Parent = model
"#;

/// Built-in Organic materials take a random offset per part: CorrodedMetal slabs left as loaded,
/// with their Material set again, recoloured, moved and back, and cloned (which property changes
/// draw a new offset?); and a 200-stud slab, built-in and as an Organic MaterialVariant (8 studs
/// per tile), from 236 studs up (13.4 px per stud, 15 x 10 tiles in view) to register the noise
/// field's offset.
pub fn organic_reset() -> Experiment {
	let mut model = ModelBuilder::new("OrganicReset");
	let mut runs = Vec::new();
	let slab = Vec3::new(36.0, 2.0, 36.0);
	for (index, label) in ["untouched", "toggled", "recolored", "moved", "cloned"].iter().enumerate() {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*label).into(), cframe: glam::Affine3A::from_translation(center), size: slab, color: [255, 255, 255], material: 1040, ..Default::default() });
		runs.push(Run { label: (*label).into(), environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
	}
	let big = Vec3::new(200.0, 2.0, 200.0);
	let far = |center: Vec3| CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 236.0, -0.2)).into(), target: center.into(), fov: 20.0 };
	let builtin = Vec3::new(1000.0, 0.0, 0.0);
	model.part(&PartSpec { name: "big".into(), cframe: glam::Affine3A::from_translation(builtin), size: big, color: [255, 255, 255], material: 1040, ..Default::default() });
	runs.push(Run { label: "big".into(), environment: flat_ambient(), camera: far(builtin + Vec3::Y) });
	let varianted = Vec3::new(1300.0, 0.0, 0.0);
	variant(&mut model, "RorOrganicBig", &CORRODED_METAL[..1], 8.0, true);
	let part = model.part(&PartSpec { name: "bigvariant".into(), cframe: glam::Affine3A::from_translation(varianted), size: big, color: [255, 255, 255], material: 256, ..Default::default() });
	model.set(part, "MaterialVariantSerialized", "RorOrganicBig".to_string());
	runs.push(Run { label: "bigvariant".into(), environment: flat_ambient(), camera: far(varianted + Vec3::Y) });
	with_setup(&mut model, &format!("{RESET_SETUP}{VARIANT_SETUP}"));
	Experiment { name: "organic-reset".into(), model, probes: Vec::new(), runs }
}

/// Every built-in Organic material on a white 200-stud slab, seen straight down from 236 studs
/// (13.4 px per stud, about 15 x 10 tiles in view) under flat light: each material's noise frequency
/// (`organic.py offsets <run> <pack> --far`).
pub fn organic_builtin() -> Experiment {
	let materials: [(&str, u32); 18] = [
		("Wood", 512),
		("Basalt", 788),
		("Slate", 800),
		("Concrete", 816),
		("Limestone", 820),
		("Granite", 832),
		("Rock", 896),
		("Sandstone", 912),
		("CorrodedMetal", 1040),
		("Grass", 1280),
		("LeafyGrass", 1284),
		("Sand", 1296),
		("Snow", 1328),
		("Mud", 1344),
		("Ground", 1360),
		("Asphalt", 1376),
		("Ice", 1536),
		("Glacier", 1552),
	];
	let mut model = ModelBuilder::new("OrganicBuiltIn");
	let mut runs = Vec::new();
	for (index, (name, material)) in materials.iter().enumerate() {
		let center = Vec3::new(index as f32 * 400.0, 0.0, 0.0);
		model.part(&PartSpec { name: (*name).into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(200.0, 2.0, 200.0), color: [255, 255, 255], material: *material, ..Default::default() });
		runs.push(Run {
			label: (*name).into(),
			environment: flat_ambient(),
			camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 237.0, -0.2)).into(), target: (center + Vec3::Y).into(), fov: 20.0 },
		});
	}
	Experiment { name: "organic-builtin".into(), model, probes: Vec::new(), runs }
}

/// Built-in Organic slabs that carry a fixed UniqueId: run twice (ROR_RECAPTURE=1 the second time),
/// does the pattern repeat across loads?
pub fn organic_uniqueid() -> Experiment {
	let mut model = ModelBuilder::new("OrganicUniqueId");
	let mut runs = Vec::new();
	for index in 0..2u32 {
		let center = Vec3::new(index as f32 * 100.0, 0.0, 0.0);
		let label = format!("slab{index}");
		let part = model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(36.0, 2.0, 36.0), color: [255, 255, 255], material: 1040, ..Default::default() });
		model.set(part, "UniqueId", rbx_dom_weak::types::UniqueId::new(index + 1, 0x1234_5678, 0x0102_0304_0506_0708 + index as i64));
		runs.push(Run { label, environment: flat_ambient(), camera: top_view(center + Vec3::Y) });
	}
	Experiment { name: "organic-uniqueid".into(), model, probes: Vec::new(), runs }
}

/// Sky images (local files are fine for a Sky): white, and dark blue-gray (45, 47, 49).
const SKY_WHITE: &str = "rbxasset://textures/AvatarEditorImages/LightPixel.png";
const SKY_DARK: &str = "rbxasset://textures/AvatarEditorImages/DarkPixel.png";



/// The sky specular alone (no sun, ambient or sky diffuse).
fn sky_specular_only(scale: f32) -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: scale,
		global_shadows: false,
		..Environment::default()
	}
}

/// The default sky without sun and moon discs, and a sky that is white straight up (the Up image)
/// and dark everywhere else: sharp edges in the reflected sky.
fn default_sky() -> scene::environment::Sky {
	scene::environment::Sky { celestial_bodies_shown: false, ..Default::default() }
}







/// Part.Reflectance: white and black SmoothPlastic, white Plastic and a white Metal ball at
/// Reflectance 0, 0.25, 0.5, 0.75 and 1, from 30 degrees above, under the sky specular alone, the sky
/// specular with the sun and sky diffuse, and the sun alone (does Reflectance need the sky specular?).
/// Where Part.Reflectance's mirror shows (the Linked Sword, Reflectance 0.4 with a SpecialMesh
/// texture, shows its texture and no sky in Studio): a block, the head mesh as a SpecialMesh and as a
/// MeshPart, each without and with a texture, and the Linked Sword itself, at Reflectance 0 (front
/// row) and 0.5 (back row), under the default lighting.
pub fn reflectance_meshes() -> Experiment {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{ContentId, Enum, Vector3};

	const HEAD_MESH: &str = "rbxasset://avatar/heads/head.mesh";
	const SKY: &str = "rbxasset://textures/sky/sky512_ft.tex";
	const SWORD_MESH: &str = "http://www.roblox.com/asset/?id=12221720";
	const SWORD_TEXTURE: &str = "http://www.roblox.com/asset/?id=12224218";
	let mut model = ModelBuilder::new("ReflectanceMeshes");
	for (row, reflectance) in [0.0f32, 0.5].iter().enumerate() {
		let z = row as f32 * 8.0;
		let at = |column: usize| Vec3::new(column as f32 * 7.0 - 17.5, 0.0, z);
		model.part(&PartSpec { name: format!("block_r{reflectance}"), cframe: glam::Affine3A::from_translation(at(0)), size: Vec3::splat(4.0), color: [255, 255, 255], reflectance: *reflectance, cast_shadow: false, ..Default::default() });
		for (column, texture) in [(1usize, None), (2, Some(SKY))] {
			let part = model.part(&PartSpec { name: format!("special{}_r{reflectance}", if texture.is_some() { "-tex" } else { "" }), cframe: glam::Affine3A::from_translation(at(column)), size: Vec3::splat(1.0), color: [255, 255, 255], reflectance: *reflectance, cast_shadow: false, ..Default::default() });
			let mut mesh = InstanceBuilder::new("SpecialMesh").with_property("MeshType", Enum::from_u32(5)).with_property("MeshId", ContentId::from(HEAD_MESH)).with_property("Scale", Vector3::new(3.0, 3.0, 3.0));
			if let Some(texture) = texture {
				mesh = mesh.with_property("TextureId", ContentId::from(texture));
			}
			model.child(part, mesh);
		}
		for (column, texture) in [(3usize, None), (4, Some(SKY))] {
			let part = model.part(&PartSpec { class: "MeshPart", name: format!("meshpart{}_r{reflectance}", if texture.is_some() { "-tex" } else { "" }), cframe: glam::Affine3A::from_translation(at(column)), size: Vec3::splat(3.6), color: [255, 255, 255], reflectance: *reflectance, cast_shadow: false, ..Default::default() });
			model.set(part, "MeshId", ContentId::from(HEAD_MESH));
			model.set(part, "InitialSize", Vector3::new(1.197849, 1.2024246, 1.197849));
			if let Some(texture) = texture {
				model.set(part, "TextureID", ContentId::from(texture));
			}
		}
		let part = model.part(&PartSpec { name: format!("sword_r{reflectance}"), cframe: glam::Affine3A::from_translation(at(5)) * glam::Affine3A::from_rotation_y(std::f32::consts::FRAC_PI_2), size: Vec3::new(1.0, 0.8, 4.0), color: [99, 95, 98], reflectance: *reflectance, cast_shadow: false, ..Default::default() });
		model.child(part, InstanceBuilder::new("SpecialMesh").with_property("MeshType", Enum::from_u32(5)).with_property("MeshId", ContentId::from(SWORD_MESH)).with_property("TextureId", ContentId::from(SWORD_TEXTURE)));
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 14.0, -30.0], target: [0.0, 0.0, 4.0], fov: 40.0 };
	let runs = vec![
		Run { label: "default".into(), environment: Environment::default(), camera: camera.clone() },
		Run { label: "sky-only".into(), environment: Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, global_shadows: false, ..Environment::default() }, camera },
	];
	Experiment { name: "reflectance-meshes".into(), model, probes: Vec::new(), runs }
}

pub fn reflectance() -> Experiment {
	let mut model = ModelBuilder::new("Reflectance");
	let mut probes = Vec::new();
	let direction = Vec3::new(0.0, 30f32.to_radians().sin(), -30f32.to_radians().cos());
	let across = direction.cross(Vec3::Y).normalize();
	let up = across.cross(direction).normalize();
	let rows: [(&str, u32, [u8; 3]); 4] = [("smooth", 272, [255; 3]), ("black", 272, [0; 3]), ("plastic", 256, [255; 3]), ("metal", 1088, [255; 3])];
	for (row, (name, material, color)) in rows.iter().enumerate() {
		for (column, reflectance) in [0.0f32, 0.25, 0.5, 0.75, 1.0].iter().enumerate() {
			let center = across * ((column as f32 - 2.0) * 12.0) + up * ((1.5 - row as f32) * 12.0);
			let label = format!("{name}_r{reflectance}");
			model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(8.0), color: *color, material: *material, shape: Some(0), reflectance: *reflectance, cast_shadow: false, ..Default::default() });
			probes.push(Probe { label, position: center.into(), inputs: serde_json::json!({"material": material, "color": color, "reflectance": reflectance, "center": center.to_array(), "radius": 4.0}) });
		}
	}
	let camera = CameraSpec::LookAt { eye: (direction * 150.0).into(), target: [0.0, 0.0, 0.0], fov: 30.0 };
	let base = Environment { sky: Some(default_sky()), clock_time: 8.0, ..sky_specular_only(1.0) };
	let mut runs = vec![
		Run { label: "specular".into(), environment: base.clone(), camera: camera.clone() },
		Run { label: "all".into(), environment: Environment { brightness: 2.0, environment_diffuse_scale: 1.0, ..base.clone() }, camera: camera.clone() },
		Run { label: "sun".into(), environment: Environment { brightness: 2.0, environment_specular_scale: 0.0, ..base.clone() }, camera: camera.clone() },
		Run { label: "specular-half".into(), environment: Environment { environment_specular_scale: 0.5, ..base.clone() }, camera: camera.clone() },
	];
	// what scales the Reflectance mirror: the sun's brightness and height, the flat ambient, the sky
	// diffuse, the exposure
	let off = Environment { environment_specular_scale: 0.0, ..base };
	for brightness in [0.5f32, 1.0, 4.0] {
		runs.push(Run { label: format!("sun{brightness}"), environment: Environment { brightness, ..off.clone() }, camera: camera.clone() });
	}
	runs.push(Run { label: "sun-noon".into(), environment: Environment { brightness: 2.0, clock_time: 12.0, ..off.clone() }, camera: camera.clone() });
	runs.push(Run { label: "ambient".into(), environment: Environment { ambient: Vec3::splat(0.6), ..off.clone() }, camera: camera.clone() });
	runs.push(Run { label: "outdoor".into(), environment: Environment { outdoor_ambient: Vec3::splat(0.6), ..off.clone() }, camera: camera.clone() });
	runs.push(Run { label: "diffuse".into(), environment: Environment { environment_diffuse_scale: 1.0, ..off.clone() }, camera: camera.clone() });
	runs.push(Run { label: "sun-ev1".into(), environment: Environment { brightness: 2.0, exposure_compensation: 1.0, ..off }, camera });
	Experiment { name: "reflectance".into(), model, probes, runs }
}

pub fn experiment(name: &str) -> Result<Experiment> {
	Ok(match name {
		"atmosphere-sky" => environment::atmosphere_sky(),
		"atmosphere-haze" => environment::atmosphere_haze(),
		"atmosphere-glare" => environment::atmosphere_glare(),
		"atmosphere-altitude" => environment::atmosphere_altitude(),
		"atmosphere-objects" => environment::atmosphere_objects(),
		"atmosphere-objects2" => environment::atmosphere_objects2(),
		"atmosphere-mips" => environment::atmosphere_mips(),
		"atmosphere-night-glare" => environment::atmosphere_night_glare(),
		"fog" => environment::fog(),
		"fog-near" => environment::fog_near(),
		"tone-toe" => environment::tone_toe(),
		"dawn-tint" => environment::dawn_tint(),
		"shift-diffuse" => environment::shift_diffuse(),
		"atmosphere-shift" => environment::atmosphere_shift(),
		"sun-shift" => environment::sun_shift(),
		"sky-elements" => environment::sky_elements(),
		"sky-time" => environment::sky_time(),
		"sky-night" => environment::sky_night(),
		"post-color" => post::post_color(),
		"post-color-extreme" => post::post_color_extreme(),
		"post-color-order" => post::post_color_order(),
		"tone-retro" => post::tone_retro(),
		"post-blur" => post::post_blur(),
		"post-depth" => post::post_depth(),
		"post-depth-effects" => post::post_depth_effects(),
		"post-depth-grid" => post::post_depth_grid(),
		"post-depth-halo" => post::post_depth_halo(),
		"post-depth-blended" => post::post_depth_blended(),
		"post-sunrays" => post::post_sunrays(),
		"post-sunrays-time" => post::post_sunrays_time(),
		"post-sunrays-clock" => post::post_sunrays_clock(),
		"post-sunrays-depth" => post::post_sunrays_depth(),
		"particle-basic" => particles::particle_basic(),
		"particle-orient" => particles::particle_orient(),
		"particle-age" => particles::particle_age(),
		"particle-order" => particles::particle_order(),
		"particle-motion" => particles::particle_motion(),
		"particle-spread" => particles::particle_spread(),
		"particle-shapes" => particles::particle_shapes(),
		"particle-shape-partial" => particles::particle_shape_partial(),
		"particle-shape-taper" => particles::particle_shape_taper(),
		"particle-shape-ellipse" => particles::particle_shape_ellipse(),
		"particle-cylinder-axis" => particles::particle_cylinder_axis(),
		"particle-overlap" => particles::particle_overlap(),
		"particle-drag" => particles::particle_drag(),
		"particle-spread-frames" => particles::particle_spread_frames(),
		"particle-locked-accel" => particles::particle_locked_accel(),
		"particle-copies" => particles::particle_copies(),
		"particle-drag-accel" => particles::particle_drag_accel(),
		"particle-soft" => particles::particle_soft(),
		"particle-soft-size" => particles::particle_soft_size(),
		"particle-bright" => particles::particle_bright(),
		"particle-bright-gray" => particles::particle_bright_gray(),
		"particle-influence" => particles::particle_influence(),
		"particle-zoffset-near" => particles::particle_zoffset_near(),
		"particle-zoffset-close" => particles::particle_zoffset_close(),
		"particle-faint-stack" => particles::particle_faint_stack(),
		"particle-offscreen" => particles::particle_offscreen(),
		"particle-drag-negative" => particles::particle_drag_negative(),
		"particle-near" => particles::particle_near(),
		"particle-offscreen-reach" => particles::particle_offscreen_reach(),
		"particle-soft-big" => particles::particle_soft_big(),
		"particle-timescale" => particles::particle_timescale(),
		"particle-held-live" => particles::particle_held(false),
		"particle-held-frozen" => particles::particle_held(true),
		"particle-overdraw" => particles::particle_overdraw(),
		"particle-quantize" => particles::particle_quantize(),
		"particle-budget" => particles::particle_budget(),
		"trail-shape" => trails::trail_shape(),
		"trail-shading" => trails::trail_shading(),
		"beam-scroll" => scroll::beam_scroll(),
		"trail-texture" => trails::trail_texture(),
		"trail-life" => trails::trail_life(),
		"legacy-survey" => legacy::legacy_survey(),
		"legacy-far" => legacy::legacy_far(),
		"legacy-far-fire" => legacy::legacy_far_fire(),
		"legacy-fire-look" => legacy::legacy_fire_look(),
		"legacy-sparkles-look" => legacy::legacy_sparkles_look(),
		"legacy-smoke-look" => legacy::legacy_smoke_look(),
		"legacy-forcefield-look" => legacy::legacy_forcefield_look()?,
		"legacy-forcefield-where" => legacy::legacy_forcefield_where()?,
		"legacy-fire-cohort" => legacy::legacy_fire_cohort(),
		"legacy-fire-parts" => legacy::legacy_fire_parts(),
		"legacy-fire-cohort-long" => legacy::legacy_fire_cohort_long(),
		"legacy-fire-flame" => legacy::legacy_fire_flame(),
		"legacy-fire-secondary" => legacy::legacy_fire_secondary(),
		"legacy-fire-steady" => legacy::legacy_fire_steady(),
		"legacy-fire-growth" => legacy::legacy_fire_growth(),
		"legacy-fire-after" => legacy::legacy_fire_after(),
		"legacy-fire-moving" => legacy::legacy_fire_moving(),
		"legacy-fire-blend" => legacy::legacy_fire_blend(),
		"legacy-fire-grid" => legacy::legacy_fire_grid(),
		"legacy-fire-cover" => legacy::legacy_fire_cover(),
		"legacy-fire-pair" => legacy::legacy_fire_pair(),
		"legacy-fire-mean" => legacy::legacy_fire_mean(),
		"legacy-fire-life" => legacy::legacy_fire_life(),
		"legacy-fire-hide" => legacy::legacy_fire_hide(),
		"legacy-sparkles-mean" => legacy::legacy_sparkles_mean(),
		"legacy-sparkles-panel" => legacy::legacy_sparkles_panel(),
		"legacy-smoke-mean" => legacy::legacy_smoke_mean(),
		"legacy-smoke-cohort" => legacy::legacy_smoke_cohort(),
		"legacy-smoke-single" => legacy::legacy_smoke_single(),
		"legacy-smoke-cover" => legacy::legacy_smoke_cover(),
		"legacy-smoke-late" => legacy::legacy_smoke_late(),
		"legacy-smoke-white" => legacy::legacy_smoke_white(),
		"legacy-explosion-ages" => legacy::legacy_explosion_ages(),
		"legacy-explosion-end" => legacy::legacy_explosion_end(),
		"legacy-explosion-live" => legacy::legacy_explosion_live(),
		"legacy-explosion-panel" => legacy::legacy_explosion_panel(),
		"legacy-forcefield-panel" => legacy::legacy_forcefield_panel(),
		other if other.starts_with("legacy-explosion-") && legacy::legacy_explosion_radius(&other["legacy-explosion-".len()..]).is_some() => legacy::legacy_explosion_radius(&other["legacy-explosion-".len()..]).context("legacy-explosion radius")?,
		"legacy-forcefield-mean" => legacy::legacy_forcefield_mean()?,
		"legacy-smoke-stack" => legacy::legacy_stack("smoke").context("legacy stack")?,
		"legacy-sparkles-stack" => legacy::legacy_stack("sparkles").context("legacy stack")?,
		other if other.starts_with("legacy-fire-") && legacy::legacy_fire_scale(&other["legacy-fire-".len()..]).is_some() => legacy::legacy_fire_scale(&other["legacy-fire-".len()..]).context("legacy-fire scale")?,
		other if other.starts_with("vfx-attack:") => attacks::vfx_attack(&other["vfx-attack:".len()..])?,
		other if other.starts_with("vfx-emitter:") => attacks::vfx_emitter(&other["vfx-emitter:".len()..])?,
		other if other.starts_with("exposure:") || other.starts_with("exposure(") => exposure::experiment(other)?,
		other if other.starts_with("vfx-sweep:") => sweep::experiment(&other["vfx-sweep:".len()..])?,
		other if other.starts_with("particle-model:") => particles::particle_model(&other["particle-model:".len()..])?,
		other if other.starts_with("particle-view:") => particles::particle_view(&other["particle-view:".len()..])?,
		"forcefield-levels" => forcefield_levels(),
		"forcefield-blend" => forcefield_blend(),
		"forcefield-angles" => forcefield_angles(),
		"forcefield-shadow" => forcefield_shadow(),
		"forcefield-depth" => forcefield_depth(),
		"forcefield-contact" => forcefield_contact(),
		"forcefield-neon" => forcefield_neon(),
		"forcefield-basics" => forcefield_basics(),
		"forcefield-pattern" => forcefield_pattern(),
		"beam-shape" => beam_shape(),
		"beam-texture" => beam_texture(),
		"beam-color" => beam_color(),
		"beam-brightness" => beam_brightness(),
		"beam-influence" => beam_influence(),
		"beam-ties" => beam_ties(),
		"beam-sequence" => beam_sequence(),
		"beam-order" => beam_order(),
		"beam-sort" => beam_sort(),
		"beam-misc" => beam_misc(),
		"ibl-ground" => ibl_ground(),
		"reflectance" => reflectance(),
		"reflectance-meshes" => reflectance_meshes(),
		"specular-spheres" => specular_spheres(),
		"material-swatches" => material_swatches(),
		"surfaces" => surfaces(),
		"surfaces-shapes" => surfaces_shapes(),
		"highlights" => highlights(),
		"neon-slabs" => neon_slabs(),
		"neon-angles" => neon_angles(),
		"neon-slabs-bloom" => neon_slabs_bloom(),
		"neon-tone" => neon_tone(),
		"tone-top" => tone_top(),
		"bloom-wide" => bloom_wide(),
		"aa-edges-far" => aa_edges(),
		"lightgrid" => lightgrid(),
		"humanoid-phase" => humanoid_phase(),
		"lg-height" => lightgrid::lg_height(),
		"lg-size" => lightgrid::lg_size(),
		"lg-edge" => lightgrid::lg_edge(),
		"lg-specular" => lightgrid::lg_specular(),
		"lg-occupancy" => lightgrid::lg_occupancy(),
		"lg-terms" => lightgrid::lg_terms(),
		"lg-sky" => lightgrid::lg_sky(),
		"lg-walls" => lightgrid::lg_walls(),
		"lg-shapes" => lightgrid::lg_shapes(),
		"lg-steps" => lightgrid::lg_steps(),
		"ao-distance" => lightgrid::ao_distance(),
		"ao-far" => lightgrid::ao_far(),
		"ao-fade" => lightgrid::ao_fade(),
		"ao-gap" => lightgrid::ao_gap(),
		"ao-corner" => lightgrid::ao_corner(),
		"ao-scale" => lightgrid::ao_scale(),
		"ao-depth" => lightgrid::ao_depth(),
		"ao-dolly" => lightgrid::ao_dolly(),
		"ao-size" => lightgrid::ao_size(),
		"ao-layers" => lightgrid::ao_layers(),
		"ao-ramps" => lightgrid::ao_ramps(),
		"ao-relief" => lightgrid::ao_relief(),
		"ao-thin" => lightgrid::ao_thin(),
		"ao-sees" => lightgrid::ao_sees(),
		"ao-transparency" => lightgrid::ao_transparency(),
		"material-mapping" => material_mapping(),
		"neon-discs" => neon_discs(),
		"metal-diffuse" => metal_diffuse(),
		"sky-reflection" => sky_reflection(),
		"material-offsets" => material_offsets(),
		"organic-anchor" => organic_anchor(),
		"organic-variant" => organic_variant(),
		"organic-maps" => organic_maps(),
		"organic-slopes" => organic_slopes(),
		"organic-sun" => organic_sun(),
		"organic-reset" => organic_reset(),
		"organic-uniqueid" => organic_uniqueid(),
		"organic-builtin" => organic_builtin(),
		"meshpart-materials" => meshpart_materials(),
		"material-placement" => material_placement(),
		"material-distance" => material_distance(),
		"material-far" => material_far(),
		"material-depths" => material_depths(),
		"material-tint" => material_tint(),
		"material-zoom" => material_zoom(),
		"material-phase" => material_phase(),
		"material-glass" => material_glass(),
		"sky-faces" => sky_faces(),
		"sky-light" => sky_light(),
		"sky-kernel" => sky_kernel(),
		"shadow-contact" => shadow_contact(),
		"shadow-cascades" => shadows::shadow_cascades(),
		"shadow-cascade-map" => shadows::shadow_cascade_map(),
		"shadow-softness" => shadows::shadow_softness(),
		"shadow-cascade-down" => shadows::shadow_cascade_down(),
		"shadow-texels" => shadows::shadow_texels(),
		"shadow-gap" => shadows::shadow_gap(),
		"sun-sky-mix" => sun_sky_mix(),
		"neon-transparency" => neon_transparency(),
		"neon-colors" => neon_colors(),
		"shadow-steps" => shadow_steps(),
		"bloom-probe" => bloom_probe(),
		"shapes-close" => shapes_close(),
		"specialmesh-sizes" => specialmesh_sizes(),
		"tessellation-ball" => tessellation(0),
		"tessellation-cylinder" => tessellation(2),
		"shadow-edge" => shadow_edge(),
		"sun-board" => sun_board(&[
			(0.25, 0.0),
			(0.5, 0.0),
			(0.75, 0.0),
			(1.0, 0.0),
			(1.5, 0.0),
			(2.0, 0.0),
			(2.5, 0.0),
			(3.0, 0.0),
			(5.0, 0.0),
			(8.0, 0.0),
			(1.0, -2.0),
			(1.0, -1.0),
			(1.0, 1.0),
			(1.0, 2.0),
			(1.0, 3.0),
		]),
		"ambient-cubes" => ambient_cubes(),
		"neon-rows" => neon_rows(),
		"bloom-threshold" => bloom::bloom_threshold(),
		"bloom-size" => bloom::bloom_size(),
		"bloom-colors" => bloom::bloom_colors(),
		"bloom-kernel" => bloom::bloom_kernel(),
		"bloom-effect" => bloom::bloom_effect(),
		"bloom-lines" => bloom::bloom_lines(),
		"bloom-phase" => bloom::bloom_phase(),
		"bloom-dots" => bloom::bloom_dots(),
		"bloom-pairs" => bloom::bloom_pairs(),
		"bloom-shape" => bloom::bloom_shape(),
		"bloom-hue" => bloom::bloom_hue(),
		"neon-cap" => bloom::neon_cap(),
		"bloom-effect-size" => bloom::bloom_effect_size(),
		"light-falloff" => lights::light_falloff(),
		"light-range" => lights::light_range(),
		"light-spot" => lights::light_spot(),
		"light-surface" => lights::light_surface(),
		"light-surface-floor" => lights::light_surface_floor(),
		"light-angle-small" => lights::light_angle_small(),
		"light-surface-apex-small" => lights::light_surface_apex_small(),
		"light-surface-back" => lights::light_surface_back(),
		"light-shadows" => lights::light_shadows(),
		"light-materials" => lights::light_materials(),
		"light-surface-rect" => lights::light_surface_rect(),
		"light-brightness" => lights::light_brightness(),
		"light-particles" => lights::light_particles(),
		"light-transparency" => lights::light_transparency(),
		"light-many" => lights::light_many(),
		"light-near" => lights::light_near(),
		"light-surface-apex" => lights::light_surface_apex(),
		"light-particle-field" => lights::light_particle_field(),
		"light-particle-voxels" => lights::light_particle_voxels(),
		"light-night" => lights::light_night(),
		"light-graze" => lights::light_graze(),
		"light-parents" => lights::light_parents(),
		"light-particle-reach" => lights::light_particle_reach(),
		"light-reach" => lights::light_reach(),
		"light-far" => lights::light_far(),
		"light-far-transparent" => lights::light_far_transparent(),
		"light-far-range" => lights::light_far_range(),
		"light-latency" => lights::light_latency(),
		"light-night-sky" => lights::light_night_sky(),
		"light-night-terms" => lights::light_night_terms(),
		"light-night-twilight" => lights::light_night_twilight(),
		"light-night-sky2" => lights::light_night_sky2(),
		"light-graze-kinds" => lights::light_graze_kinds(),
		"light-graze-pairs" => lights::light_graze_pairs(),
		"light-particle-cones" => lights::light_particle_cones(),
		"light-particle-narrow" => lights::light_particle_narrow(),
		"light-particle-cut" => lights::light_particle_cut(),
		"light-graze-more" => lights::light_graze_more(),
		"gui-surface-basics" => guis::gui_surface_basics(),
		"gui-billboard-basics" => guis::gui_billboard_basics(),
		"gui-light" => guis::gui_light(),
		"gui-highlight" => guis::gui_highlight(),
		"gui-billboard-limits" => guis::gui_billboard_limits(),
		"gui-brightness" => guis::gui_brightness(),
		"gui-brightness-alpha" => guis::gui_brightness_alpha(),
		"gui-bloom-layers" => guis::gui_bloom_layers(),
		"mesh-origin" => geometry::mesh_origin()?,
		"specialmesh-placement" => geometry::specialmesh_placement()?,
		"mesh-lod-hq" => geometry::mesh_lod_hq()?,
		"character-anchor" => characters::character_anchor()?,
		"character-single" => characters::character_single()?,
		other if other.starts_with("decompose:") => decompose(std::path::Path::new(&other["decompose:".len()..]))?,
		other if other.starts_with("shadow-view:") => {
			let (eye, look, fov, path) = view_spec(other, "shadow-view")?;
			shadows::shadow_view(path, eye, look, fov)?
		}
		other if other.starts_with("decompose-view:") => {
			let (eye, look, fov, path) = view_spec(other, "decompose-view")?;
			decompose_view(path, eye, look, fov)?
		}
		other if other.starts_with("sky-context:") => {
			let (eye, look, fov, path) = view_spec(other, "sky-context")?;
			sky_context(path, eye, look, fov)?
		}
		other if other.starts_with("cap-terms:") => cap_terms(std::path::Path::new(&other["cap-terms:".len()..]))?,
		other if other.starts_with("fresh-view:") => {
			let (eye, look, fov, path) = view_spec(other, "fresh-view")?;
			fresh_view(path, eye, look, fov)?
		}
		other if other.starts_with("fresh-decompose:") => {
			let (eye, look, fov, path) = view_spec(other, "fresh-decompose")?;
			fresh_decompose(path, eye, look, fov)?
		}
		other if other.starts_with("ao-model:") => lightgrid::ao_model(std::path::Path::new(&other["ao-model:".len()..]))?,
		// a parity close-up's camera
		other if other.starts_with("ao-view:") => lightgrid::ao_view(std::path::Path::new(&other["ao-view:".len()..]), Vec3::new(150.0, 70.0, -412.0), Vec3::new(-0.633, -0.416, 0.653), 40.0)?,
		// from ao-model's default camera on a character
		other if other.starts_with("ao-fov:") => lightgrid::ao_fov(std::path::Path::new(&other["ao-fov:".len()..]), Vec3::new(56.38277, 5.810_473, 37.54002), Vec3::new(-0.469_846_6, -0.342_020_06, 0.813_797_5), 70.0)?,
		other => bail!("unknown experiment {other:?} (the names are in crates/ror/src/studio/calibrate.rs, experiment())"),
	})
}



/// `glass-distort`'s black Plastic wall (80 x 40 studs about the origin, head-on from 51 studs) reads
/// its sky specular 6-10 % under ror's on its lower half and as ror's on its top, where `ibl-walls`'
/// panels 500 studs up match at every height: the same wall at y = 0 and 500 studs up, under the sky
/// specular alone with GlobalShadows off and on. (The walls stand 2,000 studs apart, beyond ror's
/// light grid, which then covers neither: `lg-specular` has the wall inside it.)
pub fn ibl_ground() -> Experiment {
	let mut model = ModelBuilder::new("IblGround");
	let mut runs = Vec::new();
	let sky = Environment { sky: Some(default_sky()), ..sky_specular_only(1.0) };
	for (index, height) in [0.0f32, 500.0].into_iter().enumerate() {
		let origin = Vec3::new(2000.0 * index as f32, height, 0.0);
		model.part(&PartSpec { name: format!("wall-y{height}"), cframe: glam::Affine3A::from_translation(origin + Vec3::new(0.0, 0.0, 6.5)), size: Vec3::new(80.0, 40.0, 0.5), color: [0, 0, 0], ..Default::default() });
		let camera = CameraSpec::LookAt { eye: (origin - Vec3::Z * 45.0).into(), target: origin.into(), fov: 40.0 };
		for shadows in [false, true] {
			runs.push(Run { label: format!("y{height}-shadows{}", u8::from(shadows)), environment: Environment { global_shadows: shadows, ..sky.clone() }, camera: camera.clone() });
		}
	}
	Experiment { name: "ibl-ground".into(), model, probes: Vec::new(), runs }
}

/// The catalog cap's dark crown reads 0.4-1.1 of 255 brighter in ror in green and blue (its sky
/// specular 2-3 % over Studio's, the sun's term 1-2 %): the cap (its asset file, a Plastic block with
/// a textured FileMesh) in tools/parity.py's iso view, with a dark and a white Plastic ball and a dark
/// SmoothPlastic one beside it as far from the camera; under the sky specular alone at 0 and +2 EV
/// (the tone curve's toe against the reflection), the sun alone and the default lighting.
pub fn cap_terms(path: &std::path::Path) -> Result<Experiment> {
	let document = crate::scene::load::read_file(path)?;
	let center = crate::scene::extract::extract(&document).parts.first().map(|part| Vec3::from(part.cframe.translation)).unwrap_or(Vec3::ZERO);
	let mut experiment = decompose(path)?;
	experiment.name = "cap-terms".into();
	// tools/parity.py's catalog-cap iso view (its report's camera)
	let eye = Vec3::new(1.627_135_3, 7.053_428, -1.192_503);
	let look = Vec3::new(-0.577_353, -0.577_344_7, 0.577_353);
	let right = look.cross(Vec3::Y).normalize();
	let up = right.cross(look).normalize();
	let distance = (center - eye).length();
	for (name, offset, color, material) in [("dark-plastic", right * 2.4, 70u8, 256u32), ("white-plastic", -right * 2.4, 255, 256), ("dark-smooth", -up * 2.2, 70, 272)] {
		// as far from the eye as the cap
		let position = eye + (center + offset - eye).normalize() * distance;
		experiment.model.part(&PartSpec { name: name.into(), cframe: glam::Affine3A::from_translation(position), size: Vec3::splat(1.6), color: [color; 3], material, shape: Some(0), cast_shadow: false, ..Default::default() });
	}
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov: 70.0 };
	let off = Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..Environment::default() };
	let sky = Environment { environment_specular_scale: 1.0, ..off.clone() };
	experiment.runs = vec![
		Run { label: "sky-ev0".into(), environment: sky.clone(), camera: camera.clone() },
		Run { label: "sky-ev2".into(), environment: Environment { exposure_compensation: 2.0, ..sky }, camera: camera.clone() },
		Run { label: "sun-ev0".into(), environment: Environment { brightness: 2.0, ..off }, camera: camera.clone() },
		Run { label: "all-ev0".into(), environment: Environment::default(), camera },
	];
	Ok(experiment)
}

/// A fresh capture of a view the cache already holds: the default lighting with FogEnd 100001 (a new
/// capture key, the same image within the fog's 10⁻⁵), to tell Studio's state then from now
/// (`sky-context`: a ship close-up's capture from another session reflects a warm glow towards
/// the sun that today's captures do not).
pub fn fresh_view(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> Result<Experiment> {
	let mut experiment = decompose(path)?;
	experiment.name = "fresh-view".into();
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov };
	experiment.runs = vec![Run { label: "all".into(), environment: Environment { fog_end: 100001.0, ..Environment::default() }, camera }];
	Ok(experiment)
}

/// `decompose-view` captured afresh (FogEnd 100001, new capture keys; the same image within the fog's
/// 10⁻⁵): its cached captures of a ship close-up are another session's, which caught the
/// ForceField barrier's band and a warm glow in the sky specular, and its "all" differs from the
/// parity capture on 46k pixels.
pub fn fresh_decompose(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> Result<Experiment> {
	let mut experiment = decompose_view(path, eye, look, fov)?;
	experiment.name = "fresh-decompose".into();
	for run in &mut experiment.runs {
		run.environment.fog_end = 100001.0;
	}
	// every light off: what glows by itself (Neon, ForceField) and its bloom, in every other run too
	let camera = experiment.runs[0].camera.clone();
	let off = Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, fog_end: 100001.0, ..Environment::default() };
	experiment.runs.push(Run { label: "off".into(), environment: off, camera });
	Ok(experiment)
}

/// A view experiment's spec, `<name>:<eye x,y,z,look x,y,z,fov>:<path>`: eye, look, fov and path.
fn view_spec<'a>(spec: &'a str, name: &str) -> Result<(Vec3, Vec3, f32, &'a std::path::Path)> {
	let (camera, path) = spec[name.len() + 1..].split_once(':').with_context(|| format!("{name}:<eye x,y,z,look x,y,z,fov>:<path>"))?;
	let numbers: Vec<f32> = camera.split(',').map(|value| value.trim().parse::<f32>()).collect::<Result<_, _>>().with_context(|| format!("{name}: the camera's seven numbers"))?;
	if numbers.len() != 7 {
		bail!("{name}: the camera needs eye x,y,z, look x,y,z and fov");
	}
	Ok((Vec3::new(numbers[0], numbers[1], numbers[2]), Vec3::new(numbers[3], numbers[4], numbers[5]), numbers[6], std::path::Path::new(path)))
}

