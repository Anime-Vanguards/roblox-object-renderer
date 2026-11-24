//! End-to-end tests: places built here, wrapped in archives, and read back through a DOM parser
//! that knows nothing about archives.

use std::borrow::Cow;
use std::collections::BTreeMap;

use rbx_dom_weak::types::{Attributes, BinaryString, Content, ContentId, SharedString, Tags, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use crate::manifest::Digest;
use crate::scene::{self, Reader, Reference, TAG_EMBEDDED, TYPE_CONTENT, write_chunk, write_references};
use crate::{Archive, Asset, Builder, EmbeddedContent, Error, Limits, SceneFormat, is_archive, unwrap, unwrap_with, zip};

const LONG_SOURCE: &str = "-- a module long enough to be moved out of the scene\nlocal Module = {}\nfunction Module.greet(name)\n\treturn `hello {name}`\nend\nreturn Module\n";

fn blob(seed: u8, len: usize) -> Vec<u8> {
	(0..len).map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// A place with scripts (two sharing one long source), a binary string that is not UTF-8, two mesh
/// parts sharing one long shared string, and content values of both kinds.
fn sample_place() -> Vec<u8> {
	let collision = SharedString::new(blob(3, 900));
	let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_children([
		InstanceBuilder::new("Workspace").with_children([
			InstanceBuilder::new("MeshPart")
				.with_name("Crate")
				.with_property("PhysicalConfigData", collision.clone())
				.with_property("MeshContent", Content::from_uri("rbxassetid://1818")),
			InstanceBuilder::new("MeshPart")
				.with_name("Barrel")
				.with_property("PhysicalConfigData", collision)
				.with_property("MeshContent", Content::none()),
			InstanceBuilder::new("MeshPart").with_name("Plain").with_property("PhysicalConfigData", SharedString::new(b"tiny".to_vec())),
		]),
		InstanceBuilder::new("ServerScriptService").with_children([
			InstanceBuilder::new("Script").with_name("Main").with_property("Source", "print('short')"),
			InstanceBuilder::new("ModuleScript").with_name("Shared").with_property("Source", LONG_SOURCE),
			InstanceBuilder::new("ModuleScript").with_name("Copy").with_property("Source", LONG_SOURCE),
			InstanceBuilder::new("ModuleScript").with_name("Small").with_property("Source", "return 1"),
			InstanceBuilder::new("BinaryStringValue")
				.with_name("Packed")
				.with_property("Value", BinaryString::from(blob(200, 300))),
		]),
	]));
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
	bytes
}

fn sample_model() -> Vec<u8> {
	let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_children([InstanceBuilder::new("Model")
		.with_name("Rig")
		.with_children([InstanceBuilder::new("ModuleScript").with_name("Config").with_property("Source", LONG_SOURCE)])]));
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
	bytes
}

/// What a DOM parser reads from a serialized place: every instance by its path, with its class and
/// its properties' values. Byte values are compared as bytes, the rest by their debug form.
fn dump(bytes: &[u8]) -> BTreeMap<String, BTreeMap<String, String>> {
	fn walk(dom: &WeakDom, referent: rbx_dom_weak::types::Ref, path: &str, out: &mut BTreeMap<String, BTreeMap<String, String>>) {
		let instance = dom.get_by_ref(referent).unwrap();
		let path = format!("{path}/{}", instance.name);
		let mut properties = BTreeMap::new();
		properties.insert("ClassName".to_owned(), instance.class.to_string());
		for (name, value) in &instance.properties {
			let text = match value {
				Variant::String(text) => format!("string {text:?}"),
				Variant::BinaryString(bytes) => format!("bytes {:?}", AsRef::<[u8]>::as_ref(bytes)),
				Variant::SharedString(bytes) => format!("bytes {:?}", bytes.data()),
				other => format!("{other:?}"),
			};
			properties.insert(name.to_string(), text);
		}
		assert!(out.insert(path.clone(), properties).is_none(), "two instances at {path}");
		for child in instance.children() {
			walk(dom, *child, &path, out);
		}
	}
	let dom = rbx_binary::from_reader(bytes).unwrap();
	let mut out = BTreeMap::new();
	for child in dom.root().children() {
		walk(&dom, *child, "", &mut out);
	}
	out
}

fn property<'a>(dump: &'a BTreeMap<String, BTreeMap<String, String>>, path: &str, name: &str) -> Option<&'a str> {
	dump.get(path).and_then(|properties| properties.get(name)).map(String::as_str)
}

fn digest(bytes: &[u8]) -> String {
	Digest::Blake3_128.hex(bytes)
}

/// A ZIP from a manifest's lines and objects named by their own digests.
fn archive(lines: &[String], objects: &[&[u8]]) -> Vec<u8> {
	let named: Vec<(String, &[u8])> = objects.iter().map(|bytes| (digest(bytes), *bytes)).collect();
	archive_named(lines, &named)
}

/// A ZIP from a manifest's lines and objects under the names given, whatever their bytes are.
fn archive_named(lines: &[String], objects: &[(String, &[u8])]) -> Vec<u8> {
	let manifest = lines.join("\n") + "\n";
	let mut members: Vec<(String, &[u8])> = vec![("manifest.jsonl".to_owned(), manifest.as_bytes())];
	members.extend(objects.iter().map(|(name, bytes)| (format!("objects/{name}"), *bytes)));
	zip::write(&members, false)
}

