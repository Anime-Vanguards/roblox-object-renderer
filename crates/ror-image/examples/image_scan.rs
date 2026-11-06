//! Decodes every image found in Roblox's local HTTP cache (`%LOCALAPPDATA%\Roblox\rbx-storage`,
//! RBXH entries) and the Studio install, and reports counts per container / format, failures and
//! throughput.
//!
//! ```text
//! cargo run -p ror-image --release --example image_scan -- [--storage DIR] [--studio DIR] [--rgba] [--mips] [--parallel] [--limit N]
//! ```
//!
//! `--rgba` also expands block formats with `to_rgba8`, `--mips` builds mip chains for RGBA8
//! images (both filters), `--parallel` repeats the decode pass on all cores.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use ror_image::{Container, Image, PixelFormat};

/// One RBXH entry of Roblox's HTTP cache.
struct RbxhEntry<'a> {
	status: u32,
	url: &'a [u8],
	content: &'a [u8],
}

/// RBXH layout: "RBXH", u32 version (2), u32 url length + url, u8, u32 HTTP status, u32 header
/// length, u32 (header hash, 0 without headers), u32 content length, u32 content hash, u32 (0),
/// header bytes, content bytes. Little-endian.
fn parse_rbxh(bytes: &[u8]) -> Option<RbxhEntry<'_>> {
	let u32_at = |offset: usize| -> Option<u32> { bytes.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
	if bytes.get(0..4)? != b"RBXH" || u32_at(4)? != 2 {
		return None;
	}
	let url_len = u32_at(8)? as usize;
	let url = bytes.get(12..12usize.checked_add(url_len)?)?;
	let mut offset = 12 + url_len;
	let _flags = *bytes.get(offset)?;
	offset += 1;
	let status = u32_at(offset)?;
	let header_len = u32_at(offset + 4)? as usize;
	let content_len = u32_at(offset + 12)? as usize;
	offset += 24;
	let content_start = offset.checked_add(header_len)?;
	let content = bytes.get(content_start..content_start.checked_add(content_len)?)?;
	Some(RbxhEntry { status, url, content })
}

/// Rough kind of non-image content, for the report.
fn content_kind(bytes: &[u8]) -> &'static str {
	if bytes.starts_with(b"version ") {
		"mesh"
	} else if bytes.starts_with(b"<roblox!") {
		"rbxm"
	} else if bytes.starts_with(b"<roblox") {
		"rbxmx"
	} else if bytes.starts_with(b"OggS") {
		"ogg"
	} else if bytes.starts_with(b"ID3") || bytes.starts_with(&[0xff, 0xfb]) {
		"mp3"
	} else if bytes.starts_with(b"SLIM") || bytes.starts_with(b"SLMD") {
		"SLIM/SLMD"
	} else if bytes.starts_with(&[0, 1, 0, 0]) || bytes.starts_with(b"OTTO") {
		"font"
	} else if bytes.starts_with(b"{") || bytes.starts_with(b"[") {
		"json"
	} else if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
		"zstd"
	} else {
		"other"
	}
}

#[derive(Default)]
struct Bucket {
	count: usize,
	bytes_in: u64,
	texels: u64,
	decode: Duration,
	failures: Vec<String>,
	has_alpha: usize,
	srgb: BTreeMap<String, usize>,
	extra: BTreeMap<String, usize>,
}

struct Job {
	label: String,
	bytes: Vec<u8>,
	extension: String,
}

fn key(container: Container, format: PixelFormat) -> String {
	format!("{container:?} {format:?}")
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			walk(&path, out);
		} else {
			out.push(path);
		}
	}
}

fn studio_dir() -> Option<PathBuf> {
	let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("Versions");
	let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
	for entry in std::fs::read_dir(local).ok()?.flatten() {
		let exe = entry.path().join("RobloxStudioBeta.exe");
		if let Ok(meta) = exe.metadata() {
			let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
			if best.as_ref().is_none_or(|(t, _)| modified > *t) {
				best = Some((modified, entry.path()));
			}
		}
	}
	best.map(|(_, p)| p)
}

