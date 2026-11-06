//! Environment experiments: Atmosphere (the sky behind it and distant objects), the sky's elements
//! (sun, moon, stars, SkyboxOrientation), fog and the post effects (ColorCorrection, SunRays, Blur,
//! DepthOfField). notes/render.md "Atmosphere", "Sky elements", "Post effects";
//! tools/analysis/atmosphere_fit.py, post_fit.py.

use glam::Vec3;

use super::{Experiment, Probe, Run, SKY_DARK, SKY_WHITE};
use crate::render::camera::CameraSpec;
use crate::render::environment::sun_direction;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec, facing};
use crate::scene::environment::{Atmosphere, Sky};

/// The camera every atmosphere experiment looks from.
const EYE: Vec3 = Vec3::new(0.0, 10.0, 0.0);

/// Studio's default Atmosphere (a new instance's properties).
pub fn default_atmosphere() -> Atmosphere {
	Atmosphere {
		density: 0.395,
		offset: 0.0,
		color: Vec3::new(199.0, 170.0, 107.0) / 255.0,
		decay: Vec3::new(92.0, 60.0, 13.0) / 255.0,
		glare: 0.0,
		haze: 0.0,
	}
}

/// A dusk place's Atmosphere.
pub fn lobby_atmosphere() -> Atmosphere {
	Atmosphere {
		density: 0.375,
		offset: 0.0,
		color: Vec3::new(180.0, 225.0, 255.0) / 255.0,
		decay: Vec3::new(180.0, 225.0, 255.0) / 255.0,
		glare: 0.25,
		haze: 2.2,
	}
}

/// A skybox of one image on every face, without sun, moon or stars.
pub fn uniform_sky(image: &str) -> Sky {
	Sky { skybox: std::array::from_fn(|_| image.to_string()), celestial_bodies_shown: false, ..Default::default() }
}


/// The horizontal direction towards the sun at a clock time (latitude 0).
fn sun_azimuth(clock_time: f32) -> Vec3 {
	let sun = sun_direction(clock_time, 0.0);
	Vec3::new(sun.x, 0.0, sun.z).normalize()
}

/// A camera at the eye looking along `direction`.
fn view(direction: Vec3, fov: f32) -> CameraSpec {
	CameraSpec::LookAt { eye: EYE.into(), target: (EYE + direction * 100.0).into(), fov }
}

/// Distances of the tile rows (studs from the eye).
const DISTANCES: [f32; 10] = [8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0];

/// Rows of tile pairs facing the eye around a horizontal `forward`: one pair per distance, spread
/// across 80 degrees of azimuth, a row at each elevation. Each pair is a black Neon tile (radiance
/// 0: what the atmosphere adds) and a gray Neon tile (radiance 25.75 · 0.502⁴ = 1.635: what it lets
/// through), 0.05 · distance across (40 px at a 70 degree field of view).
fn tile_rows(model: &mut ModelBuilder, probes: &mut Vec<Probe>, row: &str, forward: Vec3, elevations: &[f32]) {
	tile_rows_at(model, probes, row, forward, elevations, &DISTANCES);
}

/// [`tile_rows`] at other distances.
fn tile_rows_at(model: &mut ModelBuilder, probes: &mut Vec<Probe>, row: &str, forward: Vec3, elevations: &[f32], distances: &[f32]) {
	tile_rows_about(model, probes, row, EYE, forward, elevations, distances);
}

/// [`tile_rows_at`] about another eye.
fn tile_rows_about(model: &mut ModelBuilder, probes: &mut Vec<Probe>, row: &str, eye: Vec3, forward: Vec3, elevations: &[f32], distances: &[f32]) {
	let right = forward.cross(Vec3::Y).normalize();
	for &elevation in elevations {
		for (index, &distance) in distances.iter().enumerate() {
			let azimuth = (-40.0 + 80.0 * index as f32 / (distances.len() - 1) as f32).to_radians();
			let (sin_e, cos_e) = elevation.to_radians().sin_cos();
			let direction = (forward * azimuth.cos() + right * azimuth.sin()) * cos_e + Vec3::Y * sin_e;
			let center = eye + direction * distance;
			let side = 0.05 * distance;
			let across = direction.cross(Vec3::Y).normalize();
			for (kind, gray, offset, x) in [("black", 0u8, -0.55f32, 0.0f32), ("gray", 128, 0.55, 1.635)] {
				let position = center + across * (offset * side);
				let label = format!("{row}-e{elevation}-d{distance}-{kind}");
				model.part(&PartSpec {
					name: label.clone(),
					cframe: facing(position, -direction),
					size: Vec3::new(side, side, side * 0.02),
					color: [gray; 3],
					material: 288,
					cast_shadow: false,
					..Default::default()
				});
				probes.push(Probe {
					label,
					position: (position - direction * side * 0.01).into(),
					inputs: serde_json::json!({"row": row, "kind": kind, "x": x, "distance": distance, "elevation": elevation, "azimuth": azimuth.to_degrees(), "direction": direction.to_array()}),
				});
			}
		}
	}
}


/// A skybox white in the faces `white` names (Bk, Dn, Ft, Lf, Rt, Up = 0..5) and dark elsewhere.
fn faces_sky(white: &[usize]) -> Sky {
	Sky { skybox: std::array::from_fn(|face| if white.contains(&face) { SKY_WHITE } else { SKY_DARK }.to_string()), celestial_bodies_shown: false, ..Default::default() }
}

/// An atmosphere of the given haze and glare, colours in 0..1.
fn atmosphere(density: f32, haze: f32, glare: f32, color: [f32; 3], decay: [f32; 3]) -> Atmosphere {
	Atmosphere { density, offset: 0.0, color: Vec3::from(color), decay: Vec3::from(decay), glare, haze }
}

