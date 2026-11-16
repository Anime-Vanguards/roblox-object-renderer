//! Ranked opportunities with the edit that would realise each: what it is and where (instance ids),
//! why it costs (its work as attributed), what it would save per view and on average in phone ms, and
//! an edit for `ror cost --edits` to try and `--export` to apply. Savings are estimated from the
//! attribution (a part's share of its batch's draw, its triangles, pixels and its part price) or from
//! the batches (draws that would join); `ror cost --edits` with the finding's edit measures them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rbx_dom_weak::types::Ref;
use serde::Serialize;
use serde_json::json;

use super::attribute::{Owners, ViewWork, Work};
use super::edits::Edit;
use super::ids::InstanceIds;
use super::prices::{self, Quality};
use super::{Analysis, Cluster, Kind, ViewCost};
use crate::scene::load::Document;
use crate::scene::{Scene, props};

/// Targets listed in a finding's text (its edit lists every one).
const LISTED: usize = 12;
/// Findings kept.
pub const KEPT: usize = 60;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Saving {
	/// Phone frame ms saved per view, averaged over every view (0 where it does not apply).
	pub mean_ms: f64,
	pub worst_ms: f64,
	pub worst_view: String,
	pub mean_draws: f64,
	pub mean_triangles: f64,
	pub mean_blended: f64,
	/// Phone frame ms per view, the views it applies to.
	pub views: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
	/// Stable across runs of one place: its kind and what it is about.
	pub id: String,
	pub rank: usize,
	pub kind: &'static str,
	pub title: String,
	/// The first few instances it is about; `edit` names them all.
	pub targets: Vec<String>,
	pub target_count: usize,
	/// What the targets cost now, per view on average.
	pub why: Work,
	pub saving: Saving,
	pub edit: Edit,
	/// What the edit does to the picture: "none" (the same pixels), "unseen" (nothing these cameras
	/// show), or "changes" (check it: `ror cost --edits ... --look`).
	pub look: &'static str,
	/// How the saving was estimated: "attribution" (the targets' share of the work) or "batches" (draw
	/// calls that would join).
	pub basis: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub note: Option<String>,
}

pub struct Inputs<'a> {
	pub scene: &'a Scene,
	pub document: &'a Document,
	pub analysis: &'a Analysis,
	pub owners: &'a Owners,
	pub ids: &'a InstanceIds,
	pub costs: &'a [ViewCost],
	pub works: &'a [ViewWork],
	pub quality: &'a Quality,
	/// Pixels were counted in every view (a layers render each).
	pub pixels: bool,
	/// The scene parts any view shows at the highest quality (agent::Costed::shown).
	pub shown: Option<&'a HashSet<usize>>,
}

impl Inputs<'_> {
	fn id(&self, referent: Ref) -> String {
		self.ids.id(referent).unwrap_or(super::attribute::SHARED).to_string()
	}

	/// The saving of taking away what `instances` cost (their attributed work) in each view.
	fn saving_of(&self, instances: &HashSet<Ref>, keep_parts: bool) -> (Saving, Work) {
		// summed in id order: the same run gives the same digits
		let mut instances: Vec<Ref> = instances.iter().copied().collect();
		instances.sort_by_key(|referent| self.id(*referent));
		let mut saving = Saving::default();
		let mut why = Work::default();
		for work in self.works {
			let mut view = Work::default();
			for referent in &instances {
				if let Some(entry) = work.by_instance.get(referent) {
					view.add(entry);
				}
			}
			why.add(&view);
			let mut ms = view.ms;
			if keep_parts {
				ms -= view.parts * prices::PART_MS;
			}
			if ms > 1e-6 {
				saving.views.insert(work.name.clone(), round_ms(ms));
				if ms > saving.worst_ms {
					saving.worst_ms = round_ms(ms);
					saving.worst_view = work.name.clone();
				}
			}
			saving.mean_ms += ms;
			saving.mean_draws += view.draws;
			saving.mean_triangles += view.triangles;
			saving.mean_blended += view.blended;
		}
		let count = self.works.len().max(1) as f64;
		saving.mean_ms = round_ms(saving.mean_ms / count);
		saving.mean_draws = (saving.mean_draws / count * 100.0).round() / 100.0;
		saving.mean_triangles = (saving.mean_triangles / count).round();
		saving.mean_blended = (saving.mean_blended / count).round();
		(saving, why.scaled(1.0 / count).rounded())
	}

	/// A saving of draw calls per view.
	fn draws_saved(&self, per_view: &[f64]) -> Saving {
		let mut saving = Saving::default();
		for (work, &draws) in self.works.iter().zip(per_view) {
			let ms = draws * prices::DRAW_MS;
			if draws > 0.0 {
				saving.views.insert(work.name.clone(), round_ms(ms));
				if ms > saving.worst_ms {
					saving.worst_ms = round_ms(ms);
					saving.worst_view = work.name.clone();
				}
			}
			saving.mean_ms += ms;
			saving.mean_draws += draws;
		}
		let count = self.works.len().max(1) as f64;
		saving.mean_ms = round_ms(saving.mean_ms / count);
		saving.mean_draws = (saving.mean_draws / count * 100.0).round() / 100.0;
		saving
	}
}

