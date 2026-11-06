// Writes every script's Source of a model / place file into a directory, one .luau file per script
// named by its dotted path (reading how a game plays its effects):
//   cargo run -p ror --release --example tools -- dump_scripts <file> <out dir>
use rbx_dom_weak::types::Variant;

pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let path = args.next().expect("file");
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let document = ror::scene::load::read_file(std::path::Path::new(&path))?;
	let dom = &document.dom;
	let mut stack = vec![dom.root_ref()];
	let mut count = 0;
	while let Some(referent) = stack.pop() {
		let Some(instance) = dom.get_by_ref(referent) else {
			continue;
		};
		stack.extend(instance.children().iter().copied());
		let source = match instance.properties.get(&rbx_dom_weak::ustr("Source")) {
			Some(Variant::String(source)) => source.clone(),
			Some(Variant::BinaryString(bytes)) => String::from_utf8_lossy(bytes.as_ref()).to_string(),
			_ => continue,
		};
		let name: String = document.path_of(referent).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
		let name = if name.len() > 180 { format!("{}-{count}", &name[name.len() - 170..]) } else { name };
		std::fs::write(out.join(format!("{name}.{}.luau", instance.class)), source)?;
		count += 1;
	}
	println!("{count} scripts");
	Ok(())
}
