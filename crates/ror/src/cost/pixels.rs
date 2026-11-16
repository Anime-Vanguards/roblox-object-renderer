//! What each view's pixels cost the GPU, part by part: from the renderer's layers of the view
//! ([`crate::render::ids::Layers`]: the frontmost opaque part and its depth at every pixel, and the
//! see-through parts' and decals' fragments in front of it), the effects' quads rasterized here against
//! that depth, and the local lights whose range holds each opaque pixel. Weighed in viewport-units
//! ([`super::prices::weight`]): one viewport of opaque Plastic shaded once is 1.

use std::collections::HashMap;

use glam::{Vec3, Vec4};

use super::prices::{Quality, weight};
use super::{Analysis, Kind};
use crate::render::camera::Camera;
use crate::render::Layers;
use crate::scene::{LightKind, Part, Scene};

const NEON: u32 = 288;
const PLASTIC: u32 = 256;
const SMOOTH_PLASTIC: u32 = 272;
const GLASS: u32 = 1568;
const ALPHA_TRANSPARENCY: u32 = 1;

/// One view's pixel work.
#[derive(Clone, Debug, Default)]
pub struct ViewPixels {
	pub width: u32,
	pub height: u32,
	/// Per part (index in `Scene::parts`): opaque pixels it is in front at, and the viewport-units they
	/// cost (its shading, sun-shadow sampling).
	pub part_pixels: HashMap<usize, u64>,
	pub part_units: HashMap<usize, f64>,
	/// Per part: blended fragments of it and its decals in front of the opaque surface, and their units.
	pub part_blended: HashMap<usize, u64>,
	pub part_blended_units: HashMap<usize, f64>,
	/// Per part: light evaluations on its opaque pixels (each local light holding one, once).
	pub part_light_pixels: HashMap<usize, u64>,
	/// Per item (index in `Analysis::items`) of an emitter, beam or trail: its blended fragments.
	pub effect_blended: HashMap<usize, u64>,
	pub effect_units: HashMap<usize, f64>,
	/// Per light (index in `Scene::lights`): opaque pixels in its range, and their units (charged when
	/// the quality lights per pixel).
	pub light_pixels: HashMap<usize, u64>,
	pub light_units: HashMap<usize, f64>,
	pub sky_pixels: u64,
	pub sky_units: f64,
	/// Per pixel, for the heat images: blended layers (parts, decals and effects), local lights, and
	/// the pixel's cost in units times the pixel count (1: a viewport of Plastic's worth per pixel).
	pub layers: Vec<u32>,
	pub lights: Vec<u16>,
	pub cost: Vec<f32>,
	/// Per pixel: the frontmost opaque part + 1 (0: none), as the renderer gave it.
	pub opaque: Vec<u32>,
}

impl ViewPixels {
	pub fn total_units(&self) -> f64 {
		self.part_units.values().sum::<f64>() + self.part_blended_units.values().sum::<f64>() + self.effect_units.values().sum::<f64>() + self.light_units.values().sum::<f64>() + self.sky_units
	}
}

/// How an opaque part shades a pixel ([`weight`]).
pub fn shading_weight(part: &Part) -> f64 {
	match part.material {
		NEON => weight::NEON,
		GLASS => weight::GLASS,
		_ if part.surface_appearance.is_some() || part.texture.is_some() => weight::TEXTURED,
		PLASTIC | SMOOTH_PLASTIC => weight::PLAIN,
		_ => weight::TEXTURED,
	}
}

/// What one blended fragment of a part (or its decal) costs.
fn blended_weight(part: &Part) -> f64 {
	if part.surface_appearance.as_ref().is_some_and(|appearance| appearance.alpha_mode == ALPHA_TRANSPARENCY) { weight::BLENDED_TEXTURED } else { weight::BLENDED }
}

/// A light's volume: where it shades.
struct LightVolume {
	index: usize,
	position: Vec3,
	range: f32,
	/// A spot or surface light's direction and cos(Angle / 2) (None: all round).
	cone: Option<(Vec3, f32)>,
}