fn round_ms(value: f64) -> f64 {
	(value * 1e4).round() / 1e4
}

fn listed(targets: &[String]) -> Vec<String> {
	targets.iter().take(LISTED).cloned().collect()
}

/// Every finding, ranked by mean saving (ties by id), the first [`KEPT`].
pub fn find(inputs: &Inputs) -> Vec<Finding> {
	let mut found = Vec::new();
	if inputs.pixels {
		found.extend(unseen(inputs));
		found.extend(see_through(inputs));
		found.extend(lights(inputs));
		found.extend(emitters(inputs));
	}
	found.extend(appearances(inputs));
	found.extend(duplicate_meshes(inputs));
	found.extend(levels_of_detail(inputs));
	found.extend(invisible_effects(inputs));
	found.extend(merges(inputs));
	found.extend(shadows(inputs));
	found.retain(|finding| finding.saving.mean_ms > 0.0 || finding.kind == "lightReachesNothing");
	found.sort_by(|a, b| b.saving.mean_ms.total_cmp(&a.saving.mean_ms).then_with(|| a.id.cmp(&b.id)));
	found.truncate(KEPT);
	for (rank, finding) in found.iter_mut().enumerate() {
		finding.rank = rank + 1;
	}
	found
}

/// Parts no view shows: no opaque pixel in front, no blended fragment, in any view, at the highest
/// quality (an edit reaches every device: a part a phone culls past its view distance still shows on a
/// desktop). Grouped by the highest instance whose every part is unseen, hidden (Transparency 1, no
/// shadow: they keep their collision, scripts and effects).
fn unseen(inputs: &Inputs) -> Vec<Finding> {
	let dom = &inputs.document.dom;
	// per part instance: seen in some view
	let mut seen: HashMap<Ref, bool> = HashMap::new();
	for index in 0..inputs.scene.parts.len() {
		let referent = inputs.owners.part(index);
		if referent.is_none() {
			continue;
		}
		let shown = inputs.shown.is_some_and(|shown| shown.contains(&index)) || inputs.works.iter().any(|work| work.by_instance.get(&referent).is_some_and(|entry| entry.pixels > 0.0 || entry.blended > 0.0));
		*seen.entry(referent).or_insert(false) |= shown;
	}
	// an ancestor is all-unseen when every drawn part under it is
	let mut state: HashMap<Ref, (u32, u32)> = HashMap::new();
	for (&referent, &shown) in &seen {
		let mut current = referent;
		while !current.is_none() {
			let entry = state.entry(current).or_default();
			entry.0 += 1;
			if shown {
				entry.1 += 1;
			}
			current = inputs.ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
		}
	}
	let all_unseen = |referent: Ref| state.get(&referent).is_some_and(|(_, shown)| *shown == 0);
	// the highest all-unseen instance above each unseen part (not a service, the Workspace itself)
	let mut roots: BTreeMap<String, Ref> = BTreeMap::new();
	for (&referent, &shown) in &seen {
		// an unseen part with a part some view shows under it (a hidden base holding a visible prop) is
		// left: hiding it would hide that part too
		if shown || !all_unseen(referent) {
			continue;
		}
		let mut root = referent;
		let mut current = referent;
		loop {
			let parent = inputs.ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
			let parent_is_service = parent.is_none() || inputs.ids.info(parent).is_some_and(|info| info.0.is_none());
			if parent_is_service || !all_unseen(parent) {
				break;
			}
			root = parent;
			current = parent;
		}
		roots.insert(inputs.id(root), root);
	}
	// one finding per root holding 20 parts or more, the rest by their top-level model
	let mut groups: BTreeMap<String, Vec<Ref>> = BTreeMap::new();
	for (id, referent) in roots {
		let parts = state.get(&referent).map_or(0, |(count, _)| *count);
		let key = if parts >= 20 {
			id.clone()
		} else {
			// the root's ancestor three below the service (Workspace.Map.Assets.Island), as a bucket
			let mut chain = vec![referent];
			let mut current = referent;
			while let Some(parent) = inputs.ids.info(current).map(|info| info.0).filter(|parent| !parent.is_none()) {
				chain.push(parent);
				current = parent;
			}
			chain.reverse();
			chain.get(3).or(chain.last()).map(|referent| inputs.id(*referent)).unwrap_or(id.clone())
		};
		groups.entry(key).or_default().push(referent);
	}
	let mut found = Vec::new();
	for (key, roots) in groups {
		let mut parts: HashSet<Ref> = HashSet::new();
		let _ = dom;
		for &root in &roots {
			for (&referent, &shown) in &seen {
				if shown {
					continue;
				}
				let mut current = referent;
				while !current.is_none() {
					if current == root {
						parts.insert(referent);
						break;
					}
					current = inputs.ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
				}
			}
		}
		let (saving, why) = inputs.saving_of(&parts, false);
		let mut targets: Vec<String> = roots.iter().map(|referent| inputs.id(*referent)).collect();
		targets.sort();
		found.push(Finding {
			id: format!("unseen {key}"),
			rank: 0,
			kind: "unseen",
			title: format!("{} parts under {key} show in none of the {} views: hide them (or remove them where nothing needs them)", parts.len(), inputs.works.len()),
			targets: listed(&targets),
			target_count: targets.len(),
			why,
			saving,
			edit: Edit::Hide { targets, classes: Vec::new() },
			look: "unseen",
			basis: "attribution",
			note: Some("unseen from these cameras at the highest quality; hiding a part also takes it out of occlusion culling, which can add draws behind it (try it: --edits); check against the cameras players use before removing".into()),
		});
	}
	found
}

