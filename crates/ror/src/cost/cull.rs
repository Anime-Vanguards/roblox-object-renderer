//! What a camera skips before drawing, as Roblox's renderer does: what lies outside the view
//! (frustum culling, per instance) and what is hidden behind occluders (occlusion culling).
//!
//! Roblox's occlusion culling (live since December 2024) rasterizes occluders into a depth buffer on
//! a thread of its own and tests each object's screen-space bounding box against it, not its
//! polygons; block parts are its cheapest occluders, low-poly meshes work too and "high detail"
//! meshes are left out (Roblox staff on the DevForum, notes/cost.md "Occlusion"). This is the same
//! scheme in software at a fixed resolution: occluder triangles facing the camera at their nearest
//! depth per pixel, an object hidden when no pixel of its box's screen rectangle lies behind its
//! nearest point.

use glam::{Affine3A, Mat4, Vec3, Vec4};

use crate::render::camera::Camera;

/// Width of the occlusion buffer (its height follows the view's aspect).
pub const BUFFER_WIDTH: u32 = 256;

/// The view as the culling sees it: world-to-view transform, projection and the depth buffer.
#[derive(Clone)]
pub struct Culler {
	view: Affine3A,
	projection: Mat4,
	near: f32,
	width: u32,
	height: u32,
	/// Distance along the view axis of the nearest occluder at each pixel (infinity: none).
	depth: Vec<f32>,
	/// Which occluder wrote each pixel ([`Culler::occlude_as`]; `u16::MAX`: none).
	owner: Vec<u16>,
	/// The occluder [`Culler::occlude`] writes as.
	writer: u16,
	/// Frustum planes (inward normals, w the offset) in world space.
	planes: [Vec4; 6],
	pub eye: Vec3,
	pub forward: Vec3,
}

impl Culler {
	pub fn new(camera: &Camera, aspect: f32) -> Self {
		let view = camera.world.inverse();
		let projection = Mat4::perspective_rh(camera.fov.to_radians(), aspect, camera.near.max(0.01), 100_000.0);
		let clip = projection * Mat4::from(view);
		let rows = [clip.row(0), clip.row(1), clip.row(2), clip.row(3)];
		let planes = [rows[3] + rows[0], rows[3] - rows[0], rows[3] + rows[1], rows[3] - rows[1], rows[3] + rows[2], rows[3] - rows[2]].map(|plane| plane / plane.truncate().length().max(1e-12));
		let width = BUFFER_WIDTH;
		let height = ((BUFFER_WIDTH as f32 / aspect.max(0.01)).round() as u32).clamp(16, 4096);
		Self {
			view,
			projection,
			near: camera.near.max(0.01),
			width,
			height,
			depth: vec![f32::INFINITY; (width * height) as usize],
			owner: vec![u16::MAX; (width * height) as usize],
			writer: u16::MAX,
			planes,
			eye: camera.eye,
			forward: -Vec3::from(camera.world.matrix3.z_axis),
		}
	}

	/// Whether a world box touches the view frustum.
	pub fn in_frustum(&self, (min, max): (Vec3, Vec3)) -> bool {
		self.planes.iter().all(|plane| {
			let normal = plane.truncate();
			// the box corner furthest along the plane's normal
			let corner = Vec3::new(if normal.x >= 0.0 { max.x } else { min.x }, if normal.y >= 0.0 { max.y } else { min.y }, if normal.z >= 0.0 { max.z } else { min.z });
			normal.dot(corner) + plane.w >= 0.0
		})
	}

	/// Distance of a world point along the view axis.
	pub fn depth_of(&self, point: Vec3) -> f32 {
		(point - self.eye).dot(self.forward)
	}

	/// Pixel coordinates (x right, y down) and depth of a view-space point in front of the near plane.
	fn project(&self, view_point: Vec3) -> (f32, f32, f32) {
		let clip = self.projection * view_point.extend(1.0);
		let ndc = clip.truncate() / clip.w;
		((ndc.x * 0.5 + 0.5) * self.width as f32, (0.5 - ndc.y * 0.5) * self.height as f32, -view_point.z)
	}

	/// Rasterizes an occluder's triangles, its pixels marked as written by `owner`.
	pub fn occlude_as(&mut self, triangles: &[[Vec3; 3]], owner: u16) {
		self.writer = owner;
		for triangle in triangles {
			self.occlude(*triangle);
		}
		self.writer = u16::MAX;
	}

