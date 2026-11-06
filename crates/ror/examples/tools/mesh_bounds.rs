// Prints a mesh asset's vertex bounds (and its size and centre) by content URI:
//   cargo run -p ror --release --example tools -- mesh_bounds <uri>...
pub fn main() -> anyhow::Result<()> {
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in std::env::args().skip(1) {
		let fetched = assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let mesh = ror_mesh::decode_any(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let mut low = [f32::MAX; 3];
		let mut high = [f32::MIN; 3];
		for position in &mesh.positions {
			for axis in 0..3 {
				low[axis] = low[axis].min(position[axis]);
				high[axis] = high[axis].max(position[axis]);
			}
		}
		let size: Vec<f32> = (0..3).map(|axis| high[axis] - low[axis]).collect();
		let centre: Vec<f32> = (0..3).map(|axis| (high[axis] + low[axis]) * 0.5).collect();
		println!("{uri}: {} vertices, min {low:?} max {high:?} size {size:?} centre {centre:?}", mesh.positions.len());
	}
	Ok(())
}
