//! The sun's shadow maps far from the camera and under cut-outs, against a Roblox 0.747 client's
//! frames (fixtures/sun-shadows.luau; notes/render.md "Shadows"): no shadow further than
//! `SUN_SHADOWS.reach` from the camera, and no leak under a plate 3 studs over a floor in the last
//! cascade.

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};

/// The sun at ClockTime 16.2, GeographicLatitude 38 (the fixture's)
const SUN: Vec3 = Vec3::new(-0.862626, 0.439530, 0.250380);

struct Sun {
	renderer: Renderer,
	state: std::path::PathBuf,
}

impl Sun {
	fn new() -> Self {
		let state = std::env::temp_dir().join(format!("ror-sun-shadow-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), state }
	}

	/// The fixture's Lighting: the sun alone (no ambient or environment light).
	fn lit(mut scene: Scene) -> Scene {
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
		(environment.clock_time, environment.geographic_latitude, environment.shadow_softness, environment.brightness) = (16.2, 38.0, 0.2, 2.0);
		scene
	}

	/// The mean level (0-255) of the `side` x `side` pixels round `at` in `image` seen by `view`.
	fn level(image: &ViewImage, view: &ViewRequest, at: Vec3, side: i32) -> f32 {
		let camera = view.camera.place(None, view.width, view.height);
		let clip = camera.view_projection(view.width as f32 / view.height as f32) * at.extend(1.0);
		let (x, y) = (((clip.x / clip.w) * 0.5 + 0.5) * view.width as f32, (0.5 - (clip.y / clip.w) * 0.5) * view.height as f32);
		let mut sum = 0.0;
		for dy in -(side / 2)..=side / 2 {
			for dx in -(side / 2)..=side / 2 {
				let index = ((y as i32 + dy) as u32 * view.width + (x as i32 + dx) as u32) as usize * 4;
				sum += image.rgba[index..index + 3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0;
			}
		}
		sum / ((side / 2 * 2 + 1) * (side / 2 * 2 + 1)) as f32
	}
}

impl Drop for Sun {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

/// A 2,000-stud floor and, when `height` is given, a 60 x 0.4 x 60 plate that far over it whose shadow
/// falls on `target` in the band it throws beside itself (+X, across the sun), where a camera looking
/// along +Z sees the floor past the plate.
fn far_scene(target: Vec3, height: Option<f32>) -> Scene {
	let mut model = ModelBuilder::new("Far");
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(2000.0, 1.0, 2000.0), color: [255; 3], material: 272, ..Default::default() });
	if let Some(height) = height {
		let offset = -SUN * (height / SUN.y);
		// the shadow's band beside the plate (as wide as offset.x) centred on the target
		let middle = target - offset + Vec3::new(offset.x / 2.0 - 30.0, 0.2, 0.0);
		model.part(&PartSpec { name: "Plate".into(), cframe: Affine3A::from_translation(middle), size: Vec3::new(60.0, 0.4, 60.0), color: [40; 3], material: 272, ..Default::default() });
	}
	Sun::lit(crate::scene::extract::extract(&model.into_document()))
}

/// The engine (fixtures/sun-shadows.luau "plates" and "leaks"): a plate 3 studs over the floor (Δ 6.8
/// studs along the sun) shadows it fully in the last cascade, 443 studs from the camera, and not at all
/// 670 studs away, beyond the shadows' reach of 587.5 (a field of view of 70, its last sphere reaching
/// 796 studs). ror let 0.2-0.6 of the light under such plates there and shadowed to the sphere's end.
#[test]
fn the_suns_shadow_ends_at_its_reach_and_leaks_nothing_far_out() {
	let sun = Sun::new();
	for (target, back, shadowed) in [(Vec3::ZERO, 395.0, true), (Vec3::new(0.0, 0.0, 300.0), 640.0, false)] {
		let eye = target + Vec3::new(0.0, 200.0, -back);
		let view = ViewRequest { width: 512, height: 512, camera: CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 70.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
		let level = |height: Option<f32>| {
			let (images, _) = sun.renderer.render(&far_scene(target, height), std::slice::from_ref(&view)).unwrap();
			// (the band beside a plate 3 studs up is 5.9 studs wide: 5 pixels here 443 studs out, 3 at 670)
			Sun::level(&images[0], &view, target, 1)
		};
		// what a plate 30 studs up takes off (no cascade leaks under it), and one 3 studs up
		let (open, deep, near) = (level(None), level(Some(30.0)), level(Some(3.0)));
		let distance = eye.distance(target);
		if shadowed {
			assert!(open - deep > 0.5 * open, "{distance:.0} studs out: a plate 30 studs up shadows the floor ({open} open, {deep} under it)");
			let share = (open - near) / (open - deep);
			assert!(share > 0.95, "{distance:.0} studs out: a plate 3 studs up takes {share} of the light a plate 30 studs up does, all of it expected");
		} else {
			assert!((open - near).abs() < 0.03 * open && (open - deep).abs() < 0.03 * open, "{distance:.0} studs out: no shadow beyond the reach ({open} open, {near} and {deep} under plates 3 and 30 studs up)");
		}
	}
}
