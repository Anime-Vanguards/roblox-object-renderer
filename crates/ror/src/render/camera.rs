//! Cameras: an orbit around the scene's bounds (fitted to the view), a look-at, or an explicit
//! CFrame, with Roblox's vertical field of view.

use glam::{Affine3A, Mat4, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum CameraSpec {
	/// Looks at the bounds' centre from `yaw` / `pitch` degrees (yaw 0 = in front of the model,
	/// on its -Z side looking +Z; yaw 90 = its right (+X) side; pitch up from the horizon). The
	/// distance fits every corner of the bounds into the view, keeping `padding` (a fraction of
	/// the view) free at the tightest edge, unless `distance` is given.
	#[serde(rename_all = "camelCase")]
	Orbit {
		#[serde(default)]
		yaw: f32,
		#[serde(default = "default_pitch")]
		pitch: f32,
		#[serde(default)]
		distance: Option<f32>,
		#[serde(default = "default_padding")]
		padding: f32,
		#[serde(default)]
		target: Option<[f32; 3]>,
		#[serde(default = "default_fov")]
		fov: f32,
	},
	#[serde(rename_all = "camelCase")]
	LookAt {
		eye: [f32; 3],
		target: [f32; 3],
		#[serde(default = "default_fov")]
		fov: f32,
	},
	/// A Roblox CFrame: position then the rotation matrix by rows (x y z R00 R01 R02 R10 ... R22),
	/// looking down its -Z axis.
	#[serde(rename_all = "camelCase")]
	CFrame {
		components: [f32; 12],
		#[serde(default = "default_fov")]
		fov: f32,
		/// The near plane (studs), 0.1 by default.
		#[serde(default)]
		near: Option<f32>,
	},
}

fn default_pitch() -> f32 {
	20.0
}

fn default_padding() -> f32 {
	0.08
}

pub fn default_fov() -> f32 {
	70.0
}

impl Default for CameraSpec {
	fn default() -> Self {
		CameraSpec::Orbit { yaw: 30.0, pitch: default_pitch(), distance: None, padding: default_padding(), target: None, fov: default_fov() }
	}
}

/// A camera placed for a view.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
	pub eye: Vec3,
	/// Camera-to-world transform (looks down -Z, +Y up).
	pub world: Affine3A,
	/// Vertical field of view, degrees.
	pub fov: f32,
	pub near: f32,
}

impl Camera {
	pub fn view(&self) -> Mat4 {
		Mat4::from(self.world.inverse())
	}

	/// Reversed-Z infinite perspective projection (depth 1 at `near`, 0 at infinity).
	pub fn projection(&self, aspect: f32) -> Mat4 {
		Mat4::perspective_infinite_reverse_rh(self.fov.to_radians(), aspect, self.near)
	}

	pub fn view_projection(&self, aspect: f32) -> Mat4 {
		self.projection(aspect) * self.view()
	}

	/// Unprojects clip positions to directions from the camera: its rotation and projection inverted,
	/// the eye at the origin. The inverse view-projection loses the rays' precision far from the origin
	/// (a survey's cameras ~2,050 studs out drew the sky 0.5 % small and 2 px off).
	pub fn ray_unprojection(&self, aspect: f32) -> Mat4 {
		let rotation = Affine3A { matrix3: self.world.matrix3, translation: glam::Vec3A::ZERO };
		(self.projection(aspect) * Mat4::from(rotation.inverse())).inverse()
	}

	/// This camera exactly, as a spec (a view placed once and drawn by more than one renderer).
	pub fn spec(&self) -> CameraSpec {
		CameraSpec::CFrame { components: self.cframe_components(), fov: self.fov, near: Some(self.near) }
	}

	/// The camera's CFrame components (x y z R00 R01 R02 R10 R11 R12 R20 R21 R22).
	pub fn cframe_components(&self) -> [f32; 12] {
		let m = self.world.matrix3;
		let p = self.world.translation;
		[p.x, p.y, p.z, m.x_axis.x, m.y_axis.x, m.z_axis.x, m.x_axis.y, m.y_axis.y, m.z_axis.y, m.x_axis.z, m.y_axis.z, m.z_axis.z]
	}
}

fn look_at(eye: Vec3, target: Vec3, fov: f32, near: f32) -> Camera {
	let forward = (target - eye).normalize_or(Vec3::NEG_Z);
	let up = if forward.y.abs() > 0.999 { Vec3::Z } else { Vec3::Y };
	let right = forward.cross(up).normalize();
	let up = right.cross(forward);
	let world = Affine3A::from_cols(right.into(), up.into(), (-forward).into(), eye.into());
	Camera { eye, world, fov, near }
}

impl CameraSpec {
	pub fn fov(&self) -> f32 {
		match self {
			CameraSpec::Orbit { fov, .. } | CameraSpec::LookAt { fov, .. } | CameraSpec::CFrame { fov, .. } => *fov,
		}
	}