/// The archive `archived` with `stored` as its scene: the other objects stay, a record for each.
fn with_scene(archived: &[u8], stored: &[u8]) -> Vec<u8> {
	let view = Archive::open(archived).unwrap();
	let root = view.root().unwrap();
	let mut lines = place_lines(stored);
	let mut all: Vec<&[u8]> = vec![stored];
	for (name, object) in view.objects() {
		if name != root.digest {
			lines.push(format!(r#"{{"kind":"content","objectDigest":"{name}"}}"#));
			all.push(object);
		}
	}
	archive(&lines, &all)
}

const HEADER: &str = r#"{"kind":"header","version":1,"digestAlgorithm":"blake3-128"}"#;

fn place_lines(scene: &[u8]) -> Vec<String> {
	vec![
		HEADER.to_owned(),
		r#"{"kind":"package","type":"place","place":"Place.rbxl"}"#.to_owned(),
		format!(r#"{{"kind":"content","path":"Place.rbxl","objectDigest":"{}"}}"#, digest(scene)),
	]
}

/// Rewrites the first `PROP` chunk of `property` through `edit`, which gets the chunk's data after
/// the property's name and returns what replaces it.
fn edit_property(scene_bytes: &[u8], property: &str, edit: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
	let (list, end) = scene::chunks(scene_bytes).unwrap();
	let mut out = scene_bytes[..scene::HEADER_LEN].to_vec();
	let mut done = false;
	for chunk in &list {
		let mut scratch = Vec::new();
		let payload = scene::data(scene_bytes, chunk, &mut scratch, usize::MAX).unwrap();
		if &chunk.name == b"PROP" && !done {
			let mut reader = Reader::new(payload);
			reader.u32("class").unwrap();
			if reader.sized("name").unwrap() == property.as_bytes() {
				let mut rewritten = payload[..reader.at].to_vec();
				rewritten.extend_from_slice(&edit(reader.rest()));
				write_chunk(&mut out, b"PROP", &rewritten).unwrap();
				done = true;
				continue;
			}
		}
		out.extend_from_slice(&scene_bytes[chunk.whole.clone()]);
	}
	assert!(done, "no PROP chunk for {property}");
	out.extend_from_slice(&scene_bytes[end..]);
	out
}

/// Replaces the shared string table with the chunks `edit` makes of its data.
fn edit_table(scene_bytes: &[u8], edit: impl Fn(&[u8]) -> Vec<Vec<u8>>) -> Vec<u8> {
	let (list, end) = scene::chunks(scene_bytes).unwrap();
	let mut out = scene_bytes[..scene::HEADER_LEN].to_vec();
	let mut done = false;
	for chunk in &list {
		if &chunk.name == b"SSTR" {
			let mut scratch = Vec::new();
			for payload in edit(scene::data(scene_bytes, chunk, &mut scratch, usize::MAX).unwrap()) {
				write_chunk(&mut out, b"SSTR", &payload).unwrap();
			}
			done = true;
			continue;
		}
		out.extend_from_slice(&scene_bytes[chunk.whole.clone()]);
	}
	assert!(done, "no shared string table");
	out.extend_from_slice(&scene_bytes[end..]);
	out
}

/// A shared string table's entries, each with its hash, and a table of the entries given.
fn table_entries(payload: &[u8]) -> Vec<&[u8]> {
	let mut reader = Reader::new(payload);
	assert_eq!(reader.u32("version").unwrap(), 0);
	let count = reader.u32("count").unwrap();
	(0..count)
		.map(|_| {
			let start = reader.at;
			reader.take(16, "hash").unwrap();
			reader.sized("data").unwrap();
			&payload[start..reader.at]
		})
		.collect()
}

fn table_of(entries: &[&[u8]]) -> Vec<u8> {
	let mut out = 0u32.to_le_bytes().to_vec();
	out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
	out.extend(entries.iter().flat_map(|entry| entry.iter().copied()));
	out
}

/// A place of scripts that all have the same long source, and mesh parts that share one blob.
fn repeating_place(scripts: usize, parts: usize, blob_len: usize) -> Vec<u8> {
	let shared = SharedString::new(blob(5, blob_len));
	let scripts = (0..scripts).map(|index| InstanceBuilder::new("ModuleScript").with_name(format!("Module{index}")).with_property("Source", LONG_SOURCE));
	let parts = (0..parts).map(|index| InstanceBuilder::new("MeshPart").with_name(format!("Part{index}")).with_property("PhysicalConfigData", shared.clone()));
	let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_children(scripts).with_children(parts));
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
	bytes
}

#[test]
fn a_wrapped_place_is_read_back_untouched() {
	let place = sample_place();
	let bytes = Builder::place(place.clone()).build().unwrap();
	assert!(is_archive(&bytes));
	let archive = Archive::open(&bytes).unwrap();
	assert!(archive.warnings().is_empty(), "{:?}", archive.warnings());
	assert_eq!(archive.manifest().package.kind, "place");
	assert_eq!(archive.manifest().package.place.as_deref(), Some("Place.rbxl"));
	assert_eq!(archive.manifest().content.len(), 1);
	assert_eq!(archive.content("Place.rbxl"), Some(place.as_slice()));
	let root = archive.root().unwrap();
	assert_eq!(root.format, SceneFormat::Binary);
	assert_eq!(root.digest, digest(&place));
	let scene = archive.flatten().unwrap();
	assert!(matches!(scene.bytes, Cow::Borrowed(_)), "a standalone scene is not copied");
	assert_eq!(&*scene.bytes, place.as_slice());
	assert_eq!(scene.package.as_deref(), Some("place"));
	assert_eq!(scene.report.restored_values, 0);
	assert!(scene.report.embedded_content.is_empty());
}

#[test]
fn the_same_place_makes_the_same_archive() {
	let place = sample_place();
	let first = Builder::place(place.clone()).externalize(64).build().unwrap();
	let second = Builder::place(place).externalize(64).build().unwrap();
	assert_eq!(first, second);
}

#[test]
fn values_moved_out_of_the_scene_come_back() {
	let place = sample_place();
	let original = dump(&place);
	let bytes = Builder::place(place.clone()).externalize(64).build().unwrap();
	let archive = Archive::open(&bytes).unwrap();
	assert!(archive.warnings().is_empty(), "{:?}", archive.warnings());

	// the long source (shared by two scripts), the binary string and the shared collision blob
	// each became one object beside the scene
	let objects: Vec<&[u8]> = archive.objects().map(|(_, bytes)| bytes).collect();
	assert_eq!(objects.len(), 4);
	assert!(objects.contains(&LONG_SOURCE.as_bytes()));
	assert!(objects.contains(&blob(3, 900).as_slice()));
	assert!(objects.contains(&blob(200, 300).as_slice()));

	// a parser that does not know archives reads the stored scene without those values, and without
	// an error: the misread flattening exists to prevent
	let root = archive.root().unwrap();
	let stored = dump(root.bytes);
	assert_eq!(property(&stored, "/ServerScriptService/Shared", "Source"), None);
	assert_eq!(property(&stored, "/ServerScriptService/Small", "Source"), None);
	assert_eq!(property(&stored, "/Workspace/Crate", "PhysicalConfigData"), None);

	let scene = archive.flatten().unwrap();
	assert!(matches!(scene.bytes, Cow::Owned(_)));
	assert_eq!(scene.format, SceneFormat::Binary);
	assert_eq!(dump(&scene.bytes), original);
	// two scripts, one binary string, two mesh parts
	assert_eq!(scene.report.restored_values, 5);
	assert_eq!(scene.report.restored_bytes, (2 * LONG_SOURCE.len() + 300 + 2 * 900) as u64);
	assert!(scene.report.embedded_content.is_empty());
	assert!(scene.report.warnings.is_empty(), "{:?}", scene.report.warnings);

	// the flattened scene is standalone: wrapping it again changes nothing
	assert!(matches!(unwrap(&scene.bytes).unwrap().bytes, Cow::Borrowed(_)));
}

#[test]
fn unwrap_passes_plain_files_through_and_opens_archives() {
	let place = sample_place();
	let plain = unwrap(&place).unwrap();
	assert!(matches!(plain.bytes, Cow::Borrowed(_)));
	assert_eq!(plain.package, None);
	assert_eq!(plain.format, SceneFormat::Binary);

	let xml = b"<roblox version=\"4\"></roblox>";
	assert_eq!(unwrap(xml).unwrap().format, SceneFormat::Xml);
	let declared = b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<roblox version=\"4\"></roblox>";
	assert_eq!(unwrap(declared).unwrap().format, SceneFormat::Xml);
	let spaced = b"\xEF\xBB\xBF\r\n  <roblox version=\"4\"></roblox>";
	assert_eq!(unwrap(spaced).unwrap().format, SceneFormat::Xml);

	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	let scene = unwrap(&archived).unwrap();
	assert_eq!(scene.package.as_deref(), Some("place"));
	assert_eq!(dump(&scene.bytes), dump(&place));

	assert!(matches!(unwrap(b"version 4.00\n"), Err(Error::NotArchive)));
	assert!(matches!(Archive::open(&place), Err(Error::NotArchive)));
}

#[test]
fn a_zip_that_is_not_an_archive_says_so() {
	let bytes = zip::write(&[("word/document.xml".to_owned(), b"<w/>".as_slice())], false);
	let error = unwrap(&bytes).unwrap_err();
	assert!(matches!(error, Error::Manifest(_)));
	assert!(error.to_string().contains("not a Roblox archive"), "{error}");
	let empty = zip::write(&[], false);
	assert!(is_archive(&empty));
	assert!(matches!(unwrap(&empty), Err(Error::Manifest(_))));
}

#[test]
fn an_xml_scene_is_passed_through() {
	let xml = b"<roblox version=\"4\"><Item class=\"Folder\"></Item></roblox>".to_vec();
	let bytes = Builder::place(xml.clone()).externalize(1).build().unwrap();
	let scene = unwrap(&bytes).unwrap();
	assert_eq!(scene.format, SceneFormat::Xml);
	assert_eq!(&*scene.bytes, xml.as_slice());
}

#[test]
fn a_model_package_is_found_by_field_then_path_then_content() {
	let model = sample_model();
	let original = dump(&model);
	let built = Builder::model(model.clone()).externalize(64).build().unwrap();
	let scene = unwrap(&built).unwrap();
	assert_eq!(scene.package.as_deref(), Some("model"));
	assert_eq!(dump(&scene.bytes), original);

	let package = r#"{"kind":"package","type":"model"}"#.to_owned();
	let script = b"return 1".as_slice();
	// no field: the only record with a model's extension
	let by_path = archive(
		&[
			HEADER.to_owned(),
			package.clone(),
			format!(r#"{{"kind":"content","path":"Scripts/Config.luau","objectDigest":"{}"}}"#, digest(script)),
			format!(r#"{{"kind":"content","path":"Rig.rbxm","objectDigest":"{}"}}"#, digest(&model)),
		],
		&[script, &model],
	);
	assert_eq!(Archive::open(&by_path).unwrap().root().unwrap().path.as_deref(), Some("Rig.rbxm"));
	// no path either: the only object that is a serialized model
	let by_content = archive(
		&[
			HEADER.to_owned(),
			package.clone(),
			format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(script)),
			format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(&model)),
		],
		&[script, &model],
	);
	assert_eq!(Archive::open(&by_content).unwrap().root().unwrap().digest, digest(&model));
	// two candidates and nothing to choose by
	let other = sample_place();
	let ambiguous = archive(
		&[
			HEADER.to_owned(),
			package.clone(),
			format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(&other)),
			format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(&model)),
		],
		&[&other, &model],
	);
	let error = Archive::open(&ambiguous).unwrap().root().unwrap_err();
	assert!(matches!(error, Error::Unsupported(_)), "{error}");
	let none = archive(&[HEADER.to_owned(), package], &[]);
	assert!(matches!(Archive::open(&none).unwrap().root(), Err(Error::Unsupported(_))));
}

