// Prints the properties of every instance of a class (and its children's classes / names):
//   cargo run -p ror --example tools -- instance_props <file> <ClassName> [max instances]
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let class = args.next().expect("class");
	let limit: usize = args.next().map(|value| value.parse().unwrap_or(5)).unwrap_or(5);
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	let mut shown = 0;
	let mut total = 0;
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else { continue };
		stack.extend(instance.children().iter().copied());
		if instance.class != class {
			continue;
		}
		total += 1;
		if shown >= limit {
			continue;
		}
		shown += 1;
		// the instance's path from the root, as `--select` takes it
		let mut names = Vec::new();
		let mut parent = instance.parent();
		while let Some(ancestor) = dom.get_by_ref(parent) {
			if ancestor.referent() == dom.root_ref() {
				break;
			}
			names.push(ancestor.name.clone());
			parent = ancestor.parent();
		}
		names.reverse();
		println!("{} {:?} in {}", instance.class, instance.name, names.join("."));
		let mut properties: Vec<_> = instance.properties.iter().collect();
		properties.sort_by_key(|(name, _)| name.as_str());
		for (name, value) in properties {
			let text = format!("{value:?}");
			println!("  {name} = {}", if text.len() > 160 { &text[..160] } else { &text });
		}
		for &child in instance.children() {
			if let Some(child) = dom.get_by_ref(child) {
				println!("  child {} {:?}", child.class, child.name);
			}
		}
	}
	println!("{total} instances of {class}");
	Ok(())
}
