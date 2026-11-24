//! Who pays for what in each view: every draw call, triangle, pixel, blended layer, light evaluation
//! and shadow triangle charged to the instance that causes it, priced by preset
//! ([`super::prices`]), and rolled up the instance tree (place, folder, model, part) so an agent can
//! go from a whole map down to the part that costs.
//!
//! - Draw calls: a batch's draws shared among the copies it draws (a part drawn alone pays its own).
//! - Triangles: what each copy draws at its level of detail.
//! - Parts: every part in the place costs the render queue and culling each frame, drawn or not.
//! - Pixels: the opaque pixels a part is in front at, by how it shades; blended fragments of see-through
//!   parts, decals and effects each time they shade; light evaluations charged to the light.
//! - Shadows: where the quality draws sun shadows, the triangles each caster within their reach draws
//!   into the shadow maps (cached by Roblox and redrawn as the camera moves: a share is charged).
//! - The sky, clouds, the Atmosphere and post effects: charged to their instances in Lighting.

use std::collections::{BTreeMap, HashMap};

use rbx_dom_weak::types::Ref;
use serde::Serialize;

use super::ids::InstanceIds;
use super::pixels::ViewPixels;
use super::prices::{self, Preset, Quality};
use super::{Analysis, Kind, Pass, ViewCost};
use crate::render::camera::Camera;
use crate::scene::load::Document;
use crate::scene::{Scene, props};

/// The share of the casters' shadow triangles charged each frame: Roblox caches its shadow maps and
/// redraws them as the camera moves (cost-probes sh1: a full redraw of 15 draws after a change, 3 a
/// frame while the camera moves). Estimated.
pub const SHADOW_REDRAW_SHARE: f64 = 0.25;
/// How far the sun's shadows reach from the camera (the renderer's last cascade, render/mod.rs).
pub const SHADOW_REACH: f32 = 400.0;

/// Full-screen features: viewport-units per frame (estimated, [`prices::weight`]'s scale).
pub fn feature_units(class: &str) -> Option<f64> {
	Some(match class {
		"Atmosphere" => 0.15,
		"Clouds" => 0.6,
		"BloomEffect" => 0.5,
		"DepthOfFieldEffect" => 0.8,
		"SunRaysEffect" => 0.4,
		"BlurEffect" => 0.6,
		"ColorCorrectionEffect" => 0.1,
		"ColorGradingEffect" => 0.1,
		_ => return None,
	})
}

/// Work and its price, for one instance in one view or summed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
	pub draws: f64,
	pub triangles: f64,
	/// Opaque pixels in front, at the preset's resolution.
	pub pixels: f64,
	/// Blended fragments (overdraw): see-through parts, decals, particles, beams, trails.
	pub blended: f64,
	/// Local light evaluations a light causes: the opaque pixels in its range (on a light).
	pub light_pixels: f64,
	/// The same evaluations as the surfaces see them: the lights over each of its pixels (on a part;
	/// charged to the lights, so the two never add up to more than one count).
	pub lit_pixels: f64,
	pub shadow_triangles: f64,
	/// Parts in the place.
	pub parts: f64,
	/// Relative GPU shading/overdraw work in viewport-units (1.0 = one viewport of opaque Plastic).
	pub gpu_units: f64,
	pub cpu_ms: f64,
	/// Calibrated GPU ms, or `None` (`null` in JSON) when the preset has no GPU calibration (see `gpu_units`).
	pub gpu_ms: Option<f64>,
	/// Frame ms: CPU plus the GPU's share of the waits when calibrated, else CPU ms only.
	pub ms: f64,
}

impl Default for Work {
	fn default() -> Self {
		Self {
			draws: 0.0,
			triangles: 0.0,
			pixels: 0.0,
			blended: 0.0,
			light_pixels: 0.0,
			lit_pixels: 0.0,
			shadow_triangles: 0.0,
			parts: 0.0,
			gpu_units: 0.0,
			cpu_ms: 0.0,
			gpu_ms: Some(0.0),
			ms: 0.0,
		}
	}
}

