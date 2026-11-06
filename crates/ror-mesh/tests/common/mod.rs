//! Writers for synthetic test vectors (every format is built from scratch here; no Roblox data is
//! committed).
#![allow(dead_code)]

/// Little-endian byte writer.
#[derive(Default)]
pub struct Writer {
	pub bytes: Vec<u8>,
}

impl Writer {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
		self.bytes.extend_from_slice(bytes);
		self
	}

	pub fn u8(&mut self, value: u8) -> &mut Self {
		self.bytes.push(value);
		self
	}

	pub fn u16(&mut self, value: u16) -> &mut Self {
		self.raw(&value.to_le_bytes())
	}

	pub fn u32(&mut self, value: u32) -> &mut Self {
		self.raw(&value.to_le_bytes())
	}

	pub fn u64(&mut self, value: u64) -> &mut Self {
		self.raw(&value.to_le_bytes())
	}

	pub fn f32(&mut self, value: f32) -> &mut Self {
		self.raw(&value.to_le_bytes())
	}

	pub fn vec3(&mut self, value: [f32; 3]) -> &mut Self {
		self.f32(value[0]).f32(value[1]).f32(value[2])
	}

	pub fn sized(&mut self, bytes: &[u8]) -> &mut Self {
		self.u32(bytes.len() as u32).raw(bytes)
	}
}

/// One test vertex.
#[derive(Clone, Copy)]
pub struct Vertex {
	pub position: [f32; 3],
	pub normal: [f32; 3],
	pub uv: [f32; 2],
	/// Raw tangent bytes (`(b - 127) / 127` each).
	pub tangent: [u8; 4],
	pub color: [u8; 4],
}

/// A unit quad in the XY plane facing +Z (two counter-clockwise triangles) plus one extra triangle
/// for a second LOD.
pub fn quad() -> (Vec<Vertex>, Vec<[u32; 3]>) {
	let vertex = |x: f32, y: f32, u: f32, v: f32| Vertex {
		position: [x, y, 0.0],
		normal: [0.0, 0.0, 1.0],
		uv: [u, v],
		// tangent +X (bytes 254, 127, 127), w = +1 (254)
		tangent: [254, 127, 127, 254],
		color: [255, 128, 64, 255],
	};
	let vertices = vec![
		vertex(0.0, 0.0, 0.0, 1.0),
		vertex(1.0, 0.0, 1.0, 1.0),
		vertex(1.0, 1.0, 1.0, 0.0),
		vertex(0.0, 1.0, 0.0, 0.0),
	];
	// LOD0: two triangles; LOD1: one triangle covering the quad diagonal.
	let faces = vec![[0, 1, 2], [0, 2, 3], [0, 1, 2]];
	(vertices, faces)
}

pub fn write_vertex(w: &mut Writer, vertex: &Vertex, with_color: bool) {
	w.vec3(vertex.position).vec3(vertex.normal).f32(vertex.uv[0]).f32(vertex.uv[1]).raw(&vertex.tangent);
	if with_color {
		w.raw(&vertex.color);
	}
}

pub fn write_faces(w: &mut Writer, faces: &[[u32; 3]]) {
	for face in faces {
		w.u32(face[0]).u32(face[1]).u32(face[2]);
	}
}

pub fn v2(with_color: bool) -> Vec<u8> {
	let (vertices, faces) = quad();
	let mut w = Writer::new();
	w.raw(b"version 2.00\n")
		.u16(12)
		.u8(if with_color { 40 } else { 36 })
		.u8(12)
		.u32(vertices.len() as u32)
		.u32(faces.len() as u32);
	for vertex in &vertices {
		write_vertex(&mut w, vertex, with_color);
	}
	write_faces(&mut w, &faces);
	w.bytes
}

pub fn v3(minor: u8, lods: &[u32]) -> Vec<u8> {
	let (vertices, faces) = quad();
	let mut w = Writer::new();
	w.raw(format!("version 3.0{minor}\n").as_bytes())
		.u16(16)
		.u8(40)
		.u8(12)
		.u16(4)
		.u16(lods.len() as u16)
		.u32(vertices.len() as u32)
		.u32(faces.len() as u32);
	for vertex in &vertices {
		write_vertex(&mut w, vertex, true);
	}
	write_faces(&mut w, &faces);
	for &lod in lods {
		w.u32(lod);
	}
	w.bytes
}

