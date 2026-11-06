//! Sun shadow experiments: straight shadow edges on white floors and tilted boards under the sun
//! alone, measured across each edge at full resolution (notes/render.md "Shadows";
//! tools/analysis/shadow_profiles.py), and a model's sun term from a camera (`shadow_view`). Each
//! edge is one probe whose inputs give the edge's point on the receiver, the unit normal towards its
//! lit side and the map-plane studs per receiver stud across it; the analysis reads the profile from
//! the images.

use glam::{Affine3A, Vec3};

use super::{Experiment, Probe, Run, sun_only};
use crate::render::camera::CameraSpec;
use crate::render::environment::sun_direction;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};

/// The default sun and the axes of its shadow map's plane: (towards the sun, the horizontal
/// direction towards it, the horizontal direction across it (screen right looking towards the
/// sun), the projection of world up).
fn sun_axes() -> (Vec3, Vec3, Vec3, Vec3) {
	let environment = Environment::default();
	let sun = sun_direction(environment.clock_time, environment.geographic_latitude);
	let towards = Vec3::new(sun.x, 0.0, sun.z).normalize();
	let across = towards.cross(Vec3::Y).normalize();
	let up = (Vec3::Y - sun * sun.y).normalize();
	(sun, towards, across, up)
}

/// Sun alone at Brightness 2 with shadows of the given softness.
fn sun_shadows(softness: f32) -> Environment {
	Environment { global_shadows: true, shadow_softness: softness, ..sun_only(2.0) }
}

fn white_floor(model: &mut ModelBuilder, center: Vec3, size: f32) {
	model.part(&PartSpec {
		name: "Floor".into(),
		cframe: Affine3A::from_translation(center - Vec3::Y),
		size: Vec3::new(size, 2.0, size),
		color: [255; 3],
		material: 272,
		..Default::default()
	});
}

/// Map-plane studs per receiver stud across an edge: `along` the edge's direction on the receiver,
/// `normal` across it (both on the receiver), `sun` towards the sun.
fn map_scale(sun: Vec3, along: Vec3, normal: Vec3) -> f32 {
	let (a, n) = (along.dot(sun), normal.dot(sun));
	(1.0 - n * n / (1.0 - a * a).max(1e-6)).max(0.0).sqrt()
}

/// One straight shadow edge on a receiver: its point, the unit normal towards the lit side and the
/// direction along it (both on the receiver surface), and the occluder's distance along the sun ray.
fn edge(probes: &mut Vec<Probe>, label: String, point: Vec3, normal: Vec3, along: Vec3, distance: f32, extra: serde_json::Value) {
	let (sun, ..) = sun_axes();
	let mut inputs = serde_json::json!({
		"edge": label,
		"point": point.to_array(),
		"normal": normal.to_array(),
		"along": along.to_array(),
		"mapScale": map_scale(sun, along, normal),
		"distance": distance,
	});
	if let (Some(inputs), serde_json::Value::Object(extra)) = (inputs.as_object_mut(), extra) {
		inputs.extend(extra);
	}
	probes.push(Probe { label, position: point.into(), inputs });
}

