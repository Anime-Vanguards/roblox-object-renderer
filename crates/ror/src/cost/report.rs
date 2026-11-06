//! What would cut a view's draw calls most, from its batches: each opportunity names what splits
//! batches (or draws alone) and what one change would save, by the same rules that count them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use super::{Analysis, Batch, Cluster, Count, CostReport, Kind, Pass, ViewCost};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opportunity {
	/// The same in every view it applies to (what the summary groups by).
	pub id: String,
	pub kind: &'static str,
	pub title: String,
	/// What the change would save in this view.
	pub saves: Count,
	pub detail: Vec<String>,
	pub examples: Vec<String>,
}

/// An opportunity over every view.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
	pub id: String,
	pub kind: &'static str,
	pub title: String,
	/// Views it applies to.
	pub views: usize,
	/// Draw calls and triangles it saves per view, on average over every view (0 where it does not
	/// apply), and in the view it saves most.
	pub mean_draws: f64,
	pub mean_triangles: f64,
	pub max: Count,
	pub detail: Vec<String>,
	pub examples: Vec<String>,
}

fn thousands(value: u64) -> String {
	if value >= 1_000_000 {
		format!("{:.2}M", value as f64 / 1e6)
	} else if value >= 10_000 {
		format!("{:.0}k", value as f64 / 1e3)
	} else if value >= 1_000 {
		format!("{:.1}k", value as f64 / 1e3)
	} else {
		value.to_string()
	}
}

/// A label's two parts: geometry (or what it is) and appearance.
fn split(label: &str) -> (&str, &str) {
	label.split_once(" · ").unwrap_or((label, ""))
}

/// What a batch's first instance is called (the last name of its path), to tell ids apart.
fn named(batch: &Batch) -> String {
	batch.examples.first().map(|path| path.rsplit(['.', '/']).next().unwrap_or(path).to_string()).unwrap_or_default()
}

/// What RenderFidelity Performance would change for one mesh in one view. Roblox steps a mesh down
/// its whole level-of-detail chain by distance only at Performance fidelity (crate::lod; Automatic
/// takes only the levels the file marks high quality, often one), so Performance saves
/// triangles past 250 studs, but copies on both sides of 250, 500, 2,000 or 3,000 studs then draw
/// apart, a batch per level.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelGain {
	/// The geometry part of its batches' labels (`MeshPart <id>`).
	pub mesh: String,
	pub triangles: u64,
	/// Batches its levels would add.
	pub batches: u64,
	/// It has no chain: the triangles are those of the levels Roblox makes for it at run time
	/// ([`crate::lod::RUNTIME_LEVELS`]), which ror cannot draw.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub estimated: bool,
}

/// [`LevelGain`]s of one view (a camera at `eye`, its vertical field of view `fov` degrees): each mesh in
/// view at Performance, with its level-of-detail chain or, without one, the levels Roblox makes for it.
pub fn level_gains(analysis: &Analysis, batches: &[Batch], eye: glam::Vec3, fov: f32) -> Vec<LevelGain> {
	// per batch label: its batches now, the (distance band, level) pairs its copies would take, and
	// the triangles they would save
	let mut labels: BTreeMap<&str, (u64, BTreeSet<(usize, usize)>, u64, bool)> = BTreeMap::new();
	for batch in batches.iter().filter(|batch| batch.kind == Kind::Part && !batch.sorted && batch.label.starts_with("MeshPart ")) {
		let first = &analysis.items[batch.items[0]];
		if first.lod.is_some_and(|rule| rule.fidelity == crate::lod::PERFORMANCE) || first.cluster != Cluster::Static {
			continue;
		}
		let entry = labels.entry(batch.label.as_str()).or_default();
		entry.0 += 1;
		for &index in &batch.items {
			let item = &analysis.items[index];
			let distance = item.center.distance(eye);
			// what it draws now (Automatic may step its high-quality levels), and at Performance
			let (_, now) = super::item_level(item, eye, fov);
			let (level, then) = super::level_under(item, Some(super::ItemLod { fidelity: crate::lod::PERFORMANCE, high_quality: 1 }), eye, fov);
			entry.1.insert((super::distance_band(distance), level));
			entry.2 += now.saturating_sub(then);
			entry.3 |= item.triangles.len() < 2;
		}
	}
	let mut by_mesh: BTreeMap<&str, LevelGain> = BTreeMap::new();
	for (label, (now, groups, triangles, estimated)) in labels {
		let mesh = split(label).0;
		let gain = by_mesh.entry(mesh).or_insert_with(|| LevelGain { mesh: mesh.to_string(), estimated, ..Default::default() });
		gain.triangles += triangles;
		gain.batches += (groups.len() as u64).saturating_sub(now);
	}
	by_mesh.into_values().filter(|gain| gain.triangles > 0 || gain.batches > 0).collect()
}

