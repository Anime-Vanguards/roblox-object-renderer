//! Conservative clip-volume tests against the bounds of the vertices actually drawn.

use glam::{Affine3A, Mat4, Vec3, Vec4};

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Bounds {
	center: Vec3,
	extent: Vec3,
}

impl Bounds {
	pub fn new((low, high): (Vec3, Vec3), world: Affine3A) -> Self {
		let center = world.transform_point3((low + high) * 0.5);
		let half = (high - low) * 0.5;
		let extent = Vec3::from(world.matrix3.x_axis.abs()) * half.x + Vec3::from(world.matrix3.y_axis.abs()) * half.y + Vec3::from(world.matrix3.z_axis.abs()) * half.z;
		// Leave room for the vertex shader's matrix arithmetic at large world coordinates and for
		// vertices lying exactly on a clip plane. Borderline draws stay on the GPU.
		let margin = 1e-4 + (center.abs().max_element() + extent.max_element()) * 2e-6;
		Self { center, extent: extent + Vec3::splat(margin) }
	}

	/// A sphere round the box: its centre and radius.
	pub fn sphere(&self) -> (Vec3, f32) {
		(self.center, self.extent.length())
	}
}

pub(super) struct Frustum {
	planes: [Vec4; 6],
}

impl Frustum {
	pub fn new(projection: Mat4) -> Self {
		let [x, y, z, w] = std::array::from_fn(|index| projection.row(index));
		Self { planes: [w + x, w - x, w + y, w - y, z, w - z] }
	}

	pub fn intersects(&self, bounds: Bounds) -> bool {
		!self.planes.iter().any(|plane| {
			let normal = plane.truncate();
			normal.dot(bounds.center) + plane.w + normal.abs().dot(bounds.extent) < 0.0
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clip_planes_keep_crossing_boxes_and_reject_boxes_outside_each_plane() {
		let frustum = Frustum::new(Mat4::IDENTITY);
		let bounds = |center: Vec3, half: f32| Bounds::new((Vec3::splat(-half), Vec3::splat(half)), Affine3A::from_translation(center));
		assert!(frustum.intersects(bounds(Vec3::new(0.0, 0.0, 0.5), 0.1)));
		for center in [Vec3::new(2.0, 0.0, 0.5), Vec3::new(-2.0, 0.0, 0.5), Vec3::new(0.0, 2.0, 0.5), Vec3::new(0.0, -2.0, 0.5), Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 0.0, 2.0)] {
			assert!(!frustum.intersects(bounds(center, 0.1)));
			assert!(frustum.intersects(bounds(center, 1.0)));
		}
	}

	#[test]
	fn transformed_bounds_enclose_every_corner_with_rotation_and_nonuniform_scale() {
		let world = Affine3A::from_scale_rotation_translation(Vec3::new(0.1, 20.0, -3.0), glam::Quat::from_euler(glam::EulerRot::XYZ, 0.4, 1.7, -0.8), Vec3::new(12000.0, -30.0, 400.0));
		let low = Vec3::new(-2.0, 0.2, -0.4);
		let high = Vec3::new(3.0, 0.3, 2.0);
		let bounds = Bounds::new((low, high), world);
		for corner in 0..8 {
			let point = world.transform_point3(Vec3::new(if corner & 1 == 0 { low.x } else { high.x }, if corner & 2 == 0 { low.y } else { high.y }, if corner & 4 == 0 { low.z } else { high.z }));
			assert!(((point - bounds.center).abs().cmple(bounds.extent)).all());
		}
	}

	#[test]
	fn infinite_reversed_depth_keeps_distant_geometry_and_clips_behind_the_eye() {
		let frustum = Frustum::new(Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1));
		let bounds = |center| Bounds::new((Vec3::splat(-0.2), Vec3::splat(0.2)), Affine3A::from_translation(center));
		assert!(frustum.intersects(bounds(Vec3::new(0.0, 0.0, -100000.0))));
		assert!(frustum.intersects(bounds(Vec3::ZERO)));
		assert!(!frustum.intersects(bounds(Vec3::new(0.0, 0.0, 2.0))));
	}
}