#[test]
fn the_specifications_example_opens() {
	let place = sample_place();
	let script = b"print('main')".as_slice();
	let texture = blob(9, 64);
	let mut lines = place_lines(&place);
	lines.push(format!(r#"{{"kind":"content","path":"Scripts/Main.luau","objectDigest":"{}"}}"#, digest(script)));
	lines.push(r#"{"kind":"content","path":"Models/Tree","asset":{"id":1818,"version":3}}"#.to_owned());
	lines.push(format!(r#"{{"kind":"content","asset":{{"id":2020,"version":1}},"objectDigest":"{}"}}"#, digest(&texture)));
	let bytes = archive(&lines, &[&place, script, &texture]);
	let archive = Archive::open(&bytes).unwrap();
	assert!(archive.warnings().is_empty(), "{:?}", archive.warnings());
	assert_eq!(archive.content("Scripts/Main.luau"), Some(script));
	assert_eq!(archive.content("Models/Tree"), None);
	let records = &archive.manifest().content;
	assert_eq!(records[2].asset, Some(Asset { id: 1818, version: Some(3), tag: None }));
	assert_eq!(records[3].asset.as_ref().map(|asset| asset.id), Some(2020));
	assert_eq!(archive.object(records[3].object_digest.as_deref().unwrap()), Some(texture.as_slice()));
	assert_eq!(dump(&archive.flatten().unwrap().bytes), dump(&place));

	// the builder writes the same shape
	let built = Builder::place(place.clone())
		.content(Some("Scripts/Main.luau"), script.to_vec())
		.asset(Some("Models/Tree"), Asset { id: 1818, version: Some(3), tag: None }, None)
		.asset(None, Asset { id: 2020, version: Some(1), tag: Some("live".into()) }, Some(texture.clone()))
		.build()
		.unwrap();
	let rebuilt = Archive::open(&built).unwrap();
	assert_eq!(rebuilt.manifest().content.len(), 4);
	assert_eq!(rebuilt.manifest().content[3].asset, Some(Asset { id: 2020, version: Some(1), tag: Some("live".into()) }));
	assert_eq!(rebuilt.content("Scripts/Main.luau"), Some(script));
}

#[test]
fn zip64_archives_open() {
	let place = sample_place();
	let bytes = Builder::place(place.clone()).externalize(64).zip64(true).build().unwrap();
	assert_eq!(dump(&unwrap(&bytes).unwrap().bytes), dump(&place));
}

#[test]
fn an_object_that_does_not_match_its_name_is_refused() {
	let place = sample_place();
	let lines = place_lines(&place);
	let manifest = lines.join("\n") + "\n";
	let mut changed = place.clone();
	*changed.last_mut().unwrap() ^= 1;
	let name = format!("objects/{}", digest(&place));
	let bytes = zip::write(&[("manifest.jsonl".to_owned(), manifest.as_bytes()), (name, changed.as_slice())], false);
	assert!(matches!(Archive::open(&bytes), Err(Error::Digest { .. })));
}

#[test]
fn an_unknown_digest_algorithm_opens_unchecked_with_a_warning() {
	let place = sample_place();
	let mut lines = place_lines(&place);
	lines[0] = r#"{"kind":"header","version":1,"digestAlgorithm":"sha3-256"}"#.to_owned();
	let bytes = archive(&lines, &[&place]);
	let opened = Archive::open(&bytes).unwrap();
	assert_eq!(opened.warnings().len(), 1);
	assert!(opened.warnings()[0].contains("sha3-256"), "{:?}", opened.warnings());
	let scene = opened.flatten().unwrap();
	assert_eq!(scene.report.warnings.len(), 1);
	assert_eq!(&*scene.bytes, place.as_slice());
}

#[test]
fn records_and_members_that_disagree() {
	let place = sample_place();
	let stray = b"an object no record names".as_slice();
	// an object with no record, and a member that is neither: read past, and said
	let manifest = place_lines(&place).join("\n") + "\n";
	let bytes = zip::write(
		&[
			("manifest.jsonl".to_owned(), manifest.as_bytes()),
			(format!("objects/{}", digest(&place)), place.as_slice()),
			(format!("objects/{}", digest(stray)), stray),
			("objects/".to_owned(), b"".as_slice()),
			("thumbnail.png".to_owned(), b"png".as_slice()),
		],
		false,
	);
	let opened = Archive::open(&bytes).unwrap();
	assert_eq!(opened.warnings().len(), 2, "{:?}", opened.warnings());
	assert!(opened.warnings().iter().any(|warning| warning.contains("thumbnail.png")));
	assert!(opened.warnings().iter().any(|warning| warning.contains("no content record")));

	// a record whose object is missing: refused
	let mut lines = place_lines(&place);
	lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(stray)));
	let error = Archive::open(&archive(&lines, &[&place])).unwrap_err();
	assert!(matches!(error, Error::Manifest(_)), "{error}");

	// a place path with no record: refused when the scene is asked for
	let mut lines = place_lines(&place);
	lines[1] = r#"{"kind":"package","type":"place","place":"Other.rbxl"}"#.to_owned();
	let bytes = archive(&lines, &[&place]);
	assert!(matches!(Archive::open(&bytes).unwrap().root(), Err(Error::Manifest(_))));

	// a package type this reader does not open
	let mut lines = place_lines(&place);
	lines[1] = r#"{"kind":"package","type":"experience"}"#.to_owned();
	let bytes = archive(&lines, &[&place]);
	let opened = Archive::open(&bytes).unwrap();
	assert_eq!(opened.manifest().package.kind, "experience");
	assert!(matches!(opened.flatten(), Err(Error::Unsupported(_))));

	// a scene that is only an asset reference
	let lines = vec![
		HEADER.to_owned(),
		r#"{"kind":"package","type":"place","place":"Place.rbxl"}"#.to_owned(),
		r#"{"kind":"content","path":"Place.rbxl","asset":{"id":7}}"#.to_owned(),
	];
	assert!(matches!(Archive::open(&archive(&lines, &[])).unwrap().root(), Err(Error::Unsupported(_))));

	// a scene that is not a Roblox file
	let text = b"not a place".as_slice();
	assert!(matches!(Archive::open(&archive(&place_lines(text), &[text])).unwrap().root(), Err(Error::Scene(_))));
}

/// The sample place with its mesh parts' `MeshContent` column given one opaque reference: for a
/// part whose content is `None`, or (`on_none` false) for the part whose content is an asset id.
fn place_with_opaque_content(envelope: &[u8], identity: Option<&str>, on_none: bool) -> Vec<u8> {
	edit_property(&sample_place(), "MeshContent", |rest| {
		assert_eq!(rest[0], TYPE_CONTENT);
		// three mesh parts; sources are interleaved and transformed, and 0 stays 0
		let sources = scene::read_interleaved(&rest[1..13], 3);
		let mut references = vec![None; 3];
		let slot = sources.iter().rposition(|source| (*source == 0) == on_none).unwrap();
		let digest: [u8; 16] = blake3::hash(envelope).as_bytes()[..16].try_into().unwrap();
		references[slot] = Some(Reference { digest, size: envelope.len() as u64, identity: identity.map(str::to_owned) });
		let mut out = vec![TAG_EMBEDDED];
		write_references(&mut out, &references);
		out.extend_from_slice(rest);
		out
	})
}

#[test]
fn opaque_content_stays_none_and_is_reported() {
	let envelope = blob(40, 120);
	let stored = place_with_opaque_content(&envelope, Some("content-1"), true);
	let mut lines = place_lines(&stored);
	lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(&envelope)));
	let bytes = archive(&lines, &[&stored, &envelope]);
	let scene = unwrap(&bytes).unwrap();
	assert!(matches!(scene.bytes, Cow::Owned(_)));
	// the flattened scene reads as the place did before the reference was added
	assert_eq!(dump(&scene.bytes), dump(&sample_place()));
	assert_eq!(
		scene.report.embedded_content,
		vec![EmbeddedContent { class: "MeshPart".into(), property: "MeshContent".into(), values: 1, bytes: 120 }]
	);
	assert_eq!(scene.report.restored_values, 0);
}

#[test]
fn references_this_reader_cannot_honour_are_errors_not_guesses() {
	let envelope = blob(40, 120);
	let open = |stored: &[u8], objects: &[&[u8]]| {
		let mut lines = place_lines(stored);
		for object in objects.iter().skip(1) {
			lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(object)));
		}
		unwrap(&archive(&lines, objects)).map(|scene| scene.report)
	};

	// the object a property names is not in the archive
	let stored = place_with_opaque_content(&envelope, Some("content-1"), true);
	let error = open(&stored, &[&stored]).unwrap_err();
	assert!(matches!(error, Error::Scene(_)), "{error}");
	assert!(error.to_string().contains("does not hold"), "{error}");

	// a content value pointing at plain bytes (no identity)
	let stored = place_with_opaque_content(&envelope, None, true);
	assert!(matches!(open(&stored, &[&stored, &envelope]), Err(Error::Scene(_))));

	// a reference over a content value that is there: the asset id it holds would be read, and the
	// report would call it an object
	let error = unwrap(&content_over_an_asset_id()).unwrap_err();
	assert!(matches!(error, Error::Scene(_)), "{error}");
	assert!(error.to_string().contains("MeshPart.MeshContent") && error.to_string().contains("`None` placeholder"), "{error}");

	// a metadata tag other than the reference column
	let stored = edit_property(&sample_place(), "MeshContent", |rest| {
		let mut out = vec![0x83];
		out.extend_from_slice(rest);
		out
	});
	let error = open(&stored, &[&stored]).unwrap_err();
	assert!(matches!(error, Error::Unsupported(_)), "{error}");
	assert!(error.to_string().contains("MeshPart.MeshContent"), "{error}");
}

