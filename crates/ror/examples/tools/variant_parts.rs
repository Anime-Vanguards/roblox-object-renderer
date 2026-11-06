// Parts drawn with a MaterialVariant: path, variant, base material, colour, size and position, and the
// variant's maps (what the scene resolves for each part), optionally under a selection:
//   cargo run -p ror --release --example tools -- variant_parts <file> [select] [variant name filter]
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("usage: variant_parts <file> [select] [filter]");
	let mut document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	if let Some(select) = args.next().filter(|select| !select.is_empty()) {
		document.select(&select)?;
	}
	let filter = args.next().unwrap_or_default();
	let scene = ror::scene::extract::extract(&document);
	let mut count = 0;
	for (index, part) in scene.parts.iter().enumerate() {
		let Some(variant) = scene.materials.variant(part.material, &part.material_variant) else {
			continue;
		};
		if !variant.name.contains(filter.as_str()) {
			continue;
		}
		count += 1;
		let position = part.cframe.translation;
		println!(
			"#{index} {} | {} on {} | colour {:?} size {:?} at ({:.1}, {:.1}, {:.1}) | spt {} pattern {} maps {:?} {:?} {:?} {:?} {:?} pack {:?}",
			part.path,
			variant.name,
			part.material,
			(part.color * 255.0).round().to_array(),
			part.size.to_array(),
			position.x,
			position.y,
			position.z,
			variant.studs_per_tile,
			variant.pattern,
			variant.color_map,
			variant.normal_map,
			variant.metalness_map,
			variant.roughness_map,
			variant.emissive_mask,
			variant.texture_pack
		);
	}
	println!("{count} parts with a variant of {}", scene.parts.len());
	Ok(())
}
