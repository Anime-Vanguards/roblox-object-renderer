// Decodes images to PNG (level 0, RGBA8): cargo run -p ror --example tools -- decode_image <out dir> <file>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	for path in args {
		let bytes = std::fs::read(&path)?;
		let image = ror_image::decode(&bytes).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
		let rgba = ror_image::to_rgba8(&image);
		let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
		let target = out.join(format!("{name}.png"));
		ror::output::write_png(&target, rgba.width, rgba.height, &rgba.levels[0], true)?;
		println!("{} {}x{} {:?} levels {} dxt5nm {} srgb {:?} -> {}", path, image.width, image.height, image.format, image.levels.len(), image.dxt5nm, image.srgb, target.display());
	}
	Ok(())
}
