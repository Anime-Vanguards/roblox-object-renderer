// Writes instances of a model or place (dotted paths, as `--select` takes them) into a model file of
// their own, e.g. one character of a place, to compare it under the default lighting:
//   cargo run -p ror --release --example tools -- extract_model <in> <out.rbxm> <path>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("input file");
	let output = args.next().expect("output .rbxm");
	let mut document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let mut roots = Vec::new();
	for path in args {
		document.select(&path)?;
		roots.extend(document.roots.iter().copied());
	}
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &roots).map_err(|error| anyhow::anyhow!("{error}"))?;
	ror::output::write_atomic(std::path::Path::new(&output), &bytes)?;
	println!("{output}: {} instances, {} bytes", roots.len(), bytes.len());
	Ok(())
}