/// Cascades: the same filter measured at many distances from the camera in one image. Ten pillars
/// facing the sun stand in a fan in front of a camera that looks towards the sun's azimuth from 15
/// studs up, 18-268 studs away (width and height grow beyond 40 studs so the shadow stays a strip
/// wider than its penumbra); their vertical edges cast shadow edges along the azimuth, towards the
/// camera, whose profile runs across the image (no foreshortening, no depth gradient across them).
/// Two fields of view (60 and 30 degrees, the same eye), ShadowSoftness 0, 0.2 and 1.
pub fn shadow_cascades() -> Experiment {
	let (_, towards, across, _) = sun_axes();
	let mut model = ModelBuilder::new("ShadowCascades");
	white_floor(&mut model, towards * 300.0, 1200.0);
	let mut probes = Vec::new();
	let (sun, ..) = sun_axes();
	let tan_elevation = sun.y / Vec3::new(sun.x, 0.0, sun.z).length();
	for index in 0..10 {
		let distance = 18.0 * 1.35f32.powi(index);
		let angle = -0.42 + 0.84 * index as f32 / 9.0;
		let scale = (distance / 40.0).max(1.0);
		let (width, height) = (1.6 * scale, 8.0 * scale);
		let base = (towards * angle.cos() + across * angle.sin()) * distance;
		let label = format!("p{index}");
		model.part(&PartSpec {
			name: label.clone(),
			cframe: Affine3A::from_cols(across.into(), Vec3::Y.into(), (-towards).into(), (base + Vec3::Y * height * 0.5).into()),
			size: Vec3::new(width, height, width),
			color: [160; 3],
			material: 272,
			..Default::default()
		});
		// the shadow runs from the pillar's camera-side face towards the camera, height / tan(elevation)
		let length = height / tan_elevation;
		for (side, sign) in [("l", -1.0f32), ("r", 1.0)] {
			for fraction in [0.3f32, 0.5, 0.7] {
				let behind = length * fraction;
				let point = base - towards * (width * 0.5 + behind) + across * (sign * width * 0.5) + Vec3::Y * 0.001;
				edge(
					&mut probes,
					format!("{label}{side}{fraction}"),
					point,
					across * sign,
					-towards,
					behind * (1.0 + tan_elevation * tan_elevation).sqrt(),
					serde_json::json!({"pillar": index, "pillarDistance": distance, "width": width, "half": (width * 0.45).min(1.5)}),
				);
			}
		}
	}
	let eye = Vec3::new(0.0, 15.0, 0.0);
	let mut runs = Vec::new();
	for (label, pitch, fov) in [("fov60", 30.0f32, 60.0f32), ("fov30", 14.0, 30.0)] {
		let pitch = pitch.to_radians();
		let target = eye + (towards * pitch.cos() - Vec3::Y * pitch.sin()) * 40.0;
		let camera = CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov };
		for softness in [0.0f32, 0.2, 1.0] {
			runs.push(Run { label: format!("{label}-s{softness}"), environment: sun_shadows(softness), camera: camera.clone() });
		}
	}
	Experiment { name: "shadow-cascades".into(), model, probes, runs }
}

/// Where each cascade is: long bars 30 studs up (above every camera, out of view) along the sun's
/// azimuth, 3 studs wide every 6, stripe the whole floor with shadow edges whose profile is the
/// filter in the map plane; the penumbra width along every edge maps the cascades over the image.
/// Cameras of several fields of view, pitches and heights looking towards the sun's azimuth, one
/// looking across it, at ShadowSoftness 0 (the cascades' texel size alone) and 0.2.
pub fn shadow_cascade_map() -> Experiment {
	let (_, towards, across, _) = sun_axes();
	let mut model = ModelBuilder::new("ShadowCascadeMap");
	white_floor(&mut model, towards * 300.0, 2000.0);
	let mut probes = Vec::new();
	let (start, end) = (-80.0f32, 700.0f32);
	for index in -40..=40 {
		let lateral = index as f32 * 6.0;
		let center = across * lateral + towards * (start + end) * 0.5 + Vec3::Y * 30.5;
		model.part(&PartSpec {
			name: format!("Bar{index}"),
			cframe: Affine3A::from_cols(across.into(), Vec3::Y.into(), (-towards).into(), center.into()),
			size: Vec3::new(3.0, 1.0, end - start),
			color: [160; 3],
			material: 272,
			..Default::default()
		});
		for (side, sign) in [("l", -1.0f32), ("r", 1.0)] {
			// a reference point of the stripe edge 60 studs out; the analysis scans along it
			let point = across * (lateral + sign * 1.5) + towards * 60.0 + Vec3::Y * 0.001;
			edge(&mut probes, format!("b{index}{side}"), point, across * sign, towards, 41.3, serde_json::json!({"bar": index, "half": 1.2}));
		}
	}
	let mut runs = Vec::new();
	// (label, eye height, pitch down, field of view, yaw from the sun's azimuth)
	for (label, height, pitch, fov, yaw) in [
		("fov60", 15.0f32, 30.0f32, 60.0f32, 0.0f32),
		("fov30", 15.0, 14.0, 30.0, 0.0),
		("fov70-low", 6.0, 18.0, 70.0, 0.0),
		("fov45-high", 28.0, 40.0, 45.0, 0.0),
		("fov60-across", 15.0, 30.0, 60.0, 90.0),
	] {
		let (pitch, yaw) = (pitch.to_radians(), yaw.to_radians());
		let heading = towards * yaw.cos() + across * yaw.sin();
		let eye = Vec3::Y * height;
		let target = eye + (heading * pitch.cos() - Vec3::Y * pitch.sin()) * 40.0;
		let camera = CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov };
		runs.push(Run { label: format!("{label}-s0"), environment: sun_shadows(0.0), camera: camera.clone() });
		if label == "fov60" {
			runs.push(Run { label: format!("{label}-s0.2"), environment: sun_shadows(0.2), camera });
		}
	}
	Experiment { name: "shadow-cascade-map".into(), model, probes, runs }
}