	/// Places the camera for a `width` x `height` view of `bounds`.
	pub fn place(&self, bounds: Option<(Vec3, Vec3)>, width: u32, height: u32) -> Camera {
		let aspect = width as f32 / height.max(1) as f32;
		match *self {
			CameraSpec::LookAt { eye, target, fov } => {
				let eye = Vec3::from(eye);
				look_at(eye, Vec3::from(target), fov, near_for(eye.distance(Vec3::from(target))))
			}
			CameraSpec::CFrame { components: c, fov, near } => {
				let world = Affine3A::from_cols(
					glam::Vec3A::new(c[3], c[6], c[9]),
					glam::Vec3A::new(c[4], c[7], c[10]),
					glam::Vec3A::new(c[5], c[8], c[11]),
					glam::Vec3A::new(c[0], c[1], c[2]),
				);
				Camera { eye: Vec3::new(c[0], c[1], c[2]), world, fov, near: near.unwrap_or(0.1) }
			}
			CameraSpec::Orbit { yaw, pitch, distance, padding, target, fov } => {
				let bounds = bounds.unwrap_or((Vec3::splat(-1.0), Vec3::splat(1.0)));
				let center = target.map(Vec3::from).unwrap_or((bounds.0 + bounds.1) * 0.5);
				let (direction, fitted) = orbit_fit(bounds, center, yaw, pitch, fov, padding, aspect);
				let distance = distance.unwrap_or(fitted);
				look_at(center + direction * distance, center, fov, near_for(distance))
			}
		}
	}

	/// The camera framing `bounds` rather than what the scene draws (`--frame`, `--focus`): an orbit
	/// with its target and distance fixed for a `width` x `height` view; other cameras are fixed
	/// already.
	pub fn framing(&self, bounds: (Vec3, Vec3), width: u32, height: u32) -> CameraSpec {
		match *self {
			CameraSpec::Orbit { yaw, pitch, distance, padding, target, fov } => {
				let center = target.map(Vec3::from).unwrap_or((bounds.0 + bounds.1) * 0.5);
				let (_, fitted) = orbit_fit(bounds, center, yaw, pitch, fov, padding, width as f32 / height.max(1) as f32);
				CameraSpec::Orbit { yaw, pitch, distance: Some(distance.unwrap_or(fitted)), padding, target: Some(center.into()), fov }
			}
			ref other => other.clone(),
		}
	}
}

/// An orbit's direction from its centre, and the distance that fits every corner of `(min, max)`
/// into a view of `aspect`, keeping `padding` of the view free at the tightest edge.
fn orbit_fit((min, max): (Vec3, Vec3), center: Vec3, yaw: f32, pitch: f32, fov: f32, padding: f32, aspect: f32) -> (Vec3, f32) {
	let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
	let direction = Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()).normalize();
	let probe = look_at(center + direction, center, fov, 0.1);
	let right = Vec3::from(probe.world.matrix3.x_axis);
	let up = Vec3::from(probe.world.matrix3.y_axis);
	let tan_y = (fov.to_radians() * 0.5).tan() * (1.0 - padding.clamp(0.0, 0.9));
	let tan_x = tan_y * aspect;
	let mut needed: f32 = 0.0;
	for index in 0..8 {
		let corner = Vec3::new(if index & 1 == 0 { min.x } else { max.x }, if index & 2 == 0 { min.y } else { max.y }, if index & 4 == 0 { min.z } else { max.z }) - center;
		let along = corner.dot(direction);
		needed = needed.max(corner.dot(right).abs() / tan_x + along).max(corner.dot(up).abs() / tan_y + along);
	}
	// at least a twentieth of a stud off (the near plane is a hundredth): an effect a fifth of a stud
	// across fills the view (an effect's black hole, which half a stud's floor left a dot)
	(direction, needed.max(0.05))
}

fn near_for(distance: f32) -> f32 {
	(distance * 0.002).clamp(0.01, 0.1)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_framed_orbit_places_as_the_orbit_of_its_box() {
		let bounds = (Vec3::new(-3.0, 0.0, -1.0), Vec3::new(5.0, 4.0, 2.0));
		let orbit = CameraSpec::Orbit { yaw: 30.0, pitch: 20.0, distance: None, padding: 0.08, target: None, fov: 40.0 };
		let fitted = orbit.place(Some(bounds), 800, 600);
		// framed once, it stands where it would for that box whatever the scene's own bounds
		let framed = orbit.framing(bounds, 800, 600).place(Some((Vec3::splat(-100.0), Vec3::splat(100.0))), 800, 600);
		assert!(fitted.eye.distance(framed.eye) < 1e-4, "{:?} against {:?}", fitted.eye, framed.eye);
	}
}