#[test]
fn rewriting_a_chunk_as_it_is_changes_nothing_a_parser_reads() {
	let place = sample_place();
	let stored = edit_property(&place, "Name", |rest| rest.to_vec());
	assert_eq!(dump(&stored), dump(&place));
}

#[test]
fn a_placeholder_that_is_not_empty_is_refused() {
	let place = sample_place();
	let bytes = Builder::place(place).externalize(64).build().unwrap();
	let archive_view = Archive::open(&bytes).unwrap();
	let root = archive_view.root().unwrap();
	// put a value back inline where the scene says the bytes live in an object
	let stored = edit_property(root.bytes, "Value", |rest| {
		assert_eq!(rest[0], TAG_EMBEDDED);
		let mut out = rest[..rest.len() - 4].to_vec();
		out.extend_from_slice(&3u32.to_le_bytes());
		out.extend_from_slice(b"abc");
		out
	});
	let mut lines = place_lines(&stored);
	let objects: Vec<&[u8]> = archive_view.objects().map(|(_, bytes)| bytes).filter(|bytes| *bytes != root.bytes).collect();
	for object in &objects {
		lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(object)));
	}
	let mut all: Vec<&[u8]> = vec![&stored];
	all.extend(objects);
	let error = unwrap(&archive(&lines, &all)).unwrap_err();
	assert!(matches!(error, Error::Scene(_)), "{error}");
	assert!(error.to_string().contains("BinaryStringValue.Value"), "{error}");
}