/// Skinning sections shared by v4/v5/chunked writers: two bones ("Root", "Child"), one subset over
/// all four vertices whose bone table is `[1, 0]` (so local slot 0 is global bone 1).
pub struct Skin {
	pub envelopes: Vec<[u8; 8]>,
	pub bones: Vec<u8>,
	pub names: Vec<u8>,
	pub subsets: Vec<u8>,
}

pub fn skin() -> Skin {
	// vertex 0: slot 0 (bone 1) weight 255; vertex 1: slot 0/1 split 128/127; vertex 2: no weights;
	// vertex 3: slot 1 (bone 0) weight 51 + slot 0 weight 204.
	let envelopes = vec![
		[0, 0, 0, 0, 255, 0, 0, 0],
		[0, 1, 0, 0, 128, 127, 0, 0],
		[0, 0, 0, 0, 0, 0, 0, 0],
		[1, 0, 0, 0, 51, 204, 0, 0],
	];
	let names = b"Root\0Child\0".to_vec();
	let mut bones = Writer::new();
	for (name_offset, parent, position) in [(0u32, 0xFFFFu16, [0.0, 0.0, 0.0]), (5, 0, [0.0, 1.0, 0.0])] {
		bones.u32(name_offset).u16(parent).u16(0xFFFF).f32(2.5);
		// rotation rows: a 90 degree turn about Z (columns: right = +Y, up = -X, back = +Z)
		bones.vec3([0.0, -1.0, 0.0]).vec3([1.0, 0.0, 0.0]).vec3([0.0, 0.0, 1.0]);
		bones.vec3(position);
	}
	let mut subsets = Writer::new();
	subsets.u32(0).u32(2).u32(0).u32(4).u32(2);
	let mut table = [0xFFFFu16; 26];
	table[0] = 1;
	table[1] = 0;
	for entry in table {
		subsets.u16(entry);
	}
	Skin { envelopes, bones: bones.bytes, names, subsets: subsets.bytes }
}

/// Version 4.0x / 5.00. `facs` is only written for 5.00.
pub fn v4_v5(version: &str, skinned: bool, lods: &[u32], facs: Option<&[u8]>) -> Vec<u8> {
	let (vertices, faces) = quad();
	let skin = skin();
	let five = version == "5.00";
	let mut w = Writer::new();
	w.raw(format!("version {version}\n").as_bytes())
		.u16(if five { 32 } else { 24 })
		.u16(3)
		.u32(vertices.len() as u32)
		.u32(faces.len() as u32)
		.u16(lods.len() as u16)
		.u16(if skinned { 2 } else { 0 })
		.u32(if skinned { skin.names.len() as u32 } else { 0 })
		.u16(if skinned { 1 } else { 0 })
		.u8(2)
		.u8(0x3F);
	if five {
		w.u32(facs.map_or(0, |_| 1)).u32(facs.map_or(0, |facs| facs.len() as u32));
	}
	for vertex in &vertices {
		write_vertex(&mut w, vertex, true);
	}
	if skinned {
		for envelope in &skin.envelopes {
			w.raw(envelope);
		}
	}
	write_faces(&mut w, &faces);
	for &lod in lods {
		w.u32(lod);
	}
	if skinned {
		w.raw(&skin.bones).raw(&skin.names).raw(&skin.subsets);
	}
	if five && let Some(facs) = facs {
		w.raw(facs);
	}
	w.bytes
}

pub fn chunk(w: &mut Writer, name: &str, version: u32, data: &[u8]) {
	let mut padded = [0u8; 8];
	padded[..name.len()].copy_from_slice(name.as_bytes());
	w.raw(&padded).u32(version).sized(data);
}

pub fn coremesh_v1() -> Vec<u8> {
	let (vertices, faces) = quad();
	let mut core = Writer::new();
	core.u32(vertices.len() as u32);
	for vertex in &vertices {
		write_vertex(&mut core, vertex, true);
	}
	core.u32(faces.len() as u32);
	write_faces(&mut core, &faces);
	core.bytes
}

pub fn lods_chunk(lods: &[u32]) -> Vec<u8> {
	let mut w = Writer::new();
	w.u16(3).u8(1).u32(lods.len() as u32);
	for &lod in lods {
		w.u32(lod);
	}
	w.bytes
}

