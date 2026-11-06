//! Humanoid characters (notes/render.md "Characters"): which parts are a Humanoid's body, and what
//! dresses them — BodyColors, Shirt, Pants and ShirtGraphic, composited into the texture the body
//! parts sample, and CharacterMeshes.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::Ref;
use serde::{Deserialize, Serialize};

use super::load::Document;
use super::props;

/// The limbs a Humanoid of each `Enum.HumanoidRigType` treats as its body.
pub const R6_BODY_PARTS: [&str; 6] = ["Head", "Torso", "Left Arm", "Right Arm", "Left Leg", "Right Leg"];
pub const R15_BODY_PARTS: [&str; 15] = [
	"Head",
	"UpperTorso",
	"LowerTorso",
	"LeftUpperArm",
	"LeftLowerArm",
	"LeftHand",
	"RightUpperArm",
	"RightLowerArm",
	"RightHand",
	"LeftUpperLeg",
	"LeftLowerLeg",
	"LeftFoot",
	"RightUpperLeg",
	"RightLowerLeg",
	"RightFoot",
];

/// Which composited texture of a character a body part samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Layout {
	/// R6: every limb in one 1024 x 512 atlas.
	R6,
	R15Torso,
	R15LeftArm,
	R15RightArm,
	R15LeftLeg,
	R15RightLeg,
}

/// A clothing image and its `Color3` tint (sRGB 0..1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
	pub image: String,
	pub tint: [f32; 3],
}

/// Everything a composited texture is made of (the renderer draws it, `render::composite`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Composite {
	pub layout: Layout,
	/// Body colours (sRGB 0..1) of the regions the layout holds: R6 torso, left arm, right arm, left
	/// leg, right leg; R15 its one limb group.
	pub colors: Vec<[f32; 3]>,
	pub pants: Option<Layer>,
	pub shirt: Option<Layer>,
	pub graphic: Option<Layer>,
}

/// A CharacterMesh: the mesh an R6 limb draws instead of its own (its BaseTextureId and
/// OverlayTextureId show nowhere: characters "cm-torso-base", "cm-torso-overlay").
#[derive(Clone, Debug, PartialEq)]
pub struct LimbMesh {
	pub mesh: String,
}

/// A Humanoid's model as its body parts see it.
#[derive(Debug, PartialEq)]
pub struct Character {
	/// `Enum.HumanoidRigType` (0 R6, 1 R15).
	pub rig_type: u32,
	pub shirt: Option<Layer>,
	pub pants: Option<Layer>,
	pub graphic: Option<Layer>,
	/// The colour of each body part by name: the part's own, or its BodyColors colour.
	pub colors: HashMap<String, Vec3>,
	/// BodyColors: head, torso, left arm, right arm, left leg, right leg (sRGB 0..1).
	pub body_colors: Option<[Vec3; 6]>,
	/// CharacterMeshes by `Enum.BodyPart` (0 Head, 1 Torso, 2 LeftArm, 3 RightArm, 4 LeftLeg, 5 RightLeg).
	pub meshes: HashMap<u32, LimbMesh>,
}

/// The `Enum.BodyPart` (BodyColors' and CharacterMeshes' index) a body part belongs to.
pub fn body_part_index(limb: &str) -> Option<usize> {
	Some(match limb {
		"Head" => 0,
		"Torso" | "UpperTorso" | "LowerTorso" => 1,
		"Left Arm" | "LeftUpperArm" | "LeftLowerArm" | "LeftHand" => 2,
		"Right Arm" | "RightUpperArm" | "RightLowerArm" | "RightHand" => 3,
		"Left Leg" | "LeftUpperLeg" | "LeftLowerLeg" | "LeftFoot" => 4,
		"Right Leg" | "RightUpperLeg" | "RightLowerLeg" | "RightFoot" => 5,
		_ => return None,
	})
}

impl Character {
	fn read(document: &Document, model: &Instance, humanoid: &Instance) -> Character {
		let rig_type = props::enum_value(humanoid, "RigType", 0);
		let mut character = Character { rig_type, shirt: None, pants: None, graphic: None, colors: HashMap::new(), body_colors: None, meshes: HashMap::new() };
		let layer = |instance: &Instance, names: &[&str]| -> Option<Layer> {
			let image = props::content(instance, names)?;
			let tint = props::color(instance, "Color3", Vec3::ONE);
			Some(Layer { image, tint: tint.to_array() })
		};
		let limbs: &[&str] = if rig_type == 1 { &R15_BODY_PARTS } else { &R6_BODY_PARTS };
		for child in model.children().iter().filter_map(|&child| document.dom.get_by_ref(child)) {
			match child.class.as_str() {
				"Shirt" if character.shirt.is_none() => character.shirt = layer(child, &["ShirtTemplateContent", "ShirtTemplate"]),
				"Pants" if character.pants.is_none() => character.pants = layer(child, &["PantsTemplateContent", "PantsTemplate"]),
				"ShirtGraphic" if character.graphic.is_none() => character.graphic = layer(child, &["TextureContent", "Graphic"]),
				"BodyColors" if character.body_colors.is_none() => {
					let names = ["HeadColor3", "TorsoColor3", "LeftArmColor3", "RightArmColor3", "LeftLegColor3", "RightLegColor3"];
					character.body_colors = Some(names.map(|name| props::color(child, name, Vec3::splat(163.0 / 255.0))));
				}
				"CharacterMesh" => {
					let body_part = props::enum_value(child, "BodyPart", 0);
					if let Some(mesh) = props::content(child, &["MeshContent", "MeshId"]) {
						character.meshes.entry(body_part).or_insert(LimbMesh { mesh });
					}
				}
				_ if props::class_is_a(child.class.as_str(), "BasePart") && limbs.contains(&child.name.as_str()) => {
					character.colors.entry(child.name.clone()).or_insert_with(|| props::color(child, "Color", Vec3::splat(163.0 / 255.0)));
				}
				_ => {}
			}
		}
		if let Some(body_colors) = character.body_colors {
			for (limb, color) in character.colors.iter_mut() {
				if let Some(index) = body_part_index(limb) {
					*color = body_colors[index];
				}
			}
		}
		character
	}

