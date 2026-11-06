// Lists the MeshParts of model / place files whose meshes carry non-white vertex colours (with their
// material and whether they have a SurfaceAppearance or TextureID):
//   cargo run -p ror --release --example tools -- vertex_color_scan <file> ...
pub fn main() -> anyhow::Result<()> {
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let (mut total, mut coloured) = (0usize, 0usize);
	for path in std::env::args().skip(1) {
		let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
		let scene = ror::scene::extract::extract(&document);
		for part in &scene.parts {
			let ror::scene::Geometry::MeshPart { mesh, .. } = &part.geometry else {
				continue;
			};
			total += 1;
			let Ok(fetched) = assets.fetch(mesh) else {
				continue;
			};
			let Ok(decoded) = ror_mesh::decode_mesh(&fetched.bytes) else {
				continue;
			};
			let tinted = decoded.colors.iter().filter(|color| color[0] < 250 || color[1] < 250 || color[2] < 250).count();
			if tinted == 0 {
				continue;
			}
			coloured += 1;
			println!("{path}: {} material {} sa {} texture {} — {tinted} of {} vertices tinted", part.path, part.material, part.surface_appearance.is_some(), part.texture.is_some(), decoded.colors.len());
		}
	}
	println!("{coloured} of {total} MeshParts have tinted vertex colours");
	Ok(())
}
