//! Beams (notes/render.md "Beams"): a ribbon along a cubic Bézier curve between two attachments,
//! built on the CPU for every view (FaceCamera and ZOffset depend on the camera) and drawn with the
//! transparent geometry into the √-space scene target.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec3};

use super::camera::Camera;
use super::pipelines::{DEPTH_FORMAT, HDR_FORMAT, SAMPLE_COUNTS};
use super::resources::TextureUse;
use crate::scene::{Beam, Scene};

const COMMON: &str = include_str!("../../shaders/common.wgsl");
const BEAM: &str = include_str!("../../shaders/beam.wgsl");

/// An unlit beam shows Color² · Brightness at this radiance, like a sky texel (beam-shape: a white
/// Brightness-1 beam displays 223 of 255).
pub const BEAM_RADIANCE: f32 = 1.935;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct BeamVertex {
	pub position: [f32; 3],
	/// (u, v) times q, and q (the local width): u runs along the beam (the image's y), v across it
	/// (the image's x), projectively over each segment's trapezoid (beam-texture: taper)
	pub uvq: [f32; 3],
	/// sRGB Color and opacity (1 - Transparency) here
	pub color: [f32; 4],
	/// the ribbon's normal, for LightInfluence
	pub normal: [f32; 3],
	/// x emission (Brightness · BEAM_RADIANCE), y LightInfluence, z LightEmission, w unused
	pub params: [f32; 4],
}

impl BeamVertex {
	const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Float32x3, 4 => Float32x4];

	fn layout() -> wgpu::VertexBufferLayout<'static> {
		wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<BeamVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &Self::ATTRIBUTES }
	}
}

/// The beam pipelines (per MSAA sample count), their texture bind group layout and sampler.
pub struct BeamPipelines {
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
	pipelines: Vec<(u32, wgpu::RenderPipeline)>,
}

impl BeamPipelines {
	pub fn new(device: &wgpu::Device, frame_layout: &wgpu::BindGroupLayout, cache: Option<&wgpu::PipelineCache>) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("beam texture"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
					count: None,
				},
				wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
			],
		});
		// repeats both ways: the image's first row shows along the far edge (beam-texture: stretch-1);
		// trilinear without anisotropy: a texture squeezed across the beam blurs along it too
		// (beam-texture: grid)
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("beam"),
			address_mode_u: wgpu::AddressMode::Repeat,
			address_mode_v: wgpu::AddressMode::Repeat,
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("beam"), source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{BEAM}\n{}", super::pipelines::VOXEL_READ).into()) });
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("beam"), bind_group_layouts: &[frame_layout, &layout], push_constant_ranges: &[] });
		// premultiplied in √x; the alpha written is opacity · (1 - LightEmission): 0 adds the beam
		let blend = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add };
		let pipelines = SAMPLE_COUNTS
			.iter()
			.map(|&samples| {
				let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
					label: Some("beam"),
					layout: Some(&pipeline_layout),
					vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_beam"), buffers: &[BeamVertex::layout()], compilation_options: Default::default() },
					primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
					depth_stencil: Some(wgpu::DepthStencilState {
						format: DEPTH_FORMAT,
						depth_write_enabled: false,
						depth_compare: wgpu::CompareFunction::GreaterEqual,
						stencil: Default::default(),
						bias: Default::default(),
					}),
					multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
					fragment: Some(wgpu::FragmentState {
						module: &module,
						entry_point: Some("fs_beam"),
						targets: &[Some(wgpu::ColorTargetState { format: HDR_FORMAT, blend: Some(wgpu::BlendState { color: blend, alpha: blend }), write_mask: wgpu::ColorWrites::ALL })],
						compilation_options: Default::default(),
					}),
					multiview: None,
					cache,
				});
				(samples, pipeline)
			})
			.collect();
		Self { layout, sampler, pipelines }
	}

	pub fn pipeline(&self, samples: u32) -> &wgpu::RenderPipeline {
		&self.pipelines.iter().find(|(count, _)| *count == samples).unwrap_or(&self.pipelines[0]).1
	}

	/// A texture's bind group for beams and trails.
	pub fn texture_group(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
		device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("beam texture"),
			layout: &self.layout,
			entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
		})
	}
}

/// The box the vertices with LightInfluence span (beams and trails read the voxels where they are).
pub(super) fn lit_bounds(vertices: &[BeamVertex]) -> Option<(Vec3, Vec3)> {
	super::voxel_field::bounds(vertices.iter().filter(|vertex| vertex.params[1] > 0.0).map(|vertex| Vec3::from(vertex.position)), 0.0)
}

/// A view's beam geometry: one vertex buffer, each beam's vertex range and the point it sorts by.
pub struct ViewBeams {
	buffer: Option<wgpu::Buffer>,
	pub draws: Vec<BeamDraw>,
	/// The box where its beams with LightInfluence read the lights' voxels (render::voxel_field).
	pub lit: Option<(Vec3, Vec3)>,
}

