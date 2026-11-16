//! Stable instance ids for the cost loop (`ror cost`'s analysis, findings and edits): an instance's
//! dotted path from the file's root as ror writes paths (`Workspace.Map["Rock.001"]`), each step that
//! shares its name with an earlier sibling numbered by its place among them, from 1 (`Model[3]`). Two
//! reads of one file give every instance the same id, so runs diff cleanly and an edit names what it
//! changes; Roblox's own UniqueId, where the file keeps it, goes beside it as `uid`.
//!
//! Ids are taken before edits apply ([`InstanceIds::new`] on the document as read): removing an
//! instance does not renumber its siblings, so the same id names the same instance before and after.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use rbx_dom_weak::types::Ref;

use crate::scene::load::{Document, path_segment};

#[derive(Default)]
pub struct InstanceIds {
	ids: HashMap<Ref, String>,
	by_id: HashMap<String, Ref>,
	uids: HashMap<Ref, String>,
	/// Each instance's parent's id ("" for the file's topmost), its class and name.
	info: HashMap<Ref, (Ref, String, String)>,
}

impl InstanceIds {
	/// Every instance of `document`, by its children's order in the file.
	pub fn new(document: &Document) -> Self {
		let dom = &document.dom;
		let mut ids = Self::default();
		let root = dom.root_ref();
		let mut stack: Vec<(Ref, String)> = vec![(root, String::new())];
		while let Some((parent, prefix)) = stack.pop() {
			let Some(instance) = dom.get_by_ref(parent) else { continue };
			let mut seen: HashMap<&str, u32> = HashMap::new();
			let mut children = Vec::with_capacity(instance.children().len());
			for &child in instance.children() {
				let Some(node) = dom.get_by_ref(child) else { continue };
				let count = seen.entry(node.name.as_str()).or_insert(0);
				*count += 1;
				children.push((child, *count));
			}
			for &(child, occurrence) in children.iter().rev() {
				let node = dom.get_by_ref(child).expect("a child");
				let repeated = seen.get(node.name.as_str()).is_some_and(|count| *count > 1);
				let mut id = prefix.clone();
				let segment = path_segment(&node.name);
				if !id.is_empty() && !segment.starts_with('[') {
					id.push('.');
				}
				id.push_str(&segment);
				if repeated {
					id.push_str(&format!("[{occurrence}]"));
				}
				if let Some(uid) = dom.get_unique_id(child) {
					ids.uids.insert(child, uid.to_string());
				}
				ids.info.insert(child, (if parent == root { Ref::none() } else { parent }, node.class.to_string(), node.name.clone()));
				ids.by_id.insert(id.clone(), child);
				ids.ids.insert(child, id.clone());
				stack.push((child, id));
			}
		}
		ids
	}

	pub fn id(&self, referent: Ref) -> Option<&str> {
		self.ids.get(&referent).map(String::as_str)
	}

	pub fn uid(&self, referent: Ref) -> Option<&str> {
		self.uids.get(&referent).map(String::as_str)
	}

	/// The parent (none at the file's top), class and name of an instance as read.
	pub fn info(&self, referent: Ref) -> Option<&(Ref, String, String)> {
		self.info.get(&referent)
	}

	/// The instance an id names, as read.
	pub fn resolve(&self, id: &str) -> Option<Ref> {
		self.by_id.get(id.trim()).copied()
	}

	/// The instance an id names, or why none does: the nearest step that exists, to tell a typo from a
	/// removed instance.
	pub fn require(&self, id: &str) -> Result<Ref> {
		if let Some(referent) = self.resolve(id) {
			return Ok(referent);
		}
		let steps = split_id(id).with_context(|| format!("instance id {id:?}"))?;
		let mut known = String::new();
		let mut path = String::new();
		for (name, occurrence) in &steps {
			let segment = path_segment(name);
			if !path.is_empty() && !segment.starts_with('[') {
				path.push('.');
			}
			path.push_str(&segment);
			if let Some(occurrence) = occurrence {
				path.push_str(&format!("[{occurrence}]"));
			}
			if self.by_id.contains_key(&path) {
				known = path.clone();
			} else {
				break;
			}
		}
		if known.is_empty() {
			bail!("no instance {id:?} in the file")
		}
		bail!("no instance {id:?} in the file (its nearest ancestor that exists is {known:?})")
	}

	/// Every instance, in no particular order.
	pub fn all(&self) -> impl Iterator<Item = (Ref, &str)> {
		self.ids.iter().map(|(referent, id)| (*referent, id.as_str()))
	}
}

/// An id's steps: each name and its occurrence among same-named siblings (`None`: the only one).
pub fn split_id(id: &str) -> Result<Vec<(String, Option<u32>)>> {
	let mut steps: Vec<(String, Option<u32>)> = Vec::new();
	let mut rest = id.trim();
	while !rest.is_empty() {
		if let Some(after) = rest.strip_prefix('[') {
			if let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') {
				let end = after[1..].find(quote).context("unterminated quoted name")? + 1;
				steps.push((after[1..end].to_string(), None));
				rest = after[end + 1..].strip_prefix(']').context("expected ] after a quoted name")?;
			} else {
				let end = after.find(']').context("expected ] after an occurrence")?;
				let occurrence: u32 = after[..end].trim().parse().context("an occurrence is a number: Name[2]")?;
				let last = steps.last_mut().context("an occurrence follows a name")?;
				last.1 = Some(occurrence);
				rest = &after[end + 1..];
			}
		} else {
			let end = rest.find(['.', '[']).unwrap_or(rest.len());
			if end == 0 {
				bail!("an empty step");
			}
			steps.push((rest[..end].to_string(), None));
			rest = &rest[end..];
		}
		rest = rest.strip_prefix('.').unwrap_or(rest);
	}
	Ok(steps)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::build::{ModelBuilder, PartSpec};

	#[test]
	fn repeated_names_are_numbered_and_ids_survive_a_second_read() {
		let mut model = ModelBuilder::new("Map");
		for name in ["Rock", "Tree", "Rock", "Rock.001", "Rock"] {
			model.part(&PartSpec { name: name.into(), ..Default::default() });
		}
		let document = model.into_document();
		let ids = InstanceIds::new(&document);
		let mut names: Vec<&str> = ids.all().map(|(_, id)| id).filter(|id| id.contains("Rock") || id.contains("Tree")).collect();
		names.sort();
		assert_eq!(names, ["Map.Rock[1]", "Map.Rock[2]", "Map.Rock[3]", "Map.Tree", "Map[\"Rock.001\"]"]);
		let again = InstanceIds::new(&document);
		for (referent, id) in ids.all() {
			assert_eq!(again.id(referent), Some(id));
			assert_eq!(ids.resolve(id), Some(referent));
		}
		assert_eq!(split_id("Map[\"Rock.001\"][2].Leaf").unwrap(), [("Map".to_string(), None), ("Rock.001".to_string(), Some(2)), ("Leaf".to_string(), None)]);
		let error = ids.require("Map.Rock[4]").unwrap_err().to_string();
		assert!(error.contains("nearest ancestor that exists is \"Map\""), "{error}");
	}
}
