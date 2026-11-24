//! Writes small archives for other readers' tests, with the plain files they hold:
//!
//!   fixtures <out dir>
use std::io::Cursor;
use std::path::Path;

use rbx_dom_weak::types::{Attributes, BinaryString, Content, SharedString, Tags};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

const LONG: &str = "-- kept apart\nlocal M = {}\nfunction M.greet(name)\n\treturn `hello {name}`\nend\nreturn M\n";

fn blob(seed: u8, len: usize) -> Vec<u8> {
	(0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

fn envelope(payload: &[u8], metadata: &[u8]) -> SharedString {
	let mut bytes = 0i32.to_le_bytes().to_vec();
	bytes.extend_from_slice(&(payload.len() as i32).to_le_bytes());
	bytes.extend_from_slice(payload);
	bytes.extend_from_slice(&(metadata.len() as i32).to_le_bytes());
	bytes.extend_from_slice(metadata);
	SharedString::new(bytes)
}

fn place(opaque: bool) -> WeakDom {
	let collision = SharedString::new(blob(3, 96));
	let content = if opaque {
		Content::from_embedded("rbxruntime://A".into(), envelope(b"hello", br#"{"INTERNAL_TYPE":"Text"}"#))
	} else {
		Content::none()
	};
	let tags: Tags = vec!["first-tag".to_owned(), "second-tag".to_owned()].into();
	WeakDom::new(InstanceBuilder::new("DataModel").with_children([
		InstanceBuilder::new("Workspace").with_children([
			InstanceBuilder::new("MeshPart")
				.with_name("Crate")
				.with_property("PhysicalConfigData", collision.clone())
				.with_property("MeshContent", Content::from_uri("rbxassetid://1818")),
			InstanceBuilder::new("MeshPart")
				.with_name("Barrel")
				.with_property("PhysicalConfigData", collision)
				.with_property("MeshContent", content),
		]),
		InstanceBuilder::new("ServerScriptService").with_children([
			InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('short')"),
			InstanceBuilder::new("ModuleScript").with_name("Shared").with_property("Source", LONG),
			InstanceBuilder::new("ModuleScript").with_name("Copy").with_property("Source", LONG),
			InstanceBuilder::new("BinaryStringValue").with_name("Packed").with_property("Value", BinaryString::from(blob(200, 40))),
			InstanceBuilder::new("Folder")
				.with_name("Marked")
				.with_property("Tags", tags)
				.with_property("Attributes", Attributes::new().with("Note", "a note long enough to be moved out")),
		]),
	]))
}

fn model() -> WeakDom {
	WeakDom::new(InstanceBuilder::new("DataModel").with_children([InstanceBuilder::new("Model")
		.with_name("Rig")
		.with_children([InstanceBuilder::new("ModuleScript").with_name("Config").with_property("Source", LONG)])]))
}

fn legacy(dom: &WeakDom) -> Vec<u8> {
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, dom, dom.root().children()).unwrap();
	bytes
}

fn main() {
	let out = std::env::args().nth(1).expect("out dir");
	let out = Path::new(&out);
	std::fs::create_dir_all(out).unwrap();
	let write = |name: &str, bytes: &[u8]| {
		std::fs::write(out.join(name), bytes).unwrap();
		println!("{name}: {} bytes", bytes.len());
	};
	let place_bytes = legacy(&place(false));
	let model_bytes = legacy(&model());
	write("place.rbxl", &place_bytes);
	write("model.rbxm", &model_bytes);
	write("plain.rbx", &ror_archive::Builder::place(place_bytes.clone()).build().unwrap());
	write("externalized.rbx", &ror_archive::Builder::place(place_bytes.clone()).externalize(16).build().unwrap());
	write("everything.rbx", &ror_archive::Builder::place(place_bytes.clone()).externalize_all(16).build().unwrap());
	write("zip64.rbx", &ror_archive::Builder::place(place_bytes.clone()).externalize(16).zip64(true).build().unwrap());
	write("model.rbx", &ror_archive::Builder::model(model_bytes.clone()).externalize(16).build().unwrap());
	let xml = b"<roblox version=\"4\"><Item class=\"Folder\" referent=\"RBX0\"><Properties><string name=\"Name\">Plain</string></Properties></Item></roblox>";
	write("xml-model.rbx", &ror_archive::Builder::model(xml.to_vec()).build().unwrap());
	// content kept as an opaque object: the draft rbx-dom writer's archive
	let opaque = place(true);
	let mut theirs = Cursor::new(Vec::new());
	rbx_archive::to_writer(&mut theirs, &opaque, opaque.root().children()).unwrap();
	write("opaque.rbx", &theirs.into_inner());
}
