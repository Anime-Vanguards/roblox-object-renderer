//! Beams: a ribbon along a cubic Bézier curve between two Attachments (notes/render.md "Beams").

use glam::{Affine3A, Vec3};
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::{Ref, Variant};

use super::load::Document;
use super::props;

#[derive(Clone, Debug, PartialEq)]
pub struct Beam {
	pub path: String,
	pub referent: Ref,
	/// World CFrames of Attachment0 and Attachment1 (the parent part's CFrame times the attachment's).
	pub attachment0: Affine3A,
	pub attachment1: Affine3A,
	/// `Beam.Color` keypoints: (time 0..1, sRGB colour 0..1).
	pub color: Vec<(f32, Vec3)>,
	/// `Beam.Transparency` keypoints: (time 0..1, value, envelope).
	pub transparency: Vec<(f32, f32, f32)>,
	pub brightness: f32,
	pub light_emission: f32,
	pub light_influence: f32,
	pub texture: Option<String>,
	pub texture_length: f32,
	/// `Enum.TextureMode`: 0 Stretch, 1 Wrap, 2 Static.
	pub texture_mode: u32,
	/// Texture cycles per second (ror draws time 0, where the texture rests as at TextureSpeed 0).
	pub texture_speed: f32,
	/// How far TextureSpeed has scrolled the texture, in cycles (a playback's moment; 0 at rest).
	pub texture_offset: f32,
	pub width0: f32,
	pub width1: f32,
	pub curve_size0: f32,
	pub curve_size1: f32,
	pub segments: u32,
	pub face_camera: bool,
	pub z_offset: f32,
}

impl Beam {
	/// `Color` at `t` (0 at Attachment0, 1 at Attachment1), sRGB: linear between keypoints.
	pub fn color_at(&self, t: f32) -> Vec3 {
		sample(&self.color, t, |&(time, color)| (time, color), Vec3::ONE, |a, b, f| a.lerp(b, f))
	}

	/// `Transparency` at `t`: linear between keypoints (envelopes ignored).
	pub fn transparency_at(&self, t: f32) -> f32 {
		sample(&self.transparency, t, |&(time, value, _)| (time, value), 0.0, |a, b, f| a + (b - a) * f)
	}
}

fn sample<K, V: Copy>(keypoints: &[K], t: f32, unpack: impl Fn(&K) -> (f32, V), fallback: V, lerp: impl Fn(V, V, f32) -> V) -> V {
	let Some(first) = keypoints.first() else {
		return fallback;
	};
	let (mut time0, mut value0) = unpack(first);
	if t <= time0 {
		return value0;
	}
	for keypoint in &keypoints[1..] {
		let (time1, value1) = unpack(keypoint);
		if t <= time1 {
			let span = time1 - time0;
			return if span > 1e-6 { lerp(value0, value1, (t - time0) / span) } else { value1 };
		}
		(time0, value0) = (time1, value1);
	}
	value0
}

/// A Beam instance, or `None` when Studio draws nothing for it: disabled, or an attachment missing,
/// outside what is drawn, or not on a part.
pub fn read(document: &Document, referent: Ref, instance: &Instance, warnings: &mut Vec<String>) -> Option<Beam> {
	if !props::bool(instance, "Enabled", true) {
		return None;
	}
	let path = document.path_of(referent);
	let attachment = |name: &str| -> Option<Affine3A> {
		let Some(Variant::Ref(target)) = props::get(instance, name) else {
			return None;
		};
		if target.is_none() || !drawn(document, *target) {
			return None;
		}
		world_cframe(document, *target)
	};
	let (Some(attachment0), Some(attachment1)) = (attachment("Attachment0"), attachment("Attachment1")) else {
		warnings.push(format!("{path}: beam without both attachments (not drawn)"));
		return None;
	};
	let color = super::particles::color_keys(instance, "Color");
	let transparency = match props::get(instance, "Transparency") {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.iter().map(|key| (key.time, key.value, key.envelope)).collect(),
		Some(Variant::Float32(value)) => vec![(0.0, *value, 0.0)],
		_ => vec![(0.0, 0.5, 0.0)],
	};
	Some(Beam {
		path,
		referent,
		attachment0,
		attachment1,
		color,
		transparency,
		brightness: props::f32(instance, "Brightness", 1.0),
		light_emission: props::f32(instance, "LightEmission", 0.0),
		light_influence: props::f32(instance, "LightInfluence", 0.0),
		texture: props::content(instance, &["TextureContent", "Texture"]),
		texture_length: props::f32(instance, "TextureLength", 1.0),
		texture_mode: props::enum_value(instance, "TextureMode", 0),
		texture_speed: props::f32(instance, "TextureSpeed", 1.0),
		texture_offset: 0.0,
		width0: props::f32(instance, "Width0", 1.0),
		width1: props::f32(instance, "Width1", 1.0),
		curve_size0: props::f32(instance, "CurveSize0", 0.0),
		curve_size1: props::f32(instance, "CurveSize1", 0.0),
		segments: props::i32(instance, "Segments", 10).max(1) as u32,
		face_camera: props::bool(instance, "FaceCamera", false),
		z_offset: props::f32(instance, "ZOffset", 0.0),
	})
}