impl Work {
	pub fn empty_for(profile: &prices::Profile) -> Self {
		Self {
			gpu_ms: profile.gpu_ms_per_unit.map(|_| 0.0),
			..Self::default()
		}
	}

	pub fn add(&mut self, other: &Work) {
		self.draws += other.draws;
		self.triangles += other.triangles;
		self.pixels += other.pixels;
		self.blended += other.blended;
		self.light_pixels += other.light_pixels;
		self.lit_pixels += other.lit_pixels;
		self.shadow_triangles += other.shadow_triangles;
		self.parts += other.parts;
		self.gpu_units += other.gpu_units;
		self.cpu_ms += other.cpu_ms;
		self.gpu_ms = match (self.gpu_ms, other.gpu_ms) {
			(Some(a), Some(b)) => Some(a + b),
			_ => None,
		};
		self.ms += other.ms;
	}

	pub fn scaled(&self, by: f64) -> Work {
		Work {
			draws: self.draws * by,
			triangles: self.triangles * by,
			pixels: self.pixels * by,
			blended: self.blended * by,
			light_pixels: self.light_pixels * by,
			lit_pixels: self.lit_pixels * by,
			shadow_triangles: self.shadow_triangles * by,
			parts: self.parts * by,
			gpu_units: self.gpu_units * by,
			cpu_ms: self.cpu_ms * by,
			gpu_ms: self.gpu_ms.map(|v| v * by),
			ms: self.ms * by,
		}
	}

	pub fn minus(&self, other: &Work) -> Work {
		let mut out = *self;
		out.add(&other.scaled(-1.0));
		out
	}

	/// Rounded for output: counts to 0.01, ms and gpuUnits to 1e-4 (the same run gives the same digits).
	pub fn rounded(&self) -> Work {
		let r = |value: f64, step: f64| (value / step).round() * step;
		Work {
			draws: r(self.draws, 0.01),
			triangles: r(self.triangles, 0.01),
			pixels: r(self.pixels, 0.01),
			blended: r(self.blended, 0.01),
			light_pixels: r(self.light_pixels, 0.01),
			lit_pixels: r(self.lit_pixels, 0.01),
			shadow_triangles: r(self.shadow_triangles, 0.01),
			parts: r(self.parts, 0.01),
			gpu_units: r(self.gpu_units, 1e-4),
			cpu_ms: r(self.cpu_ms, 1e-4),
			gpu_ms: self.gpu_ms.map(|v| r(v, 1e-4)),
			ms: r(self.ms, 1e-4),
		}
	}

	/// The prices applied for `profile`: CPU from the counts, GPU units recorded always, GPU ms and
	/// GPU wait share added only when the profile has GPU calibration (otherwise `gpu_ms` is `None`).
	fn priced(mut self, units: f64, profile: &prices::Profile) -> Work {
		self.gpu_units += units;
		self.cpu_ms += self.draws * profile.draw_ms
			+ self.triangles / 1000.0 * profile.kilo_triangle_ms
			+ self.parts * profile.part_ms
			+ self.shadow_triangles * SHADOW_REDRAW_SHARE / 1000.0 * profile.kilo_triangle_ms;
		if let (Some(gpu_ms_per_unit), Some(gpu_to_frame)) = (profile.gpu_ms_per_unit, profile.gpu_to_frame) {
			let gpu_ms = self.gpu_ms.unwrap_or(0.0) + self.gpu_units * gpu_ms_per_unit;
			self.gpu_ms = Some(gpu_ms);
			self.ms = self.cpu_ms + gpu_ms * gpu_to_frame;
		} else {
			self.gpu_ms = None;
			self.ms = self.cpu_ms;
		}
		self
	}
}

