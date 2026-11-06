// Prints the sizes and first bytes of every union's mesh properties (MeshData, MeshData2,
// ChildData, ChildData2, AssetId), or writes one union's property to a file:
//   cargo run -p ror --release --example tools -- union_data <file> [<union name> <property> <out file>]
use rbx_dom_weak::types::Variant;

fn bytes(value: &Variant) -> Option<Vec<u8>> {
	match value {
		Variant::BinaryString(data) => Some(AsRef::<[u8]>::as_ref(data).to_vec()),
		Variant::SharedString(data) => Some(data.data().to_vec()),
		Variant::String(text) => Some(text.as_bytes().to_vec()),
		_ => None,
	}
}

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let target = args.next();
	let property = args.next();
	let out = args.next();
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	let mut shown = 0;
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else { continue };
		stack.extend(instance.children().iter().copied());
		if !matches!(instance.class.as_str(), "UnionOperation" | "NegateOperation" | "IntersectOperation") {
			continue;
		}
		if let (Some(target), Some(property), Some(out)) = (&target, &property, &out) {
			if &instance.name == target
				&& let Some(data) = instance.properties.get(&rbx_dom_weak::ustr(property)).and_then(bytes)
			{
				std::fs::write(out, &data)?;
				println!("wrote {} bytes", data.len());
				return Ok(());
			}
			continue;
		}
		if shown >= 1000 {
			continue;
		}
		shown += 1;
		let mut line = format!("{} {:?}:", instance.class, instance.name);
		if let Some(data) = instance.properties.get(&rbx_dom_weak::ustr("ChildData2")).and_then(bytes).filter(|data| !data.is_empty()) {
			match rbx_binary::from_reader(data.as_slice()) {
				Ok(children) => {
					let mut classes = std::collections::BTreeMap::new();
					let mut stack = vec![children.root_ref()];
					while let Some(referent) = stack.pop() {
						let Some(child) = children.get_by_ref(referent) else { continue };
						stack.extend(child.children().iter().copied());
						*classes.entry(child.class.to_string()).or_insert(0) += 1;
					}
					line += &format!(" children {classes:?}");
				}
				Err(error) => line += &format!(" children: {error}"),
			}
		}
		for name in ["MeshData", "MeshData2", "ChildData", "ChildData2", "AssetId", "PhysicalConfigData"] {
			match instance.properties.get(&rbx_dom_weak::ustr(name)) {
				Some(value) => match bytes(value) {
					Some(data) => line += &format!(" {name}={}B {:?}", data.len(), String::from_utf8_lossy(&data[..data.len().min(16)])),
					None => line += &format!(" {name}={value:?}"),
				},
				None => {}
			}
		}
		println!("{line}");
	}
	Ok(())
}
