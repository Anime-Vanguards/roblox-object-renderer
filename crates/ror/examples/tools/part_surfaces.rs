// Prints the surface types (Right, Top, Back, Left, Bottom, Front) and MaterialVariants of a model's or
// place's parts that have any, optionally under a selection:
//   cargo run -p ror --release --example tools -- part_surfaces <file> [select] [max]
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("usage: part_surfaces <file> [select] [max]");
	let mut document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	if let Some(select) = args.next().filter(|select| !select.is_empty()) {
		document.select(&select)?;
	}
	let max: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(50);
	let scene = ror::scene::extract::extract(&document);
	let mut shown = 0;
	for part in &scene.parts {
		if part.surfaces.iter().all(|&surface| surface == 0) && part.material_variant.is_empty() {
			continue;
		}
		println!("{} [{}] material {} variant {:?} size {:?} surfaces {:?}", part.path, part.class, part.material, part.material_variant, part.size.to_array(), part.surfaces);
		shown += 1;
		if shown >= max {
			break;
		}
	}
	println!("{shown} parts with surface types shown, of {}", scene.parts.len());
	Ok(())
}
