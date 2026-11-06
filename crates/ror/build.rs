//! `ROR_TEXTURE_CODE`: a hash of the code that decides a kept texture file's bytes (render::texture_files
//! keeps textures on disk under it, so another build's never stand in for this one's): render/texels.rs
//! (which image a key draws, its mips and compression, how it goes up, the file layout),
//! render/transcode.rs (a transcode's packs joined), ror-image's sources, the locked version and
//! checksum of each crate ror-image depends on (its decoders), and the workspace manifests' lines naming
//! those crates (their features), line endings aside. Nothing else: a rebuild that changes the GPU
//! caches, the residency's reads or an unrelated dependency keeps every texture (render::texels).
//!
//! And x264 with the `x264` feature (`cfg(ror_x264)`: video.rs encodes MP4s in the process; without it
//! through ffmpeg): ror_x264.lib, which vendor/x264/build.py builds, linked from the folder
//! `ROR_X264_LIB_DIR` names, for x86_64-pc-windows-msvc only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A rerun-if-changed path relative to the crate dir, so a build tree cloned to another folder keeps cargo's list matching.
fn rerun(root: &Path, path: &Path) {
	println!("cargo:rerun-if-changed={}", path.strip_prefix(root).unwrap_or(path).display());
}

fn files(path: &Path, out: &mut Vec<PathBuf>) {
	if path.is_dir() {
		let mut entries: Vec<PathBuf> = std::fs::read_dir(path).map(|entries| entries.flatten().map(|entry| entry.path()).collect()).unwrap_or_default();
		entries.sort();
		for entry in entries {
			files(&entry, out);
		}
	} else if path.is_file() {
		out.push(path.to_path_buf());
	}
}

/// A Cargo.lock package: name, version, the rest of its identity (source and checksum) and its
/// dependencies as the lock names them (`name`, `name version` or `name version (source)`).
struct Locked {
	name: String,
	version: String,
	identity: String,
	dependencies: Vec<String>,
}

fn quoted(line: &str) -> Option<String> {
	let start = line.find('"')? + 1;
	let end = start + line[start..].find('"')?;
	Some(line[start..end].to_string())
}

fn lock_packages(text: &str) -> Vec<Locked> {
	let mut out = Vec::new();
	for block in text.split("[[package]]").skip(1) {
		let mut package = Locked { name: String::new(), version: String::new(), identity: String::new(), dependencies: Vec::new() };
		let mut in_dependencies = false;
		for line in block.lines().map(str::trim) {
			if in_dependencies {
				if line.starts_with(']') {
					in_dependencies = false;
				} else if let Some(dependency) = quoted(line) {
					package.dependencies.push(dependency);
				}
				continue;
			}
			let value = || quoted(line).unwrap_or_default();
			if line.starts_with("name =") {
				package.name = value();
			} else if line.starts_with("version =") {
				package.version = value();
			} else if line.starts_with("source =") || line.starts_with("checksum =") {
				package.identity.push_str(&value());
				package.identity.push(' ');
			} else if line.starts_with("dependencies =") {
				in_dependencies = !line.ends_with(']');
			} else if line.starts_with('[') {
				break;
			}
		}
		out.push(package);
	}
	out
}

/// The crate names `manifest`'s `[dependencies]` (and target-specific ones) list, dev and build ones
/// left out.
fn dependency_names(manifest: &str) -> BTreeSet<String> {
	let mut names = BTreeSet::new();
	let mut section = String::new();
	for line in manifest.lines().map(str::trim) {
		if line.starts_with('[') {
			section = line.to_string();
			continue;
		}
		let normal = section == "[dependencies]" || (section.starts_with("[target.") && section.ends_with(".dependencies]"));
		if normal && let Some(key) = line.split(['=', '.']).next().map(str::trim).filter(|key| !key.is_empty() && !key.starts_with('#')) {
			names.insert(key.to_string());
		}
	}
	names
}

/// Every package `root` depends on (not itself), by lock line `name version identity`.
fn closure(packages: &[Locked], root: &str, direct: &BTreeSet<String>) -> BTreeMap<String, String> {
	let find = |reference: &str| -> Option<&Locked> {
		let mut parts = reference.split_whitespace();
		let name = parts.next()?;
		let version = parts.next();
		packages.iter().find(|package| package.name == name && version.is_none_or(|version| package.version == version))
	};
	let mut seen = BTreeMap::new();
	let Some(root) = packages.iter().find(|package| package.name == root) else {
		return seen;
	};
	let mut stack: Vec<&Locked> = root.dependencies.iter().filter(|reference| reference.split_whitespace().next().is_some_and(|name| direct.contains(name))).filter_map(|reference| find(reference)).collect();
	while let Some(package) = stack.pop() {
		let key = format!("{} {}", package.name, package.version);
		if seen.contains_key(&key) {
			continue;
		}
		seen.insert(key.clone(), format!("{key} {}", package.identity));
		stack.extend(package.dependencies.iter().filter_map(|reference| find(reference)));
	}
	seen
}

