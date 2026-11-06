//! What a render shows, read back from its output folder (`ror render --ids`): each view's parts by
//! the share of the image they cover, the part at a pixel, and two renders compared (the pixels that
//! changed, and the parts that gained or lost them). Text first: an agent learns what an image holds
//! without looking at it, and a before / after pair says what a change did.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::output;

/// A part as `parts.json` lists it: its index + 1 is its id in `<view>.ids.png`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartEntry {
	pub path: String,
	pub class: String,
	pub material: u32,
	pub transparency: f32,
	#[serde(default)]
	pub surface_appearance: bool,
}

/// A view of a render: its name, its image and (rendered with `--ids`) which part is in front at
/// every pixel.
#[derive(Clone, Debug)]
pub struct RenderedView {
	pub name: String,
	pub image: PathBuf,
	pub ids: Option<PathBuf>,
}

/// A render's output folder.
#[derive(Clone, Debug)]
pub struct Rendered {
	pub dir: PathBuf,
	pub views: Vec<RenderedView>,
	/// Empty without `--ids`.
	pub parts: Vec<PartEntry>,
}

#[derive(Deserialize)]
struct Summary {
	#[serde(default)]
	views: Vec<SummaryView>,
}

#[derive(Deserialize)]
struct SummaryView {
	name: String,
}

impl Rendered {
	/// The views `summary.json` names (in its order), else every image in the folder; their files are
	/// looked up beside it (`<view>.png`, `<view>.ids.png`), so a moved folder still reads.
	pub fn open(dir: &Path) -> Result<Rendered> {
		if !dir.is_dir() {
			bail!("{} is not a render's output folder", dir.display());
		}
		let names: Vec<String> = match std::fs::read(dir.join("summary.json")) {
			Ok(bytes) => serde_json::from_slice::<Summary>(&bytes).context("summary.json")?.views.into_iter().map(|view| view.name).collect(),
			Err(_) => {
				let mut names: Vec<String> = std::fs::read_dir(dir)?
					.filter_map(|entry| entry.ok())
					.filter_map(|entry| entry.file_name().to_str().map(str::to_string))
					.filter_map(|name| name.strip_suffix(".png").map(str::to_string))
					.filter(|name| !name.ends_with(".ids") && !name.ends_with(".diff"))
					.collect();
				names.sort();
				names
			}
		};
		let views: Vec<RenderedView> = names
			.into_iter()
			.map(|name| {
				let ids = dir.join(format!("{name}.ids.png"));
				RenderedView { image: dir.join(format!("{name}.png")), ids: ids.is_file().then_some(ids), name }
			})
			.filter(|view| view.image.is_file())
			.collect();
		if views.is_empty() {
			bail!("{} holds no rendered view", dir.display());
		}
		let parts = match std::fs::read(dir.join("parts.json")) {
			Ok(bytes) => serde_json::from_slice(&bytes).context("parts.json")?,
			Err(_) => Vec::new(),
		};
		Ok(Rendered { dir: dir.to_path_buf(), views, parts })
	}

	/// The view of that name, or the first.
	pub fn view(&self, name: Option<&str>) -> Result<&RenderedView> {
		match name {
			Some(name) => self.views.iter().find(|view| view.name == name).with_context(|| format!("no view \"{name}\" (views: {})", self.view_names())),
			None => Ok(&self.views[0]),
		}
	}

	pub fn view_names(&self) -> String {
		self.views.iter().map(|view| view.name.as_str()).collect::<Vec<_>>().join(", ")
	}
}

/// Ids of an `<view>.ids.png`: the part's index + 1 (0: no part) in red, green and blue, low byte first.
pub fn read_ids(path: &Path) -> Result<(u32, u32, Vec<u32>)> {
	let (width, height, rgba) = output::decode_png(path)?;
	Ok((width, height, rgba.chunks_exact(4).map(|px| px[0] as u32 | (px[1] as u32) << 8 | (px[2] as u32) << 16).collect()))
}

/// One path's parts (parts may share a path) and the pixels they cover in a view.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
	pub path: String,
	pub class: String,
	pub material: u32,
	pub transparency: f32,
	pub surface_appearance: bool,
	/// Parts of that path seen.
	pub parts: usize,
	pub pixels: u64,
}

/// What a view shows: how much of it parts cover, and each path's share (most first).
#[derive(Clone, Debug)]
pub struct ViewCoverage {
	pub width: u32,
	pub height: u32,
	/// Pixels a part is in front at.
	pub drawn: u64,
	pub paths: Vec<Coverage>,
}