	/// Rasterizes one occluder triangle (world space) at its nearest depth per pixel, unless it faces
	/// away: Roblox's occluders are one-sided (a map's ceiling mesh under the camera hid only
	/// what its upward faces cover; notes/cost.md "Occlusion").
	pub fn occlude(&mut self, triangle: [Vec3; 3]) {
		let points = triangle.map(|point| self.view.transform_point3(point));
		if (points[1] - points[0]).cross(points[2] - points[0]).dot(points[0]) >= 0.0 {
			return;
		}
		// clip against the near plane (view space looks down -Z)
		let near = -self.near;
		// at most 4 corners left (one plane cuts one corner off), on the stack: two allocations a
		// triangle were a grid view's most frequent ones
		let (mut polygon, mut corners) = ([Vec3::ZERO; 4], 0);
		for index in 0..3 {
			let (a, b) = (points[index], points[(index + 1) % 3]);
			let (a_in, b_in) = (a.z <= near, b.z <= near);
			if a_in {
				polygon[corners] = a;
				corners += 1;
			}
			if a_in != b_in {
				let t = (near - a.z) / (b.z - a.z);
				polygon[corners] = a + (b - a) * t;
				corners += 1;
			}
		}
		if corners < 3 {
			return;
		}
		let mut projected = [(0.0, 0.0, 0.0); 4];
		for (index, point) in polygon[..corners].iter().enumerate() {
			projected[index] = self.project(*point);
		}
		for index in 1..corners - 1 {
			self.fill(projected[0], projected[index], projected[index + 1]);
		}
	}