/// Opportunities of one view's batches.
pub fn opportunities(analysis: &Analysis, batches: &[Batch]) -> Vec<Opportunity> {
	let mut found = Vec::new();
	let item = |batch: &Batch| &analysis.items[batch.items[0]];

	// one geometry in several batches: its copies differ in material, texture, SurfaceAppearance or
	// transparency, and each difference is a draw call of its own (instancing: Color, Size, rotation,
	// Reflectance, DoubleSided and transparency's amount split nothing)
	let mut by_geometry: BTreeMap<u32, Vec<&Batch>> = BTreeMap::new();
	for batch in batches.iter().filter(|batch| batch.kind == Kind::Part && !batch.sorted) {
		let first = item(batch);
		if first.cluster == Cluster::Static
			&& let Some(geometry) = first.geometry
		{
			by_geometry.entry(geometry).or_default().push(batch);
		}
	}
	for (geometry, group) in &by_geometry {
		// its appearances (a batch per level of detail of one appearance is "levelsOfDetail"'s)
		let mut appearances: BTreeMap<&str, u64> = BTreeMap::new();
		for batch in group {
			*appearances.entry(split(&batch.label).1).or_default() += batch.instances;
		}
		if appearances.len() < 2 {
			continue;
		}
		let each = group.iter().map(|batch| batch.draws).min().unwrap_or(1);
		let saves = (appearances.len() as u64 - 1) * each;
		let instances: u64 = appearances.values().sum();
		let mut ranked: Vec<(&str, u64)> = appearances.into_iter().collect();
		ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
		let mut parts: Vec<&&Batch> = group.iter().collect();
		parts.sort_by_key(|batch| std::cmp::Reverse(batch.instances));
		let name = &analysis.labels[*geometry as usize];
		let called = named(parts[0]);
		found.push(Opportunity {
			id: format!("appearance {name}"),
			kind: "appearance",
			title: format!("{name} ({called}): {instances} copies in {} appearances, a batch each; one appearance for all saves {saves} draws", ranked.len()),
			saves: Count { draws: saves, triangles: 0 },
			detail: ranked.iter().take(6).map(|(appearance, count)| format!("{appearance} x{count}")).collect(),
			examples: parts.iter().take(3).flat_map(|batch| batch.examples.first().cloned()).collect(),
		});
	}

	// uploads of one shape under different ids: Roblox instances by id, so each upload is a batch
	// (its importer does not de-duplicate meshes)
	let mut by_shape: HashMap<u64, Vec<&Batch>> = HashMap::new();
	for batch in batches.iter().filter(|batch| batch.kind == Kind::Part && !batch.sorted) {
		let first = item(batch);
		if first.cluster != Cluster::Static {
			continue;
		}
		let Some((key, false)) = first.geometry.and_then(|geometry| analysis.geometry_meshes.get(&geometry)) else { continue };
		if let Some(stats) = analysis.meshes.stats.get(key) {
			by_shape.entry(stats.shape).or_default().push(batch);
		}
	}
	let mut shapes: Vec<(&u64, &Vec<&Batch>)> = by_shape.iter().collect();
	shapes.sort_by_key(|(shape, _)| **shape);
	for (shape, group) in shapes {
		let geometries: BTreeSet<u32> = group.iter().filter_map(|batch| item(batch).geometry).collect();
		if geometries.len() < 2 {
			continue;
		}
		let appearances: HashSet<(u32, bool)> = group.iter().filter_map(|batch| item(batch).appearance.map(|appearance| (appearance, item(batch).transparent))).collect();
		let draws: u64 = group.iter().map(|batch| batch.draws).sum();
		let merged = appearances.len() as u64;
		if draws <= merged {
			continue;
		}
		let names: Vec<String> = geometries.iter().map(|geometry| analysis.labels[*geometry as usize].clone()).collect();
		let instances: u64 = group.iter().map(|batch| batch.instances).sum();
		let called = group.first().map(|batch| named(batch)).unwrap_or_default();
		found.push(Opportunity {
			id: format!("uploads {shape:016x}"),
			kind: "duplicateMeshes",
			title: format!("one mesh ({called}) uploaded as {} assets, {instances} copies: one MeshId for all saves {} draws", geometries.len(), draws - merged),
			saves: Count { draws: draws - merged, triangles: 0 },
			detail: names.into_iter().take(6).collect(),
			examples: group.iter().take(3).flat_map(|batch| batch.examples.first().cloned()).collect(),
		});
	}

	// sorted parts: ForceField and transparent parts holding decals draw one at a time (one draw per
	// depth), ForceField twice
	let mut sorted: BTreeMap<String, (u64, u64, u64, Vec<String>)> = BTreeMap::new();
	for batch in batches.iter().filter(|batch| batch.sorted && matches!(batch.kind, Kind::Part | Kind::Decal | Kind::Texture) && batch.pass == Pass::Opaque) {
		let entry = sorted.entry(batch.label.clone()).or_default();
		entry.0 += batch.draws;
		entry.1 += batch.instances;
		entry.2 = batch.draws / batch.instances.max(1);
		if entry.3.len() < 3 {
			entry.3.extend(batch.examples.first().cloned());
		}
	}
	for (label, (draws, instances, _, examples)) in sorted {
		let each = if label.contains("ForceField") { 2 } else { 1 };
		if draws <= each {
			continue;
		}
		let why = if label.contains("ForceField") { "ForceField draws each part on its own, twice" } else { "a transparent part holding a decal draws on its own, decal too" };
		found.push(Opportunity {
			id: format!("sorted {label}"),
			kind: "sorted",
			title: format!("{label}: {instances} draw apart ({why}): {} draws more than one batch", draws - each),
			saves: Count { draws: draws - each, triangles: 0 },
			detail: Vec::new(),
			examples,
		});
	}

	// effects that show nothing still draw
	let invisible: Vec<&Batch> = batches.iter().filter(|batch| batch.invisible).collect();
	if !invisible.is_empty() {
		let draws: u64 = invisible.iter().map(|batch| batch.draws).sum();
		found.push(Opportunity {
			id: "invisible effects".into(),
			kind: "invisibleEffects",
			title: format!("{} beams and emitters show nothing (Transparency 1 or width 0) and still draw: disable them to save {draws} draws", invisible.len()),
			saves: Count { draws, triangles: invisible.iter().map(|batch| batch.triangles).sum() },
			detail: Vec::new(),
			examples: invisible.iter().take(5).map(|batch| analysis.items[batch.items[0]].path.clone()).collect(),
		});
	}

	// emitters under one parent with one texture could be one emitter
	let mut emitters: HashMap<(String, String), Vec<&Batch>> = HashMap::new();
	for batch in batches.iter().filter(|batch| batch.kind == Kind::Emitter) {
		let path = &analysis.items[batch.items[0]].path;
		let parent = path.rsplit_once('.').map(|(parent, _)| parent.to_string()).unwrap_or_default();
		emitters.entry((parent, String::new())).or_default().push(batch);
	}
	let crowded: Vec<(&(String, String), &Vec<&Batch>)> = emitters.iter().filter(|(_, group)| group.len() >= 3).collect();
	let emitter_count = batches.iter().filter(|batch| batch.kind == Kind::Emitter).count() as u64;
	if emitter_count > 0 {
		let mut crowded: Vec<_> = crowded.into_iter().map(|((parent, _), group)| (parent.clone(), group.len())).collect();
		crowded.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
		found.push(Opportunity {
			id: "emitters".into(),
			kind: "emitters",
			title: format!("{emitter_count} ParticleEmitters with live particles: a draw each, plus one for all"),
			saves: Count::default(),
			detail: crowded.iter().take(5).map(|(parent, count)| format!("{count} emitters under {parent}")).collect(),
			examples: Vec::new(),
		});
	}

	// characters: each Humanoid model draws apart (FastClusters), its transparent parts one by one,
	// its name over it, two post-process draws once any shows; as parts of a model without a
	// Humanoid (an AnimationController's) they would join the world's batches
	let character_batches: Vec<&Batch> = batches
		.iter()
		.filter(|batch| batch.kind == Kind::Part && matches!(item(batch).cluster, Cluster::Character(..)))
		.collect();
	if !character_batches.is_empty() {
		let names: Vec<&Batch> = batches.iter().filter(|batch| batch.kind == Kind::NameTag).collect();
		let draws: u64 = character_batches.iter().map(|batch| batch.draws).sum::<u64>() + names.len() as u64 + 2;
		let mut as_static: HashSet<(u32, u32, bool)> = HashSet::new();
		let mut models: BTreeSet<u32> = BTreeSet::new();
		for batch in &character_batches {
			for &index in &batch.items {
				let item = &analysis.items[index];
				if let (Some(geometry), Some(appearance), Cluster::Character(id, _)) = (item.geometry, item.appearance, item.cluster) {
					as_static.insert((geometry, appearance, item.transparent));
					models.insert(id);
				}
			}
		}
		let instead = as_static.len() as u64;
		found.push(Opportunity {
			id: "characters".into(),
			kind: "characters",
			title: format!(
				"{} Humanoid models draw apart: {draws} draws (and their shadows); static display models without a Humanoid would take about {instead}",
				models.len()
			),
			saves: Count { draws: draws.saturating_sub(instead), triangles: 0 },
			detail: models.iter().take(6).map(|id| analysis.characters[*id as usize].clone()).collect(),
			examples: Vec::new(),
		});
	}

	// Highlights draw their parts again
	let highlights: Vec<&Batch> = batches.iter().filter(|batch| batch.kind == Kind::Highlight).collect();
	if !highlights.is_empty() {
		let draws: u64 = highlights.iter().map(|batch| batch.draws).sum();
		found.push(Opportunity {
			id: "highlights".into(),
			kind: "highlights",
			title: format!("Highlights draw their parts again: {draws} draws"),
			saves: Count { draws, triangles: highlights.iter().map(|batch| batch.triangles).sum() },
			detail: Vec::new(),
			examples: highlights.iter().take(3).flat_map(|batch| batch.examples.first().cloned()).collect(),
		});
	}

	// a SelectionBox is twelve draws, a BoxHandleAdornment one
	let boxes: Vec<&Batch> = batches.iter().filter(|batch| batch.kind == Kind::Adornment && batch.draws == 12).collect();
	if !boxes.is_empty() {
		found.push(Opportunity {
			id: "selection boxes".into(),
			kind: "selectionBoxes",
			title: format!("{} SelectionBoxes draw 12 times each (an edge a draw)", boxes.len()),
			saves: Count { draws: boxes.len() as u64 * 11, triangles: 0 },
			detail: Vec::new(),
			examples: boxes.iter().take(3).map(|batch| analysis.items[batch.items[0]].path.clone()).collect(),
		});
	}

	// one texture on several faces: decals batch per face
	let mut decal_faces: BTreeMap<String, Vec<&Batch>> = BTreeMap::new();
	for batch in batches.iter().filter(|batch| matches!(batch.kind, Kind::Decal | Kind::Texture) && !batch.sorted) {
		decal_faces.entry(split(&batch.label).0.to_string()).or_default().push(batch);
	}
	for (texture, group) in decal_faces {
		if group.len() < 2 {
			continue;
		}
		found.push(Opportunity {
			id: format!("decal faces {texture}"),
			kind: "decalFaces",
			title: format!("{texture} on {} faces: a draw per face; on one face of each part it would be one", group.len()),
			saves: Count { draws: group.len() as u64 - 1, triangles: 0 },
			detail: group.iter().map(|batch| format!("{} x{}", split(&batch.label).1, batch.instances)).collect(),
			examples: group.iter().take(3).flat_map(|batch| batch.examples.first().cloned()).collect(),
		});
	}

	// one mesh at several levels of detail: a batch per level
	let mut levels: HashMap<&str, (u64, u64)> = HashMap::new();
	for batch in batches.iter().filter(|batch| batch.kind == Kind::Part && !batch.sorted) {
		let entry = levels.entry(batch.label.as_str()).or_default();
		entry.0 += 1;
		entry.1 += batch.draws;
	}
	for (label, (count, draws)) in levels {
		if count > 1 {
			found.push(Opportunity {
				id: format!("levels {label}"),
				kind: "levelsOfDetail",
				title: format!("{label}: at {count} levels of detail in view, a batch each"),
				saves: Count { draws: draws - draws / count, triangles: 0 },
				detail: Vec::new(),
				examples: Vec::new(),
			});
		}
	}

	// the heaviest batches by triangles
	let mut heavy: Vec<&Batch> = batches.iter().filter(|batch| batch.kind == Kind::Part && batch.triangles >= 50_000).collect();
	heavy.sort_by_key(|batch| std::cmp::Reverse(batch.triangles));
	for batch in heavy.into_iter().take(5) {
		let first = item(batch);
		let per = first.triangles[0];
		let lods = first.triangles.len();
		found.push(Opportunity {
			id: format!("triangles {}", batch.label),
			kind: "triangles",
			title: format!(
				"{}: {} triangles ({} x {}{}); halving its triangles saves {}",
				batch.label,
				thousands(batch.triangles),
				batch.instances,
				thousands(per as u64),
				if lods <= 1 { ", no level-of-detail chain" } else { "" },
				thousands(batch.triangles / 2)
			),
			saves: Count { draws: 0, triangles: batch.triangles / 2 },
			detail: Vec::new(),
			examples: batch.examples.clone(),
		});
	}

	found.sort_by(|a, b| b.saves.weight().total_cmp(&a.saves.weight()));
	found
}