/// The pixels each part path covers in `view` (needs `--ids`).
pub fn coverage(rendered: &Rendered, view: &RenderedView) -> Result<ViewCoverage> {
	let ids_file = view.ids.as_ref().with_context(|| format!("view \"{}\" has no ids (render with --ids)", view.name))?;
	let (width, height, ids) = read_ids(ids_file)?;
	Ok(coverage_of(&rendered.parts, width, height, &ids))
}

fn coverage_of(parts: &[PartEntry], width: u32, height: u32, ids: &[u32]) -> ViewCoverage {
	// ids are dense (a part's index + 1), so pixels are counted in a table; an id past parts.json in a map
	let mut counts = vec![0u64; parts.len() + 1];
	let mut beyond: HashMap<u32, u64> = HashMap::new();
	for &id in ids {
		match counts.get_mut(id as usize) {
			Some(count) => *count += 1,
			None => *beyond.entry(id).or_default() += 1,
		}
	}
	let pixels: Vec<(u32, u64)> = counts.iter().enumerate().skip(1).filter(|&(_, &count)| count > 0).map(|(id, &count)| (id as u32, count)).chain(beyond).collect();
	let drawn = pixels.iter().map(|&(_, count)| count).sum();
	// by path: parts sharing one (a mesh split in many) count as one line
	let mut paths: HashMap<String, Coverage> = HashMap::new();
	for (id, count) in pixels {
		let part = parts.get(id as usize - 1);
		let path = part.map_or_else(|| format!("part {id} (not in parts.json)"), |part| part.path.clone());
		let entry = paths.entry(path.clone()).or_insert_with(|| Coverage {
			path,
			class: part.map_or_else(String::new, |part| part.class.clone()),
			material: part.map_or(0, |part| part.material),
			transparency: part.map_or(0.0, |part| part.transparency),
			surface_appearance: part.is_some_and(|part| part.surface_appearance),
			parts: 0,
			pixels: 0,
		});
		entry.parts += 1;
		entry.pixels += count;
	}
	let mut paths: Vec<Coverage> = paths.into_values().collect();
	paths.sort_by(|a, b| b.pixels.cmp(&a.pixels).then_with(|| a.path.cmp(&b.path)));
	ViewCoverage { width, height, drawn, paths }
}

/// What is at a pixel of a view: the part in front there (its index into `parts.json`; `None` for the
/// background, or a render without `--ids`) and the pixel's colour.
pub fn at(rendered: &Rendered, view: &RenderedView, x: u32, y: u32) -> Result<(Option<(usize, PartEntry)>, [u8; 4])> {
	let (width, height, rgba) = output::decode_png(&view.image)?;
	if x >= width || y >= height {
		bail!("{x},{y} is outside the {width}x{height} image");
	}
	let index = (y * width + x) as usize;
	let pixel = [rgba[index * 4], rgba[index * 4 + 1], rgba[index * 4 + 2], rgba[index * 4 + 3]];
	let part = match &view.ids {
		Some(ids_file) => {
			let (_, _, ids) = read_ids(ids_file)?;
			let id = ids[index];
			(id > 0).then(|| (id as usize - 1, rendered.parts.get(id as usize - 1).cloned().unwrap_or_else(|| unknown_part(id))))
		}
		None => None,
	};
	Ok((part, pixel))
}

fn unknown_part(id: u32) -> PartEntry {
	PartEntry { path: format!("part {id} (not in parts.json)"), class: String::new(), material: 0, transparency: 0.0, surface_appearance: false }
}

/// How one view differs between two renders.
#[derive(Clone, Debug)]
pub struct ViewDiff {
	pub name: String,
	pub width: u32,
	pub height: u32,
	/// Pixels whose largest channel difference is over the threshold.
	pub changed: u64,
	/// The largest channel difference, and its mean over the changed pixels.
	pub largest: u8,
	pub mean: f64,
	/// Where the changed pixels are: left, top, right, bottom (exclusive).
	pub region: Option<(u32, u32, u32, u32)>,
	/// Part paths whose covered pixels changed (both renders with `--ids`): before, after, most
	/// changed first.
	pub parts: Vec<(String, u64, u64)>,
	/// The changed pixels painted over a dimmed copy of the second render.
	pub image: Option<PathBuf>,
	/// Why the view was not compared (sizes differ, missing from one).
	pub skipped: Option<String>,
}