	/// Fills a screen triangle, keeping the nearest depth (perspective-correct: 1/depth interpolated).
	fn fill(&mut self, a: (f32, f32, f32), b: (f32, f32, f32), c: (f32, f32, f32)) {
		let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
		if area.abs() < 1e-9 {
			return;
		}
		let min_x = a.0.min(b.0).min(c.0).floor().max(0.0) as i32;
		let max_x = a.0.max(b.0).max(c.0).ceil().min(self.width as f32 - 1.0) as i32;
		let min_y = a.1.min(b.1).min(c.1).floor().max(0.0) as i32;
		let max_y = a.1.max(b.1).max(c.1).ceil().min(self.height as f32 - 1.0) as i32;
		if min_x > max_x || min_y > max_y {
			return;
		}
		let inverse = [1.0 / a.2.max(1e-6), 1.0 / b.2.max(1e-6), 1.0 / c.2.max(1e-6)];
		for y in min_y..=max_y {
			let py = y as f32 + 0.5;
			for x in min_x..=max_x {
				let px = x as f32 + 0.5;
				let w0 = ((b.0 - px) * (c.1 - py) - (b.1 - py) * (c.0 - px)) / area;
				let w1 = ((c.0 - px) * (a.1 - py) - (c.1 - py) * (a.0 - px)) / area;
				let w2 = 1.0 - w0 - w1;
				if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
					continue;
				}
				let depth = 1.0 / (w0 * inverse[0] + w1 * inverse[1] + w2 * inverse[2]);
				let index = (y as u32 * self.width + x as u32) as usize;
				if depth < self.depth[index] {
					self.depth[index] = depth;
					self.owner[index] = self.writer;
				}
			}
		}
	}

	/// The pixels of the occlusion buffer a world box covers on screen (its projected rectangle,
	/// clipped to the buffer), and its rectangle's longer side in pixels; a box reaching the near
	/// plane covers the whole buffer.
	pub fn screen_extent(&self, (min, max): (Vec3, Vec3)) -> (f32, f32) {
		let (mut low_x, mut low_y, mut high_x, mut high_y) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
		for corner in 0..8 {
			let point = Vec3::new(if corner & 1 == 0 { min.x } else { max.x }, if corner & 2 == 0 { min.y } else { max.y }, if corner & 4 == 0 { min.z } else { max.z });
			let view_point = self.view.transform_point3(point);
			if -view_point.z <= self.near {
				let whole = (self.width * self.height) as f32;
				return (whole, self.width.max(self.height) as f32);
			}
			let (x, y, _) = self.project(view_point);
			low_x = low_x.min(x);
			low_y = low_y.min(y);
			high_x = high_x.max(x);
			high_y = high_y.max(y);
		}
		let width = high_x.min(self.width as f32) - low_x.max(0.0);
		let height = high_y.min(self.height as f32) - low_y.max(0.0);
		(width.max(0.0) * height.max(0.0), (high_x - low_x).max(high_y - low_y))
	}

	/// The occluder that hides most of a hidden box: the owner of most pixels in its screen
	/// rectangle (None when nothing marked hides it).
	pub fn hidden_by(&self, (min, max): (Vec3, Vec3)) -> Option<u16> {
		let (mut low_x, mut low_y, mut high_x, mut high_y) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
		for corner in 0..8 {
			let point = Vec3::new(if corner & 1 == 0 { min.x } else { max.x }, if corner & 2 == 0 { min.y } else { max.y }, if corner & 4 == 0 { min.z } else { max.z });
			let view_point = self.view.transform_point3(point);
			if -view_point.z <= self.near {
				return None;
			}
			let (x, y, _) = self.project(view_point);
			low_x = low_x.min(x);
			low_y = low_y.min(y);
			high_x = high_x.max(x);
			high_y = high_y.max(y);
		}
		let x0 = (low_x.floor().max(0.0) as i64).min(self.width as i64 - 1);
		let x1 = (high_x.ceil() as i64 - 1).clamp(0, self.width as i64 - 1).max(x0);
		let y0 = (low_y.floor().max(0.0) as i64).min(self.height as i64 - 1);
		let y1 = (high_y.ceil() as i64 - 1).clamp(0, self.height as i64 - 1).max(y0);
		// pixels per owner: a rectangle holds a few, in runs (a map per box, one insert a pixel, was
		// a grid view's second most frequent allocation)
		let mut counts: Vec<(u16, u32)> = Vec::new();
		let mut last = usize::MAX;
		for y in y0..=y1 {
			for x in x0..=x1 {
				let owner = self.owner[(y as u32 * self.width + x as u32) as usize];
				if owner == u16::MAX {
					continue;
				}
				if counts.get(last).is_none_or(|(known, _)| *known != owner) {
					last = counts.iter().position(|(known, _)| *known == owner).unwrap_or_else(|| {
						counts.push((owner, 0));
						counts.len() - 1
					});
				}
				counts[last].1 += 1;
			}
		}
		counts.into_iter().max_by_key(|(owner, count)| (*count, u16::MAX - *owner)).map(|(owner, _)| owner)
	}

	/// Whether a world box shows past the occluders: some pixel of its screen rectangle has nothing
	/// in front of the box's nearest point. A box reaching the near plane always shows.
	pub fn visible(&self, (min, max): (Vec3, Vec3)) -> bool {
		let mut nearest = f32::INFINITY;
		let (mut low_x, mut low_y, mut high_x, mut high_y) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
		for corner in 0..8 {
			let point = Vec3::new(if corner & 1 == 0 { min.x } else { max.x }, if corner & 2 == 0 { min.y } else { max.y }, if corner & 4 == 0 { min.z } else { max.z });
			let view_point = self.view.transform_point3(point);
			if -view_point.z <= self.near {
				return true;
			}
			let (x, y, depth) = self.project(view_point);
			nearest = nearest.min(depth);
			low_x = low_x.min(x);
			low_y = low_y.min(y);
			high_x = high_x.max(x);
			high_y = high_y.max(y);
		}
		let x0 = (low_x.floor().max(0.0) as i64).min(self.width as i64 - 1);
		let x1 = (high_x.ceil() as i64 - 1).clamp(0, self.width as i64 - 1);
		let y0 = (low_y.floor().max(0.0) as i64).min(self.height as i64 - 1);
		let y1 = (high_y.ceil() as i64 - 1).clamp(0, self.height as i64 - 1);
		// boxes thinner than a pixel still cover the one they fall in
		let (x1, y1) = (x1.max(x0), y1.max(y0));
		// an occluder at the box's own nearest depth (its front face) does not hide it
		let limit = nearest * (1.0 - 1e-4) - 1e-3;
		for y in y0..=y1 {
			let row = (y as u32 * self.width) as usize;
			for x in x0..=x1 {
				if self.depth[row + x as usize] >= limit {
					return true;
				}
			}
		}
		false
	}
}

/// The eight corners of a box: a part's (its CFrame and Size) or any oriented box.
pub fn box_corners(cframe: &Affine3A, size: Vec3) -> [Vec3; 8] {
	let half = size * 0.5;
	std::array::from_fn(|corner| {
		let local = Vec3::new(if corner & 1 == 0 { -half.x } else { half.x }, if corner & 2 == 0 { -half.y } else { half.y }, if corner & 4 == 0 { -half.z } else { half.z });
		cframe.transform_point3(local)
	})
}

