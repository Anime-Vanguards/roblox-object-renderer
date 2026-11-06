//! Lists the content URIs referenced by Roblox model / place files (`.rbxm`, `.rbxmx`, `.rbxl`,
//! `.rbxlx`), grouped by `Class.Property`: a source of real asset ids for tests.
//!
//! ```text
//! cargo run -p ror-assets --example asset_ids -- <file or dir>...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rbx_dom_weak::types::Variant;

fn walk(path: &Path, out: &mut Vec<PathBuf>) {
	if path.is_dir() {
		for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
			walk(&entry.path(), out);
		}
	} else if let Some(ext) = path.extension().and_then(|e| e.to_str())
		&& matches!(ext.to_ascii_lowercase().as_str(), "rbxm" | "rbxmx" | "rbxl" | "rbxlx")
	{
		out.push(path.to_path_buf());
	}
}

fn main() {
	let mut files = Vec::new();
	for arg in std::env::args().skip(1) {
		walk(Path::new(&arg), &mut files);
	}
	// uri -> (properties it appears in, count)
	let mut uris: BTreeMap<String, (BTreeMap<String, usize>, usize)> = BTreeMap::new();
	for file in &files {
		let Ok(bytes) = std::fs::read(file) else { continue };
		let dom = if bytes.starts_with(b"<roblox!") { rbx_binary::from_reader(&bytes[..]).ok() } else { rbx_xml::from_reader_default(&bytes[..]).ok() };
		let Some(dom) = dom else {
			eprintln!("could not parse {}", file.display());
			continue;
		};
		for instance in dom.descendants() {
			for (name, value) in &instance.properties {
				let uri = match value {
					Variant::ContentId(c) => c.as_str().to_string(),
					Variant::Content(c) => match c.as_uri() {
						Some(u) => u.to_string(),
						None => continue,
					},
					_ => continue,
				};
				if uri.is_empty() {
					continue;
				}
				let entry = uris.entry(uri).or_default();
				*entry.0.entry(format!("{}.{}", instance.class, name)).or_default() += 1;
				entry.1 += 1;
			}
		}
	}
	println!("{} files", files.len());
	for (uri, (props, count)) in &uris {
		let props: Vec<String> = props.iter().map(|(p, n)| format!("{p}x{n}")).collect();
		println!("{count:5}  {uri}  [{}]", props.join(", "));
	}
}