/// The opportunities of every view, averaged over them.
pub fn summarize(views: &[ViewCost]) -> Vec<Summary> {
	let mut by_id: BTreeMap<String, Summary> = BTreeMap::new();
	let count = views.len().max(1) as f64;
	for view in views {
		for opportunity in &view.opportunities {
			let entry = by_id.entry(opportunity.id.clone()).or_insert_with(|| Summary {
				id: opportunity.id.clone(),
				kind: opportunity.kind,
				title: opportunity.title.clone(),
				views: 0,
				mean_draws: 0.0,
				mean_triangles: 0.0,
				max: Count::default(),
				detail: opportunity.detail.clone(),
				examples: opportunity.examples.clone(),
			});
			entry.views += 1;
			entry.mean_draws += opportunity.saves.draws as f64 / count;
			entry.mean_triangles += opportunity.saves.triangles as f64 / count;
			if opportunity.saves.weight() > entry.max.weight() {
				entry.max = opportunity.saves;
				entry.title = opportunity.title.clone();
				entry.detail = opportunity.detail.clone();
				entry.examples = opportunity.examples.clone();
			}
		}
	}
	let mut summaries: Vec<Summary> = by_id.into_values().collect();
	let weight = |summary: &Summary| summary.mean_draws + summary.mean_triangles / super::TRIANGLES_PER_DRAW;
	summaries.sort_by(|a, b| weight(b).total_cmp(&weight(a)));
	summaries
}

