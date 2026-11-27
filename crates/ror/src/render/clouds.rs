//! Terrain Clouds: the sky pass marches each sky pixel's ray through the cloud volumes of the user's
//! Studio install, the shape volume `content/sky/clouds.dds` (256 x 256 x 32, its mips by distance)
//! eaten by the detail volume `cloudDetail3D.dds` (64³), cut where Cover puts its threshold, lit by the
//! sun, the sky and the flat ambient, and laid over the sky; they also dim the sun on what is near the
//! camera ([`sun_cover`]).
//! This build draws them with a generic model (clouds.wgsl, [`generic`]; docs/rendering.md
//! "Clouds"), not calibrated against Studio.

use crate::scene::environment::{Atmosphere, Clouds};

/// The Covers whose thresholds were measured ([`Tuning::thresholds`]), with 0 and 1 at the ends.
const COVERS: [f32; 5] = [0.3, 0.45, 0.6, 0.75, 0.9];

/// The shape field's threshold by Cover, in between linearly.
pub fn threshold(cover: f32) -> f32 {
	let measured = tuned().thresholds;
	let mut table = vec![(0.0, 0.0)];
	table.extend(COVERS.iter().copied().zip(measured));
	table.push((1.0, 1.0));
	let cover = cover.clamp(0.0, 1.0);
	let upper = table.iter().position(|&(at, _)| at >= cover).unwrap_or(table.len() - 1).max(1);
	let ((x0, y0), (x1, y1)) = (table[upper - 1], table[upper]);
	y0 + (y1 - y0) * (cover - x0) / (x1 - x0)
}

/// The march and how the clouds are lit and laid over the sky (the sky pass's `clouds` and
/// `over_clouds`). `ROR_TUNE_CLOUDS="key=value,..."` sets them for fits (the keys are the fields' names).
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
	/// The march's steps through the layer
	pub steps: f32,
	/// The detail volume's amplitude against the shape field's values
	pub detail: f32,
	/// The detail volume's scale: its texture coordinate per unit of the clouds' space
	pub detail_scale: f32,
	/// Where the march's samples fall along the ray (a share of the way in)
	pub jitter: f32,
	/// The cover under which a pixel shows the sky alone
	pub cutoff: f32,
	/// How far the clouds' own √ radiance is weighed by the square of their cover as the sky's is
	/// (1, plain alpha blending) rather than added as it is (0)
	pub blend: f32,
	/// The sun's light's weight
	pub sun: f32,
	/// The sky light's weight
	pub sky: f32,
	/// The flat ambient's weight
	pub ambient: f32,
	/// The optical depth towards the sun's scale
	pub shadow: f32,
	/// The sky light's gray at the layer's bottom (rising 0.3 through it) while the sun is up
	pub sky_floor: f32,
	/// The sky light's gray once the sun is down
	pub sky_low: f32,
	/// The sun light scattered on however deep the point
	pub multiple: f32,
	/// Color's exponent
	pub power: f32,
	/// The threshold at Cover 0.3, 0.45, 0.6, 0.75 and 0.9 (clouds and clouds-horizon: each Cover's
	/// clouded share of the views against Studio's)
	pub thresholds: [f32; 5],
	/// The extinction's scale (Density's own scale)
	pub extinction: f32,
	/// Density's exponent
	pub density_power: f32,
}

mod generic;
pub use generic::{TUNING, sun_cover};


fn tuned() -> Tuning {
	static TUNED: std::sync::OnceLock<Tuning> = std::sync::OnceLock::new();
	*TUNED.get_or_init(|| {
		let mut tuning = TUNING;
		for pair in std::env::var("ROR_TUNE_CLOUDS").unwrap_or_default().split(',') {
			let Some((key, value)) = pair.split_once('=') else {
				continue;
			};
			let Ok(value) = value.trim().parse::<f32>() else {
				continue;
			};
			let field = match key.trim() {
				"steps" => &mut tuning.steps,
				"detail" => &mut tuning.detail,
				"detail_scale" => &mut tuning.detail_scale,
				"jitter" => &mut tuning.jitter,
				"cutoff" => &mut tuning.cutoff,
				"blend" => &mut tuning.blend,
				"sun" => &mut tuning.sun,
				"sky" => &mut tuning.sky,
				"ambient" => &mut tuning.ambient,
				"shadow" => &mut tuning.shadow,
				"sky_floor" => &mut tuning.sky_floor,
				"sky_low" => &mut tuning.sky_low,
				"multiple" => &mut tuning.multiple,
				"power" => &mut tuning.power,
				"t30" => &mut tuning.thresholds[0],
				"t45" => &mut tuning.thresholds[1],
				"t60" => &mut tuning.thresholds[2],
				"t75" => &mut tuning.thresholds[3],
				"t90" => &mut tuning.thresholds[4],
				"extinction" => &mut tuning.extinction,
				"density_power" => &mut tuning.density_power,
				_ => continue,
			};
			*field = value;
		}
		tuning
	})
}

