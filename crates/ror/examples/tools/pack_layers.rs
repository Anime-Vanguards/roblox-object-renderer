// A TexturePack's transcoded layers next to its maps (notes/image.md "Texture transcodes",
// notes/materials.md "Transcodes"): fetches the pack, its colour, normal and third layer (every pack
// file, joined) and the maps it lists, and writes level 0 of each with the stored values as RGBA8
// PNGs (`<out>/<pack>_<layer>.png`, `<out>/<pack>_<map>.png`; `--levels`: every level of the layers,
// `_L<level>`) and the layers' KTX2 keys to `<out>/<pack>.json`.
//
//     cargo run --release -p ror --example tools -- pack_layers <out dir> [--levels] [--offline] <pack id>...
//
// Shares the asset cache (`ROR_STATE_DIR`, else this checkout's `.cache`).
use std::path::Path;

use anyhow::{Result, anyhow};
use ror_assets::{AssetConfig, Assets, Transcode};

fn layer(assets: &Assets, uri: &str, transcode: Transcode) -> Result<ror_image::Image> {
	let decode = |pack: u32| -> Result<ror_image::Image> {
		let fetched = assets.fetch_transcode(uri, transcode, pack).map_err(|error| anyhow!("{error}"))?;
		ror_image::decode(&fetched.bytes).map_err(|error| anyhow!("{uri}: pack {pack}: {error}"))
	};
	let first = decode(0)?;
	let packs = ror_image::ktx2_pack_table(&first).len() as u32;
	if packs == 0 {
		return Ok(first);
	}
	let mut files = vec![first];
	for pack in 1..packs {
		files.push(decode(pack)?);
	}
	ror_image::join_packs(files).map_err(|error| anyhow!("{uri}: {error}"))
}

fn write_levels(out: &Path, name: &str, image: &ror_image::Image, all: bool) -> Result<()> {
	let rgba = ror_image::to_rgba8(image);
	for (level, data) in rgba.levels.iter().enumerate() {
		let width = (rgba.width >> level).max(1);
		let height = (rgba.height >> level).max(1);
		let bytes = (width * height * 4) as usize;
		if data.len() < bytes {
			break;
		}
		let file = if level == 0 { format!("{name}.png") } else { format!("{name}_L{level}.png") };
		ror::output::write_png(&out.join(file), width, height, &data[..bytes], true)?;
		if !all {
			break;
		}
	}
	Ok(())
}

pub fn main() -> Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let (mut levels, mut offline) = (false, false);
	let mut ids = Vec::new();
	for arg in args {
		match arg.as_str() {
			"--levels" => levels = true,
			"--offline" => offline = true,
			_ => ids.push(arg),
		}
	}
	let assets = Assets::new(AssetConfig { state_dir: ror::output::state_dir(), repo_dir: Some(ror::output::repo_dir()), offline, ..AssetConfig::default() });
	for id in ids {
		let uri = format!("rbxassetid://{id}");
		let pack = match assets.fetch(&uri).map_err(|error| anyhow!("{error}")).and_then(|fetched| ror::render::texture_pack::parse(&fetched.bytes)) {
			Ok(pack) => pack,
			Err(error) => {
				println!("{id}: {error:#}");
				continue;
			}
		};
		println!("{id}: usage {} alphamode {} tiling {}", pack.usage, pack.alpha_mode, pack.tiling);
		let mut keys = serde_json::Map::new();
		for (name, transcode) in [("color", Transcode::PackColor), ("normal", Transcode::PackNormal), ("third", Transcode::PackMetalnessRoughness)] {
			match layer(&assets, &uri, transcode) {
				Ok(image) => {
					println!(
						"  {name}: {:?} {}x{} levels {} dxt5nm {} alpha {} {}",
						image.format,
						image.width,
						image.height,
						image.levels.len(),
						image.dxt5nm,
						image.has_alpha,
						image.metadata.iter().filter(|(key, _)| !key.starts_with("pack")).map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(" ")
					);
					keys.insert(name.into(), serde_json::json!({
						"format": format!("{:?}", image.format),
						"width": image.width,
						"height": image.height,
						"levels": image.levels.len(),
						"dxt5nm": image.dxt5nm,
						"hasAlpha": image.has_alpha,
						"metadata": image.metadata.iter().map(|(key, value)| (key.clone(), serde_json::Value::from(value.clone()))).collect::<serde_json::Map<String, serde_json::Value>>(),
					}));
					write_levels(&out, &format!("{id}_{name}"), &image, levels)?;
				}
				Err(error) => println!("  {name}: {error:#}"),
			}
		}
		for (name, map) in [("map_color", &pack.color), ("map_normal", &pack.normal), ("map_metalness", &pack.metalness), ("map_roughness", &pack.roughness), ("map_emissive", &pack.emissive)] {
			let Some(map) = map else {
				continue;
			};
			match assets.fetch(map).map_err(|error| anyhow!("{error}")).and_then(|fetched| ror_image::decode(&fetched.bytes).map_err(|error| anyhow!("{map}: {error}"))) {
				Ok(image) => {
					println!("  {name} {map}: {:?} {}x{}", image.format, image.width, image.height);
					keys.insert(name.into(), serde_json::json!({"uri": map, "width": image.width, "height": image.height}));
					write_levels(&out, &format!("{id}_{name}"), &image, false)?;
				}
				Err(error) => println!("  {name} {map}: {error:#}"),
			}
		}
		ror::output::write_atomic(&out.join(format!("{id}.json")), serde_json::to_string_pretty(&keys)?.as_bytes())?;
	}
	Ok(())
}
