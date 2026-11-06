//! Decodes every mesh-like asset it can find and reports counts, failures, sanity-check violations
//! and decode throughput.
//!
//! ```text
//! cargo run -p ror-mesh --release --example mesh_scan -- [options] [paths...]
//! ```
//!
//! Paths may be files or directories (searched recursively); files may be raw assets or Roblox
//! cache records (`RBXH`). Inside directories, files with an extension are only read when it is
//! `.mesh`, `.rbxm`, `.rbxmx` or `.bin`. Without paths it scans `%LOCALAPPDATA%\Roblox\rbx-storage`,
//! the blobs stored inline in `rbx-storage.db` (read from a temporary copy, never in place) and the
//! newest Studio install under `%LOCALAPPDATA%\Roblox\Versions`.
//!
//! Options: `--no-db`, `--db <file>`, `--examples <n>` (examples printed per failure kind / check,
//! default 5), `--stats` (per-format convention statistics), `--list-issues` (every issue),
//! `--list <what>` (print the origin of every decoded mesh whose format label contains `<what>`, or
//! that is `skinned` / has `facs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ror_mesh::validate::{self, Severity, Stats};
use ror_mesh::{Kind, Mesh, MeshError, MeshFormat, rbx_storage};

#[derive(Default)]
struct FormatTally {
	decoded: usize,
	bytes: usize,
	vertices: usize,
	triangles: usize,
	times: Vec<Duration>,
	stats: Stats,
	skinned: usize,
	with_facs: usize,
	with_lods: usize,
	tangents_usable: usize,
	non_white: usize,
}

#[derive(Default)]
struct Report {
	formats: BTreeMap<String, FormatTally>,
	failures: BTreeMap<String, Vec<String>>,
	issues: BTreeMap<(Severity, &'static str), Vec<String>>,
	meshes_with: BTreeMap<Severity, usize>,
	skipped: BTreeMap<&'static str, usize>,
	inputs: usize,
	input_bytes: u64,
	list: Option<String>,
}

fn main() {
	let mut args = std::env::args().skip(1);
	let mut paths = Vec::new();
	let mut use_db = true;
	let mut db_path = None;
	let mut examples = 5;
	let mut show_stats = false;
	let mut list_issues = false;
	let mut list = None;
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--list" => list = args.next(),
			"--no-db" => use_db = false,
			"--db" => db_path = args.next().map(PathBuf::from),
			"--examples" => examples = args.next().and_then(|value| value.parse().ok()).unwrap_or(5),
			"--stats" => show_stats = true,
			"--list-issues" => list_issues = true,
			_ => paths.push(PathBuf::from(arg)),
		}
	}
	if paths.is_empty() {
		match rbx_storage::default_dir() {
			Some(dir) => {
				if db_path.is_none() {
					db_path = Some(dir.with_extension("db"));
				}
				paths.push(dir);
			}
			None => eprintln!("no rbx-storage directory found"),
		}
		if let Some(install) = newest_studio_install() {
			paths.push(install);
		}
	}
	if !use_db {
		db_path = None;
	}

	println!("scanning:");
	for path in &paths {
		println!("  {}", path.display());
	}
	if let Some(db) = &db_path {
		println!("  {} (inline blobs, read from a copy)", db.display());
	}

	let started = Instant::now();
	let mut report = Report { list, ..Report::default() };
	for path in &paths {
		walk(path, true, &mut |origin, bytes| scan_item(&origin, bytes, &mut report));
	}
	if let Some(db) = &db_path
		&& let Err(error) = read_db(db, &mut |origin, bytes| scan_item(&origin, bytes, &mut report))
	{
		eprintln!("rbx-storage.db not read: {error}");
	}
	println!(
		"{} inputs, {:.1} MB, {:.2} s wall (including I/O and checks)",
		report.inputs,
		report.input_bytes as f64 / 1e6,
		started.elapsed().as_secs_f64()
	);
	print_report(&report, examples, show_stats, list_issues);
}

