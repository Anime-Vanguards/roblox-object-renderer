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
	let manifest = lines.join("\n") + "\n";
	let named: Vec<(String, &[u8])> = objects.iter().map(|bytes| (format!("objects/{}", digest(bytes)), *bytes)).collect();
	let mut members: Vec<(String, &[u8])> = vec![("manifest.jsonl".to_owned(), manifest.as_bytes())];
	members.extend(named);
	zip::write(&members, false)
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

/// The sample place with its mesh parts' `MeshContent` column given one opaque reference, for the
/// part whose content is `None`.
fn place_with_opaque_content(envelope: &[u8], identity: Option<&str>) -> Vec<u8> {
	edit_property(&sample_place(), "MeshContent", |rest| {
		assert_eq!(rest[0], TYPE_CONTENT);
		// three mesh parts; sources are interleaved and transformed, and 0 stays 0
		let sources = scene::read_interleaved(&rest[1..13], 3);
		let mut references = vec![None; 3];
		let none = sources.iter().rposition(|source| *source == 0).unwrap();
		let digest: [u8; 16] = blake3::hash(envelope).as_bytes()[..16].try_into().unwrap();
		references[none] = Some(Reference { digest, size: envelope.len() as u64, identity: identity.map(str::to_owned) });
		let mut out = vec![TAG_EMBEDDED];
		write_references(&mut out, &references);
		out.extend_from_slice(rest);
		out
	})
}

#[test]
fn opaque_content_stays_none_and_is_reported() {
	let envelope = blob(40, 120);
	let stored = place_with_opaque_content(&envelope, Some("content-1"));
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
	let stored = place_with_opaque_content(&envelope, Some("content-1"));
	let error = open(&stored, &[&stored]).unwrap_err();
	assert!(matches!(error, Error::Scene(_)), "{error}");
	assert!(error.to_string().contains("does not hold"), "{error}");

	// a content value pointing at plain bytes (no identity)
	let stored = place_with_opaque_content(&envelope, None);
	assert!(matches!(open(&stored, &[&stored, &envelope]), Err(Error::Scene(_))));

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
