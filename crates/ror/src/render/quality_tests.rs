//! What a view draws at a graphics quality level beyond the shadows and lights (crate::quality, notes/render.md
//! "Graphics quality levels"): the shading tiers by the focus and the camera, the ambient occlusion from 20 and the
//! anti-aliasing by level, the sun's light on faces turned from it, against a Roblox 0.747 client's frames
//! (fixtures/quality-levels.luau `shading`, `kinds`, `ao`, `backs`, `chunk`, `cluster`).

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};

/// The sun at ClockTime 16.2, GeographicLatitude 38 (the fixture's)
const SUN: Vec3 = Vec3::new(-0.862626, 0.439530, 0.250380);

struct Views {
	renderer: Renderer,
	state: std::path::PathBuf,
}

impl Views {
	fn new() -> Self {
		let state = std::env::temp_dir().join(format!("ror-quality-shading-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), state }
	}

	/// A Sky of one colour of the test's own (no Roblox install needed), no sun or moon drawn
	fn sky(&self, color: [u8; 3]) -> crate::scene::environment::Sky {
		let path = self.state.join(format!("sky-{}-{}-{}.png", color[0], color[1], color[2]));
		crate::output::write_png(&path, 8, 8, &[color[0], color[1], color[2], 255].repeat(64), false).unwrap();
		let uri = format!("file:///{}", path.display().to_string().replace('\\', "/"));
		crate::scene::environment::Sky { skybox: std::array::from_fn(|_| uri.clone()), celestial_bodies_shown: false, ..Default::default() }
	}

	/// The mean level (0-255) of a view's pixels
	fn level(&self, scene: &Scene, view: ViewRequest) -> f32 {
		let (images, _) = self.renderer.render(scene, &[view]).unwrap();
		let image = &images[0];
		image.rgba.chunks(4).map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).sum::<f32>() / 3.0).sum::<f32>() / (image.rgba.len() / 4) as f32
	}
}

impl Drop for Views {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

fn view(eye: Vec3, target: Vec3, fov: f32, quality: Option<u32>) -> ViewRequest {
	ViewRequest { width: 16, height: 16, camera: CameraSpec::LookAt { eye: eye.to_array(), target: target.to_array(), fov }, background: Background::Color([0.0, 0.0, 0.0]), bloom: false, quality, ..Default::default() }
}

/// The client's lowest shading has no sun glare on a large floor (lit by vertex, its corners far from the
/// glare), the per-pixel shading does: a white SmoothPlastic floor under the sun alone, seen against the sun
/// where it mirrors it (`shading`, `kinds`: floor 69 of 255 at level 4, 191 at 6 and 21). At 6 a part within 36
/// studs of the focus is lit per pixel and one farther by vertex (the floor 13 studs from a focus at the eye per
/// pixel, 38 studs from one below it by vertex).
#[test]
fn the_sun_glares_where_a_level_lights_per_pixel() {
	let views = Views::new();
	let mut model = ModelBuilder::new("Glare");
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(400.0, 1.0, 400.0), color: [255; 3], material: 272, ..Default::default() });
	let mut scene = crate::scene::extract::extract(&model.into_document());
	let environment = &mut scene.environment;
	(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
	(environment.clock_time, environment.geographic_latitude, environment.brightness) = (16.2, 38.0, 2.0);
	environment.sky = Some(views.sky([0, 0, 0]));
	// against the sun, 26 degrees down: the mirror image of the sun on the floor at the origin
	let ahead = Vec3::new(SUN.x, 0.0, SUN.z).normalize();
	let back = -ahead * 26f32.to_radians().cos() + Vec3::Y * 26f32.to_radians().sin();
	let eye = back * 30.0;
	let glare = |quality: Option<u32>, focus: Vec3| views.level(&scene, view(eye, focus, 2.0, quality));
	let top = glare(None, Vec3::ZERO);
	let lowest = glare(Some(4), Vec3::ZERO);
	assert!(top > lowest + 40.0, "the glare at 21 ({top}) over the lowest shading's floor ({lowest})");
	assert!((glare(Some(21), Vec3::ZERO) - top).abs() < 0.5);
	// 6: the focus on the floor, then 150 studs along the view (52 studs under the floor)
	let near = glare(Some(6), Vec3::ZERO);
	let far = glare(Some(6), eye - back * 150.0);
	assert!(near > lowest + 30.0, "level 6, the focus on the floor: per pixel ({near}, lowest {lowest})");
	assert!((far - lowest).abs() < 2.0, "level 6, the focus 52 studs from the floor: by vertex ({far}, lowest {lowest})");
	assert!((glare(Some(1), Vec3::ZERO) - lowest).abs() < 0.5, "1 to 4 alike");
}

