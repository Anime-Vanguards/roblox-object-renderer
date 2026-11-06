//! Bones: the skeleton a skinned MeshPart deforms with (notes/mesh.md "Skinning in Studio").
//!
//! Studio deforms a MeshPart saved with `HasSkinnedMesh` (skin-pose "flag-off") with the Bones under
//! it and under the parts joined to it by a Motor6D, Weld or WeldConstraint, in either direction,
//! that share its nearest Model ancestor (skin-joints, skin-partners2). A Bone of the same name
//! under a joined part wins over the MeshPart's own ("dup-own-and-motor", "split"); among joined
//! parts a weld's wins over a Motor6D's, and of one kind the later joint's (skin-partners2). Bones
//! elsewhere in the model, under parts two joints away or in another Model are not used
//! ("pose-sibling", "motor-chain", "animator-no-joint", "partner-in-other-model"). A Bone's world
//! CFrame is its parent's (a part's CFrame, or the parent Bone's world CFrame) times its own CFrame:
//! `Bone.Transform`, the animation offset, is not saved in files.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Affine3A;
use rbx_dom_weak::types::{Ref, Variant};

use super::load::Document;
use super::props;

/// The Bones a skinned MeshPart can use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skeleton {
	/// World CFrame of each Bone by name.
	pub bones: HashMap<String, Affine3A>,
}

/// A part joined to another: the joint's precedence (welds over Motor6Ds, then document order).
#[derive(Clone, Copy)]
struct Joined {
	part: Ref,
	weld: bool,
	order: usize,
}

/// Joints and Bones of a document, indexed for the MeshParts that look them up.
#[derive(Default)]
pub struct Skeletons {
	/// Parts joined to each part (either side of the joint).
	joined: Option<HashMap<Ref, Vec<Joined>>>,
	/// The Bones under each part (`None`: none).
	under: HashMap<Ref, Option<Arc<Skeleton>>>,
	/// Merged skeletons per list of sources.
	merged: HashMap<Vec<Ref>, Option<Arc<Skeleton>>>,
}

impl Skeletons {
	/// The skeleton of a MeshPart: the Bones under it, overridden by those under the parts joined
	/// to it. `None` when there are none.
	pub fn for_part(&mut self, document: &Document, part: Ref) -> Option<Arc<Skeleton>> {
		let joined = self.joined.get_or_insert_with(|| joints(document));
		// lowest precedence first: the MeshPart's own Bones, then its joined parts of the same Model,
		// Motor6Ds before welds, each kind in joint order
		let mut sources = vec![part];
		if let Some(others) = joined.get(&part) {
			let model = nearest_model(document, part);
			let mut others: Vec<Joined> = others.iter().copied().filter(|other| other.part != part && nearest_model(document, other.part) == model).collect();
			others.sort_by_key(|other| (other.weld, other.order));
			sources.extend(others.iter().map(|other| other.part));
		}
		let with_bones: Vec<Ref> = sources.into_iter().filter(|&source| self.bones_under(document, source).is_some()).collect();
		match with_bones.len() {
			0 => None,
			1 => self.bones_under(document, with_bones[0]),
			_ => {
				if let Some(merged) = self.merged.get(&with_bones) {
					return merged.clone();
				}
				let mut skeleton = Skeleton::default();
				for &source in &with_bones {
					if let Some(bones) = self.bones_under(document, source) {
						skeleton.bones.extend(bones.bones.iter().map(|(name, world)| (name.clone(), *world)));
					}
				}
				let merged = Some(Arc::new(skeleton));
				self.merged.insert(with_bones, merged.clone());
				merged
			}
		}
	}

	fn bones_under(&mut self, document: &Document, part: Ref) -> Option<Arc<Skeleton>> {
		self.under.entry(part).or_insert_with(|| collect(document, part).map(Arc::new)).clone()
	}
}

