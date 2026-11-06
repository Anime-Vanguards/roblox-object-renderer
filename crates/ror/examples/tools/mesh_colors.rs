// Prints a mesh's vertex colours (distinct values and how many vertices carry each) by content URI:
//   cargo run -p ror --release --example tools -- mesh_colors <uri>...
use std::collections::BTreeMap;

pub fn main() -> anyhow::Result<()> {
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in std::env::args().skip(1) {
		let fetched = assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let mesh = ror_mesh::decode_mesh(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let mut counts: BTreeMap<[u8; 4], usize> = BTreeMap::new();
		for color in &mesh.colors {
			*counts.entry(*color).or_default() += 1;
		}
		println!("{uri}: {:?}, {} vertices, {} with colours, {} distinct", mesh.format, mesh.positions.len(), mesh.colors.len(), counts.len());
		let mut rows: Vec<_> = counts.into_iter().collect();
		rows.sort_by(|a, b| b.1.cmp(&a.1));
		for (color, count) in rows.iter().take(12) {
			println!("  {color:?} x {count}");
		}
	}
	Ok(())
}
