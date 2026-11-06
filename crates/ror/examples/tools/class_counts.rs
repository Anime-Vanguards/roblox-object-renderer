// Counts instances per class over model / place files (most common first):
//   cargo run -p ror --release --example tools -- class_counts <file> ...
use std::collections::HashMap;

pub fn main() -> anyhow::Result<()> {
	let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
	for path in std::env::args().skip(1) {
		let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
		let dom = &document.dom;
		let mut seen: HashMap<String, usize> = HashMap::new();
		let mut stack = vec![dom.root_ref()];
		while let Some(referent) = stack.pop() {
			let Some(instance) = dom.get_by_ref(referent) else {
				continue;
			};
			*seen.entry(instance.class.to_string()).or_default() += 1;
			stack.extend(instance.children().iter().copied());
		}
		for (class, count) in seen {
			let entry = counts.entry(class).or_default();
			entry.0 += count;
			entry.1 += 1;
		}
	}
	let mut rows: Vec<_> = counts.into_iter().collect();
	rows.sort_by(|a, b| b.1.0.cmp(&a.1.0));
	for (class, (count, files)) in rows {
		println!("{count:7} {files:4} files  {class}");
	}
	Ok(())
}
