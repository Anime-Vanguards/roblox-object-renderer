//! The cost loop an agent runs on a map: measure where each view's cost goes (`analysis.json`, the
//! instance tree in `tree.json`, `analysis.txt`), take the ranked findings and their edits, try edits in
//! memory against the same views (`whatif.json`: before and after per view, and with `--look` the share
//! of pixels each view changes and crops of where), and export accepted edits as a change list with
//! Lune scripts for the real place. docs/cost-loop.md is the guide.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use rbx_dom_weak::types::Ref;
use serde::Serialize;
use serde_json::{Value, json};

use super::attribute::{self, Node, Owners, ViewWork, Work};
use super::edits::{self, EditsFile};
use super::findings::{self, Finding};
use super::ids::InstanceIds;
use super::memory::{self, MemoryReport};
use super::pixels::{self, ViewPixels};
use super::prices::{self, Preset, Quality};
use super::{Analysis, CostOptions, ViewCost};
use crate::render::camera::Camera;
use crate::scene::Scene;
use crate::scene::load::Document;

pub const SCHEMA: &str = "ror-cost-analysis/1";
pub const TREE_SCHEMA: &str = "ror-cost-tree/1";
pub const WHATIF_SCHEMA: &str = "ror-cost-whatif/1";

/// A view's numbers as the analysis reports them.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewLine {
	pub name: String,
	/// Roblox's counters as the model counts them (every pass but Shadows).
	pub draws: u64,
	pub triangles: u64,
	/// Everything attributed, priced.
	pub work: Work,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub over_budget: Vec<String>,
}

/// Limits per view.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
	pub name: String,
	pub draws: u64,
	pub triangles: u64,
	pub ms: f64,
}

impl Budget {
	/// `phone` (600 draws and 800k triangles, Roblox's guidance for most devices; 10 ms of the model's
	/// phone frame for the map, a suggestion), `desktop` (twice those), or `draws=N,triangles=N,ms=N`
	/// (k and m suffixes: 800k), each over `phone`'s.
	pub fn parse(text: &str) -> Result<Self> {
		let mut budget = Budget { name: "phone".into(), draws: 600, triangles: 800_000, ms: 10.0 };
		for part in text.split(',').map(str::trim).filter(|part| !part.is_empty()) {
			match part.split_once('=') {
				None if part == "phone" => {}
				None if part == "desktop" => budget = Budget { name: "desktop".into(), draws: 1200, triangles: 1_600_000, ms: 20.0 },
				Some((key, value)) => {
					let value = value.trim().to_ascii_lowercase();
					let (number, scale) = match value.chars().last() {
						Some('k') => (&value[..value.len() - 1], 1e3),
						Some('m') => (&value[..value.len() - 1], 1e6),
						_ => (value.as_str(), 1.0),
					};
					let number: f64 = number.parse::<f64>().with_context(|| format!("--budget {key}={value}: a number"))? * scale;
					match key.trim() {
						"draws" => budget.draws = number as u64,
						"triangles" => budget.triangles = number as u64,
						"ms" => budget.ms = number,
						other => bail!("--budget: draws, triangles or ms, not {other:?}"),
					}
					budget.name = "custom".into();
				}
				None => bail!("--budget: phone, desktop or draws=N,triangles=N,ms=N"),
			}
		}
		Ok(budget)
	}

	pub(crate) fn check(&self, draws: u64, triangles: u64, ms: f64) -> Vec<String> {
		let mut over = Vec::new();
		if draws > self.draws {
			over.push(format!("draws {draws} > {}", self.draws));
		}
		if triangles > self.triangles {
			over.push(format!("triangles {triangles} > {}", self.triangles));
		}
		if ms > self.ms {
			over.push(format!("ms {ms:.2} > {:.2}", self.ms));
		}
		over
	}

	pub(crate) fn check_f64(&self, draws: f64, triangles: f64, ms: f64) -> Vec<String> {
		let mut over = Vec::new();
		if draws > self.draws as f64 {
			over.push(format!("draws {draws:.1} > {}", self.draws));
		}
		if triangles > self.triangles as f64 {
			over.push(format!("triangles {triangles:.0} > {}", self.triangles));
		}
		if ms > self.ms {
			over.push(format!("ms {ms:.2} > {:.2}", self.ms));
		}
		over
	}
}

/// A view's lit picture as the layers render drew it: width, height, RGBA.
pub type Frame = (u32, u32, Vec<u8>);

/// One costing of a scene from every view.
pub struct Costed {
	pub analysis: Analysis,
	pub owners: Owners,
	pub costs: Vec<ViewCost>,
	pub pixels: Vec<Option<ViewPixels>>,
	pub works: Vec<ViewWork>,
	/// The lit frames the layers came with (analysis size, RGBA).
	pub frames: Vec<Option<Frame>>,
	/// The scene parts any view shows at the highest quality (in front of the opaque surface, or a
	/// see-through fragment there): what "no view shows" is judged by, since an edit reaches every
	/// device, not only the preset's. The same as the counted pixels' parts where the preset culls
	/// nothing; `None` without pixels.
	pub shown: Option<std::collections::HashSet<usize>>,
	/// Deduplicated mesh and texture memory upper-bound estimate in bytes (only when `options.memory` is set).
	pub memory: Option<MemoryReport>,
}

pub struct Setup<'a> {
	pub renderer: &'a crate::render::Renderer,
	pub options: &'a CostOptions,
	pub names: Vec<String>,
	pub shots: &'a [(Camera, u32, u32)],
	pub preset: Preset,
	pub quality: Quality,
	pub gpu: Option<&'a crate::budget::GpuGate>,
	/// The scene draws another file's Lighting (`--lighting-from`): trials keep it, and its effects are
	/// charged to no instance of this file.
	pub lent_lighting: bool,
	/// The scene's MaterialService is another file's (`--materials-from`): trials keep it.
	pub lent_materials: bool,
}

/// The preset a run counts for: the named one, else the run's size and quality.
pub fn preset_of(options: &CostOptions, shots: &[(Camera, u32, u32)]) -> Result<Preset> {
	let mut preset = match options.preset.as_deref() {
		Some(name) => Preset::named(name).with_context(|| format!("--preset {name}: phone or desktop"))?,
		None => {
			let (width, height) = shots.first().map(|(_, width, height)| (*width, *height)).unwrap_or((1920, 1080));
			Preset::custom(width, height, options.quality())
		}
	};
	if let Some(scale) = options.pixel_scale {
		preset.analysis_scale = scale.clamp(1, 8);
	}
	Ok(preset)
}

/// Costs `scene` from every view: the model's counts, the layers' pixels and the attribution.
pub fn cost(setup: &Setup, scene: &Scene, document: &Document, ids: &InstanceIds, merges: &HashMap<usize, String>) -> Result<Costed> {
	cost_inner(setup, scene, document, ids, merges, setup.options.memory)
}