/// The sky pass's first six rows of cloud parameters at level 21 without an Atmosphere
/// ([`sky_uniform`]'s): `clouds` (x 1 when drawn, y the threshold, z Density, w the steps),
/// `cloud_color` (Color to [`Tuning::power`], w the detail's amplitude), `cloud_shape_params` (the
/// detail's scale, the march's start, the cutoff, the blend), `cloud_light` (sun, sky, ambient, shadow),
/// `cloud_sky` (the sky light's floor and low gray, the multiple scattering) and `cloud_spare`.
pub fn uniform(clouds: Option<&Clouds>) -> [[f32; 4]; 6] {
	let rows = sky_uniform(clouds, 3, 0.0, None);
	std::array::from_fn(|row| rows[row])
}

/// The sky pass's cloud parameters (sky.wgsl `SkyParams`): [`uniform`]'s rows, `cloud_spare` x the
/// most of their exposed radiance laid over the sky, y the look (crate::quality::Quality::clouds), z
/// and w what an Atmosphere leaves of them ([`atmosphere_fade`]), then the flat layer's three rows
/// (`cloud_flat`, `cloud_flat_sun`, `cloud_flat_rim`; zeros where it is not drawn). `look` is the view's
/// graphics quality's (3 at level 21), `pixel` the angle one texel of the clouds' buffer spans, in
/// radians.
pub(crate) fn sky_uniform(clouds: Option<&Clouds>, look: u8, pixel: f32, atmosphere: Option<&Atmosphere>) -> [[f32; 4]; 9] {
	let Some(clouds) = clouds else {
		return [[0.0; 4]; 9];
	};
	let tuning = tuned();
	let look = look.min(3);
	let density = clouds.density.clamp(0.0, 1.0).powf(tuning.density_power);
	let steps = tuning.steps.round().max(1.0);
	let color = clouds.color.max(glam::Vec3::ZERO).powf(tuning.power);
	let flat = flat_uniform(clouds, look, density, pixel);
	let [fade, fade_power] = atmosphere_fade(atmosphere);
	[
		[1.0, threshold(clouds.cover), density, steps],
		[color.x, color.y, color.z, tuning.detail],
		[tuning.detail_scale, tuning.jitter, tuning.cutoff, tuning.blend],
		[tuning.sun, tuning.sky, tuning.ambient, tuning.shadow],
		[tuning.sky_floor, tuning.sky_low, tuning.multiple, tuning.extinction],
		[extra().ceiling, look as f32, fade, fade_power],
		flat[0],
		flat[1],
		flat[2],
	]
}

/// What an Atmosphere leaves of the clouds along a direction `e` above the horizon: exp(-c sin(e)^-b),
/// [c, b]; [0, 0] without one. A Roblox client (0.747, level 21, Offset 0-0.5, Haze 0; notes/render.md
/// "Clouds"): none faded at Density 0.1, half their contrast against the hazed sky about 4 degrees up
/// at 0.3, 10 at 0.45, 17 at 0.6, 28 at 0.8: c = 0.544 max(Density - 0.08, 0)^3.4, b 1.95 (rms 0.024
/// over five Densities and eleven elevations). Offset 1 fades less and Haze 1 a little less (not
/// followed). The lowest looks keep 0.3 + 0.7 of it (sky.wgsl `fs_sky`).
pub(crate) fn atmosphere_fade(atmosphere: Option<&Atmosphere>) -> [f32; 2] {
	let Some(atmosphere) = atmosphere else {
		return [0.0, 0.0];
	};
	[0.544 * (atmosphere.density - 0.08).max(0.0).powf(3.4), 1.95]
}

