//! `ror cost --grid`: the cost of a map by where the camera stands, as a picture. A far gameplay camera
//! every few studs over the track's placement area (or a `--region`) looks four ways (`grid<x>_<z>-<yaw>` views); each cell's views average into its
//! draws, triangles and occlusion work, painted over the map seen from above (`grid.draws.png`,
//! `grid.triangles.png`, `grid.occlusion.png`) and listed in `grid.json`.

use std::collections::BTreeMap;
use std::path::Path;

use glam::{Mat4, Vec3, Vec4};
use serde::Serialize;

use super::ViewCost;
use crate::render::camera::{Camera, CameraSpec};

/// One cell: where it stands and its views' averages.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
	pub name: String,
	/// The point its cameras circle (the floor under the cell's centre).
	pub center: [f32; 3],
	pub views: usize,
	pub draws: f64,
	pub triangles: f64,
	pub occlusion_ms: f64,
	pub tested: f64,
	pub hidden: f64,
	/// Its costliest view by draws.
	pub worst_view: String,
	pub worst_draws: u64,
}

/// The near and far gameplay cameras' offsets from the point they look at (as `--paths`): studs back,
/// studs up and pitch in radians.
pub const FAR: (f32, f32, f32) = (70.0, 55.0, -0.65);

/// The cells of a grid run: views named `<cell>-<yaw>` grouped by cell, each cell's point from its
/// cameras (their eyes circle it at the same height).
pub fn cells(costs: &[ViewCost], cameras: &[Camera]) -> Vec<Cell> {
	let mut grouped: BTreeMap<String, Vec<(&ViewCost, &Camera)>> = BTreeMap::new();
	for (cost, camera) in costs.iter().zip(cameras) {
		if let Some((cell, _)) = cost.name.rsplit_once('-') {
			grouped.entry(cell.to_string()).or_default().push((cost, camera));
		}
	}
	grouped
		.into_iter()
		.map(|(name, views)| {
			let count = views.len() as f64;
			let eye = views.iter().map(|(_, camera)| camera.eye).sum::<Vec3>() / views.len() as f32;
			let worst = views.iter().max_by_key(|(cost, _)| cost.total.draws).map(|(cost, _)| (cost.name.clone(), cost.total.draws)).unwrap_or_default();
			Cell {
				name,
				center: [eye.x, eye.y - FAR.1 - 4.0, eye.z],
				views: views.len(),
				draws: views.iter().map(|(cost, _)| cost.total.draws as f64).sum::<f64>() / count,
				triangles: views.iter().map(|(cost, _)| cost.total.triangles as f64).sum::<f64>() / count,
				occlusion_ms: views.iter().map(|(cost, _)| cost.occlusion.estimate_ms).sum::<f64>() / count,
				tested: views.iter().map(|(cost, _)| cost.occlusion.tested as f64).sum::<f64>() / count,
				hidden: views.iter().map(|(cost, _)| cost.occlusion.hidden as f64).sum::<f64>() / count,
				worst_view: worst.0,
				worst_draws: worst.1,
			}
		})
		.collect()
}

/// The camera straight above the cells, a narrow field of view from far up so the map reads like a
/// plan, north (-Z) up; and the picture's size.
pub fn overhead(cells: &[Cell], spacing: f32) -> (CameraSpec, u32, u32) {
	let (mut low, mut high) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
	for cell in cells {
		let center = Vec3::from(cell.center);
		low = low.min(center - Vec3::splat(spacing * 0.5));
		high = high.max(center + Vec3::splat(spacing * 0.5));
	}
	let extent = (high - low).max(Vec3::splat(1.0));
	let width = 1200u32;
	let height = ((width as f32 * extent.z / extent.x).round() as u32).clamp(300, 2400);
	let fov = 20.0f32;
	let aspect = width as f32 / height as f32;
	// far enough that both the width and the depth of the grid fit the view
	let half_tan = (fov.to_radians() * 0.5).tan();
	let distance = (extent.z * 0.5 / half_tan).max(extent.x * 0.5 / (half_tan * aspect)) * 1.05;
	let middle = (low + high) * 0.5;
	let eye = Vec3::new(middle.x, high.y + distance, middle.z);
	// right +X, up -Z (north), back +Y
	let components = [eye.x, eye.y, eye.z, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0];
	(CameraSpec::CFrame { components, fov, near: None }, width, height)
}

/// Pixel coordinates of a world point in a picture from `camera`.
fn project(camera: &Camera, width: u32, height: u32, point: Vec3) -> Option<(f32, f32)> {
	let projection = Mat4::perspective_rh(camera.fov.to_radians(), width as f32 / height as f32, camera.near.max(0.01), 1e6);
	let clip = projection * camera.view() * Vec4::new(point.x, point.y, point.z, 1.0);
	if clip.w <= 0.0 {
		return None;
	}
	let ndc = clip.truncate() / clip.w;
	Some(((ndc.x * 0.5 + 0.5) * width as f32, (0.5 - ndc.y * 0.5) * height as f32))
}

/// Green (cheapest) through yellow to red (costliest).
fn ramp(t: f32) -> [f32; 3] {
	let t = t.clamp(0.0, 1.0);
	if t < 0.5 { [2.0 * t, 0.75 + 0.25 * t * 2.0, 0.0] } else { [1.0, 1.0 - (t - 0.5) * 2.0, 0.0] }
}