/// The archive with bytes after the last entry of its scene's shared string table.
fn with_padded_table(archived: &[u8]) -> Vec<u8> {
	let stored = edit_table(Archive::open(archived).unwrap().root().unwrap().bytes, |payload| {
		let mut padded = payload.to_vec();
		padded.extend_from_slice(&[0xAB; 37]);
		vec![padded]
	});
	with_scene(archived, &stored)
}

/// The scene with the last entry of its shared string table in a table of its own.
fn split_table(scene_bytes: &[u8]) -> Vec<u8> {
	edit_table(scene_bytes, |payload| {
		let entries = table_entries(payload);
		assert!(entries.len() >= 2);
		let (first, last) = entries.split_at(entries.len() - 1);
		vec![table_of(first), table_of(last)]
	})
}

/// The archive's members under a header that names `algorithm`, each object named as `rename` says.
/// With `swap`, the object holding its first bytes holds its second instead, under the same name.
fn renamed(archived: &[u8], algorithm: &str, rename: &dyn Fn(&str, &[u8]) -> String, swap: Option<(&[u8], &[u8])>) -> Vec<u8> {
	let view = Archive::open(archived).unwrap();
	let root = view.root().unwrap();
	let mut lines = vec![
		format!(r#"{{"kind":"header","version":1,"digestAlgorithm":"{algorithm}"}}"#),
		r#"{"kind":"package","type":"place","place":"Place.rbxl"}"#.to_owned(),
	];
	let mut objects: Vec<(String, &[u8])> = Vec::new();
	for (name, bytes) in view.objects() {
		let renamed = rename(name, bytes);
		let path = if name == root.digest { r#""path":"Place.rbxl","# } else { "" };
		lines.push(format!(r#"{{"kind":"content",{path}"objectDigest":"{renamed}"}}"#));
		let bytes = match swap {
			Some((from, to)) if bytes == from => to,
			_ => bytes,
		};
		objects.push((renamed, bytes));
	}
	archive_named(&lines, &objects)
}

fn same_name(name: &str, _: &[u8]) -> String {
	name.to_owned()
}

/// The long source with other text of the same length: what a swapped object holds.
fn other_source() -> String {
	let other = LONG_SOURCE.replace("hello", "HELLO");
	assert_eq!(other.len(), LONG_SOURCE.len());
	other
}

/// The archive with the last of its `count` script sources referring to an object it does not hold.
fn with_missing_source(archived: &[u8], count: usize) -> Vec<u8> {
	let stored = edit_property(Archive::open(archived).unwrap().root().unwrap().bytes, "Source", |rest| {
		assert_eq!(rest[0], TAG_EMBEDDED);
		let mut reader = Reader::new(&rest[1..]);
		let mut references = scene::read_references(&mut reader, count).unwrap();
		references[count - 1].as_mut().unwrap().digest = [0xEE; 16];
		let mut out = vec![TAG_EMBEDDED];
		write_references(&mut out, &references);
		out.extend_from_slice(reader.rest());
		out
	});
	with_scene(archived, &stored)
}

/// An archive of the sample place with an opaque reference over the mesh content that is an asset id.
fn content_over_an_asset_id() -> Vec<u8> {
	let envelope = blob(40, 120);
	let stored = place_with_opaque_content(&envelope, Some("content-1"), false);
	let mut lines = place_lines(&stored);
	lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(&envelope)));
	archive(&lines, &[&stored, &envelope])
}

/// One block of a zstd frame made by hand.
enum Block<'a> {
	/// These bytes as they are.
	Raw(&'a [u8]),
	/// This many zero bytes, as a run-length block.
	Run(usize),
	/// 128 KiB of zero bytes, as a compressed block: its literals are one run, and it has no sequences.
	Zeros,
}

const BLOCK: usize = 128 * 1024;
/// The start of a frame that declares no size and has a window of 128 KiB.
const WINDOWED: &[u8] = &[0x00, 0x38];

/// A zstd frame from what follows its magic number and its blocks.
fn frame(header: &[u8], blocks: &[Block]) -> Vec<u8> {
	let mut out = vec![0x28, 0xB5, 0x2F, 0xFD];
	out.extend_from_slice(header);
	for (index, block) in blocks.iter().enumerate() {
		let (kind, size, body): (u32, usize, &[u8]) = match block {
			Block::Raw(bytes) => (0, bytes.len(), bytes),
			Block::Run(length) => (1, *length, &[0]),
			Block::Zeros => (2, 5, &[0x0D, 0x00, 0x20, 0x00, 0x00]),
		};
		let last = (index + 1 == blocks.len()) as u32;
		out.extend_from_slice(&((size as u32) << 3 | kind << 1 | last).to_le_bytes()[..3]);
		out.extend_from_slice(body);
	}
	out
}

/// The archive with the compressed chunks of its scene that `pick` chooses, by name and data, stored
/// as `store` says: it gets a chunk's data and returns the bytes to store and the size to claim.
fn stored_as(archived: &[u8], pick: impl Fn(&[u8; 4], &[u8]) -> bool, store: impl Fn(&[u8]) -> (Vec<u8>, usize)) -> Vec<u8> {
	let view = Archive::open(archived).unwrap();
	let scene_bytes = view.root().unwrap().bytes;
	let (list, end) = scene::chunks(scene_bytes).unwrap();
	let mut out = scene_bytes[..scene::HEADER_LEN].to_vec();
	for chunk in &list {
		let mut scratch = Vec::new();
		let payload = scene::data(scene_bytes, chunk, &mut scratch, usize::MAX).unwrap();
		if chunk.compressed && pick(&chunk.name, payload) {
			let (stored, size) = store(payload);
			out.extend_from_slice(&chunk.name);
			out.extend_from_slice(&(stored.len() as u32).to_le_bytes());
			out.extend_from_slice(&(size as u32).to_le_bytes());
			out.extend_from_slice(&[0; 4]);
			out.extend_from_slice(&stored);
		} else {
			out.extend_from_slice(&scene_bytes[chunk.whole.clone()]);
		}
	}
	out.extend_from_slice(&scene_bytes[end..]);
	with_scene(archived, &out)
}

/// Picks the chunks that hold the script sources.
fn sources(name: &[u8; 4], payload: &[u8]) -> bool {
	let mut reader = Reader::new(payload);
	name == b"PROP" && reader.u32("class").is_ok() && reader.sized("name").is_ok_and(|property| property == b"Source")
}

/// Every chunk as a frame that declares its size, the way Studio writes them.
fn as_studio_writes(archived: &[u8]) -> Vec<u8> {
	stored_as(archived, |_, _| true, |payload| (zstd::bulk::compress(payload, 3).unwrap(), payload.len()))
}

/// The script sources as a frame of raw blocks that declares no size.
fn as_raw_blocks(archived: &[u8]) -> Vec<u8> {
	stored_as(archived, sources, |payload| (frame(WINDOWED, &payload.chunks(48).map(Block::Raw).collect::<Vec<_>>()), payload.len()))
}

/// The script sources as a chunk that claims `size` bytes for a frame of these blocks.
fn claiming(archived: &[u8], size: usize, header: &[u8], blocks: &[Block]) -> Vec<u8> {
	stored_as(archived, sources, |_| (frame(header, blocks), size))
}

/// The script sources as two frames, one after the other, that hold the chunk's data between them.
fn as_two_frames(archived: &[u8]) -> Vec<u8> {
	stored_as(archived, sources, |payload| {
		let (first, second) = payload.split_at(payload.len() / 2);
		let mut stored = frame(WINDOWED, &[Block::Raw(first)]);
		stored.extend(frame(WINDOWED, &[Block::Raw(second)]));
		(stored, payload.len())
	})
}

/// 64 blocks of 128 KiB each: 8 MiB from a few hundred bytes.
fn many(block: fn() -> Block<'static>) -> Vec<Block<'static>> {
	(0..64).map(|_| block()).collect()
}

/// Archives of four scripts whose sources sit in a zstd frame the chunk around it does not match:
/// each one's file name for other readers, its bytes, and what this reader says when it refuses it.
fn mismatched_frames() -> [(&'static str, Vec<u8>, &'static str); 6] {
	let scripts = Builder::place(repeating_place(4, 0, 0)).externalize(64).build().unwrap();
	[
		// the chunk claims 64 bytes and the frame's run-length blocks hold 8 MiB: decoding stops at the 64th
		("zstd-overfull.rbx", claiming(&scripts, 64, WINDOWED, &many(|| Block::Run(BLOCK))), "chunk PROP zstd"),
		// the same in compressed blocks, which do not say what they hold
		("zstd-blocks.rbx", claiming(&scripts, 64, WINDOWED, &many(|| Block::Zeros)), "chunk PROP zstd"),
		// the chunk claims 65 bytes and the frame's one block holds 64
		("zstd-underfull.rbx", claiming(&scripts, 65, WINDOWED, &[Block::Run(64)]), "decompressed to 64 of 65 bytes"),
		// the chunk claims 1 GiB less 1 MiB for a frame of ten bytes: nothing is allocated for it
		("zstd-overclaim.rbx", claiming(&scripts, (1 << 30) - (1 << 20), WINDOWED, &[Block::Run(BLOCK)]), "claims 1072693248 bytes from 10"),
		// the chunk claims 65 bytes and the frame declares the 64 it holds
		("zstd-declared.rbx", claiming(&scripts, 65, &[0x20, 64], &[Block::Run(64)]), "claims 65 bytes, and its zstd frame declares 64"),
		// the chunk's data in two frames, one after the other: a chunk holds one
		("zstd-two-frames.rbx", as_two_frames(&scripts), "after its zstd frame"),
	]
}

#[test]
fn a_zstd_chunk_reads_as_an_lz4_one_does() {
	let place = sample_place();
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	for bytes in [as_studio_writes(&archived), as_raw_blocks(&archived)] {
		let scene = unwrap(&bytes).unwrap();
		assert_eq!(dump(&scene.bytes), dump(&place));
		assert_eq!(scene.report.restored_values, 5);
	}
	// the blocks the next test overfills chunks with are sound: they decode to what they say
	let zeros = zstd::bulk::decompress(&frame(WINDOWED, &[Block::Zeros, Block::Run(7), Block::Zeros]), 3 * BLOCK).unwrap();
	assert_eq!(zeros, vec![0; 2 * BLOCK + 7]);
}

#[test]
fn a_zstd_chunk_is_held_to_what_it_claims() {
	for (name, bytes, what) in mismatched_frames() {
		let error = unwrap(&bytes).unwrap_err();
		assert!(matches!(error, Error::Scene(_)), "{name}: {error}");
		assert!(error.to_string().contains(what), "{name}: {error}");
	}
}

#[test]
fn the_scene_limit_counts_every_chunk() {
	let place = sample_place();
	let bytes = Builder::place(place.clone()).externalize(64).build().unwrap();
	let view = Archive::open(&bytes).unwrap();
	let total: u64 = scene::chunks(view.root().unwrap().bytes).unwrap().0.iter().map(|chunk| chunk.size as u64).sum();
	let scene = unwrap_with(&bytes, &Limits { max_scene_bytes: total, ..Limits::default() }).unwrap();
	assert_eq!(dump(&scene.bytes), dump(&place));
	let tight = Limits { max_scene_bytes: total - 1, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("scene bytes"))));
}

#[test]
fn a_chunk_is_written_back_within_the_limit_it_was_read_under() {
	// four copies of the source go back into one chunk, which makes it the scene's largest by far
	let place = repeating_place(4, 0, 0);
	let bytes = Builder::place(place.clone()).externalize(64).build().unwrap();
	let largest = |scene: &[u8]| scene::chunks(scene).unwrap().0.iter().map(|chunk| chunk.size).max().unwrap();
	let view = Archive::open(&bytes).unwrap();
	let stored = largest(view.root().unwrap().bytes);
	let restored = largest(&unwrap(&bytes).unwrap().bytes);
	assert!(restored > stored + 4 * LONG_SOURCE.len() / 2, "{restored} bytes restored from {stored}");
	let scene = unwrap_with(&bytes, &Limits { max_chunk_bytes: restored, ..Limits::default() }).unwrap();
	assert_eq!(dump(&scene.bytes), dump(&place));
	// every chunk of the stored scene is within this limit, and the one written back is not
	let tight = Limits { max_chunk_bytes: restored - 1, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("scene chunk bytes"))));
}