/// FNV-1a over each source file's name and its bytes without carriage returns, then the locked
/// `crates` and the manifests' lines naming them (features unify across the workspace).
fn code(root: &Path, sources: &[PathBuf], crates: &BTreeMap<String, String>, manifests: &[PathBuf]) -> u64 {
	let mut found = Vec::new();
	for source in sources {
		rerun(root, source);
		files(source, &mut found);
	}
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	let mut feed = |bytes: &[u8]| {
		for &byte in bytes {
			if byte == b'\r' {
				continue;
			}
			hash ^= byte as u64;
			hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
		}
	};
	for file in &found {
		feed(file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default().as_bytes());
		feed(&std::fs::read(file).unwrap_or_default());
	}
	for line in crates.values() {
		feed(line.as_bytes());
		feed(b"\n");
	}
	let names: BTreeSet<&str> = crates.keys().filter_map(|key| key.split(' ').next()).collect();
	for manifest in manifests {
		for line in std::fs::read_to_string(manifest).unwrap_or_default().lines().map(str::trim) {
			if line.split(['=', '.']).next().map(str::trim).is_some_and(|key| names.contains(key)) {
				feed(line.as_bytes());
				feed(b"\n");
			}
		}
	}
	hash
}

fn main() {
	let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
	let workspace = root.join("../..");
	let lock_file = workspace.join("Cargo.lock");
	let manifests = [workspace.join("Cargo.toml"), root.join("Cargo.toml"), root.join("../ror-image/Cargo.toml"), root.join("../ror-mesh/Cargo.toml"), root.join("../ror-assets/Cargo.toml")];
	for manifest in manifests.iter().chain([&lock_file]) {
		rerun(&root, manifest);
	}
	let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
	let packages = lock_packages(&read(&lock_file));
	let image_crates = closure(&packages, "ror-image", &dependency_names(&read(&root.join("../ror-image/Cargo.toml"))));
	let image = [root.join("../ror-image/src"), root.join("../ror-image/Cargo.toml")];
	let texels = code(&root, &[&image[..], &[root.join("src/render/texels.rs"), root.join("src/render/transcode.rs")]].concat(), &image_crates, &manifests);
	println!("cargo:rustc-env=ROR_TEXTURE_CODE={texels:016x}");
	// a GUI image file: its pixels' premultiplying and mips (gui_files.rs, ror-image's) and its pieces'
	// compression (zstd)
	let mut gui_crates = image_crates.clone();
	gui_crates.extend(closure(&packages, "ror", &BTreeSet::from(["zstd".to_string()])));
	let gui = code(&root, &[&image[..], &[root.join("src/render/gui_files.rs")]].concat(), &gui_crates, &manifests);
	println!("cargo:rustc-env=ROR_GUI_CODE={gui:016x}");

	println!("cargo::rustc-check-cfg=cfg(ror_x264)");
	println!("cargo:rerun-if-env-changed=ROR_X264_LIB_DIR");
	let target = std::env::var("TARGET").unwrap_or_default();
	if std::env::var_os("CARGO_FEATURE_X264").is_some() && target == "x86_64-pc-windows-msvc" {
		let named = std::env::var_os("ROR_X264_LIB_DIR").filter(|dir| !dir.is_empty()).map(PathBuf::from);
		let x264 = named.expect("the x264 feature links ror_x264.lib: build it with `python vendor/x264/build.py <x264 checkout> <nasm.exe> <folder>` and set ROR_X264_LIB_DIR to that folder (docs/building.md)");
		let lib = x264.join("ror_x264.lib");
		rerun(&root, &lib);
		assert!(lib.is_file(), "ROR_X264_LIB_DIR: {} holds no ror_x264.lib (vendor/x264/build.py builds it)", x264.display());
		if lib.is_file() {
			println!("cargo:rustc-link-search=native={}", x264.display());
			println!("cargo:rustc-link-lib=static=ror_x264");
			println!("cargo:rustc-cfg=ror_x264");
		}
	}
}