/// The report as text: the scene, each view's passes, where the draws go, the biggest savings, the
/// heaviest batches by triangles, the costliest batches of the costliest view.
pub fn lines(report: &CostReport) -> Vec<String> {
	let mut lines = Vec::new();
	let scene = &report.scene;
	lines.push(format!(
		"scene: {} parts ({} transparent), {} meshes ({} distinct, {} without LODs), {} unions, {} decals, {} emitters, {} beams ({} showing nothing), {} trails, {} billboards, {} highlights, {} characters; {} batches were all of it in view",
		scene.parts,
		scene.transparent_parts,
		scene.meshes,
		scene.unique_meshes,
		scene.meshes_without_lods,
		scene.unions,
		scene.decals,
		scene.emitters,
		scene.beams,
		scene.invisible_beams,
		scene.trails,
		scene.billboards,
		scene.highlights,
		scene.characters,
		scene.batches
	));
	// each view's line, unless there are too many to read (a grid's): cost.json has them all
	for view in report.views.iter().take(if report.views.len() > 64 { 0 } else { usize::MAX }) {
		let passes: Vec<String> = view
			.passes
			.iter()
			.filter(|(pass, _)| **pass != Pass::Shadows)
			.map(|(pass, count)| format!("{} {}/{}", pass.name(), count.draws, thousands(count.triangles)))
			.collect();
		let shadows = view.passes.get(&Pass::Shadows).map(|count| format!(", character shadows {}/{}", count.draws, thousands(count.triangles))).unwrap_or_default();
		let visible = &view.visible;
		lines.push(format!(
			"{}: {} draws, {} triangles ({}{shadows}) | {} parts ({} transparent), {} meshes, {} decals, {} emitters ({} particles{}), {} beams{}, {} trails, {} billboards | culled {} outside the view, {} hidden",
			view.name,
			view.total.draws,
			thousands(view.total.triangles),
			passes.join(", "),
			visible.parts,
			visible.transparent_parts,
			visible.meshes,
			visible.decals,
			visible.emitters,
			visible.particles,
			if visible.particles > 0 { format!(", covering the view {:.1} times", visible.particle_fill) } else { String::new() },
			visible.beams,
			if visible.invisible_beams > 0 { format!(" ({} showing nothing)", visible.invisible_beams) } else { String::new() },
			visible.trails,
			visible.billboards,
			view.culled.frustum,
			view.culled.occluded
		));
		if report.occlusion {
			let work = &view.occlusion;
			lines.push(format!(
				"  occlusion: tests {} objects ({} hidden), rasterizes {} of {} occluders ({} meshes, {} vertex positions); ~{:.2} ms of culling-job CPU",
				work.tested,
				work.hidden,
				work.occluders,
				work.candidates,
				work.mesh_occluders,
				thousands(work.occluder_positions),
				work.estimate_ms
			));
		}
	}
	let count = report.views.len().max(1) as f64;
	if report.views.len() > 1 {
		let mean = |pick: fn(&ViewCost) -> u64| report.views.iter().map(pick).sum::<u64>() as f64 / count;
		let worst = report.views.iter().max_by_key(|view| view.total.draws).map(|view| format!("{} ({} draws)", view.name, view.total.draws)).unwrap_or_default();
		lines.push(format!("{} views: {:.0} draws and {} triangles on average; most draws: {worst}", report.views.len(), mean(|view| view.total.draws), thousands(mean(|view| view.total.triangles) as u64)));
	}
	// what the effects in view give the GPU to blend, on average over the views
	let fill = report.views.iter().map(|view| view.visible.particle_fill).sum::<f64>() / count;
	if fill > 0.0 {
		let fullest_view = report.views.iter().max_by(|a, b| a.visible.particle_fill.total_cmp(&b.visible.particle_fill)).map(|view| format!("{} ({:.1})", view.name, view.visible.particle_fill)).unwrap_or_default();
		let per = |pick: fn(&ViewCost) -> u64| report.views.iter().map(pick).sum::<u64>() as f64 / count;
		lines.push(format!(
			"effects{}: {:.1} emitters ({:.0} particles) covering the view {:.2} times, {:.1} beams, {:.1} transparent parts; most particle fill: {fullest_view}",
			if report.views.len() > 1 { " per view" } else { "" },
			per(|view| view.visible.emitters),
			per(|view| view.visible.particles),
			fill,
			per(|view| view.visible.beams),
			per(|view| view.visible.transparent_parts)
		));
		let mut by_emitter: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
		for view in &report.views {
			for entry in &view.visible.fullest {
				let total = by_emitter.entry(entry.path.as_str()).or_default();
				total.0 += entry.fill / count;
				total.1 += entry.particles as f64 / count;
			}
		}
		let mut by_emitter: Vec<(&str, (f64, f64))> = by_emitter.into_iter().filter(|(_, (fill, _))| *fill >= 0.01).collect();
		by_emitter.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
		if !by_emitter.is_empty() {
			lines.push(format!("  fullest emitters (views covered{}, particles):", if report.views.len() > 1 { " per view on average" } else { "" }));
			for (path, (fill, particles)) in by_emitter.iter().take(10) {
				lines.push(format!("  {fill:>8.2} {particles:>7.0}  {path}"));
			}
		}
	}
	// where the draws go, on average over the views
	let mut kinds: BTreeMap<&'static str, (f64, f64)> = BTreeMap::new();
	for view in &report.views {
		for (kind, (draws, instances)) in &view.breakdown {
			let entry = kinds.entry(kind).or_default();
			entry.0 += *draws as f64 / count;
			entry.1 += *instances as f64 / count;
		}
	}
	let mut kinds: Vec<(&str, (f64, f64))> = kinds.into_iter().collect();
	kinds.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
	let parts: Vec<String> = kinds
		.iter()
		.filter(|(_, (draws, _))| *draws >= 0.5)
		.map(|(kind, (draws, instances))| if *instances >= 0.5 { format!("{draws:.0} {kind} ({instances:.0} drawn)") } else { format!("{draws:.0} {kind}") })
		.collect();
	if !parts.is_empty() {
		lines.push(format!("where the draws go{}: {}", if report.views.len() > 1 { " per view" } else { "" }, parts.join(", ")));
	}
	if report.occlusion {
		lines.extend(occlusion_lines(report));
	}
	let gains: Vec<&Summary> = report.opportunities.iter().filter(|summary| summary.kind != "triangles" && summary.max.draws > 0).take(15).collect();
	if !gains.is_empty() {
		lines.push(if report.views.len() > 1 { "biggest draw-call gains (saved per view on average, and where it saves most):".into() } else { "biggest draw-call gains:".into() });
		for summary in gains {
			let saves = if report.views.len() > 1 { format!("-{:.1} (-{})", summary.mean_draws, summary.max.draws) } else { format!("-{}", summary.max.draws) };
			let detail = if summary.detail.is_empty() { String::new() } else { format!(" [{}]", summary.detail.join("; ")) };
			lines.push(format!("  {saves:>12}  {}{detail}", summary.title));
		}
	}
	lines.extend(level_lines(report));
	let heavy: Vec<&Summary> = report.opportunities.iter().filter(|summary| summary.kind == "triangles").take(6).collect();
	if !heavy.is_empty() {
		lines.push("heaviest batches by triangles (in the view they weigh most):".into());
		for summary in heavy {
			lines.push(format!("  {}", summary.title));
		}
	}
	if let Some(view) = report.views.iter().max_by_key(|view| view.total.draws) {
		lines.push(format!("costliest batches in {}:", view.name));
		for batch in view.batches.iter().take(10) {
			lines.push(format!(
				"  {:>4} draws {:>7} tris {:>5}x  {}{}",
				batch.draws,
				thousands(batch.triangles),
				batch.instances,
				batch.label,
				if batch.invisible { " (shows nothing)" } else { "" }
			));
		}
	}
	lines.extend(report.warnings.iter().map(|warning| format!("warning: {warning}")));
	lines
}

