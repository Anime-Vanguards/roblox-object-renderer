//! Geometric sanity checks for decoded meshes (used by the tests and the `mesh_scan` tool).
//!
//! [`check`] returns the problems it finds; [`stats`] returns the measurements behind them so a scan
//! can aggregate conventions (winding, tangent frames) over many meshes. Per-vertex checks look at
//! the vertices the highest-detail LOD uses, which is what a renderer draws.

use crate::{Mesh, MeshFormat};

/// How bad a failed check is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
	/// Known artefact of Roblox's own data that does not affect LOD0 (e.g. NaN vertices that only
	/// lower LODs of old simplifier output use).
	Note,
	/// Odd data a renderer may want to handle (inside-out meshes, degenerate normals, unweighted
	/// skinned vertices, tangents that do not match the UVs).
	Warning,
	/// Structurally inconsistent output: a decoder bug or corrupt input.
	Error,
}

/// One failed check.
#[derive(Clone, Debug)]
pub struct Issue {
	/// Stable check name (`index-range`, `winding`, ...).
	pub check: &'static str,
	pub severity: Severity,
	pub detail: String,
}

/// Measurements over the highest-detail LOD and the vertices it uses.
#[derive(Clone, Debug, Default)]
pub struct Stats {
	pub triangles: usize,
	/// Triangles with (near) zero area.
	pub degenerate: usize,
	/// Non-degenerate triangles whose geometric normal (counter-clockwise winding, right-handed)
	/// points away from the sum of their vertex normals.
	pub against_normals: usize,
	/// Distinct vertices LOD0 uses.
	pub vertices: usize,
	/// ... whose normal is not unit length (tolerance 1e-3), excluding zero normals.
	pub bad_normal_length: usize,
	/// ... whose normal is zero (degenerate in the file).
	pub zero_normals: usize,
	/// ... with a non-finite position, normal, uv or tangent.
	pub non_finite: usize,
	/// Vertices outside LOD0 with non-finite data.
	pub non_finite_elsewhere: usize,
	/// ... whose tangent xyz is not unit length (tolerance 0.05).
	pub bad_tangent_length: usize,
	/// ... whose unit tangent is not perpendicular to the normal (`|dot| > 0.1`).
	pub tangent_not_perpendicular: usize,
	/// ... whose tangent `w` is not ±1.
	pub bad_tangent_sign: usize,
	/// Triangles with a usable UV-derived tangent frame.
	pub uv_frames: usize,
	/// ... where the stored tangent points along +dP/du (stored UVs).
	pub tangent_along_u: usize,
	/// ... where `w * cross(n, t)` points along +dP/dv (stored UVs).
	pub bitangent_along_v: usize,
	pub uv_min: [f32; 2],
	pub uv_max: [f32; 2],
}

/// Whether the mesh's tangents can be used as stored: present, finite, unit length and
/// perpendicular to the normals for at least 95% of the vertices. Pre-PBR meshes store zero bytes
/// (decoding to `(-1, -1, -1, -1)`); renderers should generate tangents for those.
pub fn tangents_usable(mesh: &Mesh) -> bool {
	if mesh.tangents.len() != mesh.positions.len() || mesh.tangents.is_empty() {
		return false;
	}
	let good = mesh.tangents.iter().zip(&mesh.normals).filter(|(tangent, normal)| tangent_ok(tangent, normal)).count();
	good * 20 >= mesh.tangents.len() * 19
}

fn tangent_ok(tangent: &[f32; 4], normal: &[f32; 3]) -> bool {
	let t = [tangent[0], tangent[1], tangent[2]];
	(length(t) - 1.0).abs() < 0.05 && dot(t, *normal).abs() < 0.1 && (tangent[3].abs() - 1.0).abs() < 0.01
}