#[test]
fn bytes_after_the_shared_strings_do_not_move_the_restored_ones() {
	let place = sample_place();
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	let padded = with_padded_table(&archived);
	// a parser stops at the table's last entry, so the stored scene reads as it did
	let stored = Archive::open(&padded).unwrap();
	assert_eq!(dump(stored.root().unwrap().bytes), dump(Archive::open(&archived).unwrap().root().unwrap().bytes));
	// and the entries flattening adds go right after that entry, not after the bytes that follow it
	let scene = unwrap(&padded).unwrap();
	assert_eq!(dump(&scene.bytes), dump(&place));
	assert_eq!(scene.report.restored_values, 5);
}

#[test]
fn a_scene_with_two_shared_string_tables_gets_no_value_written_into_one() {
	let place = sample_place();
	// The stored table's last entry gets a table of its own. An entry added to the first table would
	// then take the index that entry has in a parser that counts through both.
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	let stored = split_table(Archive::open(&archived).unwrap().root().unwrap().bytes);
	let error = unwrap(&with_scene(&archived, &stored)).unwrap_err();
	assert!(matches!(error, Error::Unsupported(_)), "{error}");
	assert!(error.to_string().contains("2 shared string tables") && error.to_string().contains("MeshPart.PhysicalConfigData"), "{error}");

	// with nothing to write into a table, such a scene is passed on as it is
	let plain = split_table(&place);
	let wrapped = Builder::place(plain.clone()).build().unwrap();
	let scene = unwrap(&wrapped).unwrap();
	assert!(matches!(scene.bytes, Cow::Borrowed(_)));
	assert_eq!(&*scene.bytes, plain.as_slice());
}

