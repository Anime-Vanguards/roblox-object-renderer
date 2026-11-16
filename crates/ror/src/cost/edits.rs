//! What-if edits for the cost loop: a file of edits (remove, hide, set properties, unify an appearance,
//! merge) that `ror cost --edits` applies to the place in memory before costing it again, and
//! `--export` writes as a change list with Lune scripts that apply it to a saved copy of the place and
//! revert it key by key.
//!
//! ```json
//! {"schema": "ror-edits/1", "trials": [{"name": "fog off", "edits": [
//!   {"op": "hide", "targets": ["Workspace.Map.Fog[2]"]},
//!   {"op": "set", "targets": ["mesh:18494796095"], "properties": {"CastShadow": false, "RenderFidelity": "Performance"}},
//!   {"op": "set", "targets": ["under:Workspace.Map.Lamps"], "classes": ["PointLight"], "properties": {"Shadows": false}},
//!   {"op": "appearance", "targets": ["Workspace.Map.Rock[3]"], "from": "Workspace.Map.Rock[1]"},
//!   {"op": "merge", "name": "railings", "targets": ["Workspace.Map.Railing[1]", "Workspace.Map.Railing[2]"]},
//!   {"op": "remove", "targets": ["Workspace.Map.Backdrop"]}]}]}
//! ```
//!
//! Targets are instance ids ([`super::ids`]), `mesh:<asset>` (every MeshPart or FileMesh of that mesh the
//! scene draws: not copies kept in storage),
//! or `under:<id>` (everything below an instance); `classes` keeps those of the listed classes. A merge
//! is counted as one mesh of its members' appearance (the cost model batches them as one geometry); the
//! export lists it for a merge tool to build, it does not build meshes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rbx_dom_weak::types::{Color3, Color3uint8, Content, ContentId, Enum, Ref, Variant};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ids::{InstanceIds, split_id};
use crate::scene::load::Document;
use crate::scene::props;

pub const SCHEMA: &str = "ror-edits/1";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditsFile {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub schema: Option<String>,
	/// One trial's edits (named "edits").
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub edits: Vec<Edit>,
	/// Trials costed one by one against the same baseline.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub trials: Vec<Trial>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
	pub name: String,
	pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Edit {
	/// The instances and everything under them go.
	Remove {
		targets: Vec<String>,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		classes: Vec<String>,
	},
	/// Every part at or under the targets: Transparency 1 and no shadow, its decals hidden; it keeps
	/// its collision, joints, effects and lights.
	Hide {
		targets: Vec<String>,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		classes: Vec<String>,
	},
	/// Properties set on the targets (those of a class without the property are left).
	Set {
		targets: Vec<String>,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		classes: Vec<String>,
		properties: BTreeMap<String, Value>,
	},
	/// The targets take `from`'s appearance as Roblox batches by it: Material, MaterialVariant, a
	/// MeshPart's texture and its SurfaceAppearance (Color does not split batches and stays).
	Appearance {
		targets: Vec<String>,
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		classes: Vec<String>,
		from: String,
	},
	/// The parts at or under the targets counted as one mesh: copies of one appearance draw once.
	Merge { name: String, targets: Vec<String> },
}

impl EditsFile {
	pub fn read(text: &str) -> Result<Self> {
		let file: EditsFile = serde_json::from_str(text).context("an edits file: {\"schema\": \"ror-edits/1\", \"edits\": [...]} or {\"trials\": [{\"name\", \"edits\"}]}")?;
		if let Some(schema) = &file.schema
			&& schema != SCHEMA
		{
			bail!("edits schema {schema:?}: this ror reads {SCHEMA}");
		}
		if file.edits.is_empty() && file.trials.is_empty() {
			bail!("the edits file has no edits and no trials");
		}
		Ok(file)
	}

	/// The trials to cost: the file's, then its top-level edits as one named "edits".
	pub fn trials(&self) -> Vec<Trial> {
		let mut trials = self.trials.clone();
		if !self.edits.is_empty() {
			trials.push(Trial { name: "edits".into(), edits: self.edits.clone() });
		}
		trials
	}
}

/// One change as the export writes it and the Lune scripts apply and revert it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
	pub key: String,
	pub op: &'static str,
	pub id: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub uid: Option<String>,
	pub class: String,
	/// The id's steps from the file's root: each name and its occurrence among same-named siblings.
	pub steps: Vec<(String, u32)>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub property: Option<String>,
	/// `bool`, `number`, `string`, `enum:<Enum>`, `content`, `color3`, `vector3`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub value_type: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub before: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub after: Option<Value>,
	/// An appearance's source (its SurfaceAppearance cloned), with its steps.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub from: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub from_steps: Option<Vec<(String, u32)>>,
	/// A merge's members.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub members: Vec<String>,
}