pub fn stats(mesh: &Mesh) -> Stats {
	let mut stats = Stats { uv_min: [f32::INFINITY; 2], uv_max: [f32::NEG_INFINITY; 2], ..Stats::default() };
	let vertex_count = mesh.positions.len();
	let mut in_lod0 = vec![false; vertex_count];
	for &index in mesh.lod0() {
		if let Some(used) = in_lod0.get_mut(index as usize) {
			*used = true;
		}
	}
	let union = matches!(mesh.format, MeshFormat::Union { .. } | MeshFormat::Solid { .. });
	for (vertex, &used) in in_lod0.iter().enumerate() {
		let finite = mesh.positions[vertex].iter().all(|value| value.is_finite())
			&& mesh.normals.get(vertex).is_none_or(|normal| normal.iter().all(|value| value.is_finite()))
			&& mesh.uvs.get(vertex).is_none_or(|uv| uv.iter().all(|value| value.is_finite()))
			&& mesh.tangents.get(vertex).is_none_or(|tangent| tangent.iter().all(|value| value.is_finite()));
		if !used {
			if !finite {
				stats.non_finite_elsewhere += 1;
			}
			continue;
		}
		stats.vertices += 1;
		if !finite {
			stats.non_finite += 1;
			continue;
		}
		if let Some(normal) = mesh.normals.get(vertex) {
			let length = length(*normal);
			if length == 0.0 {
				stats.zero_normals += 1;
			} else if (length - 1.0).abs() > 1e-3 {
				stats.bad_normal_length += 1;
			}
		}
		if let Some(uv) = mesh.uvs.get(vertex) {
			stats.uv_min = [stats.uv_min[0].min(uv[0]), stats.uv_min[1].min(uv[1])];
			stats.uv_max = [stats.uv_max[0].max(uv[0]), stats.uv_max[1].max(uv[1])];
		}
		if let (Some(tangent), Some(normal)) = (mesh.tangents.get(vertex), mesh.normals.get(vertex)) {
			let t = [tangent[0], tangent[1], tangent[2]];
			if (length(t) - 1.0).abs() > 0.05 {
				stats.bad_tangent_length += 1;
			} else if dot(t, *normal).abs() > 0.1 {
				stats.tangent_not_perpendicular += 1;
			}
			if !union && (tangent[3].abs() - 1.0).abs() > 0.01 {
				stats.bad_tangent_sign += 1;
			}
		}
	}

	let frames =
		mesh.uvs.len() == vertex_count && mesh.tangents.len() == vertex_count && mesh.normals.len() == vertex_count;
	for triangle in mesh.lod0().chunks_exact(3) {
		let [a, b, c] = [triangle[0] as usize, triangle[1] as usize, triangle[2] as usize];
		if a >= vertex_count || b >= vertex_count || c >= vertex_count {
			continue;
		}
		stats.triangles += 1;
		let (pa, pb, pc) = (mesh.positions[a], mesh.positions[b], mesh.positions[c]);
		let e1 = sub(pb, pa);
		let e2 = sub(pc, pa);
		let face = cross(e1, e2);
		let area2 = length(face);
		let scale = length(e1).max(length(e2));
		// (written so that NaN areas count as degenerate)
		if area2.partial_cmp(&(scale * scale * 1e-6)) != Some(std::cmp::Ordering::Greater) || !area2.is_finite() {
			stats.degenerate += 1;
			continue;
		}
		if let (Some(na), Some(nb), Some(nc)) = (mesh.normals.get(a), mesh.normals.get(b), mesh.normals.get(c))
			&& dot(add(add(*na, *nb), *nc), face) < 0.0
		{
			stats.against_normals += 1;
		}
		if !frames {
			continue;
		}
		let (ua, ub, uc) = (mesh.uvs[a], mesh.uvs[b], mesh.uvs[c]);
		let (du1, dv1) = (ub[0] - ua[0], ub[1] - ua[1]);
		let (du2, dv2) = (uc[0] - ua[0], uc[1] - ua[1]);
		let det = du1 * dv2 - du2 * dv1;
		if det.abs() < 1e-12 || !det.is_finite() {
			continue;
		}
		let dpdu = scale3(sub(scale3(e1, dv2), scale3(e2, dv1)), 1.0 / det);
		let dpdv = scale3(sub(scale3(e2, du1), scale3(e1, du2)), 1.0 / det);
		let tangent = mesh.tangents[a];
		let t = [tangent[0], tangent[1], tangent[2]];
		if (length(t) - 1.0).abs() > 0.05 {
			continue;
		}
		stats.uv_frames += 1;
		if dot(t, dpdu) > 0.0 {
			stats.tangent_along_u += 1;
		}
		if dot(scale3(cross(mesh.normals[a], t), tangent[3]), dpdv) > 0.0 {
			stats.bitangent_along_v += 1;
		}
	}
	stats
}