/// Which parts are joined to which: every JointInstance (Motor6D, Weld, ...) and WeldConstraint
/// with both parts set, in document order.
fn joints(document: &Document) -> HashMap<Ref, Vec<Joined>> {
	let mut joined: HashMap<Ref, Vec<Joined>> = HashMap::new();
	let mut order = 0;
	let mut stack = vec![document.dom.root_ref()];
	while let Some(referent) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else {
			continue;
		};
		if props::is_a(instance, "JointInstance") || instance.class == "WeldConstraint" {
			let part = |name: &str| match instance.properties.get(&rbx_dom_weak::ustr(name)) {
				Some(Variant::Ref(reference)) if reference.is_some() => Some(*reference),
				_ => None,
			};
			if let (Some(part0), Some(part1)) = (part("Part0"), part("Part1")) {
				let weld = matches!(instance.class.as_str(), "Weld" | "ManualWeld" | "WeldConstraint");
				joined.entry(part0).or_default().push(Joined { part: part1, weld, order });
				joined.entry(part1).or_default().push(Joined { part: part0, weld, order });
				order += 1;
			}
		}
		stack.extend(instance.children().iter().rev().copied());
	}
	joined
}

/// The nearest Model ancestor of an instance (Workspace counts, as in Studio), `None` without one.
fn nearest_model(document: &Document, referent: Ref) -> Option<Ref> {
	let mut current = document.dom.get_by_ref(referent)?.parent();
	while let Some(instance) = document.dom.get_by_ref(current) {
		if props::is_a(instance, "Model") {
			return Some(current);
		}
		current = instance.parent();
	}
	None
}

/// Every Bone under a part with its world CFrame; the first Bone of a name in depth-first order
/// wins.
fn collect(document: &Document, part: Ref) -> Option<Skeleton> {
	let instance = document.dom.get_by_ref(part)?;
	let frame = props::cframe(instance, "CFrame")?;
	let mut skeleton = Skeleton::default();
	// each entry carries the frame its attachments are relative to (None: not in the world)
	let mut stack: Vec<(Ref, Option<Affine3A>)> = instance.children().iter().rev().map(|&child| (child, Some(frame))).collect();
	while let Some((referent, parent)) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else {
			continue;
		};
		let own = if props::is_a(instance, "Attachment") {
			let world = parent.map(|parent| parent * props::cframe(instance, "CFrame").unwrap_or(Affine3A::IDENTITY));
			if let Some(world) = world
				&& instance.class == "Bone"
			{
				skeleton.bones.entry(instance.name.clone()).or_insert(world);
			}
			world
		} else if props::is_a(instance, "BasePart") {
			// a part nested in the part: its Bones are descendants too, relative to it
			props::cframe(instance, "CFrame")
		} else {
			None
		};
		for &child in instance.children().iter().rev() {
			stack.push((child, own));
		}
	}
	(!skeleton.bones.is_empty()).then_some(skeleton)
}

#[cfg(test)]
mod tests {
	use glam::{Affine3A, Vec3};
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{ContentId, Ref, Variant, Vector3};

	use crate::scene::build::{ModelBuilder, cframe};
	use crate::scene::{Geometry, Scene, extract};

	fn mesh_part(model: &mut ModelBuilder, parent: Ref, at: Vec3, skinned: bool) -> Ref {
		let part = model.child(parent, InstanceBuilder::new("MeshPart").with_name("Body").with_property("CFrame", cframe(Affine3A::from_translation(at))).with_property("Size", Vector3::new(2.0, 2.0, 2.0)));
		model.set(part, "MeshId", ContentId::from("rbxassetid://1"));
		if skinned {
			model.set(part, "HasSkinnedMesh", true);
		}
		part
	}

	fn rig(model: &mut ModelBuilder, parent: Ref, at: Vec3) -> Ref {
		model.child(parent, InstanceBuilder::new("Part").with_name("Rig").with_property("CFrame", cframe(Affine3A::from_translation(at))))
	}

	fn bone(model: &mut ModelBuilder, parent: Ref, name: &str, offset: Vec3) -> Ref {
		model.child(parent, InstanceBuilder::new("Bone").with_name(name).with_property("CFrame", cframe(Affine3A::from_translation(offset))))
	}

	fn joint(model: &mut ModelBuilder, class: &str, part0: Ref, part1: Ref) {
		model.child(part1, InstanceBuilder::new(class).with_property("Part0", Variant::Ref(part0)).with_property("Part1", Variant::Ref(part1)));
	}

	fn body_bones(scene: &Scene) -> Vec<(String, Vec3)> {
		let part = scene.parts.iter().find(|part| part.path.ends_with("Body") && matches!(part.geometry, Geometry::MeshPart { .. })).expect("Body");
		let mut bones: Vec<(String, Vec3)> = part.skeleton.as_ref().map(|skeleton| skeleton.bones.iter().map(|(name, world)| (name.clone(), Vec3::from(world.translation))).collect()).unwrap_or_default();
		bones.sort_by(|a, b| a.0.cmp(&b.0));
		bones
	}