fn scan_item(origin: &str, bytes: &[u8], report: &mut Report) {
	report.inputs += 1;
	report.input_bytes += bytes.len() as u64;
	let content = match rbx_storage::parse_rbxh(bytes) {
		Some(entry) => entry.content,
		None => bytes,
	};
	let kind = ror_mesh::sniff(content);
	match kind {
		Kind::Unknown => {
			*report.skipped.entry("not a mesh").or_default() += 1;
			return;
		}
		Kind::Model if !contains(content, b"MeshData") => {
			*report.skipped.entry("model without MeshData").or_default() += 1;
			return;
		}
		_ => {}
	}
	let start = Instant::now();
	let result = ror_mesh::decode_any(content);
	let elapsed = start.elapsed();
	let mesh = match result {
		Ok(mesh) => mesh,
		Err(MeshError::NoUnionData) => {
			*report.skipped.entry("model without union mesh data").or_default() += 1;
			return;
		}
		Err(error) => {
			let key = format!("{} -> {}", label_for_failure(content, kind), error_kind(&error));
			report.failures.entry(key).or_default().push(format!("{origin}: {error}"));
			return;
		}
	};
	let label = format_label(&mesh, content);
	if let Some(what) = &report.list {
		let listed = match what.as_str() {
			"skinned" => mesh.skinning.is_some(),
			"facs" => mesh.facs.is_some(),
			_ => label.contains(what.as_str()),
		};
		if listed {
			println!("LIST {origin} [{}]", describe(&mesh));
		}
	}
	let tally = report.formats.entry(label).or_default();
	tally.decoded += 1;
	tally.bytes += content.len();
	tally.vertices += mesh.vertex_count();
	tally.triangles += mesh.lod0().len() / 3;
	tally.times.push(elapsed);
	tally.skinned += mesh.skinning.is_some() as usize;
	tally.with_facs += mesh.facs.is_some() as usize;
	tally.with_lods += (mesh.lods.len() > 1) as usize;
	tally.tangents_usable += validate::tangents_usable(&mesh) as usize;
	tally.non_white += mesh.colors.iter().any(|color| *color != [255; 4]) as usize;
	accumulate(&mut tally.stats, &validate::stats(&mesh));

	let issues = validate::check(&mesh);
	let mut severities: Vec<Severity> = issues.iter().map(|issue| issue.severity).collect();
	severities.dedup();
	severities.sort();
	severities.dedup();
	for severity in severities {
		*report.meshes_with.entry(severity).or_default() += 1;
	}
	for issue in issues {
		report.issues.entry((issue.severity, issue.check)).or_default().push(format!(
			"{origin} [{}]: {}",
			describe(&mesh),
			issue.detail
		));
	}
}

fn describe(mesh: &Mesh) -> String {
	format!("{:?}, {} vertices, {} triangles", mesh.format, mesh.vertex_count(), mesh.indices.len() / 3)
}

fn format_label(mesh: &Mesh, content: &[u8]) -> String {
	match mesh.format {
		MeshFormat::V1 { minor } => format!("version 1.0{minor}"),
		MeshFormat::V2 => "version 2.00".into(),
		MeshFormat::V3 { minor } => format!("version 3.0{minor}"),
		MeshFormat::V4 { minor } => format!("version 4.0{minor}"),
		MeshFormat::V5 => "version 5.00".into(),
		MeshFormat::Chunked { major, draco } => format!("version {major}.00 (COREMESH v{})", if draco { 2 } else { 1 }),
		MeshFormat::Union { version } => {
			if content.starts_with(b"<roblox") {
				format!("union asset model (CSGMDL {version})")
			} else {
				format!("CSGMDL {version}")
			}
		}
		MeshFormat::Slim { version, draco } => format!("SLIM v{version} (COREMESH v{})", if draco { 2 } else { 1 }),
		MeshFormat::Solid { version } => format!("SolidMesh v{version}"),
		MeshFormat::Unknown => "unknown".into(),
	}
}

fn label_for_failure(content: &[u8], kind: Kind) -> String {
	match kind {
		Kind::FileMesh => String::from_utf8_lossy(&content[..content.len().min(12)]).into_owned(),
		Kind::Slim => "SLIM".into(),
		Kind::Union => "union data".into(),
		Kind::Model => "model".into(),
		Kind::Unknown => "unknown".into(),
	}
}

fn error_kind(error: &MeshError) -> String {
	match error {
		MeshError::UnknownHeader(_) => "unknown header".into(),
		MeshError::UnsupportedVersion(version) => format!("unsupported {version}"),
		MeshError::Truncated(what) => format!("truncated {what}"),
		MeshError::Invalid(_) => "invalid".into(),
		MeshError::Draco(_) => "draco".into(),
		MeshError::CsgReference(_) => "CSGK reference".into(),
		MeshError::NoUnionData => "no union data".into(),
	}
}