/// See-through parts (Transparency above 0, or a SurfaceAppearance with alpha) by the blended fill
/// they cost: grouped by mesh or shape, hidden.
fn see_through(inputs: &Inputs) -> Vec<Finding> {
	let mut groups: BTreeMap<String, HashSet<Ref>> = BTreeMap::new();
	for (index, part) in inputs.scene.parts.iter().enumerate() {
		let alpha = part.surface_appearance.as_ref().is_some_and(|appearance| appearance.alpha_mode == 1);
		if part.transparency <= 0.0 && !alpha {
			continue;
		}
		let referent = inputs.owners.part(index);
		if referent.is_none() || inputs.document.dom.get_by_ref(referent).is_none() {
			continue;
		}
		let blended: f64 = inputs.works.iter().filter_map(|work| work.by_instance.get(&referent)).map(|entry| entry.blended).sum();
		if blended <= 0.0 {
			continue;
		}
		let key = match &part.geometry {
			crate::scene::Geometry::MeshPart { mesh, .. } => format!("mesh {}", super::meshes::canonical(mesh)),
			_ => format!("{} {}", part.class, inputs.id(referent)),
		};
		groups.entry(key).or_default().insert(referent);
	}
	let mut found = Vec::new();
	for (key, parts) in groups {
		let (saving, why) = inputs.saving_of(&parts, false);
		let mut targets: Vec<String> = parts.iter().map(|referent| inputs.id(*referent)).collect();
		targets.sort();
		let viewports = why.blended;
		// hide covers what is under a target too: a see-through part holding other parts is made
		// invisible on its own
		let holds_parts = parts.iter().any(|referent| inputs.document.dom.descendants_of(*referent).any(|instance| instance.referent() != *referent && props::class_is_a(instance.class.as_str(), "BasePart")));
		let edit = if holds_parts {
			Edit::Set { targets: targets.clone(), classes: Vec::new(), properties: [("Transparency".to_string(), json!(1.0)), ("CastShadow".to_string(), json!(false))].into_iter().collect() }
		} else {
			Edit::Hide { targets: targets.clone(), classes: Vec::new() }
		};
		found.push(Finding {
			id: format!("seeThrough {key}"),
			rank: 0,
			kind: "seeThrough",
			title: format!("{} see-through parts ({key}) shade {:.0} blended pixels per view: hide them, shrink them, or make them opaque", targets.len(), viewports),
			targets: listed(&targets),
			target_count: targets.len(),
			why,
			saving,
			edit,
			look: "changes",
			basis: "attribution",
			note: Some("blended layers are GPU work: on phones the frame pays it through the GPU waits (estimated weights)".into()),
		});
	}
	found
}

