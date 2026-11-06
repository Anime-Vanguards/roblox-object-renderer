// Tiles model files side by side in one file, for scaling tests (how a render grows with the models
// in it): each file's top-level instances moved `spacing` studs along X per column and along Z per
// row, every BasePart's CFrame and every Model's pivot with them:
//   cargo run -p ror --release --example tools -- tile_models <out.rbxm> <spacing> <columns> <model.rbxm>...
use rbx_dom_weak::types::{Ref, Variant, Vector3};
use rbx_dom_weak::{InstanceBuilder, WeakDom, ustr};

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out_path = args.next().expect("out.rbxm");
	let spacing: f32 = args.next().expect("spacing").parse()?;
	let columns: usize = args.next().expect("columns").parse()?;
	let files: Vec<String> = args.collect();
	let mut out = WeakDom::new(InstanceBuilder::new("DataModel"));
	let root = out.root_ref();
	let mut tops = Vec::new();
	for (index, path) in files.iter().enumerate() {
		let bytes = std::fs::read(path)?;
		let mut dom = rbx_binary::Deserializer::new().deserialize(bytes.as_slice()).map_err(|error| anyhow::anyhow!("{path}: {error}"))?;
		let offset = Vector3::new((index % columns) as f32 * spacing, 0.0, (index / columns) as f32 * spacing);
		let shift = |position: Vector3| Vector3::new(position.x + offset.x, position.y + offset.y, position.z + offset.z);
		let referents: Vec<Ref> = dom.descendants().map(|instance| instance.referent()).collect();
		for referent in referents {
			let Some(instance) = dom.get_by_ref_mut(referent) else { continue };
			if let Some(Variant::CFrame(cframe)) = instance.properties.get_mut(&ustr("CFrame")) {
				cframe.position = shift(cframe.position);
			}
			if let Some(Variant::OptionalCFrame(Some(cframe))) = instance.properties.get_mut(&ustr("WorldPivotData")) {
				cframe.position = shift(cframe.position);
			}
		}
		for child in dom.root().children().to_vec() {
			dom.transfer(child, &mut out, root);
			tops.push(child);
		}
	}
	let mut bytes = Vec::new();
	rbx_binary::Serializer::new().serialize(&mut bytes, &out, &tops).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&out_path, &bytes)?;
	println!("{} models, {} top-level instances, {} bytes -> {out_path}", files.len(), tops.len(), bytes.len());
	Ok(())
}
