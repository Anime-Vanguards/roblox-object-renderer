// Prints a mesh's vertices as CSV (position, normal, uv as the renderer sees them: top-left origin):
//   cargo run -p ror --release --example tools -- mesh_dump <file.mesh> > vertices.csv
pub fn main() -> anyhow::Result<()> {
	let path = std::env::args().nth(1).expect("mesh file");
	let bytes = std::fs::read(&path)?;
	let mesh = ror_mesh::decode_mesh(&bytes).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
	let flip = mesh.uv_v_flipped();
	println!("x,y,z,nx,ny,nz,u,v");
	for index in 0..mesh.vertex_count() {
		let p = mesh.positions[index];
		let n = mesh.normals.get(index).copied().unwrap_or([0.0; 3]);
		let mut uv = mesh.uvs.get(index).copied().unwrap_or([0.0; 2]);
		if flip {
			uv[1] = 1.0 - uv[1];
		}
		println!("{},{},{},{},{},{},{},{}", p[0], p[1], p[2], n[0], n[1], n[2], uv[0], uv[1]);
	}
	Ok(())
}
