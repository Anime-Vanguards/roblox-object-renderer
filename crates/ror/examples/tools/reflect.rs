// Prints the reflection database's defaults for classes and the items of enums:
//   cargo run -p ror --example tools -- reflect Lighting Part enum:Material
pub fn main() {
	let database = rbx_reflection_database::get().expect("reflection database");
	println!("database version {:?}", database.version);
	for arg in std::env::args().skip(1) {
		if let Some(name) = arg.strip_prefix("enum:") {
			let Some(descriptor) = database.enums.get(name) else {
				println!("no enum {name}");
				continue;
			};
			let mut items: Vec<_> = descriptor.items.iter().collect();
			items.sort_by_key(|(_, value)| **value);
			println!("enum {name}:");
			for (item, value) in items {
				println!("  {value:5} {item}");
			}
			continue;
		}
		let Some(class) = database.classes.get(arg.as_str()) else {
			println!("no class {arg}");
			continue;
		};
		println!("class {} : {:?}", class.name, class.superclass);
		let mut chain = vec![class];
		while let Some(parent) = chain.last().and_then(|c| c.superclass).and_then(|s| database.classes.get(s)) {
			chain.push(parent);
		}
		for descriptor in chain {
			let mut props: Vec<_> = descriptor.default_properties.iter().collect();
			props.sort_by_key(|(name, _)| *name);
			for (name, value) in props {
				println!("  [{}] {name} = {value:?}", descriptor.name);
			}
		}
	}
}