	#[test]
	fn own_bones_chain_through_parent_bones() {
		let mut model = ModelBuilder::new("Test");
		let root = model.model;
		let part = mesh_part(&mut model, root, Vec3::new(10.0, 0.0, 0.0), true);
		let upper = bone(&mut model, part, "Upper", Vec3::new(0.0, 1.0, 0.0));
		bone(&mut model, upper, "Lower", Vec3::new(0.0, 0.5, 0.0));
		let bones = body_bones(&extract::extract(&model.into_document()));
		assert_eq!(bones, vec![("Lower".to_string(), Vec3::new(10.0, 1.5, 0.0)), ("Upper".to_string(), Vec3::new(10.0, 1.0, 0.0))]);
	}

	#[test]
	fn needs_has_skinned_mesh() {
		let mut model = ModelBuilder::new("Test");
		let root = model.model;
		let part = mesh_part(&mut model, root, Vec3::ZERO, false);
		bone(&mut model, part, "Upper", Vec3::Y);
		assert!(body_bones(&extract::extract(&model.into_document())).is_empty());
	}

	#[test]
	fn joined_parts_bones_win_and_merge() {
		let mut model = ModelBuilder::new("Test");
		let root = model.model;
		let part = mesh_part(&mut model, root, Vec3::ZERO, true);
		bone(&mut model, part, "Own", Vec3::Y);
		bone(&mut model, part, "Shared", Vec3::Y);
		let rig = rig(&mut model, root, Vec3::new(0.0, 0.0, 5.0));
		bone(&mut model, rig, "Shared", Vec3::X);
		joint(&mut model, "Motor6D", rig, part);
		let bones = body_bones(&extract::extract(&model.into_document()));
		assert_eq!(bones, vec![("Own".to_string(), Vec3::Y), ("Shared".to_string(), Vec3::new(1.0, 0.0, 5.0))]);
	}

	#[test]
	fn welds_win_over_motors_and_later_joints_over_earlier() {
		// skin-partners2: two-partners, weld-then-motor, two-motors, two-welds
		for (first, second, winner) in [("Motor6D", "Weld", 2.0), ("Weld", "Motor6D", 1.0), ("Motor6D", "Motor6D", 2.0), ("Weld", "Weld", 2.0)] {
			let mut model = ModelBuilder::new("Test");
			let root = model.model;
			let part = mesh_part(&mut model, root, Vec3::ZERO, true);
			let a = rig(&mut model, root, Vec3::new(1.0, 0.0, 0.0));
			let b = rig(&mut model, root, Vec3::new(2.0, 0.0, 0.0));
			bone(&mut model, a, "Shared", Vec3::ZERO);
			bone(&mut model, b, "Shared", Vec3::ZERO);
			joint(&mut model, first, a, part);
			joint(&mut model, second, b, part);
			let bones = body_bones(&extract::extract(&model.into_document()));
			assert_eq!(bones, vec![("Shared".to_string(), Vec3::new(winner, 0.0, 0.0))], "{first} then {second}");
		}
	}

	#[test]
	fn ignores_unjoined_parts_far_joints_and_other_models() {
		let mut model = ModelBuilder::new("Test");
		let root = model.model;
		let part = mesh_part(&mut model, root, Vec3::ZERO, true);
		// a sibling part with Bones but no joint
		let sibling = rig(&mut model, root, Vec3::ZERO);
		bone(&mut model, sibling, "Sibling", Vec3::ZERO);
		// two joints away
		let middle = rig(&mut model, root, Vec3::ZERO);
		let far = rig(&mut model, root, Vec3::ZERO);
		bone(&mut model, far, "Far", Vec3::ZERO);
		joint(&mut model, "Motor6D", middle, part);
		joint(&mut model, "Motor6D", far, middle);
		// joined, but in another Model
		let other = model.child(root, InstanceBuilder::new("Model").with_name("Other"));
		let elsewhere = rig(&mut model, other, Vec3::ZERO);
		bone(&mut model, elsewhere, "Elsewhere", Vec3::ZERO);
		joint(&mut model, "Weld", elsewhere, part);
		assert!(body_bones(&extract::extract(&model.into_document())).is_empty());
	}
}
