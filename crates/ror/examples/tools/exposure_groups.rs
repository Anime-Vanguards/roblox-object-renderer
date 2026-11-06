// Draws ror's side of a long exposure (`ror studio calibrate exposure:...`, its run dir
// out/calibrate/<experiment>/exposure) by groups of its effects: the emitters sharing a name and
// texture, the trails, the beams, each alone over the scene without the effect, averaged in linear
// light over the whole exposure (every `frames`-th of Studio's frames in every `step`-th play, at
// `scale` of the capture's size; one sum per group, not per window: an effect's groups by window held
// 3 GB). `by` is `name` (the emitters sharing a name and texture) or `parent` (those on one attachment
// or part: an effect's 70 emitters in 9 groups). Writes
// `groups/<group>-w0.png` (16-bit, as the exposure's), `groups/all-w0.png`, `groups/bare-w0.png` and
// groups.json (each group's members and frames), for tools/analysis/exposure_attribution.py:
//   cargo run -p ror --release --example tools -- exposure_groups <exposure dir> [scale] [step] [frames] [name|parent]

#[cfg(windows)]
pub fn main() -> anyhow::Result<()> {
	use std::collections::BTreeMap;
	use std::sync::Arc;

	use anyhow::Context;
	use ror::render::camera::CameraSpec;
	use ror::render::{Background, Renderer, ViewRequest};
	use ror::studio::calibrate::exposure::{Sums, levels};
	use ror::studio::calibrate::timeline::{Captured, Timeline, scene};

	let mut args = std::env::args().skip(1);
	let dir = std::path::PathBuf::from(args.next().context("the exposure's run dir")?);
	let scale: f32 = args.next().and_then(|value| value.parse().ok()).unwrap_or(0.5);
	let step: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(4).max(1);
	let frame_step: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(2).max(1);
	let by_parent = args.next().is_some_and(|by| by == "parent");
	let experiment = dir.parent().context("the exposure's experiment dir")?;
	let bytes = std::fs::read(experiment.join("model.rbxm"))?;
	let playback = ror::studio::calibrate::timeline::stored_playback(&dir.join("playback.json"))?;
	let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("exposure.json"))?)?;
	let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("plays.json"))?)?;
	let components: Vec<f32> = summary["camera"].as_array().context("camera")?.iter().filter_map(|value| value.as_f64()).map(|value| value as f32).collect();
	let camera = CameraSpec::CFrame { components: components.try_into().map_err(|_| anyhow::anyhow!("12 camera numbers"))?, fov: summary["fov"].as_f64().unwrap_or(50.0) as f32, near: None };
	let timeline = Timeline { playback, duration: summary["end"].as_f64().unwrap_or(1.0) as f32 };
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = Renderer::new(Arc::new(ror::render::gpu::Gpu::new()?), assets)?;
	let (width, height) = ((1644.0 * scale).round() as u32, (1116.0 * scale).round() as u32);
	let request = ViewRequest { width, height, camera, background: Background::Sky, ..Default::default() };
	let pixels = (width * height) as usize;
	let levels = levels();
	let out = dir.join("groups");
	std::fs::create_dir_all(&out)?;
	// the groups from the scene at the exposure's end: emitters sharing a name and texture, the trails,
	// the beams
	let last = scene(&bytes, &timeline, &Captured { time: timeline.duration, frames: Vec::new() })?;
	let mut emitters: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
	for emitter in &last.particles {
		let key = match by_parent {
			true => (emitter.path.rsplit_once('.').map(|(parent, _)| parent.to_string()).unwrap_or_default(), String::new()),
			false => (emitter.path.rsplit('.').next().unwrap_or_default().to_string(), emitter.texture.clone().unwrap_or_default()),
		};
		emitters.entry(key).or_default().push(emitter.path.clone());
	}
	let mut groups: Vec<(String, String, Vec<String>)> = emitters.into_iter().enumerate().map(|(number, ((name, texture), members))| (format!("g{number:02}"), if texture.is_empty() { name } else { format!("{name} ({texture})") }, members)).collect();
	if !last.trails.is_empty() {
		groups.push(("trails".into(), "trails".into(), Vec::new()));
	}
	if !last.beams.is_empty() {
		groups.push(("beams".into(), "beams".into(), Vec::new()));
	}
	let mut sums: Vec<Sums> = (0..groups.len() + 2).map(|_| Sums::new(1, pixels)).collect();
	let started = std::time::Instant::now();
	let mut drawn = 0;
	for play in record["plays"].as_array().context("plays")?.iter().step_by(step) {
		let seed = play["seed"].as_u64().unwrap_or_default();
		let times: Vec<f32> = play["times"].as_array().context("times")?.iter().filter_map(|value| value.as_f64()).map(|value| value as f32).collect();
		let indices = play["indices"].as_array().context("indices")?;
		for index in indices.iter().step_by(frame_step) {
			let index = index.as_u64().unwrap_or_default() as usize;
			let mut full = scene(&bytes, &timeline, &Captured { time: times[index], frames: times[..=index].to_vec() })?;
			for emitter in &mut full.particles {
				emitter.seed ^= seed;
			}
			let mut bare = full.clone();
			bare.particles.clear();
			bare.trails.clear();
			bare.beams.clear();
			let mut draw = |scene: &ror::scene::Scene, slot: usize| -> anyhow::Result<()> {
				let (images, _) = renderer.render(scene, std::slice::from_ref(&request))?;
				sums[slot].add(0, &images.into_iter().next().context("no image")?.rgba, &levels);
				Ok(())
			};
			draw(&full, 0)?;
			draw(&bare, 1)?;
			for (slot, (_, label, members)) in groups.iter().enumerate() {
				let mut solo = bare.clone();
				match label.as_str() {
					"trails" => solo.trails = full.trails.clone(),
					"beams" => solo.beams = full.beams.clone(),
					_ => solo.particles = full.particles.iter().filter(|emitter| members.contains(&emitter.path)).cloned().collect(),
				}
				draw(&solo, slot + 2)?;
			}
			drawn += 1;
			if drawn % 100 == 0 {
				eprintln!("{drawn} frames, {} groups, {:.0} s", groups.len(), started.elapsed().as_secs_f32());
			}
		}
	}
	eprintln!("{drawn} frames, {} groups, in {:.1} s", groups.len(), started.elapsed().as_secs_f32());
	let mut listing = Vec::new();
	for (slot, sums) in sums.iter().enumerate() {
		let file = match slot {
			0 => "all".to_string(),
			1 => "bare".to_string(),
			_ => groups[slot - 2].0.clone(),
		};
		let counts = sums.write(&out, &file, width, height)?;
		let (label, members) = match slot {
			0 | 1 => (file.clone(), Vec::new()),
			_ => (groups[slot - 2].1.clone(), groups[slot - 2].2.clone()),
		};
		listing.push(serde_json::json!({ "file": file, "label": label, "members": members, "frames": counts }));
	}
	ror::output::write_atomic(&out.join("groups.json"), serde_json::to_string_pretty(&serde_json::json!({ "scale": scale, "step": step, "width": width, "height": height, "groups": listing }))?.as_bytes())?;
	println!("{} groups -> {}", groups.len(), out.display());
	Ok(())
}

#[cfg(not(windows))]
pub fn main() {}