fn cost_inner(setup: &Setup, scene: &Scene, document: &Document, ids: &InstanceIds, merges: &HashMap<usize, String>, measure_memory: bool) -> Result<Costed> {
	let options = setup.options;
	let analysis = Analysis::new_with(scene, document, &setup.renderer.assets, options, merges);
	let costs: Vec<ViewCost> = setup.names.par_iter().zip(setup.shots.par_iter()).map(|(name, (camera, width, height))| analysis.view(name, camera, *width as f32 / (*height).max(1) as f32, options)).collect();
	let owners = Owners::new(document, scene, &analysis, setup.lent_lighting);
	let (width, height) = setup.preset.analysis_size();
	let mut pixels: Vec<Option<ViewPixels>> = vec![None; costs.len()];
	let mut frames: Vec<Option<Frame>> = vec![None; costs.len()];
	if options.pixels && !costs.is_empty() {
		let requests: Vec<crate::render::ViewRequest> = setup
			.shots
			.iter()
			// what Roblox draws at the quality: each mesh at its level of detail, the parts the quality's
			// view distance and size cull leave out not drawn (the counts' rules, crate::lod)
			.map(|(camera, _, _)| crate::render::ViewRequest {
				width,
				height,
				camera: camera.spec(),
				msaa: 1,
				bloom: false,
				layers: true,
				lod: (!options.full_detail).then_some(crate::lod::Lod { quality: setup.quality.level, cull: true }),
				..Default::default()
			})
			.collect();
		let shadows = attribute::sun_shadows(scene, &setup.quality);
		// a few views a submission: each holds its layers' buffers until read
		for (chunk_index, chunk) in requests.chunks(8).enumerate() {
			let (images, _) = setup.renderer.render_gated(scene, chunk, setup.gpu)?;
			let computed: Vec<(usize, Option<ViewPixels>, Option<Frame>)> = images
				.into_par_iter()
				.enumerate()
				.map(|(offset, image)| {
					let index = chunk_index * 8 + offset;
					let drawn: Vec<usize> = costs[index].drawn.iter().map(|(item, _, _)| *item as usize).collect();
					let camera = &setup.shots[index].0;
					let counted = image.layers.as_ref().map(|layers| pixels::view(scene, &analysis, &drawn, camera, layers, &setup.quality, shadows));
					(index, counted, Some((image.width, image.height, image.rgba)))
				})
				.collect();
			for (index, counted, frame) in computed {
				pixels[index] = counted;
				frames[index] = frame;
			}
		}
	}
	// what the highest quality shows: where the preset's quality culls (view distance and size, levels
	// of detail), a second layers pass without them
	let shown = if options.pixels && !costs.is_empty() {
		let mut shown: std::collections::HashSet<usize> = std::collections::HashSet::new();
		let mut add = |layers: &crate::render::Layers| {
			shown.extend(layers.opaque.iter().filter(|id| **id > 0).map(|id| *id as usize - 1));
			shown.extend(layers.part_blended.iter().enumerate().filter(|(_, count)| **count > 0).map(|(part, _)| part));
		};
		let culls = !options.full_detail && setup.quality.level < 21;
		if culls {
			let requests: Vec<crate::render::ViewRequest> = setup.shots.iter().map(|(camera, _, _)| crate::render::ViewRequest { width, height, camera: camera.spec(), msaa: 1, bloom: false, layers: true, ..Default::default() }).collect();
			for chunk in requests.chunks(8) {
				let (images, _) = setup.renderer.render_gated(scene, chunk, setup.gpu)?;
				for image in &images {
					if let Some(layers) = &image.layers {
						add(layers);
					}
				}
			}
		} else {
			for counted in pixels.iter().flatten() {
				shown.extend(counted.part_pixels.keys().copied());
				shown.extend(counted.part_blended.keys().copied());
			}
		}
		Some(shown)
	} else {
		None
	};
	let works: Vec<ViewWork> = costs
		.par_iter()
		.zip(setup.shots.par_iter())
		.zip(pixels.par_iter())
		.map(|((cost, (camera, _, _)), counted)| attribute::view(scene, &analysis, &owners, ids, cost, camera, counted.as_ref(), &setup.preset, &setup.quality))
		.collect();
	let memory = measure_memory.then(|| memory::measure(scene, &analysis.meshes, &setup.renderer.assets));
	Ok(Costed { analysis, owners, costs, pixels, works, frames, shown, memory })
}

fn mean_work(works: &[ViewWork]) -> Work {
	let mut sum = Work::default();
	for work in works {
		sum.add(&work.total);
	}
	sum.scaled(1.0 / works.len().max(1) as f64).rounded()
}

fn worst(works: &[ViewWork]) -> Option<&ViewWork> {
	works.iter().fold(None, |best: Option<&ViewWork>, work| {
		if best.is_none_or(|best| work.total.ms > best.total.ms || (work.total.ms == best.total.ms && work.total.gpu_units > best.total.gpu_units)) {
			Some(work)
		} else {
			best
		}
	})
}

/// The scene parts each merge names, by scene index.
fn merge_indices(scene: &Scene, merges: &[(String, Vec<Ref>)]) -> HashMap<usize, String> {
	let by_ref: HashMap<Ref, usize> = scene.parts.iter().enumerate().filter_map(|(index, part)| part.referent.map(|referent| (referent, index))).collect();
	let mut out = HashMap::new();
	for (name, members) in merges {
		for member in members {
			if let Some(&index) = by_ref.get(member) {
				out.insert(index, name.clone());
			}
		}
	}
	out
}

/// Everything the loop writes for a run, and the lines it prints.
#[allow(clippy::too_many_arguments)]
pub fn run(setup: &Setup, scene: &Scene, document: &Document, reload: Option<&dyn Fn() -> Result<Document>>, out: &Path, input: &str) -> Result<(Costed, Vec<String>)> {
	let options = setup.options;
	let ids = InstanceIds::new(document);
	let costed = cost(setup, scene, document, &ids, &HashMap::new())?;
	let budget = options.budget.as_deref().map(Budget::parse).transpose()?;
	let views: Vec<ViewLine> = costed
		.costs
		.iter()
		.zip(&costed.works)
		.map(|(cost, work)| ViewLine {
			name: cost.name.clone(),
			draws: cost.total.draws,
			triangles: cost.total.triangles,
			work: work.total.rounded(),
			over_budget: budget.as_ref().map(|budget| budget.check(cost.total.draws, cost.total.triangles, work.total.ms)).unwrap_or_default(),
		})
		.collect();
	let tree = attribute::tree(&ids, &costed.works);
	let profile = prices::Profile::for_preset(&setup.preset);
	let found = findings::find(&findings::Inputs {
		scene,
		document,
		analysis: &costed.analysis,
		owners: &costed.owners,
		ids: &ids,
		costs: &costed.costs,
		works: &costed.works,
		profile: &profile,
		quality: &setup.quality,
		pixels: options.pixels && costed.pixels.iter().all(Option::is_some),
		shown: costed.shown.as_ref(),
	});
	let mean = mean_work(&costed.works);
	let worst_view = worst(&costed.works);
	let over: Vec<&ViewLine> = views.iter().filter(|view| !view.over_budget.is_empty()).collect();
	let parents: std::collections::HashSet<&str> = tree.iter().filter_map(|node| node.parent.as_deref()).collect();
	let top_nodes: Vec<&Node> = tree.iter().filter(|node| !parents.contains(node.id.as_str()) && node.id != attribute::SHARED).take(20).collect();
	let mut analysis_json = json!({
		"schema": SCHEMA,
		"input": input,
		"preset": setup.preset,
		"quality": setup.quality,
		"technology": costed.analysis.technology,
		"prices": prices::table_for(&setup.preset),
		"fidelity": prices::fidelity(&setup.preset, options.pixels),
		"unmodeled": prices::unmodeled(&setup.preset, &setup.quality),
		"shadowRedrawShare": attribute::SHADOW_REDRAW_SHARE,
		"pixelsCounted": options.pixels,
		"views": views,
		"mean": mean,
		"worst": worst_view.map(|work| json!({ "view": work.name, "work": work.total.rounded() })),
		"budget": budget.as_ref().map(|budget| json!({ "limits": budget, "viewsOver": over.len(), "views": views.len() })),
		"topInstances": top_nodes,
		"findings": found,
		"files": { "tree": "tree.json", "summary": "analysis.txt", "costs": "cost.json" },
	});
	if let Some(memory) = &costed.memory {
		analysis_json["memory"] = serde_json::to_value(memory)?;
	}
	crate::output::write_json_atomic(&out.join("analysis.json"), &analysis_json, true)?;
	crate::output::write_json_atomic(&out.join("tree.json"), &json!({ "schema": TREE_SCHEMA, "views": costed.works.iter().map(|work| work.name.clone()).collect::<Vec<_>>(), "nodes": tree }), false)?;
	let mut lines = summary_lines(input, setup, &views, &mean, worst_view, costed.memory.as_ref(), &tree, &found, budget.as_ref());
	if options.heat {
		write_heat(&costed, out)?;
		lines.push(format!("heat images: <view>.heat-cost.png, .heat-overdraw.png, .heat-lights.png, .heat-density.png in {}", out.display()));
	}
	for id in &options.explain {
		let explained = explain(&ids, &costed, scene, id)?;
		crate::output::write_json_atomic(&out.join(format!("explain-{}.json", file_name(id))), &explained.0, true)?;
		lines.extend(explained.1);
	}
	crate::output::write_atomic(&out.join("analysis.txt"), (lines.join("\n") + "\n").as_bytes())?;
	if let Some(file) = &options.edits {
		let reload = reload.context("--edits needs the scene's file or asset id to read it again (not a playback)")?;
		lines.extend(whatif(setup, scene, &ids, &costed, file, reload, out, input)?);
	}
	if budget.is_some() && options.fail_over_budget && !over.is_empty() {
		lines.push(format!("over budget: {} of {} views", over.len(), views.len()));
	}
	Ok((costed, lines))
}