/// Which instance each thing that costs belongs to.
pub struct Owners {
	/// Per scene part: its instance.
	parts: Vec<Ref>,
	/// Instances by the path the scene names them by (the first of a repeated path, in file order).
	by_path: HashMap<String, Ref>,
	/// Per light in the scene: its instance.
	lights: Vec<Ref>,
	/// Per analysis item of an emitter, beam or trail: its instance.
	effects: HashMap<usize, Ref>,
	/// The enabled full-screen features in Lighting (and the Terrain's Clouds): instance and class.
	pub features: Vec<(Ref, String)>,
	/// The Lighting's Sky instance, if any.
	pub sky: Option<Ref>,
}

impl Owners {
	/// `lent`: the scene draws another file's Lighting (`--lighting-from`), not the document's.
	pub fn new(document: &Document, scene: &Scene, analysis: &Analysis, lent: bool) -> Self {
		let dom = &document.dom;
		let mut by_path: HashMap<String, Ref> = HashMap::new();
		let mut stack = vec![dom.root_ref()];
		let mut order = Vec::new();
		while let Some(referent) = stack.pop() {
			let Some(instance) = dom.get_by_ref(referent) else { continue };
			if referent != dom.root_ref() {
				order.push(referent);
			}
			stack.extend(instance.children().iter().rev().copied());
		}
		// the enabled lights at each path (the scene reads enabled lights only: scene::extract::read_light)
		let mut lights_by_path: HashMap<String, Vec<Ref>> = HashMap::new();
		for referent in order {
			let path = document.path_of(referent);
			if let Some(instance) = dom.get_by_ref(referent)
				&& matches!(instance.class.as_str(), "PointLight" | "SpotLight" | "SurfaceLight")
				&& props::bool(instance, "Enabled", true)
			{
				lights_by_path.entry(path.clone()).or_default().push(referent);
			}
			by_path.entry(path).or_insert(referent);
		}
		// an instance by a scene path: a legacy effect's (`Torch.Fire#flame`) is its instance's
		let by_scene_path = |path: &str| by_path.get(path.split('#').next().unwrap_or(path)).copied();
		let live = |referent: Ref| (!referent.is_none() && dom.get_by_ref(referent).is_some()).then_some(referent);
		// effects: the scene keeps each one's instance (a legacy effect's by its path)
		let mut effects = HashMap::new();
		for (&index, emitter) in &analysis.emitters {
			if let Some(referent) = emitter.emitter.referent.and_then(live).or_else(|| by_scene_path(&emitter.emitter.path)) {
				effects.insert(index, referent);
			}
		}
		for (&index, beam) in &analysis.beams {
			if let Some(referent) = live(beam.referent).or_else(|| by_scene_path(&beam.path)) {
				effects.insert(index, referent);
			}
		}
		for (&index, trail) in &analysis.trails {
			if let Some(referent) = live(trail.referent).or_else(|| by_scene_path(&trail.path)) {
				effects.insert(index, referent);
			}
		}
		// lights keep no instance: among the enabled ones at the light's path, the one with its
		// Brightness, Range and Shadows, else the next not yet taken
		let mut lights = vec![Ref::none(); scene.lights.len()];
		let mut taken: std::collections::HashSet<Ref> = std::collections::HashSet::new();
		for (index, light) in scene.lights.iter().enumerate() {
			let Some(candidates) = lights_by_path.get(&light.path) else { continue };
			let same = |referent: &&Ref| {
				dom.get_by_ref(**referent).is_some_and(|instance| {
					(props::f32(instance, "Brightness", 1.0) - light.brightness).abs() < 1e-4 && (props::f32(instance, "Range", 8.0) - light.range).abs() < 1e-4 && props::bool(instance, "Shadows", false) == light.shadows
				})
			};
			let pick = candidates.iter().filter(|referent| !taken.contains(*referent)).find(same).or_else(|| candidates.iter().find(|referent| !taken.contains(*referent))).or(candidates.first());
			if let Some(&referent) = pick {
				taken.insert(referent);
				lights[index] = referent;
			}
		}
		let parts = scene.parts.iter().map(|part| part.referent.or_else(|| by_path.get(&part.path).copied()).unwrap_or_else(Ref::none)).collect();
		let mut features = Vec::new();
		let mut sky = None;
		let enabled = |instance: &rbx_dom_weak::Instance| props::bool(instance, "Enabled", true);
		if lent {
			// another file's Lighting draws: its effects as the scene holds them, owned by no instance here
			use crate::scene::environment::PostEffect;
			for effect in &scene.environment.effects {
				let class = match effect {
					PostEffect::Bloom { .. } => "BloomEffect",
					PostEffect::ColorCorrection { .. } => "ColorCorrectionEffect",
					PostEffect::ColorGrading { .. } => "ColorGradingEffect",
					PostEffect::SunRays { .. } => "SunRaysEffect",
					PostEffect::DepthOfField { .. } => "DepthOfFieldEffect",
					PostEffect::Blur { .. } => "BlurEffect",
				};
				features.push((Ref::none(), class.to_string()));
			}
			if scene.environment.atmosphere.is_some() {
				features.push((Ref::none(), "Atmosphere".to_string()));
			}
			if scene.environment.clouds.is_some() {
				features.push((Ref::none(), "Clouds".to_string()));
			}
			return Self { parts, by_path, lights, effects, features, sky };
		}
		if let Some(lighting) = document.lighting.and_then(|referent| dom.get_by_ref(referent)) {
			for &child in lighting.children() {
				let Some(instance) = dom.get_by_ref(child) else { continue };
				if instance.class == "Sky" && sky.is_none() {
					sky = Some(child);
				}
				if feature_units(&instance.class).is_some() && enabled(instance) {
					features.push((child, instance.class.to_string()));
				}
			}
		}
		if scene.environment.clouds.is_some()
			&& let Some(clouds) = by_path.iter().filter(|(path, _)| path.starts_with("Workspace.Terrain")).map(|(_, referent)| *referent).find(|referent| dom.get_by_ref(*referent).is_some_and(|instance| instance.class == "Clouds"))
		{
			features.push((clouds, "Clouds".into()));
		}
		features.sort_by_key(|(referent, _)| document.path_of(*referent));
		Self { parts, by_path, lights, effects, features, sky }
	}

