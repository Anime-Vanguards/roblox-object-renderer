// Prints the reflection database's default of properties of a class:
//   cargo run -p ror --release --example tools -- class_defaults <ClassName> <Property>...
pub fn main() {
	let mut args = std::env::args().skip(1);
	let class_name = args.next().expect("class");
	let database = ror::scene::props::database();
	let class = database.classes.get(class_name.as_str()).expect("unknown class");
	for property in args {
		println!("{class_name}.{property} = {:?}", database.find_default_property(class, &property));
	}
}
