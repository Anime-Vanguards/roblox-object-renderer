//! The tone curve's dark end and the fog Roblox keeps at any FogEnd, against a Roblox 0.747 client's
//! frames (fixtures/tone-bloom.luau; notes/render.md "Colour pipeline and tone curve", "Fog"): gray
//! SmoothPlastic tiles under a flat Ambient of 1 alone, and a black wall 60 studs away.

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::Environment;
use crate::scene::environment::PostEffect;

struct Tone {
	renderer: Renderer,
	state: std::path::PathBuf,
}

impl Tone {
	fn new() -> Self {
		let state = std::env::temp_dir().join(format!("ror-tone-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), state }
	}

	/// `scene` seen from 60 studs along +Z at the fixture's view (40 degrees, 1644 x 1116), and the mean
	/// level (0-255) of the `side` x `side` pixels round each point.
	fn levels(&mut self, scene: &Scene, target: Vec3, points: &[Vec3], side: i32) -> Vec<f32> {
		let view = ViewRequest {
			width: 1644,
			height: 1116,
			camera: CameraSpec::LookAt { eye: (target - Vec3::Z * 60.0).into(), target: target.into(), fov: 40.0 },
			background: Background::Sky,
			..Default::default()
		};
		let (images, _) = self.renderer.render(scene, std::slice::from_ref(&view)).expect("a render");
		let image = &images[0];
		let camera = view.camera.place(None, view.width, view.height);
		points
			.iter()
			.map(|point| {
				let clip = camera.view_projection(view.width as f32 / view.height as f32) * point.extend(1.0);
				let (x, y) = (((clip.x / clip.w) * 0.5 + 0.5) * view.width as f32, (0.5 - (clip.y / clip.w) * 0.5) * view.height as f32);
				let mut sum = 0.0;
				for dy in -(side / 2)..=side / 2 {
					for dx in -(side / 2)..=side / 2 {
						let index = ((y as i32 + dy) as u32 * view.width + (x as i32 + dx) as u32) as usize * 4;
						sum += image.rgba[index..index + 3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0;
					}
				}
				sum / ((side / 2 * 2 + 1) * (side / 2 * 2 + 1)) as f32
			})
			.collect()
	}
}

impl Drop for Tone {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

/// The fixture's flat light: Ambient alone, no sun, sky light or shadows.
fn flat(ambient: f32, exposure: f32) -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::splat(ambient),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		exposure_compensation: exposure,
		..Environment::default()
	}
}

/// Flush 2-stud gray SmoothPlastic tiles in a row along x at z 0 in front of a black Neon wall, and
/// their middles.
fn tiles(grays: &[u8]) -> (Scene, Vec<Vec3>) {
	let mut model = ModelBuilder::new("Tiles");
	model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)), size: Vec3::new(160.0, 100.0, 1.0), color: [0; 3], material: 288, cast_shadow: false, ..Default::default() });
	let mut points = Vec::new();
	for (index, gray) in grays.iter().enumerate() {
		let center = Vec3::new((index as f32 - (grays.len() as f32 - 1.0) * 0.5) * 2.0, 0.0, 0.0);
		model.part(&PartSpec { name: format!("gray{gray}"), cframe: Affine3A::from_translation(center), size: Vec3::new(2.0, 2.0, 0.2), color: [*gray; 3], material: 272, cast_shadow: false, ..Default::default() });
		points.push(center - Vec3::Z * 0.1);
	}
	(crate::scene::extract::extract(&model.into_document()), points)
}

/// The client's display of gray tiles without fog (FogColor black) where it crosses a level: gray 11 at
/// EV -2 (x 0.00092) shows 0 and gray 14 (x 0.00149) 1, gray 22 (x 0.0037) 2; at EV -4 gray 30 shows 1
/// and gray 128 13; at EV 0 gray 64 shows 47, gray 128 131.6 (dithered between 131 and 132), gray 192
/// 192 and white 224. The curve before, with its linear toe, showed 1 for gray 11 at EV -2: one level
/// over the client from black up (its 0.5 at x 0.00072 where the client's is at 0.00136).
#[test]
fn dark_tiles_cross_levels_where_the_client_does() {
	let mut tone = Tone::new();
	for (exposure, cases) in [(-2.0f32, vec![(11u8, 0.0f32, 0.0), (14, 1.0, 0.0), (22, 2.0, 0.0), (40, 5.0, 0.0)]), (-4.0, vec![(30, 1.0, 0.0), (128, 13.0, 0.0)]), (0.0, vec![(64, 47.0, 0.5), (128, 131.6, 1.0), (192, 192.0, 1.0), (255, 224.0, 1.0)])] {
		let grays: Vec<u8> = cases.iter().map(|case| case.0).collect();
		let (mut scene, points) = tiles(&grays);
		scene.environment = Environment { fog_color: Vec3::ZERO, effects: vec![PostEffect::Bloom { intensity: 0.0, size: 24.0, threshold: 2.0 }], ..flat(1.0, exposure) };
		let levels = tone.levels(&scene, Vec3::ZERO, &points, 9);
		for ((gray, client, slack), level) in cases.iter().zip(&levels) {
			assert!((level - client).abs() <= slack + 0.01, "gray {gray} at EV {exposure}: {level} where the client shows {client}");
		}
	}
}

/// Roblox holds FogEnd to 100000: in the client a black wall 66 studs away reads the same under FogEnd
/// 10⁸ as under 100000 (4 of 255 at EV +3, 2 at +2, the default fog's x 0.00095 there), and black
/// with FogColor black. ror drew no fog at FogEnd 10⁸ (0 of 255).
#[test]
fn fog_end_is_held_to_100000() {
	let mut tone = Tone::new();
	let (mut scene, _) = tiles(&[]);
	let wall = [Vec3::new(0.0, 0.0, 5.5)];
	let mut level = |fog_end: f32, fog_color: Vec3| {
		scene.environment = Environment { fog_end, fog_color, ..flat(0.0, 3.0) };
		tone.levels(&scene, Vec3::ZERO, &wall, 21)[0]
	};
	let held = level(1.0e8, Vec3::splat(0.75));
	let default = level(100000.0, Vec3::splat(0.75));
	assert_eq!(held, default, "FogEnd 10⁸ fogs as FogEnd 100000");
	assert!((held - 4.0).abs() <= 1.0, "the default fog 66 studs out at EV +3: {held}, the client 4");
	assert_eq!(level(1.0e8, Vec3::ZERO), 0.0, "a black fog leaves black");
}