/// The map picture (`rgba`, from the overhead camera) with each cell painted by `value`, and a ramp
/// along the bottom (cheapest left).
pub fn paint(rgba: &[u8], camera: &Camera, width: u32, height: u32, cells: &[Cell], spacing: f32, value: impl Fn(&Cell) -> f64) -> Vec<u8> {
	let mut out = rgba.to_vec();
	let (low, high) = cells.iter().map(&value).fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| (low.min(v), high.max(v)));
	let range = (high - low).max(1e-9);
	let mut blend = |x: i64, y: i64, color: [f32; 3], alpha: f32| {
		if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
			return;
		}
		let index = ((y as u32 * width + x as u32) * 4) as usize;
		for channel in 0..3 {
			let base = out[index + channel] as f32 / 255.0;
			out[index + channel] = ((base * (1.0 - alpha) + color[channel] * alpha) * 255.0).round() as u8;
		}
	};
	for cell in cells {
		let center = Vec3::from(cell.center);
		let half = spacing * 0.5;
		let (Some(a), Some(b)) = (project(camera, width, height, center - Vec3::new(half, 0.0, half)), project(camera, width, height, center + Vec3::new(half, 0.0, half))) else {
			continue;
		};
		let color = ramp(((value(cell) - low) / range) as f32);
		let (x0, x1) = (a.0.min(b.0).floor() as i64, a.0.max(b.0).ceil() as i64);
		let (y0, y1) = (a.1.min(b.1).floor() as i64, a.1.max(b.1).ceil() as i64);
		for y in y0..y1 {
			for x in x0..x1 {
				// a darker outline marks the cell's edge
				let edge = x == x0 || y == y0 || x == x1 - 1 || y == y1 - 1;
				blend(x, y, if edge { [0.1, 0.1, 0.1] } else { color }, if edge { 0.6 } else { 0.5 });
			}
		}
	}
	for x in 0..(width as i64 / 3) {
		for y in (height as i64 - 14)..(height as i64 - 4) {
			blend(x + 8, y, ramp(x as f32 / (width as f32 / 3.0)), 1.0);
		}
	}
	out
}

/// The grid's summary lines (`area` names what it covers): the costliest cells by draws and by
/// occlusion work.
pub fn lines(cells: &[Cell], spacing: f32, area: &str) -> Vec<String> {
	let mut lines = Vec::new();
	if cells.is_empty() {
		return lines;
	}
	let mean = |pick: fn(&Cell) -> f64| cells.iter().map(pick).sum::<f64>() / cells.len() as f64;
	lines.push(format!(
		"grid: {} cells {spacing:.0} studs apart over {area} (a far camera looking four ways from each): {:.0} draws, {:.0}k triangles, ~{:.2} ms of occlusion work per view on average; grid.draws.png, grid.triangles.png and grid.occlusion.png paint them over the map (green cheapest, red costliest)",
		cells.len(),
		mean(|cell| cell.draws),
		mean(|cell| cell.triangles) / 1000.0,
		mean(|cell| cell.occlusion_ms)
	));
	// the pictures' colours run from their cheapest cell to their costliest, not from zero
	let range = |pick: fn(&Cell) -> f64| cells.iter().map(pick).fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| (low.min(v), high.max(v)));
	let ((draws_low, draws_high), (triangles_low, triangles_high), (occlusion_low, occlusion_high)) = (range(|cell| cell.draws), range(|cell| cell.triangles), range(|cell| cell.occlusion_ms));
	lines.push(format!(
		"  green to red: {draws_low:.0} to {draws_high:.0} draws, {:.0}k to {:.0}k triangles, ~{occlusion_low:.2} to ~{occlusion_high:.2} ms of occlusion work",
		triangles_low / 1000.0,
		triangles_high / 1000.0
	));
	let mut by_draws: Vec<&Cell> = cells.iter().collect();
	by_draws.sort_by(|a, b| b.draws.total_cmp(&a.draws));
	let list: Vec<String> = by_draws.iter().take(5).map(|cell| format!("({:.0}, {:.0}) {:.0} draws (worst {} {})", cell.center[0], cell.center[2], cell.draws, cell.worst_view, cell.worst_draws)).collect();
	lines.push(format!("  costliest cells by draws: {}", list.join(", ")));
	let mut by_occlusion: Vec<&Cell> = cells.iter().collect();
	by_occlusion.sort_by(|a, b| b.occlusion_ms.total_cmp(&a.occlusion_ms));
	let list: Vec<String> = by_occlusion.iter().take(5).map(|cell| format!("({:.0}, {:.0}) ~{:.2} ms, {:.0} tested", cell.center[0], cell.center[2], cell.occlusion_ms, cell.tested)).collect();
	lines.push(format!("  most occlusion work: {}", list.join(", ")));
	lines
}

/// `grid.json`: every cell.
pub fn write(cells: &[Cell], spacing: f32, out: &Path) -> anyhow::Result<()> {
	crate::output::write_atomic(&out.join("grid.json"), serde_json::to_string_pretty(&serde_json::json!({"spacing": spacing, "cells": cells}))?.as_bytes())
}