/// The sky through the Atmosphere, with nothing in front of it: four horizontal views 90 degrees
/// apart starting at the sun's azimuth and one straight up (90 degree fields of view), over black
/// and white skyboxes (and skyboxes white in one face). Sweeps: Haze with an orange Color (blue
/// shows how much of the skybox is left, green how Color enters), Color, which faces of the skybox
/// light the haze, Glare, Decay, Density, Offset, the sun's Brightness and height,
/// EnvironmentDiffuseScale.
pub fn atmosphere_sky() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereSky");
	model.part(&PartSpec { name: "Anchor".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -2000.0, 0.0)), size: Vec3::splat(0.2), ..Default::default() });
	let white = [1.0f32; 3];
	let orange = [1.0f32, 0.5, 0.0];
	let black_sky = uniform_sky(SKY_DARK);
	let white_sky = uniform_sky(SKY_WHITE);
	let base = Environment::default();
	let mut settings: Vec<(String, Environment)> = Vec::new();
	let mut add = |label: String, sky: &Sky, atmosphere: Atmosphere, tweak: &dyn Fn(&mut Environment)| {
		let mut environment = Environment { sky: Some(sky.clone()), atmosphere: Some(atmosphere), ..base.clone() };
		tweak(&mut environment);
		settings.push((label, environment));
	};
	let none = |_: &mut Environment| {};
	for haze in [0.25f32, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0] {
		add(format!("haze{haze}-white"), &white_sky, atmosphere(0.395, haze, 0.0, orange, white), &none);
	}
	for haze in [1.0f32, 5.0] {
		add(format!("haze{haze}-black"), &black_sky, atmosphere(0.395, haze, 0.0, orange, white), &none);
	}
	add("color-a".into(), &white_sky, atmosphere(0.395, 10.0, 0.0, [0.25, 0.5, 0.75], white), &none);
	add("color-b".into(), &white_sky, atmosphere(0.395, 10.0, 0.0, [0.1, 0.9, 0.6], white), &none);
	for face in 0..6 {
		add(format!("face{face}"), &faces_sky(&[face]), atmosphere(0.395, 10.0, 0.0, white, white), &none);
	}
	add("face-not-up".into(), &faces_sky(&[0, 1, 2, 3, 4]), atmosphere(0.395, 10.0, 0.0, white, white), &none);
	add("diffuse0".into(), &white_sky, atmosphere(0.395, 10.0, 0.0, orange, white), &|environment: &mut Environment| environment.environment_diffuse_scale = 0.0);
	for glare in [0.25f32, 0.5, 1.0, 2.0, 5.0, 10.0] {
		add(format!("glare{glare}"), &black_sky, atmosphere(0.395, 2.0, glare, white, white), &|environment: &mut Environment| environment.exposure_compensation = -2.0);
	}
	add("glare1-white".into(), &white_sky, atmosphere(0.395, 2.0, 1.0, white, white), &|environment: &mut Environment| environment.exposure_compensation = -2.0);
	add("decay-orange".into(), &black_sky, atmosphere(0.395, 2.0, 1.0, white, orange), &|environment: &mut Environment| environment.exposure_compensation = -2.0);
	add("color-orange".into(), &black_sky, atmosphere(0.395, 2.0, 1.0, orange, white), &|environment: &mut Environment| environment.exposure_compensation = -2.0);
	for density in [0.1f32, 1.0] {
		add(format!("density{density}"), &black_sky, atmosphere(density, 2.0, 1.0, white, orange), &|environment: &mut Environment| environment.exposure_compensation = -2.0);
	}
	for offset in [0.5f32, 1.0] {
		for (label, sky) in [("black", &black_sky), ("white", &white_sky)] {
			add(format!("offset{offset}-{label}"), sky, Atmosphere { offset, ..atmosphere(0.395, 2.0, 1.0, white, orange) }, &|environment: &mut Environment| environment.exposure_compensation = -2.0);
		}
	}
	for brightness in [1.0f32, 4.0] {
		add(format!("brightness{brightness}"), &black_sky, atmosphere(0.395, 2.0, 1.0, white, orange), &move |environment: &mut Environment| {
			environment.exposure_compensation = -2.0;
			environment.brightness = brightness;
		});
	}
	for clock in [6.5f32, 9.0, 12.0, 17.0, 18.0, 19.0, 0.0] {
		add(format!("clock{clock}"), &black_sky, atmosphere(0.395, 2.0, 1.0, white, orange), &move |environment: &mut Environment| {
			environment.exposure_compensation = -2.0;
			environment.clock_time = clock;
		});
	}
	let toward = sun_azimuth(base.clock_time);
	let right = toward.cross(Vec3::Y).normalize();
	let views = [
		("a0", view(toward, 90.0)),
		("a90", view(right, 90.0)),
		("a180", view(-toward, 90.0)),
		("a270", view(-right, 90.0)),
		("up", view(Vec3::new(0.0, 1.0, 0.0001), 90.0)),
	];
	let mut runs = Vec::new();
	for (label, environment) in settings {
		for (view_label, camera) in &views {
			runs.push(Run { label: format!("{label}-{view_label}"), environment: environment.clone(), camera: camera.clone() });
		}
	}
	Experiment { name: "atmosphere-sky".into(), model, probes: Vec::new(), runs }
}

/// An empty fixture (one tiny part far below the eye).
fn empty(name: &str) -> ModelBuilder {
	let mut model = ModelBuilder::new(name);
	model.part(&PartSpec { name: "Anchor".into(), cframe: glam::Affine3A::from_translation(Vec3::new(0.0, -2000.0, 0.0)), size: Vec3::splat(0.2), ..Default::default() });
	model
}

/// How much of the skybox the haze hides by elevation, finely: a white skybox seen at EV -2 (its
/// radiance 0.48, mid-curve) through an orange Color (blue keeps only the skybox's share), Glare 0,
/// Haze from 0.1 to 10, towards the horizon and straight up.
pub fn atmosphere_haze() -> Experiment {
	let model = empty("AtmosphereHaze");
	let toward = sun_azimuth(14.5);
	let views = [("a0", view(toward, 90.0)), ("up", view(Vec3::new(0.0, 1.0, 0.0001), 90.0))];
	let mut runs = Vec::new();
	for haze in [0.0f32, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 2.05, 2.1, 2.15, 2.2, 2.4, 2.6, 2.8, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0, 10.0] {
		let environment = Environment {
			sky: Some(uniform_sky(SKY_WHITE)),
			atmosphere: Some(atmosphere(0.395, haze, 0.0, [1.0, 0.5, 0.0], [1.0; 3])),
			exposure_compensation: -2.0,
			..Environment::default()
		};
		for (view_label, camera) in &views {
			runs.push(Run { label: format!("haze{haze}-{view_label}"), environment: environment.clone(), camera: camera.clone() });
		}
	}
	Experiment { name: "atmosphere-haze".into(), model, probes: Vec::new(), runs }
}

