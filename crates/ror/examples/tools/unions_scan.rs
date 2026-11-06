//! Lists the unions (PartOperations) of model or place files: which geometry they carry (MeshData,
//! MeshData2, ChildData, ChildData2, AssetId, PhysicalConfigData, ...) and with which flags.
//!
//! ```text
//! cargo run -p ror --release --example tools -- unions_scan <files...> [--all] [--props]
//! ```

use std::collections::BTreeMap;

use rbx_dom_weak::types::{Ref, Variant};
use ror::scene::{load, props};

fn size_of(value: &Variant) -> String {
	match value {
		Variant::BinaryString(bytes) => format!("{} B", AsRef::<[u8]>::as_ref(bytes).len()),
		Variant::SharedString(shared) => format!("{} B shared", shared.data().len()),
		Variant::String(text) => format!("{:?}", if text.len() > 40 { &text[..40] } else { text }),
		Variant::ContentId(id) => format!("{:?}", id.as_str()),
		Variant::Content(content) => format!("{:?}", content),
		other => {
			let text = format!("{other:?}");
			if text.len() > 60 { text[..60].to_string() } else { text }
		}
	}
}

fn head(value: &Variant) -> String {
	let bytes: &[u8] = match value {
		Variant::BinaryString(bytes) => bytes.as_ref(),
		Variant::SharedString(shared) => shared.data(),
		_ => return String::new(),
	};
	bytes.iter().take(12).map(|&byte| if byte.is_ascii_graphic() { byte as char } else { '.' }).collect()
}

pub fn main() -> anyhow::Result<()> {
	let mut files = Vec::new();
	let (mut all, mut show_props) = (false, false);
	let mut decode = Vec::new();
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--all" => all = true,
			"--props" => show_props = true,
			"--decode" => decode.extend(args.next()),
			_ => files.push(arg),
		}
	}
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in &decode {
		let fetched = assets.fetch(uri).map_err(|error| anyhow::anyhow!("{error}"))?;
		match ror_mesh::decode_union(&fetched.bytes) {
			Ok(mesh) => {
				let (min, max) = mesh.lod0_bounds().unwrap_or_default();
				println!("{uri}: {:?}, {} vertices, {} triangles, {} LODs, bounds {min:?} .. {max:?}", mesh.format, mesh.vertex_count(), mesh.lod0().len() / 3, mesh.lods.len());
			}
			Err(error) => println!("{uri}: {error}"),
		}
	}
	for file in files {
		let document = load::read_file(std::path::Path::new(&file))?;
		let mut stack: Vec<Ref> = vec![document.dom.root_ref()];
		let mut unions = Vec::new();
		while let Some(referent) = stack.pop() {
			let Some(instance) = document.dom.get_by_ref(referent) else {
				continue;
			};
			if props::is_a(instance, "PartOperation") {
				unions.push(referent);
			}
			stack.extend(instance.children().iter().copied());
		}
		// which geometry properties are non-empty, per combination
		let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
		let keys = ["MeshData", "MeshData2", "ChildData", "ChildData2", "AssetId", "Content", "PhysicalConfigData", "PhysicsData"];
		for &referent in &unions {
			let instance = document.dom.get_by_ref(referent).expect("union");
			let present: Vec<&str> = keys
				.iter()
				.copied()
				.filter(|key| match instance.properties.get(&rbx_dom_weak::ustr(key)) {
					Some(Variant::BinaryString(bytes)) => !AsRef::<[u8]>::as_ref(bytes).is_empty(),
					Some(Variant::SharedString(shared)) => !shared.data().is_empty(),
					Some(Variant::ContentId(id)) => !id.as_str().is_empty(),
					Some(Variant::Content(content)) => content.as_uri().is_some_and(|uri| !uri.is_empty()),
					Some(Variant::String(text)) => !text.is_empty(),
					_ => false,
				})
				.collect();
			*kinds.entry(format!("{} [{}]", instance.class, present.join(", "))).or_default() += 1;
		}
		println!("{file}: {} unions", unions.len());
		for (kind, count) in &kinds {
			println!("  {count:5} {kind}");
		}
		for &referent in unions.iter().take(if all { usize::MAX } else { 6 }) {
			let instance = document.dom.get_by_ref(referent).expect("union");
			let path = document.path_of(referent);
			let short = if path.len() > 70 { format!("...{}", &path[path.len() - 67..]) } else { path };
			let source = if props::binary(instance, "MeshData2").is_some() { "MeshData2" } else if props::content(instance, &["Content", "AssetId"]).is_some() { "Content" } else { "-" };
			let children: Vec<String> = instance.children().iter().filter_map(|&child| document.dom.get_by_ref(child)).map(|child| child.class.to_string()).collect();
			println!(
				"  {short}: {source} size {} material {} color {:?} UsePartColor {} children [{}]",
				props::vec3(instance, "Size", glam::Vec3::ZERO),
				props::enum_name("Material", props::enum_value(instance, "Material", 256)).unwrap_or("?"),
				props::color(instance, "Color", glam::Vec3::ZERO) * 255.0,
				props::bool(instance, "UsePartColor", false),
				children.join(",")
			);
			// the source parts kept for re-solving (ChildData2: a binary model)
			if let Some(child_data) = props::binary(instance, "ChildData2").or_else(|| props::binary(instance, "ChildData")).filter(|bytes| bytes.starts_with(b"<roblox")) {
				match load::read_bytes(&child_data) {
					Ok(children) => {
						let mut classes: BTreeMap<String, usize> = BTreeMap::new();
						for descendant in children.dom.descendants() {
							*classes.entry(descendant.class.to_string()).or_default() += 1;
						}
						println!("      ChildData: {classes:?}");
					}
					Err(error) => println!("      ChildData: {error:#}"),
				}
			}
			// an unpublished union keeps its solid in SolidMeshHolder (a NetAssetRef): dump it
			if let Some(Variant::NetAssetRef(holder)) = instance.properties.get(&rbx_dom_weak::ustr("SolidMeshHolder")) {
				let data = holder.data();
				if !data.is_empty() {
					let name = format!("out/geometry/solid-{}.bin", instance.name);
					std::fs::create_dir_all("out/geometry")?;
					std::fs::write(&name, data)?;
					println!("      SolidMeshHolder: {} bytes -> {name}", data.len());
				}
			}
			if show_props {
				let mut entries: Vec<String> = instance.properties.iter().map(|(key, value)| format!("{key}={} {}", size_of(value), head(value))).collect();
				entries.sort();
				for entry in entries {
					println!("      {entry}");
				}
			}
		}
	}
	Ok(())
}