/// What a trial's edits did to the document.
#[derive(Default)]
pub struct Applied {
	pub changes: Vec<Change>,
	/// Merges: name and member parts.
	pub merges: Vec<(String, Vec<Ref>)>,
	pub warnings: Vec<String>,
	/// Instances the edits touched, for the look check's crops and the findings' checks.
	pub touched: BTreeSet<String>,
}

fn steps_of(id: &str) -> Vec<(String, u32)> {
	split_id(id).map(|steps| steps.into_iter().map(|(name, occurrence)| (name, occurrence.unwrap_or(1))).collect()).unwrap_or_default()
}

fn mesh_of(instance: &rbx_dom_weak::Instance) -> Option<String> {
	match instance.class.as_str() {
		"MeshPart" | "FileMesh" | "SpecialMesh" => props::content(instance, &["MeshContent", "MeshId"]).map(|uri| super::meshes::canonical(&uri)),
		_ => None,
	}
}

/// The instances a target names, in file order.
fn resolve(document: &Document, ids: &InstanceIds, target: &str, classes: &[String]) -> Result<Vec<Ref>> {
	let dom = &document.dom;
	// what the scene draws: a mesh's copies kept in storage (ServerStorage, ReplicatedStorage) are not
	let drawn = |referent: Ref| {
		let mut current = referent;
		while let Some(instance) = dom.get_by_ref(current) {
			if document.roots.contains(&current) {
				return true;
			}
			current = instance.parent();
		}
		false
	};
	let mut found: Vec<Ref> = if let Some(mesh) = target.strip_prefix("mesh:") {
		let mesh = mesh.trim();
		let wanted = super::meshes::canonical(if mesh.chars().all(|c| c.is_ascii_digit()) { format!("rbxassetid://{mesh}") } else { mesh.to_string() }.as_str());
		let mut hits: Vec<(String, Ref)> = ids
			.all()
			.filter_map(|(referent, id)| {
				let instance = dom.get_by_ref(referent)?;
				let mesh = mesh_of(instance)?;
				// a FileMesh's part is what draws it
				let owner = if instance.class == "MeshPart" { referent } else { instance.parent() };
				(mesh == wanted && drawn(owner)).then(|| (ids.id(owner).unwrap_or(id).to_string(), owner))
			})
			.collect();
		hits.sort_by(|a, b| a.0.cmp(&b.0));
		hits.into_iter().map(|(_, referent)| referent).collect()
	} else if let Some(root) = target.strip_prefix("under:") {
		let root = ids.require(root)?;
		if dom.get_by_ref(root).is_none() {
			return Ok(Vec::new());
		}
		let mut below: Vec<(String, Ref)> = dom.descendants_of(root).filter(|instance| instance.referent() != root).map(|instance| (ids.id(instance.referent()).unwrap_or_default().to_string(), instance.referent())).collect();
		below.sort_by(|a, b| a.0.cmp(&b.0));
		below.into_iter().map(|(_, referent)| referent).collect()
	} else {
		vec![ids.require(target)?]
	};
	if !classes.is_empty() {
		found.retain(|referent| dom.get_by_ref(*referent).is_some_and(|instance| classes.iter().any(|class| props::class_is_a(instance.class.as_str(), class))));
	}
	found.dedup();
	Ok(found)
}

