// Checks that `render::downloads` lists what a render fetches: warms an empty state dir with it
// (assetdelivery's batch endpoint), then renders offline from that dir and prints what was missing.
//   cargo run -p ror --release --example tools -- warm_check <model.rbxm> <empty state dir> [downloads at once]
use std::sync::Arc;
use std::time::Instant;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = std::path::PathBuf::from(args.next().expect("model"));
	let state = std::path::PathBuf::from(args.next().expect("state dir"));
	let concurrent: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(48);
	let document = ror::scene::load::read_file(&path)?;
	let scene = ror::scene::extract::extract(&document);
	let downloads = ror::render::downloads(&scene);
	println!("{} parts, {} downloads", scene.parts.len(), downloads.len());
	let config = |offline: bool| ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), offline, max_concurrent_downloads: concurrent, ..ror_assets::AssetConfig::new(&state) };
	let online = ror_assets::Assets::new(config(false));
	let start = Instant::now();
	let report = online.warm(&downloads, &ror::render::transcode_packs);
	println!("warm {:.2} s: {report:?}", start.elapsed().as_secs_f64());
	let renderer = ror::render::Renderer::new(Arc::new(ror::render::gpu::Gpu::new()?), ror_assets::Assets::new(config(true)))?;
	let (_, stats) = renderer.render(&scene, &[ror::render::ViewRequest { width: 256, height: 256, ..Default::default() }])?;
	println!("offline render: {} missing", stats.missing.len());
	for missing in &stats.missing {
		println!("  {missing}");
	}
	Ok(())
}