pub struct BeamDraw {
	pub beam: usize,
	pub vertices: Range<u32>,
	pub center: Vec3,
}

impl super::Renderer {
	/// Each beam's texture bind group: white without a texture (a solid ribbon), `None` while its
	/// texture cannot be loaded, as Studio then draws no ribbon (texture-missing "beam-*": a texture
	/// missing, refused (HTTP 403) or no image).
	pub(super) fn beam_textures(&self, scene: &Scene, missing: &mut Vec<String>) -> Vec<Option<Arc<wgpu::BindGroup>>> {
		let mut groups: HashMap<Option<String>, Option<Arc<wgpu::BindGroup>>> = HashMap::new();
		scene
			.beams
			.iter()
			.map(|beam| {
				groups
					.entry(beam.texture.clone())
					.or_insert_with(|| {
						let texture = match beam.texture.as_ref().map(|uri| self.load_whole(&super::transcode::image_key(uri), TextureUse::Color)) {
							None => None,
							Some(Ok(texture)) => Some(texture),
							Some(Err(error)) => {
								missing.push(format!("{}: {error:#} (not drawn)", beam.path));
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

	/// The beams' triangles seen from `camera`.
	pub(super) fn beam_geometry(&self, scene: &Scene, camera: &Camera, lease: &super::reuse::Lease) -> ViewBeams {
		let mut vertices = Vec::new();
		let mut draws = Vec::new();
		let back = Vec3::from(camera.world.z_axis).normalize_or(Vec3::Z);
		for (index, beam) in scene.beams.iter().enumerate() {
			let start = vertices.len() as u32;
			let center = triangles(beam, camera.eye, back, &mut vertices);
			if vertices.len() as u32 > start {
				draws.push(BeamDraw { beam: index, vertices: start..vertices.len() as u32, center });
			}
		}
		let buffer = (!vertices.is_empty()).then(|| lease.buffer("beams", wgpu::BufferUsages::VERTEX, bytemuck::cast_slice(&vertices)));
		ViewBeams { buffer, draws, lit: lit_bounds(&vertices) }
	}

	/// Draws one beam of `view` into the main pass (the frame bind group is set).
	pub(super) fn draw_beam(&self, pass: &mut wgpu::RenderPass<'_>, view: &ViewBeams, draw: &BeamDraw, groups: &[Option<Arc<wgpu::BindGroup>>], samples: u32) {
		let (Some(buffer), Some(group)) = (&view.buffer, &groups[draw.beam]) else {
			return;
		};
		pass.set_pipeline(self.beams.pipeline(samples));
		pass.set_vertex_buffer(0, buffer.slice(..));
		pass.set_bind_group(1, group.as_ref(), &[]);
		pass.draw(draw.vertices.clone(), 0..1);
	}
}

/// The width direction a fraction `f` of the way from Attachment0 to Attachment1: the Y axis of
/// their rotations slerped, not a normalised lerp of the axes (beam-shape: twist), nor a slerp of
/// the Y axes alone (beam-misc: twist-diagonal, Attachment1 turned about the diagonal).
fn twist(rotation0: Quat, rotation1: Quat, f: f32) -> Vec3 {
	rotation0.slerp(rotation1, f) * Vec3::Y
}

/// An attachment's rotation (its CFrame may carry scale from an odd parent; only the axes count).
fn rotation(cframe: &glam::Affine3A) -> Quat {
	let m = cframe.matrix3;
	let x = Vec3::from(m.x_axis).normalize_or(Vec3::X);
	let y = Vec3::from(m.y_axis);
	let z = x.cross(y).normalize_or(Vec3::Z);
	Quat::from_mat3(&glam::Mat3::from_cols(x, z.cross(x), z))
}

fn bezier(p: [Vec3; 4], t: f32) -> Vec3 {
	let u = 1.0 - t;
	p[0] * (u * u * u) + p[1] * (3.0 * u * u * t) + p[2] * (3.0 * u * t * t) + p[3] * (t * t * t)
}

fn bezier_tangent(p: [Vec3; 4], t: f32) -> Vec3 {
	let u = 1.0 - t;
	(p[1] - p[0]) * (3.0 * u * u) + (p[2] - p[1]) * (6.0 * u * t) + (p[3] - p[2]) * (3.0 * t * t)
}

/// How many evenly spaced values a beam's Color and Transparency sequences are sampled into.
const SEQUENCE_SAMPLES: usize = 32;

/// A resampled sequence at `f` (0..1), linear between its samples.
fn resampled<T: Copy>(samples: &[T], f: f32, lerp: impl Fn(T, T, f32) -> T) -> T {
	let position = f.clamp(0.0, 1.0) * (samples.len() - 1) as f32;
	let index = (position.floor() as usize).min(samples.len() - 2);
	lerp(samples[index], samples[index + 1], position - index as f32)
}

/// The segments a beam is drawn with from `eye`: all of them while its farther attachment is within
/// 200 studs, then Segments · (1000 - d) / 800 rounded down (`super::distance_lod`), d that
/// attachment's distance, at least a tenth of them and at least one. Its Color and Transparency are
/// taken at those vertices only, so a beam transparent at both ends vanishes once it is down to one
/// segment (an effect's sky beams).
/// beam-lod-grid (faded beams, Segments 2 to 12, 40 to 2,350 studs; a beam's middle shows its count,
/// opaque for an even one, 1 - 1 / n for an odd one): every count, odd and even, where the rule puts
/// it, FaceCamera on or off, and a curved beam's polyline loses its corners alike; beam-lod-edges:
/// Segments 2, 3, 4 and 5 vanish between 199 and 202, 465 and 470, 597 and 603, 677 and 683 studs
/// (200, 467, 600 and 680 by the rule), at 2,000 studs Segments 18 is gone and 20 shows (the tenth),
/// beams reaching from 300 to 900 studs vanish whichever end is Attachment0 (the midpoint, 600, would
/// keep 2), and the same with the camera turned 25 degrees (the distance, not the depth);
/// beam-lod-few: a beam four times as wide alike.
pub fn lod_segments(segments: u32, start: Vec3, end: Vec3, eye: Vec3) -> usize {
	let distance = eye.distance(start).max(eye.distance(end));
	let factor = super::distance_lod(distance).max(0.1);
	((segments.clamp(1, 1024) as f32 * factor).floor() as usize).max(1)
}

/// Appends a beam's triangles (a list) seen from `eye` (`back`: the camera's backward axis); returns
/// the point it sorts by among transparent geometry: midway between its attachments, not the curve's
/// middle (beam-sort: a U-shaped beam sorts behind glass in front of its bulge), moved by ZOffset.
pub fn triangles(beam: &Beam, eye: Vec3, back: Vec3, out: &mut Vec<BeamVertex>) -> Vec3 {
	let (a0, a1) = (&beam.attachment0, &beam.attachment1);
	let start = Vec3::from(a0.translation);
	let end = Vec3::from(a1.translation);
	// P1 CurveSize0 along Attachment0's X axis, P2 CurveSize1 against Attachment1's (beam-shape:
	// curve); a straight beam is cut evenly (beam-color: gradient3-s10 keeps a vertex at 0.3)
	let control = if beam.curve_size0 == 0.0 && beam.curve_size1 == 0.0 {
		[start, start.lerp(end, 1.0 / 3.0), start.lerp(end, 2.0 / 3.0), end]
	} else {
		[start, start + Vec3::from(a0.matrix3.x_axis).normalize_or_zero() * beam.curve_size0, end - Vec3::from(a1.matrix3.x_axis).normalize_or_zero() * beam.curve_size1, end]
	};
	let (rotation0, rotation1) = (rotation(a0), rotation(a1));
	let y0 = rotation0 * Vec3::Y;
	let segments = lod_segments(beam.segments, start, end, eye);
	let points: Vec<Vec3> = (0..=segments).map(|index| bezier(control, index as f32 / segments as f32)).collect();
	// distance along the polyline from Attachment0
	let mut along = Vec::with_capacity(points.len());
	let mut length = 0.0;
	for (index, point) in points.iter().enumerate() {
		if index > 0 {
			length += point.distance(points[index - 1]);
		}
		along.push(length);
	}
	// a beam with both attachments on one point and no curve draws nothing (vfx-beam-zero: facing the
	// camera, flat and with its second attachment turned, where ror's rows took their directions from
	// rounding and drew a diamond or a pinwheel; an effect's chain; 0.001 studs apart Studio draws
	// a speck, a curved one its loop)
	if length < 1e-5 {
		return (start + end) * 0.5 + back * beam.z_offset;
	}
	let emission = beam.brightness.max(0.0) * BEAM_RADIANCE;
	let params = [emission, beam.light_influence.clamp(0.0, 1.0), beam.light_emission, 0.0];
	let texture_length = if beam.texture_length.abs() > 1e-6 { beam.texture_length } else { 1e-6 };
	let max_width = beam.width0.max(beam.width1);
	// Color and Transparency as Studio samples them: 32 values at k / 31, linear in between
	// (beam-sequence: keypoints between those times lose their corners)
	let colors: Vec<Vec3> = (0..SEQUENCE_SAMPLES).map(|k| beam.color_at(k as f32 / (SEQUENCE_SAMPLES - 1) as f32)).collect();
	let transparencies: Vec<f32> = (0..SEQUENCE_SAMPLES).map(|k| beam.transparency_at(k as f32 / (SEQUENCE_SAMPLES - 1) as f32)).collect();
	let mut rows = Vec::with_capacity(points.len());
	for (index, &point) in points.iter().enumerate() {
		let t = index as f32 / segments as f32;
		// width, twist, Color and Transparency follow the distance along the beam, not the curve's
		// parameter (beam-shape: pitch's twist; beam-color: curve-gradient)
		let f = if length > 1e-6 { along[index] / length } else { t };
		let mut tangent = bezier_tangent(control, t);
		if tangent.length_squared() < 1e-10 {
			// a zero CurveSize at an end: the direction to the neighbouring point
			tangent = if index + 1 < points.len() { points[index + 1] - point } else { point - points[index - 1] };
		}
		let tangent = tangent.normalize_or(Vec3::X);
		// the width runs along the attachments' Y axes, slerped, not made perpendicular to the curve
		// (beam-shape: pitch, twist); FaceCamera turns it across the view, v = 0 on the side of
		// (camera - point) x tangent (beam-texture: facecamera from the front, back and top)
		let side = if beam.face_camera { (eye - point).cross(tangent).normalize_or(y0) } else { twist(rotation0, rotation1, f) };
		let half_width = (beam.width0 + (beam.width1 - beam.width0) * f).max(0.0) * 0.5;
		let normal = tangent.cross(side).normalize_or(Vec3::Y);
		// ZOffset moves the whole beam towards the camera along its view axis, so it also grows on
		// screen (beam-order: zoffset-oblique, 3 studs at 42 studs is 7.5 % larger)
		let toward = |position: Vec3| position + back * beam.z_offset;
		// along the beam from Attachment0: Stretch repeats the image TextureLength times over the whole
		// length, Wrap and Static every TextureLength studs (beam-texture: stretch-3, wrap-3; Static
		// equals Wrap at rest, also when the beam is moved along its line); TextureSpeed moves the
		// image towards Attachment1 by its offset in repeats
		let u = match beam.texture_mode {
			0 => f * beam.texture_length,
			_ => along[index] / texture_length,
		} - beam.texture_offset;
		let color = resampled(&colors, f, |a, b, w| a.lerp(b, w));
		let opacity = 1.0 - resampled(&transparencies, f, |a, b, w| a + (b - a) * w).clamp(0.0, 1.0);
		let q = (half_width * 2.0).max(max_width * 1e-3).max(1e-6);
		let vertex = |position: Vec3, v: f32| BeamVertex { position: toward(position).into(), uvq: [u * q, v * q, q], color: [color.x, color.y, color.z, opacity], normal: normal.into(), params };
		rows.push((vertex(point + side * half_width, 0.0), vertex(point - side * half_width, 1.0)));
	}
	for pair in rows.windows(2) {
		let ((a, b), (c, d)) = (pair[0], pair[1]);
		out.extend_from_slice(&[a, b, d, a, d, c]);
	}
	(start + end) * 0.5 + back * beam.z_offset
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sequences_lose_corners_between_samples() {
		// a transparency spike down to 0 at 0.5 (1 at 0.45 and 0.55), between the samples at 15 / 31
		// and 16 / 31: Studio tops out at 0.318 (beam-sequence)
		let spike = |t: f32| ((t - 0.5).abs() * 20.0).min(1.0);
		let samples: Vec<f32> = (0..SEQUENCE_SAMPLES).map(|k| spike(k as f32 / (SEQUENCE_SAMPLES - 1) as f32)).collect();
		let lerp = |a: f32, b: f32, w: f32| a + (b - a) * w;
		let tip = resampled(&samples, 0.5, lerp);
		assert!((tip - 0.3226).abs() < 1e-3, "{tip}");
		assert_eq!(resampled(&samples, 0.0, lerp), 1.0);
		assert_eq!(resampled(&samples, 1.0, lerp), 1.0);
	}

	#[test]
	fn far_beams_lose_segments() {
		// beam-lod-edges: the last distance each count kept 2 segments at and the first it had one
		let eye = Vec3::ZERO;
		let at = |distance: f32| Vec3::new(0.0, 0.0, distance);
		for (segments, kept, lost) in [(2, 199.2, 202.1), (3, 465.0, 470.0), (4, 596.7, 603.3), (5, 676.7, 683.3)] {
			assert_eq!(lod_segments(segments, at(kept), at(10.0), eye), 2, "Segments {segments} at {kept}");
			assert_eq!(lod_segments(segments, at(10.0), at(lost), eye), 1, "Segments {segments} at {lost}");
		}
		// a tenth of them however far
		assert_eq!(lod_segments(20, at(2000.0), at(2000.0), eye), 2);
		assert_eq!(lod_segments(18, at(2000.0), at(2000.0), eye), 1);
		assert_eq!(lod_segments(10, at(150.0), at(190.0), eye), 10);
	}
}