	/// A body part's colour as the character shows it.
	pub fn color(&self, limb: &str) -> Option<Vec3> {
		self.colors.get(limb).copied()
	}

	/// The CharacterMesh an R6 limb draws, if any.
	pub fn limb_mesh(&self, limb: &str) -> Option<&LimbMesh> {
		if self.rig_type == 1 || limb == "Head" {
			return None;
		}
		self.meshes.get(&(body_part_index(limb)? as u32))
	}

	/// The composited texture a body part samples, when the character wears clothing.
	pub fn composite(&self, limb: &str) -> Option<Composite> {
		let color = |name: &str| self.colors.get(name).copied().unwrap_or(Vec3::splat(163.0 / 255.0)).to_array();
		let (layout, colors, shirt, pants) = if self.rig_type == 1 {
			// the templates' arm blocks dress a shirt's arms and a pants' legs, their torso both; an R15
			// limb's texture holds its own colour (the parts of a group share it when their colours
			// agree). Pants without a Shirt dress the arms instead of the legs, as a shirt would
			// (characters: r15-pants, r15-pants-grid)
			let (arms, legs) = match &self.shirt {
				Some(shirt) => (Some(shirt), self.pants.as_ref()),
				None => (self.pants.as_ref(), None),
			};
			let (layout, shirt, pants) = match limb {
				"UpperTorso" | "LowerTorso" => (Layout::R15Torso, self.shirt.as_ref(), self.pants.as_ref()),
				"LeftUpperArm" | "LeftLowerArm" | "LeftHand" => (Layout::R15LeftArm, arms, None),
				"RightUpperArm" | "RightLowerArm" | "RightHand" => (Layout::R15RightArm, arms, None),
				"LeftUpperLeg" | "LeftLowerLeg" | "LeftFoot" => (Layout::R15LeftLeg, None, legs),
				"RightUpperLeg" | "RightLowerLeg" | "RightFoot" => (Layout::R15RightLeg, None, legs),
				_ => return None,
			};
			(layout, vec![color(limb)], shirt, pants)
		} else {
			if !matches!(limb, "Torso" | "Left Arm" | "Right Arm" | "Left Leg" | "Right Leg") {
				return None;
			}
			(Layout::R6, ["Torso", "Left Arm", "Right Arm", "Left Leg", "Right Leg"].iter().map(|name| color(name)).collect(), self.shirt.as_ref(), self.pants.as_ref())
		};
		// a ShirtGraphic on the torso's front, R6's or R15's (characters: r15-graphic)
		let graphic = matches!(layout, Layout::R6 | Layout::R15Torso).then(|| self.graphic.clone()).flatten();
		if shirt.is_none() && pants.is_none() && graphic.is_none() {
			return None;
		}
		Some(Composite { layout, colors, pants: pants.cloned(), shirt: shirt.cloned(), graphic })
	}
}

/// Characters found while extracting, by model.
#[derive(Default)]
pub struct Characters {
	models: HashMap<Ref, Option<Arc<Character>>>,
}

impl Characters {
	/// The character a part is a body part of (a limb of its Humanoid's rig type, a child of the
	/// Humanoid's model).
	pub fn body_part(&mut self, document: &Document, instance: &Instance) -> Option<Arc<Character>> {
		let parent = instance.parent();
		let character = self
			.models
			.entry(parent)
			.or_insert_with(|| {
				let model = document.dom.get_by_ref(parent)?;
				let humanoid = model.children().iter().filter_map(|&child| document.dom.get_by_ref(child)).find(|child| child.class == "Humanoid")?;
				Some(Arc::new(Character::read(document, model, humanoid)))
			})
			.clone()?;
		let limbs: &[&str] = if character.rig_type == 1 { &R15_BODY_PARTS } else { &R6_BODY_PARTS };
		limbs.contains(&instance.name.as_str()).then_some(character)
	}
}
