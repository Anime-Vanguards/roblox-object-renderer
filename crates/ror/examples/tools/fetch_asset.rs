// Fetches assets by content URI or id (through ror's asset cache) and writes their bytes to files in
// a directory, named after each id's label:
//   cargo run -p ror --release --example tools -- fetch_asset <out dir> <label=uri or id>...
pub fn main() -> anyhow::Result<()> {
	let mut args = std::env::args().skip(1);
	let out = std::path::PathBuf::from(args.next().expect("out dir"));
	std::fs::create_dir_all(&out)?;
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for arg in args {
		let (label, uri) = arg.split_once('=').unwrap_or((&arg, &arg));
		let uri = if uri.chars().all(|c| c.is_ascii_digit()) { format!("rbxassetid://{uri}") } else { uri.to_string() };
		match assets.fetch(&uri) {
			Ok(fetched) => {
				let name: String = label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
				let target = out.join(format!("{name}.rbxm"));
				std::fs::write(&target, &fetched.bytes)?;
				println!("{uri}: {} bytes -> {}", fetched.bytes.len(), target.display());
			}
			Err(error) => println!("{uri}: {error}"),
		}
	}
	Ok(())
}