/// RenderFidelity Performance over every view: the meshes whose level-of-detail chain would save more
/// triangles than the batches its levels split off cost (a draw weighed as TRIANGLES_PER_DRAW).
fn level_lines(report: &CostReport) -> Vec<String> {
	let count = report.views.len().max(1) as f64;
	let mut by_mesh: BTreeMap<&str, (f64, f64, bool)> = BTreeMap::new();
	for gain in report.views.iter().flat_map(|view| &view.level_gains) {
		let entry = by_mesh.entry(gain.mesh.as_str()).or_default();
		entry.0 += gain.triangles as f64 / count;
		entry.1 += gain.batches as f64 / count;
		entry.2 |= gain.estimated;
	}
	if by_mesh.is_empty() {
		return Vec::new();
	}
	let net = |(triangles, batches, _): (f64, f64, bool)| triangles - batches * super::TRIANGLES_PER_DRAW;
	let mut pays: Vec<(&str, (f64, f64, bool))> = by_mesh.iter().map(|(mesh, value)| (*mesh, *value)).filter(|(_, value)| net(*value) > 1000.0).collect();
	pays.sort_by(|a, b| net(b.1).total_cmp(&net(a.1)));
	let costs = by_mesh.values().filter(|value| net(**value) <= 0.0 && value.1 > 0.0).count();
	let mut lines = vec![format!(
		"levels of detail: {} meshes in view are not at RenderFidelity Performance (only it steps down a mesh's whole level-of-detail chain: full detail to 250 studs from the camera to the part's box at a 70-degree field of view, then the next levels past 500, 2,000 and 3,000; Automatic only its high-quality levels); Performance pays on {}{}",
		by_mesh.len(),
		pays.len(),
		if costs > 0 { format!(", and on {costs} would add more batches (copies on both sides of 250 studs draw apart) than its triangles are worth") } else { String::new() }
	)];
	for (mesh, (triangles, batches, estimated)) in pays.iter().take(10) {
		let note = if *estimated { " (no chain: Roblox makes its levels at run time, ror cannot draw them; triangles estimated)" } else { "" };
		lines.push(format!("  {:>8} triangles {:+.1} draws per view  {mesh}{note}", format!("-{}", thousands(*triangles as u64)), batches));
	}
	lines
}

