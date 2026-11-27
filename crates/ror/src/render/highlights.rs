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
			for item in draws.iter().filter(|item| !item.transparent && !item.decal) {
				pass.set_pipeline(pipelines.highlight_depth.get(item.double_sided));
				draw(&mut pass, item);
			}
			for &(index, highlight) in &prepared.draws {
				if prepared.styles[highlight].depth_mode == 1 {
					let item = &draws[index];
					pass.set_pipeline(pipelines.highlight_occluded.get(item.double_sided));
					draw(&mut pass, item);
				}
			}
		}
		for &(index, highlight) in &prepared.draws {
			if prepared.styles[highlight].depth_mode != 1 {
				let item = &draws[index];
				pass.set_pipeline(pipelines.highlight_on_top.get(item.double_sided));
				draw(&mut pass, item);
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

	/// The outline band is one layer of the fill and the outline (a Roblox client's cubes before walls
	/// of their own colour: red fill, green outline; vfx-highlight-edges: red fill, white outline).
	#[test]
	fn an_almost_transparent_outline_shows_the_fill_under_its_band() {
		let (w, h) = (6usize, 6usize);
		let mut mask = vec![0u8; 4 * w * h];
		for y in 2..10 {
			for x in 2..10 {
				mask[y * 2 * w + x] = 1;
			}
		}
		let run = |background: u8, fill: [f32; 3], fill_transparency: f32, outline: [f32; 3], outline_transparency: f32| {
			let style = Highlight { parts: vec![0], fill_color: glam::Vec3::from(fill), fill_transparency, outline_color: glam::Vec3::from(outline), outline_transparency, depth_mode: 0 };
			let mut rgba = vec![background; w * h * 4];
			for alpha in rgba.iter_mut().skip(3).step_by(4) {
				*alpha = 255;
			}
			composite(&mut rgba, w as u32, h as u32, &mask, std::slice::from_ref(&style));
			let at = |x: usize, y: usize| [rgba[(y * w + x) * 4], rgba[(y * w + x) * 4 + 1], rgba[(y * w + x) * 4 + 2]];
			// the band's pixel inside the edge and the one past it alike; the interior is the fill alone
			assert_eq!(at(1, 2), at(0, 2));
			(at(0, 2), at(2, 2))
		};
		let (red, green, white) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 1.0]);
		// a white outline at 0.9 and 0.99 over an opaque red fill: the outline's opacity's square root
		assert_eq!(run(0, red, 0.0, white, 0.9).0, [255, 81, 81]);
		assert_eq!(run(0, red, 0.0, white, 0.99).0, [255, 25, 25]);
		// the client's values over black, gray 70 and white 168 (within 1)
		let near = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 1);
		assert!(near(run(0, red, 0.25, green, 0.5).0, [110, 156, 0]), "{:?}", run(0, red, 0.25, green, 0.5).0);
		assert!(near(run(0, red, 0.5, green, 0.75).0, [90, 90, 0]));
		assert!(near(run(70, red, 0.5, green, 0.9).0, [149, 92, 35]), "{:?}", run(70, red, 0.5, green, 0.9).0);
		assert!(near(run(168, red, 0.75, green, 0.9).0, [176, 167, 127]), "{:?}", run(168, red, 0.75, green, 0.9).0);
		// a half-transparent fill under a half-transparent outline shows the outline alone, and no
		// outline leaves the fill on the band (and the interior to the fill)
		assert!(near(run(77, red, 0.5, green, 0.5).0, [38, 166, 38]));
		let (band, inside) = run(57, red, 0.5, green, 1.0);
		assert!(near(band, [156, 28, 28]) && band == inside, "{band:?} {inside:?}");
	}

	/// Measured on Roblox client 0.748: a camera inside a single-sided Part sees neither that part's
	/// Highlight (AlwaysOnTop or Occluded) nor Highlight occlusion from its back faces; a
	/// DoubleSided part draws its Highlight and occludes Highlights behind it from inside.
	#[test]
	fn camera_inside_single_sided_part_culls_highlight_and_highlight_occlusion() {
		use std::sync::Arc;

		use glam::{Affine3A, Vec3};

		use crate::render::camera::CameraSpec;
		use crate::render::{Background, Renderer, ViewRequest};
		use crate::scene::build::{ModelBuilder, PartSpec};

		let state = std::env::temp_dir().join(format!("ror-inside-highlight-test-{}", std::process::id()));
		let gpu = Arc::new(crate::render::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");

		let mut model = ModelBuilder::new("InsideHighlight");
		model.part(&PartSpec { name: "Enclosure".into(), size: Vec3::splat(4.0), color: [0, 0, 255], ..Default::default() });
		model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, -10.0)), size: Vec3::new(40.0, 40.0, 1.0), color: [255, 0, 0], ..Default::default() });
		let base = crate::scene::extract::extract(&model.into_document());
		let enclosure = base.parts.iter().position(|part| part.path.ends_with("Enclosure")).unwrap();
		let backdrop = base.parts.iter().position(|part| part.path.ends_with("Backdrop")).unwrap();

		let view = ViewRequest {
			width: 64,
			height: 64,
			msaa: 1,
			bloom: false,
			background: Background::Sky,
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, -10.0], fov: 70.0 },
			..Default::default()
		};
		let center = (32 * 64 + 32) * 4;
		let blue_highlight = |depth_mode: u32| Highlight {
			parts: vec![enclosure],
			fill_color: Vec3::new(0.0, 0.0, 1.0),
			fill_transparency: 0.0,
			outline_color: Vec3::new(0.0, 0.0, 1.0),
			outline_transparency: 1.0,
			depth_mode,
		};
		let cyan_occluded = Highlight {
			parts: vec![backdrop],
			fill_color: Vec3::new(0.0, 1.0, 1.0),
			fill_transparency: 0.0,
			outline_color: Vec3::new(0.0, 1.0, 1.0),
			outline_transparency: 1.0,
			depth_mode: 1,
		};

		// 1. Single-sided enclosure with AlwaysOnTop blue Highlight + Backdrop with Occluded cyan Highlight:
		//    enclosure's back faces must draw no blue Highlight and must NOT occlude Backdrop's Highlight.
		for depth_mode in [0u32, 1u32] {
			let mut scene = base.clone();
			scene.highlights = vec![blue_highlight(depth_mode), cyan_occluded.clone()];
			let (images, _) = renderer.render(&scene, std::slice::from_ref(&view)).expect("single-sided highlight render");
			let rgb = &images[0].rgba[center..center + 3];
			assert_eq!(rgb, [0, 255, 255], "depth_mode={depth_mode}: expected unoccluded cyan backdrop, got {rgb:?}");
		}

		// 2. DoubleSided enclosure with AlwaysOnTop blue Highlight: back faces DO draw pure blue.
		let mut double_hl = base.clone();
		Arc::make_mut(&mut double_hl.parts[enclosure]).double_sided = true;
		double_hl.highlights = vec![blue_highlight(0)];
		let (images, _) = renderer.render(&double_hl, std::slice::from_ref(&view)).expect("double-sided highlight render");
		assert_eq!(&images[0].rgba[center..center + 3], [0, 0, 255]);

		// 3. DoubleSided enclosure without Highlight in front of Backdrop with Occluded cyan Highlight:
		//    DoubleSided back faces DO occlude Backdrop's Highlight.
		let mut double_occ = base.clone();
		Arc::make_mut(&mut double_occ.parts[enclosure]).double_sided = true;
		double_occ.highlights = vec![cyan_occluded];
		let (images, _) = renderer.render(&double_occ, std::slice::from_ref(&view)).expect("double-sided occlusion render");
		let rgb = &images[0].rgba[center..center + 3];
		assert!(rgb[2] > 100 && rgb[1] < 80, "expected DoubleSided enclosure to occlude cyan backdrop highlight, got {rgb:?}");

		let _ = std::fs::remove_dir_all(&state);
	}

	/// Measured on Roblox client 0.748.0.7481526:
	/// - Built-in primitive `SpecialMesh`es (`Brick`, `Wedge`, `Sphere`, `Cylinder`, `Head`) apply
	///   `abs(Scale)`: negative axes never mirror them or turn them inside-out in main, Highlight,
	///   or `--ids` passes (an inner box stays hidden from outside, back faces stay culled inside).
	/// - `SpecialMesh` `FileMesh` applies signed `Scale` with standard back-face culling: odd
	///   negative axes invert triangle winding across main, Highlight, and `--ids` passes (front
	///   culled, back drawn).
	#[test]
	fn negative_scale_special_mesh_matches_roblox_culling() {
		use std::sync::Arc;

		use glam::{Affine3A, Vec3};
		use rbx_dom_weak::InstanceBuilder;
		use rbx_dom_weak::types::{ContentId, Enum, Vector3};

		use crate::render::camera::CameraSpec;
		use crate::render::{Background, Renderer, ViewRequest};
		use crate::scene::build::{ModelBuilder, PartSpec};

		let state = std::env::temp_dir().join(format!("ror-neg-scale-special-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(crate::render::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");

		let outside_view = ViewRequest {
			width: 64,
			height: 64,
			msaa: 1,
			bloom: false,
			ids: true,
			background: Background::Sky,
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, 10.0], target: [0.0, 0.0, 0.0], fov: 60.0 },
			..Default::default()
		};
		let inside_view = ViewRequest {
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: [0.0, 0.0, -10.0], fov: 60.0 },
			..outside_view.clone()
		};
		let center = 32 * 64 + 32;

		// 1. Primitive SpecialMesh Brick with Scale = (-1, 1, 1) around a yellow inner box:
		//    must stay right-side-out (outer blue face hides inner yellow box in RGBA, Highlight,
		//    and part_ids; from inside, back faces cull to show red wall).
		let mut model = ModelBuilder::new("NegPrimitive");
		let outer = model.part(&PartSpec { name: "Outer".into(), size: Vec3::splat(4.0), color: [0, 0, 255], ..Default::default() });
		model.child(
			outer,
			InstanceBuilder::new("SpecialMesh")
				.with_property("MeshType", Enum::from_u32(crate::scene::extract::mesh_type::BRICK))
				.with_property("Scale", Vector3::new(-1.0, 1.0, 1.0)),
		);
		model.part(&PartSpec { name: "Inner".into(), size: Vec3::splat(1.5), color: [255, 255, 0], ..Default::default() });
		model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, -15.0)), size: Vec3::new(60.0, 60.0, 1.0), color: [255, 0, 0], ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let outer_idx = scene.parts.iter().position(|p| p.path.ends_with("Outer")).unwrap();
		let wall_idx = scene.parts.iter().position(|p| p.path.ends_with("Wall")).unwrap();
		scene.highlights = vec![Highlight {
			parts: vec![outer_idx],
			fill_color: Vec3::new(0.0, 1.0, 0.0),
			fill_transparency: 0.0,
			outline_color: Vec3::new(0.0, 1.0, 0.0),
			outline_transparency: 1.0,
			depth_mode: 1, // Occluded
		}];
		let (images, _) = renderer.render(&scene, &[outside_view.clone(), inside_view]).expect("primitive render");
		assert_eq!(images[0].ids.as_ref().unwrap()[center], outer_idx as u32 + 1, "negative-scale primitive Brick must not cull its front face in part_ids");
		assert_eq!(&images[0].rgba[center * 4..center * 4 + 3], [0, 255, 0], "Occluded Highlight must cover Outer's front face (not Inner behind it)");
		assert_eq!(images[1].ids.as_ref().unwrap()[center], wall_idx as u32 + 1, "negative-scale primitive Brick must still cull back faces from inside");
		let in_rgb = &images[1].rgba[center * 4..center * 4 + 3];
		assert!(in_rgb[0] > 120 && in_rgb[1] < 40, "from inside, red wall should show through culled back faces, got {in_rgb:?}");

		// 2. Single-sided FileMesh quad (CCW facing +Z) with Scale = (-4, 4, 4) vs (4, 4, 4):
		//    signed Scale mirrors vertices and flips winding, so (-4, 4, 4) is culled from +Z
		//    in RGBA, Highlight, and part_ids, and visible from -Z.
		let quad_mesh = state.join("quad.mesh");
		let v = |x: i32, y: i32| format!("[{x},{y},0][0,0,1][0,0,0]");
		let faces = [(-1, -1), (1, -1), (1, 1), (-1, -1), (1, 1), (-1, 1)].map(|(x, y)| v(x, y)).concat();
		std::fs::write(&quad_mesh, format!("version 1.01\n2\n{faces}\n")).unwrap();
		let back_view = ViewRequest {
			camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0, 0.0, 0.0], fov: 60.0 },
			..outside_view.clone()
		};
		let mut fm = ModelBuilder::new("NegFileMesh");
		let q = fm.part(&PartSpec { name: "Quad".into(), size: Vec3::splat(4.0), color: [0, 0, 255], ..Default::default() });
		fm.child(
			q,
			InstanceBuilder::new("SpecialMesh")
				.with_property("MeshType", Enum::from_u32(crate::scene::extract::mesh_type::FILE_MESH))
				.with_property("MeshId", ContentId::from(quad_mesh.display().to_string()))
				.with_property("Scale", Vector3::new(-4.0, 4.0, 4.0)),
		);
		let mut fm_scene = crate::scene::extract::extract(&fm.into_document());
		let q_idx = fm_scene.parts.iter().position(|p| p.path.ends_with("Quad")).unwrap();
		fm_scene.highlights = vec![Highlight {
			parts: vec![q_idx],
			fill_color: Vec3::new(0.0, 1.0, 0.0),
			fill_transparency: 0.0,
			outline_color: Vec3::new(0.0, 1.0, 0.0),
			outline_transparency: 1.0,
			depth_mode: 0, // AlwaysOnTop
		}];
		let (images, _) = renderer.render(&fm_scene, &[outside_view, back_view]).expect("filemesh render");
		assert_eq!(images[0].ids.as_ref().unwrap()[center], 0, "FileMesh with odd negative scale must flip winding (culled from +Z)");
		assert_ne!(&images[0].rgba[center * 4..center * 4 + 3], [0, 255, 0], "culled front of negative-scale FileMesh must draw no Highlight");
		assert_eq!(images[1].ids.as_ref().unwrap()[center], q_idx as u32 + 1, "FileMesh with odd negative scale must draw from -Z in part_ids");
		assert_eq!(&images[1].rgba[center * 4..center * 4 + 3], [0, 255, 0], "FileMesh with odd negative scale must draw Highlight from -Z");

		let _ = std::fs::remove_dir_all(&state);
	}
}