/// The property's canonical key on this instance (what the file holds: MeshContent over MeshId,
/// TextureContent over TextureID, Color3uint8 over Color) and its current value with the class default.
fn property_key(instance: &rbx_dom_weak::Instance, name: &str) -> String {
	let aliases: &[&str] = match name {
		"MeshId" | "MeshContent" => &["MeshContent", "MeshId"],
		"TextureID" | "TextureContent" => &["TextureContent", "TextureID"],
		"TextureId" => &["TextureContent", "TextureId"],
		"Color" | "Color3uint8" => &["Color3uint8", "Color"],
		"MaterialVariant" | "MaterialVariantSerialized" => &["MaterialVariantSerialized", "MaterialVariant"],
		_ => &[],
	};
	for alias in aliases {
		if instance.properties.contains_key(&rbx_dom_weak::ustr(alias)) {
			return alias.to_string();
		}
	}
	for alias in aliases {
		if props::get(instance, alias).is_some() {
			return alias.to_string();
		}
	}
	name.to_string()
}

/// The enum a property takes, from the reflection database.
fn enum_of(class: &str, property: &str) -> Option<&'static str> {
	let database = props::database();
	let mut current = database.classes.get(class);
	while let Some(descriptor) = current {
		if let Some(found) = descriptor.properties.get(property) {
			return match found.data_type {
				rbx_reflection::DataType::Enum(name) => Some(name),
				_ => None,
			};
		}
		current = descriptor.superclass.and_then(|superclass| database.classes.get(superclass));
	}
	None
}

/// A value as the change list writes it, and its type there.
fn to_json(value: &Variant, enum_name: Option<&str>) -> Option<(Value, String)> {
	Some(match value {
		Variant::Bool(value) => (json!(value), "bool".into()),
		Variant::Float32(value) => (json!(((*value as f64) * 1e6).round() / 1e6), "number".into()),
		Variant::Float64(value) => (json!(value), "number".into()),
		Variant::Int32(value) => (json!(value), "number".into()),
		Variant::Int64(value) => (json!(value), "number".into()),
		Variant::String(value) => (json!(value), "string".into()),
		Variant::ContentId(value) => (json!(value.as_str()), "content".into()),
		Variant::Content(value) => (json!(value.as_uri().unwrap_or_default()), "content".into()),
		Variant::Color3(value) => (json!([value.r, value.g, value.b]), "color3".into()),
		Variant::Color3uint8(value) => (json!([value.r as f32 / 255.0, value.g as f32 / 255.0, value.b as f32 / 255.0]), "color3".into()),
		Variant::Vector3(value) => (json!([value.x, value.y, value.z]), "vector3".into()),
		Variant::Enum(value) => {
			let name = enum_name?;
			let item = props::database().enums.get(name).and_then(|descriptor| descriptor.items.iter().find(|(_, item)| **item == value.to_u32()).map(|(item, _)| item.to_string()));
			(json!(item.unwrap_or_else(|| value.to_u32().to_string())), format!("enum:{name}"))
		}
		_ => return None,
	})
}

