//! Sky-derived lighting: the skybox cube, its irradiance (9 SH coefficients), a prefiltered
//! specular cube, the split-sum BRDF lookup table, and the sun's direction and colour.
//!
//! Cube faces are filled through [`cube_direction`] — the texel → direction table every graphics
//! API shares — from [`roblox_sky_direction`], Roblox's skybox face layout, so the hardware lookup
//! returns Roblox's sky for any direction.

use glam::{Vec2, Vec3};
use rayon::prelude::*;

/// Linear RGB radiance images for the six cube faces in API order (+X, -X, +Y, -Y, +Z, -Z).
#[derive(Clone)]
pub struct CubeImage {
	pub size: u32,
	pub faces: [Vec<Vec3>; 6],
}

/// The direction of texel (s, t) of a cube face (s, t in 0..1, t downward).
pub fn cube_direction(face: usize, s: f32, t: f32) -> Vec3 {
	let sc = 2.0 * s - 1.0;
	let tc = 2.0 * t - 1.0;
	let direction = match face {
		0 => Vec3::new(1.0, -tc, -sc),
		1 => Vec3::new(-1.0, -tc, sc),
		2 => Vec3::new(sc, 1.0, tc),
		3 => Vec3::new(sc, -1.0, -tc),
		4 => Vec3::new(sc, -tc, 1.0),
		_ => Vec3::new(-sc, -tc, -1.0),
	};
	direction.normalize()
}

/// The cube face and (s, t) a direction samples.
pub fn cube_lookup(direction: Vec3) -> (usize, f32, f32) {
	let a = direction.abs();
	let (face, sc, tc, ma) = if a.x >= a.y && a.x >= a.z {
		if direction.x > 0.0 { (0, -direction.z, -direction.y, a.x) } else { (1, direction.z, -direction.y, a.x) }
	} else if a.y >= a.z {
		if direction.y > 0.0 { (2, direction.x, direction.z, a.y) } else { (3, direction.x, -direction.z, a.y) }
	} else if direction.z > 0.0 {
		(4, direction.x, -direction.y, a.z)
	} else {
		(5, -direction.x, -direction.y, a.z)
	};
	(face, (sc / ma + 1.0) * 0.5, (tc / ma + 1.0) * 0.5)
}

/// Roblox's skybox: which Sky image (Bk, Dn, Ft, Lf, Rt, Up = 0..5) a direction falls on, and
/// where (u right, v down) in it. Measured on Studio's default sky (`sky-faces`,
/// tools/analysis/sky_faces.py, correlation ≥ 0.987): Ft at -Z and Bk at +Z, upright seen from
/// inside; Lf at +X and Rt at -X (named from the viewer's back, as many skybox formats do); Up and
/// Dn turned a quarter turn (u runs towards -Z, v towards -X on Up and +X on Dn).
pub fn roblox_sky_direction(direction: Vec3) -> (usize, f32, f32) {
	let a = direction.abs();
	let (image, u, v) = if a.x >= a.y && a.x >= a.z {
		if direction.x > 0.0 {
			// Lf: looking +X, right is +Z
			(3, direction.z / a.x, -direction.y / a.x)
		} else {
			// Rt: looking -X, right is -Z
			(4, -direction.z / a.x, -direction.y / a.x)
		}
	} else if a.y >= a.z {
		if direction.y > 0.0 {
			(5, -direction.z / a.y, -direction.x / a.y)
		} else {
			(1, -direction.z / a.y, direction.x / a.y)
		}
	} else if direction.z > 0.0 {
		// Bk: looking +Z, right is -X
		(0, -direction.x / a.z, -direction.y / a.z)
	} else {
		// Ft: looking -Z, right is +X
		(2, direction.x / a.z, -direction.y / a.z)
	};
	(image, (u + 1.0) * 0.5, (v + 1.0) * 0.5)
}

/// A decoded sky face, row-major.
pub struct SkyFace {
	pub width: u32,
	pub height: u32,
	pub pixels: SkyPixels,
}

/// A sky face's texels: their linear radiance, or an 8-bit image's RGBA texels with the radiance of
/// each channel value, worked out as a texel is read (the same floats: a 1024² face held as radiance
/// took 12 MB, and the default sky's six faces with their copies 130 MB while its maps were made).
pub enum SkyPixels {
	Radiance(Vec<Vec3>),
	Texels { rgba: Vec<u8>, radiance: Box<[f32; 256]> },
}

impl SkyFace {
	/// The RGBA8 texels of a face made from them.
	pub fn texels(&self) -> Option<&[u8]> {
		match &self.pixels {
			SkyPixels::Texels { rgba, .. } => Some(rgba),
			SkyPixels::Radiance(_) => None,
		}
	}

	fn sample(&self, u: f32, v: f32) -> Vec3 {
		let x = (u * self.width as f32 - 0.5).clamp(0.0, self.width as f32 - 1.0);
		let y = (v * self.height as f32 - 0.5).clamp(0.0, self.height as f32 - 1.0);
		let (x0, y0) = (x.floor() as u32, y.floor() as u32);
		let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
		let (fx, fy) = (x - x0 as f32, y - y0 as f32);
		let at = |x: u32, y: u32| {
			let index = (y * self.width + x) as usize;
			match &self.pixels {
				SkyPixels::Radiance(pixels) => pixels[index],
				SkyPixels::Texels { rgba, radiance } => Vec3::new(radiance[rgba[index * 4] as usize], radiance[rgba[index * 4 + 1] as usize], radiance[rgba[index * 4 + 2] as usize]),
			}
		};
		let top = at(x0, y0).lerp(at(x1, y0), fx);
		let bottom = at(x0, y1).lerp(at(x1, y1), fx);
		top.lerp(bottom, fy)
	}
}

/// SkyboxOrientation's turn of the skybox: a direction in the world looks along `rotation ·
/// direction` in its images. Rz · Rx · Ry (sky-elements: (10, 20, 30) degrees 94 / 92 % within 2 of
/// 255 front / up, 5 / 13 % as Ry · Rx · Rz; single axes alike either way).
pub fn sky_rotation(orientation_degrees: Vec3) -> glam::Mat3 {
	glam::Mat3::from_euler(glam::EulerRot::ZXY, orientation_degrees.z.to_radians(), orientation_degrees.x.to_radians(), orientation_degrees.y.to_radians()).transpose()
}

/// Builds the sky cube from the six Roblox skybox images (Bk, Dn, Ft, Lf, Rt, Up).
pub fn sky_cube(images: &[SkyFace; 6], size: u32, orientation_degrees: Vec3) -> CubeImage {
	let rotation = sky_rotation(orientation_degrees);
	cube_from(size, |_, _, direction| {
		let (image, u, v) = roblox_sky_direction(rotation * direction);
		images[image].sample(u, v)
	})
}

