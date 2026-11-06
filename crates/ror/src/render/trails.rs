//! Trails (notes/render.md "Trails"): the ribbon between the points a trail's attachments left,
//! built on the CPU for every view (FaceCamera depends on the camera) and drawn with the beams'
//! pipeline and shading into the √-space scene target.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use glam::Vec3;

use super::beams::{BEAM_RADIANCE, BeamVertex};
use super::camera::Camera;
use super::resources::TextureUse;
use crate::scene::particles::{color_at, number_at};
use crate::scene::{Scene, Trail};

/// How many evenly spaced values a trail's Color, Transparency and WidthScale are sampled into over
/// a point's life, as beams and particles do.
const SEQUENCE_SAMPLES: usize = 32;

/// A view's trail geometry: one vertex buffer, each trail's vertex range and the point it sorts by.
pub struct ViewTrails {
	buffer: Option<wgpu::Buffer>,
	pub draws: Vec<TrailDraw>,
	/// The box where its trails with LightInfluence read the lights' voxels (render::voxel_field).
	pub lit: Option<(Vec3, Vec3)>,
}

pub struct TrailDraw {
	pub trail: usize,
	pub vertices: Range<u32>,
	pub center: Vec3,
}

impl super::Renderer {
	/// Each trail's texture bind group: white without a texture, `None` while its texture cannot be
	/// loaded, as Studio then draws no trail (texture-missing "trail-*", as beams).
	pub(super) fn trail_textures(&self, scene: &Scene, missing: &mut Vec<String>) -> Vec<Option<Arc<wgpu::BindGroup>>> {
		let mut groups: HashMap<Option<String>, Option<Arc<wgpu::BindGroup>>> = HashMap::new();
		scene
			.trails
			.iter()
			.map(|trail| {
				groups
					.entry(trail.texture.clone())
					.or_insert_with(|| {
						let texture = match trail.texture.as_ref().map(|uri| self.load_whole(&super::transcode::image_key(uri), TextureUse::Color)) {
							None => None,
							Some(Ok(texture)) => Some(texture),
							Some(Err(error)) => {
								missing.push(format!("{}: {error:#} (not drawn)", trail.path));
								return None;
							}
						};
						let view = texture.as_ref().map_or_else(|| self.defaults.white.view.clone(), |texture| texture.view());
						Some(Arc::new(self.beams.texture_group(&self.gpu.device, &view)))
					})
					.clone()
			})
			.collect()
	}

	/// The trails' triangles seen from `camera`.
	pub(super) fn trail_geometry(&self, scene: &Scene, camera: &Camera, lease: &super::reuse::Lease) -> ViewTrails {
		let mut vertices = Vec::new();
		let mut draws = Vec::new();
		for (index, trail) in scene.trails.iter().enumerate() {
			let start = vertices.len() as u32;
			if let Some(center) = triangles(trail, camera.eye, &mut vertices)
				&& vertices.len() as u32 > start
			{
				draws.push(TrailDraw { trail: index, vertices: start..vertices.len() as u32, center });
			}
		}
		let buffer = (!vertices.is_empty()).then(|| lease.buffer("trails", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&vertices)));
		ViewTrails { buffer, draws, lit: super::beams::lit_bounds(&vertices) }
	}

	/// Draws one trail of `view` into the main pass (the frame bind group is set).
	pub(super) fn draw_trail(&self, pass: &mut wgpu::RenderPass<'_>, view: &ViewTrails, draw: &TrailDraw, groups: &[Option<Arc<wgpu::BindGroup>>], samples: u32) {
		let (Some(buffer), Some(group)) = (&view.buffer, &groups[draw.trail]) else {
			return;
		};
		pass.set_pipeline(self.beams.pipeline(samples));
		pass.set_vertex_buffer(0, buffer.slice(..));
		pass.set_bind_group(1, group.as_ref(), &[]);
		pass.draw(draw.vertices.clone(), 0..1);
	}
}

/// A sequence resampled into [`SEQUENCE_SAMPLES`] values, taken at `f` (0..1), linear in between.
fn resampled(at: impl Fn(f32) -> f32, f: f32) -> f32 {
	let position = f.clamp(0.0, 1.0) * (SEQUENCE_SAMPLES - 1) as f32;
	let index = (position.floor() as usize).min(SEQUENCE_SAMPLES - 2);
	let (a, b) = (at(index as f32 / (SEQUENCE_SAMPLES - 1) as f32), at((index + 1) as f32 / (SEQUENCE_SAMPLES - 1) as f32));
	a + (b - a) * (position - index as f32)
}