/// The sun's light in the haze (Glare) and its hue away from the sun (Decay): a black skybox, Haze
/// 10 (the haze covers the sky up to ~50 degrees) and the sun low (ClockTime 17, 13.7 degrees up) so
/// every angle from the sun is hazed; four horizontal views from the sun's azimuth and one up, at
/// EV -2. Glare 0.25 to 4, Decay and Color in three channel mixes each, the haze's share (Haze 3),
/// the sun's height and the lobby's colours.
pub fn atmosphere_glare() -> Experiment {
	let model = empty("AtmosphereGlare");
	let clock = 17.0f32;
	let toward = sun_azimuth(clock);
	let right = toward.cross(Vec3::Y).normalize();
	let views = [
		("a0", view(toward, 90.0)),
		("a90", view(right, 90.0)),
		("a180", view(-toward, 90.0)),
		("a270", view(-right, 90.0)),
		("up", view(Vec3::new(0.0, 1.0, 0.0001), 90.0)),
	];
	let white = [1.0f32; 3];
	let mix_a = [0.25f32, 0.5, 0.75];
	let mix_b = [0.1f32, 0.9, 0.6];
	let lobby = lobby_atmosphere();
	let cases: Vec<(String, Atmosphere, f32)> = vec![
		("w-w-g1".into(), atmosphere(0.395, 10.0, 1.0, white, white), clock),
		("w-w-g0.25".into(), atmosphere(0.395, 10.0, 0.25, white, white), clock),
		("w-w-g4".into(), atmosphere(0.395, 10.0, 4.0, white, white), clock),
		("w-a-g1".into(), atmosphere(0.395, 10.0, 1.0, white, mix_a), clock),
		("w-b-g1".into(), atmosphere(0.395, 10.0, 1.0, white, mix_b), clock),
		("w-k-g1".into(), atmosphere(0.395, 10.0, 1.0, white, [0.0; 3]), clock),
		("a-w-g1".into(), atmosphere(0.395, 10.0, 1.0, mix_a, white), clock),
		("b-w-g1".into(), atmosphere(0.395, 10.0, 1.0, mix_b, white), clock),
		("a-a-g1".into(), atmosphere(0.395, 10.0, 1.0, mix_a, mix_a), clock),
		("a-b-g1".into(), atmosphere(0.395, 10.0, 1.0, mix_a, mix_b), clock),
		("w-a-g0.25".into(), atmosphere(0.395, 10.0, 0.25, white, mix_a), clock),
		("w-a-g4".into(), atmosphere(0.395, 10.0, 4.0, white, mix_a), clock),
		("w-a-g1-h3".into(), atmosphere(0.395, 3.0, 1.0, white, mix_a), clock),
		("w-a-g1-c16".into(), atmosphere(0.395, 10.0, 1.0, white, mix_a), 16.0),
		("w-a-g1-c17.5".into(), atmosphere(0.395, 10.0, 1.0, white, mix_a), 17.5),
		("lobby-h10".into(), Atmosphere { haze: 10.0, ..lobby }, clock),
		("lobby".into(), lobby, clock),
	];
	let mut runs = Vec::new();
	for (label, atmosphere, clock_time) in cases {
		let environment = Environment { sky: Some(uniform_sky(SKY_DARK)), atmosphere: Some(atmosphere), exposure_compensation: -2.0, clock_time, ..Environment::default() };
		for (view_label, camera) in &views {
			runs.push(Run { label: format!("{label}-{view_label}"), environment: environment.clone(), camera: camera.clone() });
		}
	}
	Experiment { name: "atmosphere-glare".into(), model, probes: Vec::new(), runs }
}


/// Does the Atmosphere depend on the eye's height? Tile rows (8 to 1024 studs, level and 10 degrees
/// down) about eyes at 10, 500, 1,000, 1,870 and 3,000 studs up, 20,000 studs apart, under a
/// cutscene's hazy Atmosphere (Density 0.3, Offset 0.25, Haze 2, Glare 0.3) and Studio's default at
/// Density 0.5, over a black skybox; each altitude's view towards the sun and level.
pub fn atmosphere_altitude() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereAltitude");
	let mut probes = Vec::new();
	let toward = sun_azimuth(14.5);
	let altitudes = [10.0f32, 500.0, 1000.0, 1870.0, 3000.0];
	let eye_at = |index: usize, altitude: f32| Vec3::new(index as f32 * 20_000.0, altitude, 0.0);
	for (index, altitude) in altitudes.iter().enumerate() {
		tile_rows_about(&mut model, &mut probes, &format!("h{altitude}"), eye_at(index, *altitude), toward, &[-10.0, 0.0], &DISTANCES[..8]);
	}
	let hazy = Atmosphere { offset: 0.25, ..atmosphere(0.3, 2.0, 0.3, [0.741, 0.878, 0.773], [0.522, 0.585, 0.463]) };
	let cases = [("hazy", hazy), ("default", Atmosphere { density: 0.5, ..default_atmosphere() })];
	let mut runs = Vec::new();
	for (label, atmosphere) in cases {
		let environment = Environment { sky: Some(uniform_sky(SKY_DARK)), atmosphere: Some(atmosphere), ..Environment::default() };
		for (index, altitude) in altitudes.iter().enumerate() {
			let eye = eye_at(index, *altitude);
			let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + toward * 100.0).into(), fov: 70.0 };
			runs.push(Run { label: format!("{label}-h{altitude}"), environment: environment.clone(), camera });
		}
	}
	Experiment { name: "atmosphere-altitude".into(), model, probes, runs }
}


/// What distant objects fade into, with the sun overhead (GeographicLatitude 23.5 at noon: the angle
/// to the sun is 90 degrees minus the elevation, the same all round): tile rows at elevations -10 to
/// 30 degrees, distances 8 to 4096, one view. Haze, Glare and Offset combinations over black and
/// white skyboxes, and Density 0.3 against 0.5 (does the haze's light follow the extinction?).
pub fn atmosphere_objects() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereObjects");
	let mut probes = Vec::new();
	tile_rows(&mut model, &mut probes, "row", Vec3::NEG_Z, &[-10.0, 0.0, 5.0, 15.0, 30.0]);
	let camera = view(Vec3::NEG_Z, 90.0);
	let black = Some(uniform_sky(SKY_DARK));
	let white = Some(uniform_sky(SKY_WHITE));
	let tan = [199.0 / 255.0, 170.0 / 255.0, 107.0 / 255.0];
	let orange = [1.0f32, 0.5, 0.0];
	let cases: Vec<(&str, Option<Sky>, Atmosphere)> = vec![
		("haze0-o0", white.clone(), atmosphere(0.5, 0.0, 0.0, tan, [1.0; 3])),
		("haze0-o1", white.clone(), Atmosphere { offset: 1.0, ..atmosphere(0.5, 0.0, 0.0, tan, [1.0; 3]) }),
		("haze2-o0", white.clone(), atmosphere(0.5, 2.0, 0.0, orange, [1.0; 3])),
		("haze2-o0.5", white.clone(), Atmosphere { offset: 0.5, ..atmosphere(0.5, 2.0, 0.0, orange, [1.0; 3]) }),
		("haze2-o1", white.clone(), Atmosphere { offset: 1.0, ..atmosphere(0.5, 2.0, 0.0, orange, [1.0; 3]) }),
		("glare-o0", black.clone(), atmosphere(0.5, 2.0, 1.0, [1.0; 3], [1.0; 3])),
		("glare-o0.5", black.clone(), Atmosphere { offset: 0.5, ..atmosphere(0.5, 2.0, 1.0, [1.0; 3], [1.0; 3]) }),
		("glare-o1", black.clone(), Atmosphere { offset: 1.0, ..atmosphere(0.5, 2.0, 1.0, [1.0; 3], [1.0; 3]) }),
		("glare-h10-o0", black.clone(), atmosphere(0.5, 10.0, 1.0, [1.0; 3], [1.0; 3])),
		("glare-h10-o1", black.clone(), Atmosphere { offset: 1.0, ..atmosphere(0.5, 10.0, 1.0, [1.0; 3], [1.0; 3]) }),
		("glare-h0.5-o0", black.clone(), atmosphere(0.5, 0.5, 1.0, [1.0; 3], [1.0; 3])),
		("glare-d0.3-o0", black.clone(), atmosphere(0.3, 2.0, 1.0, [1.0; 3], [1.0; 3])),
		("glare-d0.3-o1", black.clone(), Atmosphere { offset: 1.0, ..atmosphere(0.3, 2.0, 1.0, [1.0; 3], [1.0; 3]) }),
	];
	let runs = cases
		.into_iter()
		.map(|(label, sky, atmosphere)| Run {
			label: label.into(),
			environment: Environment { sky, atmosphere: Some(atmosphere), geographic_latitude: 23.5, clock_time: 12.0, exposure_compensation: -1.0, ..Environment::default() },
			camera: camera.clone(),
		})
		.collect();
	Experiment { name: "atmosphere-objects".into(), model, probes, runs }
}

