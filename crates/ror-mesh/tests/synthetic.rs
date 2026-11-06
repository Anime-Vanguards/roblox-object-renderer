//! Decoder tests on synthetic test vectors built in `common` (no Roblox data is committed).

mod common;

use common::*;
use ror_mesh::validate::{self, Severity};
use ror_mesh::{Kind, Mesh, MeshError, MeshFormat, decode_any, decode_mesh, decode_slim, decode_union, sniff};

fn close(a: f32, b: f32) -> bool {
	(a - b).abs() < 1e-5
}

/// Checks the quad's attributes as every format stores them.
fn assert_quad(mesh: &Mesh, with_color: bool) {
	assert_eq!(mesh.vertex_count(), 4);
	assert_eq!(mesh.positions[2], [1.0, 1.0, 0.0]);
	assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
	assert_eq!(mesh.uvs[0], [0.0, 1.0]);
	assert_eq!(mesh.uvs[2], [1.0, 0.0]);
	assert_eq!(mesh.tangents[1], [1.0, 0.0, 0.0, 1.0]);
	if with_color {
		assert_eq!(mesh.colors, vec![[255, 128, 64, 255]; 4]);
	} else {
		assert!(mesh.colors.is_empty());
	}
	assert_eq!(&mesh.indices[..6], &[0, 1, 2, 0, 2, 3]);
	assert_no_errors(mesh);
}

fn assert_no_errors(mesh: &Mesh) {
	let errors: Vec<_> = validate::check(mesh).into_iter().filter(|issue| issue.severity == Severity::Error).collect();
	assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn version_1_text() {
	let data = "version 1.00\r\n1\r\n[0,0,0][0,0,2][0.25,0.75,0][2,0,0][0,0,2][1,0.75,0][2,2,1e-006][0,0,2][1,0,0]";
	let mesh = decode_mesh(data.as_bytes()).unwrap();
	assert_eq!(mesh.format, MeshFormat::V1 { minor: 0 });
	// 1.00 positions are halved; uvs are as stored (V from the bottom), normals normalised.
	assert_eq!(mesh.positions, vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.5e-6]]);
	assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
	assert_eq!(mesh.uvs[0], [0.25, 0.75]);
	assert!(mesh.uv_v_flipped());
	assert_eq!(mesh.indices, vec![0, 1, 2]);
	assert_eq!(mesh.lods, vec![0..3]);
	assert!(mesh.tangents.is_empty() && mesh.colors.is_empty());

	let mesh = decode_mesh(data.replace("1.00", "1.01").replace("\r\n", "\n").as_bytes()).unwrap();
	assert_eq!(mesh.format, MeshFormat::V1 { minor: 1 });
	assert_eq!(mesh.positions[1], [2.0, 0.0, 0.0]);

	// Face count and data must agree.
	assert!(decode_mesh(data.replace("\r\n1\r\n", "\r\n2\r\n").as_bytes()).is_err());
	assert!(decode_mesh(b"version 1.00\n1\n[0,0,0][0,0,1]").is_err());
	assert!(decode_mesh(b"version 1.00\n1\n[0,0,x][0,0,1][0,0,0][0,0,0][0,0,1][0,0,0][0,0,0][0,0,1][0,0,0]").is_err());
}

#[test]
fn version_2() {
	for with_color in [true, false] {
		let mesh = decode_mesh(&v2(with_color)).unwrap();
		assert_eq!(mesh.format, MeshFormat::V2);
		assert!(!mesh.uv_v_flipped());
		assert_quad(&mesh, with_color);
		assert_eq!(mesh.lods, vec![0..9]);
	}
}

#[test]
fn version_3_lods() {
	let mesh = decode_mesh(&v3(0, &[0, 2, 3])).unwrap();
	assert_eq!(mesh.format, MeshFormat::V3 { minor: 0 });
	assert_quad(&mesh, true);
	assert_eq!(mesh.lods, vec![0..6, 6..9]);
	assert_eq!(mesh.lod0(), &[0, 1, 2, 0, 2, 3]);

	let mesh = decode_mesh(&v3(1, &[])).unwrap();
	assert_eq!(mesh.format, MeshFormat::V3 { minor: 1 });
	assert_eq!(mesh.lods, vec![0..9]);

	// Decreasing or out-of-range offsets are rejected.
	assert!(decode_mesh(&v3(0, &[0, 3, 2])).is_err());
	assert!(decode_mesh(&v3(0, &[0, 4])).is_err());
}