/// Emitters by the blended pixels their particles shade: half the Rate, about half the particles alive
/// and half the fill (their draws stay).
fn emitters(inputs: &Inputs) -> Vec<Finding> {
	let mut rates: HashMap<Ref, f32> = HashMap::new();
	for (index, item) in inputs.analysis.items.iter().enumerate() {
		if item.kind != Kind::Emitter {
			continue;
		}
		let Some(emitter) = inputs.analysis.emitters.get(&index) else { continue };
		let referent = inputs.owners.item(inputs.scene, inputs.analysis, index);
		if !referent.is_none() && emitter.emitter.rate > 0.0 {
			rates.insert(referent, emitter.emitter.rate);
		}
	}
	let mut found = Vec::new();
	let mut ordered: Vec<(String, Ref, f32)> = rates.into_iter().map(|(referent, rate)| (inputs.id(referent), referent, rate)).collect();
	ordered.sort_by(|a, b| a.0.cmp(&b.0));
	for (id, referent, rate) in ordered {
		let one: HashSet<Ref> = [referent].into_iter().collect();
		let (mut saving, why) = inputs.saving_of(&one, false);
		if why.blended <= 0.0 {
			continue;
		}
		// the quality's rate cap (a phone's 100 a second): half of what it makes, not of its Rate
		let made = rate.min(inputs.quality.max_rate);
		let rate = ((made as f64) * 0.5 * 100.0).round() / 100.0;
		// half the fill's GPU work, through the waits; the draw stays
		let half = |ms: f64, work: &Work| ((ms - work.draws * prices::DRAW_MS - work.triangles / 1000.0 * prices::KILO_TRIANGLE_MS) * 0.5).max(0.0);
		let mut per_view = BTreeMap::new();
		let mut total = 0.0;
		let mut worst = (0.0, String::new());
		for work in inputs.works {
			if let Some(entry) = work.by_instance.get(&referent) {
				let ms = round_ms(half(entry.ms, entry));
				if ms > 0.0 {
					per_view.insert(work.name.clone(), ms);
					if ms > worst.0 {
						worst = (ms, work.name.clone());
					}
				}
				total += ms;
			}
		}
		let count = inputs.works.len().max(1) as f64;
		saving.views = per_view;
		saving.mean_ms = round_ms(total / count);
		saving.worst_ms = worst.0;
		saving.worst_view = worst.1;
		saving.mean_draws = 0.0;
		saving.mean_triangles = (saving.mean_triangles * 0.5).round();
		saving.mean_blended = (saving.mean_blended * 0.5).round();
		found.push(Finding {
			id: format!("emitter {id}"),
			rank: 0,
			kind: "emitterFill",
			title: format!("{id} (making {made} particles a second here) shades {:.0} blended pixels per view: Rate {rate} halves its fill", why.blended),
			targets: vec![id.clone()],
			target_count: 1,
			why,
			saving,
			edit: Edit::Set { targets: vec![id], classes: Vec::new(), properties: [("Rate".to_string(), json!(rate))].into_iter().collect() },
			look: "changes",
			basis: "attribution",
			note: Some("particle fill is GPU work, priced through the waits (estimated weights)".into()),
		});
	}
	found
}