/// A pillar facing the sun standing at `base`, with probes on its shadow's two vertical-edge
/// shadows at a third and two thirds of the shadow's length.
fn sun_pillar(model: &mut ModelBuilder, probes: &mut Vec<Probe>, label: &str, base: Vec3, width: f32, height: f32, extra: serde_json::Value) {
	let (sun, towards, across, _) = sun_axes();
	let tan_elevation = sun.y / Vec3::new(sun.x, 0.0, sun.z).length();
	model.part(&PartSpec {
		name: label.into(),
		cframe: Affine3A::from_cols(across.into(), Vec3::Y.into(), (-towards).into(), (base + Vec3::Y * height * 0.5).into()),
		size: Vec3::new(width, height, width),
		color: [160; 3],
		material: 272,
		..Default::default()
	});
	let length = height / tan_elevation;
	for (side, sign) in [("l", -1.0f32), ("r", 1.0)] {
		for fraction in [0.35f32, 0.65] {
			let behind = length * fraction;
			let point = base - towards * (width * 0.5 + behind) + across * (sign * width * 0.5) + Vec3::Y * 0.001;
			let mut inputs = serde_json::json!({"half": (width * 0.45).min(1.5), "length": length, "width": width});
			if let (Some(inputs), serde_json::Value::Object(extra)) = (inputs.as_object_mut(), extra.clone()) {
				inputs.extend(extra);
			}
			edge(probes, format!("{label}{side}{fraction}"), point, across * sign, -towards, behind * (1.0 + tan_elevation * tan_elevation).sqrt(), inputs);
		}
	}
}

/// The filter against ShadowSoftness inside three cascades of one view: pillars facing the sun 20
/// studs from the camera (the cascade round the camera), 58 (the view's first cascade ahead) and 110
/// (the next), seen from 10 studs up through a 30 degree field of view, ShadowSoftness 0-1.
pub fn shadow_softness() -> Experiment {
	let (_, towards, across, _) = sun_axes();
	let mut model = ModelBuilder::new("ShadowSoftness");
	white_floor(&mut model, towards * 60.0, 400.0);
	let mut probes = Vec::new();
	// (group, distance, angles, width, height)
	for (group, distance, angles, width, height) in [(0, 20.0f32, [-0.3f32, -0.18], 1.6f32, 8.0f32), (1, 58.0, [-0.06, 0.06], 2.0, 10.0), (2, 110.0, [0.18, 0.3], 3.0, 14.0)] {
		for (index, angle) in angles.iter().enumerate() {
			let base = (towards * angle.cos() + across * angle.sin()) * distance;
			sun_pillar(&mut model, &mut probes, &format!("g{group}p{index}"), base, width, height, serde_json::json!({"group": group, "pillarDistance": distance}));
		}
	}
	let eye = Vec3::new(0.0, 10.0, 0.0);
	let pitch = 19f32.to_radians();
	let target = eye + (towards * pitch.cos() - Vec3::Y * pitch.sin()) * 40.0;
	let camera = CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov: 30.0 };
	let runs = [0.0f32, 0.05, 0.1, 0.2, 0.3, 0.5, 0.75, 1.0]
		.iter()
		.map(|&softness| Run { label: format!("s{softness}"), environment: sun_shadows(softness), camera: camera.clone() })
		.collect();
	Experiment { name: "shadow-softness".into(), model, probes, runs }
}