#[test]
fn an_object_is_checked_against_its_reference_under_any_algorithm() {
	let place = sample_place();
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();

	// the same members under a header that names an algorithm this reader does not compute: nothing
	// can be checked against its name, which is said, and every value still comes back
	let unnamed = renamed(&archived, "sha3-256", &same_name, None);
	let scene = unwrap(&unnamed).unwrap();
	assert_eq!(dump(&scene.bytes), dump(&place));
	assert_eq!(scene.report.warnings.len(), 1, "{:?}", scene.report.warnings);
	assert!(scene.report.warnings[0].contains("sha3-256"), "{:?}", scene.report.warnings);

	// an object swapped for other bytes of the same size is not what the property refers to
	let other = other_source();
	let swapped = renamed(&archived, "sha3-256", &same_name, Some((LONG_SOURCE.as_bytes(), other.as_bytes())));
	let error = unwrap(&swapped).unwrap_err();
	assert!(matches!(error, Error::Digest { .. }), "{error}");
	// under the algorithm this reader computes, opening the archive already refuses it
	let swapped = renamed(&archived, "blake3-128", &same_name, Some((LONG_SOURCE.as_bytes(), other.as_bytes())));
	assert!(matches!(Archive::open(&swapped), Err(Error::Digest { .. })));

	// objects named by longer digests cannot be told apart by a reference's 16 bytes: no guess is made
	let full_name = |_: &str, bytes: &[u8]| blake3::hash(bytes).to_hex().to_string();
	let error = unwrap(&renamed(&archived, "blake3-256", &full_name, None)).unwrap_err();
	assert!(matches!(error, Error::Scene(_)), "{error}");
	assert!(error.to_string().contains("blake3-256"), "{error}");
}

#[test]
fn the_restored_bytes_limit_stops_a_column_before_it_is_rebuilt() {
	// scripts only: theirs is the one column that restores anything
	let place = repeating_place(4, 0, 0);
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	assert_eq!(dump(&unwrap(&archived).unwrap().bytes), dump(&place));
	// point the last script's reference at an object the archive does not hold
	let bytes = with_missing_source(&archived, 4);
	// with room for every value, the missing object is what stops the read
	assert!(matches!(unwrap(&bytes), Err(Error::Scene(_))));
	// With room for two copies of the source, the read stops at the third. It does not go on copying
	// to the end of the column, where the missing object would stop it instead.
	let tight = Limits { max_restored_bytes: 2 * LONG_SOURCE.len() as u64 + 1, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("restored bytes"))));
}

#[test]
fn a_shared_string_counts_once_against_the_limit() {
	// four scripts hold a copy of the source each; four mesh parts share one entry of the table
	let place = repeating_place(4, 4, 800);
	let bytes = Builder::place(place.clone()).externalize(64).build().unwrap();
	let written = 4 * LONG_SOURCE.len() as u64 + 800;
	let scene = unwrap_with(&bytes, &Limits { max_restored_bytes: written, ..Limits::default() }).unwrap();
	assert_eq!(dump(&scene.bytes), dump(&place));
	assert_eq!(scene.report.restored_values, 8);
	// the report counts what the values hold, the limit what the file gains
	assert_eq!(scene.report.restored_bytes, 4 * LONG_SOURCE.len() as u64 + 4 * 800);
	let tight = Limits { max_restored_bytes: written - 1, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("restored bytes"))));
}

#[test]
fn limits_hold() {
	let place = sample_place();
	let bytes = Builder::place(place).externalize(64).build().unwrap();
	let tight = Limits { max_restored_bytes: 1000, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("restored bytes"))));
	let tight = Limits { max_archive_bytes: 100, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("archive bytes"))));
	let tight = Limits { max_manifest_bytes: 10, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("manifest bytes"))));
	let tight = Limits { max_chunk_bytes: 8, ..Limits::default() };
	assert!(matches!(unwrap_with(&bytes, &tight), Err(Error::Limit("scene chunk bytes"))));
}

#[test]
fn a_cut_or_altered_archive_never_panics() {
	let place = sample_place();
	let bytes = Builder::place(place).externalize(64).build().unwrap();
	for len in (0..bytes.len()).step_by(7) {
		assert!(unwrap(&bytes[..len]).is_err(), "cut at {len}");
	}
	// flipping any byte either fails cleanly or, where the byte is not checked (a timestamp), still reads
	let mut altered = bytes.clone();
	for at in (0..bytes.len()).step_by(13) {
		altered[at] ^= 0x55;
		let _ = unwrap(&altered);
		altered[at] = bytes[at];
	}
}

