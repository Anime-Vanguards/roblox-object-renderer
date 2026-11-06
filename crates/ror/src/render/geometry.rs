//! Vertex data: the GPU vertex layout, Roblox's primitive part shapes in unit space, and the
//! conversion of decoded mesh assets.
//!
//! Primitive vertices carry material texture coordinates: `uv` spans each flat face from its
//! top-left corner as seen from outside (0..1 along the face's [`MATERIAL_FACES`] edges) and
//! `color.a` names the face, so the shader can turn them into studs for any part size
//! (notes/materials.md "Mapping").

use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Vertex {
	pub position: [f32; 3],
	pub normal: [f32; 3],
	pub uv: [f32; 2],
	/// xyz tangent, w handedness
	pub tangent: [f32; 4],
	pub color: [u8; 4],
}

impl Vertex {
	pub const ATTRIBUTES: [wgpu::VertexAttribute; 5] =
		wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4, 4 => Unorm8x4];

	pub fn layout() -> wgpu::VertexBufferLayout<'static> {
		wgpu::VertexBufferLayout {
			array_stride: std::mem::size_of::<Vertex>() as u64,
			step_mode: wgpu::VertexStepMode::Vertex,
			attributes: &Self::ATTRIBUTES,
		}
	}

	/// A posed skinned mesh's bind pose (`GpuMesh::bind`), its position and normal beside the posed
	/// vertex (mesh.wgsl `vs_decal_bind`).
	pub const BIND_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![5 => Float32x3, 6 => Float32x3];

	pub fn bind_layout() -> wgpu::VertexBufferLayout<'static> {
		wgpu::VertexBufferLayout {
			array_stride: std::mem::size_of::<Vertex>() as u64,
			step_mode: wgpu::VertexStepMode::Vertex,
			attributes: &Self::BIND_ATTRIBUTES,
		}
	}
}

/// CPU-side geometry before upload.
#[derive(Clone, Debug, Default)]
pub struct CpuMesh {
	pub vertices: Vec<Vertex>,
	pub indices: Vec<u32>,
	/// Index ranges per level of detail (`lods[0]` highest); one range for primitives.
	pub lods: Vec<std::ops::Range<u32>>,
	/// Bounds of the vertices the highest LOD uses.
	pub bounds: (Vec3, Vec3),
	/// The file's count of high-quality levels (its header's; 0 where the format has none): the levels
	/// RenderFidelity Automatic takes (crate::lod)
	pub high_quality: u8,
}

impl CpuMesh {
	fn finish(mut self) -> Self {
		if self.lods.is_empty() {
			self.lods.push(0..self.indices.len() as u32);
		}
		self.bounds = bounds_of(&self.vertices, &self.indices[self.lods[0].start as usize..self.lods[0].end as usize]);
		self
	}
}

fn bounds_of(vertices: &[Vertex], indices: &[u32]) -> (Vec3, Vec3) {
	let mut min = Vec3::splat(f32::INFINITY);
	let mut max = Vec3::splat(f32::NEG_INFINITY);
	for &index in indices {
		if let Some(vertex) = vertices.get(index as usize) {
			let position = Vec3::from(vertex.position);
			min = min.min(position);
			max = max.max(position);
		}
	}
	if min.x > max.x {
		return (Vec3::ZERO, Vec3::ZERO);
	}
	(min, max)
}

struct Builder {
	mesh: CpuMesh,
	/// A point strictly inside the (convex) shape; faces are turned away from it.
	interior: Vec3,
}

impl Builder {
	fn new() -> Self {
		Self { mesh: CpuMesh::default(), interior: Vec3::ZERO }
	}

	fn with_interior(interior: Vec3) -> Self {
		Self { mesh: CpuMesh::default(), interior }
	}

	fn vertex(&mut self, position: Vec3, normal: Vec3, uv: Vec2) -> u32 {
		self.mesh.vertices.push(Vertex {
			position: position.into(),
			normal: normal.normalize_or_zero().into(),
			uv: uv.into(),
			tangent: [0.0; 4],
			color: [255; 4],
		});
		self.mesh.vertices.len() as u32 - 1
	}

	fn triangle(&mut self, a: u32, b: u32, c: u32) {
		self.mesh.indices.extend_from_slice(&[a, b, c]);
	}