/// Cascade radii seen straight down: bars 200 studs up along the sun's azimuth stripe the floor
/// round the origin (3 studs wide every 6), seen straight down from 30 and 50 studs through fields
/// of view of 30, 45 and 70 degrees at ShadowSoftness 0. Spheres centred on the view axis meet the
/// floor in circles round the camera's foot.
pub fn shadow_cascade_down() -> Experiment {
	let (sun, towards, across, _) = sun_axes();
	let tan_elevation = sun.y / Vec3::new(sun.x, 0.0, sun.z).length();
	let mut model = ModelBuilder::new("ShadowCascadeDown");
	white_floor(&mut model, Vec3::ZERO, 600.0);
	let mut probes = Vec::new();
	let altitude = 200.0;
	let offset = towards * (altitude / tan_elevation);
	for index in -16..=16 {
		let lateral = index as f32 * 6.0;
		let center = offset + across * lateral + Vec3::Y * (altitude + 0.5);
		model.part(&PartSpec {
			name: format!("Bar{index}"),
			cframe: Affine3A::from_cols(across.into(), Vec3::Y.into(), (-towards).into(), center.into()),
			size: Vec3::new(3.0, 1.0, 220.0),
			color: [160; 3],
			material: 272,
			..Default::default()
		});
		for (side, sign) in [("l", -1.0f32), ("r", 1.0)] {
			let point = across * (lateral + sign * 1.5) + Vec3::Y * 0.001;
			edge(&mut probes, format!("b{index}{side}"), point, across * sign, towards, altitude / sun.y, serde_json::json!({"bar": index, "half": 1.2}));
		}
	}
	let mut runs = Vec::new();
	for height in [30.0f32, 50.0] {
		for fov in [30.0f32, 45.0, 70.0] {
			let camera = CameraSpec::LookAt { eye: [0.0, height, 0.0], target: [0.0, 0.0, 0.0], fov };
			runs.push(Run { label: format!("h{height}-fov{fov}"), environment: sun_shadows(0.0), camera });
		}
	}
	Experiment { name: "shadow-cascade-down".into(), model, probes, runs }
}

/// Shadow-map texels: bars 2 studs above the floor whose shadows run at 0, 22.5, 45 and 67.5
/// degrees from +X, each under its own camera straight down from 8 studs through a 20 degree field
/// of view (the cascade round the camera, ~400 px a stud: the staircase of an edge shows the texel
/// grid), at ShadowSoftness 0; the 22.5 degree bar also at 0.2 and 1, and from 42 studs through 8
/// degrees (the next cascade).
pub fn shadow_texels() -> Experiment {
	let (sun, ..) = sun_axes();
	let mut model = ModelBuilder::new("ShadowTexels");
	white_floor(&mut model, Vec3::new(15.0, 0.0, 0.0), 80.0);
	let mut probes = Vec::new();
	let height = 2.0;
	// a point at height h shadows the floor h · (-sun.x, -sun.z) / sun.y further on
	let shift = Vec3::new(-sun.x, 0.0, -sun.z) * (height / sun.y);
	let mut centres = Vec::new();
	for (index, angle) in [0.0f32, 22.5, 45.0, 67.5].iter().enumerate() {
		let centre = Vec3::new(index as f32 * 10.0, 0.0, 0.0);
		let direction = Vec3::new(angle.to_radians().cos(), 0.0, angle.to_radians().sin());
		let normal = Vec3::new(-direction.z, 0.0, direction.x);
		let label = format!("bar{angle}");
		model.part(&PartSpec {
			name: label.clone(),
			cframe: Affine3A::from_cols(direction.into(), Vec3::Y.into(), normal.into(), (centre - shift + Vec3::Y * (height + 0.1)).into()),
			size: Vec3::new(12.0, 0.2, 0.6),
			color: [160; 3],
			material: 272,
			..Default::default()
		});
		for (side, sign) in [("a", 1.0f32), ("b", -1.0)] {
			let point = centre + normal * (sign * 0.3) + Vec3::Y * 0.001;
			edge(&mut probes, format!("{label}{side}"), point, normal * sign, direction, height / sun.y, serde_json::json!({"angle": angle, "half": 0.25}));
		}
		centres.push(centre);
	}
	let down = |centre: Vec3, height: f32, fov: f32| CameraSpec::LookAt { eye: (centre + Vec3::Y * height).into(), target: centre.into(), fov };
	let mut runs = Vec::new();
	for (index, angle) in [0.0f32, 22.5, 45.0, 67.5].iter().enumerate() {
		runs.push(Run { label: format!("a{angle}-s0"), environment: sun_shadows(0.0), camera: down(centres[index], 8.0, 20.0) });
	}
	for softness in [0.2f32, 1.0] {
		runs.push(Run { label: format!("a22.5-s{softness}"), environment: sun_shadows(softness), camera: down(centres[1], 8.0, 20.0) });
	}
	runs.push(Run { label: "a22.5-far-s0".into(), environment: sun_shadows(0.0), camera: down(centres[1], 42.0, 8.0) });
	Experiment { name: "shadow-texels".into(), model, probes, runs }
}

