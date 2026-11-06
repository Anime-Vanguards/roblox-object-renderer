// Copies a model (or one instance of a place, by path) without the instances of some classes, e.g. a
// unit without its ParticleEmitters to compare renders with and without them:
//   cargo run -p ror --release --example tools -- strip_class <in> <out.rbxm> [--select <path>] <Class>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("input");
	let output = args.next().expect("output");
	let mut select = None;
	let mut classes = Vec::new();
	while let Some(arg) = args.next() {
		if arg == "--select" {
			select = args.next();
		} else {
			classes.push(arg);
		}
	}
	let mut document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	if let Some(select) = &select {
		document.select(select)?;
	}
	// the outermost of each kind: an instance goes with its descendants (nested Bones)
	let mut doomed = Vec::new();
	let mut stack = document.roots.clone();
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else { continue };
		if classes.iter().any(|class| instance.class == class.as_str()) {
			doomed.push(referent);
			continue;
		}
		stack.extend(instance.children().iter().copied());
	}
	for referent in &doomed {
		document.dom.destroy(*referent);
	}
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &document.roots).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&output, bytes)?;
	println!("removed {} instances", doomed.len());
	Ok(())
}