/// The views two renders share, compared pixel by pixel (a channel differing by more than
/// `threshold` of 255 counts as changed), with `<view>.diff.png` written into `out`.
pub fn diff(before: &Rendered, after: &Rendered, threshold: u8, out: &Path) -> Result<Vec<ViewDiff>> {
	std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
	let mut diffs = Vec::new();
	for view in &after.views {
		let skipped = |reason: String| ViewDiff { name: view.name.clone(), width: 0, height: 0, changed: 0, largest: 0, mean: 0.0, region: None, parts: Vec::new(), image: None, skipped: Some(reason) };
		let Some(old) = before.views.iter().find(|old| old.name == view.name) else {
			diffs.push(skipped(format!("not in {}", before.dir.display())));
			continue;
		};
		let (width, height, new_rgba) = output::decode_png(&view.image)?;
		let (old_width, old_height, old_rgba) = output::decode_png(&old.image)?;
		if (width, height) != (old_width, old_height) {
			diffs.push(skipped(format!("sizes differ: {old_width}x{old_height} before, {width}x{height} after")));
			continue;
		}
		let mut result = compare_pixels(&old_rgba, &new_rgba, width, threshold);
		result.name = view.name.clone();
		(result.width, result.height) = (width, height);
		if let (Some(old_ids), Some(new_ids)) = (&old.ids, &view.ids) {
			let (_, _, old_ids) = read_ids(old_ids)?;
			let (_, _, new_ids) = read_ids(new_ids)?;
			let old_coverage = coverage_of(&before.parts, width, height, &old_ids);
			let new_coverage = coverage_of(&after.parts, width, height, &new_ids);
			result.parts = part_changes(&old_coverage, &new_coverage);
		}
		let image = out.join(format!("{}.diff.png", view.name));
		output::write_png_indexed(&image, width, height, &diff_image(&old_rgba, &new_rgba, threshold), &diff_palette())?;
		result.image = Some(image);
		diffs.push(result);
	}
	Ok(diffs)
}

fn compare_pixels(old: &[u8], new: &[u8], width: u32, threshold: u8) -> ViewDiff {
	let (mut changed, mut largest, mut sum) = (0u64, 0u8, 0u64);
	let mut region: Option<(u32, u32, u32, u32)> = None;
	for (index, (a, b)) in old.chunks_exact(4).zip(new.chunks_exact(4)).enumerate() {
		let delta = (0..4).map(|channel| a[channel].abs_diff(b[channel])).max().unwrap_or(0);
		if delta > threshold {
			changed += 1;
			sum += delta as u64;
			largest = largest.max(delta);
			let (x, y) = (index as u32 % width, index as u32 / width);
			region = Some(region.map_or((x, y, x + 1, y + 1), |(left, top, right, bottom)| (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1))));
		}
	}
	ViewDiff { name: String::new(), width: 0, height: 0, changed, largest, mean: if changed > 0 { sum as f64 / changed as f64 } else { 0.0 }, region, parts: Vec::new(), image: None, skipped: None }
}

fn part_changes(old: &ViewCoverage, new: &ViewCoverage) -> Vec<(String, u64, u64)> {
	let mut pixels: HashMap<&str, (u64, u64)> = HashMap::new();
	for path in &old.paths {
		pixels.entry(&path.path).or_default().0 = path.pixels;
	}
	for path in &new.paths {
		pixels.entry(&path.path).or_default().1 = path.pixels;
	}
	let mut changes: Vec<(String, u64, u64)> = pixels.into_iter().filter(|(_, (a, b))| a != b).map(|(path, (a, b))| (path.to_string(), a, b)).collect();
	changes.sort_by(|a, b| a.2.abs_diff(a.1).cmp(&b.2.abs_diff(b.1)).reverse().then_with(|| a.0.cmp(&b.0)));
	changes
}

/// The diff image's colours: its grays (20-105) and oranges (255, 33-160, 0), 214 in all.
const DIFF_GRAYS: std::ops::RangeInclusive<u8> = 20..=105;
const DIFF_ORANGES: std::ops::RangeInclusive<u8> = 33..=160;

fn diff_palette() -> Vec<u8> {
	DIFF_GRAYS.map(|gray| [gray, gray, gray]).chain(DIFF_ORANGES.map(|green| [255, green, 0])).flatten().collect()
}

