//! Skinning diagnostics: the skinned MeshParts of model files, the Bones ror poses them with
//! (`scene::bones`, the rules in notes/mesh.md "Skinning in Studio") and how far those sit from the
//! meshes' bind poses.
//!
//! ```text
//! cargo run -p ror --release --example tools -- bones_scan <model files...> [--bones]
//! cargo run -p ror --release --example tools -- bones_scan <model file> --tree <dotted path>
//! cargo run -p ror --release --example tools -- bones_scan --mesh <uri>
//! ```
//!
//! `--bones` lists every bone of each skinned MeshPart; `--tree` prints an instance subtree (parts'
//! anchoring, joints and the MeshParts' HasSkinnedMesh); `--mesh` lists a mesh's bones and weights.

use glam::{Affine3A, Mat3A, Vec3, Vec3A};
use rbx_dom_weak::types::{Ref, Variant};
use ror::scene::bones::Skeletons;
use ror::scene::{load, props};

fn bind_cframe(bone: &ror_mesh::Bone) -> Affine3A {
	let r = bone.rotation;
	let matrix = Mat3A::from_cols(Vec3A::new(r[0][0], r[1][0], r[2][0]), Vec3A::new(r[0][1], r[1][1], r[2][1]), Vec3A::new(r[0][2], r[1][2], r[2][2]));
	Affine3A::from_mat3_translation(matrix.into(), Vec3::from(bone.position))
}

fn decode(assets: &ror_assets::Assets, uri: &str) -> anyhow::Result<ror_mesh::Mesh> {
	let fetched = assets.fetch(uri).map_err(|error| anyhow::anyhow!("{error}"))?;
	Ok(ror_mesh::decode_mesh(&fetched.bytes)?)
}

fn list_mesh(assets: &ror_assets::Assets, uri: &str) -> anyhow::Result<()> {
	let mesh = decode(assets, uri)?;
	let (min, max) = mesh.lod0_bounds().unwrap_or_default();
	println!("{uri}: {:?}, {} vertices, LOD0 bounds {min:?} .. {max:?}", mesh.format, mesh.vertex_count());
	let triangles: Vec<String> = mesh.lods.iter().map(|lod| ((lod.end - lod.start) / 3).to_string()).collect();
	println!("  LOD type {}, {} high-quality, triangles per LOD [{}]", mesh.lod_type, mesh.high_quality_lods, triangles.join(", "));
	let Some(skinning) = &mesh.skinning else {
		println!("  no bones");
		return Ok(());
	};
	let mut weighted = vec![0usize; skinning.bones.len()];
	let mut dominant = vec![0usize; skinning.bones.len()];
	for (joints, weights) in skinning.joints.iter().zip(&skinning.weights) {
		for slot in 0..4 {
			if weights[slot] > 0.0 {
				weighted[joints[slot] as usize] += 1;
			}
			if weights[slot] > 0.5 {
				dominant[joints[slot] as usize] += 1;
			}
		}
	}
	let unweighted = skinning.weights.iter().filter(|weights| weights.iter().sum::<f32>() == 0.0).count();
	println!("  {} bones, {} subsets, {unweighted} unweighted vertices", skinning.bones.len(), skinning.subsets.len());
	for (index, bone) in skinning.bones.iter().enumerate() {
		let bind = bind_cframe(bone);
		let parent = bone.parent.and_then(|parent| skinning.bones.get(parent as usize)).map(|parent| parent.name.as_str()).unwrap_or("-");
		println!("  [{index}] {} (parent {parent}) at {} up {}: {} vertices weighted, {} mostly", bone.name, Vec3::from(bind.translation), Vec3::from(bind.matrix3.y_axis), weighted[index], dominant[index]);
	}
	Ok(())
}

fn print_tree(document: &load::Document, root: Ref) {
	let mut stack = vec![(root, 0usize)];
	while let Some((referent, depth)) = stack.pop() {
		let Some(instance) = document.dom.get_by_ref(referent) else {
			continue;
		};
		let mut extra = String::new();
		if props::is_a(instance, "BasePart") {
			extra = format!(" anchored={}", props::bool(instance, "Anchored", false));
			if instance.class == "MeshPart" {
				extra += &format!(" HasSkinnedMesh={}", props::bool(instance, "HasSkinnedMesh", false));
			}
		}
		if props::is_a(instance, "JointInstance") || instance.class == "WeldConstraint" {
			let name = |key: &str| match instance.properties.get(&rbx_dom_weak::ustr(key)) {
				Some(Variant::Ref(reference)) => document.dom.get_by_ref(*reference).map(|part| part.name.clone()).unwrap_or_default(),
				_ => String::new(),
			};
			extra = format!(" {} -> {}", name("Part0"), name("Part1"));
		}
		// long Bone chains: their roots only
		if instance.class == "Bone" && document.dom.get_by_ref(instance.parent()).is_some_and(|parent| parent.class == "Bone") {
			continue;
		}
		println!("{}{} {}{extra}", "  ".repeat(depth), instance.class, instance.name);
		for &child in instance.children().iter().rev() {
			stack.push((child, depth + 1));
		}
	}
}

