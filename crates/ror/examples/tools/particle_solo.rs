// Renders a model once per ParticleEmitter with only that emitter (to see which one draws what):
//   cargo run -p ror --release --example tools -- particle_solo <model> <out dir> [filter] [camera JSON]
use std::sync::Arc;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("model");
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	let filter = args.next().unwrap_or_default();
	let camera: ror::render::camera::CameraSpec = match args.next() {
		Some(json) => serde_json::from_str(&json)?,
		None => ror::render::camera::CameraSpec::Orbit { yaw: 30.0, pitch: 20.0, distance: None, padding: 0.08, target: None, fov: 50.0 },
	};
	let document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let scene = ror::scene::extract::extract(&document);
	let gpu = Arc::new(ror::render::gpu::Gpu::new()?);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = ror::render::Renderer::new(gpu, assets)?;
	std::fs::create_dir_all(&out)?;
	let request = ror::render::ViewRequest { width: 822, height: 558, camera, ..Default::default() };
	for (index, emitter) in scene.particles.iter().enumerate() {
		if !emitter.path.contains(&filter) {
			continue;
		}
		let mut solo = scene.clone();
		solo.particles = vec![emitter.clone()];
		let (images, _) = renderer.render(&solo, std::slice::from_ref(&request))?;
		let name: String = emitter.path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
		let path = out.join(format!("{index:02}-{name}.png"));
		ror::output::write_png(&path, images[0].width, images[0].height, &images[0].rgba, true)?;
		println!("{}", path.display());
	}
	Ok(())
}