/// The second image dimmed to gray, changed pixels painted over it: orange for small changes, red
/// for large ones; as indices into [`diff_palette`].
fn diff_image(old: &[u8], new: &[u8], threshold: u8) -> Vec<u8> {
	let oranges = DIFF_GRAYS.len() as u8 - DIFF_ORANGES.start();
	old.chunks_exact(4)
		.zip(new.chunks_exact(4))
		.map(|(a, b)| {
			let delta = (0..4).map(|channel| a[channel].abs_diff(b[channel])).max().unwrap_or(0);
			if delta > threshold {
				160u8.saturating_sub(delta / 2).wrapping_add(oranges)
			} else {
				((b[0] as u32 * 54 + b[1] as u32 * 183 + b[2] as u32 * 19) / 256 * b[3] as u32 / 255 / 3) as u8
			}
		})
		.collect()
}

/// Roblox's `Enum.Material` name for a value parts carry.
pub fn material_name(material: u32) -> Option<&'static str> {
	Some(match material {
		256 => "Plastic",
		272 => "SmoothPlastic",
		288 => "Neon",
		512 => "Wood",
		528 => "WoodPlanks",
		784 => "Marble",
		788 => "Basalt",
		800 => "Slate",
		804 => "CrackedLava",
		816 => "Concrete",
		820 => "Limestone",
		832 => "Granite",
		836 => "Pavement",
		848 => "Brick",
		864 => "Pebble",
		880 => "Cobblestone",
		896 => "Rock",
		912 => "Sandstone",
		1040 => "CorrodedMetal",
		1056 => "DiamondPlate",
		1072 => "Foil",
		1088 => "Metal",
		1280 => "Grass",
		1284 => "LeafyGrass",
		1296 => "Sand",
		1312 => "Fabric",
		1328 => "Snow",
		1344 => "Mud",
		1360 => "Ground",
		1376 => "Asphalt",
		1392 => "Salt",
		1536 => "Ice",
		1552 => "Glacier",
		1568 => "Glass",
		1584 => "ForceField",
		1792 => "Air",
		2048 => "Water",
		_ => return None,
	})
}

