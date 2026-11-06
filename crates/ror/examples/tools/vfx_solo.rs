// Renders a model at a playback's moment once per group of its effects (emitters, trails and beams
// whose path starts with each prefix), to see which draws what:
//   cargo run -p ror --release --example tools -- vfx_solo <model> <playback.json> <camera JSON> <out dir> <prefix>...
use std::sync::Arc;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let model = args.next().expect("model");
	let playback: ror::scene::Playback = serde_json::from_str(&std::fs::read_to_string(args.next().expect("playback"))?)?;
	let camera: ror::render::camera::CameraSpec = serde_json::from_str(&args.next().expect("camera JSON"))?;
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	let prefixes: Vec<String> = args.collect();
	let mut document = ror::scene::load::read_file(std::path::Path::new(&model))?;
	let resolved = playback.resolve(&document)?;
	resolved.apply_state(&mut document)?;
	let mut scene = ror::scene::extract::extract(&document);
	resolved.apply_history(&document, &mut scene);
	let gpu = Arc::new(ror::render::gpu::Gpu::new()?);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = ror::render::Renderer::new(gpu, assets)?;
	let request = ror::render::ViewRequest { width: 822, height: 558, camera, ..Default::default() };
	for prefix in prefixes.iter().map(String::as_str).chain(["all"]) {
		let mut solo = scene.clone();
		if prefix != "all" {
			solo.particles.retain(|emitter| emitter.path.starts_with(prefix));
			solo.trails.retain(|trail| trail.path.starts_with(prefix));
			solo.beams.retain(|beam| beam.path.starts_with(prefix));
		}
		let (images, _) = renderer.render(&solo, std::slice::from_ref(&request))?;
		let image = images.into_iter().next().expect("image");
		let name: String = prefix.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
		ror::output::write_png(&out.join(format!("{name}.png")), image.width, image.height, &image.rgba, true)?;
		println!("{prefix}: {} emitters, {} trails, {} beams", solo.particles.len(), solo.trails.len(), solo.beams.len());
	}
	Ok(())
}
