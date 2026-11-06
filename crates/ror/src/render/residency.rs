//! Texture residency: a material map kept on disk (render::texture_files) goes to the GPU from the
//! finest level a render's views sample, not whole. Before the views draw, a pass of their own draws
//! every surface each view draws (mesh.wgsl `fs_residency`), rasterised as the view's passes that read
//! material maps rasterise them: the main pass's size and samples and, for the parts that write depth
//! by their texels' alpha, the scene depth's single sample and the ambient occlusion's prepass. Each
//! fragment records, per map, the finest whole level `studio_mips` picks there; the map then goes up
//! from [`MARGIN`] levels finer than the finest any fragment picked (Vulkan lets the GPU's
//! anisotropic footprint, its axes measured roughly and their ratio rounded up, sample up to 1.5
//! levels finer than the LOD the gradients give). A texture held from level b samples as the full
//! texture does at every level from b on: its sizes halve evenly down to b
//! (`texture_files::max_base`) and the shader picks levels by the full size (mesh.wgsl
//! MaterialInfo). A map no fragment samples goes up from its coarsest allowed level. A map's GPU copy
//! only gains levels: a render that needs more than an earlier one reads the rest, and what was read
//! stays for every later render the texture cache keeps it for.
//!
//! A large place seen whole (1920 x 1080, iso) samples 100 MB of its 1,317 maps' 1.46 GB: peak commit
//! 3.7-3.8 GB -> 1.6 GB, the job 811-927 -> 513-601 ms; a 30-frame orbit of it 4.1 -> 2.05 GB. Views from inside a map sample nearly all of theirs
//! (a map's 32 gameplay views: 181 of 197 MB), where the feedback's wait for the GPU costs
//! more than it saves: an estimate as the parts are prepared ([`worth_feedback`]) sends their maps
//! up whole then, and a feedback that finds less than [`FEEDBACK_FROM`] bytes to save reads them
//! whole too (notes/render.md "Texture residency").

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use parking_lot::Mutex;

use super::camera::Camera;
use super::pipelines::DEPTH_FORMAT;
use super::resources::GpuTexture;
use super::{DrawItem, MaterialBinding, Prepared, Renderer, ViewRequest};
use crate::scene::Scene;

/// How many levels finer than the finest a view's fragments pick a map goes up from.
pub(super) const MARGIN: u32 = 2;

/// Below this many bytes the views' maps could save, they are read whole without the feedback
/// (`Renderer::residency_from`).
pub(super) const FEEDBACK_FROM: u64 = 32 << 20;

/// The feedback's slots: the next never taken, and those given back.
static IDS: Mutex<(u32, Vec<u32>)> = Mutex::new((1, Vec::new()));

/// A slot in the residency feedback for a texture whose GPU copy may start below its finest level.
pub(super) fn take_id() -> u32 {
	let mut ids = IDS.lock();
	if let Some(id) = ids.1.pop() {
		return id;
	}
	ids.0 += 1;
	ids.0 - 1
}

pub(super) fn give_back_id(id: u32) {
	IDS.lock().1.push(id);
}

/// `ROR_RESIDENCY=off` holds every material map whole (the levels a view reads, measured).
fn enabled() -> bool {
	static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ENABLED.get_or_init(|| !std::env::var("ROR_RESIDENCY").is_ok_and(|value| value.eq_ignore_ascii_case("off") || value == "0"))
}

/// One way a view's passes rasterise draws that read material maps: their target's size and samples.
pub(super) struct Raster<'a> {
	pub width: u32,
	pub height: u32,
	pub samples: u32,
	pub draws: Vec<&'a DrawItem>,
}

/// How a view draws the parts that read material maps: its camera and size, and its rasters (more
/// than it may draw: what more covers only reads more levels).
pub(super) struct ViewPlan<'a> {
	camera: Camera,
	render: (u32, u32),
	rasters: Vec<Raster<'a>>,
}

