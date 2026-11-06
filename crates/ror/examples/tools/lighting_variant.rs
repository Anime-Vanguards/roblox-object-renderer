//! Copies a place's Lighting service (its properties, Sky, Atmosphere and post effects) into a new
//! place file with some of its properties set, for `ror compare --lighting-from` (a place at
//! night: TimeOfDay=00:00:00 Ambient=0,0,0 OutdoorAmbient=0,0,0). Values: a string for TimeOfDay,
//! r,g,b (0-1) for colours, true / false, else a number:
//!   cargo run -p ror --release --example tools -- lighting_variant <place> <out.rbxl> [Property=value ...]
pub fn main() -> anyhow::Result<()> {
	use rbx_dom_weak::types::{Color3, Variant};
	use rbx_dom_weak::{InstanceBuilder, WeakDom, ustr};

	let mut args = std::env::args().skip(1);
	let path = args.next().expect("place");
	let out = args.next().expect("out.rbxl");
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let source = &document.dom;
	let lighting = source.root().children().iter().copied().find(|&child| source.get_by_ref(child).is_some_and(|instance| instance.class == "Lighting")).ok_or_else(|| anyhow::anyhow!("{path} has no Lighting"))?;
	let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
	let copy = source.clone_into_external(lighting, &mut dom);
	let root = dom.root_ref();
	dom.transfer_within(copy, root);
	let instance = dom.get_by_ref_mut(copy).expect("the copied Lighting");
	for setting in args {
		let (name, value) = setting.split_once('=').ok_or_else(|| anyhow::anyhow!("{setting}: Property=value"))?;
		let parts: Vec<f32> = value.split(',').filter_map(|part| part.trim().parse().ok()).collect();
		let variant = if name == "TimeOfDay" {
			Variant::String(value.to_string())
		} else if parts.len() == 3 {
			Variant::Color3(Color3::new(parts[0], parts[1], parts[2]))
		} else if let Ok(flag) = value.parse::<bool>() {
			Variant::Bool(flag)
		} else {
			Variant::Float32(value.parse().map_err(|_| anyhow::anyhow!("{setting}: not a number"))?)
		};
		instance.properties.insert(ustr(name), variant);
		println!("{name} = {value}");
	}
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, &[copy]).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&out, bytes)?;
	println!("{out}");
	Ok(())
}
