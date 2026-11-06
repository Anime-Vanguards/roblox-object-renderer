//! A view's cost drawn over its scene: `<view>.draws.png` gives every draw call a colour of its own
//! (all copies one call draws share it), `<view>.heat.png` colours what draws by how much of a draw
//! call it costs: red alone in its batch, through yellow, to blue sharing one with a hundred copies
//! or more. Parts the view culls draw grey; effects keep their shape in their colour.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;

use super::{Analysis, Kind, ViewCost};
use crate::scene::{Part, Scene};

const SMOOTH_PLASTIC: u32 = 272;
/// What the view does not draw (outside it or hidden).
const CULLED: Vec3 = Vec3::splat(0.32);

/// A colour per label: well apart in hue, steady between views and runs.
fn label_color(label: &str) -> Vec3 {
	let hash = super::meshes::fnv(label.as_bytes());
	let hue = (hash % 360) as f32;
	let saturation = 0.55 + ((hash >> 12) % 40) as f32 / 100.0;
	let value = 0.7 + ((hash >> 24) % 30) as f32 / 100.0;
	hsv(hue, saturation, value)
}

fn hsv(hue: f32, saturation: f32, value: f32) -> Vec3 {
	let c = value * saturation;
	let h = hue / 60.0;
	let x = c * (1.0 - (h % 2.0 - 1.0).abs());
	let (r, g, b) = match h as u32 {
		0 => (c, x, 0.0),
		1 => (x, c, 0.0),
		2 => (0.0, c, x),
		3 => (0.0, x, c),
		4 => (x, 0.0, c),
		_ => (c, 0.0, x),
	};
	Vec3::new(r, g, b) + Vec3::splat(value - c)
}

/// Red for a draw call (or more) of its own, yellow at ten copies a call, blue at a hundred or more.
pub fn heat_color(instances: u64, draws: u64) -> Vec3 {
	let share = (instances.max(1) as f32 / draws.max(1) as f32).log10().clamp(0.0, 2.0) / 2.0;
	hsv(share * 230.0, 0.85, 0.95)
}

/// The colour each drawn part and effect takes: by batch, or by its share of a draw call.
fn colors(analysis: &Analysis, view: &ViewCost, heat: bool) -> (HashMap<usize, Vec3>, HashMap<String, Vec3>) {
	let mut parts = HashMap::new();
	let mut effects = HashMap::new();
	for (rank, batch) in view.batches.iter().enumerate() {
		let color = if heat {
			heat_color(batch.instances, batch.draws)
		} else if batch.sorted || batch.items.len() == 1 {
			label_color(&format!("{}#{rank}", batch.label))
		} else {
			label_color(&batch.label)
		};
		for &index in &batch.items {
			let item = &analysis.items[index];
			match item.kind {
				Kind::Part => {
					if let Some(part) = item.part {
						parts.insert(part, color);
					}
				}
				Kind::Emitter | Kind::Beam | Kind::Trail => {
					effects.insert(item.path.clone(), color);
				}
				_ => {}
			}
		}
	}
	(parts, effects)
}

/// The scene drawn as the cost view shows it.
pub fn scene(scene: &Scene, analysis: &Analysis, view: &ViewCost, heat: bool) -> Scene {
	let (part_colors, effect_colors) = colors(analysis, view, heat);
	let parts: Vec<Arc<Part>> = scene
		.parts
		.iter()
		.enumerate()
		.filter(|(_, part)| part.transparency < 1.0)
		.map(|(index, part)| {
			let mut part = Part::clone(part);
			part.color = part_colors.get(&index).copied().unwrap_or(CULLED);
			part.material = SMOOTH_PLASTIC;
			part.material_variant.clear();
			part.texture = None;
			part.surface_appearance = None;
			part.composite = None;
			part.decals.clear();
			part.reflectance = 0.0;
			part.vertex_color = Vec3::ONE;
			part.transparency = part.transparency.min(0.6);
			Arc::new(part)
		})
		.collect();
	let flat = |color: Vec3| vec![(0.0, color), (1.0, color)];
	let particles = scene
		.particles
		.iter()
		.filter_map(|emitter| {
			let color = *effect_colors.get(&emitter.path)?;
			let mut emitter = emitter.clone();
			emitter.color = flat(color);
			emitter.light_emission = 0.0;
			emitter.brightness = 1.0;
			Some(emitter)
		})
		.collect();
	let beams = scene
		.beams
		.iter()
		.filter_map(|beam| {
			let color = *effect_colors.get(&beam.path)?;
			let mut beam = beam.clone();
			beam.color = flat(color);
			beam.light_emission = 0.0;
			beam.brightness = 1.0;
			Some(beam)
		})
		.collect();
	let trails = scene
		.trails
		.iter()
		.filter_map(|trail| {
			let color = *effect_colors.get(&trail.path)?;
			let mut trail = trail.clone();
			trail.color = flat(color);
			trail.light_emission = 0.0;
			trail.brightness = 1.0;
			Some(trail)
		})
		.collect();
	let mut environment = scene.environment.clone();
	environment.effects.clear();
	environment.atmosphere = None;
	environment.clouds = None;
	environment.fog_end = 1.0e6;
	environment.exposure_compensation = 0.0;
	environment.ambient = Vec3::splat(0.45);
	environment.outdoor_ambient = Vec3::splat(0.45);
	Scene { parts, particles, beams, trails, environment, materials: scene.materials.clone(), warnings: Vec::new(), ..Default::default() }
}

/// The colours of a view's costliest batches, as `#rrggbb` (what `draws.png` shows them in).
pub fn legend(view: &ViewCost, count: usize) -> Vec<(String, String)> {
	view.batches
		.iter()
		.enumerate()
		.take(count)
		.map(|(rank, batch)| {
			let color = if batch.sorted || batch.items.len() == 1 { label_color(&format!("{}#{rank}", batch.label)) } else { label_color(&batch.label) };
			let hex = format!("#{:02x}{:02x}{:02x}", (color.x * 255.0) as u8, (color.y * 255.0) as u8, (color.z * 255.0) as u8);
			(batch.label.clone(), hex)
		})
		.collect()
}