/// What a clip's residency feedback has seen while its camera holds still: the view it drew (camera,
/// sizes, rasters) and the prepared parts that went through it. A later frame under the same view finds
/// most parts unchanged, the same preparation (render::prepared keeps an unchanged part's): its draws
/// make the fragments they made then, each picking the levels it picked, and a map's levels only
/// grow, so it needs no level it does not hold. The frame then skips the feedback and its wait for the
/// GPU (a 1440p map sim's frame waited 5.7 ms for it, every frame); the maps of the parts it has not
/// seen (moved, posed, new) are read whole.
#[derive(Default)]
pub(super) struct ClipResidency {
	seen: Mutex<Option<Seen>>,
}

struct Seen {
	view: Vec<u32>,
	/// By address, held so that no other part takes its address meanwhile
	parts: HashMap<usize, Arc<super::prepared::PreparedPart>>,
}

/// The views' cameras, sizes and rasters, as bits.
fn view_key(plans: &[ViewPlan]) -> Vec<u32> {
	let mut key = Vec::new();
	for plan in plans {
		let camera = &plan.camera;
		key.extend(camera.eye.to_array().map(f32::to_bits));
		key.extend(camera.world.to_cols_array().map(f32::to_bits));
		key.extend([camera.fov.to_bits(), camera.near.to_bits(), plan.render.0, plan.render.1, plan.rasters.len() as u32]);
		for raster in &plan.rasters {
			key.extend([raster.width, raster.height, raster.samples]);
		}
	}
	key
}

/// The index of `draw` among `prepared`'s draws (each raster's draws borrow them).
fn draw_index(prepared: &Prepared, draw: &DrawItem) -> usize {
	(std::ptr::from_ref(draw) as usize - prepared.draws.as_ptr() as usize) / std::mem::size_of::<DrawItem>()
}

impl ClipResidency {
	/// Whether an earlier frame's feedback is kept (its view may be this frame's).
	pub(super) fn has_seen(&self) -> bool {
		self.seen.lock().is_some()
	}

	/// The maps of the plans' draws whose parts the feedback has not seen under this view, or `None`
	/// when it drew another view (or none).
	fn unseen_maps(&self, prepared: &Prepared, plans: &[ViewPlan]) -> Option<std::collections::HashSet<u32>> {
		let seen = self.seen.lock();
		let seen = seen.as_ref().filter(|seen| seen.view == view_key(plans))?;
		let mut maps = std::collections::HashSet::new();
		for draw in plans.iter().flat_map(|plan| plan.rasters.iter().flat_map(|raster| &raster.draws)) {
			let part = &prepared.parts[prepared.draw_parts[draw_index(prepared, draw)] as usize];
			if !seen.parts.contains_key(&(Arc::as_ptr(part) as usize)) {
				maps.extend(draw.material.maps.iter().flatten().filter(|map| map.id != 0).map(|map| map.id));
			}
		}
		Some(maps)
	}

	/// The plans' parts have gone through a feedback under their view (or `keep` is off: the next
	/// frame draws another view, and nothing is kept).
	fn saw(&self, prepared: &Prepared, plans: &[ViewPlan], keep: bool) {
		let mut seen = self.seen.lock();
		if !keep {
			*seen = None;
			return;
		}
		let mut parts = HashMap::new();
		for draw in plans.iter().flat_map(|plan| plan.rasters.iter().flat_map(|raster| &raster.draws)) {
			let part = &prepared.parts[prepared.draw_parts[draw_index(prepared, draw)] as usize];
			parts.entry(Arc::as_ptr(part) as usize).or_insert_with(|| part.clone());
		}
		*seen = Some(Seen { view: view_key(plans), parts });
	}
}

/// The material bind groups of a render's draws, with the levels their maps hold now.
pub(super) struct ViewGroups {
	groups: HashMap<usize, wgpu::BindGroup>,
}

impl ViewGroups {
	/// The bind group of a material the views draw.
	pub fn of(&self, material: &MaterialBinding) -> &wgpu::BindGroup {
		&self.groups[&(std::ptr::from_ref(material) as usize)]
	}
}

/// What a render's residency did: its time, the feedback's, and the maps it read and their bytes.
#[derive(Default)]
pub(super) struct Report {
	pub ms: f64,
	pub feedback_ms: f64,
	pub read: usize,
	pub read_bytes: u64,
}