#[test]
fn version_4_skinning() {
	let mesh = decode_mesh(&v4_v5("4.01", true, &[0, 0], None)).unwrap();
	assert_eq!(mesh.format, MeshFormat::V4 { minor: 1 });
	assert_quad(&mesh, true);
	// `[0, 0]` (Roblox's "no LODs") is one level with every face.
	assert_eq!(mesh.lods, vec![0..9]);
	assert_eq!(mesh.lod_type, 3);
	assert_eq!(mesh.high_quality_lods, 2);

	let skinning = mesh.skinning.as_ref().unwrap();
	assert_eq!(skinning.bones.len(), 2);
	assert_eq!(skinning.bones[0].name, "Root");
	assert_eq!(skinning.bones[0].parent, None);
	assert_eq!(skinning.bones[1].name, "Child");
	assert_eq!(skinning.bones[1].parent, Some(0));
	assert_eq!(skinning.bones[1].position, [0.0, 1.0, 0.0]);
	assert_eq!(skinning.bones[1].rotation, [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
	assert!(close(skinning.bones[0].culling, 2.5));
	assert_eq!(skinning.subsets.len(), 1);
	assert_eq!(skinning.subsets[0].vertices, 0..4);
	assert_eq!(skinning.subsets[0].bone_indices, vec![1, 0]);

	// Slots resolve through the subset table ([1, 0]); weights normalise to 1.
	assert_eq!(skinning.joints[0], [1, 0, 0, 0]);
	assert_eq!(skinning.weights[0], [1.0, 0.0, 0.0, 0.0]);
	assert_eq!(skinning.joints[1], [1, 0, 0, 0]);
	assert!(close(skinning.weights[1][0], 128.0 / 255.0) && close(skinning.weights[1][1], 127.0 / 255.0));
	assert_eq!(skinning.weights[2], [0.0; 4]);
	assert_eq!(skinning.joints[3], [0, 1, 0, 0]);
	assert!(close(skinning.weights[3][0], 0.2) && close(skinning.weights[3][1], 0.8));

	let mesh = decode_mesh(&v4_v5("4.00", false, &[0, 2, 3], None)).unwrap();
	assert_eq!(mesh.format, MeshFormat::V4 { minor: 0 });
	assert!(mesh.skinning.is_none());
	assert_eq!(mesh.lods, vec![0..6, 6..9]);
}

#[test]
fn version_5_facs() {
	let mesh = decode_mesh(&v4_v5("5.00", true, &[0, 2, 3], Some(b"facs-blob"))).unwrap();
	assert_eq!(mesh.format, MeshFormat::V5);
	assert_quad(&mesh, true);
	assert_eq!(mesh.facs.as_deref(), Some(&b"facs-blob"[..]));
	assert!(mesh.skinning.is_some());

	let mesh = decode_mesh(&v4_v5("5.00", false, &[], None)).unwrap();
	assert!(mesh.facs.is_none());
}

#[test]
fn chunked_versions() {
	for (major, draco) in [(6, false), (7, false), (7, true)] {
		let mesh = decode_mesh(&chunked(major, draco)).unwrap();
		assert_eq!(mesh.format, MeshFormat::Chunked { major, draco });
		assert_quad(&mesh, true);
		assert_eq!(mesh.lods, vec![0..6, 6..9]);
		assert_eq!(mesh.lod_type, 3);
		assert_eq!(mesh.high_quality_lods, 1);
		assert_eq!(mesh.facs.as_deref(), Some(&b"FACSDATA"[..]));
		assert_eq!(mesh.hsr_always_visible.as_deref(), Some(&[0b101][..]));
		let skinning = mesh.skinning.as_ref().unwrap();
		assert_eq!(skinning.joints[3], [0, 1, 0, 0]);
	}
	let file = chunked(7, true);
	let chunks = ror_mesh::file_chunks(&file).unwrap();
	let names: Vec<&[u8]> = chunks.iter().map(|chunk| chunk.name).collect();
	assert_eq!(names, vec![&b"COREMESH"[..], b"LODS", b"SKINNING", b"FACS", b"HSRAVIS", b"FUTURE"]);
}

#[test]
fn chunked_errors() {
	// Unknown version of a known chunk.
	let mut w = Writer::new();
	w.raw(b"version 7.00\n");
	chunk(&mut w, "COREMESH", 3, &[0; 8]);
	assert!(matches!(decode_mesh(&w.bytes), Err(MeshError::UnsupportedVersion(_))));
	// No COREMESH.
	let mut w = Writer::new();
	w.raw(b"version 7.00\n");
	chunk(&mut w, "LODS", 1, &lods_chunk(&[]));
	assert!(decode_mesh(&w.bytes).is_err());
	// Envelope count must match the vertices.
	let mut w = Writer::new();
	w.raw(b"version 6.00\n");
	chunk(&mut w, "COREMESH", 1, &coremesh_v1());
	let mut skin = Writer::new();
	skin.u32(2).raw(&[0; 16]).u32(0).u32(0).u32(0);
	chunk(&mut w, "SKINNING", 1, &skin.bytes);
	assert!(decode_mesh(&w.bytes).is_err());
	// A truncated Draco stream.
	let mut w = Writer::new();
	w.raw(b"version 7.00\n");
	let mut core = coremesh_v2();
	let length = core.len();
	core.truncate(length - 3);
	core[..4].copy_from_slice(&((length - 7) as u32).to_le_bytes());
	chunk(&mut w, "COREMESH", 2, &core);
	assert!(matches!(decode_mesh(&w.bytes), Err(MeshError::Draco(_))));
}

#[test]
fn unknown_and_unsupported_headers() {
	assert!(matches!(decode_mesh(b"version 8.00\n"), Err(MeshError::UnsupportedVersion(_))));
	assert!(matches!(decode_mesh(b"hello world, not a mesh"), Err(MeshError::UnknownHeader(_))));
	assert!(matches!(decode_mesh(b""), Err(MeshError::UnknownHeader(_))));
	assert!(matches!(decode_union(b"CSG"), Err(MeshError::UnknownHeader(_))));
	assert_eq!(sniff(b"version 4.01\n"), Kind::FileMesh);
	assert_eq!(sniff(b"<roblox!"), Kind::Model);
	assert_eq!(sniff(b"PNG"), Kind::Unknown);
}

#[test]
fn union_v2_v4() {
	for version in [2, 4] {
		let data = csgmdl_v2_v4(version);
		assert_eq!(sniff(&data), Kind::Union);
		let mesh = decode_union(&data).unwrap();
		assert_eq!(mesh.format, MeshFormat::Union { version });
		assert_eq!(mesh.vertex_count(), 4);
		assert_eq!(mesh.positions[2], [1.0, 1.0, 0.0]);
		assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
		assert_eq!(mesh.colors[0], [10, 20, 30, 255]);
		assert_eq!(mesh.normal_ids, vec![1, 2, 3, 4]);
		assert_eq!(mesh.uvs[1], [4.0, 4.0]);
		assert_eq!(mesh.tangents[0], [1.0, 0.0, 0.0, 1.0]);
		assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
		assert_eq!(mesh.lods, vec![0..6]);
		assert_no_errors(&mesh);
	}
	assert!(matches!(
		decode_union(&{
			let mut data = csgmdl_v2_v4(2);
			obfuscate(&mut data);
			data[6] = 9;
			obfuscate(&mut data);
			data
		}),
		Err(MeshError::UnsupportedVersion(_))
	));
}

#[test]
fn union_v5() {
	// Indices exercising every delta form: small positive, negative, and 3-byte jumps.
	let vertex_count = 300;
	let mut indices = vec![0, 1, 2, 0, 2, 3, 299, 100, 5, 5, 200, 1, 64, 0, 127];
	let lod1 = vec![0, 1, 2, 2, 1, 0];
	indices.extend(&lod1);
	let markers = [0, 15, 21];
	let data = csgmdl_v5(vertex_count, &indices, &markers);
	let mesh = decode_union(&data).unwrap();
	assert_eq!(mesh.format, MeshFormat::Union { version: 5 });
	assert_eq!(mesh.vertex_count(), vertex_count);
	assert_eq!(mesh.indices, indices);
	assert_eq!(mesh.lods, vec![0..15, 15..21]);
	assert_eq!(mesh.positions[299][2], 299.0);
	assert!(close(mesh.normals[0][2], 1.0));
	assert!(close(mesh.tangents[0][1], 1.0));
	assert_eq!(mesh.colors[7], [200, 100, 50, 255]);
	assert_eq!(mesh.normal_ids[7], 2);
	assert_no_errors(&mesh);

	// Markers that do not split on triangles, or run past the indices, are rejected.
	assert!(decode_union(&csgmdl_v5(vertex_count, &indices, &[0, 14, 21])).is_err());
	assert!(decode_union(&csgmdl_v5(vertex_count, &indices, &[0, 15, 24])).is_err());
	// Indices beyond the vertices are rejected.
	assert!(decode_union(&csgmdl_v5(10, &indices, &markers)).is_err());
}

#[test]
fn union_solid_mesh() {
	let data = solid_mesh();
	assert_eq!(sniff(&data), Kind::Union);
	let mesh = decode_union(&data).unwrap();
	assert_eq!(mesh.format, MeshFormat::Solid { version: 3 });
	// 36 corners, one vertex per distinct (position, normal, colour): 4 per face
	assert_eq!(mesh.indices.len(), 36);
	assert_eq!(mesh.vertex_count(), 24);
	assert_eq!(mesh.lods, vec![0..36]);
	for triangle in mesh.indices.chunks_exact(3) {
		let [a, b, c] = [triangle[0], triangle[1], triangle[2]].map(|index| mesh.positions[index as usize]);
		let edge = |p: [f32; 3], q: [f32; 3]| [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
		let (u, v) = (edge(a, b), edge(a, c));
		let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
		let normal = mesh.normals[triangle[0] as usize];
		// counter-clockwise from outside, matching the stored normal
		assert!(cross[0] * normal[0] + cross[1] * normal[1] + cross[2] * normal[2] > 0.0);
		let color = mesh.colors[triangle[0] as usize];
		assert_eq!(color, if normal[1] > 0.5 { [60, 60, 200, 255] } else { [200, 60, 60, 255] });
	}
	assert_no_errors(&mesh);
	// a size field that disagrees with the data, and trailing bytes, are rejected
	let mut wrong = data.clone();
	wrong[2] = wrong[2].wrapping_add(1);
	assert!(decode_union(&wrong).is_err());
	let mut longer = data.clone();
	longer.push(0);
	assert!(decode_union(&longer).is_err());
}

#[test]
fn union_csgk_and_models() {
	let key = b"CSGK0123456789abcdef0123456789abcdef";
	match decode_union(key) {
		Err(MeshError::CsgReference(text)) => assert_eq!(text, "0123456789abcdef0123456789abcdef"),
		other => panic!("{other:?}"),
	}

	let data = csgmdl_v5(4, &[0, 1, 2, 0, 2, 3], &[0, 6]);
	for compress in [false, true] {
		let model = rbxm_with_mesh_data(&data, compress);
		assert_eq!(sniff(&model), Kind::Model);
		let mesh = decode_union(&model).unwrap();
		assert_eq!(mesh.format, MeshFormat::Union { version: 5 });
		assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
		assert!(decode_any(&model).is_ok());
	}
	assert!(matches!(decode_union(&rbxm_with_mesh_data(b"", false)), Err(MeshError::NoUnionData)));

	let xml = format!(
		"<roblox version=\"4\"><Item class=\"PartOperationAsset\"><Properties><BinaryString name=\"MeshData\"><![CDATA[{}]]></BinaryString></Properties></Item></roblox>",
		base64(&data)
	);
	let mesh = decode_union(xml.as_bytes()).unwrap();
	assert_eq!(mesh.vertex_count(), 4);
}

#[test]
fn slim_files() {
	let slim = slim(&chunked(7, true));
	assert_eq!(sniff(&slim), Kind::Slim);
	let decoded = decode_slim(&slim).unwrap();
	assert_eq!(decoded.parts.len(), 1);
	let part = &decoded.parts[0];
	assert_eq!(part.bounds, Some(([-1.0; 3], [1.0; 3])));
	let material = part.material.as_ref().unwrap();
	assert_eq!(material.name.as_deref(), Some("Grassy"));
	assert_eq!(material.color, Some([10, 20, 30]));
	assert_eq!(material.textures, vec!["rbxassetid://123".to_string()]);
	assert_eq!(decoded.transforms.len(), 1);
	assert_quad(&part.mesh, true);

	let merged = decode_mesh(&slim).unwrap();
	assert_eq!(merged.format, MeshFormat::Slim { version: 3, draco: true });
	assert_eq!(merged.indices, vec![0, 1, 2, 0, 2, 3]);
	assert_eq!(merged.lods, vec![0..6]);
}

#[test]
fn cache_records() {
	let content = v2(true);
	let record = rbxh(&content);
	let entry = ror_mesh::rbx_storage::parse_rbxh(&record).unwrap();
	assert_eq!(entry.status, 200);
	assert_eq!(entry.content, &content[..]);
	assert!(ror_mesh::rbx_storage::parse_rbxh(&record[..record.len() - 1]).is_none());
	assert!(ror_mesh::rbx_storage::parse_rbxh(b"RBXH").is_none());
}

#[test]
fn normals_are_normalised() {
	let mut data = v2(true);
	// Scale the first normal (bytes 13 + 12 + 12 .. +12) to length 3 and zero the second.
	let normal_at = |vertex: usize| 13 + 12 + vertex * 40 + 12;
	data[normal_at(0) + 8..normal_at(0) + 12].copy_from_slice(&3.0f32.to_le_bytes());
	data[normal_at(1) + 8..normal_at(1) + 12].copy_from_slice(&0.001f32.to_le_bytes());
	let mesh = decode_mesh(&data).unwrap();
	assert_eq!(mesh.normals[0], [0.0, 0.0, 1.0]);
	assert_eq!(mesh.normals[1], [0.0, 0.0, 0.0]);
}

/// All synthetic vectors, for the robustness tests.
fn vectors() -> Vec<Vec<u8>> {
	vec![
		b"version 1.00\n1\n[0,0,0][0,0,1][0,0,0][1,0,0][0,0,1][1,0,0][1,1,0][0,0,1][1,1,0]".to_vec(),
		v2(true),
		v2(false),
		v3(1, &[0, 2, 3]),
		v4_v5("4.01", true, &[0, 2, 3], None),
		v4_v5("5.00", true, &[0, 2, 3], Some(b"facs")),
		chunked(6, false),
		chunked(7, true),
		csgmdl_v2_v4(2),
		csgmdl_v2_v4(4),
		csgmdl_v5(300, &[0, 1, 2, 299, 100, 5], &[0, 6]),
		rbxm_with_mesh_data(&csgmdl_v5(4, &[0, 1, 2], &[0, 3]), true),
		slim(&chunked(7, false)),
		solid_mesh(),
	]
}

#[test]
fn truncation_never_panics() {
	for data in vectors() {
		assert!(decode_any(&data).is_ok());
		for length in 0..data.len() {
			let _ = decode_any(&data[..length]);
		}
	}
}

#[test]
fn corruption_never_panics() {
	// Deterministic pseudo-random byte flips (xorshift), many per vector.
	let mut state = 0x2545_F491_4F6C_DD1Du64;
	let mut next = move || {
		state ^= state << 13;
		state ^= state >> 7;
		state ^= state << 17;
		state
	};
	for data in vectors() {
		for _ in 0..2000 {
			let mut corrupt = data.clone();
			for _ in 0..1 + next() % 4 {
				let at = (next() % corrupt.len() as u64) as usize;
				corrupt[at] = next() as u8;
			}
			if let Ok(mesh) = decode_any(&corrupt) {
				// Whatever decodes must be structurally sound.
				let errors: Vec<_> = validate::check(&mesh)
					.into_iter()
					.filter(|issue| {
						issue.severity == Severity::Error
							&& !matches!(
								issue.check,
								"non-finite"
									| "normal-length" | "skinning"
									| "bone-parent" | "subset-range"
									| "subset-bones"
							)
					})
					.collect();
				assert!(errors.is_empty(), "{errors:?}");
			}
		}
	}
}