pub fn main() -> anyhow::Result<()> {
	let mut files = Vec::new();
	let mut every_bone = false;
	let mut tree: Option<String> = None;
	let mut meshes: Vec<String> = Vec::new();
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--bones" => every_bone = true,
			"--tree" => tree = args.next(),
			"--mesh" => meshes.extend(args.next()),
			_ => files.push(arg),
		}
	}
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(ror::output::repo_dir()), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	for uri in &meshes {
		list_mesh(&assets, uri)?;
	}
	for file in files {
		let mut document = load::read_file(std::path::Path::new(&file))?;
		if let Some(path) = &tree {
			document.select(path)?;
			print_tree(&document, document.roots[0]);
			continue;
		}
		let mut stack: Vec<Ref> = document.roots.clone();
		let (mut bones, mut mesh_parts) = (0, Vec::new());
		while let Some(referent) = stack.pop() {
			let Some(instance) = document.dom.get_by_ref(referent) else {
				continue;
			};
			match instance.class.as_str() {
				"Bone" => bones += 1,
				"MeshPart" => mesh_parts.push(referent),
				_ => {}
			}
			stack.extend(instance.children().iter().copied());
		}
		println!("{file}: {bones} Bones, {} MeshParts", mesh_parts.len());
		let mut skeletons = Skeletons::default();
		for part in mesh_parts {
			let instance = document.dom.get_by_ref(part).expect("part");
			let Some(uri) = props::content(instance, &["MeshContent", "MeshId"]) else {
				continue;
			};
			let mesh = match decode(&assets, &uri) {
				Ok(mesh) => mesh,
				Err(error) => {
					println!("  {}: {error:#}", document.path_of(part));
					continue;
				}
			};
			let Some(skinning) = &mesh.skinning else {
				continue;
			};
			let skeleton = skeletons.for_part(&document, part);
			// the bind poses in the part (notes/mesh.md: scaled position, bind rotation) against the Bones
			let size = props::vec3(instance, "Size", Vec3::ONE);
			let scale = size / props::vec3(instance, "InitialSize", size);
			let (min, max) = mesh.lod0_bounds().map(|(min, max)| (Vec3::from(min), Vec3::from(max))).unwrap_or_default();
			let centre = (min + max) * 0.5;
			let to_part = props::cframe(instance, "CFrame").unwrap_or_default().inverse();
			let (mut found, mut furthest) = (0, (0.0f32, 0.0f32, String::new()));
			let mut lines = Vec::new();
			for bone in &skinning.bones {
				let bind = bind_cframe(bone);
				let Some(world) = skeleton.as_ref().and_then(|skeleton| skeleton.bones.get(&bone.name)) else {
					lines.push(format!("    {}: no Bone", bone.name));
					continue;
				};
				found += 1;
				let relative = to_part * *world;
				let distance = (Vec3::from(relative.translation) - scale * (Vec3::from(bind.translation) - centre)).length();
				let turn = (relative.matrix3 - bind.matrix3).abs();
				let turn = turn.x_axis.max(turn.y_axis).max(turn.z_axis).max_element();
				if distance + turn > furthest.0 + furthest.1 {
					furthest = (distance, turn, bone.name.clone());
				}
				lines.push(format!("    {}: {distance:.4} studs, {turn:.4} rotation from bind", bone.name));
			}
			println!(
				"  {} {uri}: {} bones, {found} with a Bone; HasSkinnedMesh {}; scale {scale}; furthest from bind {} ({:.4} studs, {:.4} rotation)",
				document.path_of(part),
				skinning.bones.len(),
				props::bool(instance, "HasSkinnedMesh", false),
				furthest.2,
				furthest.0,
				furthest.1
			);
			if every_bone {
				for line in lines {
					println!("{line}");
				}
			}
		}
	}
	Ok(())
}