/// A model's sun term from a camera (close-ups: hair over a face): the model under the sun alone
/// with shadows and without, and under the default lighting, from `eye` looking along `look`.
pub fn shadow_view(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> anyhow::Result<Experiment> {
	let document = crate::scene::load::read_file(path)?;
	let mut model = ModelBuilder::new("ShadowView");
	model.dom = document.dom;
	let roots = document.roots.clone();
	let container = model.dom.insert(model.dom.root_ref(), rbx_dom_weak::InstanceBuilder::new("Model").with_name("ShadowView"));
	for root in roots {
		model.dom.transfer_within(root, container);
	}
	model.model = container;
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look.normalize() * 10.0).into(), fov };
	let runs = vec![
		Run { label: "sun".into(), environment: sun_shadows(0.2), camera: camera.clone() },
		Run { label: "sun-noshadow".into(), environment: sun_only(2.0), camera: camera.clone() },
		Run { label: "default".into(), environment: Environment::default(), camera },
	];
	Ok(Experiment { name: "shadow-view".into(), model, probes: Vec::new(), runs })
}

/// Near occluders (hair over a face, accessories on a body): floating white boards turned 45, 60
/// and 75 degrees from facing the sun, each with a thin plate parallel to it over its upper half,
/// the plate's sun-side face 0.08, 0.15, 0.3, 0.6 or 1.2 studs in front of the board along the sun's
/// rays. The plate's lower edge shadows a band of about that width below it; one camera 14 studs
/// away per tilt (the cascade round the camera), 30 degrees below the boards' normal, looks up
/// under the edges. ShadowSoftness 0 (the narrowest filter, so the bands' darkness shows), and 0.2
/// for the 60 degree row.
pub fn shadow_gap() -> Experiment {
	let (sun, _, across, up) = sun_axes();
	let mut model = ModelBuilder::new("ShadowGap");
	let mut probes = Vec::new();
	let thickness = 0.02;
	let mut runs = Vec::new();
	for (row, tilt) in [45.0f32, 60.0, 75.0].iter().enumerate() {
		let centre = Vec3::new(0.0, 20.0, 0.0) + across * ((row as f32 - 1.0) * 24.0);
		let tilt_rad = tilt.to_radians();
		// the board's normal turned from the sun towards -up (in the map plane), its in-plane axis
		// towards the sun, and its right (across the sun) shared by every board
		let normal = sun * tilt_rad.cos() - up * tilt_rad.sin();
		let rise = sun * tilt_rad.sin() + up * tilt_rad.cos();
		for (column, depth_gap) in [0.08f32, 0.15, 0.3, 0.6, 1.2].iter().enumerate() {
			// the plate's front face depth_gap in front of the board along the rays
			let gap = depth_gap * tilt_rad.cos() - thickness;
			let board = centre + across * ((column as f32 - 2.0) * 2.8);
			let label = format!("t{tilt}d{depth_gap}");
			model.part(&PartSpec {
				name: format!("{label}-board"),
				cframe: Affine3A::from_cols((-across).into(), rise.into(), normal.into(), (board - normal * 0.1).into()),
				size: Vec3::new(2.4, 2.4, 0.2),
				color: [255; 3],
				material: 272,
				..Default::default()
			});
			if gap > 0.0 {
				model.part(&PartSpec {
					name: format!("{label}-plate"),
					cframe: Affine3A::from_cols((-across).into(), rise.into(), normal.into(), (board + rise * 0.6 + normal * (gap + thickness * 0.5)).into()),
					size: Vec3::new(2.4, 1.2, thickness),
					color: [160; 3],
					material: 272,
					..Default::default()
				});
			}
			// the plate's lower edge seen along the board's normal; lit side downwards (-rise)
			edge(
				&mut probes,
				label,
				board + normal * 0.001,
				-rise,
				across,
				*depth_gap,
				serde_json::json!({"tilt": tilt, "gap": gap, "depthGap": depth_gap, "band": gap * tilt_rad.tan(), "half": 1.0}),
			);
		}
		// 30 degrees further from the sun than the boards' normal (towards -up): below their edges
		let view = (tilt + 30.0).to_radians();
		let direction = (sun * view.cos() - up * view.sin()).normalize();
		let camera = CameraSpec::LookAt { eye: (centre + direction * 14.0).into(), target: centre.into(), fov: 50.0 };
		runs.push(Run { label: format!("t{tilt}-s0"), environment: sun_shadows(0.0), camera: camera.clone() });
		if *tilt == 60.0 {
			runs.push(Run { label: format!("t{tilt}-s0.2"), environment: sun_shadows(0.2), camera });
		}
	}
	Experiment { name: "shadow-gap".into(), model, probes, runs }
}
