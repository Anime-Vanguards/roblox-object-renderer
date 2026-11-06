//! Geometry experiments (notes/mesh.md "Placement in Studio", "Unions in Studio", "LOD in Studio"):
//! where Studio puts a MeshPart's or a SpecialMesh's mesh relative to its part (un-centred meshes
//! from Studio's own content), how it draws unions from each of their geometry sources, and which
//! LOD of a mesh it draws at which distance and RenderFidelity.

use anyhow::{Context, Result};
use glam::{Affine3A, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3uint8, ContentId, Enum, Ref, Vector3};

use super::{Experiment, Run};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, cframe};

/// Studio's classic head variant whose bounds are 0.1 above its origin: y -0.525 .. 0.725.
const HEAD_A: &str = "rbxasset://avatar/heads/headA.mesh";
/// y -0.75 .. 0.85 (centre +0.05).
const HEAD_E: &str = "rbxasset://avatar/heads/headE.mesh";
/// The centred classic head (bounds ±0.599).
const HEAD: &str = "rbxasset://avatar/heads/head.mesh";
/// A flat sheet far from its origin: x 0.02 .. 974, y 42 .. 522, z 63.61 .. 63.68.
const SHIRT_TEMPLATE: &str = "rbxasset://avatar/compositing/CompositShirtTemplate.mesh";
/// A gray grid (121..219), opaque.
const GRID: &str = "rbxasset://textures/grid4.png";

/// The LOD0 bounds of a mesh.
fn mesh_bounds(uri: &str) -> Result<(Vec3, Vec3)> {
	let config = crate::output::asset_config(crate::output::state_dir(), false);
	let fetched = ror_assets::Assets::new(config).fetch(uri).map_err(|error| anyhow::anyhow!("{error}"))?;
	let mesh = ror_mesh::decode_mesh(&fetched.bytes)?;
	let (min, max) = mesh.lod0_bounds().context("empty mesh")?;
	Ok((Vec3::from(min), Vec3::from(max)))
}

/// A builder whose root is a Folder (each case then picks its own container).
fn folder_root(name: &str) -> ModelBuilder {
	let mut model = ModelBuilder::new(name);
	if let Some(root) = model.dom.get_by_ref_mut(model.model) {
		root.class = rbx_dom_weak::ustr("Folder");
	}
	model
}

