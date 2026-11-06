// The sky light's ambient cube (render::environment::CubeImage::ambient_cube) of the default sky
// built from each of the skybox images' mip levels (box-filtered to 4 x 4 a face when larger), to
// compare with Studio's (tools/analysis/sky_cube.py on the sky-kernel experiment):
//   cargo run -p ror --release --example tools -- sky_cube_mips
use glam::Vec3;
use ror::render::environment::{SkyFace, SkyPixels, sky_cube};

const FACES: [&str; 6] = ["bk", "dn", "ft", "lf", "rt", "up"];

pub fn main() -> anyhow::Result<()> {
	let assets = ror_assets::Assets::new(ror_assets::AssetConfig { repo_dir: Some(std::env::current_dir()?), ..ror_assets::AssetConfig::new(ror::output::state_dir()) });
	let mut images = Vec::new();
	for face in FACES {
		let uri = format!("rbxasset://textures/sky/sky512_{face}.tex");
		let fetched = assets.fetch(&uri).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		let image = ror_image::decode(&fetched.bytes).map_err(|error| anyhow::anyhow!("{uri}: {error}"))?;
		images.push(ror_image::to_rgba8(&image));
	}
	let levels = images[0].levels.len();
	for level in 0..levels {
		let faces: Vec<SkyFace> = images
			.iter()
			.map(|image| {
				let (width, height, _) = image.level_size(level);
				let pixels = image.levels[level].chunks_exact(4).map(|px| Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32) / 255.0).map(|c| c * c * 1.935 * 1.044).collect();
				SkyFace { width, height, pixels: SkyPixels::Radiance(pixels) }
			})
			.collect();
		let (width, _, _) = images[0].level_size(level);
		let faces: [SkyFace; 6] = faces.try_into().map_err(|_| anyhow::anyhow!("six faces"))?;
		let mut cube = sky_cube(&faces, width.max(4), Vec3::ZERO);
		while cube.size > 4 {
			cube = cube.downsample();
		}
		let light = cube.ambient_cube();
		println!("level {level} ({width}): +x {:.4?} -x {:.4?} +y {:.4?} -y {:.4?} +z {:.4?} -z {:.4?}", light[0].to_array(), light[1].to_array(), light[2].to_array(), light[3].to_array(), light[4].to_array(), light[5].to_array());
	}
	Ok(())
}