/// The haze's light on objects across Haze and Offset: tile rows at eleven elevations from -30 to
/// 40 degrees, the sun overhead, a black skybox, Color and Decay white and Glare 1 (the glare then
/// depends on the elevation alone); Haze 0.5 to 10 at Offset 0 and 1, Offset steps at Haze 2, and
/// the haze's own colour (Glare 0, orange Color over a white skybox) at Haze 2.
pub fn atmosphere_objects2() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereObjects2");
	let mut probes = Vec::new();
	tile_rows(&mut model, &mut probes, "row", Vec3::NEG_Z, &[-30.0, -10.0, -3.0, 0.0, 3.0, 6.0, 10.0, 15.0, 22.0, 30.0, 40.0]);
	let camera = CameraSpec::LookAt { eye: EYE.into(), target: (EYE + Vec3::new(0.0, 5f32.to_radians().tan(), -1.0) * 100.0).into(), fov: 90.0 };
	let black = Some(uniform_sky(SKY_DARK));
	let white = Some(uniform_sky(SKY_WHITE));
	let mut cases: Vec<(String, Option<Sky>, Atmosphere)> = Vec::new();
	for haze in [0.5f32, 1.0, 2.0, 3.0, 5.0, 10.0] {
		for offset in [0.0f32, 1.0] {
			cases.push((format!("h{haze}-o{offset}"), black.clone(), Atmosphere { offset, ..atmosphere(0.5, haze, 1.0, [1.0; 3], [1.0; 3]) }));
		}
	}
	for offset in [0.25f32, 0.5, 0.75] {
		cases.push((format!("h2-o{offset}"), black.clone(), Atmosphere { offset, ..atmosphere(0.5, 2.0, 1.0, [1.0; 3], [1.0; 3]) }));
	}
	for offset in [0.0f32, 1.0] {
		cases.push((format!("color-h2-o{offset}"), white.clone(), Atmosphere { offset, ..atmosphere(0.5, 2.0, 0.0, [1.0, 0.5, 0.0], [1.0; 3]) }));
	}
	let runs = cases
		.into_iter()
		.map(|(label, sky, atmosphere)| Run {
			label,
			environment: Environment { sky, atmosphere: Some(atmosphere), geographic_latitude: 23.5, clock_time: 12.0, exposure_compensation: -1.0, ..Environment::default() },
			camera: camera.clone(),
		})
		.collect();
	Experiment { name: "atmosphere-objects2".into(), model, probes, runs }
}

/// Is the light distant objects fade into a blurred sky, sharper the farther they are? A skybox
/// white only in its Up image (its edge 45 degrees up towards -Z) without haze: tile rows at 20 to
/// 75 degrees around that edge, Density 0.5, Offset 0 and 1; and the same with Haze 2 and Glare 1.
pub fn atmosphere_mips() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereMips");
	let mut probes = Vec::new();
	tile_rows(&mut model, &mut probes, "row", Vec3::NEG_Z, &[20.0, 30.0, 38.0, 42.0, 45.0, 48.0, 52.0, 60.0, 75.0]);
	let camera = CameraSpec::LookAt { eye: EYE.into(), target: (EYE + Vec3::new(0.0, 1.0, -1.0) * 100.0).into(), fov: 90.0 };
	let sky = Some(faces_sky(&[5]));
	let mut runs = Vec::new();
	for offset in [0.0f32, 0.5, 1.0] {
		for (label, haze, glare) in [("clear", 0.0f32, 0.0f32), ("haze2", 2.0, 1.0)] {
			let atmosphere = Atmosphere { offset, ..atmosphere(0.5, haze, glare, [1.0; 3], [1.0; 3]) };
			runs.push(Run {
				label: format!("{label}-o{offset}"),
				environment: Environment { sky: sky.clone(), atmosphere: Some(atmosphere), geographic_latitude: 23.5, clock_time: 12.0, exposure_compensation: -1.0, ..Environment::default() },
				camera: camera.clone(),
			});
		}
	}
	Experiment { name: "atmosphere-mips".into(), model, probes, runs }
}

