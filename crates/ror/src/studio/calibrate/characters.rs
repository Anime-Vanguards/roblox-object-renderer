//! Character experiments (notes/render.md "Characters"): Studio's own R6 and R15 rigs
//! (`content/avatar/character.rbxm`, `characterR15.rbxm`) dressed in Shirts, Pants and ShirtGraphics,
//! tinted, with BodyColors and CharacterMeshes, each rig in its own Model under a Folder root.

use anyhow::{Context, Result};
use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, Color3uint8, Content, Enum, Ref, Variant};

use super::{Experiment, Run};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, cframe};
use crate::scene::load::Document;
use crate::scene::props;

pub const UNIFORM_202: &str = "rbxassetid://7547288112";

/// Studio's rig of a rig type, loaded from its content folder.
pub(super) fn studio_rig(r15: bool) -> Result<Document> {
	let uri = if r15 { "rbxasset://avatar/characterR15.rbxm" } else { "rbxasset://avatar/character.rbxm" };
	let config = crate::output::asset_config(crate::output::state_dir(), false);
	let fetched = ror_assets::Assets::new(config).fetch(uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
	crate::scene::load::read_bytes(&fetched.bytes)
}

/// A builder whose root is a Folder (each rig is a Model of its own).
fn folder_root(name: &str) -> ModelBuilder {
	let mut model = ModelBuilder::new(name);
	if let Some(root) = model.dom.get_by_ref_mut(model.model) {
		root.class = rbx_dom_weak::ustr("Folder");
	}
	model
}

/// Flat ambient light only: colours read directly (x = 0.98 · albedo).
pub fn flat() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// What a rig wears.
#[derive(Clone, Default)]
pub struct Outfit {
	/// Every body part's Color (else Studio's rig colour, 163, 162, 165).
	pub color: Option<[u8; 3]>,
	pub shirt: Option<(&'static str, [f32; 3])>,
	pub pants: Option<(&'static str, [f32; 3])>,
	pub graphic: Option<(&'static str, [f32; 3])>,
	/// A BodyColors instance: head, torso, left arm, right arm, left leg, right leg.
	pub body_colors: Option<[[u8; 3]; 6]>,
	/// CharacterMeshes: (Enum.BodyPart, mesh, base texture, overlay texture).
	pub meshes: Vec<(u32, &'static str, Option<&'static str>, Option<&'static str>)>,
	/// The Torso's own Decal (R6 "roblox", Front) texture.
	pub torso_decal: Option<&'static str>,
	/// A body part made another size (about its centre).
	pub resize: Option<(&'static str, [f32; 3])>,
	/// Leave the parts as saved (unanchored in Studio's rigs) instead of anchoring them.
	pub unanchored: bool,
	/// Keep the rig's own rotation (only move it) instead of turning it to face -Z.
	pub unturned: bool,
}

/// Puts a copy of Studio's rig, anchored and recoloured, into the experiment with its Torso (R6) or
/// UpperTorso (R15) at `at`, facing -Z, and dresses it. Returns the rig's Model.
pub fn add_rig(model: &mut ModelBuilder, source: &Document, name: &str, at: Vec3, outfit: &Outfit) -> Result<Ref> {
	let source_root = *source.roots.first().context("empty rig file")?;
	let copy = source.dom.clone_into_external(source_root, &mut model.dom);
	let root = model.model;
	model.dom.transfer_within(copy, root);
	let mut descendants = Vec::new();
	let mut stack = vec![copy];
	while let Some(referent) = stack.pop() {
		let Some(instance) = model.dom.get_by_ref(referent) else {
			continue;
		};
		stack.extend(instance.children().iter().copied());
		descendants.push(referent);
	}
	let torso = model
		.dom
		.get_by_ref(copy)
		.context("copied rig")?
		.children()
		.iter()
		.filter_map(|&child| model.dom.get_by_ref(child))
		.find(|instance| instance.name == "Torso" || instance.name == "UpperTorso")
		.and_then(|torso| props::cframe(torso, "CFrame"))
		.context("rig without a torso")?;
	// Studio's R6 rig faces +Z, its R15 rig -Z: turn both to face -Z with the torso at `at`
	let turn = if outfit.unturned { Affine3A::IDENTITY } else { Affine3A::from_mat3(glam::Mat3::from(torso.matrix3.inverse())) };
	let place = Affine3A::from_translation(at) * turn * Affine3A::from_translation(-Vec3::from(torso.translation));
	for &referent in &descendants {
		let Some(instance) = model.dom.get_by_ref_mut(referent) else {
			continue;
		};
		if !props::class_is_a(instance.class.as_str(), "BasePart") {
			continue;
		}
		if let Some(Variant::CFrame(value)) = instance.properties.get(&rbx_dom_weak::ustr("CFrame")) {
			let moved = place * props::to_affine(value);
			instance.properties.insert(rbx_dom_weak::ustr("CFrame"), cframe(moved).into());
		}
		if !outfit.unanchored {
			instance.properties.insert(rbx_dom_weak::ustr("Anchored"), Variant::Bool(true));
		}
		if let Some((name, [x, y, z])) = outfit.resize
			&& instance.name == name
		{
			instance.properties.insert(rbx_dom_weak::ustr("Size"), Variant::Vector3(rbx_dom_weak::types::Vector3::new(x, y, z)));
		}
		if let Some([r, g, b]) = outfit.color
			&& instance.name != "HumanoidRootPart"
		{
			instance.properties.insert(rbx_dom_weak::ustr("Color"), Variant::Color3uint8(Color3uint8::new(r, g, b)));
		}
	}
	if let Some(instance) = model.dom.get_by_ref_mut(copy) {
		instance.name = name.to_string();
		instance.properties.remove(&rbx_dom_weak::ustr("WorldPivotData"));
	}
	let content = |uri: &str| Variant::Content(Content::from_uri(uri));
	let color3 = |[r, g, b]: [f32; 3]| Variant::Color3(Color3::new(r, g, b));
	if let Some((template, tint)) = outfit.shirt {
		model.child(copy, InstanceBuilder::new("Shirt").with_name("Shirt").with_property("ShirtTemplateContent", content(template)).with_property("Color3", color3(tint)));
	}
	if let Some((template, tint)) = outfit.pants {
		model.child(copy, InstanceBuilder::new("Pants").with_name("Pants").with_property("PantsTemplateContent", content(template)).with_property("Color3", color3(tint)));
	}
	if let Some((texture, tint)) = outfit.graphic {
		model.child(copy, InstanceBuilder::new("ShirtGraphic").with_name("Shirt Graphic").with_property("TextureContent", content(texture)).with_property("Color3", color3(tint)));
	}
	if let Some(colors) = outfit.body_colors {
		let names = ["HeadColor3", "TorsoColor3", "LeftArmColor3", "RightArmColor3", "LeftLegColor3", "RightLegColor3"];
		let mut builder = InstanceBuilder::new("BodyColors").with_name("Body Colors");
		for (name, [r, g, b]) in names.iter().zip(colors) {
			builder = builder.with_property(*name, Variant::Color3(Color3::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)));
		}
		model.child(copy, builder);
	}
	for (body_part, mesh, base, overlay) in &outfit.meshes {
		let mut builder = InstanceBuilder::new("CharacterMesh").with_name("CharacterMesh").with_property("BodyPart", Enum::from_u32(*body_part)).with_property("MeshContent", content(mesh));
		if let Some(base) = base {
			builder = builder.with_property("BaseTextureContent", content(base));
		}
		if let Some(overlay) = overlay {
			builder = builder.with_property("OverlayTextureContent", content(overlay));
		}
		model.child(copy, builder);
	}
	if let Some(texture) = outfit.torso_decal {
		let torso = descendants.iter().copied().find(|&referent| model.dom.get_by_ref(referent).is_some_and(|instance| instance.name == "Torso"));
		let decal = torso.and_then(|torso| model.dom.get_by_ref(torso)).and_then(|torso| torso.children().iter().copied().find(|&child| model.dom.get_by_ref(child).is_some_and(|child| child.class == "Decal")));
		if let Some(decal) = decal {
			model.set(decal, "TextureContent", content(texture));
		}
	}
	Ok(copy)
}

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];




/// `character-anchor`: is a body part's darker colour about how the rig is placed? Studio's R6 and R15
/// rigs as saved (unanchored, only moved), anchored, turned, and dressed in a uniform shirt either
/// way, under flat ambient light and the default lighting, from the front.
pub fn character_anchor() -> Result<Experiment> {
	let r6 = studio_rig(false)?;
	let r15 = studio_rig(true)?;
	let mut model = folder_root("CharacterAnchor");
	let saved = Outfit { unanchored: true, unturned: true, ..Outfit::default() };
	let shirt = Some((UNIFORM_202, WHITE));
	let rigs: [(&str, &Document, Outfit); 8] = [
		("r6-saved", &r6, saved.clone()),
		("r6-anchored", &r6, Outfit { unturned: true, ..Outfit::default() }),
		("r6-turned", &r6, Outfit { unanchored: true, ..Outfit::default() }),
		("r6-saved-shirt", &r6, Outfit { shirt, ..saved.clone() }),
		("r6-anchored-shirt", &r6, Outfit { shirt, unturned: true, ..Outfit::default() }),
		("r15-saved", &r15, saved.clone()),
		("r15-anchored", &r15, Outfit { unturned: true, ..Outfit::default() }),
		("r15-saved-shirt", &r15, Outfit { shirt, ..saved }),
	];
	for (index, (name, rig, outfit)) in rigs.iter().enumerate() {
		add_rig(&mut model, rig, name, Vec3::new(index as f32 * 5.0, 10.0, 0.0), outfit)?;
	}
	let camera = CameraSpec::LookAt { eye: [17.5, 9.0, -60.0], target: [17.5, 9.0, 0.0], fov: 30.0 };
	let runs = vec![
		Run { label: "ambient".into(), environment: flat(), camera: camera.clone() },
		Run { label: "default".into(), environment: Environment::default(), camera },
	];
	Ok(Experiment { name: "character-anchor".into(), model, probes: Vec::new(), runs })
}

/// `character-single`: Studio's R6 rig exactly as its file has it (the model the R6 compare loads),
/// under flat ambient light and the default lighting, from its front (+Z, the way it faces) and its
/// back.
pub fn character_single() -> Result<Experiment> {
	let rig = studio_rig(false)?;
	let root = *rig.roots.first().context("empty rig file")?;
	let model = ModelBuilder { dom: rig.dom, model: root };
	let mut runs = Vec::new();
	for (label, environment) in [("ambient", flat()), ("default", Environment::default())] {
		for (view, eye) in [("front", [0.0, 18.0, 36.5]), ("back", [0.0, 18.0, 8.5])] {
			runs.push(Run { label: format!("{label}-{view}"), environment: environment.clone(), camera: CameraSpec::LookAt { eye, target: [0.0, 18.0, 22.5], fov: 30.0 } });
		}
	}
	Ok(Experiment { name: "character-single".into(), model, probes: Vec::new(), runs })
}