pub fn skinning_chunk() -> Vec<u8> {
	let skin = skin();
	let mut w = Writer::new();
	w.u32(skin.envelopes.len() as u32);
	for envelope in &skin.envelopes {
		w.raw(envelope);
	}
	w.u32(2).raw(&skin.bones).sized(&skin.names).u32(1).raw(&skin.subsets);
	w.bytes
}

/// A Draco COREMESH v2 payload (`u32 size` + bitstream) for the quad, laid out like Roblox's:
/// POSITION, GENERIC f32x3 normals, TEX_COORD, GENERIC u8x4 tangents, COLOR; sequential encoding.
pub fn coremesh_v2() -> Vec<u8> {
	use draco_core::{
		DataType, EncoderBuffer, EncoderOptions, FaceIndex, GeometryAttributeType, MeshEncoder, PointAttribute,
		PointIndex,
	};

	let (vertices, faces) = quad();
	let count = vertices.len();
	let mut mesh = draco_core::Mesh::new();
	let mut add = |kind: GeometryAttributeType, data_type: DataType, components: u8, bytes: Vec<u8>| {
		let mut attribute = PointAttribute::new();
		attribute.init(kind, components, data_type, false, count);
		attribute.buffer_mut().write(0, &bytes);
		mesh.add_attribute(attribute);
	};
	let floats = |values: Vec<f32>| values.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<u8>>();
	add(
		GeometryAttributeType::Position,
		DataType::Float32,
		3,
		floats(vertices.iter().flat_map(|v| v.position).collect()),
	);
	add(GeometryAttributeType::Generic, DataType::Float32, 3, floats(vertices.iter().flat_map(|v| v.normal).collect()));
	add(GeometryAttributeType::TexCoord, DataType::Float32, 2, floats(vertices.iter().flat_map(|v| v.uv).collect()));
	add(GeometryAttributeType::Generic, DataType::Uint8, 4, vertices.iter().flat_map(|v| v.tangent).collect());
	add(GeometryAttributeType::Color, DataType::Uint8, 4, vertices.iter().flat_map(|v| v.color).collect());
	mesh.set_num_faces(faces.len());
	for (index, face) in faces.iter().enumerate() {
		mesh.set_face(FaceIndex(index as u32), [PointIndex(face[0]), PointIndex(face[1]), PointIndex(face[2])]);
	}
	let mut encoder = MeshEncoder::new();
	encoder.set_mesh(mesh);
	let mut options = EncoderOptions::new();
	options.set_encoding_method(0);
	options.set_global_int("encoding_speed", 10);
	options.set_global_int("decoding_speed", 10);
	let mut out = EncoderBuffer::new();
	encoder.encode(&options, &mut out).expect("draco encode");
	let mut w = Writer::new();
	w.sized(out.data());
	w.bytes
}

pub fn chunked(major: u8, draco: bool) -> Vec<u8> {
	let mut w = Writer::new();
	w.raw(format!("version {major}.00\n").as_bytes());
	if draco {
		chunk(&mut w, "COREMESH", 2, &coremesh_v2());
	} else {
		chunk(&mut w, "COREMESH", 1, &coremesh_v1());
	}
	chunk(&mut w, "LODS", 1, &lods_chunk(&[0, 2, 3]));
	chunk(&mut w, "SKINNING", 1, &skinning_chunk());
	let mut facs = Writer::new();
	facs.sized(b"FACSDATA");
	chunk(&mut w, "FACS", 1, &facs.bytes);
	let mut hsr = Writer::new();
	hsr.u32(3).u8(0b101);
	chunk(&mut w, "HSRAVIS", 1, &hsr.bytes);
	chunk(&mut w, "FUTURE", 9, b"ignored");
	w.bytes
}

/// The XOR key Roblox applies to CSGMDL data.
pub const CSG_KEY: [u8; 31] = [
	86, 46, 110, 88, 49, 32, 48, 4, 52, 105, 12, 119, 12, 1, 94, 0, 26, 96, 55, 105, 29, 82, 43, 7, 79, 36, 89, 101,
	83, 4, 122,
];

