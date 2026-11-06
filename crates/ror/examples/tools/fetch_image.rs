// Fetches images by content URI (rbxassetid://, rbxasset://, ...) and writes their first level as PNG:
//   cargo run -p ror --release --example tools -- fetch_image <out dir> <uri>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in args {
		let fetched = assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error} ({} bytes, starting {:02x?})", fetched.bytes.len(), &fetched.bytes[..fetched.bytes.len().min(24)]))?;
		let rgba = ror_image::to_rgba8(&image);
		let name: String = uri.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
		let target = out.join(format!("{name}.png"));
		ror::output::write_png(&target, rgba.width, rgba.height, &rgba.levels[0], true)?;
		println!("{uri}: {} {}x{} -> {}", fetched.kind(), image.width, image.height, target.display());
	}
	Ok(())
}
