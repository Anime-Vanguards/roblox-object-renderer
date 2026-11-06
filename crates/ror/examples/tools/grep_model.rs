// Searches every string-like property of a model for a substring:
//   cargo run -p ror --example tools -- grep_model <file.rbxm> <needle> [context chars]
use rbx_dom_weak::types::Variant;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let needle = args.next().expect("needle").to_lowercase();
	let context: usize = args.next().map(|value| value.parse().unwrap_or(80)).unwrap_or(80);
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	let mut seen = std::collections::BTreeSet::new();
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else { continue };
		stack.extend(instance.children().iter().copied());
		for (name, value) in &instance.properties {
			let text = match value {
				Variant::String(text) => text.clone(),
				Variant::ContentId(content) => content.as_str().to_string(),
				Variant::Content(content) => content.as_uri().unwrap_or_default().to_string(),
				Variant::BinaryString(bytes) => String::from_utf8_lossy(bytes.as_ref()).to_string(),
				_ => continue,
			};
			let lower = text.to_lowercase();
			let mut start = 0;
			while let Some(position) = lower[start..].find(&needle) {
				let at = start + position;
				let from = lower[..at].char_indices().rev().nth(context).map(|(i, _)| i).unwrap_or(0);
				let to = (at + needle.len() + context).min(text.len());
				let to = (to..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
				let snippet: String = text[from..to].chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
				if seen.insert(snippet.clone()) {
					println!("{}.{name}: {snippet}", document.path_of(referent));
				}
				start = at + needle.len();
			}
		}
	}
	Ok(())
}