fn lit() -> Environment {
	Environment {
		brightness: 2.0,
		ambient: Vec3::splat(0.35),
		outdoor_ambient: Vec3::splat(0.35),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// Flat ambient light only: colours read directly.
fn flat() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::splat(0.7),
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

/// A view of `target` from `distance` studs along -Z (the front), slightly from above.
fn front(label: &str, target: Vec3, distance: f32, fov: f32, environment: Environment) -> Run {
	let eye = target + Vec3::new(0.0, distance * 0.15, -distance);
	Run { label: label.into(), environment, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov } }
}

fn mesh_part(model: &mut ModelBuilder, name: &str, uri: &str, at: Affine3A, size: Vec3, initial: Option<Vec3>) -> Ref {
	let parent = model.model;
	let part = model.child(
		parent,
		InstanceBuilder::new("MeshPart")
			.with_name(name)
			.with_property("CFrame", cframe(at))
			.with_property("Size", Vector3::new(size.x, size.y, size.z))
			.with_property("Color", Color3uint8::new(205, 205, 205))
			.with_property("Material", Enum::from_u32(272))
			.with_property("Anchored", true),
	);
	model.set(part, "MeshId", ContentId::from(uri));
	if let Some(initial) = initial {
		model.set(part, "InitialSize", Vector3::new(initial.x, initial.y, initial.z));
	}
	part
}

/// `mesh-origin`: MeshParts of un-centred meshes (Studio's headA / headE, whose bounds sit above
/// their origin, and the shirt template far from its origin) at several Size / InitialSize ratios,
/// turned, with a PivotOffset, without InitialSize, and with an InitialSize unlike the mesh's
/// bounds; head.mesh (centred) for reference. Each seen from the front with a reference block.
pub fn mesh_origin() -> Result<Experiment> {
	let mut model = folder_root("MeshOrigin");
	let mut runs = Vec::new();
	let (head_a_min, head_a_max) = mesh_bounds(HEAD_A)?;
	let head_a = head_a_max - head_a_min;
	let (head_e_min, head_e_max) = mesh_bounds(HEAD_E)?;
	let head_e = head_e_max - head_e_min;
	let (head_min, head_max) = mesh_bounds(HEAD)?;
	let head = head_max - head_min;
	let (sheet_min, sheet_max) = mesh_bounds(SHIRT_TEMPLATE)?;
	let sheet = sheet_max - sheet_min;
	// (label, mesh, size, InitialSize, rotation about Z in degrees)
	let cases: [(&str, &str, Vec3, Option<Vec3>, f32); 9] = [
		("head-x3", HEAD, head * 3.0, Some(head), 0.0),
		("headA-x1", HEAD_A, head_a, Some(head_a), 0.0),
		("headA-x3", HEAD_A, head_a * 3.0, Some(head_a), 0.0),
		("headA-stretch", HEAD_A, head_a * Vec3::new(3.0, 1.5, 3.0), Some(head_a), 0.0),
		("headE-x3", HEAD_E, head_e * 3.0, Some(head_e), 0.0),
		("headA-turned", HEAD_A, head_a * 3.0, Some(head_a), 90.0),
		("headA-pivot", HEAD_A, head_a * 3.0, Some(head_a), 0.0),
		("headA-no-initial", HEAD_A, Vec3::splat(2.0), None, 0.0),
		("headA-initial-x2", HEAD_A, head_a * 2.0, Some(head_a * 2.0), 0.0),
	];
	for (index, (label, uri, size, initial, turn)) in cases.iter().enumerate() {
		let centre = Vec3::new(index as f32 * 16.0, 0.0, 0.0);
		let at = Affine3A::from_translation(centre) * Affine3A::from_rotation_z(turn.to_radians());
		let part = mesh_part(&mut model, label, uri, at, *size, *initial);
		if *label == "headA-pivot" {
			model.set(part, "PivotOffset", cframe(Affine3A::from_translation(Vec3::new(2.0, 1.0, 0.0))));
		}
		// a thin black reference bar 3 studs behind the part, centred on the part position
		let backdrop = model.model;
		model.child(
			backdrop,
			InstanceBuilder::new("Part")
				.with_name(format!("{label}-marker"))
				.with_property("CFrame", cframe(Affine3A::from_translation(centre + Vec3::new(0.0, 0.0, 3.0))))
				.with_property("Size", Vector3::new(6.0, 0.05, 0.05))
				.with_property("Color", Color3uint8::new(0, 0, 0))
				.with_property("Material", Enum::from_u32(272))
				.with_property("Anchored", true),
		);
		runs.push(front(label, centre, 14.0, 30.0, lit()));
	}
	// the shirt template at 1 / 100: drawn at its origin it sits ~(4.9, 2.8, 1) studs off the part
	let centre = Vec3::new(cases.len() as f32 * 16.0 + 10.0, 0.0, 0.0);
	let size = Vec3::new(sheet.x * 0.01, sheet.y * 0.01, 0.001);
	let part = mesh_part(&mut model, "template", SHIRT_TEMPLATE, Affine3A::from_translation(centre), size, Some(sheet));
	model.set(part, "DoubleSided", true);
	runs.push(front("template", centre + Vec3::new(2.0, 1.5, 0.0), 30.0, 40.0, lit()));
	Ok(Experiment { name: "mesh-origin".into(), model, probes: Vec::new(), runs })
}

/// A Part with a SpecialMesh.
fn special_part(model: &mut ModelBuilder, name: &str, at: Affine3A, size: Vec3, color: [u8; 3], mesh: InstanceBuilder) -> Ref {
	let parent = model.model;
	let part = model.child(
		parent,
		InstanceBuilder::new("Part")
			.with_name(name)
			.with_property("CFrame", cframe(at))
			.with_property("Size", Vector3::new(size.x, size.y, size.z))
			.with_property("Color", Color3uint8::new(color[0], color[1], color[2]))
			.with_property("Material", Enum::from_u32(272))
			.with_property("Anchored", true),
	);
	model.child(part, mesh);
	part
}

fn file_mesh(uri: &str, scale: Vec3, offset: Vec3) -> InstanceBuilder {
	InstanceBuilder::new("SpecialMesh")
		.with_name("Mesh")
		.with_property("MeshType", Enum::from_u32(5))
		.with_property("MeshId", ContentId::from(uri))
		.with_property("Scale", Vector3::new(scale.x, scale.y, scale.z))
		.with_property("Offset", Vector3::new(offset.x, offset.y, offset.z))
}

/// `specialmesh-placement`: FileMesh SpecialMeshes of the un-centred headA (Scale 3, with an
/// Offset, on a turned part, on a part of another Size) and of the shirt template at Scale 1/100;
/// then VertexColor on a grid-textured head under flat light (white, half gray, red-ish, doubled,
/// and red without a texture).
pub fn specialmesh_placement() -> Result<Experiment> {
	let mut model = folder_root("SpecialMeshPlacement");
	let mut runs = Vec::new();
	let three = Vec3::splat(3.0);
	// (label, part size, rotation about Z, scale, offset)
	let cases: [(&str, Vec3, f32, Vec3, Vec3); 4] = [
		("fm-headA", Vec3::new(2.0, 1.0, 1.0), 0.0, three, Vec3::ZERO),
		("fm-headA-offset", Vec3::new(2.0, 1.0, 1.0), 0.0, three, Vec3::new(0.5, 1.0, 0.0)),
		("fm-headA-turned-offset", Vec3::new(2.0, 1.0, 1.0), 90.0, three, Vec3::new(0.5, 1.0, 0.0)),
		("fm-headA-partsize", Vec3::new(6.0, 4.0, 2.0), 0.0, three, Vec3::ZERO),
	];
	for (index, (label, size, turn, scale, offset)) in cases.iter().enumerate() {
		let centre = Vec3::new(index as f32 * 16.0, 0.0, 0.0);
		let at = Affine3A::from_translation(centre) * Affine3A::from_rotation_z(turn.to_radians());
		special_part(&mut model, label, at, *size, [205, 205, 205], file_mesh(HEAD_A, *scale, *offset));
		runs.push(front(label, centre, 14.0, 30.0, lit()));
	}
	let centre = Vec3::new(cases.len() as f32 * 16.0 + 10.0, 0.0, 0.0);
	special_part(&mut model, "fm-template", Affine3A::from_translation(centre), Vec3::splat(1.0), [205, 205, 205], file_mesh(SHIRT_TEMPLATE, Vec3::splat(0.01), Vec3::ZERO));
	runs.push(front("fm-template", centre + Vec3::new(2.0, 1.5, 0.0), 30.0, 40.0, lit()));
	runs.push(Run {
		label: "fm-template-back".into(),
		environment: lit(),
		camera: CameraSpec::LookAt { eye: (centre + Vec3::new(2.0, 6.0, 30.0)).into(), target: (centre + Vec3::new(2.0, 1.5, 0.0)).into(), fov: 40.0 },
	});
	// VertexColor: the textured classic head (Scale 4), flat light
	let colors: [(&str, Option<&str>, [u8; 3], Vec3); 6] = [
		("vc-white", Some(GRID), [205, 205, 205], Vec3::ONE),
		("vc-half", Some(GRID), [205, 205, 205], Vec3::splat(0.5)),
		("vc-red", Some(GRID), [205, 205, 205], Vec3::new(1.0, 0.25, 0.25)),
		("vc-double", Some(GRID), [205, 205, 205], Vec3::splat(2.0)),
		("vc-white-redpart", Some(GRID), [255, 0, 0], Vec3::ONE),
		("vc-red-untextured", None, [205, 205, 205], Vec3::new(1.0, 0.0, 0.0)),
	];
	for (index, (label, texture, color, vertex_color)) in colors.iter().enumerate() {
		let centre = Vec3::new(index as f32 * 16.0, 0.0, 40.0);
		let mut mesh = file_mesh(HEAD, Vec3::splat(4.0), Vec3::ZERO).with_property("VertexColor", Vector3::new(vertex_color.x, vertex_color.y, vertex_color.z));
		if let Some(texture) = texture {
			mesh = mesh.with_property("TextureId", ContentId::from(*texture));
		}
		special_part(&mut model, label, Affine3A::from_translation(centre), Vec3::splat(2.0), *color, mesh);
		runs.push(front(label, centre, 14.0, 30.0, flat()));
	}
	Ok(Experiment { name: "specialmesh-placement".into(), model, probes: Vec::new(), runs })
}



/// Studio's asset-importer preview mesh: five LODs (3832, 1916, 958, 382, 190 triangles), all five
/// marked high quality (one test mesh marks one).
const HQ_LOD_MESH: &str = "rbxasset://models/AssetImporter/previewMesh.mesh";

/// `mesh-lod-hq`: `mesh-lod`'s distances (Automatic, both cameras) on a mesh whose file marks all
/// five LODs high quality, four times its InitialSize (22 studs long).
pub fn mesh_lod_hq() -> Result<Experiment> {
	let mut model = folder_root("MeshLodHq");
	let (min, max) = mesh_bounds(HQ_LOD_MESH)?;
	let extent = max - min;
	let centre = Vec3::ZERO;
	mesh_part(&mut model, "preview", HQ_LOD_MESH, Affine3A::IDENTITY, extent * 4.0, Some(extent));
	let mut runs = Vec::new();
	for distance in [20.0f32, 40.0, 80.0, 150.0, 300.0, 600.0, 1000.0, 1500.0] {
		let direction = Vec3::new(0.3, 0.25, -1.0).normalize();
		let eye = centre + direction * distance;
		runs.push(Run { label: format!("hq-fov70-d{distance}"), environment: flat(), camera: CameraSpec::LookAt { eye: eye.into(), target: centre.into(), fov: 70.0 } });
		let fov = (2.0 * (extent.x * 4.0 / distance).atan()).to_degrees();
		runs.push(Run { label: format!("hq-framed-d{distance}"), environment: flat(), camera: CameraSpec::LookAt { eye: eye.into(), target: centre.into(), fov } });
	}
	Ok(Experiment { name: "mesh-lod-hq".into(), model, probes: Vec::new(), runs })
}