/// A part's description after its path: class, material, transparency, SurfaceAppearance.
pub fn describe(class: &str, material: u32, transparency: f32, surface_appearance: bool) -> String {
	let mut words = vec![if class.is_empty() { "?".to_string() } else { class.to_string() }];
	words.push(material_name(material).map_or_else(|| format!("material {material}"), str::to_string));
	if transparency > 0.0 {
		words.push(format!("transparency {transparency:.2}"));
	}
	if surface_appearance {
		words.push("SurfaceAppearance".to_string());
	}
	words.join(" · ")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn part(path: &str) -> PartEntry {
		PartEntry { path: path.to_string(), class: "MeshPart".into(), material: 256, transparency: 0.0, surface_appearance: false }
	}

	/// A render folder in a fresh temporary directory: one 4x2 view with these ids and colours.
	fn rendered(name: &str, ids: [u32; 8], rgba: [[u8; 4]; 8], parts: &[PartEntry]) -> Rendered {
		let dir = std::env::temp_dir().join(format!("ror-inspect-test-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		output::write_png(&dir.join("front.png"), 4, 2, &rgba.concat(), false).unwrap();
		output::write_png(&dir.join("front.ids.png"), 4, 2, &ids.iter().flat_map(|id| [(id & 0xff) as u8, (id >> 8) as u8, (id >> 16) as u8, 255]).collect::<Vec<_>>(), false).unwrap();
		let json: Vec<_> = parts.iter().map(|part| serde_json::json!({"path": part.path, "class": part.class, "material": part.material, "transparency": part.transparency, "surfaceAppearance": part.surface_appearance})).collect();
		std::fs::write(dir.join("parts.json"), serde_json::to_vec(&json).unwrap()).unwrap();
		std::fs::write(dir.join("summary.json"), br#"{"views": [{"name": "front"}]}"#).unwrap();
		Rendered::open(&dir).unwrap()
	}

	const BLACK: [u8; 4] = [0, 0, 0, 255];
	const RED: [u8; 4] = [255, 0, 0, 255];

	#[test]
	fn paths_cover_their_pixels_most_first() {
		// parts 1 and 3 share a path; part 2 has its own; two pixels show nothing
		let parts = [part("Unit.Body"), part("Unit.Sword"), part("Unit.Body")];
		let render = rendered("coverage", [1, 1, 2, 0, 3, 3, 3, 0], [BLACK; 8], &parts);
		let seen = coverage(&render, render.view(None).unwrap()).unwrap();
		assert_eq!(seen.drawn, 6);
		assert_eq!((seen.paths[0].path.as_str(), seen.paths[0].parts, seen.paths[0].pixels), ("Unit.Body", 2, 5));
		assert_eq!((seen.paths[1].path.as_str(), seen.paths[1].pixels), ("Unit.Sword", 1));
	}

	#[test]
	fn a_pixel_names_its_part_and_colour() {
		let parts = [part("Unit.Body"), part("Unit.Sword")];
		let mut colours = [BLACK; 8];
		colours[2] = RED;
		let render = rendered("at", [1, 1, 2, 0, 1, 1, 1, 0], colours, &parts);
		let view = render.view(Some("front")).unwrap();
		let (found, pixel) = at(&render, view, 2, 0).unwrap();
		assert_eq!((found.map(|(index, part)| (index, part.path)), pixel), (Some((1, "Unit.Sword".to_string())), RED));
		assert!(at(&render, view, 3, 0).unwrap().0.is_none(), "the background");
		assert!(at(&render, view, 4, 0).is_err(), "outside");
	}

	#[test]
	fn renders_differ_where_their_pixels_and_parts_do() {
		let parts = [part("Unit.Body"), part("Unit.Sword")];
		let before = rendered("before", [1, 1, 2, 0, 1, 1, 1, 0], [BLACK; 8], &parts);
		let mut colours = [BLACK; 8];
		colours[3] = RED;
		colours[6] = [5, 5, 5, 255];
		// the sword grew into the background's pixel
		let after = rendered("after", [1, 1, 2, 2, 1, 1, 1, 0], colours, &parts);
		let out = after.dir.join("diff");
		let diffs = diff(&before, &after, 8, &out).unwrap();
		let front = &diffs[0];
		// only the red pixel is over the threshold (the 5/255 one is not)
		assert_eq!((front.changed, front.largest, front.region), (1, 255, Some((3, 0, 4, 1))));
		assert_eq!(front.parts, vec![("Unit.Sword".to_string(), 1, 2)]);
		assert!(out.join("front.diff.png").is_file());
		let (width, height, written) = output::decode_png(&out.join("front.diff.png")).unwrap();
		assert_eq!((width, height), (4, 2));
		assert_eq!(written, rgba_diff_image(&[BLACK; 8].concat(), &colours.concat(), 8), "the indexed diff image decodes to the RGBA one");
	}

	/// The diff image as RGBA, as it was written before it had a palette.
	fn rgba_diff_image(old: &[u8], new: &[u8], threshold: u8) -> Vec<u8> {
		old.chunks_exact(4)
			.zip(new.chunks_exact(4))
			.flat_map(|(a, b)| {
				let delta = (0..4).map(|channel| a[channel].abs_diff(b[channel])).max().unwrap_or(0);
				if delta > threshold {
					[255, 160u8.saturating_sub(delta / 2), 0, 255]
				} else {
					let gray = ((b[0] as u32 * 54 + b[1] as u32 * 183 + b[2] as u32 * 19) / 256 * b[3] as u32 / 255 / 3 + 20) as u8;
					[gray, gray, gray, 255]
				}
			})
			.collect()
	}

	#[test]
	fn the_diff_palette_holds_every_colour_the_diff_image_paints() {
		let palette = diff_palette();
		assert_eq!(palette.len(), 214 * 3);
		// pseudo-random pixels, plus the extremes: every channel value, and every alpha
		let mut state = 0x2545_f491_u32;
		let mut random = || {
			state ^= state << 13;
			state ^= state >> 17;
			state ^= state << 5;
			state
		};
		let mut old: Vec<u8> = (0..200_000).map(|_| random() as u8).collect();
		let mut new: Vec<u8> = (0..200_000).map(|_| random() as u8).collect();
		for value in 0..=255u8 {
			old.extend([value, value, value, 255, 0, 0, 0, value]);
			new.extend([value, value, value, 255, 255, 255, 255, value]);
		}
		for threshold in [0, 1, 8, 100, 254, 255] {
			let expected = rgba_diff_image(&old, &new, threshold);
			let indices = diff_image(&old, &new, threshold);
			let decoded: Vec<u8> = indices.iter().flat_map(|&index| {
				let at = index as usize * 3;
				[palette[at], palette[at + 1], palette[at + 2], 255]
			}).collect();
			assert_eq!(decoded, expected, "threshold {threshold}");
		}
	}

	#[test]
	fn materials_by_name() {
		assert_eq!(material_name(288), Some("Neon"));
		assert_eq!(material_name(3), None);
		assert_eq!(describe("Part", 1568, 0.5, true), "Part · Glass · transparency 0.50 · SurfaceAppearance");
	}
}