/// Does ColorShift_Top tint the Atmosphere's glare (and the sun's light)? The glare set-up (black
/// skybox, Haze 10, Glare 1, ClockTime 17, -2 EV) and the lobby's Atmosphere under ColorShift_Top
/// black, the lobby's pink, red and white, towards the sun and away; a white ball lit by the sun alone
/// in front of the camera (ColorShift_Bottom pink too).
pub fn atmosphere_shift() -> Experiment {
	let mut model = ModelBuilder::new("AtmosphereShift");
	let mut probes = Vec::new();
	let clock = 17.0f32;
	let toward = sun_azimuth(clock);
	let center = EYE + toward * 20.0 - Vec3::Y * 6.0;
	model.part(&PartSpec { name: "Ball".into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(5.0), color: [255; 3], material: 272, shape: Some(0), cast_shadow: false, ..Default::default() });
	probes.push(Probe { label: "ball".into(), position: (center - toward * 2.5).into(), inputs: serde_json::json!({"kind": "ball"}) });
	let views = [("a0", view(toward, 90.0)), ("a180", view(-toward, 90.0))];
	let pink = Vec3::new(1.0, 0.525, 0.675);
	let shifts = [("k", Vec3::ZERO), ("pink", pink), ("red", Vec3::new(1.0, 0.0, 0.0)), ("white", Vec3::ONE)];
	let white = [1.0f32; 3];
	let settings: [(&str, Option<Atmosphere>); 3] = [("none", None), ("glare", Some(atmosphere(0.0, 10.0, 1.0, white, white))), ("lobby", Some(Atmosphere { density: 0.0, ..lobby_atmosphere() }))];
	let lit = Environment {
		sky: Some(uniform_sky(SKY_DARK)),
		clock_time: clock,
		exposure_compensation: -2.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	};
	let mut runs = Vec::new();
	for (setting, atmosphere) in settings {
		for (shift_label, shift) in shifts {
			let environment = Environment { atmosphere, color_shift_top: shift, ..lit.clone() };
			for (view_label, camera) in &views {
				runs.push(Run { label: format!("{setting}-{shift_label}-{view_label}"), environment: environment.clone(), camera: camera.clone() });
			}
		}
	}
	runs.push(Run { label: "none-bottom-pink-a0".into(), environment: Environment { color_shift_bottom: pink, ..lit.clone() }, camera: views[0].1.clone() });
	// does the tint follow the sun's height? (the lobby's ClockTime is 14.5)
	for clock_time in [12.0f32, 14.5, 16.0, 17.5] {
		for (shift_label, shift) in [("k", Vec3::ZERO), ("pink", pink)] {
			let environment = Environment { atmosphere: Some(atmosphere(0.0, 10.0, 1.0, white, white)), color_shift_top: shift, clock_time, ..lit.clone() };
			runs.push(Run { label: format!("glare-{shift_label}-c{clock_time}"), environment, camera: view(Vec3::new(0.0, 0.3, 1.0), 70.0) });
		}
	}
	Experiment { name: "atmosphere-shift".into(), model, probes, runs }
}


/// Does ColorShift_Top tint the sun's light as it tints the glare? A white SmoothPlastic plate facing
/// up and a white ball, lit by the sun alone (ClockTime 14.5), seen from above, under ColorShift_Top
/// black, pink, red and white, with EnvironmentDiffuseScale 0 and 1; ColorShift_Bottom pink.
pub fn sun_shift() -> Experiment {
	let mut model = ModelBuilder::new("SunShift");
	let mut probes = Vec::new();
	let center = Vec3::new(0.0, 10.0, 0.0);
	model.part(&PartSpec { name: "Plate".into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::new(8.0, 0.4, 8.0), color: [255; 3], material: 272, cast_shadow: false, ..Default::default() });
	probes.push(Probe { label: "plate".into(), position: (center + Vec3::new(-2.0, 0.2, -2.0)).into(), inputs: serde_json::json!({"kind": "plate"}) });
	let ball = center + Vec3::new(1.5, 1.7, 1.5);
	model.part(&PartSpec { name: "Ball".into(), cframe: glam::Affine3A::from_translation(ball), size: Vec3::splat(3.0), color: [255; 3], material: 272, shape: Some(0), cast_shadow: false, ..Default::default() });
	let camera = CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 14.0, -6.0)).into(), target: center.into(), fov: 50.0 };
	let pink = Vec3::new(1.0, 0.525, 0.675);
	let sunlit = |diffuse: f32| Environment {
		clock_time: 14.5,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: diffuse,
		environment_specular_scale: 0.0,
		global_shadows: false,
		sky: Some(uniform_sky(SKY_DARK)),
		..Environment::default()
	};
	let mut runs = Vec::new();
	for diffuse in [0.0f32, 1.0] {
		for (label, shift) in [("k", Vec3::ZERO), ("pink", pink), ("red", Vec3::new(1.0, 0.0, 0.0)), ("white", Vec3::ONE)] {
			runs.push(Run { label: format!("d{diffuse}-{label}"), environment: Environment { color_shift_top: shift, ..sunlit(diffuse) }, camera: camera.clone() });
		}
	}
	runs.push(Run { label: "d0-bottom-pink".into(), environment: Environment { color_shift_bottom: pink, ..sunlit(0.0) }, camera: camera.clone() });
	Experiment { name: "sun-shift".into(), model, probes, runs }
}


/// ColorShift_Top on the sun's light through sunrise and sunset, when the sun's own colour is not
/// white: white plates facing +X, +Y and -Z lit by the sun alone over a dark skybox at -2 EV, at
/// 6.1-6.5, noon and 17.6-17.7, untinted, pink (a dawn cutscene's) and blue, at
/// EnvironmentDiffuseScale 0 and 1.
pub fn dawn_tint() -> Experiment {
	let mut model = ModelBuilder::new("DawnTint");
	let mut probes = Vec::new();
	let center = Vec3::new(0.0, 10.0, 0.0);
	for (label, normal) in [("px", Vec3::X), ("py", Vec3::Y), ("nz", Vec3::NEG_Z), ("nx", Vec3::NEG_X)] {
		let size = Vec3::splat(8.0) - normal.abs() * 7.6;
		model.part(&PartSpec { name: label.into(), cframe: glam::Affine3A::from_translation(center + normal * 4.2), size, color: [255; 3], material: 272, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label: label.into(), position: (center + normal * 4.4).into(), inputs: serde_json::json!({"normal": normal.to_array()}) });
	}
	let camera = CameraSpec::LookAt { eye: (center + Vec3::new(1.0, 1.0, -1.0).normalize() * 30.0).into(), target: center.into(), fov: 40.0 };
	let evening = CameraSpec::LookAt { eye: (center + Vec3::new(-1.0, 1.0, -1.0).normalize() * 30.0).into(), target: center.into(), fov: 40.0 };
	let mut runs = Vec::new();
	for clock in [6.1f32, 6.2, 6.3, 6.4, 6.5, 12.0, 17.6, 17.7] {
		for (shift_label, shift) in [("k", Vec3::ZERO), ("pink", Vec3::new(1.0, 0.349, 0.7725)), ("blue", Vec3::new(0.0, 0.0, 1.0))] {
			for diffuse in [0.0f32, 1.0] {
				let environment = Environment {
					ambient: Vec3::ZERO,
					outdoor_ambient: Vec3::ZERO,
					brightness: 2.0,
					clock_time: clock,
					geographic_latitude: 360.0,
					color_shift_top: shift,
					environment_diffuse_scale: diffuse,
					environment_specular_scale: 0.0,
					exposure_compensation: -2.0,
					global_shadows: false,
					sky: Some(uniform_sky(SKY_DARK)),
					..Environment::default()
				};
				let camera = if clock > 12.0 { evening.clone() } else { camera.clone() };
				runs.push(Run { label: format!("t{clock}-{shift_label}-d{diffuse}"), environment, camera });
			}
		}
	}
	Experiment { name: "dawn-tint".into(), model, probes, runs }
}

