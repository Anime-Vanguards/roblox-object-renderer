// Renders a model or place several times in one process and prints each render's timings (the
// first is cold, the rest warm), from its default view or a camera given as JSON:
//   cargo run -p ror --release --example tools -- bench_scene <file> [runs] [WIDTHxHEIGHT] [camera JSON]
use std::sync::Arc;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let runs: usize = args.next().map(|value| value.parse().unwrap_or(3)).unwrap_or(3);
	let size = args.next().unwrap_or_else(|| "1024x1024".into());
	let (width, height) = size.split_once('x').map(|(w, h)| (w.parse().unwrap_or(1024), h.parse().unwrap_or(1024))).unwrap_or((1024, 1024));
	let start = std::time::Instant::now();
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let scene = ror::scene::extract::extract(&document);
	println!("load + extract {:.1} ms, {} parts", start.elapsed().as_secs_f64() * 1000.0, scene.parts.len());
	let gpu = Arc::new(ror::render::gpu::Gpu::new()?);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let renderer = ror::render::Renderer::new(gpu, assets)?;
	let mut request = ror::render::ViewRequest { width, height, ..Default::default() };
	if let Some(camera) = args.next() {
		request.camera = serde_json::from_str(&camera)?;
	}
	for run in 0..runs {
		let start = std::time::Instant::now();
		let (_, stats) = renderer.render(&scene, std::slice::from_ref(&request))?;
		println!(
			"run {run}: total {:.1} ms (prepare {:.1}, assets {:.1}, draw {:.1}, readback {:.1}), draws {}, missing {}",
			start.elapsed().as_secs_f64() * 1000.0,
			stats.prepare_ms,
			stats.assets_ms,
			stats.draw_ms,
			stats.readback_ms,
			stats.draws,
			stats.missing.len()
		);
	}
	println!("caches: {}", renderer.cache_stats());
	Ok(())
}