/// The high-quality shading within its radius of the camera: a Metal plate shows the sky's reflection there and its
/// colour map lit as any surface past it (`hqline`: Metal 108 of 255 near the camera at 21, a flat 128 past level 7's
/// 55 studs). At 7 the plate seen from 20 studs is drawn as at 21, from 150 studs not; the focus on the plate, so it
/// is lit per pixel at both.
#[test]
fn metals_reflect_the_sky_within_the_high_quality_radius() {
	let views = Views::new();
	let mut model = ModelBuilder::new("Metal");
	model.part(&PartSpec { name: "Plate".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.2, 0.0)), size: Vec3::new(12.0, 0.4, 12.0), color: [163, 162, 165], material: 1088, ..Default::default() });
	let mut scene = crate::scene::extract::extract(&model.into_document());
	let environment = &mut scene.environment;
	(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::ZERO, Vec3::ZERO, 1.0, 1.0, 0.0);
	environment.sky = Some(views.sky([90, 140, 230]));
	let plate = |height: f32, quality: Option<u32>| {
		let fov = 2.0 * (4.0f32 / height).atan().to_degrees();
		views.level(&scene, view(Vec3::new(0.0, height, 0.05), Vec3::ZERO, fov, quality))
	};
	let (near, far) = (plate(20.0, None), plate(150.0, None));
	assert!((plate(20.0, Some(7)) - near).abs() < 1.0, "level 7 from 20 studs: the high-quality shading ({}, 21 {near})", plate(20.0, Some(7)));
	let low = plate(150.0, Some(7));
	assert!((low - far).abs() > 6.0, "level 7 from 150 studs: the low-quality shading ({low}, 21 {far})");
	assert!((plate(150.0, Some(18)) - far).abs() < 1.0, "from 18 the high-quality shading everywhere");
}