/// Appends a trail's triangles (a list) seen from `eye`; returns the point it sorts by among the
/// transparent geometry (the middle of its newest point), `None` without at least two points.
pub fn triangles(trail: &Trail, eye: Vec3, out: &mut Vec<BeamVertex>) -> Option<Vec3> {
	let points = &trail.points;
	if points.len() < 2 || !trail.lifetime.is_finite() {
		return None;
	}
	let lifetime = trail.lifetime.max(1e-6);
	let middles: Vec<Vec3> = points.iter().map(|point| (point.position0 + point.position1) * 0.5).collect();
	// distance along the trail from its newest point (the head) and from its oldest
	let mut from_head = vec![0.0f32; points.len()];
	for index in (0..points.len() - 1).rev() {
		from_head[index] = from_head[index + 1] + middles[index].distance(middles[index + 1]);
	}
	let length = from_head[0];
	let texture_length = if trail.texture_length.abs() > 1e-6 { trail.texture_length } else { 1e-6 };
	let emission = trail.brightness.max(0.0) * BEAM_RADIANCE;
	let params = [emission, trail.light_influence.clamp(0.0, 1.0), trail.light_emission, 0.0];
	let mut rows = Vec::with_capacity(points.len());
	for (index, point) in points.iter().enumerate() {
		let life = ((trail.time - point.time) / lifetime).clamp(0.0, 1.0);
		let width_scale = resampled(|t| number_at(&trail.width_scale, t, &[]), life);
		let transparency = resampled(|t| number_at(&trail.transparency, t, &[]), life);
		let color = {
			let position = life * (SEQUENCE_SAMPLES - 1) as f32;
			let sample = (position.floor() as usize).min(SEQUENCE_SAMPLES - 2);
			let (a, b) = (color_at(&trail.color, sample as f32 / (SEQUENCE_SAMPLES - 1) as f32), color_at(&trail.color, (sample + 1) as f32 / (SEQUENCE_SAMPLES - 1) as f32));
			a.lerp(b, position - sample as f32)
		};
		let center = middles[index];
		let tangent = {
			let previous = middles[index.saturating_sub(1)];
			let next = middles[(index + 1).min(points.len() - 1)];
			(next - previous).normalize_or(Vec3::X)
		};
		let across = (point.position0 - point.position1) * 0.5;
		// FaceCamera: the attachments' distance across the view; else between the attachments
		let half = if trail.face_camera {
			(eye - center).cross(tangent).normalize_or(across.normalize_or(Vec3::Y)) * across.length()
		} else {
			across
		} * width_scale.max(0.0);
		let normal = tangent.cross(half).normalize_or(Vec3::Y);
		// the image runs from the head towards the tail (trail-texture: the arrow pointer's tiles)
		let u = match trail.texture_mode {
			// Stretch: TextureLength times over the whole trail (stretch-1, stretch-2)
			0 => if length > 1e-6 { from_head[index] / length * trail.texture_length } else { 0.0 },
			// Static: every TextureLength studs, fixed where the trail began (static-3: the tiles
			// start at the tail, a third of a tile on at the head after 8 studs, still and moving)
			2 => (from_head[index] - trail.dropped_length - length) / texture_length,
			// Wrap: every TextureLength studs from the head (wrap-3)
			_ => from_head[index] / texture_length,
		};
		let opacity = 1.0 - transparency.clamp(0.0, 1.0);
		let q = (half.length() * 2.0).max(1e-6);
		let vertex = |position: Vec3, v: f32| BeamVertex { position: position.into(), uvq: [u * q, v * q, q], color: [color.x, color.y, color.z, opacity], normal: normal.into(), params };
		rows.push((vertex(center + half, 0.0), vertex(center - half, 1.0)));
	}
	for (index, pair) in rows.windows(2).enumerate() {
		// a strip begins after the trail was enabled again: no segment joins it to the one before
		if points[index + 1].starts_strip {
			continue;
		}
		let ((a, b), (c, d)) = (pair[0], pair[1]);
		out.extend_from_slice(&[a, b, d, a, d, c]);
	}
	Some(middles[middles.len() - 1])
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::trails::TrailPoint;

	fn trail(points: Vec<TrailPoint>) -> Trail {
		Trail {
			path: "Test.Trail".into(),
			referent: rbx_dom_weak::types::Ref::none(),
			attachment0: rbx_dom_weak::types::Ref::none(),
			attachment1: rbx_dom_weak::types::Ref::none(),
			cframe0: glam::Affine3A::IDENTITY,
			cframe1: glam::Affine3A::IDENTITY,
			color: vec![(0.0, Vec3::ONE)],
			transparency: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
			width_scale: vec![(0.0, 1.0, 0.0), (1.0, 1.0, 0.0)],
			brightness: 1.0,
			light_emission: 0.0,
			light_influence: 0.0,
			lifetime: 10.0,
			min_length: 0.0,
			max_length: 0.0,
			face_camera: false,
			enabled: true,
			texture: None,
			texture_length: 1.0,
			texture_mode: 0,
			points,
			time: 1.0,
			dropped_length: 0.0,
		}
	}

	#[test]
	fn ribbon_spans_the_attachments() {
		let point = |time: f32, x: f32| TrailPoint { time, position0: Vec3::new(x, 0.5, 0.0), position1: Vec3::new(x, -0.5, 0.0), starts_strip: time == 0.0 };
		let trail = trail(vec![point(0.0, 0.0), point(0.5, 4.0), point(1.0, 8.0)]);
		let mut vertices = Vec::new();
		assert!(triangles(&trail, Vec3::new(0.0, 0.0, -20.0), &mut vertices).is_some());
		assert_eq!(vertices.len(), 12);
		let ys: Vec<f32> = vertices.iter().map(|vertex| vertex.position[1]).collect();
		assert!(ys.iter().all(|y| (y.abs() - 0.5).abs() < 1e-5), "{ys:?}");
	}
}