/// Structural and geometric checks.
pub fn check(mesh: &Mesh) -> Vec<Issue> {
	use Severity::{Error, Note, Warning};
	let mut issues = Vec::new();
	let mut issue =
		|check: &'static str, severity: Severity, detail: String| issues.push(Issue { check, severity, detail });
	let vertex_count = mesh.positions.len();

	for (name, len, optional) in [
		("normals", mesh.normals.len(), false),
		("uvs", mesh.uvs.len(), true),
		("tangents", mesh.tangents.len(), true),
		("colors", mesh.colors.len(), true),
		("normal_ids", mesh.normal_ids.len(), true),
	] {
		if len != vertex_count && !(optional && len == 0) {
			issue("array-length", Error, format!("{len} {name} for {vertex_count} vertices"));
		}
	}
	if !mesh.indices.len().is_multiple_of(3) {
		issue("index-count", Error, format!("{} indices", mesh.indices.len()));
	}
	if let Some(max) = mesh.indices.iter().max()
		&& *max as usize >= vertex_count
	{
		issue("index-range", Error, format!("index {max} with {vertex_count} vertices"));
	}
	if mesh.lods.is_empty() && !mesh.indices.is_empty() {
		issue("lod-range", Error, "no LODs".into());
	}
	let mut previous_end = 0;
	for (level, range) in mesh.lods.iter().enumerate() {
		if range.start > range.end
			|| range.end as usize > mesh.indices.len()
			|| !range.start.is_multiple_of(3)
			|| !range.end.is_multiple_of(3)
		{
			issue("lod-range", Error, format!("LOD {level} {range:?} with {} indices", mesh.indices.len()));
		} else if range.start < previous_end {
			issue("lod-range", Error, format!("LOD {level} {range:?} overlaps the previous level"));
		} else if range.is_empty() {
			issue("lod-range", Error, format!("LOD {level} is empty"));
		}
		previous_end = range.end;
	}

	let stats = stats(mesh);
	let fraction = |count: usize, total: usize| if total == 0 { 0.0 } else { count as f64 / total as f64 };
	if stats.non_finite > 0 {
		issue(
			"non-finite",
			Error,
			format!("{} of {} LOD0 vertices have non-finite data", stats.non_finite, stats.vertices),
		);
	}
	if stats.non_finite_elsewhere > 0 {
		issue(
			"non-finite-lower-lod",
			Note,
			format!("{} vertices used only by lower LODs are non-finite", stats.non_finite_elsewhere),
		);
	}
	if stats.bad_normal_length > 0 {
		issue(
			"normal-length",
			Error,
			format!("{} of {} normals not unit length", stats.bad_normal_length, stats.vertices),
		);
	}
	if fraction(stats.zero_normals, stats.vertices) > 0.01 {
		issue(
			"zero-normals",
			Warning,
			format!("{} of {} LOD0 vertices have zero normals", stats.zero_normals, stats.vertices),
		);
	}
	let live = stats.triangles - stats.degenerate;
	if fraction(stats.against_normals, live) > 0.5 {
		issue(
			"winding",
			Warning,
			format!("{} of {live} triangles wind against their vertex normals", stats.against_normals),
		);
	}
	if stats.uv_min.iter().chain(&stats.uv_max).any(|value| value.abs() > 1e4) {
		issue("uv-range", Warning, format!("uv range {:?}..{:?}", stats.uv_min, stats.uv_max));
	}
	if tangents_usable(mesh) && fraction(stats.uv_frames - stats.tangent_along_u, stats.uv_frames) > 0.5 {
		issue(
			"tangent-frame",
			Warning,
			format!("tangent along +dP/du in {} of {} triangles", stats.tangent_along_u, stats.uv_frames),
		);
	}

	if let Some(skinning) = &mesh.skinning {
		let bone_count = skinning.bones.len();
		if skinning.joints.len() != vertex_count || skinning.weights.len() != vertex_count {
			issue(
				"skinning",
				Error,
				format!(
					"{} joints / {} weights for {vertex_count} vertices",
					skinning.joints.len(),
					skinning.weights.len()
				),
			);
		}
		let mut unweighted = 0;
		for (joints, weights) in skinning.joints.iter().zip(&skinning.weights) {
			let sum: f32 = weights.iter().sum();
			if sum == 0.0 {
				unweighted += 1;
			} else if (sum - 1.0).abs() > 1e-3 || joints.iter().any(|&joint| joint as usize >= bone_count) {
				issue("skinning", Error, format!("joints {joints:?} weights {weights:?}"));
				break;
			}
		}
		if unweighted > 0 {
			issue(
				"skinning-unweighted",
				Warning,
				format!("{unweighted} of {vertex_count} vertices have no bone weights"),
			);
		}
		for (index, bone) in skinning.bones.iter().enumerate() {
			if bone.parent.is_some_and(|parent| parent as usize >= bone_count || parent as usize == index) {
				issue(
					"bone-parent",
					Error,
					format!("bone {index} {:?} parent {:?} of {bone_count}", bone.name, bone.parent),
				);
			}
			let r = bone.rotation;
			let columns = [[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]];
			let orthonormal = (0..3)
				.all(|i| (0..3).all(|j| (dot(columns[i], columns[j]) - if i == j { 1.0 } else { 0.0 }).abs() < 1e-3));
			if !orthonormal || dot(cross(columns[0], columns[1]), columns[2]) < 0.0 {
				issue(
					"bone-rotation",
					Warning,
					format!("bone {index} {:?} rotation {r:?} is not a proper rotation", bone.name),
				);
			}
		}
		if has_parent_cycle(skinning) {
			issue("bone-parent", Error, "bone parents form a cycle".into());
		}
		let face_count = (mesh.indices.len() / 3) as u32;
		for (index, subset) in skinning.subsets.iter().enumerate() {
			if subset.vertices.end as usize > vertex_count || subset.faces.end > face_count {
				issue(
					"subset-range",
					Error,
					format!("subset {index} faces {:?} vertices {:?}", subset.faces, subset.vertices),
				);
			}
			if subset.bone_indices.iter().any(|&bone| bone as usize >= bone_count) {
				issue("subset-bones", Error, format!("subset {index} bones {:?} of {bone_count}", subset.bone_indices));
			}
		}
	}
	issues
}

fn has_parent_cycle(skinning: &crate::Skinning) -> bool {
	let count = skinning.bones.len();
	for start in 0..count {
		let mut current = start;
		for _ in 0..=count {
			match skinning.bones[current].parent {
				Some(parent) if (parent as usize) < count => current = parent as usize,
				_ => break,
			}
			if current == start {
				return true;
			}
		}
	}
	false
}

#[inline]
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
	[a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
	[a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn scale3(a: [f32; 3], s: f32) -> [f32; 3] {
	[a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
	a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
	[a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

#[inline]
fn length(a: [f32; 3]) -> f32 {
	dot(a, a).sqrt()
}