/// Ambient occlusion from level 20 (`ao`: the floor 0.25 studs from a box under Ambient alone 134 of 255 at
/// 1-19, 104 at 20 and 21), the station's box, floor and camera.
#[test]
fn ambient_occlusion_starts_at_level_20() {
	let views = Views::new();
	let mut model = ModelBuilder::new("Occlusion");
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(100.0, 1.0, 100.0), color: [220; 3], material: 272, ..Default::default() });
	model.part(&PartSpec { name: "Box".into(), cframe: Affine3A::from_translation(Vec3::new(-12.0, 4.0, 0.0)), size: Vec3::splat(8.0), color: [220; 3], material: 272, ..Default::default() });
	let mut scene = crate::scene::extract::extract(&model.into_document());
	let environment = &mut scene.environment;
	(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale, environment.brightness) = (Vec3::splat(150.0 / 255.0), Vec3::ZERO, 0.0, 0.0, 0.0);
	environment.sky = Some(views.sky([0, 0, 0]));
	let corner = |quality: Option<u32>| {
		let request = ViewRequest { width: 320, height: 180, camera: CameraSpec::LookAt { eye: [0.0, 20.0, 30.0], target: [0.0, 2.0, 0.0], fov: 50.0 }, background: Background::Color([0.0, 0.0, 0.0]), bloom: false, quality, ..Default::default() };
		let (images, _) = views.renderer.render(&scene, std::slice::from_ref(&request)).unwrap();
		// the floor 0.25 studs off the box's +Z face, 3 x 3 pixels
		let camera = request.camera.place(None, request.width, request.height);
		let clip = camera.view_projection(request.width as f32 / request.height as f32) * Vec3::new(-12.0, 0.0, 4.25).extend(1.0);
		let (x, y) = ((((clip.x / clip.w) * 0.5 + 0.5) * request.width as f32) as i32, ((0.5 - (clip.y / clip.w) * 0.5) * request.height as f32) as i32);
		let mut sum = 0.0;
		for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
			let index = ((y + dy) as u32 * request.width + (x + dx) as u32) as usize * 4;
			sum += images[0].rgba[index..index + 3].iter().map(|&channel| channel as f32).sum::<f32>() / 27.0;
		}
		sum
	};
	let (open, occluded) = (corner(Some(19)), corner(Some(20)));
	// (the occlusion is sized in pixels: at this small view it darkens 0.25 studs off by ~11 of 255)
	assert!(occluded < open - 6.0, "the floor by the box darkens at 20 ({occluded}) and not at 19 ({open})");
	assert!((corner(None) - occluded).abs() < 0.5);
}

/// Faces turned away from the sun take 0.41 · |n·l| of its light where a part's cluster lies past the level's radius of
/// the focus (crate::quality::Quality::back_light_radius; `backs`, `chunk`, `cluster`): a white plate whose face looks
/// straight away from the sun, 40 studs from the focus between it and the camera, without GlobalShadows (no light grid).
/// Lit at level 4 (every part), not at 21 (599.5 studs); at 7 (19.5 studs) lit alone in its 128 x 64 x 128-stud cell,
/// not with a block 8 studs from the focus in the cell, lit again with that block across the cell's edge. Half as lit at
/// EnvironmentDiffuseScale 0.5, not at all at 1 (the sky black: its own light nothing).
#[test]
fn faces_turned_from_the_sun_take_its_back_light_past_the_focus_radius() {
	let views = Views::new();
	let plate = Vec3::new(64.0, 32.0, 64.0);
	let away = -SUN;
	let focus = plate + away * 40.0;
	let eye = plate + away * 50.0;
	let draw_at = |other: Option<Vec3>, quality: Option<u32>, diffuse: f32| {
		let mut model = ModelBuilder::new("Backs");
		// the plate's top faces away from the sun
		let up = away;
		let side = up.cross(Vec3::Z).normalize();
		let rotation = glam::Mat3::from_cols(side, up, side.cross(up));
		model.part(&PartSpec { name: "Plate".into(), cframe: Affine3A::from_mat3_translation(rotation, plate), size: Vec3::new(8.0, 0.2, 8.0), color: [230; 3], material: 272, ..Default::default() });
		if let Some(at) = other {
			model.part(&PartSpec { name: "Other".into(), cframe: Affine3A::from_translation(at), size: Vec3::ONE, color: [230; 3], material: 272, ..Default::default() });
		}
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, diffuse, 0.0);
		(environment.clock_time, environment.geographic_latitude, environment.brightness, environment.global_shadows) = (16.2, 38.0, 2.0, false);
		environment.sky = Some(views.sky([0, 0, 0]));
		views.level(&scene, view(eye, focus, 2.0 * (3.0f32 / 50.0).atan().to_degrees(), quality))
	};
	let draw = |other: Option<Vec3>, quality: Option<u32>| draw_at(other, quality, 0.0);
	let lit = draw(None, Some(4));
	assert!(lit > 60.0, "level 4: the face turned from the sun takes its back light ({lit})");
	assert!(draw(None, None) < 1.0 && draw(None, Some(21)) < 1.0, "level 21: none within 599.5 studs of the focus");
	assert!((draw(None, Some(7)) - lit).abs() < 1.0, "level 7: alone in its cell, 40 studs out ({} against {lit})", draw(None, Some(7)));
	// a block 8 studs from the focus, off the view, in the plate's cell; then across the cell's edge at X = 128
	let near = focus + Vec3::new(0.0, 0.0, 8.0);
	assert!(near.x < 128.0 && near.y < 64.0 && near.z < 128.0 && near.min_element() >= 0.0);
	assert!(draw(Some(near), Some(7)) < 1.0, "level 7: its cluster reaches within 19.5 studs of the focus");
	assert!((draw(Some(near), Some(4)) - lit).abs() < 1.0, "level 4: every cluster");
	assert!((draw(Some(Vec3::new(130.0, near.y, near.z)), Some(7)) - lit).abs() < 1.0, "level 7: a block in the next cell leaves it out");
	let half = draw_at(None, Some(4), 0.5);
	assert!(half > 10.0 && half < lit - 15.0, "EnvironmentDiffuseScale 0.5: about half the light ({half} against {lit})");
	assert!(draw_at(None, Some(4), 1.0) < 1.0, "EnvironmentDiffuseScale 1: none");
}