	fn of_item(&self, scene: &Scene, analysis: &Analysis, index: usize) -> Ref {
		let item = &analysis.items[index];
		if let Some(part) = item.part
			&& matches!(item.kind, Kind::Part | Kind::Decal | Kind::Texture)
		{
			let _ = scene;
			return self.parts.get(part).copied().unwrap_or_else(Ref::none);
		}
		if let Some(referent) = self.effects.get(&index) {
			return *referent;
		}
		// a legacy effect's path names its instance before the `#`
		self.by_path.get(item.path.split('#').next().unwrap_or(&item.path)).copied().unwrap_or_else(Ref::none)
	}

	/// The instance of an analysis item (a part's, or an effect's by its place among same-named ones).
	pub fn item(&self, scene: &Scene, analysis: &Analysis, index: usize) -> Ref {
		self.of_item(scene, analysis, index)
	}

	/// The instance the scene names by `path` (the first of a repeated path).
	pub fn by_path(&self, path: &str) -> Option<Ref> {
		self.by_path.get(path).copied()
	}

	pub fn part(&self, index: usize) -> Ref {
		self.parts.get(index).copied().unwrap_or_else(Ref::none)
	}

	pub fn light(&self, index: usize) -> Ref {
		self.lights.get(index).copied().unwrap_or_else(Ref::none)
	}
}

/// One view's work by instance (Ref::none(): what no instance owns, the shared passes).
#[derive(Clone, Debug, Default)]
pub struct ViewWork {
	pub name: String,
	pub by_instance: HashMap<Ref, Work>,
	pub total: Work,
	/// Whether pixels were counted (a layers render of the view).
	pub pixels: bool,
}

/// What a quality draws of the sun's shadows, given the place's Lighting.
pub fn sun_shadows(scene: &Scene, quality: &Quality) -> bool {
	quality.sun_shadows && scene.environment.global_shadows
}

