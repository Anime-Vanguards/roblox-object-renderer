//! Skinning data shared by FileMesh 4.00, 5.00 and the `SKINNING` chunk of 6.00/7.00.
//!
//! Per vertex the file stores four subset-local bone slots and four `u8` weights ("envelopes").
//! Subsets partition the vertices; a vertex's slot `s` names the bone `subset.boneIndices[s]` of the
//! subset whose vertex range contains it.

use crate::reader::{le_f32, le_u16, le_u32, le_vec3};
use crate::{Bone, MeshError, Skinning, Subset};

pub(crate) const ENVELOPE_SIZE: usize = 8;
pub(crate) const BONE_SIZE: usize = 60;
pub(crate) const SUBSET_SIZE: usize = 72;
const MAX_SUBSET_BONES: usize = 26;
const NO_BONE: u16 = 0xFFFF;

/// Raw skinning sections of a file.
pub(crate) struct RawSkinning<'a> {
	/// `vertex_count * 8` bytes, or empty when the file stores none.
	pub(crate) envelopes: &'a [u8],
	/// `bone_count * 60` bytes.
	pub(crate) bones: &'a [u8],
	pub(crate) names: &'a [u8],
	/// `subset_count * 72` bytes.
	pub(crate) subsets: &'a [u8],
}

/// Builds [`Skinning`]; `None` when the file has no bones.
pub(crate) fn build(raw: RawSkinning<'_>, vertex_count: usize) -> Result<Option<Skinning>, MeshError> {
	let bone_count = raw.bones.len() / BONE_SIZE;
	if bone_count == 0 {
		return Ok(None);
	}

	let mut bones = Vec::with_capacity(bone_count);
	for record in raw.bones.chunks_exact(BONE_SIZE) {
		let name_offset = le_u32(record, 0) as usize;
		let parent = le_u16(record, 4);
		let lod_parent = le_u16(record, 6);
		let mut rotation = [[0.0; 3]; 3];
		for (row, values) in rotation.iter_mut().enumerate() {
			*values = le_vec3(record, 12 + row * 12);
		}
		bones.push(Bone {
			name: bone_name(raw.names, name_offset)?,
			parent: (parent != NO_BONE).then_some(parent),
			lod_parent: (lod_parent != NO_BONE).then_some(lod_parent),
			culling: le_f32(record, 8),
			rotation,
			position: le_vec3(record, 48),
		});
	}

	let mut subsets = Vec::with_capacity(raw.subsets.len() / SUBSET_SIZE);
	for record in raw.subsets.chunks_exact(SUBSET_SIZE) {
		let faces_begin = le_u32(record, 0);
		let faces_len = le_u32(record, 4);
		let vertices_begin = le_u32(record, 8);
		let vertices_len = le_u32(record, 12);
		let bone_count_in_subset = (le_u32(record, 16) as usize).min(MAX_SUBSET_BONES);
		let bone_indices = (0..bone_count_in_subset).map(|slot| le_u16(record, 20 + slot * 2)).collect();
		subsets.push(Subset {
			faces: faces_begin..faces_begin.saturating_add(faces_len),
			vertices: vertices_begin..vertices_begin.saturating_add(vertices_len),
			bone_indices,
		});
	}

	let mut joints = vec![[0u16; 4]; vertex_count];
	let mut weights = vec![[0f32; 4]; vertex_count];
	if raw.envelopes.len() >= vertex_count * ENVELOPE_SIZE && vertex_count > 0 {
		resolve(raw.envelopes, &subsets, bone_count, &mut joints, &mut weights)?;
	}

	Ok(Some(Skinning { bones, joints, weights, subsets }))
}

/// Resolves subset-local slots to global bone indices and normalises the weights.
fn resolve(
	envelopes: &[u8],
	subsets: &[Subset],
	bone_count: usize,
	joints: &mut [[u16; 4]],
	weights: &mut [[f32; 4]],
) -> Result<(), MeshError> {
	let vertex_count = joints.len();
	// Vertices outside every subset index the global bone list directly.
	let mut covered = vec![false; vertex_count];
	for subset in subsets {
		let start = (subset.vertices.start as usize).min(vertex_count);
		let end = (subset.vertices.end as usize).min(vertex_count);
		for (vertex, covered) in covered.iter_mut().enumerate().take(end).skip(start) {
			*covered = true;
			resolve_vertex(envelopes, vertex, Some(&subset.bone_indices), bone_count, joints, weights)?;
		}
	}
	for (vertex, _) in covered.iter().enumerate().filter(|(_, covered)| !**covered) {
		resolve_vertex(envelopes, vertex, None, bone_count, joints, weights)?;
	}
	Ok(())
}

#[inline]
fn resolve_vertex(
	envelopes: &[u8],
	vertex: usize,
	table: Option<&[u16]>,
	bone_count: usize,
	joints: &mut [[u16; 4]],
	weights: &mut [[f32; 4]],
) -> Result<(), MeshError> {
	let record = &envelopes[vertex * ENVELOPE_SIZE..vertex * ENVELOPE_SIZE + ENVELOPE_SIZE];
	let total: u32 = record[4..8].iter().map(|&weight| weight as u32).sum();
	if total == 0 {
		return Ok(());
	}
	let scale = 1.0 / total as f32;
	let mut vertex_joints = [0u16; 4];
	let mut vertex_weights = [0f32; 4];
	for slot in 0..4 {
		let weight = record[4 + slot];
		if weight == 0 {
			continue;
		}
		let local = record[slot] as usize;
		let bone = match table {
			Some(table) => match table.get(local) {
				Some(&bone) => bone,
				None => {
					return Err(MeshError::Invalid(format!(
						"vertex {vertex} uses subset bone slot {local} of {}",
						table.len()
					)));
				}
			},
			None => local as u16,
		};
		if bone as usize >= bone_count {
			return Err(MeshError::Invalid(format!("vertex {vertex} uses bone {bone} of {bone_count}")));
		}
		vertex_joints[slot] = bone;
		vertex_weights[slot] = weight as f32 * scale;
	}
	joints[vertex] = vertex_joints;
	weights[vertex] = vertex_weights;
	Ok(())
}

/// Reads the NUL-terminated (UTF-8) name starting at `offset` in the name table.
fn bone_name(names: &[u8], offset: usize) -> Result<String, MeshError> {
	let Some(tail) = names.get(offset..) else {
		return Err(MeshError::Invalid(format!(
			"bone name offset {offset} beyond the {}-byte name table",
			names.len()
		)));
	};
	let end = tail.iter().position(|&byte| byte == 0).unwrap_or(tail.len());
	Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
}