/// Local lights that reach no pixel any view shows, and lights whose evaluations cost most.
fn lights(inputs: &Inputs) -> Vec<Finding> {
	let mut dark: Vec<String> = Vec::new();
	let mut found = Vec::new();
	for (index, _) in inputs.scene.lights.iter().enumerate() {
		let referent = inputs.owners.light(index);
		if referent.is_none() {
			continue;
		}
		let reached: f64 = inputs.works.iter().filter_map(|work| work.by_instance.get(&referent)).map(|entry| entry.light_pixels).sum();
		if reached <= 0.0 {
			dark.push(inputs.id(referent));
		} else if inputs.quality.per_pixel_lights {
			let one: HashSet<Ref> = [referent].into_iter().collect();
			let (saving, why) = inputs.saving_of(&one, false);
			let id = inputs.id(referent);
			found.push(Finding {
				id: format!("light {id}"),
				rank: 0,
				kind: "lightRange",
				title: format!("{id} lights {:.0} pixels per view: a smaller Range, or no light here", why.light_pixels),
				targets: vec![id.clone()],
				target_count: 1,
				why,
				saving,
				edit: Edit::Set { targets: vec![id], classes: Vec::new(), properties: [("Enabled".to_string(), json!(false))].into_iter().collect() },
				look: "changes",
				basis: "attribution",
				note: None,
			});
		}
	}
	dark.sort();
	dark.dedup();
	if !dark.is_empty() {
		found.push(Finding {
			id: "lights reach nothing".into(),
			rank: 0,
			kind: "lightReachesNothing",
			title: format!("{} local lights reach no pixel any of the {} views shows: disable them", dark.len(), inputs.works.len()),
			targets: listed(&dark),
			target_count: dark.len(),
			why: Work::default(),
			saving: Saving::default(),
			edit: Edit::Set { targets: dark, classes: Vec::new(), properties: [("Enabled".to_string(), json!(false))].into_iter().collect() },
			look: "unseen",
			basis: "attribution",
			note: Some("no GPU work in these views; what a light costs the light grid's CPU is not priced (unmeasured)".into()),
		});
	}
	found
}

/// The static, opaque part items of each batch label in the scene, by geometry.
fn by_geometry(analysis: &Analysis) -> BTreeMap<u32, BTreeMap<u32, Vec<usize>>> {
	let mut groups: BTreeMap<u32, BTreeMap<u32, Vec<usize>>> = BTreeMap::new();
	for (index, item) in analysis.items.iter().enumerate() {
		if item.kind != Kind::Part || item.cluster != Cluster::Static || item.transparent || item.sorted {
			continue;
		}
		if let (Some(geometry), Some(appearance)) = (item.geometry, item.appearance) {
			groups.entry(geometry).or_default().entry(appearance).or_default().push(index);
		}
	}
	groups
}

/// One geometry's copies in several appearances: each a batch of its own. The copies outside the most
/// common appearance take it.
fn appearances(inputs: &Inputs) -> Vec<Finding> {
	let analysis = inputs.analysis;
	let mut found = Vec::new();
	for (geometry, by_appearance) in by_geometry(analysis) {
		if by_appearance.len() < 2 {
			continue;
		}
		let (&main, main_items) = by_appearance.iter().max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(a.0))).expect("appearances");
		// per view: the geometry's appearances drawn there, minus one
		let labels: BTreeSet<String> = by_appearance.values().flatten().filter_map(|&index| analysis.items[index].label.map(|label| analysis.labels[label as usize].clone())).collect();
		let per_view: Vec<f64> = inputs
			.costs
			.iter()
			.map(|cost| {
				let drawn: BTreeSet<&str> = cost.batches.iter().filter(|batch| labels.contains(&batch.label)).map(|batch| batch.label.as_str()).collect();
				(drawn.len() as f64 - 1.0).max(0.0)
			})
			.collect();
		let saving = inputs.draws_saved(&per_view);
		if saving.mean_ms <= 0.0 {
			continue;
		}
		let Some(&source_part) = analysis.items[main_items[0]].part.as_ref() else { continue };
		let from = inputs.id(inputs.owners.part(source_part));
		let mut targets: Vec<String> = by_appearance.iter().filter(|(appearance, _)| **appearance != main).flat_map(|(_, items)| items.iter().filter_map(|&index| analysis.items[index].part).map(|part| inputs.id(inputs.owners.part(part)))).collect();
		targets.sort();
		targets.dedup();
		let name = &analysis.labels[geometry as usize];
		found.push(Finding {
			id: format!("appearance {name}"),
			rank: 0,
			kind: "appearance",
			title: format!("{name}: copies in {} appearances draw a batch each; {} copies taking {}'s ({}) join one batch", by_appearance.len(), targets.len(), from, analysis.labels[main as usize]),
			targets: listed(&targets),
			target_count: targets.len(),
			why: Work::default(),
			saving,
			edit: Edit::Appearance { targets, classes: Vec::new(), from },
			look: "changes",
			basis: "batches",
			note: None,
		});
	}
	found
}

