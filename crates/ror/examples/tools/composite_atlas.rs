// Writes the composited textures (body colours and clothing) of every dressed character in a model,
// one PNG per distinct texture, named after the first body part using it:
//   cargo run -p ror --release --example tools -- composite_atlas <model> <out dir>
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("model file");
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	let document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let scene = ror::scene::extract::extract(&document);
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let mut seen = std::collections::HashSet::new();
	for part in &scene.parts {
		let Some(composite) = &part.composite else {
			continue;
		};
		if !seen.insert(ror::render::composite::key(composite)) {
			continue;
		}
		let start = std::time::Instant::now();
		let canvas = ror::render::composite::canvas(&assets, composite)?;
		let elapsed = start.elapsed().as_secs_f64() * 1000.0;
		let name: String = part.path.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
		let target = out.join(format!("{name}.png"));
		ror::output::write_png(&target, canvas.width, canvas.height, &canvas.to_rgba8(), true)?;
		println!("{} {:?} {}x{} in {elapsed:.1} ms -> {}", part.path, composite.layout, canvas.width, canvas.height, target.display());
	}
	Ok(())
}
