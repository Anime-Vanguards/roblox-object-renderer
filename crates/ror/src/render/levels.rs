//! A view's draws as Roblox draws them from its camera (`ViewRequest::lod`, crate::lod): each MeshPart
//! at the level of its chain its distance and fidelity give, and the parts the quality's view distance
//! and size cull leave out drawing nothing. The preparation is shared by every view and kept between
//! renders, so a view under levels of detail draws a copy of its draw list (the same draws in the same
//! order, only their index ranges changed): its own shadow maps, ids and Highlights follow it.

use std::collections::BTreeMap;

use glam::Vec3;
use serde::Serialize;

use super::DrawItem;
use super::camera::Camera;
use super::prepared::PreparedGeometry;
use crate::lod::{self, Lod};
use crate::scene::{Geometry, Scene};

/// What a view drew under levels of detail (in summary.json, each view's `lod`).
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LodView {
	/// The graphics quality level drawn for (1-21)
	pub quality: u32,
	/// Whether what the quality culls was left out (`--lod roblox`; `levels`: the levels alone)
	pub cull: bool,
	/// MeshParts whose fidelity steps (Performance, Automatic with more than one high-quality level)
	/// at each level of their chain (0: full detail), those the view leaves out not counted
	pub levels: [usize; 5],
	/// Parts the quality's view distance or size cull leaves out
	pub culled: usize,
	/// Performance MeshParts past full detail whose mesh has no chain: Roblox makes their levels at run
	/// time, ror draws them at full detail
	pub unchained: usize,
	/// Per mesh (its content URI) whose copies step: copies at each level and left out (the sixth)
	pub meshes: BTreeMap<String, [usize; 6]>,
}

/// One part's lot in a view: left out, or drawn at a level.
#[derive(Clone, Copy)]
enum Shown {
	Out,
	Level(usize),
}

/// `geometry`'s draws as `lod` has Roblox draw them from `camera`, and what they were; `None` when
/// every draw shows as prepared (all at full detail, nothing left out).
pub(super) fn for_view(scene: &Scene, geometry: &PreparedGeometry, camera: &Camera, lod: Lod) -> (Option<PreparedGeometry>, LodView) {
	let mut report = LodView { quality: lod.quality, cull: lod.cull, ..Default::default() };
	let eye = camera.eye;
	let mut shown: Vec<Option<Shown>> = vec![None; scene.parts.len()];
	let mut changed = false;
	let mut draws: Vec<DrawItem> = Vec::with_capacity(geometry.draws.len());
	for (index, draw) in geometry.draws.iter().enumerate() {
		let position = geometry.draw_parts[index] as usize;
		let lot = *shown[position].get_or_insert_with(|| {
			let part = &scene.parts[position];
			// characters and skinned meshes are left as they are (their clusters cull as one; not measured)
			if part.body_part.is_some() || part.skeleton.is_some() {
				return Shown::Level(0);
			}
			let bounds = lod::world_box(&part.cframe, part.size);
			let center = Vec3::from(part.cframe.translation);
			let drawn = lod.drawn(eye, camera.fov, bounds, center, part.size.length());
			let stepping = match &part.geometry {
				Geometry::MeshPart { mesh, fidelity, .. } if part.class == "MeshPart" && draw.mesh.bind.is_none() => {
					let levels = draw.mesh.lods.len();
					let steps = *fidelity == lod::PERFORMANCE || (*fidelity == lod::AUTOMATIC && draw.mesh.high_quality > 1 && levels > 1);
					steps.then(|| {
						let by_distance = lod::level_at(lod::box_distance(eye, bounds.0, bounds.1), camera.fov);
						(mesh, lod::fidelity_level(*fidelity, by_distance, levels, draw.mesh.high_quality), *fidelity == lod::PERFORMANCE && levels == 1 && by_distance > 0)
					})
				}
				_ => None,
			};
			if !drawn {
				report.culled += 1;
			}
			if let Some((mesh, level, unchained)) = stepping {
				let counts = report.meshes.entry(mesh.clone()).or_default();
				if drawn {
					report.levels[level] += 1;
					counts[level] += 1;
					report.unchained += usize::from(unchained);
				} else {
					counts[5] += 1;
				}
				if drawn {
					return Shown::Level(level);
				}
			}
			if drawn { Shown::Level(0) } else { Shown::Out }
		});
		let mut draw = draw.clone();
		match lot {
			Shown::Out => {
				draw.indices = 0..0;
				draw.runs = None;
				changed = true;
			}
			Shown::Level(level) if level > 0 => {
				// a decal draws over its part's level too (its runs are of the finest level's triangles);
				// a level the file leaves empty draws the one above it
				let level = (0..=level).rev().find(|&level| !draw.mesh.lods[level].is_empty()).unwrap_or(0);
				draw.indices = draw.mesh.lods[level].clone();
				draw.runs = None;
				changed = true;
			}
			Shown::Level(_) => {}
		}
		draws.push(draw);
	}
	(changed.then(|| geometry.with_draws(draws)), report)
}