/// One mesh uploaded under several ids: Roblox batches by id, so each upload is a batch. The copies
/// of the others take the most used upload's id.
fn duplicate_meshes(inputs: &Inputs) -> Vec<Finding> {
	let analysis = inputs.analysis;
	let mut by_shape: BTreeMap<u64, BTreeMap<u32, Vec<usize>>> = BTreeMap::new();
	for (geometry, by_appearance) in by_geometry(analysis) {
		let Some((key, false)) = analysis.geometry_meshes.get(&geometry) else { continue };
		let Some(stats) = analysis.meshes.stats.get(key) else { continue };
		by_shape.entry(stats.shape).or_default().entry(geometry).or_default().extend(by_appearance.into_values().flatten());
	}
	let mut found = Vec::new();
	for (shape, geometries) in by_shape {
		if geometries.len() < 2 {
			continue;
		}
		let (&main, _) = geometries.iter().max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(a.0))).expect("geometries");
		let Some((uri, _)) = analysis.geometry_meshes.get(&main) else { continue };
		// per view: per appearance, the uploads drawn in it, minus one
		let per_view: Vec<f64> = inputs
			.costs
			.iter()
			.map(|cost| {
				let mut by_appearance: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
				for batch in &cost.batches {
					let Some(&first) = batch.items.first() else { continue };
					let item = &analysis.items[first];
					if let (Some(geometry), Some(appearance)) = (item.geometry, item.appearance)
						&& geometries.contains_key(&geometry)
						&& item.kind == Kind::Part
						&& !item.sorted
					{
						by_appearance.entry(appearance).or_default().insert(geometry);
					}
				}
				by_appearance.values().map(|uploads| uploads.len() as f64 - 1.0).sum()
			})
			.collect();
		let saving = inputs.draws_saved(&per_view);
		if saving.mean_ms <= 0.0 {
			continue;
		}
		let mut targets: Vec<String> = geometries.iter().filter(|(geometry, _)| **geometry != main).flat_map(|(_, items)| items.iter().filter_map(|&index| analysis.items[index].part).map(|part| inputs.id(inputs.owners.part(part)))).collect();
		targets.sort();
		targets.dedup();
		found.push(Finding {
			id: format!("uploads {shape:016x}"),
			rank: 0,
			kind: "duplicateMesh",
			title: format!("one mesh uploaded as {} assets: {} copies taking {uri} join its batches", geometries.len(), targets.len()),
			targets: listed(&targets),
			target_count: targets.len(),
			why: Work::default(),
			saving,
			edit: Edit::Set { targets, classes: Vec::new(), properties: [("MeshId".to_string(), json!(uri))].into_iter().collect() },
			look: "none",
			basis: "batches",
			note: Some("the uploads hold the same positions and triangles; check UVs and normals match before swapping".into()),
		});
	}
	found
}