/// A view's work by instance.
#[allow(clippy::too_many_arguments)]
pub fn view(scene: &Scene, analysis: &Analysis, owners: &Owners, ids: &InstanceIds, view: &ViewCost, camera: &Camera, pixels: Option<&ViewPixels>, preset: &Preset, quality: &Quality) -> ViewWork {
	let mut work: HashMap<Ref, Work> = HashMap::new();
	let mut units: HashMap<Ref, f64> = HashMap::new();
	// draw calls: each batch's shared among the copies it lists (a queued item lists once per pass)
	for batch in &view.batches {
		if batch.items.is_empty() {
			continue;
		}
		let share = batch.draws as f64 / batch.items.len() as f64;
		for &index in &batch.items {
			work.entry(owners.of_item(scene, analysis, index)).or_default().draws += share;
		}
	}
	for &(index, _, triangles) in &view.drawn {
		work.entry(owners.of_item(scene, analysis, index as usize)).or_default().triangles += triangles as f64;
	}
	// the shared passes: the particle pass, the sky's faces and the clouds' draws
	for (pass, count) in &view.passes {
		let owner = match pass {
			Pass::Sky => owners.sky.unwrap_or_else(Ref::none),
			Pass::Particles | Pass::Cloud | Pass::GenericPostProcess => Ref::none(),
			_ => continue,
		};
		let entry = work.entry(owner).or_default();
		entry.draws += count.draws as f64;
		entry.triangles += count.triangles as f64;
	}
	// every part in the place, each frame
	for index in 0..scene.parts.len() {
		work.entry(owners.part(index)).or_default().parts += 1.0;
	}
	// sun shadows: casters within reach, at full detail (Studio draws a mesh's shadow at full detail)
	if sun_shadows(scene, quality) {
		for item in analysis.items.iter().filter(|item| item.kind == Kind::Part && !item.transparent) {
			let Some(part) = item.part.and_then(|index| scene.parts.get(index).map(|part| (index, part))) else { continue };
			if !part.1.cast_shadow || crate::lod::box_distance(camera.eye, item.bounds.0, item.bounds.1) > SHADOW_REACH {
				continue;
			}
			work.entry(owners.part(part.0)).or_default().shadow_triangles += item.triangles[0] as f64;
		}
	}
	if let Some(pixels) = pixels {
		let scale = (preset.width as f64 * preset.height as f64) / (pixels.width as f64 * pixels.height as f64).max(1.0);
		let mut sorted: Vec<(&usize, &u64)> = pixels.part_pixels.iter().collect();
		sorted.sort();
		for (&part, &count) in sorted {
			let owner = owners.part(part);
			work.entry(owner).or_default().pixels += count as f64 * scale;
			*units.entry(owner).or_default() += pixels.part_units.get(&part).copied().unwrap_or(0.0);
		}
		let mut sorted: Vec<(&usize, &u64)> = pixels.part_blended.iter().collect();
		sorted.sort();
		for (&part, &count) in sorted {
			let owner = owners.part(part);
			work.entry(owner).or_default().blended += count as f64 * scale;
			*units.entry(owner).or_default() += pixels.part_blended_units.get(&part).copied().unwrap_or(0.0);
		}
		let mut sorted: Vec<(&usize, &u64)> = pixels.part_light_pixels.iter().collect();
		sorted.sort();
		for (&part, &count) in sorted {
			work.entry(owners.part(part)).or_default().lit_pixels += count as f64 * scale;
		}
		let mut sorted: Vec<(&usize, &u64)> = pixels.effect_blended.iter().collect();
		sorted.sort();
		for (&item, &count) in sorted {
			let owner = owners.of_item(scene, analysis, item);
			work.entry(owner).or_default().blended += count as f64 * scale;
			*units.entry(owner).or_default() += pixels.effect_units.get(&item).copied().unwrap_or(0.0);
		}
		let mut sorted: Vec<(&usize, &u64)> = pixels.light_pixels.iter().collect();
		sorted.sort();
		for (&light, &count) in sorted {
			let owner = owners.light(light);
			work.entry(owner).or_default().light_pixels += count as f64 * scale;
			*units.entry(owner).or_default() += pixels.light_units.get(&light).copied().unwrap_or(0.0);
		}
		*units.entry(owners.sky.unwrap_or_else(Ref::none)).or_default() += pixels.sky_units;
	}
	for (owner, class) in &owners.features {
		// Roblox draws no bloom below quality 6 (measured, prices::Quality)
		if class == "BloomEffect" && !quality.bloom {
			continue;
		}
		*units.entry(*owner).or_default() += feature_units(class).unwrap_or(0.0);
	}
	let profile = prices::Profile::for_preset(preset);
	let mut by_instance = HashMap::new();
	let mut total = Work::empty_for(&profile);
	let key_of = |referent: &Ref| ids.id(*referent).unwrap_or(SHARED).to_string();
	let mut keys: Vec<Ref> = work.keys().chain(units.keys()).copied().collect();
	keys.sort_by_key(key_of);
	keys.dedup();
	for key in keys {
		let priced = work.get(&key).copied().unwrap_or_default().priced(units.get(&key).copied().unwrap_or(0.0), &profile);
		by_instance.insert(key, priced);
	}
	// summed in a fixed order (by id), so the same run gives the same digits
	let mut ordered: Vec<(&Ref, &Work)> = by_instance.iter().collect();
	ordered.sort_by_key(|(referent, _)| key_of(referent));
	for (_, entry) in ordered {
		total.add(entry);
	}
	ViewWork { name: view.name.clone(), by_instance, total, pixels: pixels.is_some() }
}