fn light_volumes(scene: &Scene) -> Vec<LightVolume> {
	let normals = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	scene
		.lights
		.iter()
		.enumerate()
		.filter(|(_, light)| light.range > 0.0 && light.brightness > 0.0)
		.map(|(index, light)| {
			let face = normals[light.face.min(5) as usize];
			let direction = Vec3::from(light.cframe.matrix3 * glam::Vec3A::from(face)).normalize_or_zero();
			let mut position = Vec3::from(light.cframe.translation);
			let cone = match light.kind {
				LightKind::Point => None,
				LightKind::Spot => Some((direction, (light.angle.clamp(0.0, 180.0) * 0.5).to_radians().cos())),
				LightKind::Surface => {
					position += direction * (light.part_size.abs().dot(face.abs()) * 0.5);
					Some((direction, (light.angle.clamp(0.0, 180.0) * 0.5).to_radians().cos().min(0.0)))
				}
			};
			// a surface light's face reaches its half extent past its centre
			let range = light.range + if light.kind == LightKind::Surface { light.part_size.length() * 0.5 } else { 0.0 };
			LightVolume { index, position, range, cone }
		})
		.collect()
}

impl LightVolume {
	fn holds(&self, point: Vec3) -> bool {
		let to = point - self.position;
		let distance = to.length_squared();
		if distance > self.range * self.range {
			return false;
		}
		match self.cone {
			None => true,
			Some((direction, cos)) => distance < 1e-6 || to.dot(direction) >= cos * distance.sqrt(),
		}
	}
}

/// Rasterizes a world-space triangle into a `width` x `height` raster, calling `pixel` with each
/// covered pixel's index where it lies in front of `depth` (reversed infinite Z, as the renderer's).
fn raster(view: &glam::Mat4, triangle: [Vec3; 3], width: u32, height: u32, depth: &[f32], pixel: &mut impl FnMut(usize)) {
	let clip: Vec<Vec4> = triangle.iter().map(|corner| *view * corner.extend(1.0)).collect();
	// clip against the near side (w above a hair), then fan the polygon
	const NEAR: f32 = 1e-3;
	let mut polygon: Vec<Vec4> = Vec::with_capacity(4);
	for index in 0..3 {
		let (a, b) = (clip[index], clip[(index + 1) % 3]);
		if a.w >= NEAR {
			polygon.push(a);
		}
		if (a.w >= NEAR) != (b.w >= NEAR) {
			let t = (NEAR - a.w) / (b.w - a.w);
			polygon.push(a + (b - a) * t);
		}
	}
	if polygon.len() < 3 {
		return;
	}
	let screen: Vec<Vec3> = polygon.iter().map(|c| Vec3::new((c.x / c.w * 0.5 + 0.5) * width as f32, (0.5 - c.y / c.w * 0.5) * height as f32, c.z / c.w)).collect();
	for fan in 1..screen.len() - 1 {
		let [a, b, c] = [screen[0], screen[fan], screen[fan + 1]];
		let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
		if area.abs() < 1e-9 {
			continue;
		}
		let x0 = a.x.min(b.x).min(c.x).floor().max(0.0) as i64;
		let x1 = (a.x.max(b.x).max(c.x).ceil() as i64).min(width as i64 - 1);
		let y0 = a.y.min(b.y).min(c.y).floor().max(0.0) as i64;
		let y1 = (a.y.max(b.y).max(c.y).ceil() as i64).min(height as i64 - 1);
		// samples a hair off the pixel centre, so a pixel on the edge two triangles share counts once
		for y in y0..=y1 {
			let py = y as f32 + 0.5 + 1.37e-4;
			for x in x0..=x1 {
				let px = x as f32 + 0.5 + 0.71e-4;
				let w0 = ((b.x - px) * (c.y - py) - (b.y - py) * (c.x - px)) / area;
				let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) / area;
				let w2 = 1.0 - w0 - w1;
				if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
					continue;
				}
				let z = w0 * a.z + w1 * b.z + w2 * c.z;
				let index = y as usize * width as usize + x as usize;
				if z >= depth[index] {
					pixel(index);
				}
			}
		}
	}
}

