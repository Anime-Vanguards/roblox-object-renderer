// Renders a timeline run of `ror studio calibrate` (out/calibrate/<experiment>/<run>) once without
// its effects and once per group of them (the emitters sharing a name and texture, the trails, the
// beams), with the run's camera and Studio's frame times, at a share of the capture's size; and
// writes groups.json (each group's members, particles drawn and properties) beside them, for
// tools/analysis/vfx_attribution.py (which group draws what, and how Studio's image weighs it):
//   cargo run -p ror --release --example tools -- vfx_groups <run dir> <out dir> [scale]

#[cfg(windows)]
pub fn main() -> anyhow::Result<()> {
	use std::collections::BTreeMap;
	use std::sync::Arc;

	use anyhow::Context;
	use ror::render::camera::CameraSpec;
	use ror::render::{Background, Renderer, ViewRequest};
	use ror::studio::calibrate::timeline::{Captured, Timeline, scene};

	let mut args = std::env::args().skip(1);
	let run = std::path::PathBuf::from(args.next().expect("run dir"));
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	let scale: f32 = args.next().and_then(|value| value.parse().ok()).unwrap_or(0.5);
	let experiment = run.parent().context("the run's experiment dir")?;
	let label = run.file_name().context("the run's label")?.to_string_lossy().to_string();
	let bytes = std::fs::read(experiment.join("model.rbxm"))?;
	let playback = ror::studio::calibrate::timeline::stored_playback(&run.join("playback.json"))?;
	let captured: Captured = serde_json::from_str(&std::fs::read_to_string(run.join("studio.json"))?)?;
	let samples: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(experiment.join("samples.json"))?)?;
	let record = samples.as_array().and_then(|runs| runs.iter().find(|entry| entry["label"] == label.as_str())).context("the run in samples.json")?;
	let components: Vec<f32> = record["camera"].as_array().context("camera")?.iter().filter_map(|value| value.as_f64()).map(|value| value as f32).collect();
	let camera = CameraSpec::CFrame { components: components.try_into().map_err(|_| anyhow::anyhow!("12 camera numbers"))?, fov: record["fov"].as_f64().unwrap_or(50.0) as f32, near: None };
	let full = scene(&bytes, &Timeline { playback, duration: 0.0 }, &captured)?;
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = Renderer::new(Arc::new(ror::render::gpu::Gpu::new()?), assets)?;
	let request = ViewRequest { width: (1644.0 * scale).round() as u32, height: (1116.0 * scale).round() as u32, camera, background: Background::Sky, ..Default::default() };
	std::fs::create_dir_all(&out)?;
	let render = |scene: &ror::scene::Scene, name: &str| -> anyhow::Result<()> {
		let (images, _) = renderer.render(scene, std::slice::from_ref(&request))?;
		let image = images.into_iter().next().context("no image")?;
		ror::output::write_png(&out.join(format!("{name}.png")), image.width, image.height, &image.rgba, true)
	};
	let mut bare = full.clone();
	bare.particles.clear();
	bare.trails.clear();
	bare.beams.clear();
	render(&bare, "bare")?;
	render(&full, "all")?;
	// the same particles from other random streams (each emitter's seed changed): how far apart two
	// draws of one effect are, the control Studio's image is weighed against
	let mut other = full.clone();
	for emitter in &mut other.particles {
		emitter.seed ^= 0x9E37_79B9_7F4A_7C15;
	}
	render(&other, "alt")?;
	// emitters sharing a name and texture are one group
	let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
	for (index, emitter) in full.particles.iter().enumerate() {
		let name = emitter.path.rsplit('.').next().unwrap_or_default().to_string();
		groups.entry((name, emitter.texture.clone().unwrap_or_default())).or_default().push(index);
	}
	let mut listing = Vec::new();
	for (number, ((name, texture), members)) in groups.into_iter().enumerate() {
		let mut solo = bare.clone();
		solo.particles = members.iter().map(|&index| full.particles[index].clone()).collect();
		let particles: usize = solo.particles.iter().map(|emitter| ror::render::particles::simulate(emitter).len()).sum();
		let file = format!("g{number:03}");
		if particles > 0 {
			render(&solo, &file)?;
		}
		let e = &full.particles[members[0]];
		let max_size = e.size_keys.iter().map(|key| key.1 + key.2.abs()).fold(0.0f32, f32::max);
		let squash = e.squash.iter().map(|key| key.1.abs() + key.2.abs()).fold(0.0f32, f32::max);
		let transparency = e.transparency.iter().map(|key| key.1).fold(1.0f32, f32::min);
		listing.push(serde_json::json!({
			"file": file, "kind": "emitters", "name": name, "texture": texture, "members": members.iter().map(|&index| full.particles[index].path.clone()).collect::<Vec<_>>(),
			"particles": particles,
			"props": {
				"brightness": e.brightness, "lightEmission": e.light_emission, "lightInfluence": e.light_influence, "orientation": e.orientation,
				"flipbookLayout": e.flipbook_layout, "flipbookMode": e.flipbook_mode, "flipbookStartRandom": e.flipbook_start_random, "flipbookFramerate": [e.flipbook_framerate.0, e.flipbook_framerate.1],
				"zOffset": e.z_offset, "drag": e.drag, "acceleration": e.acceleration.length(), "speed": [e.speed.0, e.speed.1], "lifetime": [e.lifetime.0, e.lifetime.1],
				"maxSize": max_size, "squash": squash, "minTransparency": transparency, "shape": e.shape, "shapeStyle": e.shape_style, "shapeInOut": e.shape_in_out,
				"attachment": e.attachment, "lockedToPart": e.locked_to_part, "timeScale": e.time_scale, "rotSpeed": [e.rot_speed.0, e.rot_speed.1],
				"spread": [e.spread_angle.x, e.spread_angle.y], "rate": e.rate, "enabled": e.enabled, "velocityInheritance": e.velocity_inheritance,
				"colorVaries": e.color.windows(2).any(|pair| (pair[0].1 - pair[1].1).abs().max_element() > 1e-3),
				"color": e.color.first().map(|key| key.1.to_array()).unwrap_or([1.0; 3]),
			},
		}));
	}
	for (kind, file) in [("trails", "trails"), ("beams", "beams")] {
		let mut solo = bare.clone();
		match kind {
			"trails" => solo.trails = full.trails.clone(),
			_ => solo.beams = full.beams.clone(),
		}
		let count = if kind == "trails" { full.trails.len() } else { full.beams.len() };
		if count > 0 {
			render(&solo, file)?;
			listing.push(serde_json::json!({ "file": file, "kind": kind, "name": kind, "members": count, "particles": 0, "props": {} }));
		}
	}
	ror::output::write_atomic(&out.join("groups.json"), serde_json::to_string_pretty(&listing)?.as_bytes())?;
	println!("{} groups -> {}", listing.len(), out.display());
	Ok(())
}

#[cfg(not(windows))]
pub fn main() {}