/// How many of a view's pixels (each way, its output pixels whatever the supersampling) one texel of the
/// clouds' buffer stands for: the march runs once a texel, blurred there by a 3 x 3 tent and read
/// through a second one (sky.wgsl `fs_clouds`, `fs_cloud_blur`, `buffered_clouds`), so their edges are
/// as soft in pixels at any view size. `ROR_TUNE_CLOUDS="buffer=N"` sets it for fits.
const BUFFER_SCALE: f32 = 4.0;
/// The most of their exposed radiance the clouds lay over the sky ([`Extra::ceiling`]): far above
/// what they reach (a client's sunlit clouds pass radiance 2, white, where a ceiling of 1 drew them gray)
const CEILING: f32 = 16.0;

/// The clouds' buffer's size for a target of `render_width` x `render_height` drawn at `supersample`.
pub fn buffer_size(render_width: u32, render_height: u32, supersample: u32) -> (u32, u32) {
	let scale = (extra().buffer.max(1.0) * supersample.max(1) as f32).round() as u32;
	(render_width.div_ceil(scale).max(1), render_height.div_ceil(scale).max(1))
}

/// Whether a view at a look below 2 lays the volume's 2D stand-in flat ([`load_flat`]).
pub(crate) fn draws_flat(look: u8) -> bool {
	{ let _ = look; false }
}


/// The flat layer's three rows of the uniform: (threshold, steepness · Density, the buffer texel's
/// angle, 1 when drawn), (A, B, C, the detail's amplitude), (R, Q, K, 0); zeros for the march's looks.
fn flat_uniform(clouds: &Clouds, look: u8, density: f32, pixel: f32) -> [[f32; 4]; 3] {
	let _ = (clouds, look, density, pixel);
	[[0.0; 4]; 3]
}

/// Figures beyond [`Tuning`]'s, set the same way (`ROR_TUNE_CLOUDS`, by name).
#[derive(Clone, Copy, Debug)]
struct Extra {
	buffer: f32,
	/// The most of the clouds' exposed radiance laid over the sky
	ceiling: f32,
}

fn extra() -> Extra {
	static EXTRA: std::sync::OnceLock<Extra> = std::sync::OnceLock::new();
	*EXTRA.get_or_init(|| {
		let mut extra = Extra {
			buffer: BUFFER_SCALE,
			ceiling: CEILING,
		};
		for pair in std::env::var("ROR_TUNE_CLOUDS").unwrap_or_default().split(',') {
			let Some((key, value)) = pair.split_once('=') else {
				continue;
			};
			let Ok(value) = value.trim().parse::<f32>() else {
				continue;
			};
			match key.trim() {
				"buffer" => extra.buffer = value,
				"ceiling" => extra.ceiling = value,
				_ => continue,
			}
		}
		extra
	})
}

/// A DX10 DDS file's R8 3D texture: its size and each mip level's bytes.
fn read_volume(bytes: &[u8]) -> Option<([u32; 3], Vec<Vec<u8>>)> {
	read_r8(bytes, 4)
}

/// A DX10 DDS file's R8 texture of a resource dimension (3: 2D, 4: 3D): its size and each mip level's
/// bytes.
fn read_r8(bytes: &[u8], dimension: u32) -> Option<([u32; 3], Vec<Vec<u8>>)> {
	if bytes.len() < 148 || &bytes[0..4] != b"DDS " || &bytes[84..88] != b"DX10" {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let (height, width, depth, levels) = (word(12), word(16), word(24).max(1), word(28).max(1));
	// DXGI_FORMAT_R8_UNORM, a resource of that dimension
	if word(128) != 61 || word(132) != dimension {
		return None;
	}
	let mut at = 148;
	let mut out = Vec::new();
	for level in 0..levels {
		let size = [(width >> level).max(1), (height >> level).max(1), (depth >> level).max(1)];
		let length = (size[0] * size[1] * size[2]) as usize;
		out.push(bytes.get(at..at + length)?.to_vec());
		at += length;
	}
	Some(([width, height, depth], out))
}

/// A volume and its mips as an R8 3D texture.
fn upload(gpu: &super::gpu::Gpu, label: &str, size: [u32; 3], levels: &[Vec<u8>]) -> wgpu::TextureView {
	upload_r8(gpu, label, size, levels, wgpu::TextureDimension::D3)
}

/// A texture and its mips as an R8 texture of `dimension` (a 2D one's depth 1).
fn upload_r8(gpu: &super::gpu::Gpu, label: &str, [width, height, depth]: [u32; 3], levels: &[Vec<u8>], dimension: wgpu::TextureDimension) -> wgpu::TextureView {
	let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
		label: Some(label),
		size: wgpu::Extent3d { width, height, depth_or_array_layers: depth },
		mip_level_count: levels.len() as u32,
		sample_count: 1,
		dimension,
		format: wgpu::TextureFormat::R8Unorm,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	for (level, texels) in levels.iter().enumerate() {
		let size = [(width >> level).max(1), (height >> level).max(1), (depth >> level).max(1)];
		gpu.queue.write_texture(
			wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
			texels,
			wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0]), rows_per_image: Some(size[1]) },
			wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: size[2] },
		);
	}
	texture.create_view(&Default::default())
}