pub(crate) fn file_name(id: &str) -> String {
	id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect::<String>().chars().take(80).collect()
}

fn ms(value: f64) -> String {
	format!("{value:.2} ms")
}

#[allow(clippy::too_many_arguments)]
fn summary_lines(input: &str, setup: &Setup, views: &[ViewLine], mean: &Work, worst: Option<&ViewWork>, memory: Option<&MemoryReport>, tree: &[Node], found: &[Finding], budget: Option<&Budget>) -> Vec<String> {
	let mut lines = Vec::new();
	let quality_label = match setup.quality.client_level {
		Some(bar) => format!("quality {} (client bar {bar}, {:.0} studs)", setup.quality.level, setup.quality.view_distance),
		None => format!("quality {}", setup.quality.level),
	};
	lines.push(format!("cost loop ({SCHEMA}): {input}, {} views, preset {} ({}x{}, {quality_label}{})", views.len(), setup.preset.name, setup.preset.width, setup.preset.height, if setup.quality.sun_shadows { ", sun shadows" } else { ", no sun shadows" }));
	let desktop = setup.preset.uses_desktop_pricing();
	if !desktop {
		let gpu_ms = mean.gpu_ms.unwrap_or(0.0);
		lines.push(format!(
			"per view, mean: {} phone frame (CPU {}, GPU {} of which the frame waits {}); {:.0} draws, {:.0}k triangles, {:.2} viewports blended, {:.0} parts",
			ms(mean.ms),
			ms(mean.cpu_ms),
			ms(gpu_ms),
			ms(gpu_ms * prices::GPU_TO_FRAME),
			mean.draws,
			mean.triangles / 1000.0,
			mean.blended / (setup.preset.width as f64 * setup.preset.height as f64),
			mean.parts
		));
		if let Some(worst) = worst {
			lines.push(format!("worst view: {} {} ({:.0} draws, {:.0}k triangles)", worst.name, ms(worst.total.ms), worst.total.draws, worst.total.triangles / 1000.0));
		}
	} else {
		lines.push(format!(
			"per view, mean: {} desktop CPU frame (GPU uncalibrated: {:.2} gpuUnits, gpuMs: null); {:.0} draws, {:.0}k triangles, {:.2} viewports blended, {:.0} parts",
			ms(mean.ms),
			mean.gpu_units,
			mean.draws,
			mean.triangles / 1000.0,
			mean.blended / (setup.preset.width as f64 * setup.preset.height as f64),
			mean.parts
		));
		if let Some(worst) = worst {
			lines.push(format!("worst view: {} {} ({:.2} gpuUnits, {:.0} draws, {:.0}k triangles)", worst.name, ms(worst.total.ms), worst.total.gpu_units, worst.total.draws, worst.total.triangles / 1000.0));
		}
		lines.push("fidelity: desktop CPU ms is estimated from one reference PC ratio (1/2.224, not players' desktops); desktop GPU ms is uncalibrated (gpuMs = null); findings rank by combined CPU + gpuUnits share".into());
	}
	if let Some(memory) = memory {
		lines.push(memory::summary_line(memory, !desktop));
		for warning in memory::missing_texture_warnings(memory) {
			lines.push(format!("warning: {warning}"));
		}
	}
	if let Some(budget) = budget {
		let over: Vec<&ViewLine> = views.iter().filter(|view| !view.over_budget.is_empty()).collect();
		lines.push(format!("budget {} ({} draws, {}k triangles, {} per view): {} of {} views over", budget.name, budget.draws, budget.triangles / 1000, ms(budget.ms), over.len(), views.len()));
		for view in over.iter().take(8) {
			lines.push(format!("  over: {}: {}", view.name, view.over_budget.join(", ")));
		}
	}
	lines.push("where it goes (instances, mean per view; tree.json drills down):".into());
	let viewport = setup.preset.width as f64 * setup.preset.height as f64;
	let line = |node: &Node, indent: &str| {
		format!("  {indent}{:>9}  {:>6.1} draws {:>6.0}k tris {:>5.2} vp blended {:>5.0} parts  {} ({} costing, worst {})", ms(node.mean.ms), node.mean.draws, node.mean.triangles / 1000.0, node.mean.blended / viewport, node.mean.parts, node.id, node.leaves, node.worst_view)
	};
	for (node, depth) in breakdown(tree).into_iter().take(14) {
		lines.push(line(node, &"  ".repeat(depth)));
	}
	lines.push("costliest single instances:".into());
	let parents: std::collections::HashSet<&str> = tree.iter().filter_map(|node| node.parent.as_deref()).collect();
	for node in tree.iter().filter(|node| !parents.contains(node.id.as_str()) && node.id != attribute::SHARED).take(6) {
		lines.push(line(node, ""));
	}
	lines.push(format!("top findings ({} in analysis.json, each with its edit):", found.len()));
	for finding in found.iter().take(10) {
		if desktop && finding.saving.mean_ms <= 0.0 && finding.saving.mean_gpu_units > 0.0 {
			lines.push(format!("  #{:<2} -{:.2} gpuUnits/view (worst {})  [{}] {}", finding.rank, finding.saving.mean_gpu_units, finding.saving.worst_view, finding.kind, finding.title));
		} else if desktop && finding.saving.mean_gpu_units > 0.0 {
			lines.push(format!("  #{:<2} -{:>8}/view, -{:.2} gpuUnits (worst -{} {})  [{}] {}", finding.rank, ms(finding.saving.mean_ms), finding.saving.mean_gpu_units, ms(finding.saving.worst_ms), finding.saving.worst_view, finding.kind, finding.title));
		} else {
			lines.push(format!("  #{:<2} -{:>8}/view (worst -{} {})  [{}] {}", finding.rank, ms(finding.saving.mean_ms), ms(finding.saving.worst_ms), finding.saving.worst_view, finding.kind, finding.title));
		}
	}
	lines.push("try one: put its edit in an edits file and run again with --edits FILE --look (docs/cost-loop.md)".into());
	lines
}

/// The tree's costliest branches for a summary: from the top, a node with one child holding most of
/// its cost is passed through to that child (Workspace.Map to Workspace.Map.Assets), and the children
/// of the costliest are listed under it, two levels deep.
fn breakdown<'a>(tree: &'a [Node]) -> Vec<(&'a Node, usize)> {
	let mut children: HashMap<&'a str, Vec<&'a Node>> = HashMap::new();
	for node in tree {
		if let Some(parent) = &node.parent {
			children.entry(parent.as_str()).or_default().push(node);
		}
	}
	// tree is sorted by mean ms: children keep that order
	fn through<'a>(children: &HashMap<&'a str, Vec<&'a Node>>, mut node: &'a Node) -> &'a Node {
		while let Some(first) = children.get(node.id.as_str()).and_then(|kids| kids.first()) {
			let keep = if node.mean.ms > 0.0 {
				first.mean.ms >= node.mean.ms * 0.8
			} else {
				node.mean.gpu_units > 0.0 && first.mean.gpu_units >= node.mean.gpu_units * 0.8
			};
			if !keep {
				break;
			}
			node = first;
		}
		node
	}
	let mut out = Vec::new();
	let tops: Vec<&Node> = tree.iter().filter(|node| node.parent.is_none() && node.id != attribute::SHARED && (node.mean.ms > 0.0 || node.mean.gpu_units > 0.0)).collect();
	for top in tops.into_iter().take(3) {
		let start = through(&children, top);
		out.push((start, 0));
		for child in children.get(start.id.as_str()).into_iter().flatten().take(6) {
			let child = through(&children, child);
			out.push((child, 1));
			if out.len() < 6 {
				for grandchild in children.get(child.id.as_str()).into_iter().flatten().take(3) {
					out.push((through(&children, grandchild), 2));
				}
			}
		}
	}
	out
}

