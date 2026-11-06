// Every mip level of image files as RGBA8 PNGs, <out dir>/<name>_L<level>.png (e.g. Roblox's KTX2
// transcodes against ror's own mips: notes/materials.md "Transcodes").
//
//     cargo run --release -p ror --example tools -- image_levels <out dir> <file>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	for path in args {
		let bytes = std::fs::read(&path)?;
		let image = ror_image::decode(&bytes).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
		let rgba = ror_image::to_rgba8(&image);
		let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().to_string();
		for (level, data) in rgba.levels.iter().enumerate() {
			let width = (rgba.width >> level).max(1);
			let height = (rgba.height >> level).max(1);
			let bytes = (width * height * 4) as usize;
			if data.len() < bytes {
				break;
			}
			ror::output::write_png(&out.join(format!("{name}_L{level}.png")), width, height, &data[..bytes], true)?;
		}
		println!("{path}: {}x{}, {} levels, dxt5nm {}", image.width, image.height, rgba.levels.len(), image.dxt5nm);
	}
	Ok(())
}