/// The occlusion culling of every view together: what it tests and where those objects are, the
/// occluders that earn their keep and those that hide nothing, and the large parts that cannot
/// occlude but would hide much with a block inside (notes/occlusion.md).
fn occlusion_lines(report: &CostReport) -> Vec<String> {
	let mut lines = Vec::new();
	let views = &report.views;
	if views.is_empty() {
		return lines;
	}
	let count = views.len() as f64;
	let mean = |pick: &dyn Fn(&ViewCost) -> f64| views.iter().map(pick).sum::<f64>() / count;
	let tested = mean(&|view| view.occlusion.tested as f64);
	let hidden = mean(&|view| view.occlusion.hidden as f64);
	let worst = views.iter().max_by(|a, b| a.occlusion.estimate_ms.total_cmp(&b.occlusion.estimate_ms)).map(|view| format!("{} {:.2} ms", view.name, view.occlusion.estimate_ms)).unwrap_or_default();
	lines.push(format!(
		"occlusion culling per view: tests {:.0} objects ({:.0}% hidden), rasterizes {:.0} of {:.0} occluders ({:.0} meshes, {} vertex positions); ~{:.2} ms of culling-job CPU (most: {worst})",
		tested,
		100.0 * hidden / tested.max(1.0),
		mean(&|view| view.occlusion.occluders as f64),
		mean(&|view| view.occlusion.candidates as f64),
		mean(&|view| view.occlusion.mesh_occluders as f64),
		thousands(mean(&|view| view.occlusion.occluder_positions as f64) as u64),
		mean(&|view| view.occlusion.estimate_ms)
	));
	// where the tested objects are
	let mut groups: HashMap<&str, (f64, f64)> = HashMap::new();
	for view in views {
		for group in &view.occlusion.groups {
			let entry = groups.entry(group.group.as_str()).or_default();
			entry.0 += group.tested as f64 / count;
			entry.1 += group.hidden as f64 / count;
		}
	}
	let mut groups: Vec<(&str, (f64, f64))> = groups.into_iter().collect();
	groups.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(b.0)));
	if !groups.is_empty() {
		lines.push("  most tested (objects per view, share hidden): each costs a test every frame however it is batched; merging or removing small pieces cuts it".into());
		for (group, (tested, hidden)) in groups.iter().take(8) {
			lines.push(format!("    {tested:>7.0}  ({:>3.0}% hidden)  {group}", 100.0 * hidden / tested.max(1e-9)));
		}
	}
	// the occluders: what each hides over the views
	struct Use {
		uses: usize,
		views: HashSet<usize>,
		hidden: u64,
		hidden_triangles: u64,
		positions: u32,
		triangles: u32,
		mesh: bool,
	}
	// by path and mesh: meshes named alike ("Model.default") stay apart; named once each (a grid's
	// 440 views made a key string for each of their ~415,000 uses). A path never ends in " (mesh N)"
	// (a name with spaces is quoted), so no two keys share a name
	let mut by_key: HashMap<(&str, Option<&str>), Use> = HashMap::new();
	for (index, view) in views.iter().enumerate() {
		for used in &view.occlusion.used {
			let entry = by_key.entry((used.path.as_str(), used.mesh_id.as_deref())).or_insert(Use { uses: 0, views: HashSet::new(), hidden: 0, hidden_triangles: 0, positions: used.positions, triangles: used.triangles, mesh: used.mesh });
			entry.uses += 1;
			entry.views.insert(index);
			entry.hidden += used.hidden;
			entry.hidden_triangles += used.hidden_triangles;
		}
	}
	let uses: HashMap<String, Use> = by_key
		.into_iter()
		.map(|((path, mesh), used)| {
			let key = match mesh {
				Some(id) => format!("{path} (mesh {id})"),
				None => path.to_string(),
			};
			(key, used)
		})
		.collect();
	let mut earning: Vec<(&str, &Use)> = uses.iter().filter(|(_, used)| used.hidden > 0).map(|(path, used)| (path.as_str(), used)).collect();
	earning.sort_by(|a, b| b.1.hidden.cmp(&a.1.hidden).then(a.0.cmp(b.0)));
	if !earning.is_empty() {
		lines.push("  occluders that hide most (objects and triangles hidden over the views; parts of one path together):".into());
		for (path, used) in earning.iter().take(6) {
			let kind = if used.mesh { format!("{} positions", used.positions) } else { "part".into() };
			lines.push(format!("    {:>7}  {:>6} triangles  {path} ({kind}; {} uses in {} views)", used.hidden, thousands(used.hidden_triangles), used.uses, used.views.len()));
		}
	}
	// mesh occluders by what rasterizing them costs (b8: 20-30 ns a triangle on a desktop core, every
	// frame they are in view), against what they hide
	let mut costly: Vec<(&str, &Use)> = uses.iter().filter(|(_, used)| used.mesh).map(|(path, used)| (path.as_str(), used)).collect();
	costly.sort_by(|a, b| (b.1.triangles as usize * b.1.uses).cmp(&(a.1.triangles as usize * a.1.uses)).then(a.0.cmp(b.0)));
	if !costly.is_empty() {
		let total: f64 = costly.iter().map(|(_, used)| used.triangles as f64 * used.uses as f64).sum::<f64>() / count;
		lines.push(format!(
			"  costliest mesh occluders (rasterized every frame they are in view, ~25 ns a triangle on a desktop, a block next to nothing; {} triangles per view in all): fewer triangles, or a block for a flat face, cut it (decimation cuts its GPU cost too); those that hide little first",
			thousands(total as u64)
		));
		for (path, used) in costly.iter().take(6) {
			lines.push(format!("    {:>7} triangles x {} uses  hides {:>5}  {path} ({} positions, {} views)", thousands(used.triangles as u64), used.uses, used.hidden, used.positions, used.views.len()));
		}
	}
	let idle = uses.iter().filter(|(_, used)| used.hidden == 0 && used.mesh).count();
	if idle > 0 {
		let wasted: u64 = uses.iter().filter(|(_, used)| used.hidden == 0 && used.mesh).map(|(_, used)| used.triangles as u64 * used.uses as u64).sum();
		lines.push(format!("  {idle} mesh occluders hide nothing in any view: {} triangles rasterized per view for nothing (cost.json lists them under each view's occlusion.used)", thousands((wasted as f64 / count) as u64)));
	}
	// the large parts that cannot occlude
	struct Missed {
		views: usize,
		hides: u64,
		most: u64,
		reason: String,
	}
	let mut missed: HashMap<&str, Missed> = HashMap::new();
	for view in views {
		for entry in &view.occlusion.missed {
			let item = missed.entry(entry.path.as_str()).or_insert(Missed { views: 0, hides: 0, most: 0, reason: entry.reason.clone() });
			item.views += 1;
			item.hides += entry.would_hide;
			if entry.would_hide > item.most {
				item.most = entry.would_hide;
				item.reason = entry.reason.clone();
			}
		}
	}
	let mut missed: Vec<(&str, Missed)> = missed.into_iter().collect();
	missed.sort_by(|a, b| b.1.hides.cmp(&a.1.hides).then(a.0.cmp(b.0)));
	if !missed.is_empty() {
		lines.push("  missed occluders: large parts that cannot occlude; a block filling each would hide, over the views (an upper bound: a block must fit inside the solid part of it, and a hollow or see-through mesh, glass fronts and frames, cannot take one; check a capture before adding it; or bring the mesh under the vertex limit):".into());
		for (path, item) in missed.iter().take(8) {
			lines.push(format!("    {:>7}  {path}: {} (in {} views, up to {} in one)", item.hides, item.reason, item.views, item.most));
		}
	}
	lines
}