/// [`dawn_tint`]'s plates under nine ColorShift_Tops (primaries, mixes, half tones, that pink) at noon
/// with EnvironmentDiffuseScale 0 to 1 in quarters, and at 6.3 at 0 and 1: how the sky's diffuse
/// share changes the sun's tint.
pub fn shift_diffuse() -> Experiment {
	let base = dawn_tint();
	let model = base.model;
	let probes = base.probes;
	let noon = CameraSpec::LookAt { eye: (Vec3::new(0.0, 10.0, 0.0) + Vec3::new(1.0, 1.0, -1.0).normalize() * 30.0).into(), target: [0.0, 10.0, 0.0], fov: 40.0 };
	let shifts = [
		("k", [0.0f32, 0.0, 0.0]),
		("white", [1.0, 1.0, 1.0]),
		("r25", [0.25, 0.0, 0.0]),
		("r75", [0.75, 0.0, 0.0]),
		("g25", [0.25, 0.25, 0.25]),
		("g75", [0.75, 0.75, 0.75]),
		("rx25", [1.0, 0.25, 0.25]),
		("rx75", [1.0, 0.75, 0.75]),
		("rg", [1.0, 0.5, 0.0]),
		("half-blue", [0.5, 0.5, 1.0]),
		("red", [1.0, 0.0, 0.0]),
		("green", [0.0, 1.0, 0.0]),
		("blue", [0.0, 0.0, 1.0]),
		("cyan", [0.0, 1.0, 1.0]),
		("yellow", [1.0, 1.0, 0.0]),
		("gray", [0.5, 0.5, 0.5]),
		("darkred", [0.5, 0.0, 0.0]),
		("rose", [1.0, 0.5, 0.5]),
		("pink", [1.0, 0.349, 0.7725]),
	];
	let mut runs = Vec::new();
	for (clock, diffuses) in [(12.0f32, vec![0.0f32, 0.25, 0.5, 0.75, 1.0]), (6.3, vec![0.0, 1.0])] {
		for diffuse in diffuses {
			for (label, shift) in shifts {
				let environment = Environment {
					ambient: Vec3::ZERO,
					outdoor_ambient: Vec3::ZERO,
					brightness: 2.0,
					clock_time: clock,
					geographic_latitude: 360.0,
					color_shift_top: Vec3::from(shift),
					environment_diffuse_scale: diffuse,
					environment_specular_scale: 0.0,
					exposure_compensation: -2.0,
					global_shadows: false,
					sky: Some(uniform_sky(SKY_DARK)),
					..Environment::default()
				};
				runs.push(Run { label: format!("t{clock}-d{diffuse}-{label}"), environment, camera: noon.clone() });
			}
		}
	}
	Experiment { name: "shift-diffuse".into(), model, probes, runs }
}


/// Lighting's fog without an Atmosphere: tile rows (black and gray Neon) at 5 to 150 studs over the
/// default sky, FogStart / FogEnd 0-100, 50-150 and 100-100, FogColor gray, red, white and black, at
/// -1 EV, without sun or ambient light, and with an Atmosphere of Density 0 added (does it switch the
/// fog off?); towards the horizon and straight up (does the sky fog?).
pub fn fog() -> Experiment {
	let mut model = ModelBuilder::new("Fog");
	let mut probes = Vec::new();
	tile_rows_at(&mut model, &mut probes, "row", Vec3::NEG_Z, &[-15.0, 0.0, 15.0], &[5.0, 15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 105.0, 120.0, 150.0]);
	let views = [("a0", view(Vec3::NEG_Z, 70.0)), ("up", view(Vec3::new(0.0, 1.0, 0.0001), 90.0))];
	let fog = |start: f32, end: f32, color: [f32; 3]| Environment { fog_start: start, fog_end: end, fog_color: Vec3::from(color), ..Environment::default() };
	let gray = [0.5f32; 3];
	let cases: Vec<(&str, Environment)> = vec![
		("none", Environment::default()),
		("gray-0-100", fog(0.0, 100.0, gray)),
		("gray-50-150", fog(50.0, 150.0, gray)),
		("gray-100-100", fog(100.0, 100.0, gray)),
		("red-0-100", fog(0.0, 100.0, [1.0, 0.0, 0.0])),
		("white-0-100", fog(0.0, 100.0, [1.0; 3])),
		("black-0-100", fog(0.0, 100.0, [0.0; 3])),
		("gray-0-100-ev-1", Environment { exposure_compensation: -1.0, ..fog(0.0, 100.0, gray) }),
		("gray-0-100-dark", Environment { brightness: 0.0, ambient: Vec3::ZERO, outdoor_ambient: Vec3::ZERO, environment_diffuse_scale: 0.0, environment_specular_scale: 0.0, ..fog(0.0, 100.0, gray) }),
		("gray-0-100-atmosphere", Environment { atmosphere: Some(Atmosphere { density: 0.0, ..default_atmosphere() }), ..fog(0.0, 100.0, gray) }),
		// which applies first: the fog, then the Atmosphere's fade into a black sky, or the reverse
		("gray-0-100-dense-black", Environment { atmosphere: Some(Atmosphere { density: 0.5, ..default_atmosphere() }), sky: Some(uniform_sky(SKY_DARK)), ..fog(0.0, 100.0, gray) }),
		("dense-black", Environment { atmosphere: Some(Atmosphere { density: 0.5, ..default_atmosphere() }), sky: Some(uniform_sky(SKY_DARK)), ..Environment::default() }),
	];
	let mut runs = Vec::new();
	for (label, environment) in cases {
		for (view_label, camera) in &views {
			runs.push(Run { label: format!("{label}-{view_label}"), environment: environment.clone(), camera: camera.clone() });
		}
	}
	Experiment { name: "fog".into(), model, probes, runs }
}