	/// A flat quad of a convex shape: corners top-left, bottom-left,
	/// bottom-right, top-right (uv (0,0), (0,1), (1,1), (1,0)). Faces are turned outwards.
	fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
		let mut normal = (b - a).cross(c - a).normalize_or_zero();
		let outward = normal.dot((a + b + c + d) * 0.25 - self.interior) >= 0.0;
		if !outward {
			normal = -normal;
		}
		let ia = self.vertex(a, normal, Vec2::new(0.0, 0.0));
		let ib = self.vertex(b, normal, Vec2::new(0.0, 1.0));
		let ic = self.vertex(c, normal, Vec2::new(1.0, 1.0));
		let id = self.vertex(d, normal, Vec2::new(1.0, 0.0));
		if outward {
			self.triangle(ia, ib, ic);
			self.triangle(ia, ic, id);
		} else {
			self.triangle(ia, ic, ib);
			self.triangle(ia, id, ic);
		}
	}

	/// A flat triangle of a convex shape, turned outwards.
	fn flat_triangle(&mut self, a: Vec3, b: Vec3, c: Vec3) {
		let mut normal = (b - a).cross(c - a).normalize_or_zero();
		let outward = normal.dot((a + b + c) / 3.0 - self.interior) >= 0.0;
		if !outward {
			normal = -normal;
		}
		let ia = self.vertex(a, normal, Vec2::new(0.0, 0.0));
		let ib = self.vertex(b, normal, Vec2::new(0.0, 1.0));
		let ic = self.vertex(c, normal, Vec2::new(1.0, 1.0));
		if outward {
			self.triangle(ia, ib, ic);
		} else {
			self.triangle(ia, ic, ib);
		}
	}

	/// Material coordinates for the flat face made of the vertices from `first` on.
	fn material_face(&mut self, first: usize, code: u8) {
		let positions: Vec<Vec3> = self.mesh.vertices[first..].iter().map(|vertex| Vec3::from(vertex.position)).collect();
		material_face(&mut self.mesh.vertices[first..], &positions, code);
	}

	fn finish(mut self) -> CpuMesh {
		generate_tangents(&mut self.mesh.vertices, &self.mesh.indices);
		self.mesh.finish()
	}
}