/// Meshes where RenderFidelity Performance would save more triangles than the batches it adds cost
/// (ror cost's levels of detail).
fn levels_of_detail(inputs: &Inputs) -> Vec<Finding> {
	let mut by_mesh: BTreeMap<String, Vec<(usize, f64, f64)>> = BTreeMap::new();
	for (view, cost) in inputs.costs.iter().enumerate() {
		for gain in &cost.level_gains {
			by_mesh.entry(gain.mesh.clone()).or_default().push((view, gain.triangles as f64, gain.batches as f64));
		}
	}
	let mut found = Vec::new();
	for (mesh, gains) in by_mesh {
		let mut saving = Saving::default();
		for (view, triangles, batches) in &gains {
			let ms = triangles / 1000.0 * prices::KILO_TRIANGLE_MS - batches * prices::DRAW_MS;
			let name = &inputs.works[*view].name;
			saving.views.insert(name.clone(), round_ms(ms));
			if ms > saving.worst_ms {
				saving.worst_ms = round_ms(ms);
				saving.worst_view = name.clone();
			}
			saving.mean_ms += ms;
			saving.mean_triangles += triangles;
			saving.mean_draws -= batches;
		}
		let count = inputs.works.len().max(1) as f64;
		saving.mean_ms = round_ms(saving.mean_ms / count);
		saving.mean_triangles = (saving.mean_triangles / count).round();
		saving.mean_draws = (saving.mean_draws / count * 100.0).round() / 100.0;
		let id = mesh.trim_start_matches("MeshPart ").to_string();
		found.push(Finding {
			id: format!("levels {id}"),
			rank: 0,
			kind: "levelOfDetail",
			title: format!("{mesh}: RenderFidelity Performance draws its lower levels from 250 studs ({:.0} triangles fewer per view, {:.1} draws more)", saving.mean_triangles, -saving.mean_draws),
			targets: vec![format!("mesh:{id}")],
			target_count: 1,
			why: Work::default(),
			saving,
			edit: Edit::Set { targets: vec![format!("mesh:{id}")], classes: vec!["MeshPart".into()], properties: [("RenderFidelity".to_string(), json!("Performance"))].into_iter().collect() },
			look: "changes",
			basis: "batches",
			note: Some("a mesh without a level-of-detail chain gets levels Roblox makes at run time, which ror cannot draw: check those in Studio".into()),
		});
	}
	found
}

/// Beams and emitters that show nothing (Transparency 1, width 0) and still draw.
fn invisible_effects(inputs: &Inputs) -> Vec<Finding> {
	let mut referents: HashSet<Ref> = HashSet::new();
	let mut targets = Vec::new();
	for cost in inputs.costs {
		for batch in cost.batches.iter().filter(|batch| batch.invisible) {
			for &index in &batch.items {
				let referent = inputs.owners.item(inputs.scene, inputs.analysis, index);
				if !referent.is_none() && referents.insert(referent) {
					targets.push(inputs.id(referent));
				}
			}
		}
	}
	if targets.is_empty() {
		return Vec::new();
	}
	targets.sort();
	let (saving, why) = inputs.saving_of(&referents, false);
	vec![Finding {
		id: "invisible effects".into(),
		rank: 0,
		kind: "invisibleEffect",
		title: format!("{} beams and emitters show nothing (Transparency 1 or width 0) and still draw: disable them", targets.len()),
		targets: listed(&targets),
		target_count: targets.len(),
		why,
		saving,
		edit: Edit::Set { targets, classes: Vec::new(), properties: [("Enabled".to_string(), json!(false))].into_iter().collect() },
		look: "none",
		basis: "attribution",
		note: None,
	}]
}