/// Studio's cloud volumes, the shape's and the detail's.
pub struct Volumes {
	pub shape: wgpu::TextureView,
	pub detail: wgpu::TextureView,
}

fn load_volume(gpu: &super::gpu::Gpu, assets: &ror_assets::Assets, path: &str) -> Result<wgpu::TextureView, String> {
	let file = assets.studio_file(path).ok_or_else(|| format!("rbxasset://{path}: no Studio install has it"))?;
	let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
	let (size, levels) = read_volume(&bytes).ok_or_else(|| format!("{}: not an R8 3D DDS", file.display()))?;
	Ok(upload(gpu, path, size, &levels))
}

/// Studio's cloud volumes from its install, with their mips, or why not.
pub fn load(gpu: &super::gpu::Gpu, assets: &ror_assets::Assets) -> Result<Volumes, String> {
	Ok(Volumes { shape: load_volume(gpu, assets, "sky/clouds.dds")?, detail: load_volume(gpu, assets, "sky/cloudDetail3D.dds")? })
}

/// A DDS file's BC1 (DXT1) texture's red channel, each mip level decoded: its size and the levels'
/// bytes.
fn read_bc1_red(bytes: &[u8]) -> Option<([u32; 3], Vec<Vec<u8>>)> {
	if bytes.len() < 128 || &bytes[0..4] != b"DDS " || &bytes[84..88] != b"DXT1" {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let (height, width, levels) = (word(12), word(16), word(28).max(1));
	let mut at = 128;
	let mut out = Vec::new();
	for level in 0..levels {
		let (w, h) = ((width >> level).max(1), (height >> level).max(1));
		let mut texels = vec![0u8; (w * h) as usize];
		for by in 0..h.div_ceil(4) {
			for bx in 0..w.div_ceil(4) {
				let block = bytes.get(at..at + 8)?;
				at += 8;
				let (c0, c1) = (u16::from_le_bytes([block[0], block[1]]), u16::from_le_bytes([block[2], block[3]]));
				let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
				let red = |c: u16| ((c >> 11) & 31) as u32 * 255 / 31;
				let (r0, r1) = (red(c0), red(c1));
				let palette = if c0 > c1 { [r0, r1, (2 * r0 + r1) / 3, (r0 + 2 * r1) / 3] } else { [r0, r1, (r0 + r1) / 2, 0] };
				for index in 0..16u32 {
					let (x, y) = (bx * 4 + index % 4, by * 4 + index / 4);
					if x < w && y < h {
						texels[(y * w + x) as usize] = palette[((bits >> (2 * index)) & 3) as usize] as u8;
					}
				}
			}
		}
		out.push(texels);
	}
	Some(([width, height, 1], out))
}

/// The 2D detail the flat layer's value takes (`sky/cloudDetail.dds`, BC1, gray: its red) from the
/// Studio install, with its mips, or why not.
pub(crate) fn load_flat_detail(gpu: &super::gpu::Gpu, assets: &ror_assets::Assets) -> Result<wgpu::TextureView, String> {
	let path = "sky/cloudDetail.dds";
	let file = assets.studio_file(path).ok_or_else(|| format!("rbxasset://{path}: no Studio install has it"))?;
	let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
	let (size, levels) = read_bc1_red(&bytes).ok_or_else(|| format!("{}: not a BC1 DDS", file.display()))?;
	Ok(upload_r8(gpu, path, size, &levels, wgpu::TextureDimension::D2))
}

/// The volume's 2D stand-in from the Studio install (`sky/cloudsfb.dds`, with its mips), which the two
/// lowest looks lay flat ([`Flat`]), or why not.
pub(crate) fn load_flat(gpu: &super::gpu::Gpu, assets: &ror_assets::Assets) -> Result<wgpu::TextureView, String> {
	let path = "sky/cloudsfb.dds";
	let file = assets.studio_file(path).ok_or_else(|| format!("rbxasset://{path}: no Studio install has it"))?;
	let bytes = std::fs::read(&file).map_err(|error| format!("{}: {error}", file.display()))?;
	let (size, levels) = read_r8(&bytes, 3).ok_or_else(|| format!("{}: not an R8 2D DDS", file.display()))?;
	Ok(upload_r8(gpu, path, size, &levels, wgpu::TextureDimension::D2))
}

/// A volume's first level on the CPU: its size (x, y, z) and texels, x fastest.
struct CpuVolume {
	size: [usize; 3],
	texels: Vec<u8>,
}

impl CpuVolume {
	fn read(assets: &ror_assets::Assets, path: &str) -> Option<Self> {
		let bytes = std::fs::read(assets.studio_file(path)?).ok()?;
		let ([width, height, depth], mut levels) = read_volume(&bytes)?;
		Some(Self { size: [width as usize, height as usize, depth as usize], texels: levels.swap_remove(0) })
	}

	/// Trilinear, repeating every way, texel centres at (i + 0.5) / n, as the sampler reads it.
	fn sample(&self, at: glam::Vec3) -> f32 {
		let [nx, ny, nz] = self.size;
		let x = at.x * nx as f32 - 0.5;
		let y = at.y * ny as f32 - 0.5;
		let z = at.z * nz as f32 - 0.5;
		let (x0, y0, z0) = (x.floor(), y.floor(), z.floor());
		let (fx, fy, fz) = (x - x0, y - y0, z - z0);
		let wrap = |value: f32, n: usize| (value as i64).rem_euclid(n as i64) as usize;
		let mut sum = 0.0;
		for (dz, wz) in [(0.0, 1.0 - fz), (1.0, fz)] {
			for (dy, wy) in [(0.0, 1.0 - fy), (1.0, fy)] {
				for (dx, wx) in [(0.0, 1.0 - fx), (1.0, fx)] {
					let index = (wrap(z0 + dz, nz) * ny + wrap(y0 + dy, ny)) * nx + wrap(x0 + dx, nx);
					sum += wx * wy * wz * self.texels[index] as f32;
				}
			}
		}
		sum / 255.0
	}
}

/// The shape and detail volumes on the CPU, read once.
fn cpu_volumes(assets: &ror_assets::Assets) -> Option<&'static (CpuVolume, CpuVolume)> {
	static VOLUMES: std::sync::OnceLock<Option<(CpuVolume, CpuVolume)>> = std::sync::OnceLock::new();
	VOLUMES.get_or_init(|| Some((CpuVolume::read(assets, "sky/clouds.dds")?, CpuVolume::read(assets, "sky/cloudDetail3D.dds")?))).as_ref()
}


