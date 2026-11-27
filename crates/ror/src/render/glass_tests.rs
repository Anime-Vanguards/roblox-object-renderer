//! Glass against a Roblox 0.747 client's frames (fixtures/glass-client.luau; notes/materials.md "Glass"): the
//! sky it reflects by its own Fresnel whatever EnvironmentSpecularScale and tinted by its colour, the light
//! opaque Glass gives up at grazing angles, its Reflectance's mirror image whatever the light, the sky in
//! place of a Glass MeshPart's vertex alpha, the push off the view's axis, and no refraction below graphics
//! quality level 16. Each scene has a uniform white or black Sky of its own (8 x 8 file faces), so nothing
//! waits on Roblox's built-in textures.

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Content, Vector3};

const GLASS: u32 = 1568;
const SMOOTH_PLASTIC: u32 = 272;

struct Bench {
	renderer: Renderer,
	state: std::path::PathBuf,
	white: String,
	black: String,
}

impl Bench {
	fn new(name: &str) -> Self {
		let state = std::env::temp_dir().join(format!("ror-glass-test-{name}-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let renderer = Renderer::new(gpu, assets).expect("a renderer");
		let face = |name: &str, level: u8| {
			let path = state.join(format!("{name}.png"));
			crate::output::write_png(&path, 8, 8, &[level, level, level, 255].repeat(64), false).unwrap();
			format!("file:///{}", path.display().to_string().replace('\\', "/"))
		};
		let (white, black) = (face("white-sky", 255), face("black-sky", 0));
		Self { renderer, state, white, black }
	}

	fn file(&self, name: &str, bytes: &[u8]) -> String {
		let path = self.state.join(name);
		std::fs::write(&path, bytes).unwrap();
		format!("file:///{}", path.display().to_string().replace('\\', "/"))
	}

	/// The model's scene under the white Sky (`white`, else a black one), in the dark unless `light` changes it
	fn scene(&self, model: ModelBuilder, white: bool, light: impl FnOnce(&mut crate::scene::Environment)) -> Scene {
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let face = if white { &self.white } else { &self.black };
		scene.environment.sky = Some(crate::scene::environment::Sky { skybox: std::array::from_fn(|_| face.clone()), celestial_bodies_shown: false, ..Default::default() });
		let env = &mut scene.environment;
		(env.ambient, env.outdoor_ambient, env.brightness, env.environment_diffuse_scale, env.environment_specular_scale, env.global_shadows) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0, false);
		light(env);
		scene
	}

	/// The scene's image (RGBA, `width` x `height`) from `eye` towards `target`
	fn image(&self, scene: &Scene, eye: [f32; 3], target: [f32; 3], fov: f32, width: u32, height: u32, quality: Option<u32>) -> Vec<u8> {
		let request = ViewRequest { width, height, camera: CameraSpec::LookAt { eye, target, fov }, background: Background::Color([0.0; 3]), bloom: false, quality, ..Default::default() };
		let (images, _) = self.renderer.render(scene, std::slice::from_ref(&request)).unwrap();
		images.into_iter().next().unwrap().rgba
	}
}

impl Drop for Bench {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

fn pixel(image: &[u8], width: u32, x: u32, y: u32) -> [f32; 3] {
	let index = ((y * width + x) * 4) as usize;
	std::array::from_fn(|c| image[index + c] as f32)
}

fn ball(model: &mut ModelBuilder, name: &str, color: [u8; 3], transparency: f32) {
	model.part(&PartSpec { name: name.into(), size: Vec3::splat(6.0), color, material: GLASS, shape: Some(0), transparency, cast_shadow: false, ..Default::default() });
}

/// Glass reflects the sky by its own Fresnel at EnvironmentSpecularScale 0 as at 1, a black Ball 0.48 of
/// what a white one does, and from Transparency 0.5 more of it at a lower Transparency, (1 - T); opaque Glass
/// keeps 0.35-0.5 of SmoothPlastic's light from a PointLight where it is seen at a grazing angle; its
/// Reflectance's mirror image reads the same lit by Ambient alone as on a face no light reaches; a Glass
/// MeshPart whose vertex alpha is 0 shows the sky in its place before a black wall. Before, the sky's
/// reflection scaled with EnvironmentSpecularScale and took no tint, opaque Glass shaded as SmoothPlastic,
/// the mirror was lit by half the diffuse light, and the MeshPart drew as opaque black Glass.
#[test]
fn glass_reflects_tints_and_gives_up_light_as_a_client_draws_it() {
	let bench = Bench::new("shading");
	let size = 64u32;
	// a 6-stud Ball 30 studs off at fov 15 is 24 px across its radius: its rim (r / R 0.92) 22 px right of the middle
	let rim = |image: &[u8]| pixel(image, size, size / 2 + 22, size / 2);
	let reflected = |color: [u8; 3], transparency: f32, specular: f32| {
		let mut model = ModelBuilder::new("Ball");
		ball(&mut model, "Ball", color, transparency);
		let scene = bench.scene(model, true, |env| env.environment_specular_scale = specular);
		rim(&bench.image(&scene, [0.0, 0.0, -30.0], [0.0; 3], 15.0, size, size, None))
	};
	let black_off = reflected([0, 0, 0], 0.5, 0.0);
	let black_on = reflected([0, 0, 0], 0.5, 1.0);
	assert!(black_off[0] > 40.0, "Glass reflects the sky at EnvironmentSpecularScale 0: {black_off:?}");
	assert!((0..3).all(|c| (black_off[c] - black_on[c]).abs() <= 2.0), "and the same at 1: {black_off:?} against {black_on:?}");
	let white = reflected([255, 255, 255], 0.5, 0.0);
	assert!(white[0] > black_off[0] + 20.0, "a white Ball reflects more than a black one: {white:?} against {black_off:?}");
	let fainter = reflected([255, 255, 255], 0.8, 0.0);
	assert!(fainter[0] + 20.0 < white[0], "and less at Transparency 0.8 than at 0.5: {fainter:?} against {white:?}");

	// floors of opaque Glass and SmoothPlastic under a PointLight, seen low over the floor in the dark
	let floor = |material: u32| {
		let mut model = ModelBuilder::new("Floor");
		model.part(&PartSpec { name: "Floor".into(), size: Vec3::new(60.0, 1.0, 60.0), color: [255, 255, 255], material, cast_shadow: false, ..Default::default() });
		let lamp = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 13.5, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		model.child(lamp, InstanceBuilder::new("PointLight").with_property("Range", 60.0f32).with_property("Brightness", 1.5f32));
		let scene = bench.scene(model, false, |env| env.clock_time = 0.0);
		// the floor 10 studs past the light, away from its mirror image
		let image = bench.image(&scene, [-30.0, 5.0, 0.0], [10.0, 0.5, 0.0], 30.0, size, size, None);
		pixel(&image, size, size / 2, size / 2)
	};
	let glass = floor(GLASS);
	let plastic = floor(SMOOTH_PLASTIC);
	assert!(plastic[0] > 40.0, "the SmoothPlastic floor is lit: {plastic:?}");
	assert!(glass[0] < 0.85 * plastic[0], "opaque Glass gives up much of the PointLight's light at a grazing angle: {glass:?} against {plastic:?}");

	// a black Glass cube at Reflectance 1 facing the camera, lit by Ambient alone or by a noon sun its face never sees
	let mirror = |ambient: f32, sun: f32| {
		let mut model = ModelBuilder::new("Mirror");
		model.part(&PartSpec { name: "Cube".into(), size: Vec3::splat(6.0), color: [0, 0, 0], material: GLASS, reflectance: 1.0, cast_shadow: false, ..Default::default() });
		let scene = bench.scene(model, true, |env| {
			env.ambient = Vec3::splat(ambient);
			env.brightness = sun;
			env.clock_time = 12.0;
		});
		pixel(&bench.image(&scene, [0.0, 0.0, -30.0], [0.0; 3], 15.0, size, size, None), size, size / 2, size / 2)
	};
	let by_ambient = mirror(0.7, 0.0);
	let by_sun = mirror(0.0, 2.0);
	assert!(by_sun[0] > 40.0, "the mirror image shows on a face no light reaches: {by_sun:?}");
	assert!((0..3).all(|c| (by_ambient[c] - by_sun[c]).abs() <= 2.0), "and reads the same lit by Ambient: {by_ambient:?} against {by_sun:?}");

	// a black Glass quad before a black wall, its vertices at alpha 0 (the sky in its place) or 1 (dark)
	let quad = |alpha: u8| {
		let mesh = bench.file(&format!("quad-{alpha}.mesh"), &super::vertex_colour_tests::quad_mesh([255, 255, 255, alpha]));
		let mut model = ModelBuilder::new("Quad");
		let part = model.part(&PartSpec { class: "MeshPart", name: "Quad".into(), size: Vec3::new(4.0, 4.0, 0.05), color: [0, 0, 0], material: GLASS, cast_shadow: false, ..Default::default() });
		model.set(part, "MeshContent", Content::from_uri(mesh.as_str()));
		model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
		model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)), size: Vec3::new(30.0, 30.0, 1.0), color: [0, 0, 0], material: SMOOTH_PLASTIC, cast_shadow: false, ..Default::default() });
		let scene = bench.scene(model, true, |env| env.ambient = Vec3::splat(0.7));
		pixel(&bench.image(&scene, [0.0, 0.0, -20.0], [0.0; 3], 15.0, size, size, None), size, size / 2, size / 2)
	};
	let hole = quad(0);
	let solid = quad(255);
	assert!(solid[0] < 40.0, "the quad at vertex alpha 1 is dark Glass: {solid:?}");
	assert!(hole[0] > 150.0, "at vertex alpha 0 the sky shows in its place, before the wall: {hole:?}");
}

