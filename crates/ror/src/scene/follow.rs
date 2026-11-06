//! Parts that follow the camera. A map's fog, mist or weather can come from one small part kept
//! under the camera instead of a map-sized one: a game tags such a part `CameraFollow` (a
//! CollectionService tag) and its client moves it there every frame, its particles left in the world
//! where they were emitted. Drawn where it was authored, its effect
//! would show (and cost) somewhere the player never looks, so ror stands it under each view's camera
//! the way the game does.

use glam::{Vec2, Vec3};
use rbx_dom_weak::types::{Ref, Variant};

use super::load::Document;
use super::props;

/// The CollectionService tag such a client follows.
pub const TAG: &str = "CameraFollow";

/// Where a `CameraFollow` part stands, read from its attributes as such a client reads them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Follow {
	/// The part's authored position.
	pub origin: Vec3,
	/// `CameraFollowAhead`: studs ahead of the camera along its look direction flattened to the
	/// ground (negative: behind it). 0 without one.
	pub ahead: f32,
	/// `CameraFollowHeight`: stands this many studs above the camera (below, negative); without one
	/// the part keeps its authored height (ground fog).
	pub height: Option<f32>,
	/// `CameraFollowRadius`: goes no further than this from its authored position across the ground
	/// (a cutscene camera far off leaves it at the edge).
	pub radius: Option<f32>,
	/// How many levels above the instance it was read for the part is (an emitter's lineage names it).
	pub levels: u8,
}

impl Follow {
	/// Where the part stands for a camera at `eye` looking along `look`.
	pub fn position(&self, eye: Vec3, look: Vec3) -> Vec3 {
		let flat = Vec2::new(look.x, look.z).normalize_or_zero() * self.ahead;
		let mut across = Vec2::new(eye.x + flat.x, eye.z + flat.y);
		if let Some(radius) = self.radius {
			let origin = Vec2::new(self.origin.x, self.origin.z);
			let offset = across - origin;
			if offset.length() > radius.max(0.0) {
				across = origin + offset.normalize_or_zero() * radius.max(0.0);
			}
		}
		let y = self.height.map_or(self.origin.y, |height| eye.y + height);
		Vec3::new(across.x, y, across.y)
	}

	/// How far the part moves from where it was authored for that camera.
	pub fn offset(&self, eye: Vec3, look: Vec3) -> Vec3 {
		self.position(eye, look) - self.origin
	}
}

/// Whether an instance carries a CollectionService tag.
pub fn has_tag(instance: &rbx_dom_weak::Instance, tag: &str) -> bool {
	match instance.properties.get(&rbx_dom_weak::ustr("Tags")) {
		Some(Variant::Tags(tags)) => tags.iter().any(|name| name == tag),
		_ => false,
	}
}

fn attribute(instance: &rbx_dom_weak::Instance, name: &str) -> Option<f32> {
	match instance.properties.get(&rbx_dom_weak::ustr("Attributes")) {
		Some(Variant::Attributes(attributes)) => match attributes.get(name)? {
			Variant::Float64(value) => Some(*value as f32),
			Variant::Float32(value) => Some(*value),
			Variant::Int32(value) => Some(*value as f32),
			Variant::Int64(value) => Some(*value as f32),
			_ => None,
		},
		_ => None,
	}
}

/// The `CameraFollow` part holding `referent` (an emitter, beam or attachment at or under it): its
/// nearest BasePart, when tagged.
pub fn of(document: &Document, referent: Ref) -> Option<Follow> {
	let mut current = document.dom.get_by_ref(referent)?;
	let mut levels = 0u8;
	loop {
		if props::is_a(current, "BasePart") {
			if !has_tag(current, TAG) {
				return None;
			}
			let origin = props::cframe(current, "CFrame").map(|cframe| Vec3::from(cframe.translation))?;
			return Some(Follow {
				origin,
				ahead: attribute(current, "CameraFollowAhead").unwrap_or(0.0),
				height: attribute(current, "CameraFollowHeight"),
				radius: attribute(current, "CameraFollowRadius"),
				levels,
			});
		}
		current = document.dom.get_by_ref(current.parent())?;
		levels = levels.saturating_add(1);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn stands_under_the_camera_at_its_own_height() {
		let follow = Follow { origin: Vec3::new(10.0, 5.0, 10.0), ahead: 0.0, height: None, radius: None, levels: 1 };
		assert_eq!(follow.position(Vec3::new(100.0, 60.0, -40.0), Vec3::new(0.0, -0.7, -0.7)), Vec3::new(100.0, 5.0, -40.0));
	}

	#[test]
	fn ahead_follows_the_look_across_the_ground_and_height_the_camera() {
		let follow = Follow { origin: Vec3::ZERO, ahead: 20.0, height: Some(-10.0), radius: None, levels: 1 };
		let at = follow.position(Vec3::new(0.0, 50.0, 0.0), Vec3::new(0.0, -0.9, -0.1));
		assert!((at - Vec3::new(0.0, 40.0, -20.0)).length() < 1e-4, "{at}");
	}

	#[test]
	fn radius_keeps_it_near_where_it_was_authored() {
		let follow = Follow { origin: Vec3::ZERO, ahead: 0.0, height: None, radius: Some(100.0), levels: 1 };
		assert!((follow.position(Vec3::new(300.0, 10.0, 0.0), Vec3::NEG_Z) - Vec3::new(100.0, 0.0, 0.0)).length() < 1e-4);
	}
}
