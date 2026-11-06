//! Copies a place's Lighting service (its properties, Sky, Atmosphere and post effects) into a new
//! place file, enabling the named children, for `ror compare --lighting-from` (a place's own
//! ColorGrading, Retro, is disabled in its Lighting):
//!   cargo run -p ror --release --example tools -- lighting_copy <place> <out.rbxl> [child name ...]
pub fn main() -> anyhow::Result<()> {
	use rbx_dom_weak::{InstanceBuilder, WeakDom, ustr};

	let mut args = std::env::args().skip(1);
	let path = args.next().expect("place");
	let out = args.next().expect("out.rbxl");
	let enable: Vec<String> = args.collect();
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let source = &document.dom;
	let lighting = source.root().children().iter().copied().find(|&child| source.get_by_ref(child).is_some_and(|instance| instance.class == "Lighting")).ok_or_else(|| anyhow::anyhow!("{path} has no Lighting"))?;
	let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
	let copy = source.clone_into_external(lighting, &mut dom);
	let root = dom.root_ref();
	dom.transfer_within(copy, root);
	let children: Vec<_> = dom.get_by_ref(copy).map(|instance| instance.children().to_vec()).unwrap_or_default();
	for child in children {
		let Some(instance) = dom.get_by_ref_mut(child) else {
			continue;
		};
		if enable.iter().any(|name| *name == instance.name) {
			instance.properties.insert(ustr("Enabled"), true.into());
			println!("enabled {} {:?}", instance.class, instance.name);
		}
	}
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, &[copy]).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&out, bytes)?;
	println!("wrote {out}");
	Ok(())
}
