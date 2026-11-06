// Prints the extracted scene's parts (index as in `ror render --ids`' parts.json) with their
// geometry, material and SurfaceAppearance:
//   cargo run -p ror --release --example tools -- scene_parts <file> [index ...]
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let wanted: Vec<usize> = args.filter_map(|value| value.parse().ok()).collect();
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let scene = ror::scene::extract::extract(&document);
	for (index, part) in scene.parts.iter().enumerate() {
		if !wanted.is_empty() && !wanted.contains(&index) {
			continue;
		}
		println!("#{index} {} {} size {:?} material {} colour {:?} transparency {} reflectance {}", part.path, part.class, part.size, part.material, part.color, part.transparency, part.reflectance);
		println!("   geometry {:?}", part.geometry);
		if let Some(appearance) = &part.surface_appearance {
			println!("   SurfaceAppearance {appearance:?}");
		}
		if let Some(texture) = &part.texture {
			println!("   texture {texture}");
		}
	}
	Ok(())
}
