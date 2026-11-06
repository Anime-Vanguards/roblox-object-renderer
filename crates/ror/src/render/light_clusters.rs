//! A view's local lights sorted into clusters of its screen and view depth (notes/render.md "Local
//! lights"): a surface pixel loops over the lights whose reach meets its cluster, not over every light
//! in view (mesh.wgsl `local_lights`). A light a cluster leaves out is one whose turn in the loop would
//! add nothing at any of its pixels: beyond its Range (and a SurfaceLight's face) for the light itself,
//! which only pixels nearer than LOCAL_LIGHT_FAR_END take, and beyond its voxels' reach for the far
//! surfaces' voxels, which only pixels past LOCAL_LIGHT_FAR_START take. The cluster's lights go in the
//! view's own order (a bit per light, read lowest first), so each pixel adds the same terms in the same
//! order as before and the image is the same; only the passes over lights that add nothing are gone.
//! a stage of 126 lights from a match camera at 1080p: its frame 24.5 -> 8.4 ms of GPU
//! work, Cavern's (132) 6.2 -> 4.7.
//!
//! Each light marks, slice by slice, the tiles the box round the part of its reach's ball in the slice
//! covers on screen (a pixel and a hair of depth more for the shader's own rounding), so a cluster may
//! hold lights it does not need, never miss one it does.

use glam::{Mat4, Vec3, Vec4Swizzles};

/// The fewest lights a view sorts: below it every pixel walks them all, as cheap as a cluster's.
pub const FEWEST_LIGHTS: usize = 8;

/// Depth slices, spaced evenly in the logarithm of the view depth from [`FIRST_DEPTH`] to [`LAST_DEPTH`]
/// (nearer in the first, farther in the last).
const SLICES: u32 = 32;
const FIRST_DEPTH: f32 = 1.0;
const LAST_DEPTH: f32 = 4096.0;

/// The most bytes a view's clusters take: the tiles grow from 32 pixels until they fit.
const MOST_BYTES: u64 = 8 << 20;

/// What one light can light, for the clusters.
#[derive(Clone, Copy, Debug)]
pub struct Reach {
	pub centre: Vec3,
	/// How far from the centre the light itself lights a surface (its Range, past a SurfaceLight's face)
	pub near: f32,
	/// How far its voxels reach a far surface's pixel (6 studs out from it), None when far surfaces take none
	pub far: Option<f32>,
}

/// What the shader needs to find a pixel's cluster and read its lights.
pub struct Clusters {
	/// A bit per light for every cluster, `words` u32s a cluster, clusters by slice, then row, then column
	pub masks: Vec<u32>,
	/// x the tile side in pixels, y tiles across, z tiles down, w u32s a cluster
	pub grid: [f32; 4],
	/// xyz the camera's forward axis, w the slices per unit of ln(depth / FIRST_DEPTH)
	pub view: [f32; 4],
}

/// The slice holding a view depth (as mesh.wgsl `light_cluster` finds it).
fn slice(depth: f32, scale: f32) -> i32 {
	((depth.max(1e-6) / FIRST_DEPTH).ln() * scale).floor().clamp(0.0, (SLICES - 1) as f32) as i32
}

