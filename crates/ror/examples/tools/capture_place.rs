// Writes the capture place: cargo run -p ror --example tools -- capture_place <out.rbxl>
pub fn main() -> anyhow::Result<()> {
	let path = std::env::args().nth(1).expect("usage: capture_place <out.rbxl>");
	let bytes = ror::studio::place::build(&ror::scene::Environment::default(), true)?;
	std::fs::write(&path, bytes)?;
	println!("wrote {path}");
	Ok(())
}