/// Material texture faces of the primitives: the unit-space edges along which u and v grow across
/// the whole face (u to the right, v down, as seen from outside; up is world +Y projected onto the
/// face, +Z on the top face and -Z on the bottom). Indexed by the code in a vertex's `color.a`;
/// mirrored by `MATERIAL_FACE_U` / `MATERIAL_FACE_V` in shaders/common.wgsl.
pub const MATERIAL_FACES: [[[f32; 3]; 2]; 9] = [
	// box faces in Enum.NormalId order: Right, Top, Back, Left, Bottom, Front
	[[0.0, 0.0, -1.0], [0.0, -1.0, 0.0]],
	[[-1.0, 0.0, 0.0], [0.0, 0.0, -1.0]],
	[[1.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
	[[0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
	[[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
	[[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
	// the wedge's slope (facing up and to the front)
	[[-1.0, 0.0, 0.0], [0.0, -1.0, -1.0]],
	// the corner wedge's slopes: facing up and back, facing up and left
	[[1.0, 0.0, 0.0], [0.0, -1.0, 1.0]],
	[[0.0, 0.0, 1.0], [-1.0, -1.0, 0.0]],
];

/// Gives the vertices `from..` (one flat face) material coordinates on face `code`: 0..1 along the
/// face's edges from its top-left corner, measured on `positions` (the vertices' own positions, or
/// the pre-push box positions of a cylinder's side).
fn material_face(vertices: &mut [Vertex], positions: &[Vec3], code: u8) {
	let [u, v] = MATERIAL_FACES[code as usize].map(Vec3::from);
	let (u_axis, v_axis) = (u.normalize(), v.normalize());
	let u_min = positions.iter().map(|p| p.dot(u_axis)).fold(f32::INFINITY, f32::min);
	let v_min = positions.iter().map(|p| p.dot(v_axis)).fold(f32::INFINITY, f32::min);
	for (vertex, position) in vertices.iter_mut().zip(positions) {
		vertex.uv = [(position.dot(u_axis) - u_min) / u.length(), (position.dot(v_axis) - v_min) / v.length()];
		vertex.color[3] = code;
	}
}

/// The eight corners of the unit box, `corner(x, y, z)` with each sign ±1.
fn corner(x: f32, y: f32, z: f32) -> Vec3 {
	Vec3::new(x, y, z) * 0.5
}

/// Per `Enum.NormalId` (Right, Top, Back, Left, Bottom, Front): the face normal, and the directions
/// of a decal's +u (right) and +v (down) on it as seen from outside. Shared with the decal shader.
pub const FACE_BASIS: [[[f32; 3]; 3]; 6] = [
	[[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]],
	[[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
	[[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
	[[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]],
	[[0.0, -1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
	[[0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
];

/// A unit cube centred on the origin (Part / Block), with material coordinates per face.
pub fn block() -> CpuMesh {
	let mut builder = Builder::new();
	for (code, [normal, right, down]) in FACE_BASIS.into_iter().enumerate() {
		let (normal, right, down) = (Vec3::from(normal), Vec3::from(right), Vec3::from(down));
		let first = builder.mesh.vertices.len();
		builder.quad(
			(normal - right - down) * 0.5,
			(normal - right + down) * 0.5,
			(normal + right + down) * 0.5,
			(normal + right - down) * 0.5,
		);
		builder.material_face(first, code as u8);
	}
	builder.finish()
}

/// A unit wedge: full bottom and back faces, the slope rising from the bottom-front edge to the
/// top-back edge (WedgePart, Part with Shape = Wedge).
pub fn wedge() -> CpuMesh {
	// the slope plane (y = z) passes through the origin; this point is below it
	let mut builder = Builder::with_interior(Vec3::new(0.0, -0.25, 0.25));
	let bfl = corner(-1.0, -1.0, -1.0);
	let bfr = corner(1.0, -1.0, -1.0);
	let bbl = corner(-1.0, -1.0, 1.0);
	let bbr = corner(1.0, -1.0, 1.0);
	let tbl = corner(-1.0, 1.0, 1.0);
	let tbr = corner(1.0, 1.0, 1.0);
	let faces: [(u8, &[Vec3]); 5] = [
		// bottom, back, slope, right and left sides
		(4, &[bbl, bfl, bfr, bbr]),
		(2, &[tbr, bbr, bbl, tbl]),
		(6, &[tbl, bfl, bfr, tbr]),
		(0, &[tbr, bfr, bbr]),
		(3, &[tbl, bbl, bfl]),
	];
	for (code, corners) in faces {
		let first = builder.mesh.vertices.len();
		match corners {
			[a, b, c, d] => builder.quad(*a, *b, *c, *d),
			[a, b, c] => builder.flat_triangle(*a, *b, *c),
			_ => unreachable!(),
		}
		builder.material_face(first, code);
	}
	builder.finish()
}

/// A unit corner wedge: a square bottom and the apex above the front-right corner.
pub fn corner_wedge() -> CpuMesh {
	// the centroid of the five vertices
	let mut builder = Builder::with_interior(Vec3::new(0.1, -0.3, -0.1));
	let bfl = corner(-1.0, -1.0, -1.0);
	let bfr = corner(1.0, -1.0, -1.0);
	let bbl = corner(-1.0, -1.0, 1.0);
	let bbr = corner(1.0, -1.0, 1.0);
	let apex = corner(1.0, 1.0, -1.0);
	let first = builder.mesh.vertices.len();
	builder.quad(bbl, bfl, bfr, bbr);
	builder.material_face(first, 4);
	// right (+X) and front (-Z) vertical triangles, then the two slopes (up-back, up-left)
	for (code, [a, b, c]) in [(0, [apex, bfr, bbr]), (5, [apex, bfl, bfr]), (7, [apex, bbr, bbl]), (8, [apex, bbl, bfl])] {
		let first = builder.mesh.vertices.len();
		builder.flat_triangle(a, b, c);
		builder.material_face(first, code);
	}
	builder.finish()
}

/// Roblox's Ball: a cube whose faces are `subdivisions` x `subdivisions` grids, every vertex pushed
/// onto the unit-diameter sphere (Studio's silhouettes have their corners at the directions of
/// (3, k), k = -3..3: 6 subdivisions; notes/render.md "Part geometry"). Material coordinates are
/// projected per pixel (the shader's ball mapping); the vertices carry their cube face's for the
/// tangents.
pub fn sphere(subdivisions: u32) -> CpuMesh {
	let mut builder = Builder::new();
	let n = subdivisions;
	for (code, [normal, right, down]) in FACE_BASIS.into_iter().enumerate() {
		let (normal, right, down) = (Vec3::from(normal), Vec3::from(right), Vec3::from(down));
		let first = builder.mesh.vertices.len() as u32;
		let mut cube = Vec::new();
		for j in 0..=n {
			for i in 0..=n {
				let u = i as f32 / n as f32;
				let v = j as f32 / n as f32;
				let point = normal + right * (u * 2.0 - 1.0) + down * (v * 2.0 - 1.0);
				let direction = point.normalize();
				builder.vertex(direction * 0.5, direction, Vec2::new(u, v));
				cube.push(point * 0.5);
			}
		}
		material_face(&mut builder.mesh.vertices[first as usize..], &cube, code as u8);
		let stride = n + 1;
		for j in 0..n {
			for i in 0..n {
				let a = first + j * stride + i;
				// top-left, bottom-left, bottom-right / top-left, bottom-right, top-right (outward, as the box)
				builder.triangle(a, a + stride, a + stride + 1);
				builder.triangle(a, a + stride + 1, a + 1);
			}
		}
	}
	builder.finish()
}

/// The ring of Roblox's cylinders: a square with `per_side` segments per side, pushed onto the
/// unit circle (24 points for 6; the same corner directions as the Ball's silhouette).
fn circle_ring(per_side: u32) -> Vec<Vec2> {
	let mut ring = Vec::new();
	let corners = [Vec2::new(1.0, -1.0), Vec2::new(1.0, 1.0), Vec2::new(-1.0, 1.0), Vec2::new(-1.0, -1.0)];
	for side in 0..4 {
		let (from, to) = (corners[side], corners[(side + 1) % 4]);
		for step in 0..per_side {
			ring.push(from.lerp(to, step as f32 / per_side as f32).normalize());
		}
	}
	ring
}

/// A unit cylinder along X (diameter 1, length 1), as Roblox orients Cylinder parts, with Studio's
/// 24-point ring. Each side of the pushed square keeps its own vertices, whose material coordinates
/// are those of the square (the cylinder's box) before the push (material-mapping: 0.9475 of the
/// box scale across the middle facet, as linear interpolation over the facets gives).
pub fn cylinder(per_side: u32) -> CpuMesh {
	let mut builder = Builder::new();
	let ring = circle_ring(per_side);
	let count = ring.len() as u32;
	let direction = |index: u32| {
		let point = ring[(index % count) as usize];
		Vec3::new(0.0, point.x, point.y)
	};
	// the square sides in ring order: +Y (top), +Z (back), -Y (bottom), -Z (front)
	let corners = [Vec2::new(1.0, -1.0), Vec2::new(1.0, 1.0), Vec2::new(-1.0, 1.0), Vec2::new(-1.0, -1.0)];
	for (side, code) in [(0u32, 1u8), (1, 2), (2, 4), (3, 5)] {
		let first = builder.mesh.vertices.len() as u32;
		let mut square = Vec::new();
		for step in 0..=per_side {
			let index = side * per_side + step;
			let radial = direction(index);
			let flat = corners[side as usize].lerp(corners[((side + 1) % 4) as usize], step as f32 / per_side as f32) * 0.5;
			for x in [-0.5, 0.5] {
				builder.vertex(Vec3::new(x, 0.0, 0.0) + radial * 0.5, radial, Vec2::ZERO);
				square.push(Vec3::new(x, flat.x, flat.y));
			}
		}
		for step in 0..per_side {
			let a = first + step * 2;
			builder.triangle(a, a + 2, a + 1);
			builder.triangle(a + 1, a + 2, a + 3);
		}
		material_face(&mut builder.mesh.vertices[first as usize..], &square, code);
	}
	for side in [-1.0f32, 1.0] {
		let normal = Vec3::new(side, 0.0, 0.0);
		let cap = builder.mesh.vertices.len();
		let center = builder.vertex(normal * 0.5, normal, Vec2::splat(0.5));
		let first = builder.mesh.vertices.len() as u32;
		for index in 0..=count {
			let radial = direction(index);
			builder.vertex(normal * 0.5 + radial * 0.5, normal, Vec2::new(0.5 + radial.z * 0.5, 0.5 - radial.y * 0.5));
		}
		for index in 0..count {
			if side > 0.0 {
				builder.triangle(center, first + index, first + index + 1);
			} else {
				builder.triangle(center, first + index + 1, first + index);
			}
		}
		// flat caps: the box face's coordinates of the actual (disk) positions, from the face's corner
		let code = if side > 0.0 { 0 } else { 3 };
		let [u, v] = MATERIAL_FACES[code as usize].map(Vec3::from);
		for vertex in &mut builder.mesh.vertices[cap..] {
			let position = Vec3::from(vertex.position);
			vertex.uv = [position.dot(u) + 0.5, position.dot(v) + 0.5];
			vertex.color[3] = code;
		}
	}
	builder.finish()
}

/// Gives vertices without a usable normal the area-weighted normal of their faces.
fn repair_normals(vertices: &mut [Vertex], indices: &[u32]) {
	let broken: Vec<bool> = vertices.iter().map(|vertex| !(Vec3::from(vertex.normal).length_squared() > 1e-8)).collect();
	if !broken.iter().any(|&broken| broken) {
		return;
	}
	let mut sums = vec![Vec3::ZERO; vertices.len()];
	for triangle in indices.chunks_exact(3) {
		let [a, b, c] = [triangle[0] as usize, triangle[1] as usize, triangle[2] as usize];
		if a >= vertices.len() || b >= vertices.len() || c >= vertices.len() {
			continue;
		}
		let normal = (Vec3::from(vertices[b].position) - Vec3::from(vertices[a].position)).cross(Vec3::from(vertices[c].position) - Vec3::from(vertices[a].position));
		for index in [a, b, c] {
			sums[index] += normal;
		}
	}
	for (index, vertex) in vertices.iter_mut().enumerate() {
		if broken[index] {
			vertex.normal = sums[index].normalize_or(Vec3::Y).into();
		}
	}
}

/// Per-vertex tangents from UV derivatives (accumulated per triangle, orthonormalised), in Roblox's
/// convention: the tangent follows +u, and `w · cross(n, t)` points up the image (-dP/dv, as v grows
/// down the image), so normal maps are green-up (notes/mesh.md "Tangents").
pub fn generate_tangents(vertices: &mut [Vertex], indices: &[u32]) {
	let mut tangents = vec![Vec3::ZERO; vertices.len()];
	let mut bitangents = vec![Vec3::ZERO; vertices.len()];
	for triangle in indices.chunks_exact(3) {
		let [a, b, c] = [triangle[0] as usize, triangle[1] as usize, triangle[2] as usize];
		if a >= vertices.len() || b >= vertices.len() || c >= vertices.len() {
			continue;
		}
		let (pa, pb, pc) = (Vec3::from(vertices[a].position), Vec3::from(vertices[b].position), Vec3::from(vertices[c].position));
		let (ua, ub, uc) = (Vec2::from(vertices[a].uv), Vec2::from(vertices[b].uv), Vec2::from(vertices[c].uv));
		let (e1, e2) = (pb - pa, pc - pa);
		let (d1, d2) = (ub - ua, uc - ua);
		let determinant = d1.x * d2.y - d2.x * d1.y;
		if determinant.abs() < 1e-12 {
			continue;
		}
		let r = 1.0 / determinant;
		let tangent = (e1 * d2.y - e2 * d1.y) * r;
		let bitangent = (e2 * d1.x - e1 * d2.x) * r;
		for index in [a, b, c] {
			tangents[index] += tangent;
			bitangents[index] += bitangent;
		}
	}
	for (index, vertex) in vertices.iter_mut().enumerate() {
		let normal = Vec3::from(vertex.normal);
		let mut tangent = tangents[index] - normal * normal.dot(tangents[index]);
		if tangent.length_squared() < 1e-12 {
			tangent = normal.any_orthonormal_vector();
		}
		let tangent = tangent.normalize();
		let sign = if normal.cross(tangent).dot(-bitangents[index]) < 0.0 { -1.0 } else { 1.0 };
		vertex.tangent = [tangent.x, tangent.y, tangent.z, sign];
	}
}

/// Converts a decoded mesh asset: UVs to a top-left origin (1.x meshes store them bottom-up), zero
/// normals replaced by the adjacent faces' normal, tangents generated when the file's are unusable.
pub fn from_mesh(mesh: &ror_mesh::Mesh) -> CpuMesh {
	let count = mesh.vertex_count();
	let flip_v = mesh.uv_v_flipped();
	let mut vertices = Vec::with_capacity(count);
	for index in 0..count {
		let mut uv = mesh.uvs.get(index).copied().unwrap_or([0.0, 0.0]);
		if flip_v {
			uv[1] = 1.0 - uv[1];
		}
		vertices.push(Vertex {
			position: mesh.positions[index],
			normal: mesh.normals.get(index).copied().unwrap_or([0.0; 3]),
			uv,
			tangent: mesh.tangents.get(index).copied().unwrap_or([0.0; 4]),
			color: mesh.colors.get(index).copied().unwrap_or([255; 4]),
		});
	}
	let indices: Vec<u32> = mesh.indices.iter().map(|&index| index.min(count.saturating_sub(1) as u32)).collect();
	repair_normals(&mut vertices, &indices);
	if mesh.tangents.len() != count || !ror_mesh::validate::tangents_usable(mesh) || flip_v {
		generate_tangents(&mut vertices, &indices);
	}
	let lods = if mesh.lods.is_empty() { vec![0..indices.len() as u32] } else { mesh.lods.clone() };
	CpuMesh { vertices, indices, lods, bounds: (Vec3::ZERO, Vec3::ZERO), high_quality: mesh.high_quality_lods }.finish()
}