/// The world-aligned box around an oriented one.
pub fn world_box(cframe: &Affine3A, size: Vec3) -> (Vec3, Vec3) {
	let extent = Vec3::from(cframe.matrix3.abs() * glam::Vec3A::from(size * 0.5));
	let center = Vec3::from(cframe.translation);
	(center - extent, center + extent)
}

/// A block's twelve triangles.
pub fn block_triangles(cframe: &Affine3A, size: Vec3) -> [[Vec3; 3]; 12] {
	let c = box_corners(cframe, size);
	// faces as corner indices (bit 0 x, bit 1 y, bit 2 z)
	const FACES: [[usize; 4]; 6] = [[1, 3, 7, 5], [0, 4, 6, 2], [2, 6, 7, 3], [0, 1, 5, 4], [4, 5, 7, 6], [0, 2, 3, 1]];
	let mut out = [[Vec3::ZERO; 3]; 12];
	for (index, face) in FACES.iter().enumerate() {
		out[index * 2] = [c[face[0]], c[face[1]], c[face[2]]];
		out[index * 2 + 1] = [c[face[0]], c[face[2]], c[face[3]]];
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn camera() -> Camera {
		// at the origin looking down -Z
		Camera { eye: Vec3::ZERO, world: Affine3A::IDENTITY, fov: 70.0, near: 0.1 }
	}

	#[test]
	fn a_wall_hides_what_is_behind_it() {
		let mut culler = Culler::new(&camera(), 1.5);
		for triangle in block_triangles(&Affine3A::from_translation(Vec3::new(0.0, 0.0, -20.0)), Vec3::new(400.0, 400.0, 1.0)) {
			culler.occlude(triangle);
		}
		let behind = (Vec3::new(-1.0, -1.0, -42.0), Vec3::new(1.0, 1.0, -40.0));
		let in_front = (Vec3::new(-1.0, -1.0, -12.0), Vec3::new(1.0, 1.0, -10.0));
		assert!(culler.in_frustum(behind));
		assert!(!culler.visible(behind));
		assert!(culler.visible(in_front));
		// the wall's own box shows
		assert!(culler.visible(world_box(&Affine3A::from_translation(Vec3::new(0.0, 0.0, -20.0)), Vec3::new(400.0, 400.0, 1.0))));
	}

	#[test]
	fn a_face_turned_away_hides_nothing() {
		let wall = block_triangles(&Affine3A::from_translation(Vec3::new(0.0, 0.0, -20.0)), Vec3::new(400.0, 400.0, 1.0));
		let normal = |triangle: &[Vec3; 3]| (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
		let behind = (Vec3::new(-1.0, -1.0, -42.0), Vec3::new(1.0, 1.0, -40.0));
		// the wall's far face alone, its back to the camera
		let mut culler = Culler::new(&camera(), 1.5);
		for triangle in wall.iter().filter(|triangle| normal(triangle).z < 0.0) {
			culler.occlude(*triangle);
		}
		assert!(culler.visible(behind));
		// its near face
		for triangle in wall.iter().filter(|triangle| normal(triangle).z > 0.0) {
			culler.occlude(*triangle);
		}
		assert!(!culler.visible(behind));
	}

	#[test]
	fn the_frustum_leaves_out_what_is_behind_the_camera() {
		let culler = Culler::new(&camera(), 1.5);
		assert!(!culler.in_frustum((Vec3::new(-1.0, -1.0, 10.0), Vec3::new(1.0, 1.0, 12.0))));
		assert!(culler.in_frustum((Vec3::new(-1.0, -1.0, -12.0), Vec3::new(1.0, 1.0, -10.0))));
		// far off to the side
		assert!(!culler.in_frustum((Vec3::new(200.0, -1.0, -12.0), Vec3::new(202.0, 1.0, -10.0))));
	}

	#[test]
	fn a_gap_in_a_wall_shows_what_is_behind_it() {
		let mut culler = Culler::new(&camera(), 1.0);
		// two halves of a wall with a gap at x in -2..2
		for (center, size) in [(Vec3::new(-102.0, 0.0, -20.0), Vec3::new(200.0, 400.0, 1.0)), (Vec3::new(102.0, 0.0, -20.0), Vec3::new(200.0, 400.0, 1.0))] {
			for triangle in block_triangles(&Affine3A::from_translation(center), size) {
				culler.occlude(triangle);
			}
		}
		assert!(culler.visible((Vec3::new(-1.0, -1.0, -42.0), Vec3::new(1.0, 1.0, -40.0))));
		assert!(!culler.visible((Vec3::new(-60.0, -1.0, -42.0), Vec3::new(-58.0, 1.0, -40.0))));
	}
}