/// A cube of `size` texels a face from each texel's face, index on it and direction, the texels in
/// parallel.
fn cube_from(size: u32, texel: impl Fn(usize, usize, Vec3) -> Vec3 + Sync) -> CubeImage {
	let faces: Vec<Vec<Vec3>> = (0..6)
		.map(|face| {
			(0..size * size)
				.into_par_iter()
				.map(|index| {
					let (x, y) = (index % size, index / size);
					texel(face, index as usize, cube_direction(face, (x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32))
				})
				.collect()
		})
		.collect();
	CubeImage { size, faces: faces.try_into().unwrap_or_else(|_| unreachable!()) }
}

/// A plain sky for when no skybox images are available: a vertical gradient.
pub fn gradient_cube(size: u32, zenith: Vec3, horizon: Vec3, ground: Vec3) -> CubeImage {
	let faces: Vec<Vec<Vec3>> = (0..6)
		.map(|face| {
			let mut pixels = Vec::with_capacity((size * size) as usize);
			for y in 0..size {
				for x in 0..size {
					let direction = cube_direction(face, (x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
					let color = if direction.y >= 0.0 { horizon.lerp(zenith, direction.y.powf(0.6)) } else { horizon.lerp(ground, (-direction.y).powf(0.4)) };
					pixels.push(color);
				}
			}
			pixels
		})
		.collect();
	CubeImage { size, faces: faces.try_into().unwrap_or_else(|_| unreachable!()) }
}

impl CubeImage {
	pub fn sample(&self, direction: Vec3) -> Vec3 {
		let (face, s, t) = cube_lookup(direction);
		let size = self.size;
		let x = ((s * size as f32) as u32).min(size - 1);
		let y = ((t * size as f32) as u32).min(size - 1);
		self.faces[face][(y * size + x) as usize]
	}

	/// Bilinear lookup that continues across face edges (a texel beyond an edge is the neighbouring
	/// face's texel in that direction), like a GPU's seamless cube filtering.
	pub fn sample_bilinear(&self, direction: Vec3) -> Vec3 {
		let (face, s, t) = cube_lookup(direction);
		let size = self.size as f32;
		let (x, y) = (s * size - 0.5, t * size - 0.5);
		let (x0, y0) = (x.floor(), y.floor());
		let (fx, fy) = (x - x0, y - y0);
		let texel = |xi: f32, yi: f32| -> Vec3 {
			if xi >= 0.0 && yi >= 0.0 && xi < size && yi < size {
				self.faces[face][(yi as u32 * self.size + xi as u32) as usize]
			} else {
				self.sample(cube_direction(face, (xi + 0.5) / size, (yi + 0.5) / size))
			}
		};
		let top = texel(x0, y0).lerp(texel(x0 + 1.0, y0), fx);
		let bottom = texel(x0, y0 + 1.0).lerp(texel(x0 + 1.0, y0 + 1.0), fx);
		top.lerp(bottom, fy)
	}

	/// Every texel times the time of day's tint along its direction ([`tint_at`]).
	pub fn tinted(&self, tint: (Vec3, Vec3)) -> CubeImage {
		cube_from(self.size, |face, index, direction| self.faces[face][index] * tint_at(tint, direction))
	}

	/// Every texel times the time of day's tint as the sky's light takes it: the colour above the
	/// horizon for every direction above it and the one below for every direction under it, not
	/// blended as the drawn sky ([`tint_at`]; light-night-sky's sky runs, the default skybox lighting a
	/// floor, a wall and tilted tiles by itself at ClockTime 0-22: Studio's light over ror's with the
	/// drawn sky's blend 0.47-0.85 on the floor at twilight and 1.11-1.53 at midnight, now 0.95-1.08,
	/// midnight's walls 1.05-1.15).
	pub fn tinted_by_halves(&self, (above, below): (Vec3, Vec3)) -> CubeImage {
		cube_from(self.size, |face, index, direction| {
			let shown = if direction.y > 0.0 { above } else { below };
			self.faces[face][index] * shown * shown
		})
	}

	/// Box-filtered half-size cube.
	pub fn downsample(&self) -> CubeImage {
		let size = (self.size / 2).max(1);
		let faces: Vec<Vec<Vec3>> = self
			.faces
			.iter()
			.map(|face| {
				let mut pixels = Vec::with_capacity((size * size) as usize);
				for y in 0..size {
					for x in 0..size {
						let at = |dx: u32, dy: u32| face[((y * 2 + dy).min(self.size - 1) * self.size + (x * 2 + dx).min(self.size - 1)) as usize];
						pixels.push((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) * 0.25);
					}
				}
				pixels
			})
			.collect();
		CubeImage { size, faces: faces.try_into().unwrap_or_else(|_| unreachable!()) }
	}

	/// Projects radiance onto 9 real SH coefficients (bands 0-2), basis order
	/// Y00, Y1-1 (y), Y10 (z), Y11 (x), Y2-2 (xy), Y2-1 (yz), Y20, Y21 (xz), Y22 (x²-y²).
	pub fn sh9(&self) -> [Vec3; 9] {
		let mut coefficients = [Vec3::ZERO; 9];
		let mut weight_sum = 0.0;
		let size = self.size;
		for face in 0..6 {
			for y in 0..size {
				for x in 0..size {
					let s = (x as f32 + 0.5) / size as f32;
					let t = (y as f32 + 0.5) / size as f32;
					let (sc, tc) = (2.0 * s - 1.0, 2.0 * t - 1.0);
					// solid angle of the texel
					let weight = 4.0 / ((1.0 + sc * sc + tc * tc).powf(1.5) * (size * size) as f32);
					let direction = cube_direction(face, s, t);
					let color = self.faces[face][(y * size + x) as usize];
					let basis = sh_basis(direction);
					for index in 0..9 {
						coefficients[index] += color * basis[index] * weight;
					}
					weight_sum += weight;
				}
			}
		}
		let normalize = 4.0 * std::f32::consts::PI / weight_sum;
		coefficients.map(|value| value * normalize)
	}

	/// The sky light along the six axes, +X, -X, +Y, -Y, +Z, -Z: the clamped-cosine average of the
	/// cube's texels (by solid angle) around each axis, what a surface of albedo 1 facing that way
	/// reflects (irradiance / π; a uniform sky gives its radiance). Studio's sky light is this ambient
	/// cube of the sky box-filtered to 4 x 4 texels a face, blended by the normal's squared
	/// components (shaders/common.wgsl `sky_light`; notes/render.md "Sky").
	pub fn ambient_cube(&self) -> [Vec3; 6] {
		let axes = [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z];
		let mut light = [Vec3::ZERO; 6];
		let mut weights = [0.0f32; 6];
		let size = self.size;
		for face in 0..6 {
			for y in 0..size {
				for x in 0..size {
					let s = (x as f32 + 0.5) / size as f32;
					let t = (y as f32 + 0.5) / size as f32;
					let (sc, tc) = (2.0 * s - 1.0, 2.0 * t - 1.0);
					let weight = 4.0 / ((1.0 + sc * sc + tc * tc).powf(1.5) * (size * size) as f32);
					let direction = cube_direction(face, s, t);
					let color = self.faces[face][(y * size + x) as usize];
					for (index, axis) in axes.iter().enumerate() {
						let cosine = axis.dot(direction).max(0.0) * weight;
						light[index] += color * cosine;
						weights[index] += cosine;
					}
				}
			}
		}
		std::array::from_fn(|index| light[index] / weights[index])
	}
}

fn sh_basis(d: Vec3) -> [f32; 9] {
	[
		0.282095,
		0.488603 * d.y,
		0.488603 * d.z,
		0.488603 * d.x,
		1.092548 * d.x * d.y,
		1.092548 * d.y * d.z,
		0.315392 * (3.0 * d.z * d.z - 1.0),
		1.092548 * d.x * d.z,
		0.546274 * (d.x * d.x - d.y * d.y),
	]
}

fn hammersley(index: u32, count: u32) -> Vec2 {
	Vec2::new(index as f32 / count as f32, index.reverse_bits() as f32 * 2.328_306_4e-10)
}

fn importance_sample_ggx(xi: Vec2, roughness: f32, normal: Vec3) -> Vec3 {
	let (tangent, bitangent) = tangent_frame(normal);
	about(ggx_half(xi, roughness), normal, tangent, bitangent)
}

/// A GGX half vector about +Z.
fn ggx_half(xi: Vec2, roughness: f32) -> Vec3 {
	let a = roughness * roughness;
	let phi = 2.0 * std::f32::consts::PI * xi.x;
	let cos_theta = ((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y)).sqrt();
	let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
	Vec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta)
}

/// The tangent and bitangent a half vector about +Z is turned about `normal` with.
fn tangent_frame(normal: Vec3) -> (Vec3, Vec3) {
	let up = if normal.z.abs() < 0.999 { Vec3::Z } else { Vec3::X };
	let tangent = up.cross(normal).normalize();
	(tangent, normal.cross(tangent))
}

fn about(h: Vec3, normal: Vec3, tangent: Vec3, bitangent: Vec3) -> Vec3 {
	(tangent * h.x + bitangent * h.y + normal * h.z).normalize()
}

/// The specular mip chain: level 0 is the sky, each further level half the size and GGX-filtered
/// (N = V = R) at roughness `i / (levels - 1)` from the level before it, so the blur accumulates down
/// the chain. Studio's sky reflections have that shape (ibl-spheres, split sky: a sharp core and a
/// halo that widens with roughness, past 90 degrees at roughness 1): 6 levels from 128, looked up at
/// lod 0.375 + 4.5 · roughness (shaders/mesh.wgsl), explain metal spheres of roughness 0..1 within
/// 5-10 % rms each (levels filtered from the sky alone at lod 5 · roughness: 8-37 %). Every level is
/// kept as Studio keeps it, [`stored`]: the loss accumulates down the chain, twice as fast in blue
/// (ibl-spheres' white metals: Studio's blue / green against ror's unstored chain 0.995 at lod 0.4 down
/// to 0.967 at 4.9; with it 0.993-1.003, and the 6 % less a sky specular at roughness 1 that ror had
/// fitted is gone).
pub fn prefilter_specular(source: &CubeImage, levels: u32) -> Vec<CubeImage> {
	let [chain] = prefilter_specular_all([source], levels);
	chain
}

/// [`prefilter_specular`] of cubes of one size together: each sample's direction is worked out once
/// for all of them, and every level's rows are filtered in parallel (a clip whose Lighting changes
/// filters a chain, and the haze's, each frame).
pub fn prefilter_specular_all<const N: usize>(sources: [&CubeImage; N], levels: u32) -> [Vec<CubeImage>; N] {
	let mut chains: [Vec<CubeImage>; N] = sources.map(|source| vec![CubeImage { size: source.size, faces: source.faces.clone().map(|face| face.into_iter().map(stored).collect()) }]);
	let samples = 128u32;
	for level in 1..levels as usize {
		let roughness = level as f32 / (levels - 1) as f32;
		let previous: [&CubeImage; N] = std::array::from_fn(|index| &chains[index][level - 1]);
		let size = (previous.first().map_or(1, |cube| cube.size) / 2).max(1);
		let halves: Vec<Vec3> = (0..samples).map(|index| ggx_half(hammersley(index, samples), roughness)).collect();
		let rows: Vec<[Vec<Vec3>; N]> = (0..6 * size)
			.into_par_iter()
			.map(|row| {
				let (face, y) = ((row / size) as usize, row % size);
				let mut pixels: [Vec<Vec3>; N] = std::array::from_fn(|_| Vec::with_capacity(size as usize));
				for x in 0..size {
					let normal = cube_direction(face, (x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
					let (tangent, bitangent) = tangent_frame(normal);
					let mut sums = [Vec3::ZERO; N];
					let mut weight = 0.0;
					for &half in &halves {
						let h = about(half, normal, tangent, bitangent);
						let l = 2.0 * normal.dot(h) * h - normal;
						let n_dot_l = normal.dot(l);
						if n_dot_l > 0.0 {
							for (sum, cube) in sums.iter_mut().zip(previous) {
								*sum += cube.sample_bilinear(l) * n_dot_l;
							}
							weight += n_dot_l;
						}
					}
					for (pixels, sum) in pixels.iter_mut().zip(sums) {
						pixels.push(stored(if weight > 0.0 { sum / weight } else { Vec3::ZERO }));
					}
				}
				pixels
			})
			.collect();
		for (index, chain) in chains.iter_mut().enumerate() {
			let faces: [Vec<Vec3>; 6] = std::array::from_fn(|face| rows[face * size as usize..(face + 1) * size as usize].iter().flat_map(|row| row[index].iter().copied()).collect());
			chain.push(CubeImage { size, faces });
		}
	}
	chains
}

/// What an R11G11B10 float texel keeps of a colour: its mantissas cut to 6 bits (red, green) and 5
/// (blue), rounded towards zero, a mean loss of 0.35 · 2⁻⁶ and 0.35 · 2⁻⁵ (as Studio's bloom levels
/// keep theirs, shaders/bloom.wgsl).
pub fn stored(color: Vec3) -> Vec3 {
	let cut = |value: f32, mask: u32| f32::from_bits(value.max(0.0).to_bits() & mask);
	Vec3::new(cut(color.x, 0xfffe_0000), cut(color.y, 0xfffe_0000), cut(color.z, 0xfffc_0000))
}

/// Split-sum BRDF table (Karis 2013) laid out like Studio's: x = roughness, y = N·V (texel
/// centres) → (scale, bias) of F0. GGX with α = roughness² and the height-correlated Smith
/// visibility: Studio's own table (`brdfLUT.dds`, [`studio_brdf_lut`]) is that within 0.002, where
/// Schlick-GGX (k = α / 2) is 0.15 lower at grazing angles (roughness 0.5, N·V 0.25: 0.606 against
/// 0.757).
pub fn brdf_lut(size: u32) -> Vec<[f32; 2]> {
	(0..size * size)
		.into_par_iter()
		.map(|index| split_sum_brdf(((index % size) as f32 + 0.5) / size as f32, ((index / size) as f32 + 0.5) / size as f32, 512))
		.collect()
}

/// One entry of [`brdf_lut`]: (scale, bias) of F0 at a roughness and N·V.
pub fn split_sum_brdf(roughness: f32, n_dot_v: f32, samples: u32) -> [f32; 2] {
	let v = Vec3::new((1.0 - n_dot_v * n_dot_v).sqrt(), 0.0, n_dot_v);
	let a2 = (roughness * roughness).powi(2);
	let (mut a, mut b) = (0.0f32, 0.0f32);
	for sample in 0..samples {
		let h = importance_sample_ggx(hammersley(sample, samples), roughness, Vec3::Z);
		let l = 2.0 * v.dot(h) * h - v;
		let n_dot_l = l.z;
		if n_dot_l > 0.0 {
			let n_dot_h = h.z.max(1e-6);
			let v_dot_h = v.dot(h).max(0.0);
			let g = 2.0 * n_dot_v * n_dot_l / (n_dot_l * (n_dot_v * n_dot_v * (1.0 - a2) + a2).sqrt() + n_dot_v * (n_dot_l * n_dot_l * (1.0 - a2) + a2).sqrt());
			let g_vis = g * v_dot_h / (n_dot_h * n_dot_v);
			let fc = (1.0 - v_dot_h).powi(5);
			a += (1.0 - fc) * g_vis;
			b += fc * g_vis;
		}
	}
	[a / samples as f32, b / samples as f32]
}

/// Studio's split-sum table (`PlatformContent/pc/textures/brdfLUT.dds`): its side and its texels
/// as raw half floats (scale, bias), in [`brdf_lut`]'s layout. None unless the file is a square
/// DX10 DDS of DXGI_FORMAT_R16G16_FLOAT.
pub fn studio_brdf_lut(bytes: &[u8]) -> Option<(u32, Vec<u16>)> {
	if bytes.len() < 148 || &bytes[0..4] != b"DDS " || &bytes[84..88] != b"DX10" {
		return None;
	}
	let u32_at = |offset: usize| u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]);
	let (height, width) = (u32_at(12), u32_at(16));
	if u32_at(128) != 34 || width != height || width == 0 {
		return None;
	}
	let data = bytes.get(148..148 + (width * height * 4) as usize)?;
	Some((width, data.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect()))
}

/// Direction towards the sun for a clock time and latitude, as `Lighting:GetSunDirection()` returns
/// it (fitted to Studio 0.741 samples within 1e-4, notes/sun-samples.txt): the sun circles the Z
/// axis, its circle tilted by `latitude - 23.5` degrees.
pub fn sun_direction(clock_time: f32, latitude_degrees: f32) -> Vec3 {
	let hour_angle = (clock_time - 12.0) / 24.0 * std::f32::consts::TAU;
	let tilt = (latitude_degrees - 23.5).to_radians();
	Vec3::new(-hour_angle.sin() * tilt.cos(), hour_angle.cos() * tilt.cos(), tilt.sin())
}

/// `Lighting:GetMoonDirection()`: the sun's direction mirrored in X and Y.
pub fn moon_direction(clock_time: f32, latitude_degrees: f32) -> Vec3 {
	let sun = sun_direction(clock_time, latitude_degrees);
	Vec3::new(-sun.x, -sun.y, sun.z)
}

/// The one directional light Lighting gives: where it comes from and its strength per channel as a
/// share of Brightness (a white surface facing it reaches this · Brightness, tinted by
/// ColorShift_Top).
#[derive(Clone, Copy, Debug)]
pub struct DirectLight {
	pub direction: Vec3,
	pub strength: Vec3,
}

/// The sun's strength per channel by ClockTime from 5.25 to 18.75, whether it is up or not yet: it
/// comes up within half an hour of sunrise, yellow first (blue from 6.15 to 6.5), and goes down within
/// half an hour before sunset, by the time, not the sun's height (light-night-sky, light-night-terms,
/// light-night-twilight: a white wall facing -Z over the noon sun's, every 0.05 hours; 6.25 at
/// latitudes 0, 30 and 60 alike, the sun 3.0-3.7 degrees up); below the horizon it still lights what
/// faces it, little.
const SUN_STRENGTH: [(f32, [f32; 3]); 31] = [
	(5.25, [0.008, 0.008, 0.008]),
	(5.4, [0.007, 0.007, 0.007]),
	(5.5, [0.005, 0.005, 0.005]),
	(5.75, [0.002, 0.002, 0.002]),
	(6.0, [0.0, 0.0, 0.0]),
	(6.05, [0.105, 0.105, 0.0]),
	(6.1, [0.41, 0.41, 0.0]),
	(6.15, [0.663, 0.663, 0.005]),
	(6.2, [0.707, 0.707, 0.042]),
	(6.25, [0.752, 0.752, 0.113]),
	(6.3, [0.801, 0.801, 0.221]),
	(6.35, [0.849, 0.849, 0.364]),
	(6.4, [0.896, 0.896, 0.538]),
	(6.45, [0.953, 0.953, 0.752]),
	(6.5, [1.0, 1.0, 1.0]),
	(17.5, [1.0, 1.0, 1.0]),
	(17.55, [0.688, 0.688, 0.672]),
	(17.6, [0.427, 0.427, 0.409]),
	(17.65, [0.233, 0.233, 0.216]),
	(17.7, [0.097, 0.097, 0.081]),
	(17.75, [0.018, 0.018, 0.011]),
	(17.8, [0.018, 0.014, 0.008]),
	(17.85, [0.018, 0.011, 0.008]),
	(17.9, [0.018, 0.008, 0.007]),
	(17.95, [0.018, 0.007, 0.005]),
	(18.0, [0.018, 0.005, 0.005]),
	(18.25, [0.018, 0.011, 0.011]),
	(18.5, [0.018, 0.018, 0.018]),
	(18.6, [0.020, 0.020, 0.020]),
	(18.7, [0.020, 0.020, 0.020]),
	(18.75, [0.020, 0.020, 0.020]),
];
/// The moon's strength by ClockTime from 19.375 to 4.625 (hours past 19.375; 24 + the morning
/// hours): it follows the time, not the moon's height, the evening a little brighter than the
/// morning (light-night, light-night-twilight: white floors and walls over ror's; at midnight
/// 0.071-0.073 at latitudes -30, 0, 23.5 and 60, the moon 36 to 90 degrees up).
const MOON_STRENGTH: [(f32, f32); 14] = [
	(19.375, 0.026),
	(19.75, 0.029),
	(20.0, 0.0298),
	(21.0, 0.0383),
	(22.0, 0.0485),
	(23.0, 0.0586),
	(24.0, 0.0714),
	(25.0, 0.0578),
	(26.0, 0.0465),
	(27.0, 0.0351),
	(28.0, 0.0263),
	(28.25, 0.025),
	(28.5, 0.023),
	(28.625, 0.023),
];
/// With the sky's light on, the night is lit at this much of Brightness · EnvironmentDiffuseScale²
/// from the moon's direction, whatever the hour, when the moon alone gives less, the dimmed sun too
/// (light-night-terms, -twilight: a white wall's light with it over without it, 0.206 ± 0.003 from
/// 19.5 to 4.5 and from 5.3 to 6 and 17.75 to 18.7 at the default Lighting, where the sky specular
/// keeps 0.961 of the noon sun's; 0.052-0.068 at EnvironmentDiffuseScale 0.5, 0.206 · 0.25 = 0.052,
/// the moon alone 0.072 at midnight; white, shadowing as the sun, ColorShift_Top tinting it; none of
/// it from Ambient, OutdoorAmbient or EnvironmentSpecularScale).
const NIGHT_SKY_LIGHT: f32 = 0.214;

fn table(points: &[(f32, f32)], at: f32) -> f32 {
	let next = points.iter().position(|&(time, _)| time >= at).unwrap_or(points.len() - 1);
	if next == 0 {
		return points[0].1;
	}
	let ((t0, v0), (t1, v1)) = (points[next - 1], points[next]);
	v0 + (v1 - v0) * ((at - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0)
}

fn table3(points: &[(f32, [f32; 3])], at: f32) -> Vec3 {
	let next = points.iter().position(|&(time, _)| time >= at).unwrap_or(points.len() - 1);
	if next == 0 {
		return Vec3::from(points[0].1);
	}
	let ((t0, v0), (t1, v1)) = (points[next - 1], points[next]);
	Vec3::from(v0).lerp(Vec3::from(v1), ((at - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0))
}

/// Lighting's directional light at a ClockTime and latitude with EnvironmentDiffuseScale
/// `diffuse_scale`: the sun from 5.25 to 18.75 (below the horizon at either end), the moon from
/// 19.375 to 4.625, nothing between (light-night-twilight: neither lights a white ball or wall from
/// 4.75 to 5.2 and 18.8 to 19.25, the moon at 4.5 and 19.5, the sun below the horizon at 5.3 and 18.7).
pub fn direct_light(clock_time: f32, latitude_degrees: f32, diffuse_scale: f32) -> DirectLight {
	let hours = clock_time.rem_euclid(24.0);
	let night_sky = Vec3::splat(NIGHT_SKY_LIGHT * diffuse_scale.clamp(0.0, 1.0).powi(2));
	if (5.25..18.75).contains(&hours) {
		return DirectLight { direction: sun_direction(clock_time, latitude_degrees), strength: table3(&SUN_STRENGTH, hours).max(night_sky) };
	}
	let since = if hours < 12.0 { hours + 24.0 } else { hours };
	if MOON_HOURS.contains(&since) {
		return DirectLight { direction: moon_direction(clock_time, latitude_degrees), strength: Vec3::splat(table(&MOON_STRENGTH, since)).max(night_sky) };
	}
	DirectLight { direction: moon_direction(clock_time, latitude_degrees), strength: Vec3::ZERO }
}

/// The moon's hours, 19.375 to 4.625 o'clock (the morning's counted on past 24).
const MOON_HOURS: std::ops::Range<f32> = 19.375..28.625;

/// A ViewportFrame's light in Lighting's units (notes/render.md "ViewportFrames"): a white surface
/// facing a `LightColor` c reaches VIEWPORT_LIGHT · c², its `Ambient` a gives the flat ambient
/// VIEWPORT_AMBIENT · a² (about half Lighting.Ambient's 1.98 · a²), and the light travels along
/// `LightDirection`. Fitted to a game's character card images (Studio captures of the cards'
/// ViewportFrames, shown as √min(1, x) per channel): over the 57 best-aligned cards the ambient's
/// share 1.034 and the light's 1.001 by medians, one factor of 1.03 for both as good; with the light
/// coming from `LightDirection` instead its share fits -0.34. `ROR_TUNE_VIEWPORT="light,ambient,
/// towards[,neon]"` overrides them for fits (towards -1: the light comes from `LightDirection`).
const VIEWPORT_LIGHT: f32 = 1.03;
const VIEWPORT_AMBIENT: f32 = 1.03;
const VIEWPORT_TOWARDS: f32 = 1.0;
/// Neon in a ViewportFrame as a share of its emission in Lighting's world (`neon_emission`; Roblox
/// draws Neon "at the lowest quality" there): the cards with Neon fit half (one card's
/// shiny card: its 39,556 Neon pixels a mean 2.0 of 255 off at 0.5, 15.8 at 1).
const VIEWPORT_NEON: f32 = 0.5;

fn viewport_tune() -> [f32; 4] {
	static TUNE: std::sync::OnceLock<[f32; 4]> = std::sync::OnceLock::new();
	*TUNE.get_or_init(|| {
		let mut tuned = [VIEWPORT_LIGHT, VIEWPORT_AMBIENT, VIEWPORT_TOWARDS, VIEWPORT_NEON];
		if let Ok(text) = std::env::var("ROR_TUNE_VIEWPORT") {
			for (slot, value) in tuned.iter_mut().zip(text.split(',').filter_map(|part| part.trim().parse::<f32>().ok())) {
				*slot = value;
			}
		}
		tuned
	})
}

/// A ViewportFrame's directional light: towards where its light comes from, and its strength per
/// channel as a share of a Brightness-1 sun's.
pub fn viewport_light(lighting: &crate::scene::environment::ViewportLighting) -> DirectLight {
	let [light, _, towards, _] = viewport_tune();
	let travels = lighting.light_direction.try_normalize().unwrap_or(Vec3::NEG_Y);
	DirectLight { direction: -travels * towards.signum(), strength: lighting.light_color * lighting.light_color * light }
}

/// A ViewportFrame's flat ambient (the shader's `Frame.ambient`).
pub fn viewport_ambient(lighting: &crate::scene::environment::ViewportLighting) -> Vec3 {
	lighting.ambient * lighting.ambient * viewport_tune()[1]
}

/// Neon's emission in a ViewportFrame, as a share of what it emits in Lighting's world.
pub fn viewport_neon() -> f32 {
	viewport_tune()[3]
}

/// Where the Atmosphere's glare sits and how bright it is as a share of the day's, per channel, at a
/// ClockTime and latitude: about the sun, at the sun's strength by the hour ([`SUN_STRENGTH`]) but
/// never below [`GLARE_NIGHT`], and about the moon at [`GLARE_NIGHT`] in the moon's hours
/// (atmosphere-sky, atmosphere-night-glare: a dark skybox under haze, Decay (1, 0.5, 0); the red,
/// even, reads the sun's table to 0.003 at 17.6, 6.1, 6.25 and 6.4; the glare about the moon from
/// 19.5 to 4.5, about the sun below the horizon at 4.75-5.5 and 18-19.25, at latitudes 0 and 45).
pub fn glare_light(clock_time: f32, latitude_degrees: f32) -> (Vec3, Vec3) {
	let hours = clock_time.rem_euclid(24.0);
	let night = Vec3::splat(GLARE_NIGHT);
	let since = if hours < 12.0 { hours + 24.0 } else { hours };
	if MOON_HOURS.contains(&since) {
		return (moon_direction(clock_time, latitude_degrees), night);
	}
	let strength = if (5.25..18.75).contains(&hours) { table3(&SUN_STRENGTH, hours) } else { Vec3::ZERO };
	(sun_direction(clock_time, latitude_degrees), strength.max(night))
}

/// Studio tints the skybox by the time of day, from a colour above to one below the horizon (sky-time,
/// sky-night, sky-elements dim: a white skybox per channel; a ball lit by the sky alone follows it):
/// none from 6.5 to 17.5, orange at dawn and dusk, purple near the horizon early in the night.
/// (ClockTime, above, below) as display (√) multipliers, linear between, round the clock; fitted to
/// every sky pixel within 0.002 rms ([`tint_at`] blends them).
const SKY_TINT: [(f32, [f32; 3], [f32; 3]); 28] = [
	(0.0, [0.202, 0.202, 0.202], [0.093, 0.093, 0.093]),
	(2.0, [0.147, 0.147, 0.147], [0.162, 0.162, 0.162]),
	(4.0, [0.086, 0.086, 0.084], [0.207, 0.207, 0.278]),
	(4.5, [0.123, 0.124, 0.139], [0.271, 0.238, 0.284]),
	(5.0, [0.159, 0.163, 0.192], [0.334, 0.267, 0.291]),
	(5.25, [0.227, 0.202, 0.143], [0.365, 0.283, 0.295]),
	(5.5, [0.294, 0.246, 0.099], [0.397, 0.297, 0.296]),
	(5.75, [0.295, 0.248, 0.097], [0.348, 0.248, 0.297]),
	(6.0, [0.296, 0.249, 0.099], [0.296, 0.197, 0.296]),
	(6.125, [0.473, 0.436, 0.322], [0.473, 0.398, 0.472]),
	(6.25, [0.649, 0.624, 0.549], [0.649, 0.598, 0.647]),
	(6.375, [0.830, 0.816, 0.774], [0.830, 0.802, 0.829]),
	(6.5, [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
	(17.5, [1.0, 1.0, 1.0], [1.0, 1.0, 1.0]),
	(17.625, [0.879, 0.830, 0.786], [0.851, 0.830, 0.802]),
	(17.75, [0.751, 0.649, 0.571], [0.700, 0.649, 0.598]),
	(17.875, [0.627, 0.473, 0.358], [0.547, 0.473, 0.398]),
	(18.0, [0.500, 0.296, 0.143], [0.398, 0.296, 0.197]),
	(18.25, [0.193, 0.143, 0.102], [0.322, 0.222, 0.272]),
	(18.5, [0.092, 0.093, 0.092], [0.296, 0.196, 0.296]),
	(18.75, [0.094, 0.095, 0.094], [0.297, 0.197, 0.297]),
	(19.0, [0.101, 0.102, 0.101], [0.297, 0.196, 0.297]),
	(19.5, [0.108, 0.110, 0.108], [0.297, 0.197, 0.297]),
	(20.0, [0.120, 0.122, 0.120], [0.296, 0.196, 0.296]),
	(21.0, [0.137, 0.137, 0.137], [0.197, 0.197, 0.197]),
	(22.0, [0.162, 0.162, 0.162], [0.162, 0.162, 0.162]),
	(23.0, [0.184, 0.184, 0.184], [0.126, 0.126, 0.126]),
	(24.0, [0.202, 0.202, 0.202], [0.093, 0.093, 0.093]),
];

/// How much of the night's sky, drawn and as light, is the sky's light itself, by ClockTime: all of
/// it from 20.75 to 3.25, none from 4 to 20 (light-night-sky, -sky2: at EnvironmentDiffuseScale 0 the
/// default sky is black at 21-3 o'clock and as it is at 4-6 and 18-20; 2-5 of 255 at 3.5 and 20.5).
const NIGHT_SKY_SHARE: [(f32, f32); 6] = [(3.25, 1.0), (3.5, 0.9), (4.0, 0.0), (20.0, 0.0), (20.5, 0.9), (20.75, 1.0)];

/// The skybox's display tint above and below the horizon at a ClockTime ([`SKY_TINT`]) under
/// EnvironmentDiffuseScale `diffuse_scale`: the night's own sky light scales with its square, as the
/// moon's does (light-night-sky2: the sky's light on a white wall at midnight 0.10-0.18 of itself at
/// 0.5, the scale and the square together; the sky drawn 1-2 of 255 where it shows 3-9); None by day.
pub fn sky_tint(clock_time: f32, diffuse_scale: f32) -> Option<(Vec3, Vec3)> {
	let clock = clock_time.rem_euclid(24.0);
	if (6.5..=17.5).contains(&clock) {
		return None;
	}
	let share = if (3.25..20.75).contains(&clock) { table(&NIGHT_SKY_SHARE, clock) } else { 1.0 };
	let scale = (1.0 - share * (1.0 - diffuse_scale.clamp(0.0, 1.0).powi(2))).sqrt();
	for pair in SKY_TINT.windows(2) {
		let ((c0, a0, b0), (c1, a1, b1)) = (pair[0], pair[1]);
		if clock <= c1 {
			let t = (clock - c0) / (c1 - c0);
			return Some((Vec3::from(a0).lerp(Vec3::from(a1), t) * scale, Vec3::from(b0).lerp(Vec3::from(b1), t) * scale));
		}
	}
	None
}

/// The radiance tint along `direction`: the display tints above and below the horizon blend linearly
/// on the sky's cube, by saturate(0.75 (1 - y / max(|x|, |z|))): all "above" from the side faces' top
/// edge, all "below" a third of the way down them (sky-elements dim: the share in √ space follows
/// the cube's coordinate within 0.015 sd, against 0.03 for the elevation's sine or tangent).
pub fn tint_at((above, below): (Vec3, Vec3), direction: Vec3) -> Vec3 {
	let side = direction.x.abs().max(direction.z.abs());
	let blend = if side > 0.0 {
		(0.75 * (1.0 - direction.y / side)).clamp(0.0, 1.0)
	} else if direction.y > 0.0 {
		0.0
	} else {
		1.0
	};
	let shown = above.lerp(below, blend);
	shown * shown
}

/// How much of the skybox Studio's Atmosphere haze hides by elevation e (atmosphere-haze: a white
/// skybox through an orange Color, the blue channel keeps the skybox's share alone): the share
/// left is floor · exp(-τ0 · sin(e)^-b) above the horizon, none below it. Fitted per Haze within
/// 0.004 rms; (Haze, τ0, b, floor), interpolated linearly between. Up to Haze 2 only the exponent
/// grows (b = 0.577 + 0.48 Haze, a 2-3 % veil at the zenith); from 2 to 3 the veil thickens (τ0 to
/// 0.84) and above 3 the haze climbs up the sky, leaving a hole of 36 % skybox straight up.
const HAZE_PROFILE: [(f32, f32, f32, f32); 22] = [
	(0.05, 0.0224, 0.597, 0.992),
	(0.1, 0.0221, 0.624, 0.992),
	(0.25, 0.0214, 0.704, 0.991),
	(0.5, 0.0213, 0.827, 0.991),
	(0.75, 0.0214, 0.946, 0.991),
	(1.0, 0.0216, 1.066, 0.992),
	(1.5, 0.0218, 1.303, 0.992),
	(2.0, 0.0221, 1.540, 0.992),
	(2.05, 0.0719, 1.604, 0.992),
	(2.1, 0.1204, 1.645, 0.989),
	(2.15, 0.1653, 1.693, 0.983),
	(2.2, 0.2113, 1.732, 0.978),
	(2.4, 0.3997, 1.857, 0.968),
	(2.6, 0.5643, 2.025, 0.932),
	(2.8, 0.7414, 2.165, 0.913),
	(3.0, 0.8705, 2.353, 0.865),
	(3.5, 0.8551, 2.664, 0.851),
	(4.0, 0.8443, 2.971, 0.842),
	(5.0, 0.8384, 3.558, 0.836),
	(6.0, 0.8377, 4.135, 0.836),
	(8.0, 0.8360, 5.293, 0.834),
	(10.0, 0.8380, 6.434, 0.836),
];

/// The haze light: Studio lights the haze with the sky light's ambient cube of the skybox (a white
/// skybox's haze is 1.024 x its radiance; one-image skies blend by n² like the sky light,
/// atmosphere-sky face0..5), here relative to the cube [`CubeImage::ambient_cube`] returns.
const HAZE_LIGHT_SCALE: f32 = 1.024;

/// The glare: 0.975 · Brightness · Glare (atmosphere-glare: linear in both, the same at every sun
/// height while the sun is up), about the sun in a lobe ((1 + cos θ) / 2)^n per channel whose
/// exponent n follows that channel's Decay, n = 0.705 (e^(3.785 (1 - Decay)) - 1) (Decay 1 fills
/// the sky evenly, Decay 0 narrows it to about 40 degrees; Color plays no part); dimmer by night
/// ([`glare_light`]).
const GLARE_SCALE: f32 = 0.975;
/// The night's glare as a share of the day's (atmosphere-sky at 0, 18 and 19 o'clock, the haze's own
/// light the skybox's untinted: the even red reads 0.221; atmosphere-night-glare: so from 17.75 to
/// 6.25 wherever the sun's table gives less, green and blue alike at 19.25 and 4.75-5.5; just after
/// sunset, 17.75-17.9, Studio's blue reads 0.256).
const GLARE_NIGHT: f32 = 0.221;

/// ColorShift_Top tints the glare keeping its channels' sum: 1 + k (s² - mean(s²)) per channel, k =
/// 1.73 (1 - 0.47 EnvironmentDiffuseScale) (atmosphere-shift: Haze 10, Glare 1, no sky light, the
/// haze's own light taken off: a dusk place's pink (1, 0.525, 0.675) gives 1.739 / 0.479 / 0.791 of the
/// untinted glare, red 2.164 / 0.423 / 0.428, white the same as black, at every ClockTime and
/// exposure; lobby-sky tint-black-*: 0.765 and 0.53 of that at EnvironmentDiffuseScale 0.5 and 1,
/// nothing from Ambient, OutdoorAmbient or EnvironmentSpecularScale).
const GLARE_SHIFT: f32 = 1.73;
const GLARE_SHIFT_DIFFUSE: f32 = 0.47;

/// The glare's tint under ColorShift_Top `shift` and EnvironmentDiffuseScale `diffuse_scale`. The
/// sun's light has a rule of its own ([`sun_tint`]), the same at EnvironmentDiffuseScale 0 under a
/// white sun.
pub fn glare_tint(shift: Vec3, diffuse_scale: f32) -> Vec3 {
	let squared = shift * shift;
	let strength = GLARE_SHIFT * (1.0 - GLARE_SHIFT_DIFFUSE * diffuse_scale.max(0.0));
	(Vec3::ONE + strength * (squared - Vec3::splat(squared.element_sum() / 3.0))).max(Vec3::ZERO)
}

/// The directional light's strength per channel (`strength`, [`direct_light`]'s) under ColorShift_Top
/// `shift` and EnvironmentDiffuseScale `diffuse_scale`, linear in the scale between two tints (u =
/// shift², D = u - mean(u), r the strength's root mean square):
/// at 0 strength + 1.73 r D, the glare's shift added in proportion to the light's brightness, not its
/// colour; at 1 strength (1 - 0.955 |D|) + 1.69 r |D| u, the light's own colour kept by how grey the
/// shift is and the shift's colour laid on by how far from grey it is (shift-diffuse: 19 shifts,
/// primaries, mixes, half tones and greys, at EnvironmentDiffuseScale 0-1 in quarters at noon and 0
/// and 1 at 6.3; dawn-tint at 6.1-6.5 and 17.6-17.7: mean error 0.5 % of the light against 10 % for
/// the glare's rule, which kept the light's colour and turned a red shift's green and blue to 0.69 at
/// 1 where Studio shows 0.22, and a blue shift at dawn to 0.36 of the yellow sun's blue for 0.91).
pub fn sun_tint(strength: Vec3, shift: Vec3, diffuse_scale: f32) -> Vec3 {
	let mix = diffuse_scale.clamp(0.0, 1.0);
	let squared = shift * shift;
	let deviation = squared - Vec3::splat(squared.element_sum() / 3.0);
	let spread = deviation.length();
	let brightness = (strength.length_squared() / 3.0).sqrt();
	let none = strength + SUN_SHIFT * brightness * deviation;
	let full = strength * (1.0 - SUN_SHIFT_KEEP * spread) + SUN_SHIFT_FULL * brightness * spread * squared;
	none.lerp(full, mix).max(Vec3::ZERO)
}

/// [`sun_tint`]'s figures: the shift's weight at EnvironmentDiffuseScale 0, and at 1 the light's own
/// colour's loss and the shift colour's weight by the shift's distance from grey.
const SUN_SHIFT: f32 = 1.73;
const SUN_SHIFT_KEEP: f32 = 0.955;
const SUN_SHIFT_FULL: f32 = 1.69;

/// [`AtmosphereSky`] as the sky pass reads it: haze (τ0, b, floor, 1 when on), the haze colour, the
/// light along +X, -X, +Y, -Y, +Z, -Z, the glare, its exponents, the sun's direction.
#[derive(Clone, Copy, Debug, Default)]
pub struct AtmosphereParams {
	pub haze: [f32; 4],
	pub color: [f32; 4],
	pub light: [[f32; 4]; 6],
	pub glare: [f32; 4],
	pub decay: [f32; 4],
	pub sun: [f32; 4],
}

/// The Atmosphere's sky: what Studio draws behind everything, and what its reflections and the
/// light distant objects fade into are made from (atmosphere-light: the sky specular changes with
/// the haze, the sky diffuse does not).
#[derive(Clone, Copy, Debug)]
pub struct AtmosphereSky {
	/// The haze profile (τ0, b, floor) for the Haze; None without haze
	haze: Option<(f32, f32, f32)>,
	/// Color², the haze's tint of the sky light
	color: Vec3,
	/// The skybox's ambient cube times the haze light scale: +X, -X, +Y, -Y, +Z, -Z
	light: [Vec3; 6],
	/// The glare's radiance at its peak, per channel
	glare: Vec3,
	/// The glare lobe's exponent per channel (from Decay)
	decay: Vec3,
	sun: Vec3,
}

impl AtmosphereSky {
	/// `light` is the skybox's ambient cube ([`CubeImage::ambient_cube`]), `glare` [`glare_light`]'s
	/// direction and share, `brightness` Lighting.Brightness and `glare_tint` [`glare_tint`]'s.
	pub fn new(atmosphere: &crate::scene::environment::Atmosphere, light: [Vec3; 6], (towards, share): (Vec3, Vec3), brightness: f32, glare_tint: Vec3) -> Self {
		let haze = (atmosphere.haze > 0.0).then(|| haze_profile(atmosphere.haze));
		Self {
			haze,
			color: atmosphere.color * atmosphere.color,
			light: light.map(|value| value * HAZE_LIGHT_SCALE),
			glare: glare_tint * share * (GLARE_SCALE * brightness.max(0.0) * atmosphere.glare.max(0.0)),
			decay: atmosphere.decay.map(glare_exponent),
			sun: towards,
		}
	}

	/// What the haze of `drawn`, the same Atmosphere under another sky light, shows over this one's
	/// per cube face (+X, -X, +Y, -Y, +Z, -Z), the haze's share along a direction still to apply
	/// ([`Self::haze_cube`]).
	pub fn light_over(&self, drawn: &AtmosphereSky) -> [Vec3; 6] {
		std::array::from_fn(|face| self.color * (drawn.light[face] - self.light[face]))
	}

	/// The haze's share of the sky ([`Self::haze`]) per direction, in every channel.
	pub fn haze_cube(&self, size: u32) -> CubeImage {
		cube_from(size, |_, _, direction| Vec3::splat(self.haze(direction)))
	}

	/// The share of the skybox the haze hides looking along `direction`.
	pub fn haze(&self, direction: Vec3) -> f32 {
		let Some((tau, exponent, floor)) = self.haze else {
			return 0.0;
		};
		if direction.y <= 0.0 {
			return 1.0;
		}
		1.0 - floor * (-tau * direction.y.powf(-exponent)).exp()
	}

	/// These parameters for the sky pass, which draws the same [`Self::sky`] from the skybox's images
	/// (shaders/sky.wgsl `through_atmosphere`).
	pub fn shader_params(&self) -> AtmosphereParams {
		let (tau, exponent, floor) = self.haze.unwrap_or((0.0, 0.0, 0.0));
		AtmosphereParams {
			haze: [tau, exponent, floor, if self.haze.is_some() { 1.0 } else { 0.0 }],
			color: self.color.extend(0.0).to_array(),
			light: self.light.map(|light| light.extend(0.0).to_array()),
			glare: self.glare.extend(0.0).to_array(),
			decay: self.decay.extend(0.0).to_array(),
			sun: self.sun.extend(0.0).to_array(),
		}
	}

	/// The sky along `direction` (unit) whose skybox there is `skybox`.
	pub fn sky(&self, direction: Vec3, skybox: Vec3) -> Vec3 {
		let haze = self.haze(direction);
		if haze <= 0.0 {
			return skybox;
		}
		let n2 = direction * direction;
		let axis = |positive: bool, plus: usize| if positive { self.light[plus] } else { self.light[plus + 1] };
		let light = (axis(direction.x > 0.0, 0) * n2.x + axis(direction.y > 0.0, 2) * n2.y + axis(direction.z > 0.0, 4) * n2.z) / (n2.x + n2.y + n2.z).max(1e-8);
		let lobe = (0.5 + 0.5 * direction.dot(self.sun)).max(0.0);
		let glare = self.glare * Vec3::new(lobe.powf(self.decay.x), lobe.powf(self.decay.y), lobe.powf(self.decay.z));
		// the haze takes over from the skybox, its glare grows with its square (atmosphere-sky glare
		// runs above the horizon: the glare's share follows h², the haze colour h)
		skybox * (1.0 - haze) + self.color * light * haze + glare * (haze * haze)
	}
}

/// (τ0, b, floor) of [`HAZE_PROFILE`] at a Haze.
fn haze_profile(haze: f32) -> (f32, f32, f32) {
	let first = HAZE_PROFILE[0];
	if haze <= first.0 {
		return (first.1, first.2, first.3);
	}
	for pair in HAZE_PROFILE.windows(2) {
		let ((h0, t0, b0, f0), (h1, t1, b1, f1)) = (pair[0], pair[1]);
		if haze <= h1 {
			let t = (haze - h0) / (h1 - h0);
			return (t0 + (t1 - t0) * t, b0 + (b1 - b0) * t, f0 + (f1 - f0) * t);
		}
	}
	let last = HAZE_PROFILE[HAZE_PROFILE.len() - 1];
	(last.1, last.2, last.3)
}

/// The glare lobe's exponent for a channel's Decay (atmosphere-glare: fitted per channel over Decay
/// 0-1 within ~5 %).
fn glare_exponent(decay: f32) -> f32 {
	0.705 * ((3.785 * (1.0 - decay.clamp(0.0, 1.0))).exp() - 1.0)
}

/// The skybox cube seen through the Atmosphere ([`AtmosphereSky::sky`] per texel).
pub fn atmosphere_cube(sky: &CubeImage, atmosphere: &AtmosphereSky) -> CubeImage {
	cube_from(sky.size, |face, index, direction| atmosphere.sky(direction, sky.faces[face][index]))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn viewport_frame_light() {
		// the match place's cards: LightColor 175 gray along (1, 1, 0.5), Ambient 210 gray
		let lighting = crate::scene::environment::ViewportLighting { ambient: Vec3::splat(210.0 / 255.0), light_color: Vec3::splat(175.0 / 255.0), light_direction: Vec3::new(1.0, 1.0, 0.5), sky: None };
		let light = viewport_light(&lighting);
		// towards where the light comes from: against the way LightDirection says it travels
		assert!((light.direction + Vec3::new(1.0, 1.0, 0.5).normalize()).length() < 1e-6, "{:?}", light.direction);
		assert!((light.strength - Vec3::splat((175.0f32 / 255.0).powi(2) * VIEWPORT_LIGHT)).abs().max_element() < 1e-6);
		assert!((viewport_ambient(&lighting) - Vec3::splat((210.0f32 / 255.0).powi(2) * VIEWPORT_AMBIENT)).abs().max_element() < 1e-6);
		// a LightDirection of zero length: straight down
		let down = viewport_light(&crate::scene::environment::ViewportLighting { light_direction: Vec3::ZERO, ..lighting });
		assert_eq!(down.direction, Vec3::Y);
	}

	#[test]
	fn prefiltering_cubes_together_changes_no_texel() {
		// the chain as it was filtered: face by face, one cube at a time, each sample turned anew
		fn one_at_a_time(source: &CubeImage, levels: u32) -> Vec<CubeImage> {
			let mut chain = vec![CubeImage { size: source.size, faces: source.faces.clone().map(|face| face.into_iter().map(stored).collect()) }];
			for level in 1..levels {
				let roughness = level as f32 / (levels - 1) as f32;
				let previous = &chain[level as usize - 1];
				let size = (previous.size / 2).max(1);
				let faces: [Vec<Vec3>; 6] = std::array::from_fn(|face| {
					let mut pixels = Vec::new();
					for y in 0..size {
						for x in 0..size {
							let normal = cube_direction(face, (x as f32 + 0.5) / size as f32, (y as f32 + 0.5) / size as f32);
							let (mut sum, mut weight) = (Vec3::ZERO, 0.0);
							for index in 0..128 {
								let h = importance_sample_ggx(hammersley(index, 128), roughness, normal);
								let l = 2.0 * normal.dot(h) * h - normal;
								let n_dot_l = normal.dot(l);
								if n_dot_l > 0.0 {
									sum += previous.sample_bilinear(l) * n_dot_l;
									weight += n_dot_l;
								}
							}
							pixels.push(stored(if weight > 0.0 { sum / weight } else { Vec3::ZERO }));
						}
					}
					pixels
				});
				chain.push(CubeImage { size, faces });
			}
			chain
		}
		let sky = gradient_cube(16, Vec3::new(0.2, 0.4, 0.9), Vec3::new(0.6, 0.7, 0.9), Vec3::new(0.3, 0.3, 0.2));
		let haze = cube_from(16, |_, _, direction| Vec3::splat(direction.y.max(0.0).powf(0.7)));
		let [sky_chain, haze_chain] = prefilter_specular_all([&sky, &haze], 4);
		for (together, alone) in [(sky_chain, one_at_a_time(&sky, 4)), (haze_chain, one_at_a_time(&haze, 4))] {
			assert_eq!(together.len(), alone.len());
			for (a, b) in together.iter().zip(&alone) {
				assert_eq!((a.size, &a.faces), (b.size, &b.faces));
			}
		}
	}

	#[test]
	fn split_sum_matches_studio_table() {
		// Studio's brdfLUT.dds at texel centres (row n·v, column roughness): (roughness, n·v, scale, bias)
		let samples = [
			(0.501953, 0.251953, 0.7554, 0.0798),
			(0.998047, 0.998047, 0.3096, 0.0),
			(0.001953, 0.501953, 0.9692, 0.0306),
			(0.751953, 0.064453, 0.8320, 0.0611),
			(0.251953, 0.783203, 0.9922, 0.0008),
			(0.126953, 0.126953, 0.5005, 0.4895),
		];
		for (roughness, n_dot_v, scale, bias) in samples {
			let [a, b] = split_sum_brdf(roughness, n_dot_v, 4096);
			assert!((a - scale).abs() < 0.006 && (b - bias).abs() < 0.006, "roughness {roughness} n·v {n_dot_v}: {a} {b} vs {scale} {bias}");
		}
	}

	#[test]
	fn sun_matches_studio_samples() {
		// latitude, clock time, Studio's GetSunDirection()
		let samples = [
			(0.0, 14.5, Vec3::new(-0.558276, 0.727560, -0.398729)),
			(41.7333, 14.5, Vec3::new(-0.578192, 0.753514, 0.312908)),
			(-30.0, 3.0, Vec3::new(0.420604, -0.420604, -0.803856)),
			(90.0, 21.0, Vec3::new(-0.281928, -0.281928, 0.917079)),
			(23.5, 6.0, Vec3::new(1.0, 0.0, 0.000004)),
		];
		for (latitude, time, expected) in samples {
			let direction = sun_direction(time, latitude);
			assert!(direction.distance(expected) < 2e-4, "{latitude} {time}: {direction} vs {expected}");
		}
	}
}