/// The fog where it is thin (atmosphere-explore: black tiles 16-2048 studs away under the default
/// FogEnd of 100000 one level darker in Studio than u (1 + 0.325 (1 - u)) gave): black Neon tiles
/// (radiance 0, so the fog alone) at 16 distances from 16 to 4096 studs, white FogColor, FogEnd
/// 100000 and 1000 at EV 0 (the sky blooms at +2 EV). With the tone curve's refitted toe both follow
/// the law, 2-6 % over it where u is small: the darker level was the toe, not the fog.
pub fn fog_near() -> Experiment {
	let mut model = ModelBuilder::new("FogNear");
	let mut probes = Vec::new();
	let distances = [16.0f32, 32.0, 48.0, 64.0, 96.0, 128.0, 192.0, 256.0, 384.0, 512.0, 768.0, 1024.0, 1536.0, 2048.0, 3072.0, 4096.0];
	let forward = Vec3::NEG_Z;
	let right = forward.cross(Vec3::Y).normalize();
	for (index, &distance) in distances.iter().enumerate() {
		// two rows of eight across 70 degrees of azimuth, 6 degrees above and below the horizon
		let (row, column) = (index / 8, index % 8);
		let azimuth = (-35.0 + 70.0 * column as f32 / 7.0).to_radians();
		let elevation = if row == 0 { 6.0f32 } else { -6.0 }.to_radians();
		let direction = (forward * azimuth.cos() + right * azimuth.sin()) * elevation.cos() + Vec3::Y * elevation.sin();
		let center = EYE + direction * distance;
		let label = format!("black-d{distance}");
		model.part(&PartSpec {
			name: label.clone(),
			cframe: facing(center, -direction),
			size: Vec3::new(0.07 * distance, 0.07 * distance, 0.01 * distance),
			color: [0, 0, 0],
			material: 288,
			cast_shadow: false,
			..Default::default()
		});
		probes.push(Probe { label, position: (center - direction * 0.005 * distance).into(), inputs: serde_json::json!({"distance": distance}) });
	}
	let camera = view(forward, 90.0);
	let fog = |end: f32, exposure: f32| Environment { fog_start: 0.0, fog_end: end, fog_color: Vec3::ONE, exposure_compensation: exposure, ..Environment::default() };
	let runs = vec![
		Run { label: "end100000-ev0".into(), environment: fog(100000.0, 0.0), camera: camera.clone() },
		Run { label: "end1000-ev0".into(), environment: fog(1000.0, 0.0), camera },
	];
	Experiment { name: "fog-near".into(), model, probes, runs }
}

/// The tone curve's toe and the fog where it is thin, apart: 24 gray SmoothPlastic tiles (colour 0 to
/// 255, many dark) 30 studs away under flat Ambient alone, without fog (FogEnd 10^8) at EV 0, -2 and
/// -4, and with the default fog (FogEnd 100000).
pub fn tone_toe() -> Experiment {
	let mut model = ModelBuilder::new("ToneToe");
	let mut probes = Vec::new();
	let grays: [u8; 24] = [0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 255];
	let center = Vec3::new(0.0, 10.0, -30.0);
	for (index, gray) in grays.iter().enumerate() {
		let (row, column) = (index / 6, index % 6);
		let position = center + Vec3::new((column as f32 - 2.5) * 3.0, (1.5 - row as f32) * 3.0, 0.0);
		let label = format!("gray{gray}");
		model.part(&PartSpec { name: label.clone(), cframe: glam::Affine3A::from_translation(position), size: Vec3::new(2.0, 2.0, 0.2), color: [*gray; 3], material: 272, cast_shadow: false, ..Default::default() });
		probes.push(Probe { label, position: (position + Vec3::Z * 0.1).into(), inputs: serde_json::json!({"gray": gray}) });
	}
	let camera = CameraSpec::LookAt { eye: EYE.into(), target: center.into(), fov: 30.0 };
	let flat = |fog_end: f32, exposure: f32| Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.5),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		fog_end,
		exposure_compensation: exposure,
		..Environment::default()
	};
	let runs = vec![
		Run { label: "nofog-ev0".into(), environment: flat(1.0e8, 0.0), camera: camera.clone() },
		Run { label: "nofog-ev-2".into(), environment: flat(1.0e8, -2.0), camera: camera.clone() },
		Run { label: "nofog-ev-4".into(), environment: flat(1.0e8, -4.0), camera: camera.clone() },
		Run { label: "fog-ev0".into(), environment: flat(100000.0, 0.0), camera },
	];
	Experiment { name: "tone-toe".into(), model, probes, runs }
}

/// The sky's elements: the sun (ClockTime 16.5, 20.6 degrees up) over black, white and the default
/// skyboxes at SunAngularSize 5 to 40, Brightness 1 to 4, -2 EV, a white square for its texture,
/// behind haze, and hidden; the moon (ClockTime 22) at MoonAngularSize 11 and 30 and as a white
/// square; stars at midnight (StarCount 0, 3000, 10000; at noon and dusk); a white skybox from dawn to
/// night (does the sky dim?); SkyboxOrientation turned four ways.
pub fn sky_elements() -> Experiment {
	let model = empty("SkyElements");
	let black = uniform_sky(SKY_DARK);
	let shown = |sky: Sky| Sky { celestial_bodies_shown: true, ..sky };
	let toward = |direction: Vec3, fov: f32| CameraSpec::LookAt { eye: EYE.into(), target: (EYE + direction * 100.0).into(), fov };
	let sun = sun_direction(16.5, 0.0);
	let moon = crate::render::environment::moon_direction(22.0, 0.0);
	let at_sun = toward(sun, 30.0);
	let at_moon = toward(moon, 30.0);
	let up = view(Vec3::new(0.0, 1.0, 0.0001), 90.0);
	let base = Environment { clock_time: 16.5, ..Environment::default() };
	let mut runs = Vec::new();
	let mut add = |label: String, environment: Environment, camera: &CameraSpec| runs.push(Run { label, environment, camera: camera.clone() });
	add("sun-default".into(), Environment { sky: None, ..base.clone() }, &at_sun);
	add("sun-black".into(), Environment { sky: Some(shown(black.clone())), ..base.clone() }, &at_sun);
	for size in [5.0f32, 10.0, 40.0] {
		add(format!("sun-black-size{size}"), Environment { sky: Some(Sky { sun_angular_size: size, ..shown(black.clone()) }), ..base.clone() }, &at_sun);
	}
	for brightness in [1.0f32, 4.0] {
		add(format!("sun-black-b{brightness}"), Environment { sky: Some(shown(black.clone())), brightness, ..base.clone() }, &at_sun);
	}
	add("sun-black-ev-2".into(), Environment { sky: Some(shown(black.clone())), exposure_compensation: -2.0, ..base.clone() }, &at_sun);
	add("sun-black-off".into(), Environment { sky: Some(black.clone()), ..base.clone() }, &at_sun);
	add("sun-black-square".into(), Environment { sky: Some(Sky { sun_texture: SKY_WHITE.into(), ..shown(black.clone()) }), exposure_compensation: -2.0, ..base.clone() }, &at_sun);
	add("sun-white".into(), Environment { sky: Some(shown(uniform_sky(SKY_WHITE))), ..base.clone() }, &at_sun);
	add("sun-haze5".into(), Environment { sky: Some(shown(black.clone())), atmosphere: Some(atmosphere(0.0, 5.0, 0.0, [1.0; 3], [1.0; 3])), ..base.clone() }, &at_sun);
	let night = Environment { clock_time: 22.0, ..Environment::default() };
	add("moon-black".into(), Environment { sky: Some(shown(black.clone())), ..night.clone() }, &at_moon);
	add("moon-black-size30".into(), Environment { sky: Some(Sky { moon_angular_size: 30.0, ..shown(black.clone()) }), ..night.clone() }, &at_moon);
	add("moon-black-square".into(), Environment { sky: Some(Sky { moon_texture: SKY_WHITE.into(), ..shown(black.clone()) }), ..night.clone() }, &at_moon);
	add("moon-default".into(), Environment { sky: None, ..night.clone() }, &at_moon);
	for (label, clock, count) in [("stars-3000", 0.0f32, 3000), ("stars-0", 0.0, 0), ("stars-10000", 0.0, 10000), ("stars-noon", 12.0, 3000), ("stars-dusk", 18.5, 3000)] {
		add(label.into(), Environment { sky: Some(Sky { star_count: count, ..shown(black.clone()) }), clock_time: clock, ..Environment::default() }, &up);
	}
	let away = view(Vec3::new(0.0, 0.3, 1.0).normalize(), 70.0);
	for clock in [5.5f32, 6.0, 6.25, 6.5, 7.0, 12.0, 17.0, 17.5, 17.75, 18.0, 18.25, 18.5, 19.0, 20.0, 0.0] {
		add(format!("dim{clock}"), Environment { sky: Some(uniform_sky(SKY_WHITE)), clock_time: clock, ..Environment::default() }, &away);
	}
	let plain = Sky { celestial_bodies_shown: false, ..Default::default() };
	for (label, orientation) in [("orient-y90", [0.0f32, 90.0, 0.0]), ("orient-x30", [30.0, 0.0, 0.0]), ("orient-z45", [0.0, 0.0, 45.0]), ("orient-xyz", [10.0, 20.0, 30.0])] {
		let environment = Environment { sky: Some(Sky { skybox_orientation: Vec3::from(orientation), ..plain.clone() }), ..Environment::default() };
		add(format!("{label}-front"), environment.clone(), &view(Vec3::NEG_Z, 90.0));
		add(format!("{label}-up"), environment, &up);
	}
	Experiment { name: "sky-elements".into(), model, probes: Vec::new(), runs }
}