fn accumulate(total: &mut Stats, add: &Stats) {
	total.triangles += add.triangles;
	total.degenerate += add.degenerate;
	total.against_normals += add.against_normals;
	total.vertices += add.vertices;
	total.bad_normal_length += add.bad_normal_length;
	total.zero_normals += add.zero_normals;
	total.non_finite += add.non_finite;
	total.non_finite_elsewhere += add.non_finite_elsewhere;
	total.bad_tangent_length += add.bad_tangent_length;
	total.tangent_not_perpendicular += add.tangent_not_perpendicular;
	total.bad_tangent_sign += add.bad_tangent_sign;
	total.uv_frames += add.uv_frames;
	total.tangent_along_u += add.tangent_along_u;
	total.bitangent_along_v += add.bitangent_along_v;
}

fn print_report(report: &Report, examples: usize, show_stats: bool, list_issues: bool) {
	println!();
	println!(
		"{:<34} {:>6} {:>8} {:>10} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
		"format", "count", "MB", "triangles", "MB/s", "mean ms", "p50 ms", "p99 ms", "max ms", "usableT"
	);
	let mut all_times = Vec::new();
	let mut all_bytes = 0usize;
	let mut decoded = 0usize;
	for (label, tally) in &report.formats {
		let mut times = tally.times.clone();
		times.sort();
		all_times.extend_from_slice(&times);
		all_bytes += tally.bytes;
		decoded += tally.decoded;
		print_timing_row(label, tally.decoded, tally.bytes, tally.triangles, &times, tally.tangents_usable);
	}
	all_times.sort();
	let triangles = report.formats.values().map(|tally| tally.triangles).sum();
	let usable = report.formats.values().map(|tally| tally.tangents_usable).sum();
	print_timing_row("total", decoded, all_bytes, triangles, &all_times, usable);

	let failed: usize = report.failures.values().map(Vec::len).sum();
	println!();
	println!("inputs {}, decoded {decoded}, failed {failed}, skipped {:?}", report.inputs, report.skipped);
	for (kind, list) in &report.failures {
		println!("FAIL {kind}: {}", list.len());
		for example in list.iter().take(examples) {
			println!("    {example}");
		}
	}

	println!();
	println!(
		"sanity: of {decoded} decoded meshes, {} have errors, {} warnings, {} notes",
		report.meshes_with.get(&Severity::Error).copied().unwrap_or(0),
		report.meshes_with.get(&Severity::Warning).copied().unwrap_or(0),
		report.meshes_with.get(&Severity::Note).copied().unwrap_or(0)
	);
	for ((severity, check), list) in report.issues.iter().rev() {
		println!("{} {check}: {} meshes", format!("{severity:?}").to_uppercase(), list.len());
		for example in list.iter().take(if list_issues { usize::MAX } else { examples }) {
			println!("    {example}");
		}
	}

	if show_stats {
		println!();
		println!(
			"{:<34} {:>9} {:>7} {:>7} {:>9} {:>7} {:>7} {:>9} {:>9} {:>9} {:>8} {:>8}",
			"conventions (LOD0)",
			"tris",
			"degen",
			"against",
			"verts",
			"zeroN",
			"NaN",
			"uvFrames",
			"T+u",
			"B+v",
			"badTlen",
			"T.N!=0"
		);
		for (label, tally) in &report.formats {
			let s = &tally.stats;
			println!(
				"{label:<34} {:>9} {:>7} {:>7} {:>9} {:>7} {:>7} {:>9} {:>9} {:>9} {:>8} {:>8}",
				s.triangles,
				s.degenerate,
				s.against_normals,
				s.vertices,
				s.zero_normals,
				s.non_finite + s.non_finite_elsewhere,
				s.uv_frames,
				s.tangent_along_u,
				s.bitangent_along_v,
				s.bad_tangent_length,
				s.tangent_not_perpendicular
			);
		}
		println!();
		println!(
			"{:<34} {:>8} {:>8} {:>8} {:>10} {:>9}",
			"features", "skinned", "FACS", "LODs>1", "vertices", "nonwhite"
		);
		for (label, tally) in &report.formats {
			println!(
				"{label:<34} {:>8} {:>8} {:>8} {:>10} {:>9}",
				tally.skinned, tally.with_facs, tally.with_lods, tally.vertices, tally.non_white
			);
		}
	}
}