pub fn obfuscate(bytes: &mut [u8]) {
	for (offset, byte) in bytes.iter_mut().enumerate() {
		*byte ^= CSG_KEY[offset % CSG_KEY.len()];
	}
}

/// CSGMDL v2 or v4 (obfuscated): the quad's LOD0 with union attributes.
pub fn csgmdl_v2_v4(version: u32) -> Vec<u8> {
	let (vertices, faces) = quad();
	let mut w = Writer::new();
	w.raw(b"CSGMDL").u32(version).raw(&[0xAB; 32]).u32(vertices.len() as u32).u32(84);
	for (index, vertex) in vertices.iter().enumerate() {
		w.vec3(vertex.position)
			.vec3(vertex.normal)
			.raw(&[10, 20, 30, 255])
			.u32(index as u32 % 6 + 1)
			.f32(vertex.uv[0] * 4.0)
			.f32(vertex.uv[1] * 4.0);
		w.raw(&[0; 16]).vec3([1.0, 0.0, 0.0]).raw(&[0; 16]);
	}
	w.u32(6);
	write_faces(&mut w, &faces[..2]);
	if version == 4 {
		w.u32(2).u32(7).u32(9);
	}
	obfuscate(&mut w.bytes);
	w.bytes
}

/// Encodes `indices` with the CSGMDL v5 delta scheme (using every delta form).
pub fn csg_delta(indices: &[u32]) -> Vec<u8> {
	let mut out = Vec::new();
	let mut previous: u32 = 0;
	for &index in indices {
		let delta = (index.wrapping_sub(previous)) & 0x7F_FFFF;
		let signed = if delta & 0x40_0000 != 0 { delta as i64 - 0x80_0000 } else { delta as i64 };
		if (0..64).contains(&signed) {
			out.push(signed as u8);
		} else if (-64..0).contains(&signed) {
			out.push((signed + 128) as u8);
		} else {
			out.extend([0x80 | (delta >> 16) as u8, (delta >> 8) as u8, delta as u8]);
		}
		previous = index;
	}
	out
}

fn quantize(value: f32) -> u16 {
	(value * 32767.0 + 32767.0).round() as u16
}

/// CSGMDL v5 (header obfuscated only). `indices` reference `vertex_count` copies of the quad's
/// vertices (cycled); `markers` are written as given.
pub fn csgmdl_v5(vertex_count: usize, indices: &[u32], markers: &[u32]) -> Vec<u8> {
	let (vertices, _) = quad();
	let vertex = |index: usize| vertices[index % vertices.len()];
	let mut w = Writer::new();
	let mut header = Writer::new();
	header.raw(b"CSGMDL").u32(5);
	let mut header = header.bytes;
	obfuscate(&mut header);
	w.raw(&header);
	w.u16(vertex_count as u16);
	for index in 0..vertex_count {
		let mut position = vertex(index).position;
		position[2] = index as f32;
		w.vec3(position);
	}
	w.u16(vertex_count as u16).u32(vertex_count as u32 * 6);
	for index in 0..vertex_count {
		let normal = vertex(index).normal;
		w.u16(quantize(normal[0])).u16(quantize(normal[1])).u16(quantize(normal[2]));
	}
	w.u16(vertex_count as u16);
	for _ in 0..vertex_count {
		w.raw(&[200, 100, 50, 255]);
	}
	w.u16(vertex_count as u16);
	for index in 0..vertex_count {
		w.u8((index % 6) as u8 + 1);
	}
	w.u16(vertex_count as u16);
	for index in 0..vertex_count {
		w.f32(vertex(index).uv[0]).f32(vertex(index).uv[1]);
	}
	w.u16(vertex_count as u16).u32(vertex_count as u32 * 6);
	for _ in 0..vertex_count {
		w.u16(quantize(0.0)).u16(quantize(1.0)).u16(quantize(0.0));
	}
	w.u32(indices.len() as u32).sized(&csg_delta(indices));
	w.u8(markers.len() as u8);
	for &marker in markers {
		w.u32(marker);
	}
	w.bytes
}

