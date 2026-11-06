//! Typed property reads with the reflection database's class defaults as the fallback, so models
//! written without default values (Rojo, Lune) read like the ones Studio saves.

use glam::{Affine3A, Mat3A, Vec3, Vec3A};
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::{CFrame, Variant};
use rbx_reflection::ReflectionDatabase;

pub fn database() -> &'static ReflectionDatabase<'static> {
	rbx_reflection_database::get_bundled()
}

/// The value of an instance's property, or the default of its class (or a superclass).
pub fn get<'a>(instance: &'a Instance, name: &str) -> Option<&'a Variant> {
	let name = rbx_dom_weak::ustr(name);
	if let Some(value) = instance.properties.get(&name) {
		return Some(value);
	}
	default(instance.class, name)
}

/// A class's default for a property, remembered per thread: the database's walk up the superclasses
/// took 0.6 us a lookup, a part's extraction a few (a 5,000-part map's MeshParts lack
/// MaterialVariantSerialized, its Parts a dozen properties).
fn default(class: rbx_dom_weak::Ustr, name: rbx_dom_weak::Ustr) -> Option<&'static Variant> {
	thread_local! {
		static DEFAULTS: std::cell::RefCell<std::collections::HashMap<(rbx_dom_weak::Ustr, rbx_dom_weak::Ustr), Option<&'static Variant>>> = Default::default();
	}
	DEFAULTS.with_borrow_mut(|defaults| {
		*defaults.entry((class, name)).or_insert_with(|| {
			let database = database();
			database.classes.get(class.as_str()).and_then(|descriptor| database.find_default_property(descriptor, name.as_str()))
		})
	})
}

pub fn f32(instance: &Instance, name: &str, fallback: f32) -> f32 {
	match get(instance, name) {
		Some(Variant::Float32(value)) => *value,
		Some(Variant::Float64(value)) => *value as f32,
		Some(Variant::Int32(value)) => *value as f32,
		Some(Variant::Int64(value)) => *value as f32,
		_ => fallback,
	}
}

pub fn i32(instance: &Instance, name: &str, fallback: i32) -> i32 {
	match get(instance, name) {
		Some(Variant::Int32(value)) => *value,
		Some(Variant::Int64(value)) => *value as i32,
		Some(Variant::Float32(value)) => *value as i32,
		Some(Variant::Float64(value)) => *value as i32,
		_ => fallback,
	}
}

pub fn bool(instance: &Instance, name: &str, fallback: bool) -> bool {
	match get(instance, name) {
		Some(Variant::Bool(value)) => *value,
		_ => fallback,
	}
}

pub fn vec3(instance: &Instance, name: &str, fallback: Vec3) -> Vec3 {
	match get(instance, name) {
		Some(Variant::Vector3(value)) => Vec3::new(value.x, value.y, value.z),
		Some(Variant::Vector3int16(value)) => Vec3::new(value.x as f32, value.y as f32, value.z as f32),
		_ => fallback,
	}
}

pub fn vec2(instance: &Instance, name: &str, fallback: [f32; 2]) -> [f32; 2] {
	match get(instance, name) {
		Some(Variant::Vector2(value)) => [value.x, value.y],
		Some(Variant::Vector2int16(value)) => [value.x as f32, value.y as f32],
		_ => fallback,
	}
}

/// A UDim2 as (x scale, x offset, y scale, y offset).
pub fn udim2(instance: &Instance, name: &str) -> Option<[f32; 4]> {
	match get(instance, name) {
		Some(Variant::UDim2(value)) => Some([value.x.scale, value.x.offset as f32, value.y.scale, value.y.offset as f32]),
		_ => None,
	}
}

/// Whether a property of an instance holds an instance reference (a beam's Attachment0, a model's
/// PrimaryPart, a GUI's Adornee): a Ref by the reflection database, or one the file holds.
pub fn is_reference(instance: &Instance, name: &str) -> bool {
	if matches!(instance.properties.get(&rbx_dom_weak::ustr(name)), Some(Variant::Ref(_))) {
		return true;
	}
	let database = database();
	let mut current = database.classes.get(instance.class.as_str());
	while let Some(descriptor) = current {
		if let Some(property) = descriptor.properties.get(name) {
			return matches!(property.data_type, rbx_reflection::DataType::Value(rbx_dom_weak::types::VariantType::Ref));
		}
		current = descriptor.superclass.and_then(|parent| database.classes.get(parent));
	}
	false
}

/// A set (non-nil) reference property.
pub fn reference(instance: &Instance, name: &str) -> Option<rbx_dom_weak::types::Ref> {
	match instance.properties.get(&rbx_dom_weak::ustr(name)) {
		Some(Variant::Ref(value)) if value.is_some() => Some(*value),
		_ => None,
	}
}

/// A colour property as sRGB components in 0..1 (Color3 or Color3uint8, BrickColor as a fallback).
pub fn color(instance: &Instance, name: &str, fallback: Vec3) -> Vec3 {
	match get(instance, name) {
		Some(Variant::Color3(value)) => Vec3::new(value.r, value.g, value.b),
		Some(Variant::Color3uint8(value)) => Vec3::new(value.r as f32, value.g as f32, value.b as f32) / 255.0,
		Some(Variant::BrickColor(value)) => {
			let rgb = value.to_color3uint8();
			Vec3::new(rgb.r as f32, rgb.g as f32, rgb.b as f32) / 255.0
		}
		_ => fallback,
	}
}