/// The skybox through the day: a white skybox (no sun or moon) and a white ball lit by the sky alone
/// in front of it, at ClockTimes through dawn, dusk and the night (Studio tints and dims the skybox
/// by the time of day; does the sky light follow?).
pub fn sky_time() -> Experiment {
	let mut model = ModelBuilder::new("SkyTime");
	let mut probes = Vec::new();
	let center = EYE + Vec3::new(0.0, -3.0, 20.0);
	model.part(&PartSpec { name: "Ball".into(), cframe: glam::Affine3A::from_translation(center), size: Vec3::splat(4.0), color: [255; 3], material: 272, shape: Some(0), cast_shadow: false, ..Default::default() });
	probes.push(Probe { label: "ball".into(), position: (center - Vec3::Z * 2.0).into(), inputs: serde_json::json!({"kind": "ball"}) });
	let camera = CameraSpec::LookAt { eye: EYE.into(), target: (EYE + Vec3::new(0.0, 0.0, 1.0) * 100.0).into(), fov: 50.0 };
	let mut runs = Vec::new();
	let clocks = [4.0f32, 4.5, 5.0, 5.25, 5.5, 5.75, 6.0, 6.125, 6.25, 6.375, 6.5, 12.0, 17.5, 17.625, 17.75, 17.875, 18.0, 18.25, 18.5, 18.75, 19.0, 19.5, 20.0, 21.0, 22.0, 23.0, 0.0, 2.0];
	for clock in clocks {
		let environment = Environment {
			sky: Some(uniform_sky(SKY_WHITE)),
			clock_time: clock,
			brightness: 0.0,
			ambient: Vec3::ZERO,
			outdoor_ambient: Vec3::ZERO,
			environment_specular_scale: 0.0,
			global_shadows: false,
			..Environment::default()
		};
		runs.push(Run { label: format!("t{clock}"), environment, camera: camera.clone() });
	}
	Experiment { name: "sky-time".into(), model, probes, runs }
}

/// The tinted skybox by elevation: a white skybox seen straight up, 45 and 15 degrees up, at the
/// horizon and 15, 45 and 90 degrees down, at dusk, at night and at dawn.
pub fn sky_night() -> Experiment {
	let model = empty("SkyNight");
	let mut runs = Vec::new();
	for clock in [17.75f32, 18.0, 18.5, 20.0, 0.0, 5.5, 6.0, 6.25] {
		for pitch in [89.9f32, 45.0, 15.0, 0.0, -15.0, -45.0, -89.9] {
			let direction = Vec3::new(0.0, pitch.to_radians().sin(), pitch.to_radians().cos());
			let environment = Environment { sky: Some(uniform_sky(SKY_WHITE)), clock_time: clock, ..Environment::default() };
			runs.push(Run { label: format!("t{clock}-p{pitch}"), environment, camera: view(direction, 20.0) });
		}
	}
	Experiment { name: "sky-night".into(), model, probes: Vec::new(), runs }
}


/// Where the Atmosphere's glare sits by night and how bright it is around sunrise and sunset
/// (atmosphere-sky: about the sun below the horizon at 18 and 19 o'clock, about the moon at midnight,
/// 0.22 of the day's from 18): atmosphere-sky's dark skybox and haze (Density 0.395, Haze 2, Glare 1,
/// Decay (1, 0.5, 0): red even, green a broad lobe, blue a narrow one) at ClockTime 17.25-22 and
/// 2-6.75, and at latitude 45 at 19.5, 0 and 4.5, seen towards -X and +X, where the sun and the moon
/// lie, at -2 EV.
pub fn atmosphere_night_glare() -> Experiment {
	let model = empty("AtmosphereNightGlare");
	let environment = Environment {
		sky: Some(uniform_sky(SKY_DARK)),
		atmosphere: Some(atmosphere(0.395, 2.0, 1.0, [1.0; 3], [1.0, 0.5, 0.0])),
		exposure_compensation: -2.0,
		..Environment::default()
	};
	let mut runs = Vec::new();
	let times = [17.25f32, 17.5, 17.6, 17.75, 17.9, 19.25, 19.5, 19.75, 20.0, 22.0, 2.0, 4.0, 4.5, 4.75, 5.0, 5.25, 5.5, 6.1, 6.25, 6.4, 6.5, 6.75];
	let cases = times.iter().map(|&clock_time| (format!("t{clock_time}"), clock_time, 0.0f32)).chain([19.5f32, 0.0, 4.5].iter().map(|&clock_time| (format!("t{clock_time}-lat45"), clock_time, 45.0)));
	for (label, clock_time, geographic_latitude) in cases {
		for (side, direction) in [("west", Vec3::NEG_X), ("east", Vec3::X)] {
			runs.push(Run { label: format!("{label}-{side}"), environment: Environment { clock_time, geographic_latitude, ..environment.clone() }, camera: view(direction, 90.0) });
		}
	}
	Experiment { name: "atmosphere-night-glare".into(), model, probes: Vec::new(), runs }
}