/// A JSON value as the property's type, from what the property holds now.
fn from_json(current: &Variant, enum_name: Option<&str>, value: &Value) -> Result<Variant> {
	let number = || value.as_f64().or_else(|| value.as_str().and_then(|text| text.parse().ok())).context("a number");
	let color = || -> Result<[f32; 3]> {
		if let Some(text) = value.as_str() {
			let hex = text.trim_start_matches('#');
			let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2).unwrap_or("00"), 16).map(|value| value as f32 / 255.0);
			return Ok([byte(0)?, byte(2)?, byte(4)?]);
		}
		let values: Vec<f32> = value.as_array().context("a colour: [r, g, b] in 0-1 or \"#rrggbb\"")?.iter().map(|value| value.as_f64().unwrap_or(0.0) as f32).collect();
		if values.len() != 3 {
			bail!("a colour has three channels");
		}
		Ok([values[0], values[1], values[2]])
	};
	Ok(match current {
		Variant::Bool(_) => Variant::Bool(value.as_bool().or_else(|| value.as_str().map(|text| text == "true")).context("true or false")?),
		Variant::Float32(_) => Variant::Float32(number()? as f32),
		Variant::Float64(_) => Variant::Float64(number()?),
		Variant::Int32(_) => Variant::Int32(number()? as i32),
		Variant::Int64(_) => Variant::Int64(number()? as i64),
		Variant::String(_) => Variant::String(value.as_str().context("a string")?.to_string()),
		Variant::ContentId(_) => Variant::ContentId(ContentId::from(content_uri(value)?)),
		Variant::Content(_) => Variant::Content(Content::from_uri(content_uri(value)?)),
		Variant::Color3(_) => {
			let [r, g, b] = color()?;
			Variant::Color3(Color3::new(r, g, b))
		}
		Variant::Color3uint8(_) => {
			let [r, g, b] = color()?;
			Variant::Color3uint8(Color3uint8::new((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8))
		}
		Variant::Vector3(_) => {
			let values: Vec<f32> = value.as_array().context("[x, y, z]")?.iter().map(|value| value.as_f64().unwrap_or(0.0) as f32).collect();
			Variant::Vector3(rbx_dom_weak::types::Vector3::new(values.first().copied().unwrap_or(0.0), values.get(1).copied().unwrap_or(0.0), values.get(2).copied().unwrap_or(0.0)))
		}
		Variant::Enum(_) => {
			let item = match value {
				Value::Number(number) => number.as_u64().context("an enum item's number")? as u32,
				Value::String(text) => {
					let name = enum_name.context("an enum")?;
					*props::database().enums.get(name).and_then(|descriptor| descriptor.items.get(text.as_str())).with_context(|| format!("{text:?} is not an item of Enum.{name}"))?
				}
				_ => bail!("an enum item's name or number"),
			};
			Variant::Enum(Enum::from_u32(item))
		}
		other => bail!("properties of type {:?} cannot be set by an edit", other.ty()),
	})
}

fn content_uri(value: &Value) -> Result<String> {
	let text = value.as_str().map(str::to_string).or_else(|| value.as_u64().map(|id| id.to_string())).context("a content id")?;
	Ok(if text.chars().all(|c| c.is_ascii_digit()) { format!("rbxassetid://{text}") } else { text })
}

struct Applier<'a> {
	ids: &'a InstanceIds,
	applied: Applied,
}

impl Applier<'_> {
	fn change(&self, document: &Document, op: &'static str, referent: Ref, key: String) -> Change {
		let id = self.ids.id(referent).unwrap_or_default().to_string();
		Change {
			key,
			op,
			steps: steps_of(&id),
			uid: self.ids.uid(referent).map(str::to_string),
			class: document.dom.get_by_ref(referent).map(|instance| instance.class.to_string()).unwrap_or_default(),
			id,
			property: None,
			value_type: None,
			before: None,
			after: None,
			from: None,
			from_steps: None,
			members: Vec::new(),
		}
	}

	/// Sets one property, recording what it was. A part's mesh and texture properties it does not
	/// have go to its SpecialMesh (a Part's MeshId, TextureId).
	fn set(&mut self, document: &mut Document, referent: Ref, name: &str, value: &Value) -> Result<bool> {
		let Some(instance) = document.dom.get_by_ref(referent) else { return Ok(false) };
		let mut referent = referent;
		let mut instance = instance;
		if props::get(instance, &property_key(instance, name)).is_none()
			&& matches!(name, "MeshId" | "MeshContent" | "TextureID" | "TextureId" | "TextureContent")
			&& let Some(&child) = instance.children().iter().find(|child| document.dom.get_by_ref(**child).is_some_and(|child| matches!(child.class.as_str(), "SpecialMesh" | "FileMesh")))
		{
			referent = child;
			instance = document.dom.get_by_ref(child).expect("the mesh");
		}
		let name = if matches!(instance.class.as_str(), "SpecialMesh" | "FileMesh") && name == "TextureID" { "TextureId" } else { name };
		let key = property_key(instance, name);
		let Some(current) = props::get(instance, &key).cloned() else { return Ok(false) };
		let enum_name = enum_of(instance.class.as_str(), &key).or_else(|| enum_of(instance.class.as_str(), name));
		let next = from_json(&current, enum_name, value).with_context(|| format!("{name} on {}", self.ids.id(referent).unwrap_or_default()))?;
		if next == current {
			return Ok(true);
		}
		let id = self.ids.id(referent).unwrap_or_default().to_string();
		let mut change = self.change(document, "set", referent, format!("set:{id}:{key}"));
		let before = to_json(&current, enum_name);
		let after = to_json(&next, enum_name);
		change.property = Some(key.clone());
		change.value_type = after.as_ref().or(before.as_ref()).map(|(_, kind)| kind.clone());
		change.before = before.map(|(value, _)| value);
		change.after = after.map(|(value, _)| value);
		document.dom.get_by_ref_mut(referent).expect("the instance").properties.insert(rbx_dom_weak::ustr(&key), next);
		self.applied.touched.insert(id);
		// set twice in one trial: one change, from what the file had to the last value
		match self.applied.changes.iter_mut().find(|earlier| earlier.key == change.key) {
			Some(earlier) => earlier.after = change.after,
			None => self.applied.changes.push(change),
		}
		Ok(true)
	}
}

