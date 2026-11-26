//! The sun's shadow maps far from the camera and under cut-outs, against a Roblox 0.747 client's
//! frames (fixtures/sun-shadows.luau; notes/render.md "Shadows"): no shadow further than
//! `SUN_SHADOWS.reach` from the camera, no leak under a plate 3 studs over a floor in the last cascade,
//! and an alpha checkerboard's holes as open as the same pattern built from parts.

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};
use rbx_dom_weak::types::{Content, Vector3};

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

	fn uri(path: &std::path::Path) -> String {
		format!("file:///{}", path.display().to_string().replace('\\', "/"))
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

/// The engine (fixtures/sun-shadows.luau "patterns", the cascade 150 studs from the camera): a card 3
/// studs over the floor cut into squares by its colour map's alpha shadows the floor as the same squares
/// built from parts do: 1.5-stud squares' holes 134.5 of 153 against 135, 0.75-stud squares a mean of
/// 84 against 95. ror read the alpha at the shadow texels' own level, whose blur shrank the holes: 121
/// against its parts' 132.5, and 65 against 91.5.
#[test]
fn a_cut_out_checkerboard_shadows_as_one_built_from_parts() {
	let sun = Sun::new();
	let mesh_path = sun.state.join("card.mesh");
	std::fs::write(&mesh_path, super::vertex_colour_tests::quad_mesh([255; 4])).unwrap();
	let mesh = Sun::uri(&mesh_path);
	let card = Vec3::new(0.0, 3.0, 0.0);
	// `count` x `count` squares on a 6-stud card, the opaque ones where (column + row) is even
	let square = |count: u32, index: u32| card + Vec3::new(((index % count) as f32 + 0.5) * 6.0 / count as f32 - 3.0, 0.0, ((index / count) as f32 + 0.5) * 6.0 / count as f32 - 3.0);
	let scene = |count: u32, alpha: bool| {
		let mut model = ModelBuilder::new("Checker");
		model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(200.0, 1.0, 200.0), color: [200; 3], material: 272, ..Default::default() });
		if alpha {
			let cframe = Affine3A::from_translation(card) * Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2);
			let part = model.part(&PartSpec { class: "MeshPart", name: "Card".into(), cframe, size: Vec3::new(6.0, 6.0, 0.001), material: 272, ..Default::default() });
			model.set(part, "MeshContent", Content::from_uri(mesh.as_str()));
			model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
		} else {
			let side = 6.0 / count as f32;
			for index in (0..count * count).filter(|index| (index % count + index / count).is_multiple_of(2)) {
				model.part(&PartSpec { name: "Square".into(), cframe: Affine3A::from_translation(square(count, index)), size: Vec3::new(side, 0.05, side), material: 272, ..Default::default() });
			}
		}
		let mut scene = crate::scene::extract::extract(&model.into_document());
		if let Some(part) = scene.parts.iter_mut().find(|part| matches!(part.geometry, crate::scene::Geometry::MeshPart { .. })) {
			let map_path = sun.state.join(format!("checker-{count}.png"));
			let cell = 64 / count;
			let rgba: Vec<u8> = (0..64u32 * 64).flat_map(|index| [60, 140, 60, if (index % 64 / cell + index / 64 / cell).is_multiple_of(2) { 255 } else { 0 }]).collect();
			crate::output::write_png(&map_path, 64, 64, &rgba, false).unwrap();
			Arc::make_mut(part).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(Sun::uri(&map_path)),
				normal_map: None,
				metalness_map: None,
				roughness_map: None,
				texture_pack: None,
				alpha_mode: 1,
				color: Vec3::ONE,
				emissive_mask: None,
				emissive_strength: 1.0,
				emissive_tint: Vec3::ONE,
			});
		}
		Sun::lit(scene)
	};
	// straight down from 150 studs over the shadow's middle: the third cascade (texels of 0.14 studs)
	let middle = card - SUN * (card.y / SUN.y);
	let view = ViewRequest { width: 256, height: 256, camera: CameraSpec::LookAt { eye: [middle.x, 150.0, middle.z - 0.05], target: middle.into(), fov: 40.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
	// the floor under each square's middle: its shadow's lighter half the holes (whichever way the card's
	// texture runs), and the mean of all; the open floor beside the shadow
	let read = |count: u32, alpha: bool| {
		let (images, stats) = sun.renderer.render(&scene(count, alpha), std::slice::from_ref(&view)).unwrap();
		assert!(stats.missing.is_empty(), "missing {:?}", stats.missing);
		let mut levels: Vec<f32> = (0..count * count).map(|index| Sun::level(&images[0], &view, square(count, index) - SUN * (card.y / SUN.y), 1)).collect();
		levels.sort_by(f32::total_cmp);
		let half = levels.len() / 2;
		let holes = levels[half..].iter().sum::<f32>() / half as f32;
		let mean = levels.iter().sum::<f32>() / levels.len() as f32;
		(holes, mean, Sun::level(&images[0], &view, middle + Vec3::new(0.0, 0.0, 9.0), 3))
	};
	for count in [4, 8] {
		let (parts_holes, parts_mean, open) = read(count, false);
		let (alpha_holes, alpha_mean, _) = read(count, true);
		assert!(open - parts_mean > 0.2 * open, "{count} x {count}: the parts shadow the floor ({parts_mean} under them, {open} open)");
		let (holes, mean) = ((alpha_holes - parts_holes) / open, (alpha_mean - parts_mean) / open);
		assert!(holes.abs() < 0.03 && mean.abs() < 0.05, "{count} x {count} squares: the alpha checkerboard's holes {alpha_holes} and mean {alpha_mean} as the parts' {parts_holes} and {parts_mean} ({holes}, {mean} of the open floor apart)");
	}
}