/// What Glass shows behind it moves off the view's axis (a pane facing the camera 10 studs aside moves the
/// stripes behind it, where before nothing moved), and nothing moves below graphics quality level 16 (a Ball
/// over stripes shows them as they are beside it).
#[test]
fn glass_moves_what_is_behind_it_off_axis_and_only_from_quality_16() {
	let bench = Bench::new("refraction");
	let (width, height) = (512u32, 348u32);
	// vertical stripes 2 studs wide, 4 studs behind the glass, black and white SmoothPlastic under Ambient alone
	let stripes = |model: &mut ModelBuilder, x: f32| {
		for index in -12..=12 {
			let color = if index % 2 == 0 { [255u8; 3] } else { [0; 3] };
			model.part(&PartSpec { name: format!("Stripe{index}"), cframe: Affine3A::from_translation(Vec3::new(x + index as f32 * 2.0, 0.0, 4.0)), size: Vec3::new(2.0, 40.0, 0.5), color, material: SMOOTH_PLASTIC, cast_shadow: false, ..Default::default() });
		}
	};
	// the largest change along a row through the glass against the row through the stripes alone, over columns x0..x1
	let moved = |image: &[u8], y_glass: u32, y_bare: u32, x0: u32, x1: u32| (x0..x1).map(|x| (pixel(image, width, x, y_glass)[0] - pixel(image, width, x, y_bare)[0]).abs()).fold(0.0f32, f32::max);
	let camera = |aside: f32| ([aside, 0.0, -30.0], [aside, 0.0, 0.0]);

	// a 3 x 3 pane at Transparency 0.7 facing the camera, 10 studs off the view's axis
	let mut model = ModelBuilder::new("Pane");
	stripes(&mut model, 0.0);
	model.part(&PartSpec { name: "Pane".into(), size: Vec3::new(3.0, 3.0, 0.2), color: [255, 255, 255], material: GLASS, transparency: 0.7, cast_shadow: false, ..Default::default() });
	let scene = bench.scene(model, true, |env| env.ambient = Vec3::splat(0.7));
	let (eye, target) = camera(10.0);
	let image = bench.image(&scene, eye, target, 40.0, width, height, None);
	// the pane spans 48 px round 160 px from the middle (either side is looked at); its rows through the
	// middle against rows above it
	let side = |middle: u32| moved(&image, height / 2, height / 2 - 40, middle - 18, middle + 18);
	let off_axis = side(256 + 160).max(side(256 - 160));
	assert!(off_axis > 60.0, "the stripes behind an off-axis pane move: the largest change {off_axis}");

	// a 6-stud Ball at Transparency 0.7 on the view's axis at quality levels 15 and 21
	let mut model = ModelBuilder::new("Ball");
	stripes(&mut model, 0.0);
	ball(&mut model, "Ball", [255, 255, 255], 0.7);
	let scene = bench.scene(model, true, |env| env.ambient = Vec3::splat(0.7));
	let (eye, target) = camera(0.0);
	let low = bench.image(&scene, eye, target, 40.0, width, height, Some(15));
	let top = bench.image(&scene, eye, target, 40.0, width, height, Some(21));
	// the Ball is 96 px across: the middle half of its row against the row 70 px above it
	let still = moved(&low, height / 2, height / 2 - 70, 256 - 24, 256 + 24);
	let bent = moved(&top, height / 2, height / 2 - 70, 256 - 24, 256 + 24);
	assert!(still < 30.0, "below level 16 the Ball shows the stripes as they are: the largest change {still}");
	assert!(bent > 60.0, "from level 16 it bends them: the largest change {bent}");
}