/// One instance's cost, view by view, and the costliest instances under it.
fn explain(ids: &InstanceIds, costed: &Costed, scene: &Scene, id: &str) -> Result<(Value, Vec<String>)> {
	let root = ids.require(id)?;
	let under = |referent: Ref| {
		let mut current = referent;
		while !current.is_none() {
			if current == root {
				return true;
			}
			current = ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
		}
		false
	};
	let mut lines = vec![format!("explain {id}:")];
	let mut per_view = Vec::new();
	let mut leaves: BTreeMap<String, Work> = BTreeMap::new();
	// where each part item of the subtree was in each view: drawn, hidden by occlusion, or neither
	let items: Vec<usize> = costed.analysis.items.iter().enumerate().filter(|(_, item)| item.part.is_some_and(|part| under(costed.owners.part(part)))).map(|(index, _)| index).collect();
	for (cost, work) in costed.costs.iter().zip(&costed.works) {
		let mut total = Work::default();
		let mut ordered: Vec<(String, &Work)> = work.by_instance.iter().filter(|(referent, _)| under(**referent)).map(|(referent, work)| (ids.id(*referent).unwrap_or_default().to_string(), work)).collect();
		ordered.sort_by(|a, b| a.0.cmp(&b.0));
		for (leaf, entry) in ordered {
			total.add(entry);
			leaves.entry(leaf).or_default().add(&entry.scaled(1.0 / costed.works.len().max(1) as f64));
		}
		let drawn: std::collections::HashSet<u32> = cost.drawn.iter().map(|(item, _, _)| *item).collect();
		let hidden: std::collections::HashSet<u32> = cost.hidden.iter().copied().collect();
		let shown = items.iter().filter(|item| drawn.contains(&(**item as u32))).count();
		let occluded = items.iter().filter(|item| hidden.contains(&(**item as u32))).count();
		let batches: Vec<Value> = cost
			.batches
			.iter()
			.filter(|batch| batch.items.iter().any(|item| items.contains(item)))
			.take(12)
			.map(|batch| json!({ "label": batch.label, "draws": batch.draws, "instances": batch.instances, "triangles": batch.triangles, "ofThese": batch.items.iter().filter(|item| items.contains(item)).count() }))
			.collect();
		lines.push(format!(
			"  {:<16} {}  {:.1} draws {:.0} tris {:.0} px {:.0} blended {:.0} lit px | parts drawn {shown}, occluded {occluded}, culled or out of view {}",
			cost.name,
			ms(total.ms),
			total.draws,
			total.triangles,
			total.pixels,
			total.blended,
			total.lit_pixels + total.light_pixels,
			items.len().saturating_sub(shown + occluded)
		));
		per_view.push(json!({ "view": cost.name, "work": total.rounded(), "partsDrawn": shown, "partsOccluded": occluded, "partsCulled": items.len().saturating_sub(shown + occluded), "batches": batches }));
	}
	let mut ranked: Vec<(String, Work)> = leaves.into_iter().collect();
	ranked.sort_by(|a, b| b.1.ms.total_cmp(&a.1.ms).then_with(|| b.1.gpu_units.total_cmp(&a.1.gpu_units)).then_with(|| a.0.cmp(&b.0)));
	lines.push(format!("  costliest under it (mean per view), of {}:", ranked.len()));
	for (leaf, work) in ranked.iter().take(10) {
		lines.push(format!("    {}  {:.2} draws {:.0} tris {:.0} px {:.0} blended  {leaf}", ms(work.ms), work.draws, work.triangles, work.pixels, work.blended));
	}
	let _ = scene;
	let value = json!({
		"schema": "ror-cost-explain/1",
		"id": id,
		"uid": ids.uid(root),
		"class": ids.info(root).map(|info| info.1.clone()),
		"views": per_view,
		"costliest": ranked.iter().take(50).map(|(leaf, work)| json!({ "id": leaf, "mean": work.rounded() })).collect::<Vec<_>>(),
	});
	Ok((value, lines))
}

/// A heat colour for `t` in 0..1: green (cheap) through yellow to red (costly).
fn ramp(t: f32) -> [u8; 3] {
	let t = t.clamp(0.0, 1.0);
	let (r, g) = if t < 0.5 { (t * 2.0, 1.0) } else { (1.0, 2.0 - t * 2.0) };
	[(40.0 + r * 215.0) as u8, (40.0 + g * 180.0) as u8, 40]
}

/// The heat images over each view's lit frame (its brightness): the pixel's cost, its blended layers,
/// the local lights holding it, and its part's triangles per pixel.
fn write_heat(costed: &Costed, out: &Path) -> Result<()> {
	for ((view, pixels), frame) in costed.costs.iter().zip(&costed.pixels).zip(&costed.frames) {
		let (Some(pixels), Some((width, height, rgba))) = (pixels, frame) else { continue };
		let count = (*width as usize) * (*height as usize);
		if pixels.cost.len() != count {
			continue;
		}
		// each part's triangles per pixel in this view
		let mut density: HashMap<u32, f32> = HashMap::new();
		for (&part, &count) in &pixels.part_pixels {
			let referent = costed.owners.part(part);
			let triangles = costed.works.iter().find(|work| work.name == view.name).and_then(|work| work.by_instance.get(&referent)).map_or(0.0, |work| work.triangles);
			density.insert(part as u32 + 1, (triangles / (count as f64).max(1.0)) as f32);
		}
		let paint = |value: &dyn Fn(usize) -> Option<f32>| -> Vec<u8> {
			let mut image = vec![0u8; count * 4];
			for index in 0..count {
				let luma = (0.3 * rgba[index * 4] as f32 + 0.59 * rgba[index * 4 + 1] as f32 + 0.11 * rgba[index * 4 + 2] as f32) / 255.0;
				let color = match value(index) {
					Some(t) => {
						let heat = ramp(t);
						[heat[0] as f32 * (0.55 + 0.45 * luma), heat[1] as f32 * (0.55 + 0.45 * luma), heat[2] as f32 * (0.55 + 0.45 * luma)]
					}
					None => [luma * 90.0; 3],
				};
				image[index * 4..index * 4 + 4].copy_from_slice(&[color[0] as u8, color[1] as u8, color[2] as u8, 255]);
			}
			image
		};
		// on a log scale: a pixel of opaque Plastic (1 unit) green, 16 units red; one blended layer
		// green-yellow, 16 red
		let cost = paint(&|index| Some(pixels.cost[index].max(1.0).log2() / 4.0));
		let overdraw = paint(&|index| (pixels.layers[index] > 0).then(|| (1.0 + pixels.layers[index] as f32).log2() / 17f32.log2()));
		let lights = paint(&|index| (pixels.lights[index] > 0).then(|| pixels.lights[index] as f32 / 8.0));
		let triangles = paint(&|index| density.get(&pixels.opaque[index]).map(|value| ((value.max(1e-4)).log10() + 3.0) / 3.0));
		for (suffix, image) in [("cost", cost), ("overdraw", overdraw), ("lights", lights), ("density", triangles)] {
			crate::output::write_png(&out.join(format!("{}.heat-{suffix}.png", view.name)), *width, *height, &image, true)?;
		}
	}
	Ok(())
}