fn main() {
	let mut args = std::env::args().skip(1);
	let mut storage = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Roblox").join("rbx-storage"));
	let mut studio = studio_dir();
	let (mut rgba, mut mips, mut parallel, mut limit) = (false, false, false, usize::MAX);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--storage" => storage = args.next().map(PathBuf::from),
			"--studio" => studio = args.next().map(PathBuf::from),
			"--no-storage" => storage = None,
			"--no-studio" => studio = None,
			"--rgba" => rgba = true,
			"--mips" => mips = true,
			"--parallel" => parallel = true,
			"--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX),
			other => {
				eprintln!("unknown argument {other:?}");
				std::process::exit(2);
			}
		}
	}

	// Collect the work
	let read_start = Instant::now();
	let mut jobs: Vec<Job> = Vec::new();
	let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
	let mut read_bytes = 0u64;

	if let Some(dir) = &storage {
		let mut files = Vec::new();
		walk(dir, &mut files);
		files.sort();
		for path in files.iter().take(limit) {
			let Ok(bytes) = std::fs::read(path) else {
				*skipped.entry("storage: unreadable".into()).or_default() += 1;
				continue;
			};
			read_bytes += bytes.len() as u64;
			let Some(entry) = parse_rbxh(&bytes) else {
				*skipped.entry("storage: not RBXH".into()).or_default() += 1;
				continue;
			};
			if entry.status != 200 {
				*skipped.entry(format!("storage: HTTP {}", entry.status)).or_default() += 1;
				continue;
			}
			if ror_image::sniff(entry.content).is_none() {
				*skipped.entry(format!("storage: {}", content_kind(entry.content))).or_default() += 1;
				continue;
			}
			let _ = entry.url;
			let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
			jobs.push(Job { label: format!("storage/{name}"), bytes: entry.content.to_vec(), extension: String::new() });
		}
	}

	if let Some(root) = &studio {
		let mut files = Vec::new();
		for sub in ["content", "PlatformContent", "ExtraContent"] {
			walk(&root.join(sub), &mut files);
		}
		files.sort();
		for path in files.iter().take(limit) {
			let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
			if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "dds" | "tex" | "ktx2" | "webp" | "gif" | "bmp" | "tga") {
				continue;
			}
			let Ok(bytes) = std::fs::read(path) else {
				continue;
			};
			read_bytes += bytes.len() as u64;
			let label = path.strip_prefix(root).unwrap_or(path).display().to_string();
			jobs.push(Job { label: format!("studio/{label}"), bytes, extension });
		}
	}
	let read_time = read_start.elapsed();
	println!("read {} images ({:.1} MB of files) in {:.2?}", jobs.len(), read_bytes as f64 / 1e6, read_time);
	for (k, v) in &skipped {
		println!("  skipped {v:6} {k}");
	}

	// Single-threaded decode pass
	let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
	let mut failures: BTreeMap<String, Bucket> = BTreeMap::new();
	let mut decoded: Vec<Image> = Vec::new();
	let total_start = Instant::now();
	let mut decode_total = Duration::ZERO;
	for job in &jobs {
		let start = Instant::now();
		let result = ror_image::decode_with_extension(&job.bytes, &job.extension);
		let elapsed = start.elapsed();
		decode_total += elapsed;
		match result {
			Ok(image) => {
				let b = buckets.entry(key(image.container, image.format)).or_default();
				b.count += 1;
				b.bytes_in += job.bytes.len() as u64;
				b.texels += image.width as u64 * image.height as u64 * image.depth as u64 * image.layers as u64;
				b.decode += elapsed;
				b.has_alpha += image.has_alpha as usize;
				*b.srgb.entry(format!("{:?}", image.srgb)).or_default() += 1;
				if image.container == Container::Ktx2 {
					*b.extra.entry(format!("levels={}", image.levels.len())).or_default() += 1;
					if let Some(cs) = image.meta("colorSpace") {
						*b.extra.entry(format!("colorSpace={cs}")).or_default() += 1;
					}
					let pack = image.meta("packIndex").map_or("none".to_string(), |p| p.to_string());
					*b.extra.entry(format!("packIndex={pack}")).or_default() += 1;
				}
				if image.dxt5nm {
					*b.extra.entry("dxt5nm".into()).or_default() += 1;
				}
				if image.depth > 1 {
					*b.extra.entry("volume".into()).or_default() += 1;
				}
				if image.cube {
					*b.extra.entry("cube".into()).or_default() += 1;
				}
				if rgba || mips {
					decoded.push(image);
				}
			}
			Err(e) => {
				let sniffed = ror_image::sniff(&job.bytes).map_or("unknown".to_string(), |c| format!("{c:?}"));
				let b = failures.entry(sniffed).or_default();
				b.count += 1;
				if b.failures.len() < 8 {
					b.failures.push(format!("{}: {e}", job.label));
				}
			}
		}
	}
	let wall = total_start.elapsed();

	println!();
	println!("{:<16} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8}  details", "container/format", "count", "MB in", "Mtexels", "ms total", "MB/s in", "img/s");
	for (k, b) in &buckets {
		let secs = b.decode.as_secs_f64().max(1e-9);
		println!(
			"{:<16} {:>6} {:>10.1} {:>10.1} {:>10.1} {:>10.0} {:>8.0}  alpha={} srgb={:?} {:?}",
			k,
			b.count,
			b.bytes_in as f64 / 1e6,
			b.texels as f64 / 1e6,
			secs * 1e3,
			b.bytes_in as f64 / 1e6 / secs,
			b.count as f64 / secs,
			b.has_alpha,
			b.srgb,
			b.extra
		);
	}
	let ok: usize = buckets.values().map(|b| b.count).sum();
	let bytes_ok: u64 = buckets.values().map(|b| b.bytes_in).sum();
	let failed: usize = failures.values().map(|b| b.count).sum();
	println!();
	println!(
		"decoded {ok} images ({:.1} MB) in {:.2?} decode time ({:.2?} wall), {:.0} MB/s, {:.0} images/s; {failed} failures",
		bytes_ok as f64 / 1e6,
		decode_total,
		wall,
		bytes_ok as f64 / 1e6 / decode_total.as_secs_f64().max(1e-9),
		ok as f64 / decode_total.as_secs_f64().max(1e-9)
	);
	for (k, b) in &failures {
		println!("  failures {k}: {}", b.count);
		for f in &b.failures {
			println!("    {f}");
		}
	}

	if parallel {
		let start = Instant::now();
		let results: Vec<bool> = jobs.par_iter().map(|job| ror_image::decode_with_extension(&job.bytes, &job.extension).is_ok()).collect();
		let elapsed = start.elapsed();
		println!(
			"parallel ({} threads): {} images in {:.2?}, {:.0} MB/s, {:.0} images/s",
			rayon::current_num_threads(),
			results.iter().filter(|&&r| r).count(),
			elapsed,
			bytes_ok as f64 / 1e6 / elapsed.as_secs_f64(),
			results.len() as f64 / elapsed.as_secs_f64()
		);
	}

	if rgba {
		let mut per: BTreeMap<String, (usize, u64, Duration)> = BTreeMap::new();
		for image in decoded.iter().filter(|i| i.format.is_block_compressed()) {
			let start = Instant::now();
			let expanded = ror_image::to_rgba8(image);
			let elapsed = start.elapsed();
			let texels: u64 = expanded.levels.iter().map(|l| l.len() as u64 / 4).sum();
			let e = per.entry(format!("{:?}", image.format)).or_default();
			e.0 += 1;
			e.1 += texels;
			e.2 += elapsed;
		}
		println!();
		for (k, (count, texels, time)) in per {
			println!("to_rgba8 {k}: {count} images, {:.1} Mtexels (all levels) in {:.2?} = {:.0} Mtexels/s", texels as f64 / 1e6, time, texels as f64 / 1e6 / time.as_secs_f64());
		}
	}

	if mips {
		let mut stored = (0usize, 0u64, Duration::ZERO);
		let mut srgb = (0usize, 0u64, Duration::ZERO);
		for image in decoded.iter().filter(|i| i.format == PixelFormat::Rgba8 && i.depth == 1) {
			let texels = image.width as u64 * image.height as u64 * image.layers as u64;
			let mut a = image.clone();
			let start = Instant::now();
			ror_image::generate_mips(&mut a);
			stored.2 += start.elapsed();
			stored.0 += 1;
			stored.1 += texels;
			let mut b = image.clone();
			let start = Instant::now();
			ror_image::generate_mips_srgb(&mut b);
			srgb.2 += start.elapsed();
			srgb.0 += 1;
			srgb.1 += texels;
		}
		println!();
		println!("generate_mips:      {} images, {:.1} Mtexels level 0 in {:.2?} = {:.0} Mtexels/s", stored.0, stored.1 as f64 / 1e6, stored.2, stored.1 as f64 / 1e6 / stored.2.as_secs_f64());
		println!("generate_mips_srgb: {} images, {:.1} Mtexels level 0 in {:.2?} = {:.0} Mtexels/s", srgb.0, srgb.1 as f64 / 1e6, srgb.2, srgb.1 as f64 / 1e6 / srgb.2.as_secs_f64());
	}
}
