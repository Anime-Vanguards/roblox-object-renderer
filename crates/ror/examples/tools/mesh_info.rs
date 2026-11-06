// Prints a mesh file's format, counts and LOD0 bounds: cargo run -p ror --example tools -- mesh_info <file.mesh>...
pub fn main() -> anyhow::Result<()> {
	for path in std::env::args().skip(1) {
		let bytes = std::fs::read(&path)?;
		let mesh = ror_mesh::decode_mesh(&bytes).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
		let (min, max) = mesh.lod0_bounds().unwrap_or(([0.0; 3], [0.0; 3]));
		println!(
			"{path}: {:?} vertices {} triangles(lod0) {} lods {} bounds min {:?} max {:?} size {:?}",
			mesh.format,
			mesh.vertex_count(),
			mesh.lod0().len() / 3,
			mesh.lods.len(),
			min,
			max,
			[max[0] - min[0], max[1] - min[1], max[2] - min[2]]
		);
	}
	Ok(())
}