/// An unpublished union's `SolidMeshHolder` data (notes/mesh.md "SolidMesh"): a 2 x 2 x 2 box with
/// one normal per face, its top blue and the rest red, the two colours shared by index.
pub fn solid_mesh() -> Vec<u8> {
	let corner = |index: u32| [if index & 1 == 0 { -1.0 } else { 1.0 }, if index & 2 == 0 { -1.0 } else { 1.0 }, if index & 4 == 0 { -1.0 } else { 1.0 }];
	// faces as (normal, corners counter-clockwise from outside)
	let faces: [([f32; 3], [u32; 4]); 6] = [
		([1.0, 0.0, 0.0], [1, 3, 7, 5]),
		([-1.0, 0.0, 0.0], [0, 4, 6, 2]),
		([0.0, 1.0, 0.0], [2, 6, 7, 3]),
		([0.0, -1.0, 0.0], [0, 1, 5, 4]),
		([0.0, 0.0, 1.0], [4, 5, 7, 6]),
		([0.0, 0.0, -1.0], [0, 2, 3, 1]),
	];
	let (mut positions, mut normals, mut colors) = (Vec::new(), Vec::new(), Vec::new());
	for (face, (_, quad)) in faces.iter().enumerate() {
		for [a, b, c] in [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]] {
			positions.extend([a, b, c]);
			normals.extend([face as u32; 3]);
			colors.extend([if face == 2 { 1 } else { 0 }; 3]);
		}
	}
	let mut body = Writer::new();
	body.raw(b"SolidMesh").u32(0);
	body.u64(8);
	for index in 0..8 {
		body.vec3(corner(index));
	}
	body.u64(faces.len() as u64);
	for (normal, _) in &faces {
		body.vec3(*normal);
	}
	body.u64(0);
	body.u64(2).vec3([200.0 / 255.0, 60.0 / 255.0, 60.0 / 255.0]).vec3([60.0 / 255.0, 60.0 / 255.0, 200.0 / 255.0]);
	for stream in [&positions, &normals, &Vec::new(), &colors] {
		body.u64(stream.len() as u64).raw(&csg_delta(stream));
	}
	let mut w = Writer::new();
	let size = body.bytes.len() as u32;
	w.u8(3).u8(1).u32(size).u32(0).u32(size).raw(&body.bytes);
	w.bytes
}

/// A binary model holding one `PartOperationAsset` with `MeshData` = `mesh_data`; the `PROP`
/// chunk is LZ4-compressed when `compress` is set.
pub fn rbxm_with_mesh_data(mesh_data: &[u8], compress: bool) -> Vec<u8> {
	let mut w = Writer::new();
	w.raw(b"<roblox!").raw(&[0x89, 0xFF, 0x0D, 0x0A, 0x1A, 0x0A]).u16(0).u32(1).u32(1).raw(&[0; 8]);
	let mut inst = Writer::new();
	inst.u32(0).sized(b"PartOperationAsset").u8(0).u32(1).u32(0);
	model_chunk(&mut w, b"INST", &inst.bytes, false);
	let mut prop = Writer::new();
	prop.u32(0).sized(b"Name").u8(1).sized(b"Instance");
	model_chunk(&mut w, b"PROP", &prop.bytes, compress);
	let mut prop = Writer::new();
	prop.u32(0).sized(b"MeshData").u8(1).sized(mesh_data);
	model_chunk(&mut w, b"PROP", &prop.bytes, compress);
	model_chunk(&mut w, b"PRNT", &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], false);
	model_chunk(&mut w, b"END\0", b"</roblox>", false);
	w.bytes
}

fn model_chunk(w: &mut Writer, name: &[u8; 4], data: &[u8], compress: bool) {
	w.raw(name);
	if compress {
		let compressed = lz4_compress(data);
		w.u32(compressed.len() as u32).u32(data.len() as u32).u32(0).raw(&compressed);
	} else {
		w.u32(0).u32(data.len() as u32).u32(0).raw(data);
	}
}

/// A valid (if uncompressed) LZ4 block: one sequence of literals only.
fn lz4_compress(data: &[u8]) -> Vec<u8> {
	let mut out = Vec::new();
	let literals = data.len();
	if literals >= 15 {
		out.push(0xF0);
		let mut rest = literals - 15;
		while rest >= 255 {
			out.push(255);
			rest -= 255;
		}
		out.push(rest as u8);
	} else {
		out.push((literals as u8) << 4);
	}
	out.extend_from_slice(data);
	out
}