/// Whether an instance is part of what is drawn: under one of the document's roots (a model's
/// instances, a place's Workspace children), or anywhere in a place's Workspace (its Terrain too).
pub(super) fn drawn(document: &Document, referent: Ref) -> bool {
	let root = document.dom.root_ref();
	let mut current = referent;
	while let Some(instance) = document.dom.get_by_ref(current) {
		if document.roots.contains(&current) || (document.is_place && instance.class == "Workspace") {
			return true;
		}
		if current == root {
			break;
		}
		current = instance.parent();
	}
	false
}

/// An Attachment's world CFrame: its CFrame relative to its parent part, or to its parent
/// attachment's world CFrame (a Bone's to its parent bone's, as for any attachment inside another:
/// in the capture Studio an attachment two levels inside a part reads WorldCFrame = part CFrame ·
/// outer CFrame · inner CFrame, to 1e-7), or to the world under Terrain. 12 % of one game's emitters sit
/// on such inner attachments, and the ends of 45 % of its beams and 20 % of its trails (vfx-attack
/// auto runs: an effect's trails and another's beams were missing). Under a Model, or a Folder in one,
/// it is relative to that model's PrimaryPart, else to the centre of the box its parts fill, not to
/// its pivot (vfx-attachment-model: still particles from attachments of Models standing 8 studs
/// aside, of a Folder and of the experiment's own model, chained and turned, and a beam between two,
/// drawn by none in ror; vfx-model-frames: a light and a particle on a Model's attachment and in a
/// Folder of it, its WorldPivotData 8 studs to one side and its parts 6 and 12 studs to the other,
/// both at the box's centre in Studio, at the PrimaryPart once it has one; 325 of one game's emitters in 7
/// effects among them).
pub(crate) fn world_cframe(document: &Document, referent: Ref) -> Option<Affine3A> {
	let attachment = document.dom.get_by_ref(referent)?;
	if !props::is_a(attachment, "Attachment") {
		return None;
	}
	let local = props::cframe(attachment, "CFrame").unwrap_or(Affine3A::IDENTITY);
	let parent = document.dom.get_by_ref(attachment.parent())?;
	if parent.class == "Terrain" {
		Some(local)
	} else if props::is_a(parent, "BasePart") {
		Some(props::cframe(parent, "CFrame")? * local)
	} else if props::is_a(parent, "Attachment") {
		Some(world_cframe(document, attachment.parent())? * local)
	} else {
		Some(frame_above(document, attachment.parent()) * local)
	}
}

/// The frame of the nearest part or model at or above an instance (through Folders): a part's
/// CFrame, a model's PrimaryPart's CFrame, else the centre of the box its parts fill (its
/// WorldPivotData does not count: vfx-model-frames), the world's above them all.
fn frame_above(document: &Document, referent: Ref) -> Affine3A {
	let mut current = referent;
	while let Some(instance) = document.dom.get_by_ref(current) {
		if props::is_a(instance, "BasePart") {
			return props::cframe(instance, "CFrame").unwrap_or(Affine3A::IDENTITY);
		}
		if props::is_a(instance, "Model") {
			if let Some(primary) = props::reference(instance, "PrimaryPart").and_then(|part| document.dom.get_by_ref(part)).and_then(|part| props::cframe(part, "CFrame")) {
				return primary;
			}
			return Affine3A::from_translation(parts_box_center(document, current));
		}
		if current == document.dom.root_ref() {
			break;
		}
		current = instance.parent();
	}
	Affine3A::IDENTITY
}

/// The centre of the world-aligned box the parts under an instance fill (their corners), the origin
/// without parts.
fn parts_box_center(document: &Document, referent: Ref) -> Vec3 {
	let (mut low, mut high) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
	let mut stack = vec![referent];
	while let Some(current) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(current) else {
			continue;
		};
		stack.extend(instance.children().iter().copied());
		if !props::is_a(instance, "BasePart") {
			continue;
		}
		let Some(cframe) = props::cframe(instance, "CFrame") else {
			continue;
		};
		let half = props::vec3(instance, "Size", Vec3::new(4.0, 1.0, 2.0)) * 0.5;
		for corner in 0..8 {
			let sign = Vec3::new(if corner & 1 == 0 { -1.0 } else { 1.0 }, if corner & 2 == 0 { -1.0 } else { 1.0 }, if corner & 4 == 0 { -1.0 } else { 1.0 });
			let point = cframe.transform_point3(half * sign);
			low = low.min(point);
			high = high.max(point);
		}
	}
	if low.x > high.x { Vec3::ZERO } else { (low + high) * 0.5 }
}