fn print_timing_row(label: &str, count: usize, bytes: usize, triangles: usize, sorted: &[Duration], usable: usize) {
	let total: Duration = sorted.iter().sum();
	let ms = |duration: Duration| duration.as_secs_f64() * 1e3;
	let pick = |fraction: f64| {
		sorted.get(((sorted.len() as f64 - 1.0) * fraction).round() as usize).copied().unwrap_or_default()
	};
	let mean = if count == 0 { 0.0 } else { ms(total) / count as f64 };
	let throughput = if total.is_zero() { 0.0 } else { bytes as f64 / 1e6 / total.as_secs_f64() };
	println!(
		"{label:<34} {count:>6} {:>8.2} {triangles:>10} {throughput:>8.1} {mean:>8.3} {:>8.3} {:>8.3} {:>8.3} {usable:>8}",
		bytes as f64 / 1e6,
		ms(pick(0.5)),
		ms(pick(0.99)),
		ms(sorted.last().copied().unwrap_or_default()),
	);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
	haystack.windows(needle.len()).any(|window| window == needle)
}

/// Calls `visit` with every readable file under `path` (inside directories: extension-less files and
/// mesh-like extensions only).
fn walk(path: &Path, explicit: bool, visit: &mut dyn FnMut(String, &[u8])) {
	if path.is_dir() {
		let Ok(entries) = std::fs::read_dir(path) else {
			return;
		};
		let mut children: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
		children.sort();
		for child in children {
			walk(&child, false, visit);
		}
		return;
	}
	if !explicit {
		let extension = path.extension().map(|extension| extension.to_ascii_lowercase());
		let wanted = match extension.as_ref().and_then(|extension| extension.to_str()) {
			None => true,
			Some(extension) => matches!(extension, "mesh" | "rbxm" | "rbxmx" | "bin"),
		};
		if !wanted {
			return;
		}
	}
	let Ok(bytes) = std::fs::read(path) else {
		return;
	};
	visit(path.display().to_string(), &bytes);
}

/// Visits every inline blob of a copy of `rbx-storage.db` (with its WAL), so the live database is
/// never opened.
fn read_db(db: &Path, visit: &mut dyn FnMut(String, &[u8])) -> Result<(), String> {
	let temp = std::env::temp_dir().join(format!("ror-mesh-scan-{}", std::process::id()));
	std::fs::create_dir_all(&temp).map_err(|error| error.to_string())?;
	let copy = temp.join("rbx-storage.db");
	let result = (|| -> Result<(), String> {
		std::fs::copy(db, &copy).map_err(|error| format!("{}: {error}", db.display()))?;
		for suffix in ["-wal", "-shm"] {
			let mut from = db.as_os_str().to_owned();
			from.push(suffix);
			let mut to = copy.as_os_str().to_owned();
			to.push(suffix);
			let _ = std::fs::copy(PathBuf::from(from), PathBuf::from(to));
		}
		let connection = rusqlite::Connection::open(&copy).map_err(|error| error.to_string())?;
		let mut statement = connection
			.prepare("select id, content from files where content is not null and length(content) > 0")
			.map_err(|error| error.to_string())?;
		let mut rows = statement.query([]).map_err(|error| error.to_string())?;
		while let Some(row) = rows.next().map_err(|error| error.to_string())? {
			let id: Vec<u8> = row.get(0).map_err(|error| error.to_string())?;
			let content: Vec<u8> = row.get(1).map_err(|error| error.to_string())?;
			visit(format!("db:{}", hex(&id)), &content);
		}
		Ok(())
	})();
	let _ = std::fs::remove_dir_all(&temp);
	result
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The newest `%LOCALAPPDATA%\Roblox\Versions\version-*` directory holding Studio
/// (`RobloxStudioBeta.exe`) and a `content` folder.
fn newest_studio_install() -> Option<PathBuf> {
	let versions = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("Versions");
	let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
	for entry in std::fs::read_dir(versions).ok()?.flatten() {
		let path = entry.path();
		if !path.join("content").is_dir() || !path.join("RobloxStudioBeta.exe").is_file() {
			continue;
		}
		let modified = std::fs::metadata(path.join("RobloxStudioBeta.exe"))
			.and_then(|metadata| metadata.modified())
			.unwrap_or(std::time::UNIX_EPOCH);
		if best.as_ref().is_none_or(|(time, _)| modified > *time) {
			best = Some((modified, path));
		}
	}
	best.map(|(_, path)| path)
}
