//! Fetches content URIs and prints source, size, sniffed type and timing per URI.
//!
//! ```text
//! cargo run -p ror-assets --release --example fetch -- <uri>... [--state-dir DIR] [--offline] [--refresh] [--repo-dir DIR] [--parallel] [--transcode image|color|normal|mr] [--pack N]
//! ```
//!
//! `--transcode` fetches a pack of the asset's KTX2 texture transcode instead (`image`: an image
//! asset; `color`, `normal`, `mr`: a TexturePack's layers), pack 0 unless `--pack` says otherwise.
//!
//! The account cookie (for assets that need authentication) is read by the library from its
//! documented sources, e.g. `ROR_COOKIE_ENV=<path to an env file>`; this program only ever prints
//! where it came from, never the value.

use std::time::Instant;

use ror_assets::{AssetConfig, Assets, Transcode};

fn main() {
	let mut config = AssetConfig::new(".cache");
	let mut uris: Vec<String> = Vec::new();
	let mut parallel = false;
	let mut transcode = None;
	let mut pack = 0u32;
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--state-dir" => config.state_dir = args.next().unwrap_or_else(|| usage()).into(),
			"--repo-dir" => config.repo_dir = Some(args.next().unwrap_or_else(|| usage()).into()),
			"--studio" => config.studio_content = Some(args.next().unwrap_or_else(|| usage()).into()),
			"--header" => {
				let header = args.next().unwrap_or_else(|| usage());
				let (name, value) = header.split_once(':').unwrap_or_else(|| usage());
				config.assetdelivery_headers.push((name.trim().to_string(), value.trim().to_string()));
			}
			"--transcode" => {
				transcode = Some(match args.next().unwrap_or_else(|| usage()).as_str() {
					"image" => Transcode::Image,
					"color" => Transcode::PackColor,
					"normal" => Transcode::PackNormal,
					"mr" => Transcode::PackMetalnessRoughness,
					_ => usage(),
				})
			}
			"--pack" => pack = args.next().and_then(|value| value.parse().ok()).unwrap_or_else(|| usage()),
			"--offline" => config.offline = true,
			"--refresh" => config.refresh = true,
			"--parallel" => parallel = true,
			"-h" | "--help" => usage(),
			_ => uris.push(arg),
		}
	}
	if uris.is_empty() {
		usage();
	}

	let assets = Assets::new(config);
	println!("state dir: {}", assets.config().state_dir.display());
	match assets.studio_content_dir() {
		Some(dir) => println!("studio content: {}", dir.display()),
		None => println!("studio content: not found"),
	}
	match assets.cookie_source() {
		Some(source) => println!("cookie: available ({source})"),
		None => {
			println!("cookie: not configured");
			for line in assets.cookie_diagnostics() {
				println!("  {line}");
			}
		}
	}

	let fetch = |uri: &str| match transcode {
		Some(transcode) => assets.fetch_transcode(uri, transcode, pack),
		None => assets.fetch(uri),
	};
	let start = Instant::now();
	if parallel && transcode.is_none() {
		let results = assets.fetch_many(&uris);
		for (uri, result) in uris.iter().zip(results) {
			report(uri, result, None);
		}
	} else {
		for uri in &uris {
			let t = Instant::now();
			let result = fetch(uri);
			report(uri, result, Some(t.elapsed()));
		}
	}
	println!("total {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
}

fn report(uri: &str, result: Result<std::sync::Arc<ror_assets::Fetched>, ror_assets::AssetError>, elapsed: Option<std::time::Duration>) {
	let time = elapsed.map_or(String::new(), |d| format!("{:8.1} ms  ", d.as_secs_f64() * 1e3));
	match result {
		Ok(fetched) => println!("{time}{:<13} {:>10} bytes  {:<12} {uri}", format!("{:?}", fetched.source), fetched.bytes.len(), fetched.kind()),
		// AssetError messages carry the URI and a status / short description only
		Err(error) => println!("{time}{:<13} {error}", "error"),
	}
}

fn usage() -> ! {
	eprintln!("usage: fetch <uri>... [--state-dir DIR] [--offline] [--refresh] [--repo-dir DIR] [--studio DIR] [--parallel] [--header 'Name: value'] [--transcode image|color|normal|mr] [--pack N]");
	std::process::exit(2);
}
