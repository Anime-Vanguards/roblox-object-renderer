// Copies a model without the parts of the given Enum.Material values (e.g. to compare a model
// without its ForceField parts):
//   cargo run -p ror --release --example tools -- filter_model <in.rbxm> <out.rbxm> <material value>...
use rbx_dom_weak::types::Variant;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let input = args.next().expect("input");
	let output = args.next().expect("output");
	let materials: Vec<u32> = args.map(|value| value.parse().expect("material value")).collect();
	let mut document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let mut doomed = Vec::new();
	let mut stack = document.roots.clone();
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else { continue };
		stack.extend(instance.children().iter().copied());
		if let Some(Variant::Enum(material)) = instance.properties.get(&rbx_dom_weak::ustr("Material"))
			&& materials.contains(&material.to_u32())
		{
			doomed.push(referent);
		}
	}
	for referent in &doomed {
		document.dom.destroy(*referent);
	}
	let roots: Vec<_> = document.roots.iter().copied().filter(|root| document.dom.get_by_ref(*root).is_some()).collect();
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &roots).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&output, bytes)?;
	println!("removed {} parts", doomed.len());
	Ok(())
}
