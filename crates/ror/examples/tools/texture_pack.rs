// Fetches texture packs (the XML assets behind SurfaceAppearance.TexturePack and the built-in
// material textures) and decodes their maps to PNGs no larger than 256 px (`--full`: full size):
// cargo run -p ror --release --example tools -- texture_pack <out dir> [--full] <asset id>...
use std::path::PathBuf;

use ror_assets::{AssetConfig, Assets};

fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
	let open = format!("<{name}>");
	let start = text.find(&open)? + open.len();
	let end = text[start..].find('<')? + start;
	Some(text[start..end].trim())
}

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let assets = Assets::new(AssetConfig { repo_dir: Some(std::env::current_dir()?), ..AssetConfig::default() });
	let mut full = false;
	for id in args {
		if id == "--full" {
			full = true;
			continue;
		}
		let pack = match assets.fetch(&format!("rbxassetid://{id}")) {
			Ok(pack) => pack,
			Err(error) => {
				println!("{id}: {error}");
				continue;
			}
		};
		let text = String::from_utf8_lossy(&pack.bytes).to_string();
		let fields: Vec<String> = ["usage", "alphamode", "tiling"].iter().filter_map(|name| tag(&text, name).map(|value| format!("{name}={value}"))).collect();
		println!("{id}: {} {}", pack.kind(), fields.join(" "));
		for name in ["color", "normal", "metalness", "roughness", "emissive"] {
			let Some(map_id) = tag(&text, name) else {
				continue;
			};
			let map = match assets.fetch(&format!("rbxassetid://{map_id}")) {
				Ok(map) => map,
				Err(error) => {
					println!("  {name} {map_id}: {error}");
					continue;
				}
			};
			let image = match ror_image::decode(&map.bytes) {
				Ok(image) => image,
				Err(error) => {
					println!("  {name} {map_id}: {} {error}", map.kind());
					continue;
				}
			};
			let rgba = ror_image::to_rgba8(&image);
			let level = if full { 0 } else { (0..rgba.levels.len()).find(|&level| (rgba.width >> level).max(1) <= 256).unwrap_or(rgba.levels.len() - 1) };
			let (width, height) = ((rgba.width >> level).max(1), (rgba.height >> level).max(1));
			let target = out.join(format!("{id}_{name}{}.png", if full { "_full" } else { "" }));
			ror::output::write_png(&target, width, height, &rgba.levels[level], true)?;
			println!(
				"  {name} {map_id}: {} {}x{} {:?} levels {} dxt5nm {} srgb {:?}",
				map.kind(),
				image.width,
				image.height,
				image.format,
				image.levels.len(),
				image.dxt5nm,
				image.srgb
			);
		}
	}
	Ok(())
}