/// Copies of several meshes in one appearance under one model, each mesh's every copy there: one mesh
/// of them all would be one draw instead of one per mesh, with nothing else changing.
fn merges(inputs: &Inputs) -> Vec<Finding> {
	let analysis = inputs.analysis;
	// per batch label: its items' parents
	let mut label_parents: HashMap<u32, HashSet<Ref>> = HashMap::new();
	let mut parent_groups: BTreeMap<(String, u32), Vec<usize>> = BTreeMap::new();
	for (index, item) in analysis.items.iter().enumerate() {
		if item.kind != Kind::Part || item.cluster != Cluster::Static || item.transparent || item.sorted || item.lod.is_some() {
			continue;
		}
		let (Some(label), Some(appearance), Some(part)) = (item.label, item.appearance, item.part) else { continue };
		let referent = inputs.owners.part(part);
		let parent = inputs.ids.info(referent).map(|info| info.0).unwrap_or_else(Ref::none);
		if parent.is_none() {
			continue;
		}
		label_parents.entry(label).or_default().insert(parent);
		parent_groups.entry((inputs.id(parent), appearance)).or_default().push(index);
	}
	let mut found = Vec::new();
	for ((parent, appearance), items) in parent_groups {
		// meshes whose every copy is here
		let labels: BTreeSet<u32> = items.iter().filter_map(|&index| analysis.items[index].label).filter(|label| label_parents.get(label).is_some_and(|parents| parents.len() == 1)).collect();
		if labels.len() < 3 {
			continue;
		}
		let names: BTreeSet<String> = labels.iter().map(|label| analysis.labels[*label as usize].clone()).collect();
		let per_view: Vec<f64> = inputs
			.costs
			.iter()
			.map(|cost| {
				let drawn: BTreeSet<&str> = cost.batches.iter().filter(|batch| names.contains(&batch.label)).map(|batch| batch.label.as_str()).collect();
				(drawn.len() as f64 - 1.0).max(0.0)
			})
			.collect();
		let saving = inputs.draws_saved(&per_view);
		if saving.mean_ms <= 0.0 {
			continue;
		}
		let mut targets: Vec<String> = items.iter().filter(|&&index| analysis.items[index].label.is_some_and(|label| labels.contains(&label))).filter_map(|&index| analysis.items[index].part).map(|part| inputs.id(inputs.owners.part(part))).collect();
		targets.sort();
		targets.dedup();
		found.push(Finding {
			id: format!("merge {parent} {}", analysis.labels[appearance as usize]),
			rank: 0,
			kind: "merge",
			title: format!("{} parts of {} meshes under {parent} share one appearance ({}) and no copy elsewhere: one mesh of them draws once", targets.len(), labels.len(), analysis.labels[appearance as usize]),
			targets: listed(&targets),
			target_count: targets.len(),
			why: Work::default(),
			saving,
			edit: Edit::Merge { name: format!("merge {parent}"), targets },
			look: "none",
			basis: "batches",
			note: Some("a merged mesh needs building and uploading (the export lists it); the triangles stay".into()),
		});
	}
	found
}

/// Where the quality draws sun shadows: casters too small to cast a shadow anyone sees (under a stud
/// across) still draw into the shadow maps.
fn shadows(inputs: &Inputs) -> Vec<Finding> {
	if !inputs.works.iter().any(|work| work.by_instance.values().any(|entry| entry.shadow_triangles > 0.0)) {
		return Vec::new();
	}
	let mut small: HashSet<Ref> = HashSet::new();
	for part in inputs.scene.parts.iter() {
		if part.cast_shadow
			&& part.size.max_element() < 1.0
			&& let Some(referent) = part.referent
		{
			small.insert(referent);
		}
	}
	if small.is_empty() {
		return Vec::new();
	}
	let mut saving = Saving::default();
	let mut views = 0.0;
	for work in inputs.works {
		let triangles: f64 = small.iter().filter_map(|referent| work.by_instance.get(referent)).map(|entry| entry.shadow_triangles).sum();
		let ms = triangles * super::attribute::SHADOW_REDRAW_SHARE / 1000.0 * prices::KILO_TRIANGLE_MS;
		if ms > 0.0 {
			saving.views.insert(work.name.clone(), round_ms(ms));
			if ms > saving.worst_ms {
				saving.worst_ms = round_ms(ms);
				saving.worst_view = work.name.clone();
			}
		}
		saving.mean_ms += ms;
		views += 1.0;
	}
	saving.mean_ms = round_ms(saving.mean_ms / f64::max(views, 1.0));
	let mut targets: Vec<String> = small.iter().map(|referent| inputs.id(*referent)).collect();
	targets.sort();
	vec![Finding {
		id: "small shadow casters".into(),
		rank: 0,
		kind: "castShadow",
		title: format!("{} parts under a stud across cast sun shadows: CastShadow off", targets.len()),
		targets: listed(&targets),
		target_count: targets.len(),
		why: Work::default(),
		saving,
		edit: Edit::Set { targets, classes: Vec::new(), properties: [("CastShadow".to_string(), json!(false))].into_iter().collect() },
		look: "changes",
		basis: "attribution",
		note: Some("only where the quality draws sun shadows (none on phones below quality 4)".into()),
	}]
}