/// Whether the views could sample much less of their material maps than whole: an estimate from
/// the sphere round what each draw draws, seen from each view (a mesh's or decal's map spread once
/// across it, a material's tiled by its studs per tile), [`MARGIN`] levels and one more finer than
/// that; maps no view sees save all. Else the maps go up whole as the parts are prepared, without
/// the feedback's wait (a map's gameplay views and clips sample 92 % of theirs), as the meshes
/// do (read at the views instead, after the other targets and buffers, they took GPU memory blocks
/// of their own: 40-60 MB more).
pub(super) fn worth_feedback(draws: &[DrawItem], instances: &[super::InstanceData], views: &[ViewRequest], bounds: Option<(glam::Vec3, glam::Vec3)>, from: u64) -> bool {
	// views under levels of detail draw other triangles than a feedback would see: maps whole
	if !enabled() || views.is_empty() || views.iter().any(|view| view.lod.is_some()) {
		return false;
	}
	// per map: its whole bytes, coarsest allowed level and the finest level estimated
	let mut finest: HashMap<u32, (u64, u32, u32)> = HashMap::new();
	for draw in draws {
		for map in draw.material.maps.iter().flatten().filter(|map| map.id != 0 && map.held().is_none_or(|held| held.base > 0)) {
			finest.entry(map.id).or_insert((map.full_bytes, map.max_base(), u32::MAX));
		}
	}
	for view in views {
		let (width, height) = (view.width.max(1), view.height.max(1));
		let camera = view.camera.place(bounds, width, height);
		let focal = (height * view.supersample.clamp(1, 4)) as f32 * 0.5 / (camera.fov.to_radians() * 0.5).tan();
		let frustum = super::culling::Frustum::new(camera.view_projection(width as f32 / height as f32));
		for draw in draws.iter().filter(|draw| draw.in_frustum(&frustum)) {
			// what it draws (a decal's sphere is its part's centre alone)
			let (center, radius) = draw.drawn_bounds.sphere();
			let distance = (center.distance(camera.eye) - radius).max(camera.near);
			let instance = &instances[draw.instance as usize];
			let tiled = instance.flags[1] & super::TEX_MATERIAL != 0;
			for map in draw.material.maps.iter().flatten() {
				let Some(entry) = finest.get_mut(&map.id) else { continue };
				let side = map.width.max(map.height) as f32;
				let texels_per_pixel = if radius <= 0.0 {
					0.0
				} else if tiled && !draw.decal {
					side / instance.size[3].max(1e-3) * distance / focal
				} else {
					side * distance / (2.0 * radius * focal)
				};
				let level = if texels_per_pixel > 1.0 { texels_per_pixel.log2().floor() as u32 } else { 0 };
				entry.2 = entry.2.min(level);
			}
		}
	}
	let total: u64 = finest.values().map(|(bytes, ..)| bytes).sum();
	let saved: u64 = finest
		.values()
		.map(|&(bytes, max_base, level)| {
			let base = if level == u32::MAX { max_base } else { level.saturating_sub(MARGIN + 1).min(max_base) };
			bytes - (bytes >> (2 * base.min(16)))
		})
		.sum();
	saved >= from && saved * 4 >= total
}