/// `lights` sorted for a view of `width` x `height` pixels through `view` (world to camera, looking
/// down -Z from `eye` along `forward`) and `projection`, its vertical field of view `fov` (radians);
/// None for a view with fewer than [`FEWEST_LIGHTS`].
/// `near_end` and `far_start`: the distances from the camera from which surfaces take none of the lights'
/// own light, and some of their voxels.
#[allow(clippy::too_many_arguments)]
pub fn build(lights: &[Reach], view: Mat4, projection: Mat4, eye: Vec3, forward: Vec3, fov: f32, width: u32, height: u32, near_end: f32, far_start: f32) -> Option<Clusters> {
	if lights.len() < FEWEST_LIGHTS || width == 0 || height == 0 {
		return None;
	}
	let words = lights.len().div_ceil(32);
	let mut tile = 32u32;
	let (mut across_tiles, mut down) = (width.div_ceil(tile), height.div_ceil(tile));
	while across_tiles as u64 * down as u64 * SLICES as u64 * words as u64 * 4 > MOST_BYTES && tile < 1024 {
		tile *= 2;
		across_tiles = width.div_ceil(tile);
		down = height.div_ceil(tile);
	}
	let scale = SLICES as f32 / (LAST_DEPTH / FIRST_DEPTH).ln();
	// a point seen in the image is at most this many times its view depth from the eye (the corners)
	let tan = (fov * 0.5).tan();
	let corner = (1.0 + tan * tan * (1.0 + (width as f32 / height as f32).powi(2))).sqrt() * 1.001;
	// the slices whose pixels can be nearer than `near_end` (view depth no more than the distance), and
	// those that can be past `far_start`, a stud of slack each
	let slice_start = |s: i32| if s == 0 { 0.0 } else { FIRST_DEPTH * (s as f32 / scale).exp() };
	let slice_end = |s: i32| if s == SLICES as i32 - 1 { f32::MAX } else { FIRST_DEPTH * ((s + 1) as f32 / scale).exp() };
	let last_near = (0..SLICES as i32).filter(|&s| slice_start(s) < near_end + 1.0).max().unwrap_or(0);
	let first_far = (0..SLICES as i32).find(|&s| slice_end(s) * corner > far_start - 1.0).unwrap_or(SLICES as i32 - 1);
	let mut masks = vec![0u32; across_tiles as usize * down as usize * SLICES as usize * words];
	let (last_x, last_y) = (across_tiles as i32 - 1, down as i32 - 1);
	let mut mark = |index: usize, centre: Vec3, reach: f32, slices: (i32, i32)| {
		// a little more than the reach: the shader's positions and distances round
		let reach = reach + 0.01 + 1e-4 * centre.distance(eye);
		let centre = view.transform_point3(centre);
		let depth = -centre.z;
		if depth + reach <= 0.0 {
			return;
		}
		// the slices a point of the ball can fall in, a hair either side of its depths for the shader's own
		// rounding of a pixel's depth
		let widen = |depth: f32, by: f32| depth * (1.0 + by * 2e-4) + by * 2e-3;
		let first = slice(widen(depth - reach, -1.0), scale).max(slices.0);
		let last = slice(widen(depth + reach, 1.0), scale).min(slices.1);
		let (word, bit) = (index / 32, 1u32 << (index % 32));
		for slice in first..=last {
			// the part of the ball within the slice's depths (widened as above), and the box round it
			let (low, high) = (widen(slice_start(slice), -1.0).max(depth - reach), widen(slice_end(slice), 1.0).min(depth + reach));
			if low > high {
				continue;
			}
			let gap = if depth < low { low - depth } else if depth > high { depth - high } else { 0.0 };
			let across = (reach * reach - gap * gap).max(0.0).sqrt();
			let mut pixels_low = glam::Vec2::splat(f32::MAX);
			let mut pixels_high = glam::Vec2::splat(f32::MIN);
			let mut whole = low < 0.01;
			for corner in 0..8 {
				if whole {
					break;
				}
				let point = Vec3::new(centre.x + if corner & 1 == 0 { -across } else { across }, centre.y + if corner & 2 == 0 { -across } else { across }, -if corner & 4 == 0 { low } else { high });
				let clip = projection * point.extend(1.0);
				if clip.w <= 1e-3 {
					whole = true;
					break;
				}
				let ndc = clip.xy() / clip.w;
				let pixel = glam::Vec2::new((ndc.x * 0.5 + 0.5) * width as f32, (0.5 - ndc.y * 0.5) * height as f32);
				pixels_low = pixels_low.min(pixel);
				pixels_high = pixels_high.max(pixel);
			}
			// a pixel of slack: a pixel's position is its centre's ray's, as interpolated
			let (x0, x1, y0, y1) = if whole {
				(0, last_x, 0, last_y)
			} else {
				let tile = tile as f32;
				let start = |value: f32, last: i32| (((value - 1.0) / tile).floor() as i64).clamp(0, last as i64) as i32;
				let end = |value: f32, last: i32| (((value + 1.0) / tile).floor() as i64).clamp(-1, last as i64) as i32;
				(start(pixels_low.x, last_x), end(pixels_high.x, last_x), start(pixels_low.y, last_y), end(pixels_high.y, last_y))
			};
			for y in y0..=y1 {
				let row = (slice as usize * down as usize + y as usize) * across_tiles as usize;
				for x in x0..=x1 {
					masks[(row + x as usize) * words + word] |= bit;
				}
			}
		}
	};
	for (index, light) in lights.iter().enumerate() {
		mark(index, light.centre, light.near, (0, last_near));
		if let Some(far) = light.far {
			mark(index, light.centre, far, (first_far, SLICES as i32 - 1));
		}
	}
	Some(Clusters { masks, grid: [tile as f32, across_tiles as f32, down as f32, words as f32], view: [forward.x, forward.y, forward.z, scale] })
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every point within a light's reach lands in a cluster that holds it, wherever it is: its own
	/// reach nearer than `near_end`, its voxels' past `far_start`.
	#[test]
	fn a_lit_point_finds_its_light() {
		let eye = Vec3::new(3.0, 20.0, -40.0);
		let target = Vec3::new(0.0, 0.0, 300.0);
		let (width, height, fov) = (1920, 1080, 70f32.to_radians());
		let projection = Mat4::perspective_infinite_reverse_rh(fov, width as f32 / height as f32, 0.1);
		let view = Mat4::look_at_rh(eye, target, Vec3::Y);
		let view_projection = projection * view;
		let forward = (target - eye).normalize();
		let (near_end, far_start) = (120.0, 100.0);
		let lights: Vec<Reach> = (0..40)
			.map(|i| {
				let centre = Vec3::new((i % 8) as f32 * 9.0 - 30.0, (i / 8) as f32 * 3.0, (i % 5) as f32 * 60.0 - 20.0);
				let near = 4.0 + (i % 7) as f32 * 3.0;
				Reach { centre, near, far: (i % 3 != 0).then_some(near + 20.0) }
			})
			.collect();
		let clusters = build(&lights, view, projection, eye, forward, fov, width, height, near_end, far_start).expect("sorted");
		let [tile, across, down, words] = clusters.grid.map(|value| value as usize);
		let scale = clusters.view[3];
		let mut checked = 0;
		for (index, light) in lights.iter().enumerate() {
			for (reach, wanted) in [Some((light.near, 0)), light.far.map(|far| (far, 1))].into_iter().flatten() {
				for step in 0..300 {
					let t = step as f32 * 0.6180339;
					let direction = Vec3::new(t.sin() * (t * 1.3).cos(), (t * 0.7).sin(), t.cos() * (t * 1.3).cos()).normalize_or_zero();
					let point = light.centre + direction * reach * ((step % 10) as f32 / 10.0);
					let distance = point.distance(eye);
					if (wanted == 0 && distance >= near_end) || (wanted == 1 && distance <= far_start) {
						continue;
					}
					let clip = view_projection * point.extend(1.0);
					if clip.w <= 0.1 {
						continue;
					}
					let ndc = clip.xy() / clip.w;
					let pixel = glam::Vec2::new((ndc.x * 0.5 + 0.5) * width as f32, (0.5 - ndc.y * 0.5) * height as f32);
					if pixel.x < 0.0 || pixel.y < 0.0 || pixel.x >= width as f32 || pixel.y >= height as f32 {
						continue;
					}
					let (x, y) = (pixel.x as usize / tile, pixel.y as usize / tile);
					let s = slice((point - eye).dot(forward), scale) as usize;
					let cluster = (s * down + y) * across + x;
					assert!(clusters.masks[cluster * words + index / 32] & (1 << (index % 32)) != 0, "light {index} missing at {point} ({distance} studs)");
					checked += 1;
				}
			}
		}
		assert!(checked > 4000, "{checked}");
	}

	#[test]
	fn few_lights_are_not_sorted() {
		let light = Reach { centre: Vec3::ZERO, near: 10.0, far: None };
		assert!(build(&[light], Mat4::IDENTITY, Mat4::IDENTITY, Vec3::ZERO, Vec3::NEG_Z, 1.0, 64, 64, 600.0, 580.0).is_none());
	}
}