/// One instance's line in the tree.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
	pub id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub uid: Option<String>,
	pub class: String,
	pub name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub parent: Option<String>,
	/// Its own work and all of its descendants', averaged over the views.
	pub mean: Work,
	/// The view where it costs most, and its work there.
	pub worst_view: String,
	pub worst: Work,
	/// Views in which it draws or shades anything.
	pub views: u32,
	/// Instances under it (itself included) that cost anything.
	pub leaves: u32,
}

/// The tree's id for what no instance owns.
pub const SHARED: &str = "(shared passes)";

/// Every instance that costs anything in some view, and each of its ancestors, with its subtree's work
/// averaged over the views and at its worst view; sorted by mean ms, ties by id.
pub fn tree(ids: &InstanceIds, views: &[ViewWork]) -> Vec<Node> {
	// per id: work per view (the subtree's), leaves
	let mut sums: BTreeMap<String, (Vec<Work>, Ref, u32)> = BTreeMap::new();
	let count = views.len();
	for (view_index, view) in views.iter().enumerate() {
		let mut leaves: Vec<(&Ref, &Work)> = view.by_instance.iter().collect();
		leaves.sort_by_key(|(referent, _)| ids.id(**referent).unwrap_or(SHARED).to_string());
		for (&referent, work) in leaves {
			// the instance and every ancestor
			let mut current = referent;
			loop {
				let id = if current.is_none() { SHARED.to_string() } else { ids.id(current).map(str::to_string).unwrap_or_else(|| SHARED.to_string()) };
				let entry = sums.entry(id).or_insert_with(|| (vec![Work::default(); count], current, 0));
				entry.0[view_index].add(work);
				let parent = ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
				if current.is_none() || parent.is_none() {
					break;
				}
				current = parent;
			}
		}
	}
	// leaves: instances that cost anything themselves, counted up their ancestors
	let mut own: BTreeMap<String, Ref> = BTreeMap::new();
	for view in views {
		for (&referent, work) in &view.by_instance {
			if work.ms > 0.0 || work.gpu_units > 0.0 || work.draws > 0.0 || work.pixels > 0.0 {
				own.insert(if referent.is_none() { SHARED.to_string() } else { ids.id(referent).unwrap_or(SHARED).to_string() }, referent);
			}
		}
	}
	for &referent in own.values() {
		let mut current = referent;
		loop {
			let id = if current.is_none() { SHARED.to_string() } else { ids.id(current).unwrap_or(SHARED).to_string() };
			if let Some(entry) = sums.get_mut(&id) {
				entry.2 += 1;
			}
			let parent = ids.info(current).map(|info| info.0).unwrap_or_else(Ref::none);
			if current.is_none() || parent.is_none() {
				break;
			}
			current = parent;
		}
	}
	let uncalibrated_gpu = views.iter().any(|view| view.total.gpu_ms.is_none());
	let mut nodes: Vec<Node> = sums
		.into_iter()
		.map(|(id, (per_view, referent, leaves))| {
			let mut mean = Work::default();
			for work in &per_view {
				mean.add(work);
			}
			let mut mean = mean.scaled(1.0 / count.max(1) as f64);
			let (worst_index, mut worst) = per_view.iter().enumerate().fold((0, Work::default()), |best, (index, work)| {
				if work.ms > best.1.ms || (uncalibrated_gpu && work.ms == best.1.ms && work.gpu_units > best.1.gpu_units) {
					(index, *work)
				} else {
					best
				}
			});
			if uncalibrated_gpu {
				mean.gpu_ms = None;
				worst.gpu_ms = None;
			}
			let in_view = per_view.iter().filter(|work| work.draws > 0.0 || work.pixels > 0.0 || work.blended > 0.0).count() as u32;
			let info = ids.info(referent);
			Node {
				uid: ids.uid(referent).map(str::to_string),
				class: info.map(|info| info.1.clone()).unwrap_or_default(),
				name: info.map(|info| info.2.clone()).unwrap_or_else(|| id.clone()),
				parent: info.and_then(|info| (!info.0.is_none()).then(|| ids.id(info.0).map(str::to_string)).flatten()),
				id,
				mean: mean.rounded(),
				worst_view: views.get(worst_index).map(|view| view.name.clone()).unwrap_or_default(),
				worst: worst.rounded(),
				views: in_view,
				leaves,
			}
		})
		.collect();
	nodes.sort_by(|a, b| {
		let ord = b.mean.ms.total_cmp(&a.mean.ms);
		if uncalibrated_gpu {
			ord.then_with(|| b.mean.gpu_units.total_cmp(&a.mean.gpu_units)).then_with(|| a.id.cmp(&b.id))
		} else {
			ord.then_with(|| a.id.cmp(&b.id))
		}
	});
	nodes
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};
	use rbx_dom_weak::InstanceBuilder;

	#[test]
	fn tree_order_uses_id_tiebreak_when_calibrated_and_gpu_units_when_uncalibrated() {
		let mut model = ModelBuilder::new("Map");
		let alpha = model.part(&PartSpec { name: "Alpha".into(), ..Default::default() });
		let beta = model.part(&PartSpec { name: "Beta".into(), ..Default::default() });
		let gamma = model.part(&PartSpec { name: "Gamma".into(), ..Default::default() });
		let document = model.into_document();
		let ids = InstanceIds::new(&document);

		let make_work = |ms: f64, gpu_units: f64, gpu_ms: Option<f64>| Work {
			draws: 1.0,
			gpu_units,
			cpu_ms: ms,
			gpu_ms,
			ms,
			..Default::default()
		};

		// Calibrated run (phone / default): Alpha and Beta tie on rounded ms (0.0302) while Beta
		// has higher gpu_units; calibrated tree preserves id order (Alpha before Beta).
		let mut cal_by = HashMap::new();
		cal_by.insert(alpha, make_work(0.03021, 0.10, Some(0.01)));
		cal_by.insert(beta, make_work(0.03024, 0.80, Some(0.01)));
		cal_by.insert(gamma, make_work(0.03022, 0.80, Some(0.01)));
		let cal_view = ViewWork { name: "v1".into(), total: make_work(0.0907, 1.70, Some(0.03)), by_instance: cal_by, pixels: true };
		let cal_tree = tree(&ids, &[cal_view]);
		let cal_ids: Vec<&str> = cal_tree.iter().map(|n| n.id.as_str()).collect();
		assert_eq!(cal_ids, vec!["Map", "Map.Alpha", "Map.Beta", "Map.Gamma"]);

		// Uncalibrated GPU run (--preset desktop, gpu_ms = None): ties on ms break by gpu_units
		// descending so pure GPU work rises above lower-GPU items, then id ascending.
		let mut desk_by = HashMap::new();
		desk_by.insert(alpha, make_work(0.03021, 0.10, None));
		desk_by.insert(beta, make_work(0.03024, 0.80, None));
		desk_by.insert(gamma, make_work(0.03022, 0.80, None));
		let desk_view = ViewWork { name: "v1".into(), total: make_work(0.0907, 1.70, None), by_instance: desk_by, pixels: true };
		let desk_tree = tree(&ids, &[desk_view]);
		let desk_ids: Vec<&str> = desk_tree.iter().map(|n| n.id.as_str()).collect();
		assert_eq!(desk_ids, vec!["Map", "Map.Beta", "Map.Gamma", "Map.Alpha"]);
	}

	/// Two emitters named Dust on one part, the first at Rate 0 (fired by script, no item), and two
	/// lights named Lamp, the first disabled (no scene light): what draws is charged to the second of
	/// each, not to the first of the name.
	#[test]
	fn same_named_effects_and_lights_are_charged_to_the_ones_that_draw() {
		let mut model = ModelBuilder::new("Map");
		let torch = model.part(&PartSpec { name: "Torch".into(), ..Default::default() });
		let silent = model.child(torch, InstanceBuilder::new("ParticleEmitter").with_name("Dust").with_property("Rate", 0.0f32));
		let dust = model.child(torch, InstanceBuilder::new("ParticleEmitter").with_name("Dust").with_property("Rate", 50.0f32));
		let off = model.child(torch, InstanceBuilder::new("PointLight").with_name("Lamp").with_property("Enabled", false).with_property("Brightness", 3.0f32));
		let lamp = model.child(torch, InstanceBuilder::new("PointLight").with_name("Lamp").with_property("Brightness", 5.0f32));
		// a legacy Fire: its emitters' scene paths are `Map.Torch.Fire#...`
		let brazier = model.part(&PartSpec { name: "Brazier".into(), ..Default::default() });
		let fire = model.child(brazier, InstanceBuilder::new("Fire"));
		let document = model.into_document();
		let scene = crate::scene::extract::extract(&document);
		let state = std::env::temp_dir().join(format!("ror-owners-test-{}", std::process::id()));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let analysis = Analysis::new(&scene, &document, &assets, &super::super::CostOptions::default());
		let owners = Owners::new(&document, &scene, &analysis, false);
		let emitters: Vec<(String, Ref)> = analysis.items.iter().enumerate().filter(|(_, item)| item.kind == Kind::Emitter).map(|(index, item)| (item.path.clone(), owners.item(&scene, &analysis, index))).collect();
		let torch_emitters: Vec<Ref> = emitters.iter().filter(|(path, _)| path.contains("Torch")).map(|(_, referent)| *referent).collect();
		assert!(!torch_emitters.is_empty(), "the Rate 50 emitter draws");
		assert!(torch_emitters.iter().all(|referent| *referent == dust && *referent != silent));
		let fire_emitters: Vec<&(String, Ref)> = emitters.iter().filter(|(path, _)| path.contains("Brazier")).collect();
		assert!(!fire_emitters.is_empty() && fire_emitters.iter().all(|(_, referent)| *referent == fire), "the Fire's emitters are the Fire's: {fire_emitters:?}");
		assert_eq!(scene.lights.len(), 1, "the disabled light is not read");
		assert!(owners.light(0) == lamp && owners.light(0) != off);
		let _ = std::fs::remove_dir_all(&state);
	}
}
