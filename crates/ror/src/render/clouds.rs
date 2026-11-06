//! Terrain Clouds: the sky pass marches each sky pixel's ray through the cloud volumes of the user's
//! Studio install, the shape volume `content/sky/clouds.dds` (256 x 256 x 32, its mips by distance)
//! eaten by the detail volume `cloudDetail3D.dds` (64³), cut where Cover puts its threshold, lit by the
//! sun, the sky and the flat ambient, and laid over the sky; they also dim the sun on what is near the
//! camera ([`sun_cover`]).
//! This build draws them with a generic model (clouds.wgsl, [`generic`]; docs/rendering.md
//! "Clouds"), not calibrated against Studio.

use crate::scene::environment::Clouds;

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

/// The sky pass's cloud parameters (sky.wgsl `SkyParams`): `clouds` (x 1 when drawn, y the
/// threshold, z Density, w the steps), `cloud_color` (Color to [`Tuning::power`], w the detail's
/// amplitude), `cloud_shape_params` (the detail's scale, the march's start, the cutoff, the blend),
/// `cloud_light` (sun, sky, ambient, shadow) and `cloud_sky` (the sky light's floor and low gray, the
/// multiple scattering).
pub fn uniform(clouds: Option<&Clouds>) -> [[f32; 4]; 6] {
	let Some(clouds) = clouds else {
		return [[0.0; 4]; 6];
	};
	let tuning = tuned();
	let color = clouds.color.max(glam::Vec3::ZERO).powf(tuning.power);
	[
		[1.0, threshold(clouds.cover), clouds.density.clamp(0.0, 1.0).powf(tuning.density_power), tuning.steps.round().max(1.0)],
		[color.x, color.y, color.z, tuning.detail],
		[tuning.detail_scale, tuning.jitter, tuning.cutoff, tuning.blend],
		[tuning.sun, tuning.sky, tuning.ambient, tuning.shadow],
		[tuning.sky_floor, tuning.sky_low, tuning.multiple, tuning.extinction],
		[0.0; 4],
	]
}

/// A DX10 DDS file's R8 3D texture: its size and each mip level's bytes.
fn read_volume(bytes: &[u8]) -> Option<([u32; 3], Vec<Vec<u8>>)> {
	if bytes.len() < 148 || &bytes[0..4] != b"DDS " || &bytes[84..88] != b"DX10" {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let (height, width, depth, levels) = (word(12), word(16), word(24).max(1), word(28).max(1));
	// DXGI_FORMAT_R8_UNORM, a 3D resource
	if word(128) != 61 || word(132) != 4 {
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
fn upload(gpu: &super::gpu::Gpu, label: &str, [width, height, depth]: [u32; 3], levels: &[Vec<u8>]) -> wgpu::TextureView {
	let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
		label: Some(label),
		size: wgpu::Extent3d { width, height, depth_or_array_layers: depth },
		mip_level_count: levels.len() as u32,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D3,
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
