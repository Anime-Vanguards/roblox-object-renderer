// Copies a model with every Beam's TextureSpeed set to 0, so Studio's frames settle for parity
// captures (the texture then rests where ror draws it), or without its Beams (`--remove`):
//   cargo run -p ror --release --example tools -- still_beams <in.rbxm> <out.rbxm> [--remove]
use rbx_dom_weak::types::Variant;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("input");
	let output = args.next().expect("output");
	let remove = args.next().as_deref() == Some("--remove");
	let mut document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let mut beams = Vec::new();
	let mut stack = document.roots.clone();
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else { continue };
		stack.extend(instance.children().iter().copied());
		if instance.class == "Beam" {
			beams.push(referent);
		}
	}
	for referent in &beams {
		if remove {
			document.dom.destroy(*referent);
		} else if let Some(instance) = document.dom.get_by_ref_mut(*referent) {
			instance.properties.insert(rbx_dom_weak::ustr("TextureSpeed"), Variant::Float32(0.0));
		}
	}
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &document.roots).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&output, bytes)?;
	println!("{} beams {}", beams.len(), if remove { "removed" } else { "stilled" });
	Ok(())
}
