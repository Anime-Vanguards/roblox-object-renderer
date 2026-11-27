//! BlurEffect and SunRaysEffect against a Roblox 0.747 client's frames (fixtures/post-effects.luau;
//! notes/render.md "Post effects"), on a scene under a flat Ambient alone.

use super::*;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};
use crate::scene::environment::PostEffect;

struct Post {
	renderer: Renderer,
	state: std::path::PathBuf,
}

impl Post {
	fn new() -> Self {
		let state = std::env::temp_dir().join(format!("ror-post-test-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), state }
	}

	/// `scene` from `camera` at the fixture's 1644 x 1116, the red channel of every pixel (0-255).
	fn draw(&self, scene: &Scene, camera: CameraSpec) -> (Vec<f32>, camera::Camera) {
		let view = ViewRequest { width: 1644, height: 1116, camera, background: Background::Sky, ..Default::default() };
		let (images, _) = self.renderer.render(scene, std::slice::from_ref(&view)).expect("a render");
		let placed = view.camera.place(None, view.width, view.height);
		(images[0].rgba.iter().step_by(4).map(|&red| red as f32).collect(), placed)
	}
}

impl Drop for Post {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

const WIDTH: usize = 1644;
const HEIGHT: usize = 1116;

/// Where a world point lands on the view, in pixels.
fn pixel(camera: &camera::Camera, point: Vec3) -> (usize, usize) {
	let clip = camera.view_projection(WIDTH as f32 / HEIGHT as f32) * point.extend(1.0);
	((((clip.x / clip.w) * 0.5 + 0.5) * WIDTH as f32) as usize, ((0.5 - (clip.y / clip.w) * 0.5) * HEIGHT as f32) as usize)
}

/// The fixture's flat light: Ambient alone, no sun, sky light or shadows.
fn flat(ambient: f32) -> Environment {
	Environment { brightness: 0.0, ambient: Vec3::splat(ambient), outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, global_shadows: false, ..Environment::default() }
}

fn sum(image: &[f32], rows: std::ops::Range<usize>, columns: std::ops::Range<usize>) -> f32 {
	rows.flat_map(|y| columns.clone().map(move |x| (y, x))).map(|(y, x)| image[y * WIDTH + x]).sum()
}

/// The client's BlurEffect blurs the radiance as x^0.525, not the shown values: at Size 24 a white line 4
/// px wide keeps 0.58-0.60 of its shown light (ror now 0.69; its blur of the shown values kept all of
/// it); and under Size 12 it lays a blur of about 4.5 px over the sharp image: at Size 4, 3 px past a white
/// square's edge pixel the client shows 15 of 255 and ror now 8 (the Gaussian of 0.64 px it drew there
/// showed 0; the client's halo near an edge is sharper than the fitted mix, open).
#[test]
fn the_blur_keeps_the_light_of_thin_lines_and_haloes_small_sizes_as_the_client_does() {
	let post = Post::new();
	let pixel_stud = 1.0 / 25.55;
	let mut model = ModelBuilder::new("Blur");
	model.part(&PartSpec { name: "Backdrop".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 6.0)), size: Vec3::new(200.0, 120.0, 1.0), color: [0; 3], material: 288, cast_shadow: false, ..Default::default() });
	model.part(&PartSpec { name: "Line".into(), cframe: Affine3A::from_translation(Vec3::new(-10.0, 0.0, 0.0)), size: Vec3::new(4.0 * pixel_stud, 6.0, 0.2), color: [255; 3], material: 272, cast_shadow: false, ..Default::default() });
	model.part(&PartSpec { name: "Square".into(), cframe: Affine3A::from_translation(Vec3::new(10.0, 0.0, 0.0)), size: Vec3::new(6.0, 6.0, 0.2), color: [255; 3], material: 272, cast_shadow: false, ..Default::default() });
	let mut scene = crate::scene::extract::extract(&model.into_document());
	let camera = CameraSpec::LookAt { eye: [0.0, 0.0, -60.0], target: [0.0, 0.0, 0.0], fov: 40.0 };
	let mut draw = |size: Option<f32>| {
		scene.environment = Environment { effects: size.map(|size| vec![PostEffect::Blur { size }]).unwrap_or_default(), ..flat(1.0) };
		post.draw(&scene, camera.clone())
	};
	let (sharp, placed) = draw(None);
	let (line_x, middle) = pixel(&placed, Vec3::new(-10.0, 0.0, -0.1));
	let around = |image: &[f32]| sum(image, middle - 40..middle + 40, line_x - 90..line_x + 90);
	let (blurred, _) = draw(Some(24.0));
	let kept = around(&blurred) / around(&sharp);
	assert!((0.5..0.75).contains(&kept), "a 4 px line at Size 24 keeps {kept} of its shown light, the client 0.58-0.60");
	// the square's edge facing the line (the view's +X lies on its left)
	let (square_x, _) = pixel(&placed, Vec3::new(10.0, 0.0, -0.1));
	let edge = (square_x..WIDTH).find(|&x| sharp[middle * WIDTH + x] < 100.0).expect("the square's edge");
	let (small, _) = draw(Some(4.0));
	let halo = (middle - 20..middle + 20).map(|y| small[y * WIDTH + edge + 3]).sum::<f32>() / 40.0;
	assert!((4.0..20.0).contains(&halo), "3 px past a white square's edge at Size 4: {halo}, the client 15");
}

/// A sun 30 degrees off the view's axis gives rays of cos 30 degrees beside it, whatever the field of view
/// (the client: 0.866-0.869 through 50 and 90 degree views; ror's exp(-0.09 r²) of the sun's distance in
/// half heights gave 0.871 and 0.970); past the view's edge they fade further.
#[test]
fn sun_rays_fade_by_the_sun_s_angle_from_the_view_s_axis() {
	// a sun 40 degrees up (its rays' daytime colour) and a view turned from it by `degrees` sideways
	let sun = Vec3::new(0.0, 40f32.to_radians().sin(), -40f32.to_radians().cos());
	let fade = |degrees: f32, fov: f32| {
		let across = sun.cross(Vec3::X).normalize();
		let target = glam::Quat::from_axis_angle(across, degrees.to_radians()) * sun * 10.0;
		let placed = CameraSpec::LookAt { eye: [0.0, 0.0, 0.0], target: target.into(), fov }.place(None, 1644, 1116);
		let aspect = 1644.0 / 1116.0;
		let uniform = super::post::sun_rays_uniform((0.05, 1.0), sun, placed.view_projection(aspect), placed.ray_unprojection(aspect), aspect).expect("rays");
		// screen.z, after the 16 floats of the rays and the 4 of the sun
		let floats: &[f32] = bytemuck::cast_slice(bytemuck::bytes_of(&uniform));
		floats[22]
	};
	let cos30 = 30f32.to_radians().cos();
	for fov in [50.0f32, 90.0] {
		let faded = fade(30.0, fov);
		assert!((faded - cos30).abs() < 0.005, "the sun 30 degrees off a {fov} degree view's axis: fade {faded}, the client 0.866-0.869");
	}
	// 40 degrees off a 50 degree view's axis lies past its edge (34.5 degrees across): under cos 40
	let past = fade(40.0, 50.0);
	assert!(past < 40f32.to_radians().cos() - 0.02 && past > 0.65, "a sun past the view's edge: fade {past}, the client 0.73-0.75");
}