/// Anti-aliasing by level: none at 1-16, 4 samples from 17 (the client's 2 at 17: ror has no 2-sample passes), as
/// `msaa` asks no more.
#[test]
fn a_level_draws_its_samples() {
	let at = |quality: Option<u32>, msaa: u32| view_samples(&ViewRequest { quality, msaa, ..Default::default() });
	assert_eq!([None, Some(21), Some(18), Some(17), Some(16), Some(1)].map(|quality| at(quality, 4)), [4, 4, 4, 4, 1, 1]);
	assert_eq!(at(Some(21), 1), 1);
}

/// A part's cluster: the box round every part whose middle shares its 128 x 64 x 128-stud cell (`cluster`: a ball's far
/// side follows a part 68 studs off in its cell, of any material, and a bar whose middle is in the cell while it reaches
/// past it; not a part across X = 128 or Y = 64).
#[test]
fn parts_share_the_box_of_their_cell() {
	let instance = |middle: Vec3, size: Vec3| {
		let mut data = InstanceData::zeroed();
		data.part_inverse = Mat4::from(Affine3A::from_scale_rotation_translation(size, glam::Quat::IDENTITY, middle)).inverse().to_cols_array_2d();
		data
	};
	let mut instances = vec![
		instance(Vec3::new(32.0, 32.0, 32.0), Vec3::splat(4.0)),
		instance(Vec3::new(100.0, 32.0, 100.0), Vec3::splat(4.0)),
		// its middle in the cell, reaching past X = 128
		instance(Vec3::new(120.0, 32.0, 32.0), Vec3::new(40.0, 4.0, 4.0)),
		// across X = 128, over Y = 64
		instance(Vec3::new(140.0, 32.0, 32.0), Vec3::splat(4.0)),
		instance(Vec3::new(32.0, 70.0, 32.0), Vec3::splat(4.0)),
	];
	cluster_boxes(&mut instances);
	let cluster = |index: usize| (Vec3::from_slice(&instances[index].cluster_low[..3]), Vec3::from_slice(&instances[index].cluster_high[..3]));
	let shared = (Vec3::new(30.0, 30.0, 30.0), Vec3::new(140.0, 34.0, 102.0));
	for index in 0..3 {
		let (low, high) = cluster(index);
		assert!(low.abs_diff_eq(shared.0, 1e-3) && high.abs_diff_eq(shared.1, 1e-3), "part {index}: {low} {high}");
	}
	assert!(cluster(3).0.abs_diff_eq(Vec3::new(138.0, 30.0, 30.0), 1e-3));
	assert!(cluster(4).1.abs_diff_eq(Vec3::new(34.0, 72.0, 34.0), 1e-3));
}