/// A 1 x 1 x 1 stand-in: no clouds drawn.
pub fn empty(gpu: &super::gpu::Gpu) -> wgpu::TextureView {
	upload(gpu, "no clouds", [1, 1, 1], &[vec![255]])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_buffer_is_a_quarter_of_the_view_each_way() {
		assert_eq!(buffer_size(1600, 900, 1), (400, 225));
		assert_eq!(buffer_size(1601, 902, 1), (401, 226), "a part texel covers the edge");
		assert_eq!(buffer_size(3200, 1800, 2), (400, 225), "the output's pixels, whatever the supersampling");
		assert_eq!(buffer_size(1, 1, 1), (1, 1));
	}

	#[test]
	fn reads_a_2d_r8_texture_and_a_bc1_one() {
		let mut bytes = vec![0u8; 148];
		bytes[0..4].copy_from_slice(b"DDS ");
		bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
		bytes[16..20].copy_from_slice(&2u32.to_le_bytes());
		bytes[28..32].copy_from_slice(&2u32.to_le_bytes());
		bytes[84..88].copy_from_slice(b"DX10");
		bytes[128..132].copy_from_slice(&61u32.to_le_bytes());
		bytes[132..136].copy_from_slice(&3u32.to_le_bytes());
		bytes.extend([10, 20, 30, 40, 25]);
		let (size, levels) = read_r8(&bytes, 3).unwrap();
		assert_eq!((size, levels), ([2, 2, 1], vec![vec![10, 20, 30, 40], vec![25]]));
		assert!(read_volume(&bytes).is_none(), "a 2D texture is not a volume");
		// one 4 x 4 BC1 block: colour 0 red 31 (255), colour 1 red 0, indices 0, 1, 2, 3 along each row
		let mut bc1 = vec![0u8; 128];
		bc1[0..4].copy_from_slice(b"DDS ");
		bc1[12..16].copy_from_slice(&4u32.to_le_bytes());
		bc1[16..20].copy_from_slice(&4u32.to_le_bytes());
		bc1[28..32].copy_from_slice(&1u32.to_le_bytes());
		bc1[84..88].copy_from_slice(b"DXT1");
		let indices: u32 = (0..16).map(|texel: u32| (texel % 4) << (2 * texel)).sum();
		bc1.extend(0xf800u16.to_le_bytes());
		bc1.extend(0u16.to_le_bytes());
		bc1.extend(indices.to_le_bytes());
		let (size, levels) = read_bc1_red(&bc1).unwrap();
		assert_eq!(size, [4, 4, 1]);
		assert_eq!(&levels[0][0..4], &[255, 0, 170, 85]);
		assert_eq!(&levels[0][12..16], &[255, 0, 170, 85]);
	}

	#[test]
	fn an_atmosphere_fades_the_clouds_by_its_density() {
		let atmosphere = |density: f32| Atmosphere { density, offset: 0.25, color: glam::Vec3::splat(0.78), decay: glam::Vec3::splat(0.44), glare: 0.0, haze: 0.0 };
		assert_eq!(atmosphere_fade(None), [0.0, 0.0]);
		assert_eq!(atmosphere_fade(Some(&atmosphere(0.05)))[0], 0.0, "too thin to fade them");
		// half their contrast about 4 degrees up at Density 0.3, 17 at 0.6 (a client's)
		let kept = |density: f32, degrees: f32| {
			let [c, b] = atmosphere_fade(Some(&atmosphere(density)));
			(-c * degrees.to_radians().sin().powf(-b)).exp()
		};
		assert!((kept(0.3, 4.0) - 0.5).abs() < 0.1 && (kept(0.6, 17.0) - 0.5).abs() < 0.1, "{} {}", kept(0.3, 4.0), kept(0.6, 17.0));
		assert!(kept(0.1, 2.0) > 0.98 && kept(0.8, 10.0) < 0.05);
	}

	#[test]
	fn uniform_keeps_its_six_rows() {
		let clouds = Clouds { cover: 0.6, density: 0.7, color: glam::Vec3::ONE };
		let rows = uniform(Some(&clouds));
		assert_eq!(rows[..], sky_uniform(Some(&clouds), 3, 0.0, None)[..6]);
		assert_eq!(rows[0][0], 1.0);
		assert_eq!(uniform(None), [[0.0; 4]; 6]);
	}


	#[test]
	fn cover_runs_through_the_measured_thresholds() {
		assert_eq!(threshold(0.0), 0.0);
		assert!((threshold(0.6) - TUNING.thresholds[2]).abs() < 1e-6);
		assert_eq!(threshold(2.0), 1.0);
		assert!(threshold(0.3) <= threshold(0.45), "the threshold rises with Cover");
	}

	#[test]
	fn reads_an_r8_volume_and_its_levels() {
		let mut bytes = vec![0u8; 148];
		bytes[0..4].copy_from_slice(b"DDS ");
		bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
		bytes[16..20].copy_from_slice(&4u32.to_le_bytes());
		bytes[24..28].copy_from_slice(&2u32.to_le_bytes());
		bytes[28..32].copy_from_slice(&2u32.to_le_bytes());
		bytes[84..88].copy_from_slice(b"DX10");
		bytes[128..132].copy_from_slice(&61u32.to_le_bytes());
		bytes[132..136].copy_from_slice(&4u32.to_le_bytes());
		bytes.extend((0..32 + 4).map(|value| value as u8));
		let (size, levels) = read_volume(&bytes).unwrap();
		assert_eq!(size, [4, 4, 2]);
		assert_eq!(levels.iter().map(Vec::len).collect::<Vec<_>>(), [32, 4]);
		assert_eq!(levels[1], [32, 33, 34, 35]);
		bytes.truncate(bytes.len() - 1);
		assert!(read_volume(&bytes).is_none(), "a level cut short");
	}

}