pub fn enum_value(instance: &Instance, name: &str, fallback: u32) -> u32 {
	match get(instance, name) {
		Some(Variant::Enum(value)) => value.to_u32(),
		Some(Variant::EnumItem(item)) => item.value,
		Some(Variant::Int32(value)) => *value as u32,
		_ => fallback,
	}
}

pub fn string(instance: &Instance, name: &str) -> Option<String> {
	match get(instance, name) {
		Some(Variant::String(value)) => Some(value.clone()),
		_ => None,
	}
}

/// The first non-empty content URI among `names` (e.g. `["MeshContent", "MeshId"]`) that the file
/// holds, the class's defaults only when it holds none of them: an emitter saved with the old Texture
/// alone draws that image, not TextureContent's default sparkles (particle-orient-texture), and one
/// saved with them empty draws none.
pub fn content(instance: &Instance, names: &[&str]) -> Option<String> {
	let uri = |value: &Variant| -> Option<String> {
		let uri = match value {
			Variant::ContentId(value) => value.as_str().to_string(),
			Variant::Content(value) => value.as_uri().unwrap_or_default().to_string(),
			Variant::String(value) => value.clone(),
			_ => return None,
		};
		let uri = uri.trim();
		(!uri.is_empty()).then(|| uri.to_string())
	};
	let own: Vec<&Variant> = names.iter().filter_map(|name| instance.properties.get(&rbx_dom_weak::ustr(name))).collect();
	if !own.is_empty() {
		return own.into_iter().find_map(uri);
	}
	names.iter().find_map(|name| get(instance, name).and_then(uri))
}

pub fn binary(instance: &Instance, name: &str) -> Option<Vec<u8>> {
	match instance.properties.get(&rbx_dom_weak::ustr(name))? {
		Variant::BinaryString(value) => {
			let bytes: &[u8] = value.as_ref();
			(!bytes.is_empty()).then(|| bytes.to_vec())
		}
		Variant::SharedString(value) => {
			let bytes = value.data();
			(!bytes.is_empty()).then(|| bytes.to_vec())
		}
		Variant::String(value) => (!value.is_empty()).then(|| value.as_bytes().to_vec()),
		// an unpublished union's SolidMeshHolder: the data inline in a shared string
		Variant::NetAssetRef(value) => {
			let bytes = value.data();
			(!bytes.is_empty()).then(|| bytes.to_vec())
		}
		_ => None,
	}
}

pub fn cframe(instance: &Instance, name: &str) -> Option<Affine3A> {
	match get(instance, name) {
		Some(Variant::CFrame(value)) => Some(to_affine(value)),
		Some(Variant::OptionalCFrame(Some(value))) => Some(to_affine(value)),
		_ => None,
	}
}

/// A Roblox CFrame as an affine transform. rbx_types stores the rotation by rows (R00 R01 R02 is
/// `orientation.x`); glam's matrices are built from columns.
pub fn to_affine(value: &CFrame) -> Affine3A {
	let rows = &value.orientation;
	let matrix = Mat3A::from_cols(
		Vec3A::new(rows.x.x, rows.y.x, rows.z.x),
		Vec3A::new(rows.x.y, rows.y.y, rows.z.y),
		Vec3A::new(rows.x.z, rows.y.z, rows.z.z),
	);
	Affine3A::from_mat3_translation(matrix.into(), Vec3::new(value.position.x, value.position.y, value.position.z))
}

/// The name of an enum item, e.g. `enum_name("Material", 256) == Some("Plastic")`.
pub fn enum_name(enum_name: &str, value: u32) -> Option<&'static str> {
	let descriptor = database().enums.get(enum_name)?;
	descriptor.items.iter().find(|(_, item)| **item == value).map(|(name, _)| *name)
}

pub fn is_a(instance: &Instance, class: &str) -> bool {
	class_is_a(instance.class.as_str(), class)
}

pub fn class_is_a(name: &str, class: &str) -> bool {
	if name == class {
		return true;
	}
	let database = database();
	let Some(descriptor) = database.classes.get(name) else {
		return false;
	};
	let mut current = descriptor.superclass;
	while let Some(parent) = current {
		if parent == class {
			return true;
		}
		current = database.classes.get(parent).and_then(|descriptor| descriptor.superclass);
	}
	false
}

#[cfg(test)]
mod tests {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Content, ContentId};

	use super::content;

	/// A file's own texture properties before the class's defaults (particle-orient-texture)
	#[test]
	fn content_prefers_what_the_file_holds() {
		let names = ["TextureContent", "Texture"];
		let dom = |builder: InstanceBuilder| rbx_dom_weak::WeakDom::new(builder);
		// the old property alone: its image, not TextureContent's default
		let old = dom(InstanceBuilder::new("ParticleEmitter").with_property("Texture", ContentId::from("rbxassetid://1")));
		assert_eq!(content(old.root(), &names).as_deref(), Some("rbxassetid://1"));
		// saved empty: none
		let empty = dom(InstanceBuilder::new("ParticleEmitter").with_property("TextureContent", Content::none()));
		assert_eq!(content(empty.root(), &names), None);
		// both: the first that is not empty
		let both = dom(InstanceBuilder::new("ParticleEmitter").with_property("TextureContent", Content::none()).with_property("Texture", ContentId::from("rbxassetid://2")));
		assert_eq!(content(both.root(), &names).as_deref(), Some("rbxassetid://2"));
		// neither: the class's default (Roblox's sparkles)
		let neither = dom(InstanceBuilder::new("ParticleEmitter"));
		assert!(content(neither.root(), &names).is_some_and(|uri| uri.contains("sparkles")));
	}
}