/// Applies a trial's edits to `document` (whose ids `ids` took before any edit).
pub fn apply(document: &mut Document, ids: &InstanceIds, edits: &[Edit]) -> Result<Applied> {
	let mut context = Applier { ids, applied: Applied::default() };
	for edit in edits {
		match edit {
			Edit::Remove { targets, classes } => {
				for target in targets {
					for referent in resolve(document, ids, target, classes)? {
						if document.dom.get_by_ref(referent).is_none() {
							continue;
						}
						let id = ids.id(referent).unwrap_or_default().to_string();
						let change = context.change(document, "remove", referent, format!("remove:{id}"));
						context.applied.changes.push(change);
						for below in document.dom.descendants_of(referent).map(|instance| instance.referent()).collect::<Vec<_>>() {
							if let Some(id) = ids.id(below) {
								context.applied.touched.insert(id.to_string());
							}
						}
						document.dom.destroy(referent);
						document.roots.retain(|root| *root != referent);
					}
				}
			}
			Edit::Hide { targets, classes } => {
				for target in targets {
					for referent in resolve(document, ids, target, classes)? {
						if document.dom.get_by_ref(referent).is_none() {
							continue;
						}
						let parts: Vec<Ref> = document.dom.descendants_of(referent).filter(|instance| props::class_is_a(instance.class.as_str(), "BasePart")).map(|instance| instance.referent()).collect();
						let decals: Vec<Ref> = document.dom.descendants_of(referent).filter(|instance| matches!(instance.class.as_str(), "Decal" | "Texture")).map(|instance| instance.referent()).collect();
						for part in parts {
							context.set(document, part, "Transparency", &json!(1.0))?;
							context.set(document, part, "CastShadow", &json!(false))?;
						}
						for decal in decals {
							context.set(document, decal, "Transparency", &json!(1.0))?;
						}
					}
				}
			}
			Edit::Set { targets, classes, properties } => {
				let mut skipped = 0;
				for target in targets {
					for referent in resolve(document, ids, target, classes)? {
						for (name, value) in properties {
							if !context.set(document, referent, name, value)? {
								skipped += 1;
							}
						}
					}
				}
				if skipped > 0 {
					context.applied.warnings.push(format!("set {:?}: {skipped} target properties do not exist on their class and were left", properties.keys().collect::<Vec<_>>()));
				}
			}
			Edit::Appearance { targets, classes, from } => {
				let source = ids.require(from)?;
				let source_instance = document.dom.get_by_ref(source).context("the appearance's source")?;
				let wanted: Vec<(&str, Option<Value>)> = ["Material", "MaterialVariant", "TextureID"]
					.into_iter()
					.map(|name| {
						let key = property_key(source_instance, name);
						let value = props::get(source_instance, &key).and_then(|value| to_json(value, enum_of(source_instance.class.as_str(), &key)).map(|(value, _)| value));
						(name, value)
					})
					.collect();
				let source_appearance = source_instance.children().iter().copied().find(|child| document.dom.get_by_ref(*child).is_some_and(|instance| instance.class == "SurfaceAppearance"));
				let source_mesh = source_instance.children().iter().copied().find(|child| document.dom.get_by_ref(*child).is_some_and(|instance| matches!(instance.class.as_str(), "SpecialMesh" | "FileMesh")));
				let source_texture = source_mesh.and_then(|mesh| document.dom.get_by_ref(mesh)).and_then(|mesh| {
					let key = property_key(mesh, "TextureId");
					props::get(mesh, &key).and_then(|value| to_json(value, None)).map(|(value, _)| value)
				});
				for target in targets {
					for referent in resolve(document, ids, target, classes)? {
						// a part that is still there (an earlier edit may have removed it)
						if referent == source || !document.dom.get_by_ref(referent).is_some_and(|instance| props::class_is_a(instance.class.as_str(), "BasePart")) {
							continue;
						}
						// a SpecialMesh's texture splits batches as a MeshPart's does
						if let Some(texture) = &source_texture {
							context.set(document, referent, "TextureId", texture)?;
						}
						for (name, value) in &wanted {
							if let Some(value) = value
								&& (*name != "TextureID" || document.dom.get_by_ref(referent).is_some_and(|instance| instance.class == "MeshPart"))
							{
								context.set(document, referent, name, value)?;
							}
						}
						// the SurfaceAppearance: the target's own go, the source's is cloned under it
						let id = ids.id(referent).unwrap_or_default().to_string();
						let own: Vec<Ref> = document.dom.get_by_ref(referent).map(|instance| instance.children().iter().copied().filter(|child| document.dom.get_by_ref(*child).is_some_and(|instance| instance.class == "SurfaceAppearance")).collect()).unwrap_or_default();
						if own.is_empty() && source_appearance.is_none() {
							continue;
						}
						let mut change = context.change(document, "surfaceAppearance", referent, format!("appearance:{id}"));
						change.from = Some(from.clone());
						change.from_steps = Some(steps_of(from));
						for child in own {
							document.dom.destroy(child);
						}
						if let Some(appearance) = source_appearance {
							let copy = document.dom.clone_within(appearance);
							document.dom.transfer_within(copy, referent);
						}
						context.applied.touched.insert(id);
						context.applied.changes.push(change);
					}
				}
			}
			Edit::Merge { name, targets } => {
				let mut members = Vec::new();
				for target in targets {
					for referent in resolve(document, ids, target, &[])? {
						if document.dom.get_by_ref(referent).is_none() {
							continue;
						}
						members.extend(document.dom.descendants_of(referent).filter(|instance| props::class_is_a(instance.class.as_str(), "BasePart")).map(|instance| instance.referent()));
					}
				}
				members.dedup();
				if members.len() < 2 {
					context.applied.warnings.push(format!("merge {name:?}: fewer than two parts"));
					continue;
				}
				let mut change = Change {
					key: format!("merge:{name}"),
					op: "merge",
					id: name.clone(),
					uid: None,
					class: "MeshPart".into(),
					steps: Vec::new(),
					property: None,
					value_type: None,
					before: None,
					after: None,
					from: None,
					from_steps: None,
					members: members.iter().filter_map(|referent| ids.id(*referent)).map(str::to_string).collect(),
				};
				change.members.sort();
				for member in &change.members {
					context.applied.touched.insert(member.clone());
				}
				context.applied.changes.push(change);
				context.applied.merges.push((name.clone(), members));
			}
		}
	}
	Ok(context.applied)
}

