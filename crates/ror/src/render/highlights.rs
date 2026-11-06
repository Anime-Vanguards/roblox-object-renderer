//! Highlights (notes/render.md "Highlights"): a fill and an outline over the adornee's parts, blended
//! into the finished (display-space) image. The parts' ids are drawn at twice the output resolution;
//! a pixel belongs to a Highlight when two of its four samples do, the outline is the two-pixel band
//! across that mask's edge (a cross dilation minus a cross erosion), the fill the mask inside it.

use super::ids::{self, IdPipelines, MASK_FORMAT, Readback};
use crate::scene::Highlight;

/// What one view needs to draw the scene's Highlights.
pub struct PreparedHighlights {
	pub styles: Vec<Highlight>,
	ids: wgpu::BindGroup,
	/// (draw index, Highlight index) of every highlighted part's draw
	draws: Vec<(usize, usize)>,
}

impl PreparedHighlights {
	/// `part_draws`: each part's (instance, draw index), `None` for parts not drawn.
	pub fn new(device: &wgpu::Device, pipelines: &IdPipelines, highlights: &[Highlight], part_draws: &[Option<(u32, usize)>], instance_count: usize) -> Option<Self> {
		// ids are 8-bit; the scene keeps at most 255 anyway (scene::extract, as Studio draws)
		let highlights = &highlights[..highlights.len().min(255)];
		let mut ids = vec![0u32; instance_count.max(1)];
		let mut draws = Vec::new();
		for (index, highlight) in highlights.iter().enumerate() {
			for &part in &highlight.parts {
				if let Some(Some((instance, draw))) = part_draws.get(part) {
					ids[*instance as usize] = index as u32 + 1;
					draws.push((*draw, index));
				}
			}
		}
		if draws.is_empty() {
			return None;
		}
		let ids = pipelines.bind_group(device, &ids);
		Some(Self { styles: highlights.to_vec(), ids, draws })
	}
}

/// Encodes the id mask (twice `width` x `height`) and its copy into a readback buffer. `draws` are
/// the scene's draws: the opaque ones give the depth that Occluded Highlights test against.
pub fn encode_mask(
	device: &wgpu::Device,
	encoder: &mut wgpu::CommandEncoder,
	pipelines: &IdPipelines,
	prepared: &PreparedHighlights,
	frame_group: &wgpu::BindGroup,
	draws: &[super::DrawItem],
	width: u32,
	height: u32,
	profile: &super::profile::Profile,
) -> Readback {
	let (mask_width, mask_height) = (width * 2, height * 2);
	let (mask, mask_view, depth_view, readback) = ids::targets(device, "highlight mask", MASK_FORMAT, mask_width, mask_height);
	{
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("highlight mask"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: &mask_view,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
			})],
			depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
				view: &depth_view,
				depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
				stencil_ops: None,
			}),
			timestamp_writes: profile.pass("highlight mask"),
			occlusion_query_set: None,
		});
		pass.set_bind_group(0, frame_group, &[]);
		pass.set_bind_group(1, &prepared.ids, &[]);
		let draw = |pass: &mut wgpu::RenderPass, item: &super::DrawItem| {
			pass.set_vertex_buffer(0, item.mesh.vertex_buffer.slice(..));
			pass.set_index_buffer(item.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
			pass.draw_indexed(item.indices.clone(), 0, item.instance..item.instance + 1);
		};
		let occluded = prepared.draws.iter().any(|&(_, highlight)| prepared.styles[highlight].depth_mode == 1);
		if occluded {
			pass.set_pipeline(&pipelines.highlight_depth);
			for item in draws.iter().filter(|item| !item.transparent && !item.decal) {
				draw(&mut pass, item);
			}
			pass.set_pipeline(&pipelines.highlight_occluded);
			for &(index, highlight) in &prepared.draws {
				if prepared.styles[highlight].depth_mode == 1 {
					draw(&mut pass, &draws[index]);
				}
			}
		}
		pass.set_pipeline(&pipelines.highlight_on_top);
		for &(index, highlight) in &prepared.draws {
			if prepared.styles[highlight].depth_mode != 1 {
				draw(&mut pass, &draws[index]);
			}
		}
	}
	ids::copy_out(encoder, &mask, &readback, mask_width, mask_height);
	readback
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fill_inside_and_a_two_pixel_outline_across_the_edge() {
		// a 6 x 6 image; the mask (12 x 12) covers output pixels 1..=4 in both directions, and one
		// sample of four of pixel (5, 2) (outside)
		let (w, h) = (6usize, 6usize);
		let mut mask = vec![0u8; 4 * w * h];
		for y in 2..10 {
			for x in 2..10 {
				mask[y * 2 * w + x] = 1;
			}
		}
		mask[4 * 2 * w + 10] = 1;
		let style = Highlight { parts: vec![0], fill_color: glam::Vec3::new(1.0, 0.0, 0.0), fill_transparency: 0.5, outline_color: glam::Vec3::ONE, outline_transparency: 0.0, depth_mode: 0 };
		let mut rgba = vec![100u8; w * h * 4];
		for pixel in rgba.chunks_exact_mut(4) {
			pixel[3] = 255;
		}
		composite(&mut rgba, w as u32, h as u32, &mask, std::slice::from_ref(&style));
		let at = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
		// the mask's interior is filled: lerp(100, 255, 0.5) and lerp(100, 0, 0.5)
		assert_eq!(at(2, 2), [178, 50, 50]);
		// its edge pixels and the ones just outside them are the outline
		for (x, y) in [(1, 2), (0, 2), (4, 3), (5, 3), (5, 2), (2, 1), (2, 0), (3, 4), (3, 5)] {
			assert_eq!(at(x, y), [255, 255, 255], "({x}, {y})");
		}
		// a corner's diagonal neighbour is not (a cross kernel)
		assert_eq!(at(0, 0), [100, 100, 100]);
	}
}