impl Renderer {
	/// How `request` draws `prepared` (as render_view does: its camera, frustum, size and samples).
	pub(super) fn residency_plan<'a>(&self, scene: &Scene, prepared: &'a Prepared, request: &ViewRequest) -> ViewPlan<'a> {
		let supersample = request.supersample.clamp(1, 4);
		let samples = if request.msaa >= 4 { 4 } else { 1 };
		let (width, height) = (request.width.max(1), request.height.max(1));
		let render = (width * supersample, height * supersample);
		let camera = request.camera.place(prepared.bounds, width, height);
		let frustum = super::culling::Frustum::new(camera.view_projection(width as f32 / height as f32));
		let draws: Vec<&DrawItem> = prepared.draws.iter().filter(|draw| draw.in_frustum(&frustum)).collect();
		let mut rasters = Vec::new();
		// multisampled, the parts drawn by their texels' alpha also go into the single-sampled scene
		// depth and the occlusion's prepass
		let by_texels: Vec<&DrawItem> = draws.iter().copied().filter(|draw| draw.write_depth).collect();
		if samples > 1 && !by_texels.is_empty() {
			if super::ambient_occlusion_enabled() && scene.environment.viewport.is_none() {
				let ((width, height), samples) = super::ao::AoTargets::size(render.0, render.1);
				rasters.push(Raster { width, height, samples, draws: by_texels.iter().copied().filter(|draw| !draw.highlight_only).collect() });
			}
			rasters.push(Raster { width: render.0, height: render.1, samples: 1, draws: by_texels });
		}
		rasters.push(Raster { width: render.0, height: render.1, samples, draws });
		ViewPlan { camera, render, rasters }
	}

	/// The bind groups of the materials `plans` draw, their maps' levels read first: from [`MARGIN`]
	/// levels finer than the finest any fragment of the views picks, or whole where that would save
	/// little.
	pub(super) fn view_groups(&self, prepared: &Prepared, plans: &[ViewPlan], report: &mut Report, clip: Option<(&ClipResidency, bool)>, whole: bool) -> Result<ViewGroups> {
		let start = Instant::now();
		let mut materials: Vec<&MaterialBinding> = Vec::new();
		let mut seen = std::collections::HashSet::new();
		for draw in plans.iter().flat_map(|plan| plan.rasters.iter().flat_map(|raster| &raster.draws)) {
			if seen.insert(Arc::as_ptr(&draw.material) as usize) {
				materials.push(&draw.material);
			}
		}
		// each map that may still read levels once: held from past its finest, or not yet
		let mut maps: Vec<&Arc<GpuTexture>> = Vec::new();
		let mut ids = std::collections::HashSet::new();
		let mut slots = 0;
		for material in &materials {
			for map in material.maps.iter().flatten().filter(|map| map.id != 0) {
				slots = slots.max(map.id + 1);
				if map.held().is_none_or(|held| held.base > 0) && ids.insert(map.id) {
					maps.push(map);
				}
			}
		}
		if !maps.is_empty() {
			let from = self.residency_from.load(std::sync::atomic::Ordering::Relaxed);
			let open: u64 = maps.iter().map(|map| map.full_bytes.saturating_sub(map.bytes())).sum();
			// a clip's frame under the view of an earlier one's feedback: the parts it saw need nothing more
			// (ClipResidency), the others' maps go up whole
			let unseen = clip.filter(|_| enabled() && !whole && open >= from).and_then(|(clip, _)| clip.unseen_maps(prepared, plans));
			let bases: Vec<u32> = if let Some(unseen) = unseen {
				maps.iter().map(|map| if unseen.contains(&map.id) { 0 } else { map.held().map_or(0, |held| held.base) }).collect()
			} else if enabled() && !whole && open >= from {
				let start = Instant::now();
				let codes = self.residency_feedback(prepared, plans, slots)?;
				report.feedback_ms += start.elapsed().as_secs_f64() * 1000.0;
				if let Some((clip, keep)) = clip {
					clip.saw(prepared, plans, keep);
				}
				let bases: Vec<u32> = maps
					.iter()
					.map(|map| match codes.get(map.id as usize).copied().unwrap_or(0) {
						// sampled by no fragment: from its coarsest
						0 => map.max_base(),
						code => (32 - code).saturating_sub(MARGIN).min(map.max_base()),
					})
					.collect();
				// what the levels from those leave out, about (a level a quarter of the one before)
				let saved: u64 = maps.iter().zip(&bases).map(|(map, &base)| map.full_bytes - (map.full_bytes >> (2 * base.min(16)))).sum();
				// little to save: whole, at once (read a second time later, a map's finer levels took GPU
				// memory blocks of their own, which stay: a map's frames, 64 MB more)
				if saved < from { vec![0; maps.len()] } else { bases }
			} else {
				vec![0; maps.len()]
			};
			let room = |bytes: u64| self.gpu_room(bytes);
			let wanted: Vec<(&Arc<GpuTexture>, u32)> = maps.into_iter().zip(bases).filter(|(map, base)| map.held().is_none_or(|held| held.base > *base)).collect();
			let before: u64 = wanted.iter().map(|(map, _)| map.bytes()).sum();
			self.staging.read(&self.gpu, &wanted, &room)?;
			report.read += wanted.len();
			report.read_bytes += wanted.iter().map(|(map, _)| map.bytes()).sum::<u64>().saturating_sub(before);
		}
		let groups = materials.into_iter().map(|material| (std::ptr::from_ref(material) as usize, self.material_group(material))).collect();
		report.ms += start.elapsed().as_secs_f64() * 1000.0;
		Ok(ViewGroups { groups })
	}

	/// Draws every view's rasters through the residency feedback and reads back each slot's code
	/// (mesh.wgsl `fs_residency`: 32 minus the finest level a fragment picks, 0 for none).
	fn residency_feedback(&self, prepared: &Prepared, plans: &[ViewPlan], slots: u32) -> Result<Vec<u32>> {
		let device = &self.gpu.device;
		let size = slots.max(1) as u64 * 4;
		let buffer = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("residency"),
			size,
			usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
			mapped_at_creation: false,
		});
		let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("residency readback"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
		let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("residency"),
			layout: &self.pipelines.residency_layout,
			entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
		});
		// its depth targets of the kinds the views' passes take next: given back once it is done (the
		// lease's drop, after the wait below)
		let lease = self.reuse.lease(&self.gpu);
		// made the first time, kept in the driver's cache for the next process (render::gpu: made anew,
		// six of them took 60 ms of each process's first feedback)
		let mut made = false;
		for plan in plans {
			for raster in &plan.rasters {
				if !self.pipelines.residency_made(raster.samples) {
					let _ = self.pipelines.residency(device, raster.samples);
					made = true;
				}
			}
		}
		if made {
			self.gpu.save_pipeline_cache();
		}
		let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("residency") });
		encoder.clear_buffer(&buffer, 0, None);
		for plan in plans {
			let frame = self.frame_uniform(Some(&plan.camera), plan.render.0, plan.render.1, 0);
			let frame_buffer = lease.buffer("frame", wgpu::BufferUsages::UNIFORM, bytemuck::bytes_of(&frame));
			let frame_group = self.frame_bind_group(prepared, &frame_buffer, &self.no_light_bindings(prepared), &self.defaults.empty_moments, &self.defaults.empty_shadow, &self.defaults.white.view, &self.defaults.black.view);
			for raster in plan.rasters.iter().filter(|raster| !raster.draws.is_empty()) {
				let depth = lease.scratch(&wgpu::TextureDescriptor {
					label: Some("depth"),
					size: wgpu::Extent3d { width: raster.width, height: raster.height, depth_or_array_layers: 1 },
					mip_level_count: 1,
					sample_count: raster.samples,
					dimension: wgpu::TextureDimension::D2,
					format: DEPTH_FORMAT,
					usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
					view_formats: &[],
				});
				let view = depth.create_view(&Default::default());
				let pipelines = self.pipelines.residency(device, raster.samples);
				{
					let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
						label: Some("residency"),
						color_attachments: &[],
						depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
						timestamp_writes: None,
						occlusion_query_set: None,
					});
					pass.set_bind_group(0, &frame_group, &[]);
					pass.set_bind_group(2, &group, &[]);
					pass.set_pipeline(&pipelines[0]);
					let mut runs = super::runs::Runs::new(1);
					for draw in raster.draws.iter().filter(|draw| !draw.decal) {
						runs.draw(&mut pass, &draw.mesh.vertex_buffer, &draw.mesh.index_buffer, draw.indices.clone(), draw.instance, Some(&draw.material.feedback));
					}
					runs.finish(&mut pass);
					let mut set = None;
					for draw in raster.draws.iter().filter(|draw| draw.decal) {
						let bind_pose = draw.mesh.bind.is_some();
						if set != Some(bind_pose) {
							pass.set_pipeline(&pipelines[if bind_pose { 2 } else { 1 }]);
							set = Some(bind_pose);
						}
						pass.set_bind_group(1, &draw.material.feedback, &[]);
						pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
						if let Some(bind) = &draw.mesh.bind {
							pass.set_vertex_buffer(1, bind.slice(..));
						}
						pass.set_index_buffer(draw.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
						draw.draw_indexed(&mut pass);
					}
				}
				// the next raster of its kind draws into it (one encoder: wgpu orders them)
				lease.release(&depth);
			}
		}
		encoder.copy_buffer_to_buffer(&buffer, 0, &readback, 0, size);
		let submission = self.gpu.submit([encoder.finish()]);
		let (sender, ready) = std::sync::mpsc::channel();
		readback.slice(..).map_async(wgpu::MapMode::Read, move |result| {
			let _ = sender.send(result);
		});
		self.gpu.wait_mapped(submission, &ready)?.map_err(|error| anyhow!("residency readback: {error}"))?;
		let codes = bytemuck::cast_slice::<u8, u32>(&readback.slice(..).get_mapped_range()).to_vec();
		readback.unmap();
		drop(lease);
		Ok(codes)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::render::resources::TextureUse;
	use crate::render::{CameraSpec, content_key, texture_files, transcode};

	/// A decal's image held from a coarser level (what a view far from it samples) draws exactly as
	/// the whole image does.
	#[test]
	fn maps_held_from_a_coarser_level_draw_as_whole_ones() {
		let state = std::env::temp_dir().join(format!("ror-residency-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu.clone(), assets).expect("a renderer");
		// every render's maps through the feedback, however few
		renderer.residency_from.store(0, std::sync::atomic::Ordering::Relaxed);
		// a 256 x 256 image with its mips, as a kept texture file the cache finds for the decal's image
		let rgba: Vec<u8> = (0..256u32 * 256 * 4).map(|index| (index.wrapping_mul(2654435761) >> 13) as u8).collect();
		let mut image = ror_image::Image::from_rgba8(256, 256, rgba);
		ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true);
		let path = state.join("decal.tex");
		assert!(texture_files::write(&path, &image, TextureUse::Color));
		let uri = "rbxassetid://424242";
		let key = transcode::image_key(uri);
		let texture = renderer.textures.get_or_load(&(content_key(&key), TextureUse::Color), || texture_files::open(&path, &key).ok_or_else(|| anyhow!("the texture file"))).unwrap();
		// an 8-stud plate with the image on its front, 40 studs away in a 64 px view: 9 px across
		let mut model = crate::scene::build::ModelBuilder::new("Plate");
		model.part(&crate::scene::build::PartSpec { name: "Plate".into(), size: glam::Vec3::new(8.0, 8.0, 1.0), ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		std::sync::Arc::make_mut(&mut scene.parts[0]).decals.push(crate::scene::Decal { face: 5, texture: uri.into(), color: glam::Vec3::ONE, transparency: 0.0, z_index: 1, tiling: None, uv_offset: [0.0; 2], uv_scale: [1.0; 2] });
		let request = ViewRequest { width: 64, height: 64, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -40.0], target: [0.0; 3], fov: 70.0 }, ..Default::default() };
		let (from_coarser, _) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		let held = texture.held().expect("its levels read");
		assert!(held.base > 0 && held.base <= texture.max_base(), "held from level {}", held.base);
		texture.levels_from(&gpu, 0, &|_| Ok(())).unwrap();
		let (whole, _) = renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		assert_eq!(texture.held().unwrap().base, 0);
		assert!(from_coarser[0].rgba.chunks(4).any(|pixel| pixel != whole[0].rgba[..4].as_ref()), "the decal shows");
		assert!(from_coarser[0].rgba == whole[0].rgba, "the same pixels");
		drop(texture);
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A clip's frames under one camera skip the feedback once it has seen their parts: a part moved
	/// close to the camera in a later frame (a new preparation, unseen) draws with its maps whole, as the
	/// frame drawn on its own does.
	#[test]
	fn a_clip_skips_the_feedback_for_what_it_saw_and_reads_the_rest_whole() {
		let state = std::env::temp_dir().join(format!("ror-clip-residency-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(super::super::gpu::Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu.clone(), assets).expect("a renderer");
		renderer.residency_from.store(0, std::sync::atomic::Ordering::Relaxed);
		// an image of `side` texels with its mips, as a kept texture file the cache finds for `uri`
		let image_file = |uri: &str, side: u32, seed: u32| {
			let rgba: Vec<u8> = (0..side * side * 4).map(|index| (index.wrapping_add(seed).wrapping_mul(2654435761) >> 13) as u8).collect();
			let mut image = ror_image::Image::from_rgba8(side, side, rgba);
			ror_image::generate_mips_with(&mut image, ror_image::MipFilter::Triangle, true);
			let path = state.join(format!("{seed}.tex"));
			assert!(texture_files::write(&path, &image, TextureUse::Color));
			let key = transcode::image_key(uri);
			renderer.textures.get_or_load(&(content_key(&key), TextureUse::Color), || texture_files::open(&path, &key).ok_or_else(|| anyhow!("the texture file"))).unwrap()
		};
		let (still_uri, mover_uri) = ("rbxassetid://434343", "rbxassetid://434344");
		let (still_map, mover_map) = (image_file(still_uri, 512, 1), image_file(mover_uri, 256, 2));
		// two 8-stud plates with an image each, 80 studs off; the second comes to 6 studs from the camera
		let scene_at = |near: f32| {
			let mut model = crate::scene::build::ModelBuilder::new("Plates");
			model.part(&crate::scene::build::PartSpec { name: "Still".into(), size: glam::Vec3::new(8.0, 8.0, 1.0), cframe: glam::Affine3A::from_translation(glam::Vec3::new(-6.0, 0.0, 0.0)), ..Default::default() });
			model.part(&crate::scene::build::PartSpec { name: "Mover".into(), size: glam::Vec3::new(8.0, 8.0, 1.0), cframe: glam::Affine3A::from_translation(glam::Vec3::new(4.0, 0.0, near)), ..Default::default() });
			let mut scene = crate::scene::extract::extract(&model.into_document());
			for (part, uri) in scene.parts.iter_mut().zip([still_uri, mover_uri]) {
				std::sync::Arc::make_mut(part).decals.push(crate::scene::Decal { face: 5, texture: uri.into(), color: glam::Vec3::ONE, transparency: 0.0, z_index: 1, tiling: None, uv_offset: [0.0; 2], uv_scale: [1.0; 2] });
			}
			scene
		};
		let request = ViewRequest { width: 96, height: 96, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -80.0], target: [0.0; 3], fov: 70.0 }, ..Default::default() };
		let kept = super::super::ClipKept::new();
		let frame = |scene: &Scene| {
			let sky = renderer.prepare_environment(scene);
			let (mut views, mut stats) = renderer.submit_frame(scene, std::slice::from_ref(&request), sky, None, Default::default(), None, Some((&kept, true))).unwrap();
			views.pop().unwrap().finish(&mut stats).map(|image| (image.rgba, stats.residency_ms)).unwrap()
		};
		let (_, first_feedback) = frame(&scene_at(0.0));
		assert!(still_map.held().is_some_and(|held| held.base > 0) && mover_map.held().is_some_and(|held| held.base > 0), "read from coarser levels");
		// the same frame again: its parts seen, no feedback
		let (_, again) = frame(&scene_at(0.0));
		assert!(first_feedback > 0.0 && again == 0.0, "feedback {first_feedback} ms, then {again} ms");
		let (near, _) = frame(&scene_at(-74.0));
		assert!(mover_map.held().unwrap().base == 0 && still_map.held().unwrap().base > 0, "the moved part's map read whole, the still one's kept");
		let (whole, _) = renderer.render(&scene_at(-74.0), std::slice::from_ref(&request)).unwrap();
		assert!(near == whole[0].rgba, "the frame is the one drawn on its own");
		drop((still_map, mover_map));
		drop(renderer);
		let _ = std::fs::remove_dir_all(&state);
	}
}
