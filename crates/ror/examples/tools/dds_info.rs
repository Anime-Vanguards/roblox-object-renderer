// Prints the format, size, levels and flags of images by content URI:
//   cargo run -p ror --release --example tools -- dds_info <uri>...
pub fn main() -> anyhow::Result<()> {
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in std::env::args().skip(1) {
		let fetched = assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		println!("{uri}: {:?} {}x{} levels {} has_alpha {} dxt5nm {} srgb {:?}", image.format, image.width, image.height, image.levels.len(), image.has_alpha, image.dxt5nm, image.srgb);
	}
	Ok(())
}
