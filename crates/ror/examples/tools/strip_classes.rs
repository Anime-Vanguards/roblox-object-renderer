// Copies a model without the instances of the given classes (and their descendants), e.g. a rig
// without its Humanoid or joints; `--anchor` anchors every part:
//   cargo run -p ror --release --example tools -- strip_classes <in.rbxm> <out.rbxm> [--anchor] <ClassName>...
use rbx_dom_weak::types::Variant;

pub fn main() -> anyhow::Result<()> {
	let mut args: Vec<String> = std::env::args().skip(1).collect();
	let input = args.remove(0);
	let output = args.remove(0);
	let anchor = args.iter().any(|arg| arg == "--anchor");
	let classes: Vec<String> = args.into_iter().filter(|arg| arg != "--anchor").collect();
	let mut document = ror::scene::load::read_file(std::path::Path::new(&input))?;
	let mut doomed = Vec::new();
	let mut parts = Vec::new();
	let mut stack = document.roots.clone();
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else {
			continue;
		};
		if classes.iter().any(|class| class == instance.class.as_str()) {
			doomed.push(referent);
			continue;
		}
		if instance.properties.contains_key(&rbx_dom_weak::ustr("Anchored")) {
			parts.push(referent);
		}
		stack.extend(instance.children().iter().copied());
	}
	for referent in &doomed {
		document.dom.destroy(*referent);
	}
	if anchor {
		for referent in &parts {
			if let Some(instance) = document.dom.get_by_ref_mut(*referent) {
				instance.properties.insert(rbx_dom_weak::ustr("Anchored"), Variant::Bool(true));
			}
		}
	}
	let roots: Vec<_> = document.roots.iter().copied().filter(|root| document.dom.get_by_ref(*root).is_some()).collect();
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &document.dom, &roots).map_err(|error| anyhow::anyhow!("{error}"))?;
	std::fs::write(&output, bytes)?;
	println!("removed {} instances, {} parts{}", doomed.len(), parts.len(), if anchor { " anchored" } else { "" });
	Ok(())
}