const APPLY: &str = include_str!("lune/apply.luau");
const REVERT: &str = include_str!("lune/revert.luau");

/// Writes the changes as a kit: `changes.json`, `changes.tsv` (one key a line), `apply.luau` and
/// `revert.luau` (Lune), and a README saying how to use them.
pub fn export(dir: &std::path::Path, place: &str, trial: &str, changes: &[Change], savings: Option<&Value>) -> Result<()> {
	std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
	let list = json!({ "schema": "ror-cost-changes/1", "place": place, "trial": trial, "predicted": savings, "changes": changes });
	crate::output::write_json_atomic(&dir.join("changes.json"), &list, true)?;
	let mut tsv = String::from("key\top\tid\tproperty\tbefore\tafter\n");
	for change in changes {
		let value = |value: &Option<Value>| value.as_ref().map(|value| value.to_string()).unwrap_or_default();
		tsv.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", change.key, change.op, change.id, change.property.clone().unwrap_or_default(), value(&change.before), value(&change.after)));
	}
	crate::output::write_atomic(&dir.join("changes.tsv"), tsv.as_bytes())?;
	crate::output::write_atomic(&dir.join("apply.luau"), APPLY.as_bytes())?;
	crate::output::write_atomic(&dir.join("revert.luau"), REVERT.as_bytes())?;
	let readme = format!(
		"# {trial}: changes for {place}\n\nWritten by `ror cost --edits ... --export`. Each change has a key; apply them to a saved copy of the place, look, and revert key by key. Use Lune 0.10.5 or later (0.10.2 cannot read current places); a place Lune writes loses a few StyleRule property tables, so open it in Studio to check before publishing.\n\n```\nlune run apply.luau <in.rbxl> <out.rbxl> changes.json [--skip keys.txt] [--only remove,set,surfaceAppearance]\nlune run revert.luau <out.rbxl> <back.rbxl> changes.json [key ...]   # every key when none is named\n```\n\n- remove: the instance moves to ServerStorage.RorCostRemoved (an ObjectValue RorCostParent remembers where it was).\n- set: the property takes `after`; `before` is what revert puts back (a hide is its parts' Transparency and CastShadow sets). Changed instances carry the attribute RorCostKeys.\n- surfaceAppearance: the target's SurfaceAppearance moves to RorCostRemoved and the source's is cloned under it.\n- merge: listed for a merge tool (the cost model counted its members as one mesh); the scripts leave it.\n\nIds are paths from the file's root, a name repeated among siblings numbered by its place among them from 1; `steps` spells them out. They resolve against the place as ror read it: apply to that version.\n"
	);
	crate::output::write_atomic(&dir.join("README.md"), readme.as_bytes())?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};

	fn map() -> Document {
		let mut model = ModelBuilder::new("Map");
		for name in ["Rock", "Rock", "Fog"] {
			model.part(&PartSpec { name: name.into(), material: 512, ..Default::default() });
		}
		model.into_document()
	}

	#[test]
	fn edits_apply_in_memory_and_record_what_they_change() {
		let mut document = map();
		let ids = InstanceIds::new(&document);
		let file = EditsFile::read(
			r#"{"schema": "ror-edits/1", "edits": [
				{"op": "set", "targets": ["Map.Rock[1]"], "properties": {"CastShadow": false, "Material": "SmoothPlastic", "Transparency": 0.5}},
				{"op": "hide", "targets": ["Map.Fog"]},
				{"op": "remove", "targets": ["Map.Rock[2]"]},
				{"op": "merge", "name": "rocks", "targets": ["Map.Rock[1]", "Map.Fog"]}
			]}"#,
		)
		.unwrap();
		let trials = file.trials();
		assert_eq!(trials.len(), 1);
		let applied = apply(&mut document, &ids, &trials[0].edits).unwrap();
		let rock = document.dom.get_by_ref(ids.resolve("Map.Rock[1]").unwrap()).unwrap();
		assert!(!props::bool(rock, "CastShadow", true));
		assert_eq!(props::enum_value(rock, "Material", 0), 272);
		assert!((props::f32(rock, "Transparency", 0.0) - 0.5).abs() < 1e-6);
		let fog = document.dom.get_by_ref(ids.resolve("Map.Fog").unwrap()).unwrap();
		assert_eq!(props::f32(fog, "Transparency", 0.0), 1.0);
		assert!(document.dom.get_by_ref(ids.resolve("Map.Rock[2]").unwrap()).is_none(), "removed");
		let keys: Vec<&str> = applied.changes.iter().map(|change| change.key.as_str()).collect();
		assert_eq!(keys, ["set:Map.Rock[1]:CastShadow", "set:Map.Rock[1]:Material", "set:Map.Rock[1]:Transparency", "set:Map.Fog:Transparency", "set:Map.Fog:CastShadow", "remove:Map.Rock[2]", "merge:rocks"]);
		let material = &applied.changes[1];
		assert_eq!((material.before.clone(), material.after.clone(), material.value_type.clone()), (Some(json!("Wood")), Some(json!("SmoothPlastic")), Some("enum:Material".to_string())));
		assert_eq!(applied.changes[5].steps, [("Map".to_string(), 1), ("Rock".to_string(), 2)]);
		assert_eq!(applied.merges.len(), 1);
		assert_eq!(applied.merges[0].1.len(), 2);
	}

	#[test]
	fn appearance_over_a_subtree_takes_parts_only_and_never_panics() {
		let mut model = ModelBuilder::new("Map");
		let rocks = model.child(model.model, rbx_dom_weak::InstanceBuilder::new("Model").with_name("Rocks"));
		let first = model.part(&PartSpec { name: "Rock".into(), class: "MeshPart", material: 800, ..Default::default() });
		let second = model.part(&PartSpec { name: "Rock".into(), class: "MeshPart", material: 512, ..Default::default() });
		for rock in [first, second] {
			model.dom.transfer_within(rock, rocks);
			model.child(rock, rbx_dom_weak::InstanceBuilder::new("SurfaceAppearance").with_name("SurfaceAppearance"));
		}
		let mut document = model.into_document();
		let ids = InstanceIds::new(&document);
		// `under:` lists Rock[2]'s own SurfaceAppearance after Rock[2]: replacing it destroys that one
		let applied = apply(&mut document, &ids, &[Edit::Appearance { targets: vec!["under:Map.Rocks".into()], classes: vec![], from: "Map.Rocks.Rock[1]".into() }]).unwrap();
		let second = document.dom.get_by_ref(ids.resolve("Map.Rocks.Rock[2]").unwrap()).unwrap();
		assert_eq!(props::enum_value(second, "Material", 0), 800);
		assert_eq!(second.children().iter().filter(|child| document.dom.get_by_ref(**child).is_some_and(|child| child.class == "SurfaceAppearance")).count(), 1);
		assert!(applied.changes.iter().all(|change| change.class == "MeshPart"), "only parts take an appearance");
	}

	#[test]
	fn a_property_set_twice_is_one_change_from_the_first_value() {
		let mut document = map();
		let ids = InstanceIds::new(&document);
		let edits = [Edit::Hide { targets: vec!["Map.Fog".into()], classes: vec![] }, Edit::Set { targets: vec!["Map.Fog".into()], classes: vec![], properties: [("Transparency".to_string(), json!(0.8))].into_iter().collect() }];
		let applied = apply(&mut document, &ids, &edits).unwrap();
		let transparency: Vec<&Change> = applied.changes.iter().filter(|change| change.property.as_deref() == Some("Transparency")).collect();
		assert_eq!(transparency.len(), 1);
		assert_eq!((transparency[0].before.clone(), transparency[0].after.clone()), (Some(json!(0.0)), Some(json!(0.8))));
	}

	#[test]
	fn a_parts_mesh_goes_to_its_special_mesh() {
		let mut model = ModelBuilder::new("Map");
		let part = model.part(&PartSpec { name: "Barrel".into(), ..Default::default() });
		let mesh = model.child(part, rbx_dom_weak::InstanceBuilder::new("SpecialMesh").with_property("MeshType", Enum::from_u32(5)).with_property("MeshId", ContentId::from("rbxassetid://1")));
		let mut document = model.into_document();
		let ids = InstanceIds::new(&document);
		let applied = apply(&mut document, &ids, &[Edit::Set { targets: vec!["Map.Barrel".into()], classes: vec![], properties: [("MeshId".to_string(), json!("rbxassetid://2"))].into_iter().collect() }]).unwrap();
		assert_eq!(props::content(document.dom.get_by_ref(mesh).unwrap(), &["MeshContent", "MeshId"]).as_deref(), Some("rbxassetid://2"));
		assert_eq!(applied.changes.len(), 1);
		assert_eq!(applied.changes[0].class, "SpecialMesh");
	}

	#[test]
	fn a_missing_target_says_where_its_path_stops() {
		let mut document = map();
		let ids = InstanceIds::new(&document);
		let error = apply(&mut document, &ids, &[Edit::Remove { targets: vec!["Map.Rok".into()], classes: vec![] }]).err().unwrap().to_string();
		assert!(error.contains("nearest ancestor that exists is \"Map\""), "{error}");
	}
}
