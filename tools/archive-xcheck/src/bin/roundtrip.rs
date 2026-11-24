//! Real files through ror-archive: wrap with values moved out, flatten, and compare what a DOM
//! parser reads before and after. Also feeds the archive to the draft rbx-dom reader.
//!
//!   roundtrip <min_bytes> <file>...
use std::io::Cursor;
use std::time::Instant;

use rbx_dom_weak::types::Variant;
use rbx_dom_weak::WeakDom;

/// One hash over everything a parser read, in tree order, and the instance count. Nothing is kept:
/// a large place has millions of property values.
fn digest(dom: &WeakDom) -> (String, usize) {
	let mut hasher = blake3::Hasher::new();
	let mut count = 0usize;
	let mut stack: Vec<(rbx_dom_weak::types::Ref, u32)> = dom.root().children().iter().rev().map(|r| (*r, 0)).collect();
	while let Some((r, depth)) = stack.pop() {
		let inst = dom.get_by_ref(r).unwrap();
		count += 1;
		hasher.update(&depth.to_le_bytes());
		hasher.update(inst.class.as_bytes());
		hasher.update(&[0]);
		hasher.update(inst.name.as_bytes());
		hasher.update(&[0]);
		let mut props: Vec<(String, blake3::Hash)> = inst
			.properties
			.iter()
			.map(|(name, value)| {
				let hash = match value {
					Variant::String(t) => blake3::hash(t.as_bytes()),
					Variant::BinaryString(b) => blake3::hash(AsRef::<[u8]>::as_ref(b)),
					Variant::SharedString(b) => blake3::hash(b.data()),
					// referents are assigned per parse
					Variant::Ref(_) => blake3::hash(b"ref"),
					other => blake3::hash(format!("{other:?}").as_bytes()),
				};
				(name.to_string(), hash)
			})
			.collect();
		props.sort_by(|a, b| a.0.cmp(&b.0));
		hasher.update(&(props.len() as u32).to_le_bytes());
		for (name, hash) in props {
			hasher.update(name.as_bytes());
			hasher.update(&[0]);
			hasher.update(hash.as_bytes());
		}
		stack.extend(inst.children().iter().rev().map(|c| (*c, depth + 1)));
	}
	(hasher.finalize().to_hex().to_string(), count)
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let min_bytes: usize = args[0].parse().expect("min bytes");
	let mut failed = 0;
	for path in &args[1..] {
		let original = std::fs::read(path).unwrap();
		if !original.starts_with(b"<roblox!") {
			println!("SKIP {path}: not a binary Roblox file");
			continue;
		}
		let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
		let t = Instant::now();
		let (before, instances) = match rbx_binary::from_reader(original.as_slice()) {
			Ok(dom) => digest(&dom),
			Err(e) => {
				println!("SKIP {name}: the DOM parser cannot read the original: {e}");
				continue;
			}
		};
		let parse_ms = t.elapsed().as_millis();
		println!("     {name}: {} bytes, {instances} instances, parse and hash {parse_ms} ms", original.len());

		// an archive with nothing moved out must flatten to the very same bytes
		let untouched = {
			let plain = ror_archive::Builder::place(original.clone()).build().unwrap();
			&*ror_archive::unwrap(&plain).unwrap().bytes == original.as_slice()
		};
		for all in [false, true] {
			let t = Instant::now();
			let builder = ror_archive::Builder::place(original.clone());
			let archive = if all { builder.externalize_all(min_bytes) } else { builder.externalize(min_bytes) }.build().unwrap();
			let pack_ms = t.elapsed().as_millis();
			let objects = ror_archive::Archive::open(&archive).unwrap().objects().count();

			let t = Instant::now();
			let scene = ror_archive::unwrap(&archive).unwrap();
			let flatten_ms = t.elapsed().as_millis();
			let (restored_values, restored_bytes, flat_len) = (scene.report.restored_values, scene.report.restored_bytes, scene.bytes.len());
			let same = digest(&rbx_binary::from_reader(&*scene.bytes).unwrap()).0 == before;
			drop(scene);
			// the draft reader on our archive (it caps archives at 512 MiB); it types its properties, so
			// it only has to take the archive that moves text and bytes
			let draft = match rbx_archive::from_reader(Cursor::new(&archive)) {
				Ok(dom) => {
					if digest(&dom).0 == before { "same".to_owned() } else { "DIFFERENT".to_owned() }
				}
				Err(e) => format!("error: {e}"),
			};
			let ok = same && untouched && (all || draft == "same");
			if !ok {
				failed += 1;
			}
			println!(
				"{} {name} [{}]: archive {} ({objects} objects), flattened {flat_len}; restored {restored_values} values / {restored_bytes} bytes; pack {pack_ms} ms, flatten {flatten_ms} ms; dom {}, plain-wrap {}, draft reader {draft}",
				if ok { "PASS" } else { "FAIL" },
				if all { "everything" } else { "text and bytes" },
				archive.len(),
				if same { "same" } else { "DIFFERENT" },
				if untouched { "identical" } else { "CHANGED" },
			);
		}
	}
	std::process::exit(if failed == 0 { 0 } else { 1 });
}
