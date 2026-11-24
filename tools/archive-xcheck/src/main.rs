//! Cross-checks ror-archive against the draft rbx-dom archive reader/writer (rojo-rbx/rbx-dom#675).
use std::collections::BTreeMap;
use std::io::Cursor;

use rbx_dom_weak::types::{BinaryString, Content, SharedString, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

const LONG: &str = "-- a module long enough to be moved out of the scene\nlocal M = {}\nfunction M.greet(name)\n\treturn `hello {name}`\nend\nreturn M\n";

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

fn sample(opaque: bool) -> WeakDom {
	let collision = SharedString::new(blob(3, 900));
	let content = if opaque {
		Content::from_embedded("rbxruntime://A".into(), envelope(b"hello", br#"{"INTERNAL_TYPE":"Text"}"#))
	} else {
		Content::none()
	};
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
			InstanceBuilder::new("MeshPart")
				.with_name("Plain")
				.with_property("PhysicalConfigData", SharedString::new(b"tiny".to_vec())),
		]),
		InstanceBuilder::new("ServerScriptService").with_children([
			InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('short')"),
			InstanceBuilder::new("ModuleScript").with_name("Shared").with_property("Source", LONG),
			InstanceBuilder::new("ModuleScript").with_name("Copy").with_property("Source", LONG),
			InstanceBuilder::new("ModuleScript").with_name("Small").with_property("Source", "return 1"),
			InstanceBuilder::new("BinaryStringValue")
				.with_name("Packed")
				.with_property("Value", BinaryString::from(blob(200, 300))),
		]),
	]))
}

type Dump = BTreeMap<String, BTreeMap<String, String>>;

fn dump(dom: &WeakDom) -> Dump {
	fn walk(dom: &WeakDom, r: rbx_dom_weak::types::Ref, path: &str, out: &mut Dump) {
		let inst = dom.get_by_ref(r).unwrap();
		let path = format!("{path}/{}", inst.name);
		let mut props = BTreeMap::new();
		props.insert("ClassName".to_owned(), inst.class.to_string());
		for (name, value) in &inst.properties {
			let text = match value {
				Variant::String(t) => format!("string {t:?}"),
				Variant::BinaryString(b) => format!("bytes {:?}", AsRef::<[u8]>::as_ref(b)),
				Variant::SharedString(b) => format!("bytes {:?}", b.data()),
				other => format!("{other:?}"),
			};
			props.insert(name.to_string(), text);
		}
		out.insert(path.clone(), props);
		for c in inst.children() {
			walk(dom, *c, &path, out);
		}
	}
	let mut out = BTreeMap::new();
	for c in dom.root().children() {
		walk(dom, *c, "", &mut out);
	}
	out
}

fn legacy(dom: &WeakDom) -> Vec<u8> {
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, dom, dom.root().children()).unwrap();
	bytes
}

fn check(name: &str, ok: bool, detail: impl std::fmt::Display) -> bool {
	println!("{} {name}: {detail}", if ok { "PASS" } else { "FAIL" });
	ok
}

fn main() {
	let mut all = true;
	let dom = sample(false);
	let place = legacy(&dom);
	let want = dump(&rbx_binary::from_reader(place.as_slice()).unwrap());

	// 1. our plain archive, read by the draft reader
	let ours = ror_archive::Builder::place(place.clone()).build().unwrap();
	match rbx_archive::from_reader(Cursor::new(&ours)) {
		Ok(read) => all &= check("ours plain -> draft reader", dump(&read) == want, format!("{} instances", want.len())),
		Err(e) => all &= check("ours plain -> draft reader", false, e),
	}

	// 2. our archive with strings and shared strings moved out, read by the draft reader
	let ours = ror_archive::Builder::place(place.clone()).externalize(64).build().unwrap();
	let view = ror_archive::Archive::open(&ours).unwrap();
	let objects = view.objects().count();
	match rbx_archive::from_reader(Cursor::new(&ours)) {
		Ok(read) => all &= check("ours externalized -> draft reader", dump(&read) == want, format!("{objects} objects")),
		Err(e) => all &= check("ours externalized -> draft reader", false, e),
	}
	// the stored scene alone is refused by the draft's plain binary reader (no resolver)
	let root = view.root().unwrap();
	all &= check(
		"draft binary reader refuses the stored scene without its objects",
		rbx_binary::from_reader(root.bytes).is_err(),
		"an error, as expected",
	);

	// 3. ZIP64 form
	let ours64 = ror_archive::Builder::place(place.clone()).externalize(64).zip64(true).build().unwrap();
	match rbx_archive::from_reader(Cursor::new(&ours64)) {
		Ok(read) => all &= check("ours zip64 -> draft reader", dump(&read) == want, ""),
		Err(e) => all &= check("ours zip64 -> draft reader", false, e),
	}

	// 4. the draft writer's archive (strings inline), read by us
	let mut theirs = Cursor::new(Vec::new());
	rbx_archive::to_writer(&mut theirs, &dom, dom.root().children()).unwrap();
	let theirs = theirs.into_inner();
	match ror_archive::unwrap(&theirs) {
		Ok(scene) => {
			let read = rbx_binary::from_reader(&*scene.bytes).unwrap();
			all &= check(
				"draft writer plain -> ours",
				dump(&read) == want && scene.report.warnings.is_empty(),
				format!("warnings {:?}", scene.report.warnings),
			);
		}
		Err(e) => all &= check("draft writer plain -> ours", false, e),
	}

	// 5. the draft writer's archive with opaque content, read by us: the value stays None and is reported
	let opaque = sample(true);
	let mut theirs = Cursor::new(Vec::new());
	rbx_archive::to_writer(&mut theirs, &opaque, opaque.root().children()).unwrap();
	let theirs = theirs.into_inner();
	match ror_archive::unwrap(&theirs) {
		Ok(scene) => {
			let read = rbx_binary::from_reader(&*scene.bytes).unwrap();
			let ok = dump(&read) == want
				&& scene.report.embedded_content.len() == 1
				&& scene.report.embedded_content[0].class == "MeshPart"
				&& scene.report.embedded_content[0].property == "MeshContent"
				&& scene.report.embedded_content[0].values == 1;
			all &= check("draft writer opaque content -> ours", ok, format!("{:?}", scene.report.embedded_content));
		}
		Err(e) => all &= check("draft writer opaque content -> ours", false, e),
	}
	// and the draft reader agrees the archive holds the embedded value
	match rbx_archive::from_reader(Cursor::new(&theirs)) {
		Ok(read) => {
			let d = dump(&read);
			let v = d["/Workspace/Barrel"].get("MeshContent").cloned().unwrap_or_default();
			all &= check("draft reader sees the opaque value", v.contains("rbxruntime://A"), v.chars().take(90).collect::<String>());
		}
		Err(e) => all &= check("draft reader sees the opaque value", false, e),
	}

	// 6. a model archive of ours is refused by the draft reader (it reads places only): a known difference
	let model = ror_archive::Builder::model(place.clone()).build().unwrap();
	all &= check(
		"draft reader refuses model packages (expected)",
		rbx_archive::from_reader(Cursor::new(&model)).is_err(),
		"",
	);

	println!("{}", if all { "ALL CHECKS PASSED" } else { "SOME CHECKS FAILED" });
	std::process::exit(if all { 0 } else { 1 });
}