/// Blends the Highlights into the finished straight-alpha RGBA8 image (display values, as Studio:
/// highlights, fill-blue, half: a lerp toward the colour by 1 - Transparency, the outline over the
/// unfilled image).
pub fn composite(rgba: &mut [u8], width: u32, height: u32, mask: &[u8], styles: &[Highlight]) {
	let (w, h) = (width as usize, height as usize);
	// the Highlight owning each output pixel: two of its four samples (highlights: the edge pixels
	// Studio counts in; 4-sample coverage over 0.3 fits best)
	use rayon::prelude::*;
	let mut owner = vec![0u8; w * h];
	// row by row on every core (each row its own: a 1080p frame's two passes took ~4 ms on one)
	owner.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, owner_row)| {
		for (x, owned) in owner_row.iter_mut().enumerate() {
			let row = 2 * w;
			let samples = [(2 * y) * row + 2 * x, (2 * y) * row + 2 * x + 1, (2 * y + 1) * row + 2 * x, (2 * y + 1) * row + 2 * x + 1].map(|index| mask[index]);
			let mut best = (0u8, 0usize);
			for &id in &samples {
				if id == 0 {
					continue;
				}
				let count = samples.iter().filter(|&&other| other == id).count();
				if count > best.1 {
					best = (id, count);
				}
			}
			if best.1 >= 2 {
				*owned = best.0;
			}
		}
	});
	let at = |x: isize, y: isize| -> u8 {
		if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
			return 0;
		}
		owner[y as usize * w + x as usize]
	};
	let blend = |pixel: &mut [u8], color: glam::Vec3, opacity: f32| {
		if opacity <= 0.0 {
			return;
		}
		let alpha = pixel[3] as f32 / 255.0;
		let total = opacity + alpha * (1.0 - opacity);
		for channel in 0..3 {
			let over = color[channel] * 255.0 * opacity + pixel[channel] as f32 * alpha * (1.0 - opacity);
			pixel[channel] = (over / total.max(1e-6)).round().clamp(0.0, 255.0) as u8;
		}
		pixel[3] = (total * 255.0).round().clamp(0.0, 255.0) as u8;
	};
	rgba.par_chunks_mut((w * 4).max(4)).take(h).enumerate().for_each(|(y, rgba_row)| {
		let y = y as isize;
		for x in 0..w as isize {
			let own = at(x, y);
			let neighbours = [at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1)];
			// the outline: the mask's edge pixels and the ones just outside them
			let outline = if own != 0 && neighbours.iter().any(|&other| other != own) {
				own
			} else {
				neighbours.iter().copied().find(|&other| other != 0 && other != own).unwrap_or(0)
			};
			let index = x as usize * 4;
			let pixel = &mut rgba_row[index..index + 4];
			// a Highlight whose outline is wholly transparent draws none, and its fill covers the band
			// the outline would have: the silhouette to its rim and one pixel beyond (vfx-highlight-many:
			// cubes filled red to the pixel past their rims, where ror left the band showing the cubes
			// and the sky; highlights fill-blue: the pixel outside the ball (35, 51, 246) in Studio, the
			// fill at 0.75 over the sky)
			let outlined = outline != 0 && styles[outline as usize - 1].outline_transparency < 1.0;
			if outlined {
				let style = &styles[outline as usize - 1];
				blend(pixel, style.outline_color, 1.0 - style.outline_transparency);
			} else if own != 0 || outline != 0 {
				let style = &styles[if own != 0 { own } else { outline } as usize - 1];
				blend(pixel, style.fill_color, 1.0 - style.fill_transparency);
			}
		}
	});
}
