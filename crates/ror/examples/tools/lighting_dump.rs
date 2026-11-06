// Prints a place's Lighting children (and the Clouds under Workspace.Terrain) with their properties,
// then the environment ror reads from it:
//   cargo run -p ror --release --example tools -- lighting_dump <place>
pub fn main() -> anyhow::Result<()> {
	let path = std::env::args().nth(1).expect("place file");
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else {
			continue;
		};
		stack.extend(instance.children().iter().copied());
		let parent = dom.get_by_ref(instance.parent()).map(|parent| parent.class.to_string()).unwrap_or_default();
		if !(parent == "Lighting" || instance.class == "Clouds") {
			continue;
		}
		println!("{} {:?} (parent {parent})", instance.class, instance.name);
		let mut properties: Vec<_> = instance.properties.iter().filter(|(name, _)| !matches!(name.as_str(), "UniqueId" | "HistoryId" | "Capabilities" | "Tags" | "Attributes" | "SourceAssetId" | "Sandboxed")).collect();
		properties.sort_by_key(|(name, _)| name.as_str());
		for (name, value) in properties {
			let text = format!("{value:?}");
			println!("  {name} = {}", if text.len() > 160 { &text[..160] } else { &text });
		}
	}
	if let Some(environment) = ror::scene::extract::environment(&document) {
		println!("{environment:#?}");
	}
	Ok(())
}