/// A view's pixel work from its layers (`camera` and `aspect` as drawn).
#[allow(clippy::too_many_arguments)]
pub fn view(scene: &Scene, analysis: &Analysis, drawn_items: &[usize], camera: &Camera, layers: &Layers, quality: &Quality, sun_shadows: bool) -> ViewPixels {
	let (width, height) = (layers.width, layers.height);
	let count = width as usize * height as usize;
	let mut out = ViewPixels { width, height, layers: layers.blended.clone(), lights: vec![0; count], cost: vec![0.0; count], opaque: layers.opaque.clone(), ..Default::default() };
	let per_pixel = 1.0 / count.max(1) as f64;
	let aspect = width as f32 / height.max(1) as f32;
	let tan = (camera.fov.to_radians() * 0.5).tan();
	let (right, up, back) = (Vec3::from(camera.world.matrix3.x_axis), Vec3::from(camera.world.matrix3.y_axis), Vec3::from(camera.world.matrix3.z_axis));
	let look = -back;
	// the lights that can reach what the view shows: in front of the camera, their sphere in the frustum
	let lights: Vec<LightVolume> = light_volumes(scene)
		.into_iter()
		.filter(|light| {
			let to = light.position - camera.eye;
			let ahead = to.dot(look);
			if ahead < -light.range {
				return false;
			}
			let x = to.dot(right).abs() - light.range;
			let y = to.dot(up).abs() - light.range;
			let reach = ahead.max(0.0) + light.range;
			x <= reach * tan * aspect * 1.05 && y <= reach * tan * 1.05
		})
		.collect();
	let light_charge = if quality.per_pixel_lights { weight::LIGHT } else { 0.0 };
	let shadow_charge = if sun_shadows { weight::SUN_SHADOW } else { 0.0 };
	let weights: Vec<f64> = scene.parts.iter().map(|part| shading_weight(part)).collect();
	for (index, (&opaque, &depth)) in layers.opaque.iter().zip(&layers.depth).enumerate() {
		if opaque == 0 || depth <= 0.0 {
			out.sky_pixels += 1;
			out.sky_units += weight::SKY * per_pixel;
			out.cost[index] = weight::SKY as f32;
			continue;
		}
		let part = opaque as usize - 1;
		let shade = weights.get(part).copied().unwrap_or(weight::PLAIN) + shadow_charge;
		*out.part_pixels.entry(part).or_default() += 1;
		*out.part_units.entry(part).or_default() += shade * per_pixel;
		let mut cost = shade;
		if !lights.is_empty() {
			let (x, y) = ((index % width as usize) as f32, (index / width as usize) as f32);
			let ndc = (2.0 * (x + 0.5) / width as f32 - 1.0, 1.0 - 2.0 * (y + 0.5) / height as f32);
			let distance = camera.near / depth;
			let point = camera.eye + right * (ndc.0 * tan * aspect * distance) + up * (ndc.1 * tan * distance) + look * distance;
			let mut reached = 0u16;
			for light in &lights {
				if light.holds(point) {
					reached += 1;
					*out.light_pixels.entry(light.index).or_default() += 1;
					*out.light_units.entry(light.index).or_default() += light_charge * per_pixel;
				}
			}
			if reached > 0 {
				out.lights[index] = reached;
				*out.part_light_pixels.entry(part).or_default() += reached as u64;
				cost += light_charge * reached as f64;
			}
		}
		out.cost[index] = cost as f32;
	}
	// see-through parts and decals: the renderer counted their fragments per part and per pixel
	for (part, &fragments) in layers.part_blended.iter().enumerate() {
		if fragments == 0 {
			continue;
		}
		let Some(drawn) = scene.parts.get(part) else { continue };
		let w = blended_weight(drawn);
		out.part_blended.insert(part, fragments as u64);
		out.part_blended_units.insert(part, w * fragments as f64 * per_pixel);
	}
	for (index, &layers) in layers.blended.iter().enumerate() {
		if layers > 0 {
			out.cost[index] += (weight::BLENDED * layers as f64) as f32;
		}
	}
	// effects: their quads and ribbons against the opaque depth
	let view_projection = camera.view_projection(aspect);
	let back_axis = back;
	for &index in drawn_items {
		let item = &analysis.items[index];
		let mut triangles: Vec<[Vec3; 3]> = Vec::new();
		let mut share = 1.0f32;
		match item.kind {
			Kind::Emitter => {
				let Some(emitter) = analysis.emitters.get(&index) else { continue };
				let offset = emitter.follow.map(|follow| follow.offset(camera.eye, look)).unwrap_or(Vec3::ZERO);
				let mut source = emitter.emitter.clone();
				source.cframe.translation += glam::Vec3A::from(offset);
				let particles: Vec<_> = emitter.particles.iter().map(|particle| crate::render::particles::Particle { position: particle.position + offset, ..*particle }).collect();
				let mut vertices = Vec::new();
				crate::render::particles::quads(&source, &particles, camera, 1.0, Some(&view_projection), &mut vertices);
				triangles.extend(vertices.as_chunks::<3>().0.iter().map(|t| [Vec3::from(t[0].position), Vec3::from(t[1].position), Vec3::from(t[2].position)]));
				// a phone makes at most quality.max_rate particles a second: fewer live ones
				if emitter.emitter.rate > quality.max_rate {
					share = quality.max_rate / emitter.emitter.rate;
				}
			}
			Kind::Beam => {
				let Some(beam) = analysis.beams.get(&index) else { continue };
				let mut vertices = Vec::new();
				crate::render::beams::triangles(beam, camera.eye, back_axis, &mut vertices);
				triangles.extend(vertices.as_chunks::<3>().0.iter().map(|t| [Vec3::from(t[0].position), Vec3::from(t[1].position), Vec3::from(t[2].position)]));
			}
			Kind::Trail => {
				let Some(trail) = analysis.trails.get(&index) else { continue };
				let mut vertices = Vec::new();
				crate::render::trails::triangles(trail, camera.eye, &mut vertices);
				triangles.extend(vertices.as_chunks::<3>().0.iter().map(|t| [Vec3::from(t[0].position), Vec3::from(t[1].position), Vec3::from(t[2].position)]));
			}
			_ => continue,
		}
		let mut fragments = 0u64;
		let w = if item.kind == Kind::Emitter { weight::PARTICLE } else { weight::BLENDED };
		for triangle in triangles {
			raster(&view_projection, triangle, width, height, &layers.depth, &mut |pixel| {
				fragments += 1;
				out.layers[pixel] += 1;
				out.cost[pixel] += (w * share as f64) as f32;
			});
		}
		if fragments > 0 {
			let fragments = (fragments as f64 * share as f64).round() as u64;
			out.effect_blended.insert(index, fragments);
			out.effect_units.insert(index, w * fragments as f64 * per_pixel);
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_triangle_counts_where_it_lies_in_front() {
		let camera = Camera { eye: Vec3::new(0.0, 0.0, 10.0), world: glam::Affine3A::from_translation(Vec3::new(0.0, 0.0, 10.0)), fov: 90.0, near: 0.1 };
		let view = camera.view_projection(1.0);
		let (width, height) = (32u32, 32u32);
		// an opaque wall at distance 5 over the left half
		let wall = 0.1 / 5.0;
		let depth: Vec<f32> = (0..width * height).map(|index| if index % width < 16 { wall } else { 0.0 }).collect();
		// a quad over the whole view at distance 8: behind the wall, in front of the sky
		let quad = [[Vec3::new(-20.0, -20.0, 2.0), Vec3::new(20.0, -20.0, 2.0), Vec3::new(20.0, 20.0, 2.0)], [Vec3::new(-20.0, -20.0, 2.0), Vec3::new(20.0, 20.0, 2.0), Vec3::new(-20.0, 20.0, 2.0)]];
		let mut covered = vec![0u32; (width * height) as usize];
		for triangle in quad {
			raster(&view, triangle, width, height, &depth, &mut |pixel| covered[pixel] += 1);
		}
		let left: u32 = (0..height).flat_map(|y| (0..16).map(move |x| (y * width + x) as usize)).map(|index| covered[index]).sum();
		let right: u32 = (0..height).flat_map(|y| (16..32).map(move |x| (y * width + x) as usize)).map(|index| covered[index]).sum();
		assert_eq!(left, 0, "hidden by the wall");
		assert_eq!(right, 16 * 32, "every pixel of the right half once (the shared edge drawn once)");
	}

	#[test]
	fn lights_hold_what_their_range_and_cone_hold() {
		let point = LightVolume { index: 0, position: Vec3::ZERO, range: 10.0, cone: None };
		assert!(point.holds(Vec3::new(0.0, 9.0, 0.0)) && !point.holds(Vec3::new(0.0, 11.0, 0.0)));
		let spot = LightVolume { index: 0, position: Vec3::ZERO, range: 10.0, cone: Some((Vec3::NEG_Y, 45f32.to_radians().cos())) };
		assert!(spot.holds(Vec3::new(1.0, -5.0, 0.0)) && !spot.holds(Vec3::new(6.0, -5.0, 0.0)) && !spot.holds(Vec3::new(0.0, 5.0, 0.0)));
	}
}