/// A trial's result per view.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrialView {
	name: String,
	before: Work,
	after: Work,
	delta: Work,
	draws_before: u64,
	draws_after: u64,
	triangles_before: u64,
	triangles_after: u64,
	#[serde(skip_serializing_if = "Option::is_none")]
	look: Option<Look>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Look {
	/// Share of pixels more than 8 of 255 apart in any channel.
	changed: f64,
	/// The largest difference of any channel.
	max: u8,
	crops: Vec<Crop>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Crop {
	file: String,
	/// x, y, width, height in the look image.
	rect: [u32; 4],
	changed_pixels: u64,
}

const LOOK_THRESHOLD: u8 = 8;

/// Tries each trial of `file` against the baseline: the same views costed again with the edits applied
/// in memory, before and after per view; with `--look` the views drawn before and after and compared.
#[allow(clippy::too_many_arguments)]
fn whatif(setup: &Setup, scene: &Scene, ids: &InstanceIds, baseline: &Costed, file: &EditsFile, reload: &dyn Fn() -> Result<Document>, out: &Path, input: &str) -> Result<Vec<String>> {
	let options = setup.options;
	let (look_width, look_height) = {
		let (width, height) = setup.preset.analysis_size();
		((width * 2).min(setup.preset.width), (height * 2).min(setup.preset.height))
	};
	let look_requests: Vec<crate::render::ViewRequest> = setup.shots.iter().map(|(camera, _, _)| crate::render::ViewRequest { width: look_width, height: look_height, camera: camera.spec(), ..Default::default() }).collect();
	let before_images = if options.look { Some(setup.renderer.render_gated(scene, &look_requests, setup.gpu)?.0) } else { None };
	let mut lines = vec![format!("what-if ({WHATIF_SCHEMA}): {} trials against the same {} views", file.trials().len(), setup.names.len())];
	let mut trials_json = Vec::new();
	// each trial's folder (look crops, export kit) its own, even where two names make one file name
	let mut folders: std::collections::HashSet<String> = std::collections::HashSet::new();
	for trial in file.trials() {
		let mut folder_name = file_name(&trial.name);
		let mut repeat = 1;
		while !folders.insert(folder_name.clone()) {
			repeat += 1;
			folder_name = format!("{}-{repeat}", file_name(&trial.name));
		}
		let mut document = reload()?;
		let trial_ids = InstanceIds::new(&document);
		let applied = edits::apply(&mut document, &trial_ids, &trial.edits).with_context(|| format!("trial {:?}", trial.name))?;
		let mut edited = crate::scene::extract::extract(&document);
		// another file's Lighting or MaterialService stays as the baseline drew it (edits to this file's
		// own reach the trial: a GlobalShadows or Clouds edit counts)
		if setup.lent_lighting {
			edited.environment = scene.environment.clone();
		}
		if setup.lent_materials {
			edited.materials = scene.materials.clone();
		}
		let merges = merge_indices(&edited, &applied.merges);
		let after = cost_inner(setup, &edited, &document, &trial_ids, &merges, false)?;
		let after_images = if options.look { Some(setup.renderer.render_gated(&edited, &look_requests, setup.gpu)?.0) } else { None };
		let folder = out.join("look").join(&folder_name);
		let mut views = Vec::new();
		for (index, (before_work, after_work)) in baseline.works.iter().zip(&after.works).enumerate() {
			let look = match (&before_images, &after_images) {
				(Some(before), Some(after)) => Some(compare(&before[index], &after[index], &folder, &before_work.name)?),
				_ => None,
			};
			views.push(TrialView {
				name: before_work.name.clone(),
				before: before_work.total.rounded(),
				after: after_work.total.rounded(),
				delta: after_work.total.minus(&before_work.total).rounded(),
				draws_before: baseline.costs[index].total.draws,
				draws_after: after.costs[index].total.draws,
				triangles_before: baseline.costs[index].total.triangles,
				triangles_after: after.costs[index].total.triangles,
				look,
			});
		}
		let mean_before = mean_work(&baseline.works);
		let mean_after = mean_work(&after.works);
		let delta = mean_after.minus(&mean_before).rounded();
		let worst_saving = views.iter().fold(None, |best: Option<&TrialView>, view| if best.is_none_or(|best| view.delta.ms < best.delta.ms) { Some(view) } else { best });
		let mut by_look: Vec<&TrialView> = views.iter().filter(|view| view.look.is_some()).collect();
		by_look.sort_by(|a, b| b.look.as_ref().map_or(0.0, |look| look.changed).total_cmp(&a.look.as_ref().map_or(0.0, |look| look.changed)).then_with(|| a.name.cmp(&b.name)));
		// what changed most, instance by instance (mean per view, by id)
		let before_tree: HashMap<String, f64> = attribute::tree(ids, &baseline.works).into_iter().map(|node| (node.id, node.mean.ms)).collect();
		let after_tree: HashMap<String, f64> = attribute::tree(&trial_ids, &after.works).into_iter().map(|node| (node.id, node.mean.ms)).collect();
		let mut changes: Vec<(String, f64)> = before_tree.iter().map(|(id, before)| (id.clone(), after_tree.get(id).copied().unwrap_or(0.0) - before)).chain(after_tree.iter().filter(|(id, _)| !before_tree.contains_key(*id)).map(|(id, after)| (id.clone(), *after))).filter(|(_, delta)| delta.abs() > 1e-4).collect();
		changes.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
		let changed_nodes: Vec<Value> = changes.iter().filter(|(id, _)| id.matches(['.', '[']).count() >= 2).take(15).map(|(id, delta)| json!({ "id": id, "deltaMs": (delta * 1e4).round() / 1e4 })).collect();
		lines.push(format!(
			"  {}: {} edits ({} changes), mean {} -> {} ({:+.3} ms/view: {:+.1} draws, {:+.0} triangles, {:+.0} blended px); best view {} {:+.3} ms",
			trial.name,
			trial.edits.len(),
			applied.changes.len(),
			ms(mean_before.ms),
			ms(mean_after.ms),
			delta.ms,
			delta.draws,
			delta.triangles,
			delta.blended,
			worst_saving.map(|view| view.name.as_str()).unwrap_or("-"),
			worst_saving.map_or(0.0, |view| view.delta.ms)
		));
		if let Some(first) = by_look.first().and_then(|view| view.look.as_ref().map(|look| (view, look))) {
			let unchanged = views.iter().filter(|view| view.look.as_ref().is_some_and(|look| look.changed == 0.0)).count();
			lines.push(format!("    look: worst {} {:.3} % of pixels changed (max {}), {unchanged} of {} views identical; crops in {}", first.0.name, first.1.changed * 100.0, first.1.max, views.len(), folder.display()));
		}
		for warning in &applied.warnings {
			lines.push(format!("    note: {warning}"));
		}
		let predicted = json!({ "meanMs": delta.ms, "worstView": worst_saving.map(|view| view.name.clone()), "worstMs": worst_saving.map(|view| view.delta.ms) });
		if let Some(dir) = &options.export {
			let kit = dir.join(&folder_name);
			edits::export(&kit, input, &trial.name, &applied.changes, Some(&predicted))?;
			lines.push(format!("    export: {} (lune run apply.luau <place> <out> changes.json)", kit.display()));
		}
		trials_json.push(json!({
			"name": trial.name,
			"edits": trial.edits,
			"changes": applied.changes.len(),
			"warnings": applied.warnings,
			"mean": { "before": mean_before, "after": mean_after, "delta": delta },
			"worstSaving": worst_saving.map(|view| json!({ "view": view.name, "deltaMs": view.delta.ms })),
			"worstLook": by_look.first().map(|view| json!({ "view": view.name, "changed": view.look.as_ref().map(|look| look.changed) })),
			"changedInstances": changed_nodes,
			"views": views,
		}));
	}
	crate::output::write_json_atomic(&out.join("whatif.json"), &json!({ "schema": WHATIF_SCHEMA, "input": input, "preset": setup.preset, "trials": trials_json }), true)?;
	crate::output::write_atomic(&out.join("whatif.txt"), (lines.join("\n") + "\n").as_bytes())?;
	Ok(lines)
}

/// Compares a view drawn before and after: the share of pixels changed, and crops (before | after) of
/// the regions that changed most.
fn compare(before: &crate::render::ViewImage, after: &crate::render::ViewImage, folder: &Path, view: &str) -> Result<Look> {
	let (width, height) = (before.width as usize, before.height as usize);
	let mut changed = vec![false; width * height];
	let mut max = 0u8;
	let mut count = 0u64;
	for (index, changed) in changed.iter_mut().enumerate() {
		let difference = (0..3).map(|channel| before.rgba[index * 4 + channel].abs_diff(after.rgba[index * 4 + channel])).max().unwrap_or(0);
		max = max.max(difference);
		if difference > LOOK_THRESHOLD {
			*changed = true;
			count += 1;
		}
	}
	let mut crops = Vec::new();
	if count > 0 {
		std::fs::create_dir_all(folder)?;
		// 16-pixel cells with a change, joined into regions; the three largest by changed pixels
		const CELL: usize = 16;
		let (columns, rows) = (width.div_ceil(CELL), height.div_ceil(CELL));
		let mut cells = vec![0u32; columns * rows];
		for y in 0..height {
			for x in 0..width {
				if changed[y * width + x] {
					cells[(y / CELL) * columns + x / CELL] += 1;
				}
			}
		}
		let mut region = vec![usize::MAX; columns * rows];
		let mut regions: Vec<(u64, [usize; 4])> = Vec::new();
		for start in 0..cells.len() {
			if cells[start] == 0 || region[start] != usize::MAX {
				continue;
			}
			let id = regions.len();
			let mut stack = vec![start];
			let mut bounds = [usize::MAX, usize::MAX, 0, 0];
			let mut pixels = 0u64;
			region[start] = id;
			while let Some(cell) = stack.pop() {
				let (cx, cy) = (cell % columns, cell / columns);
				pixels += cells[cell] as u64;
				bounds = [bounds[0].min(cx), bounds[1].min(cy), bounds[2].max(cx), bounds[3].max(cy)];
				for (dx, dy) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
					let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
					if nx < 0 || ny < 0 || nx >= columns as i64 || ny >= rows as i64 {
						continue;
					}
					let next = ny as usize * columns + nx as usize;
					if cells[next] > 0 && region[next] == usize::MAX {
						region[next] = id;
						stack.push(next);
					}
				}
			}
			regions.push((pixels, bounds));
		}
		regions.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
		for (rank, (pixels, bounds)) in regions.into_iter().take(3).enumerate() {
			let pad = CELL;
			let x0 = (bounds[0] * CELL).saturating_sub(pad);
			let y0 = (bounds[1] * CELL).saturating_sub(pad);
			let x1 = ((bounds[2] + 1) * CELL + pad).min(width);
			let y1 = ((bounds[3] + 1) * CELL + pad).min(height);
			let (crop_width, crop_height) = (x1 - x0, y1 - y0);
			// before | after | where it changed (red over the dimmed picture), side by side
			let row = crop_width * 3;
			let mut image = vec![0u8; row * crop_height * 4];
			for y in 0..crop_height {
				for x in 0..crop_width {
					let at = (y0 + y) * width + x0 + x;
					let source = at * 4;
					let left = (y * row + x) * 4;
					let middle = (y * row + crop_width + x) * 4;
					let right = (y * row + crop_width * 2 + x) * 4;
					image[left..left + 4].copy_from_slice(&before.rgba[source..source + 4]);
					image[middle..middle + 4].copy_from_slice(&after.rgba[source..source + 4]);
					let luma = (before.rgba[source] as u32 * 3 + before.rgba[source + 1] as u32 * 6 + before.rgba[source + 2] as u32) / 30;
					let mark = if changed[at] { [255, 40, 40, 255] } else { [luma as u8, luma as u8, luma as u8, 255] };
					image[right..right + 4].copy_from_slice(&mark);
				}
			}
			let name = format!("{}.crop{}.png", file_name(view), rank + 1);
			crate::output::write_png(&folder.join(&name), row as u32, crop_height as u32, &image, true)?;
			crops.push(Crop { file: folder.join(&name).display().to_string(), rect: [x0 as u32, y0 as u32, crop_width as u32, crop_height as u32], changed_pixels: pixels });
		}
		crate::output::write_png(&folder.join(format!("{}.before.png", file_name(view))), before.width, before.height, &before.rgba, true)?;
		crate::output::write_png(&folder.join(format!("{}.after.png", file_name(view))), after.width, after.height, &after.rgba, true)?;
	}
	Ok(Look { changed: ((count as f64 / (width * height).max(1) as f64) * 1e6).round() / 1e6, max, crops })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::camera::CameraSpec;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use glam::{Affine3A, Vec3};

	/// A wall, a block in front of it, a block hidden behind it and a see-through sheet in front.
	fn map() -> Document {
		let mut model = ModelBuilder::new("Map");
		let at = |x: f32, y: f32, z: f32| Affine3A::from_translation(Vec3::new(x, y, z));
		model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(40.0, 20.0, 1.0), ..Default::default() });
		model.part(&PartSpec { name: "Crate".into(), cframe: at(4.0, 0.0, 6.0), size: Vec3::new(2.0, 2.0, 2.0), material: 512, ..Default::default() });
		model.part(&PartSpec { name: "Behind".into(), cframe: at(0.0, 0.0, -8.0), size: Vec3::new(3.0, 3.0, 3.0), material: 800, ..Default::default() });
		model.part(&PartSpec { name: "Fog".into(), cframe: at(0.0, 0.0, 10.0), size: Vec3::new(30.0, 16.0, 0.2), transparency: 0.8, ..Default::default() });
		// a base hidden behind the wall holding a prop in front of it: hiding the base would hide the prop
		let base = model.part(&PartSpec { name: "Base".into(), cframe: at(-8.0, 0.0, -8.0), size: Vec3::new(2.0, 2.0, 2.0), ..Default::default() });
		let prop = model.part(&PartSpec { name: "Prop".into(), cframe: at(-8.0, -3.0, 6.0), size: Vec3::new(1.5, 1.5, 1.5), material: 800, ..Default::default() });
		model.dom.transfer_within(prop, base);
		model.into_document()
	}

	#[test]
	fn the_loop_finds_the_unseen_part_and_a_trial_measures_hiding_the_sheet() {
		let state = std::env::temp_dir().join(format!("ror-loop-test-{}", std::process::id()));
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = crate::render::Renderer::new(gpu, assets).expect("a renderer");
		let document = map();
		let scene = crate::scene::extract::extract(&document);
		let spec = CameraSpec::LookAt { eye: [0.0, 2.0, 40.0], target: [0.0, 0.0, 0.0], fov: 60.0 };
		let shots = vec![(spec.place(None, 320, 180), 320, 180)];
		let edits = EditsFile::read(r#"{"trials": [{"name": "fog off", "edits": [{"op": "hide", "targets": ["Map.Fog"]}]}]}"#).unwrap();
		let options = CostOptions { attribute: true, pixels: true, edits: Some(edits), ..Default::default() };
		let setup = Setup { renderer: &renderer, options: &options, names: vec!["front".into()], shots: &shots, preset: Preset::custom(320, 180, 21), quality: Quality::of(21, false), gpu: None, lent_lighting: false, lent_materials: false };
		let out = state.join("out");
		std::fs::create_dir_all(&out).unwrap();
		let reload = || Ok(map());
		let (costed, lines) = run(&setup, &scene, &document, Some(&reload), &out, "map").unwrap();
		let analysis: Value = serde_json::from_slice(&std::fs::read(out.join("analysis.json")).unwrap()).unwrap();
		assert_eq!(analysis["schema"], SCHEMA);
		let findings = analysis["findings"].as_array().unwrap();
		let unseen = findings.iter().find(|finding| finding["kind"] == "unseen").expect("an unseen finding");
		assert_eq!(unseen["edit"]["targets"], json!(["Map.Behind"]), "{unseen}");
		assert!(!findings.iter().any(|finding| finding["kind"] == "unseen" && finding["edit"]["targets"].to_string().contains("Map.Base")), "the base holding a visible prop is no unseen root");
		let fog = findings.iter().find(|finding| finding["kind"] == "seeThrough").expect("a see-through finding");
		assert!(fog["why"]["blended"].as_f64().unwrap() > 1000.0, "{fog}");
		// the wall is in front everywhere it shows; the hidden block has no pixel
		let ids = InstanceIds::new(&document);
		let work = &costed.works[0];
		let of = |id: &str| work.by_instance.get(&ids.resolve(id).unwrap()).copied().unwrap_or_default();
		assert!(of("Map.Wall").pixels > of("Map.Crate").pixels && of("Map.Crate").pixels > 0.0);
		assert_eq!(of("Map.Behind").pixels, 0.0);
		// the trial: hiding the sheet takes its blended pixels and its draw away, nothing else
		let whatif: Value = serde_json::from_slice(&std::fs::read(out.join("whatif.json")).unwrap()).unwrap();
		assert_eq!(whatif["schema"], WHATIF_SCHEMA);
		let delta = &whatif["trials"][0]["views"][0]["delta"];
		assert!((delta["blended"].as_f64().unwrap() + of("Map.Fog").blended).abs() < 1.0, "{delta}");
		assert!(delta["ms"].as_f64().unwrap() < 0.0 && delta["draws"].as_f64().unwrap() <= -1.0 + 1e-6, "{delta}");
		assert!(lines.iter().any(|line| line.contains("fog off")), "{lines:?}");
		// the same input, the same output
		let again = state.join("again");
		std::fs::create_dir_all(&again).unwrap();
		run(&setup, &scene, &document, Some(&reload), &again, "map").unwrap();
		for file in ["analysis.json", "tree.json", "whatif.json"] {
			assert_eq!(std::fs::read(out.join(file)).unwrap(), std::fs::read(again.join(file)).unwrap(), "{file} differs between two runs");
		}
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn desktop_preset_prices_cpu_as_desktop_and_refuses_gpu_ms() {
		let state = std::env::temp_dir().join(format!("ror-desktop-pricing-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = crate::render::Renderer::new(gpu, assets).expect("a renderer");
		let mut model = ModelBuilder::new("Map");
		let at = |x: f32, y: f32, z: f32| Affine3A::from_translation(Vec3::new(x, y, z));
		let wall = model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(40.0, 20.0, 1.0), ..Default::default() });
		model.part(&PartSpec { name: "Behind".into(), cframe: at(0.0, 0.0, -8.0), size: Vec3::new(3.0, 3.0, 3.0), material: 800, ..Default::default() });
		model.part(&PartSpec { name: "Fog".into(), cframe: at(0.0, 0.0, 10.0), size: Vec3::new(30.0, 16.0, 0.2), transparency: 0.8, ..Default::default() });
		model.child(
			wall,
			rbx_dom_weak::InstanceBuilder::new("PointLight")
				.with_name("Lamp")
				.with_property("Brightness", 4.0f32)
				.with_property("Range", 40.0f32),
		);
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let spec = CameraSpec::LookAt { eye: [0.0, 2.0, 40.0], target: [0.0, 0.0, 0.0], fov: 60.0 };
		let shots = vec![(spec.place(None, 320, 180), 320, 180)];
		let options = CostOptions { attribute: true, pixels: true, ..Default::default() };

		let phone_out = state.join("phone");
		let custom_out = state.join("custom");
		let desk_out = state.join("desktop");
		std::fs::create_dir_all(&phone_out).unwrap();
		std::fs::create_dir_all(&custom_out).unwrap();
		std::fs::create_dir_all(&desk_out).unwrap();
		let phone_preset = Preset::named("phone").unwrap();
		let custom_preset = Preset::custom(320, 180, 21);
		let desk_preset = Preset::named("desktop").unwrap();
		let phone_setup = Setup { renderer: &renderer, options: &options, names: vec!["front".into()], shots: &shots, preset: phone_preset, quality: Quality::of(2, true), gpu: None, lent_lighting: false, lent_materials: false };
		let custom_setup = Setup { renderer: &renderer, options: &options, names: vec!["front".into()], shots: &shots, preset: custom_preset, quality: Quality::of(21, false), gpu: None, lent_lighting: false, lent_materials: false };
		let desk_setup = Setup { renderer: &renderer, options: &options, names: vec!["front".into()], shots: &shots, preset: desk_preset, quality: Quality::of(21, false), gpu: None, lent_lighting: false, lent_materials: false };
		run(&phone_setup, &scene, &document, None, &phone_out, "map").unwrap();
		run(&custom_setup, &scene, &document, None, &custom_out, "map").unwrap();
		let (_, desk_lines) = run(&desk_setup, &scene, &document, None, &desk_out, "map").unwrap();

		let phone_json: Value = serde_json::from_slice(&std::fs::read(phone_out.join("analysis.json")).unwrap()).unwrap();
		let custom_json: Value = serde_json::from_slice(&std::fs::read(custom_out.join("analysis.json")).unwrap()).unwrap();
		let custom_txt = std::fs::read_to_string(custom_out.join("analysis.txt")).unwrap();
		let desk_json: Value = serde_json::from_slice(&std::fs::read(desk_out.join("analysis.json")).unwrap()).unwrap();
		let desk_txt = std::fs::read_to_string(desk_out.join("analysis.txt")).unwrap();

		// Phone preset: calibrated phone frame
		assert_eq!(phone_json["fidelity"]["cpuCalibrated"], true);
		assert_eq!(phone_json["fidelity"]["gpuCalibrated"], true);
		assert!(phone_json["mean"]["gpuMs"].as_f64().unwrap() > 0.0);
		assert!(phone_json["mean"]["gpuUnits"].as_f64().unwrap() > 0.0);

		// Custom preset (no --preset): keeps phone frame pricing and rankings (matching 5f1abba)
		assert_eq!(custom_json["fidelity"]["pricing"], "phone");
		assert_eq!(custom_json["fidelity"]["cpuCalibrated"], true);
		assert_eq!(custom_json["fidelity"]["gpuCalibrated"], true);
		let custom_cpu = custom_json["mean"]["cpuMs"].as_f64().unwrap();
		let custom_gpu = custom_json["mean"]["gpuMs"].as_f64().unwrap();
		let custom_ms = custom_json["mean"]["ms"].as_f64().unwrap();
		assert!(custom_gpu > 0.3, "no-preset custom run must keep phone gpuMs: {custom_json}");
		assert!((custom_cpu - 0.0799).abs() <= 2e-4, "expected 5f1abba cpuMs 0.0799, got {custom_cpu}");
		assert!((custom_gpu - 0.3853).abs() <= 5e-4, "expected 5f1abba gpuMs 0.3853, got {custom_gpu}");
		assert!((custom_ms - 0.1551).abs() <= 2e-4, "expected 5f1abba ms 0.1551, got {custom_ms}");
		assert!((custom_ms - (custom_cpu + custom_gpu * prices::GPU_TO_FRAME)).abs() <= 2e-4);
		assert!(custom_txt.contains("phone frame"), "no-preset analysis.txt must keep phone frame wording:\n{custom_txt}");
		assert_eq!(custom_json["findings"][0]["kind"], "seeThrough", "GPU-heavy Fog must rank #1 on no-preset phone frame");

		// Desktop preset (--preset desktop): estimated CPU, refused gpuMs as null, GPU findings not buried behind CPU
		assert_eq!(desk_json["fidelity"]["pricing"], "desktop");
		assert_eq!(desk_json["fidelity"]["cpuCalibrated"], false);
		assert_eq!(desk_json["fidelity"]["cpuBasis"], "estimated");
		assert_eq!(desk_json["fidelity"]["gpuCalibrated"], false);
		assert_eq!(desk_json["fidelity"]["status"], "partial");
		assert!(desk_json["fidelity"]["gpuRefusal"]["settleWith"].is_string());
		assert!(desk_json["unmodeled"].as_array().unwrap().iter().any(|u| u["kind"] == "desktopGpuMs"));
		assert!(desk_json["mean"]["gpuMs"].is_null(), "desktop gpuMs must be null, not 0: {}", desk_json["mean"]);
		assert!(desk_json["views"][0]["work"]["gpuMs"].is_null());
		assert!(desk_json["prices"].as_array().unwrap().iter().find(|p| p["key"] == "gpuUnit").unwrap()["value"].is_null());
		assert!(desk_json["prices"].as_array().unwrap().iter().find(|p| p["key"] == "gpuToFrame").unwrap()["value"].is_null());
		assert!(desk_json["mean"]["gpuUnits"].as_f64().unwrap() > 0.0);
		assert!((desk_json["mean"]["ms"].as_f64().unwrap() - desk_json["mean"]["cpuMs"].as_f64().unwrap()).abs() <= 1e-4);
		assert!(!desk_txt.to_ascii_lowercase().contains("phone"), "desktop analysis.txt mentioned phone:\n{desk_txt}");
		assert!(desk_txt.contains("gpuMs: null") && !desk_txt.contains("GPU 0.00 ms"), "desktop analysis.txt must refuse GPU ms as null:\n{desk_txt}");
		let desk_findings = desk_json["findings"].as_array().unwrap();
		let fog_rank = desk_findings.iter().find(|f| f["kind"] == "seeThrough").unwrap()["rank"].as_u64().unwrap();
		let light_finding = desk_findings.iter().find(|f| f["kind"] == "lightRange").expect("desktop Q21 lightRange finding");
		let unseen_rank = desk_findings.iter().find(|f| f["kind"] == "unseen").unwrap()["rank"].as_u64().unwrap();
		assert!(fog_rank < unseen_rank, "desktop GPU+CPU seeThrough finding must not hide behind tiny unseen CPU finding: {desk_findings:?}");
		assert!(light_finding["saving"]["meanGpuUnits"].as_f64().unwrap() > 0.0, "{light_finding}");
		assert_eq!(light_finding["saving"]["worstView"], "front", "{light_finding}");
		assert!(desk_lines.iter().any(|l| l.contains("gpuUnits/view") && l.contains("[lightRange]")), "{desk_lines:?}");
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn stdout_prints_one_memory_line_matching_preset_pricing() {
		let state = std::env::temp_dir().join(format!("ror-one-mem-line-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();

		let tex_path = state.join("tile64.png");
		{
			let file = std::fs::File::create(&tex_path).unwrap();
			let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 64, 64);
			enc.set_color(png::ColorType::Rgb);
			enc.set_depth(png::BitDepth::Eight);
			let mut w = enc.write_header().unwrap();
			w.write_image_data(&vec![128u8; 64 * 64 * 3]).unwrap();
		}
		let tex_uri = format!("file://{}", tex_path.to_string_lossy().replace('\\', "/"));

		let mut model = ModelBuilder::new("Map");
		let wall = model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(20.0, 10.0, 1.0), ..Default::default() });
		model.child(
			wall,
			rbx_dom_weak::InstanceBuilder::new("Decal")
				.with_property("Texture", rbx_dom_weak::types::ContentId::from(tex_uri))
				.with_property("Face", rbx_dom_weak::types::Enum::from_u32(5)),
		);
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(state.join("cache")) });
		let renderer = crate::render::Renderer::new(gpu, assets).expect("a renderer");

		let spec = CameraSpec::LookAt { eye: [0.0, 2.0, 25.0], target: [0.0, 0.0, 0.0], fov: 60.0 };
		let views = vec![crate::job::NamedView {
			name: "front".into(),
			view: crate::render::ViewRequest { width: 320, height: 180, camera: spec.clone(), ..Default::default() },
		}];
		let shots = vec![(spec.place(None, 320, 180), 320, 180)];

		let run_case = |name: &str, options: CostOptions| -> (crate::cost::CostReport, std::path::PathBuf) {
			let out = state.join(name);
			std::fs::create_dir_all(&out).unwrap();
			let rep = crate::cost::run(&renderer, &scene, &document, &options, &views, &shots, &out, None, None, "map", (false, false)).unwrap();
			(rep, out)
		};

		let mem_lines = |lines: &[String], agent: &[String]| -> Vec<String> {
			lines.iter().chain(agent.iter()).filter(|l| l.starts_with("memory (")).cloned().collect()
		};

		// Without --memory (default): reads zero textures and emits no memory block or line.
		let before_bytes = renderer.assets.memory_cache_bytes();
		let (opt_out, opt_out_dir) = run_case("opt_out", CostOptions { attribute: true, pixels: false, no_images: true, ..Default::default() });
		assert!(opt_out.memory.is_none(), "default run without --memory must omit CostReport.memory");
		assert!(mem_lines(&opt_out.lines, &opt_out.agent).is_empty(), "default run without --memory must emit zero memory lines");
		let opt_out_analysis: Value = serde_json::from_slice(&std::fs::read(opt_out_dir.join("analysis.json")).unwrap()).unwrap();
		assert!(opt_out_analysis.get("memory").is_none(), "default analysis.json without --memory must omit memory key");
		assert_eq!(renderer.assets.memory_cache_bytes(), before_bytes, "cost run without --memory must not read scene textures");

		// With --memory: prints one memory line matching preset pricing and populates memory report.
		let (plain, _) = run_case("plain", CostOptions { attribute: true, pixels: false, no_images: true, memory: true, ..Default::default() });
		let (phone, _) = run_case("phone", CostOptions { attribute: true, pixels: false, no_images: true, memory: true, preset: Some("phone".into()), ..Default::default() });
		let (desk, _) = run_case("desktop", CostOptions { attribute: true, pixels: false, no_images: true, memory: true, preset: Some("desktop".into()), ..Default::default() });
		let (no_agent, _) = run_case("no_agent", CostOptions { attribute: false, pixels: false, no_images: true, memory: true, ..Default::default() });
		assert!(renderer.assets.memory_cache_bytes() > before_bytes, "--memory run must read the texture");

		let plain_mem = mem_lines(&plain.lines, &plain.agent);
		let phone_mem = mem_lines(&phone.lines, &phone.agent);
		let desk_mem = mem_lines(&desk.lines, &desk.agent);
		let no_agent_mem = mem_lines(&no_agent.lines, &no_agent.agent);

		assert_eq!(plain_mem.len(), 1, "plain --memory stdout must print one memory line: lines={:?} agent={:?}", plain.lines, plain.agent);
		assert_eq!(phone_mem.len(), 1, "phone --memory stdout must print one memory line: lines={:?} agent={:?}", phone.lines, phone.agent);
		assert_eq!(desk_mem.len(), 1, "desktop --memory stdout must print one memory line: lines={:?} agent={:?}", desk.lines, desk.agent);
		assert_eq!(no_agent_mem.len(), 1, "non-attributed --memory stdout must print one memory line: {:?}", no_agent.lines);
		assert_eq!(plain_mem[0], phone_mem[0], "plain (phone frame) and --preset phone must report the exact same memory line");
		assert!(plain_mem[0].contains("ETC2") && !plain_mem[0].contains("BCn"), "plain phone-frame run must print ETC2: {}", plain_mem[0]);
		assert!(desk_mem[0].contains("BCn") && !desk_mem[0].contains("ETC2"), "desktop run must print BCn: {}", desk_mem[0]);
		assert!(!desk_mem[0].to_ascii_lowercase().contains("phone"), "desktop memory line must not mention phone: {}", desk_mem[0]);
		assert_ne!(plain_mem[0], desk_mem[0], "phone and desktop memory lines must differ");
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn budgets_parse_presets_and_limits() {
		let phone = Budget::parse("phone").unwrap();
		assert_eq!((phone.draws, phone.triangles), (600, 800_000));
		let custom = Budget::parse("draws=400,triangles=500k,ms=6.5").unwrap();
		assert_eq!((custom.draws, custom.triangles, custom.ms), (400, 500_000, 6.5));
		assert_eq!(custom.check(450, 100, 1.0), ["draws 450 > 400"]);
		assert!(Budget::parse("frames=3").is_err());
	}

	#[test]
	fn missing_texture_id_is_named_on_stdout_and_analysis_txt_when_memory_requested() {
		let state = std::env::temp_dir().join(format!("ror-missing-tex-warn-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();

		let mut model = ModelBuilder::new("MissingTex");
		let wall = model.part(&PartSpec { name: "Wall".into(), size: Vec3::new(10.0, 10.0, 1.0), ..Default::default() });
		model.child(
			wall,
			rbx_dom_weak::InstanceBuilder::new("Decal")
				.with_property("Texture", rbx_dom_weak::types::ContentId::from("rbxassetid://99999991"))
				.with_property("Face", rbx_dom_weak::types::Enum::from_u32(5)),
		);
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(state.join("cache")) });
		let renderer = crate::render::Renderer::new(gpu, assets).expect("a renderer");

		let spec = CameraSpec::LookAt { eye: [0.0, 2.0, 20.0], target: [0.0, 0.0, 0.0], fov: 60.0 };
		let views = vec![crate::job::NamedView {
			name: "front".into(),
			view: crate::render::ViewRequest { width: 320, height: 180, camera: spec.clone(), ..Default::default() },
		}];
		let shots = vec![(spec.place(None, 320, 180), 320, 180)];

		// Without --memory: no missing texture warning emitted (plain cost unchanged).
		let out_plain = state.join("plain");
		std::fs::create_dir_all(&out_plain).unwrap();
		let plain = crate::cost::run(&renderer, &scene, &document, &CostOptions { attribute: true, pixels: false, no_images: true, memory: false, ..Default::default() }, &views, &shots, &out_plain, None, None, "map", (false, false)).unwrap();
		assert!(plain.warnings.is_empty(), "plain run without --memory must emit zero warnings: {:?}", plain.warnings);
		assert!(!plain.lines.iter().any(|l| l.contains("texture not loaded")), "plain run without --memory must not warn on missing textures: {:?}", plain.lines);
		assert!(!plain.agent.iter().any(|l| l.contains("texture not loaded")));

		// With --memory: missing texture id is printed on stdout (lines + agent) and in analysis.txt.
		let out_mem = state.join("mem");
		std::fs::create_dir_all(&out_mem).unwrap();
		let mem = crate::cost::run(&renderer, &scene, &document, &CostOptions { attribute: true, pixels: false, no_images: true, memory: true, ..Default::default() }, &views, &shots, &out_mem, None, None, "map", (false, false)).unwrap();
		assert!(mem.warnings.iter().any(|w| w.contains("texture not loaded") && w.contains("rbxassetid://99999991")), "{:?}", mem.warnings);
		assert!(mem.lines.iter().any(|l| l.contains("warning: texture not loaded (bytes uncounted): rbxassetid://99999991")), "{:?}", mem.lines);
		assert!(mem.agent.iter().any(|l| l.contains("warning: texture not loaded (bytes uncounted): rbxassetid://99999991")), "{:?}", mem.agent);
		let txt = std::fs::read_to_string(out_mem.join("analysis.txt")).unwrap();
		assert!(txt.contains("warning: texture not loaded (bytes uncounted): rbxassetid://99999991"), "{txt}");
		let _ = std::fs::remove_dir_all(&state);
	}
}