/// What a Highlight draws on its outline band (the silhouette's rim and the pixel past it): one layer
/// of its fill and its outline, as opaque as the more opaque of the two, in their colours mixed in
/// linear light by the outline's share of that opacity (a Roblox client 0.747: cubes over black, gray
/// and white walls of their own colour, a red fill at FillTransparency 0-0.75 and a green outline at
/// OutlineTransparency 0-1, every band pixel within 1 of 255: fill 0.5 and outline 0.9 over 70 read
/// (149, 92, 35); vfx-highlight-edges, Studio and the client alike: a red fill under a white outline
/// at 0.9 / 0.99 shows (255, 81, 81) / (255, 25, 25), the outline's opacity's square root). Without
/// an outline (Transparency 1) the fill covers the band (vfx-highlight-many, highlights fill-blue); a
/// half-transparent fill under a half-transparent outline shows the outline alone (highlights half).
fn band(style: &Highlight) -> (glam::Vec3, f32) {
	let fill = 1.0 - style.fill_transparency;
	let line = (1.0 - style.outline_transparency).max(0.0);
	let opacity = fill.max(line);
	if opacity <= 0.0 {
		return (style.fill_color, 0.0);
	}
	let share = (line / opacity).clamp(0.0, 1.0);
	let mixed = (style.fill_color * style.fill_color).lerp(style.outline_color * style.outline_color, share);
	(glam::Vec3::new(mixed.x.sqrt(), mixed.y.sqrt(), mixed.z.sqrt()), opacity)
}

/// Blends the Highlights into the finished straight-alpha RGBA8 image (display values, as Studio:
/// highlights, fill-blue, half: a lerp toward the colour by 1 - Transparency inside, [`band`] on the
/// outline).
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
			if outline != 0 {
				let style = &styles[outline as usize - 1];
				let (color, opacity) = band(style);
				blend(pixel, color, opacity);
			} else if own != 0 {
				let style = &styles[own as usize - 1];
				blend(pixel, style.fill_color, 1.0 - style.fill_transparency);
			}
		}
	});
}
