//! Building models in code: calibration fixtures and tests.

use glam::{Affine3A, Vec3};
use rbx_dom_weak::types::{CFrame, Color3uint8, Enum, Matrix3, Ref, Variant, Vector3};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::load::Document;

pub fn cframe(transform: Affine3A) -> CFrame {
	let m = transform.matrix3;
	let p = transform.translation;
	CFrame::new(
		Vector3::new(p.x, p.y, p.z),
		Matrix3::new(Vector3::new(m.x_axis.x, m.y_axis.x, m.z_axis.x), Vector3::new(m.x_axis.y, m.y_axis.y, m.z_axis.y), Vector3::new(m.x_axis.z, m.y_axis.z, m.z_axis.z)),
	)
}

/// A CFrame at `position` whose front (-Z) faces `direction`.
pub fn facing(position: Vec3, direction: Vec3) -> Affine3A {
	let forward = direction.normalize();
	let up = if forward.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
	let right = forward.cross(up).normalize();
	let up = right.cross(forward);
	Affine3A::from_cols(right.into(), up.into(), (-forward).into(), position.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct PartSpec {
	pub class: &'static str,
	pub name: String,
	pub cframe: Affine3A,
	pub size: Vec3,
	pub color: [u8; 3],
	pub material: u32,
	/// `Enum.PartType` for Part
	pub shape: Option<u32>,
	pub transparency: f32,
	pub reflectance: f32,
	pub cast_shadow: bool,
	/// Every surface Smooth (else the class defaults, Studs on top for Part)
	pub smooth: bool,
}

impl Default for PartSpec {
	fn default() -> Self {
		Self {
			class: "Part",
			name: "Part".into(),
			cframe: Affine3A::IDENTITY,
			size: Vec3::new(4.0, 1.0, 2.0),
			color: [163, 162, 165],
			material: 272,
			shape: None,
			transparency: 0.0,
			reflectance: 0.0,
			cast_shadow: true,
			smooth: true,
		}
	}
}

pub struct ModelBuilder {
	pub dom: WeakDom,
	pub model: Ref,
}

impl ModelBuilder {
	pub fn new(name: &str) -> Self {
		let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
		let root = dom.root_ref();
		let model = dom.insert(root, InstanceBuilder::new("Model").with_name(name));
		Self { dom, model }
	}

	pub fn part(&mut self, spec: &PartSpec) -> Ref {
		let mut builder = InstanceBuilder::new(spec.class)
			.with_name(spec.name.clone())
			.with_property("CFrame", cframe(spec.cframe))
			.with_property("Size", Vector3::new(spec.size.x, spec.size.y, spec.size.z))
			.with_property("Color", Color3uint8::new(spec.color[0], spec.color[1], spec.color[2]))
			.with_property("Material", Enum::from_u32(spec.material))
			.with_property("Transparency", spec.transparency)
			.with_property("Reflectance", spec.reflectance)
			.with_property("CastShadow", spec.cast_shadow)
			.with_property("Anchored", true);
		if let Some(shape) = spec.shape {
			builder = builder.with_property("Shape", Enum::from_u32(shape));
		}
		if spec.smooth {
			for face in ["TopSurface", "BottomSurface", "FrontSurface", "BackSurface", "LeftSurface", "RightSurface"] {
				builder = builder.with_property(face, Enum::from_u32(0));
			}
		}
		self.dom.insert(self.model, builder)
	}

	pub fn child(&mut self, parent: Ref, builder: InstanceBuilder) -> Ref {
		self.dom.insert(parent, builder)
	}

	pub fn set(&mut self, instance: Ref, name: &str, value: impl Into<Variant>) {
		if let Some(instance) = self.dom.get_by_ref_mut(instance) {
			instance.properties.insert(rbx_dom_weak::ustr(name), value.into());
		}
	}

	pub fn to_rbxm(&self) -> anyhow::Result<Vec<u8>> {
		let mut bytes = Vec::new();
		rbx_binary::to_writer(&mut bytes, &self.dom, &[self.model]).map_err(|error| anyhow::anyhow!("{error}"))?;
		Ok(bytes)
	}

	pub fn into_document(self) -> Document {
		let model = self.model;
		let mut document = Document::new(self.dom);
		document.roots = vec![model];
		document
	}
}
