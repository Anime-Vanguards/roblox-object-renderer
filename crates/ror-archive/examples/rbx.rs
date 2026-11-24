//! Inspects, flattens and writes Roblox archives.
//!
//!   cargo run -p ror-archive --release --example rbx -- info <file.rbx>
//!   cargo run -p ror-archive --release --example rbx -- flatten <file.rbx> <out.rbxl|out.rbxm>
//!   cargo run -p ror-archive --release --example rbx -- pack <place.rbxl> <out.rbx> [--model] [--externalize <bytes>]

use std::process::ExitCode;

use ror_archive::{Archive, Builder, SceneFormat};

fn main() -> ExitCode {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let result = match args.first().map(String::as_str) {
		Some("info") if args.len() == 2 => info(&args[1]),
		Some("flatten") if args.len() == 3 => flatten(&args[1], &args[2]),
		Some("pack") if args.len() >= 3 => pack(&args[1], &args[2], &args[3..]),
		_ => {
			eprintln!("usage: rbx info <file.rbx>");
			eprintln!("       rbx flatten <file.rbx> <out.rbxl|out.rbxm>");
			eprintln!("       rbx pack <place.rbxl|model.rbxm> <out.rbx> [--model] [--externalize <bytes>]");
			return ExitCode::from(2);
		}
	};
	match result {
		Ok(()) => ExitCode::SUCCESS,
		Err(message) => {
			eprintln!("error: {message}");
			ExitCode::FAILURE
		}
	}
}

fn read(path: &str) -> Result<Vec<u8>, String> {
	std::fs::read(path).map_err(|error| format!("{path}: {error}"))
}

fn format_name(format: SceneFormat) -> &'static str {
	match format {
		SceneFormat::Binary => "binary",
		SceneFormat::Xml => "XML",
	}
}

fn info(path: &str) -> Result<(), String> {
	let bytes = read(path)?;
	let archive = Archive::open(&bytes).map_err(|error| error.to_string())?;
	let manifest = archive.manifest();
	println!("{path}: {} bytes", bytes.len());
	println!("manifest version {}, digests {}", manifest.version, manifest.digest_algorithm);
	println!("package: {}", manifest.package.kind);
	let objects: Vec<_> = archive.objects().collect();
	let total: usize = objects.iter().map(|(_, bytes)| bytes.len()).sum();
	println!("objects: {} ({total} bytes)", objects.len());
	println!("content records: {}", manifest.content.len());
	for record in &manifest.content {
		let mut line = String::from("  ");
		line.push_str(record.path.as_deref().unwrap_or("(no path)"));
		if let Some(digest) = &record.object_digest {
			let len = archive.object(digest).map_or(0, <[u8]>::len);
			line.push_str(&format!("  {digest}  {len} bytes"));
		}
		if let Some(asset) = &record.asset {
			line.push_str(&format!("  asset {}", asset.id));
			if let Some(version) = asset.version {
				line.push_str(&format!(" v{version}"));
			}
		}
		println!("{line}");
	}
	match archive.flatten() {
		Ok(scene) => {
			println!("scene: {} {}, {} bytes standalone", format_name(scene.format), manifest.package.kind, scene.bytes.len());
			if scene.report.restored_values > 0 {
				println!(
					"  {} property values ({} bytes) are stored outside the scene and were put back",
					scene.report.restored_values, scene.report.restored_bytes
				);
			}
			for content in &scene.report.embedded_content {
				println!(
					"  {}.{}: {} value(s) are opaque content objects ({} bytes); a standalone file reads them as None",
					content.class, content.property, content.values, content.bytes
				);
			}
			for warning in &scene.report.warnings {
				println!("  warning: {warning}");
			}
		}
		Err(error) => {
			for warning in archive.warnings() {
				println!("  warning: {warning}");
			}
			println!("scene: {error}");
		}
	}
	Ok(())
}

fn flatten(input: &str, output: &str) -> Result<(), String> {
	let bytes = read(input)?;
	let scene = ror_archive::unwrap(&bytes).map_err(|error| error.to_string())?;
	std::fs::write(output, &scene.bytes).map_err(|error| format!("{output}: {error}"))?;
	println!(
		"{output}: {} bytes ({}), {} value(s) restored",
		scene.bytes.len(),
		format_name(scene.format),
		scene.report.restored_values
	);
	for content in &scene.report.embedded_content {
		println!("  {}.{}: {} opaque content value(s) read as None", content.class, content.property, content.values);
	}
	for warning in &scene.report.warnings {
		println!("  warning: {warning}");
	}
	Ok(())
}

fn pack(input: &str, output: &str, options: &[String]) -> Result<(), String> {
	let mut model = input.to_ascii_lowercase().ends_with(".rbxm") || input.to_ascii_lowercase().ends_with(".rbxmx");
	let mut externalize = None;
	let mut options = options.iter();
	while let Some(option) = options.next() {
		match option.as_str() {
			"--model" => model = true,
			"--externalize" => {
				let value = options.next().ok_or("--externalize needs a size in bytes")?;
				externalize = Some(value.parse::<usize>().map_err(|_| format!("--externalize {value}: not a size in bytes"))?);
			}
			other => return Err(format!("unknown option {other}")),
		}
	}
	let scene = read(input)?;
	let mut builder = if model { Builder::model(scene) } else { Builder::place(scene) };
	if let Some(min_bytes) = externalize {
		builder = builder.externalize(min_bytes);
	}
	let bytes = builder.build().map_err(|error| error.to_string())?;
	std::fs::write(output, &bytes).map_err(|error| format!("{output}: {error}"))?;
	println!("{output}: {} bytes", bytes.len());
	Ok(())
}