#[test]
fn a_scene_cut_inside_the_archive_fails_cleanly() {
	// the container is sound (CRC and digest match) but the scene inside is not
	let place = sample_place();
	let stored = Builder::place(place).externalize(64).build().unwrap();
	let archive_view = Archive::open(&stored).unwrap();
	let root = archive_view.root().unwrap();
	let objects: Vec<&[u8]> = archive_view.objects().map(|(_, bytes)| bytes).filter(|bytes| *bytes != root.bytes).collect();
	for len in (0..root.bytes.len()).step_by(11) {
		let cut = &root.bytes[..len];
		let mut lines = place_lines(cut);
		let mut all: Vec<&[u8]> = vec![cut];
		for object in &objects {
			lines.push(format!(r#"{{"kind":"content","objectDigest":"{}"}}"#, digest(object)));
			all.push(object);
		}
		assert!(unwrap(&archive(&lines, &all)).is_err(), "scene cut at {len}");
	}
}

/// A place whose string and shared string columns hold more than text and bytes: tags, attributes,
/// a content id, and an asset map that shares its shared string with a mesh part.
fn structured_place() -> Vec<u8> {
	let shared = SharedString::new(blob(9, 400));
	let tags: Tags = (0..20).map(|index| format!("tag-number-{index}")).collect::<Vec<_>>().into();
	let attributes = Attributes::new().with("Note", "n".repeat(200));
	let url = format!("https://assetdelivery.roblox.com/v1/asset/?id={}", "7".repeat(60));
	let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_children([
		InstanceBuilder::new("Folder").with_name("Marked").with_property("Tags", tags).with_property("Attributes", attributes),
		// on a class that has no such property, so the writer keeps the content id as it is given
		InstanceBuilder::new("Folder").with_name("Theme").with_property("SoundId", ContentId::from(url)),
		InstanceBuilder::new("Folder").with_name("Map").with_property("ContentMap", shared.clone()),
		InstanceBuilder::new("MeshPart").with_name("Shares").with_property("PhysicalConfigData", shared),
		InstanceBuilder::new("MeshPart").with_name("Alone").with_property("PhysicalConfigData", SharedString::new(blob(11, 500))),
		InstanceBuilder::new("ModuleScript").with_name("Module").with_property("Source", LONG_SOURCE),
	]));
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
	bytes
}

#[test]
fn only_text_and_bytes_move_unless_everything_is_asked_for() {
	let place = structured_place();
	let original = dump(&place);
	for (path, name) in [("/Marked", "Tags"), ("/Marked", "Attributes"), ("/Theme", "SoundId"), ("/Map", "ContentMap")] {
		assert!(property(&original, path, name).is_some_and(|value| value.len() >= 64), "{path}.{name} is in the place");
	}

	let bytes = Builder::place(place.clone()).externalize(64).build().unwrap();
	let archive = Archive::open(&bytes).unwrap();
	// the scene, the source, and the one shared string no property that stays inline points at
	let objects: Vec<&[u8]> = archive.objects().map(|(_, bytes)| bytes).collect();
	assert_eq!(objects.len(), 3);
	assert!(objects.contains(&LONG_SOURCE.as_bytes()));
	assert!(objects.contains(&blob(11, 500).as_slice()));
	assert!(!objects.contains(&blob(9, 400).as_slice()), "the asset map's bytes stay in the scene, for the mesh part too");
	// a parser that does not know archives still reads every structured value from the stored scene
	let stored = dump(archive.root().unwrap().bytes);
	for (path, name) in [("/Marked", "Tags"), ("/Marked", "Attributes"), ("/Theme", "SoundId"), ("/Map", "ContentMap")] {
		assert_eq!(property(&stored, path, name), property(&original, path, name), "{path}.{name} stayed inline");
	}
	assert_eq!(property(&stored, "/Module", "Source"), None);
	let scene = archive.flatten().unwrap();
	assert_eq!(dump(&scene.bytes), original);
	assert_eq!(scene.report.restored_values, 2);

	let bytes = Builder::place(place).externalize_all(64).build().unwrap();
	let archive = Archive::open(&bytes).unwrap();
	// also the tags, the attributes, the content id and the shared string of the asset map
	assert_eq!(archive.objects().count(), 7);
	let stored = dump(archive.root().unwrap().bytes);
	for (path, name) in [("/Marked", "Tags"), ("/Marked", "Attributes"), ("/Theme", "SoundId"), ("/Map", "ContentMap"), ("/Shares", "PhysicalConfigData")] {
		assert_ne!(property(&stored, path, name), property(&original, path, name), "{path}.{name} moved out");
	}
	let scene = archive.flatten().unwrap();
	assert_eq!(dump(&scene.bytes), original);
	// the asset map and the mesh part that shares its bytes count once each
	assert_eq!(scene.report.restored_values, 7);
}

/// Writes the archives above that a reader must refuse or read with care, and the place they are
/// made from, into a folder. Other readers of the format run their tests on them (the Lune port keeps
/// them beside the archives `tools/archive-xcheck` writes), so every reader is held to the same bytes:
///
/// ```sh
/// ROR_ARCHIVE_FIXTURES=<folder> cargo test --release -p ror-archive -- --ignored for_other_readers
/// ```
#[test]
#[ignore = "writes files: set ROR_ARCHIVE_FIXTURES to the folder they go in"]
fn archives_for_other_readers_to_test_against() {
	let folder = std::env::var_os("ROR_ARCHIVE_FIXTURES").expect("ROR_ARCHIVE_FIXTURES names the folder to write into");
	let folder = std::path::Path::new(&folder);
	std::fs::create_dir_all(folder).unwrap();
	let place = sample_place();
	let archived = Builder::place(place.clone()).externalize(64).build().unwrap();
	let stored = split_table(Archive::open(&archived).unwrap().root().unwrap().bytes);
	let other = other_source();
	let scripts = Builder::place(repeating_place(4, 0, 0)).externalize(64).build().unwrap();
	let files = [
		// the place the archives below are made from
		("sample.rbxl", place.clone()),
		// reads as the place: bytes follow the shared string table's last entry
		("table-padding.rbx", with_padded_table(&archived)),
		// refused: two shared string tables, and a value to write into one
		("two-tables.rbx", with_scene(&archived, &stored)),
		// reads as the place, with a warning: the header names an algorithm no reader computes
		("unnamed-digests.rbx", renamed(&archived, "sha3-256", &same_name, None)),
		// refused: the same, with one object's bytes swapped for others of the same length
		("swapped-object.rbx", renamed(&archived, "sha3-256", &same_name, Some((LONG_SOURCE.as_bytes(), other.as_bytes())))),
		// four scripts share one source, and the last refers to an object the archive does not hold:
		// refused for that, or, with room for fewer than three copies, for the limit first
		("limit-order.rbx", with_missing_source(&scripts, 4)),
		// refused: an opaque content reference over a value that is an asset id
		("content-over-uri.rbx", content_over_an_asset_id()),
		// reads as the place: every chunk is a zstd frame that declares its size, as Studio writes them
		("zstd.rbx", as_studio_writes(&archived)),
		// reads as the place: the script sources are a zstd frame of raw blocks that declares no size
		("zstd-raw-blocks.rbx", as_raw_blocks(&archived)),
	];
	// refused, each for its own reason: a chunk and the zstd frame in it that do not match
	let mismatched = mismatched_frames().map(|(name, bytes, _)| (name, bytes));
	for (name, bytes) in files.into_iter().chain(mismatched) {
		std::fs::write(folder.join(name), &bytes).unwrap();
		println!("{name}: {} bytes", bytes.len());
	}
}
