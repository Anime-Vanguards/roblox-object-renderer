// Prints a model's or place's instance tree down to a depth, with each instance's count of BaseParts
// beneath it (picking instances to compare with `--select`):
//   cargo run -p ror --release --example tools -- place_tree <file> [dotted path] [depth]
use rbx_dom_weak::types::Ref;
use rbx_dom_weak::WeakDom;

fn parts(dom: &WeakDom, referent: Ref) -> usize {
	let Some(instance) = dom.get_by_ref(referent) else {
		return 0;
	};
	let own = usize::from(ror::scene::props::class_is_a(instance.class.as_str(), "BasePart"));
	own + instance.children().iter().map(|&child| parts(dom, child)).sum::<usize>()
}

fn print(dom: &WeakDom, referent: Ref, path: &str, depth: usize, max_depth: usize) {
	let Some(instance) = dom.get_by_ref(referent) else {
		return;
	};
	println!("{}{path} [{}] {} parts", "  ".repeat(depth), instance.class, parts(dom, referent));
	if depth >= max_depth {
		return;
	}
	for &child in instance.children() {
		if let Some(child_instance) = dom.get_by_ref(child) {
			print(dom, child, &format!("{path}.{}", child_instance.name), depth + 1, max_depth);
		}
	}
}

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let select = args.next();
	let max_depth: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(2);
	let mut document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	if let Some(select) = &select {
		document.select(select)?;
	}
	for &root in &document.roots.clone() {
		let name = document.dom.get_by_ref(root).map(|instance| instance.name.clone()).unwrap_or_default();
		print(&document.dom, root, select.as_deref().unwrap_or(&name), 0, max_depth);
	}
	Ok(())
}