pub fn base64(bytes: &[u8]) -> String {
	const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut out = String::new();
	for chunk in bytes.chunks(3) {
		let value =
			(chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
		for index in 0..4 {
			if index <= chunk.len() {
				out.push(TABLE[(value >> (18 - index * 6) & 63) as usize] as char);
			} else {
				out.push('=');
			}
		}
	}
	out
}

/// A SLIM file with one (mesh, material) pair and a root record holding an identity transform.
pub fn slim(mesh: &[u8]) -> Vec<u8> {
	let mut w = Writer::new();
	// mesh record: class 2, props 17 (AABB, column 2 element 0), 15 (mesh, column 4 item 0),
	// 24 (hash, column 4 item 1)
	let mut payload = Writer::new();
	payload.u32(1).u16(2);
	array_column(&mut payload, 2, &[&[17u16, 15, 24].iter().flat_map(|id| id.to_le_bytes()).collect::<Vec<u8>>()]);
	payload.u32(3).u32(0).u32(0).u32(1);
	payload.u32(0); // column 0
	payload.u32(0); // column 1
	payload.u32(6).vec3([-1.0, -1.0, -1.0]).vec3([1.0, 1.0, 1.0]); // column 2
	payload.u32(0); // column 3
	array_column(&mut payload, 1, &[mesh, b"0123456789abcdef0123456789abcdef"]);
	payload.u32(0).u32(0); // columns 5, 6
	array_column(&mut payload, 4, &[]);
	array_column(&mut payload, 8, &[]);
	slim_record(&mut w, b"SLIM", 3, &payload.bytes);

	// material record: class 3, props 7 (name, column 4 item 0), 18 (colour, column 0 elements 0..3),
	// 3 (texture, column 4 item 1)
	let mut payload = Writer::new();
	payload.u32(1).u16(3);
	array_column(&mut payload, 2, &[&[7u16, 18, 3].iter().flat_map(|id| id.to_le_bytes()).collect::<Vec<u8>>()]);
	payload.u32(3).u32(0).u32(0).u32(1);
	payload.u32(3).raw(&[10, 20, 30]); // column 0
	payload.u32(0).u32(0).u32(0); // columns 1, 2, 3
	array_column(&mut payload, 1, &[b"Grassy", b"rbxassetid://123"]);
	payload.u32(0).u32(0);
	array_column(&mut payload, 4, &[]);
	array_column(&mut payload, 8, &[]);
	slim_record(&mut w, b"SLIM", 3, &payload.bytes);

	// root record: class 4, prop 25 (transforms, column 7 item 0)
	let mut payload = Writer::new();
	payload.u32(1).u16(4);
	array_column(&mut payload, 2, &[&25u16.to_le_bytes()]);
	payload.u32(1).u32(0);
	payload.u32(0).u32(0).u32(0).u32(0);
	array_column(&mut payload, 1, &[]);
	payload.u32(0).u32(0);
	let identity: Vec<u8> = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]
		.iter()
		.flat_map(|value| value.to_le_bytes())
		.collect();
	array_column(&mut payload, 4, &[&identity]);
	array_column(&mut payload, 8, &[]);
	slim_record(&mut w, b"SLIM", 3, &payload.bytes);
	w.bytes
}

fn slim_record(w: &mut Writer, magic: &[u8; 4], version: u32, payload: &[u8]) {
	w.raw(magic).u32(payload.len() as u32).u32(version).u32(0).raw(payload);
}

/// `u32 count`, `u32 end[count]` (in units), `u32 total units`, data.
fn array_column(w: &mut Writer, unit: usize, items: &[&[u8]]) {
	w.u32(items.len() as u32);
	let mut end = 0;
	for item in items {
		end += item.len() / unit;
		w.u32(end as u32);
	}
	w.u32(end as u32);
	for item in items {
		w.raw(item);
	}
}

/// Wraps content in an `RBXH` cache record.
pub fn rbxh(content: &[u8]) -> Vec<u8> {
	let mut w = Writer::new();
	w.raw(b"RBXH")
		.u32(2)
		.sized(b"")
		.u8(0)
		.u32(200)
		.u32(0)
		.u32(0)
		.u32(content.len() as u32)
		.u32(0x1234)
		.u32(0)
		.raw(content);
	w.bytes
}
