//! Trails: a ribbon left behind by two Attachments as they move (notes/render.md "Trails"). A model at
//! rest has no motion, so a trail draws only what a playback (scene::playback) recorded for it.

use glam::{Affine3A, Vec3};
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::{Ref, Variant};

use super::load::Document;
use super::props;

/// Where the trail's attachments were at one moment: seconds after the playback started, and the
/// world positions of Attachment0 and Attachment1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailPoint {
	pub time: f32,
	pub position0: Vec3,
	pub position1: Vec3,
	/// The first point after the trail was enabled again: no segment joins it to the one before
	/// (trail-shape gap: two strips).
	pub starts_strip: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trail {
	pub path: String,
	/// The Trail instance (its playback's events).
	pub referent: Ref,
	/// The Attachments (their motion is the trail) and their world CFrames at the moment drawn.
	pub attachment0: Ref,
	pub attachment1: Ref,
	pub cframe0: Affine3A,
	pub cframe1: Affine3A,
	/// `Trail.Color` keypoints over a point's life (time 0..1, sRGB colour 0..1).
	pub color: Vec<(f32, Vec3)>,
	/// `Trail.Transparency` and `Trail.WidthScale` over a point's life: (time, value, envelope).
	pub transparency: Vec<(f32, f32, f32)>,
	pub width_scale: Vec<(f32, f32, f32)>,
	pub brightness: f32,
	pub light_emission: f32,
	pub light_influence: f32,
	/// Seconds a point lasts.
	pub lifetime: f32,
	/// Studs the attachments move before the next point; MaxLength caps the whole trail (0: none).
	pub min_length: f32,
	pub max_length: f32,
	pub face_camera: bool,
	pub enabled: bool,
	pub texture: Option<String>,
	pub texture_length: f32,
	/// `Enum.TextureMode`: 0 Stretch, 1 Wrap, 2 Static.
	pub texture_mode: u32,
	/// The recorded points, oldest first, the newest at the moment drawn (the attachments' current
	/// place, the head); empty for a trail that never moved.
	pub points: Vec<TrailPoint>,
	/// The moment drawn, seconds after the playback started (each point's age is `time - point.time`).
	pub time: f32,
	/// The distance along the trail of its oldest point from where the trail began (points dropped
	/// for age or MaxLength), for Static textures.
	pub dropped_length: f32,
}

fn sequence(instance: &Instance, name: &str, fallback: f32) -> Vec<(f32, f32, f32)> {
	match props::get(instance, name) {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.iter().map(|key| (key.time, key.value, key.envelope)).collect(),
		Some(Variant::Float32(value)) => vec![(0.0, *value, 0.0), (1.0, *value, 0.0)],
		_ => vec![(0.0, fallback, 0.0), (1.0, fallback, 0.0)],
	}
}

/// A Trail instance, or `None` when it is missing an attachment or an attachment is not drawn.
pub fn read(document: &Document, referent: Ref, instance: &Instance, warnings: &mut Vec<String>) -> Option<Trail> {
	let path = document.path_of(referent);
	let attachment = |name: &str| -> Option<(Ref, Affine3A)> {
		let target = props::reference(instance, name)?;
		if !super::beams::drawn(document, target) {
			return None;
		}
		Some((target, super::beams::world_cframe(document, target)?))
	};
	let (Some((attachment0, cframe0)), Some((attachment1, cframe1))) = (attachment("Attachment0"), attachment("Attachment1")) else {
		warnings.push(format!("{path}: trail without both attachments (not drawn)"));
		return None;
	};
	let color = super::particles::color_keys(instance, "Color");
	Some(Trail {
		path,
		referent,
		attachment0,
		attachment1,
		cframe0,
		cframe1,
		color,
		transparency: sequence(instance, "Transparency", 0.5),
		width_scale: sequence(instance, "WidthScale", 1.0),
		brightness: props::f32(instance, "Brightness", 1.0),
		light_emission: props::f32(instance, "LightEmission", 0.0),
		light_influence: props::f32(instance, "LightInfluence", 1.0),
		lifetime: props::f32(instance, "Lifetime", 2.0),
		min_length: props::f32(instance, "MinLength", 0.1),
		max_length: props::f32(instance, "MaxLength", 0.0),
		face_camera: props::bool(instance, "FaceCamera", false),
		enabled: props::bool(instance, "Enabled", true),
		texture: props::content(instance, &["TextureContent", "Texture"]),
		texture_length: props::f32(instance, "TextureLength", 1.0),
		texture_mode: props::enum_value(instance, "TextureMode", 0),
		points: Vec::new(),
		time: 0.0,
		dropped_length: 0.0,
	})
}
