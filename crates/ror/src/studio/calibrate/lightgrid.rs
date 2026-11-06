//! Roblox's light grid (OutdoorAmbient and the sky's light occluded by parts, in voxels) and its
//! small-scale ambient occlusion: white SmoothPlastic floors under roofs, bars in front of walls, a
//! crease seen from near and far (notes/render.md "Light grid", "Ambient occlusion";
//! tools/analysis/lightgrid_fit.py, ao_fit.py).

// case tables are tuples
#![allow(clippy::type_complexity)]

use glam::{Affine3A, Vec3};

use super::{Experiment, Probe, Run};
use crate::render::camera::CameraSpec;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec};

/// Every light off, global shadows on (as the first `lightgrid` experiment).
fn off() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::ZERO,
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: true,
		..Environment::default()
	}
}

fn outdoor() -> Environment {
	Environment { outdoor_ambient: Vec3::splat(0.7), ..off() }
}

fn ambient() -> Environment {
	Environment { ambient: Vec3::splat(0.7), ..off() }
}

fn sky() -> Environment {
	Environment { environment_diffuse_scale: 1.0, ..off() }
}

/// A white SmoothPlastic slab 1 stud thick with its top at `top`, centred on (x, z).
fn floor(model: &mut ModelBuilder, name: &str, x: f32, z: f32, half: f32, top: f32) {
	model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation(Vec3::new(x, top - 0.5, z)), size: Vec3::new(2.0 * half, 1.0, 2.0 * half), color: [255; 3], ..Default::default() });
}

/// A white SmoothPlastic box from `min` to `max`.
fn slab(model: &mut ModelBuilder, name: &str, min: Vec3, max: Vec3) -> rbx_dom_weak::types::Ref {
	model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation((min + max) * 0.5), size: max - min, color: [255; 3], ..Default::default() })
}

/// Straight down onto (x, z) from `height`.
fn down(x: f32, z: f32, height: f32, fov: f32) -> CameraSpec {
	CameraSpec::LookAt { eye: Vec3::new(x, height, z - 0.05).into(), target: Vec3::new(x, 0.0, z).into(), fov }
}

fn probe(label: &str, x: f32, z: f32, inputs: serde_json::Value) -> Probe {
	Probe { label: label.into(), position: [x, 0.0, z], inputs }
}

/// The grid's vertical behaviour: a 48-stud square roof 1 stud thick with its bottom at 2-16 studs
/// over a floor whose top is at 0 (and floors raised to 1, 2, 3 under a roof at 8; a roof filling
/// 8-12), each seen straight down from just under the roof at its centre, 24 studs from any open
/// side. OutdoorAmbient alone, and Ambient alone for two as the control (Ambient ignores the grid).
pub fn lg_height() -> Experiment {
	let mut model = ModelBuilder::new("LgHeight");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, floor top, roof bottom, roof thickness)
	let mut cases: Vec<(String, f32, f32, f32)> = [2.0f32, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0, 12.0, 16.0].iter().map(|h| (format!("h{h}"), 0.0, *h, 1.0)).collect();
	cases.extend([("f1-h8".to_string(), 1.0, 8.0, 1.0), ("f2-h8".into(), 2.0, 8.0, 1.0), ("f3-h8".into(), 3.0, 8.0, 1.0), ("h8-thick".into(), 0.0, 8.0, 4.0)]);
	for (index, (label, top, bottom, thickness)) in cases.iter().enumerate() {
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 0.0, 32.0, *top);
		slab(&mut model, &format!("Roof-{label}"), Vec3::new(x - 24.0, *bottom, -24.0), Vec3::new(x + 24.0, bottom + thickness, 24.0));
		probes.push(probe(label, x, 0.0, serde_json::json!({"floor": top, "roof": bottom, "thickness": thickness})));
		let camera = down(x, 0.0, (bottom - 0.4).min(top + 6.0), 70.0);
		runs.push(Run { label: format!("{label}-outdoor"), environment: outdoor(), camera: camera.clone() });
		if label == "h2" || label == "h8" {
			runs.push(Run { label: format!("{label}-ambient"), environment: ambient(), camera });
		}
	}
	Experiment { name: "lg-height".into(), model, probes, runs }
}

/// The grid's reach sideways: roofs filling 8-12 studs (4 thick) of 4, 8, 16 and 24 studs square,
/// 8 and 16 at 4-8, 16 and 32 at 16-20, their corners on multiples of 4; straight down from under
/// each roof's centre. OutdoorAmbient alone.
pub fn lg_size() -> Experiment {
	let mut model = ModelBuilder::new("LgSize");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	let cases: [(f32, f32); 8] = [(8.0, 4.0), (8.0, 8.0), (8.0, 16.0), (8.0, 24.0), (4.0, 8.0), (4.0, 16.0), (16.0, 16.0), (16.0, 32.0)];
	for (index, (bottom, side)) in cases.iter().enumerate() {
		let label = format!("b{bottom}-s{side}");
		let (gx, gz) = (index as f32 * 100.0, 0.0);
		floor(&mut model, &format!("Floor-{label}"), gx + side * 0.5, gz + side * 0.5, 32.0, 0.0);
		slab(&mut model, &format!("Roof-{label}"), Vec3::new(gx, *bottom, gz), Vec3::new(gx + side, bottom + 4.0, gz + side));
		let (cx, cz) = (gx + side * 0.5, gz + side * 0.5);
		probes.push(probe(&label, cx, cz, serde_json::json!({"roof": bottom, "side": side, "corner": [gx, gz]})));
		runs.push(Run { label: label.clone(), environment: outdoor(), camera: down(cx, cz, (bottom - 0.5).min(7.5), 90.0) });
	}
	Experiment { name: "lg-size".into(), model, probes, runs }
}

/// The grid's alignment and its interpolation across an edge: a roof filling 8-12 studs, 48 wide,
/// open on the -Z side with its edge at z = 0, 1, 2 and 3 (studs past a multiple of 4), seen from
/// under the roof looking out across the edge. OutdoorAmbient alone, and the sky's light alone for
/// the edge at 0.
pub fn lg_edge() -> Experiment {
	let mut model = ModelBuilder::new("LgEdge");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, edge) in [0.0f32, 1.0, 2.0, 3.0].iter().enumerate() {
		let label = format!("e{edge}");
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 8.0, 32.0, 0.0);
		slab(&mut model, &format!("Roof-{label}"), Vec3::new(x - 24.0, 8.0, *edge), Vec3::new(x + 24.0, 12.0, edge + 40.0));
		probes.push(probe(&label, x, *edge, serde_json::json!({"edge": edge})));
		let camera = CameraSpec::LookAt { eye: Vec3::new(x, 7.0, edge + 10.0).into(), target: Vec3::new(x, 0.0, edge - 6.0).into(), fov: 70.0 };
		runs.push(Run { label: format!("{label}-outdoor"), environment: outdoor(), camera: camera.clone() });
		if *edge == 0.0 {
			runs.push(Run { label: format!("{label}-sky"), environment: sky(), camera });
		}
	}
	Experiment { name: "lg-edge".into(), model, probes, runs }
}

/// Whether the sky specular follows the grid as OutdoorAmbient does (v = 1 - (1 - L)⁴) where it is
/// partly open (`ibl-ground`: a thin wall's sky specular falls to 0.89 down the wall where v stays
/// 1): that wall (80 x 40 x 0.5 studs, head-on from 51) and `lg-edge`'s e0 roof edge over its floor,
/// all white, under OutdoorAmbient alone and the sky specular alone (+2 EV).
pub fn lg_specular() -> Experiment {
	let mut model = ModelBuilder::new("LgSpecular");
	let mut runs = Vec::new();
	let specular = Environment { environment_specular_scale: 1.0, exposure_compensation: 2.0, ..off() };
	slab(&mut model, "Wall", Vec3::new(-40.0, -20.0, 6.25), Vec3::new(40.0, 20.0, 6.75));
	let wall = CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -45.0).into(), target: [0.0, 0.0, 0.0], fov: 40.0 };
	runs.push(Run { label: "wall-outdoor".into(), environment: outdoor(), camera: wall.clone() });
	runs.push(Run { label: "wall-specular".into(), environment: specular.clone(), camera: wall });
	let x = 300.0;
	floor(&mut model, "Floor", x, 8.0, 32.0, 0.0);
	slab(&mut model, "Roof", Vec3::new(x - 24.0, 8.0, 0.0), Vec3::new(x + 24.0, 12.0, 40.0));
	let edge = CameraSpec::LookAt { eye: Vec3::new(x, 7.0, 10.0).into(), target: Vec3::new(x, 0.0, -6.0).into(), fov: 70.0 };
	runs.push(Run { label: "edge-outdoor".into(), environment: outdoor(), camera: edge.clone() });
	runs.push(Run { label: "edge-specular".into(), environment: specular, camera: edge });
	Experiment { name: "lg-specular".into(), model, probes: Vec::new(), runs }
}

/// What fills a voxel: 32-stud square roofs with their bottom at 8 studs, 0.2-4 studs thick, and
/// 4 thick: half and 0.9 transparent, invisible, Glass, ForceField, Neon, CastShadow off,
/// CanCollide off; slats 1 stud wide every 2 studs; straight down from under each roof's centre.
/// OutdoorAmbient alone.
pub fn lg_occupancy() -> Experiment {
	let mut model = ModelBuilder::new("LgOccupancy");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, thickness, material, transparency, cast shadow, can collide)
	let cases: [(&str, f32, u32, f32, bool, bool); 14] = [
		("t0.2", 0.2, 272, 0.0, true, true),
		("t0.5", 0.5, 272, 0.0, true, true),
		("t1", 1.0, 272, 0.0, true, true),
		("t2", 2.0, 272, 0.0, true, true),
		("t4", 4.0, 272, 0.0, true, true),
		("tr0.5", 4.0, 272, 0.5, true, true),
		("tr0.9", 4.0, 272, 0.9, true, true),
		("tr1", 4.0, 272, 1.0, true, true),
		("glass", 4.0, 1568, 0.0, true, true),
		("forcefield", 4.0, 1584, 0.0, true, true),
		("neon", 4.0, 288, 0.0, true, true),
		("noshadow", 4.0, 272, 0.0, false, true),
		("nocollide", 4.0, 272, 0.0, true, false),
		("slats", 1.0, 272, 0.0, true, true),
	];
	for (index, (label, thickness, material, transparency, cast_shadow, can_collide)) in cases.iter().enumerate() {
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 0.0, 32.0, 0.0);
		if *label == "slats" {
			// bars along Z, 1 stud wide, every 2 studs across X
			for k in 0..16 {
				let x0 = x - 16.0 + k as f32 * 2.0;
				slab(&mut model, &format!("Slat-{k}"), Vec3::new(x0, 8.0, -16.0), Vec3::new(x0 + 1.0, 9.0, 16.0));
			}
		} else {
			let min = Vec3::new(x - 16.0, 8.0, -16.0);
			let roof = model.part(&PartSpec {
				name: format!("Roof-{label}"),
				cframe: Affine3A::from_translation(min + Vec3::new(16.0, thickness * 0.5, 16.0)),
				size: Vec3::new(32.0, *thickness, 32.0),
				color: [255; 3],
				material: *material,
				transparency: *transparency,
				cast_shadow: *cast_shadow,
				..Default::default()
			});
			if !can_collide {
				model.set(roof, "CanCollide", false);
			}
		}
		probes.push(probe(label, x, 0.0, serde_json::json!({"thickness": thickness, "material": material, "transparency": transparency})));
		runs.push(Run { label: (*label).into(), environment: outdoor(), camera: down(x, 0.0, 7.5, 70.0) });
	}
	Experiment { name: "lg-occupancy".into(), model, probes, runs }
}

/// Which terms the grid scales: under a 48-stud roof filling 8-12 studs and on open floor beside
/// it, OutdoorAmbient, Ambient, the sky's diffuse and specular light (EV +2), OutdoorAmbient with
/// GlobalShadows off, and a PointLight (Range 6) alone; straight down from 7.5 studs.
pub fn lg_terms() -> Experiment {
	let mut model = ModelBuilder::new("LgTerms");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, (label, roofed, lit)) in [("roof", true, false), ("open", false, false), ("roof-light", true, true), ("open-light", false, true)].iter().enumerate() {
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 0.0, 32.0, 0.0);
		if *roofed {
			slab(&mut model, &format!("Roof-{label}"), Vec3::new(x - 24.0, 8.0, -24.0), Vec3::new(x + 24.0, 12.0, 24.0));
		}
		if *lit {
			let carrier = model.part(&PartSpec { name: format!("Light-{label}"), cframe: Affine3A::from_translation(Vec3::new(x + 3.0, 4.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
			model.child(
				carrier,
				rbx_dom_weak::InstanceBuilder::new("PointLight")
					.with_name("PointLight")
					.with_property("Brightness", 1.0f32)
					.with_property("Range", 6.0f32)
					.with_property("Color", rbx_dom_weak::types::Color3::new(1.0, 1.0, 1.0))
					.with_property("Shadows", false)
					.with_property("Enabled", true),
			);
		}
		probes.push(probe(label, x, 0.0, serde_json::json!({"roofed": roofed, "lit": lit})));
		let camera = down(x, 0.0, 7.5, 70.0);
		if *lit {
			runs.push(Run { label: label.to_string(), environment: off(), camera });
			continue;
		}
		runs.push(Run { label: format!("{label}-outdoor"), environment: outdoor(), camera: camera.clone() });
		runs.push(Run { label: format!("{label}-ambient"), environment: ambient(), camera: camera.clone() });
		runs.push(Run { label: format!("{label}-sky"), environment: sky(), camera: camera.clone() });
		runs.push(Run { label: format!("{label}-specular"), environment: Environment { environment_specular_scale: 1.0, exposure_compensation: 2.0, ..off() }, camera: camera.clone() });
		runs.push(Run { label: format!("{label}-outdoor-noshadows"), environment: Environment { global_shadows: false, ..outdoor() }, camera });
	}
	Experiment { name: "lg-terms".into(), model, probes, runs }
}

/// What the sky's light falls to where the grid is closed: lg-terms' roofed and open floors under
/// skies all dark, all white, white in the Up or the Dn image only and the default sky, the sky's
/// diffuse light alone; the default sky at EnvironmentDiffuseScale 0.5 and at midnight; the
/// specular light alone (EV +2) under the white sky (in the open and roofed, roofed also at midnight
/// and at 8:00) and under the default, dark and Up-only skies roofed.
pub fn lg_sky() -> Experiment {
	let mut model = ModelBuilder::new("LgSky");
	let mut probes = Vec::new();
	for (index, (label, roofed)) in [("roof", true), ("open", false)].iter().enumerate() {
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 0.0, 32.0, 0.0);
		if *roofed {
			slab(&mut model, &format!("Roof-{label}"), Vec3::new(x - 24.0, 8.0, -24.0), Vec3::new(x + 24.0, 12.0, 24.0));
		}
		probes.push(probe(label, x, 0.0, serde_json::json!({"roofed": roofed})));
	}
	let faces = |white: &[usize]| {
		let skybox = std::array::from_fn(|face| if white.contains(&face) { super::SKY_WHITE.to_string() } else { super::SKY_DARK.to_string() });
		Some(crate::scene::environment::Sky { skybox, celestial_bodies_shown: false, ..Default::default() })
	};
	// faces in Sky order: Bk, Dn, Ft, Lf, Rt, Up
	let skies = [("plain", Some(super::default_sky())), ("dark", faces(&[])), ("white", faces(&[0, 1, 2, 3, 4, 5])), ("up", faces(&[5])), ("dn", faces(&[1]))];
	let mut runs = Vec::new();
	for (index, label) in ["roof", "open"].iter().enumerate() {
		let camera = down(index as f32 * 100.0, 0.0, 7.5, 70.0);
		for (sky_label, skybox) in &skies {
			runs.push(Run { label: format!("{label}-{sky_label}"), environment: Environment { sky: skybox.clone(), ..sky() }, camera: camera.clone() });
		}
		runs.push(Run {
			label: format!("{label}-white-specular"),
			environment: Environment { environment_specular_scale: 1.0, exposure_compensation: 2.0, sky: skies[2].1.clone(), ..off() },
			camera: camera.clone(),
		});
	}
	let roof = down(0.0, 0.0, 7.5, 70.0);
	runs.push(Run { label: "roof-plain-half".into(), environment: Environment { environment_diffuse_scale: 0.5, sky: skies[0].1.clone(), ..off() }, camera: roof.clone() });
	runs.push(Run { label: "roof-plain-midnight".into(), environment: Environment { clock_time: 0.0, sky: skies[0].1.clone(), ..sky() }, camera: roof.clone() });
	// the specular light under the roof by sky and by time of day
	let specular = |skybox: &Option<crate::scene::environment::Sky>, clock_time: f32| Environment { environment_specular_scale: 1.0, exposure_compensation: 2.0, clock_time, sky: skybox.clone(), ..off() };
	for (sky_label, skybox) in [("plain", &skies[0].1), ("dark", &skies[1].1), ("up", &skies[3].1)] {
		runs.push(Run { label: format!("roof-{sky_label}-specular"), environment: specular(skybox, 14.0), camera: roof.clone() });
	}
	runs.push(Run { label: "roof-white-specular-midnight".into(), environment: specular(&skies[2].1, 0.0), camera: roof.clone() });
	runs.push(Run { label: "roof-white-specular-morning".into(), environment: specular(&skies[2].1, 8.0), camera: roof });
	// repeats from a camera 0.01 lower (fresh captures): lg-terms' specular run without a Sky, and
	// roof-white-specular
	let again = down(0.0, 0.0, 7.49, 70.0);
	runs.push(Run { label: "roof-nosky-specular-again".into(), environment: specular(&None, 14.0), camera: again.clone() });
	runs.push(Run { label: "roof-white-specular-again".into(), environment: specular(&skies[2].1, 14.0), camera: again });
	Experiment { name: "lg-sky".into(), model, probes, runs }
}

/// Where a wall samples the grid: under a roof filling 8-12 studs over x 0..64 (open on the -X
/// side), white walls 0.2 thick and 2 wide facing -X, from the floor to the roof, with their faces at
/// x = 1-6, 8 and 40, seen from the open side. OutdoorAmbient alone, and Ambient alone as the AO
/// control.
pub fn lg_walls() -> Experiment {
	let mut model = ModelBuilder::new("LgWalls");
	floor(&mut model, "Floor", 16.0, 0.0, 48.0, 0.0);
	slab(&mut model, "Roof", Vec3::new(0.0, 8.0, -36.0), Vec3::new(64.0, 12.0, 36.0));
	let mut probes = Vec::new();
	for (index, x) in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 40.0].iter().enumerate() {
		let z = -28.0 + index as f32 * 8.0;
		slab(&mut model, &format!("Wall{x}"), Vec3::new(*x, 0.0, z - 1.0), Vec3::new(x + 0.2, 8.0, z + 1.0));
		probes.push(Probe { label: format!("wall{x}"), position: [*x, 4.0, z], inputs: serde_json::json!({"x": x, "z": z}) });
	}
	let camera = CameraSpec::LookAt { eye: Vec3::new(-26.0, 4.0, 0.0).into(), target: Vec3::new(10.0, 3.0, 0.0).into(), fov: 70.0 };
	let runs = vec![
		Run { label: "outdoor".into(), environment: outdoor(), camera: camera.clone() },
		Run { label: "ambient".into(), environment: ambient(), camera },
	];
	Experiment { name: "lg-walls".into(), model, probes, runs }
}

/// The grid's spread layer by layer: roofs open on the -Z side (edge at z = 0, 48 wide) filling
/// 12-16, 16-20 and 20-24 studs over floors at 0, and the 12-16 roof over a floor raised to 4,
/// seen from under each roof looking out across the edge; walls 0.2 thick facing the open side of
/// an 8-12 roof (edge at x = 0) at x = 10-16, away from its other sides. OutdoorAmbient alone.
pub fn lg_steps() -> Experiment {
	let mut model = ModelBuilder::new("LgSteps");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	for (index, (bottom, top)) in [(12.0f32, 0.0f32), (16.0, 0.0), (20.0, 0.0), (12.0, 4.0)].iter().enumerate() {
		let label = format!("b{bottom}-f{top}");
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 8.0, 32.0, *top);
		slab(&mut model, &format!("Roof-{label}"), Vec3::new(x - 24.0, *bottom, 0.0), Vec3::new(x + 24.0, bottom + 4.0, 40.0));
		probes.push(probe(&label, x, 0.0, serde_json::json!({"roof": bottom, "floor": top})));
		let camera = CameraSpec::LookAt { eye: Vec3::new(x, top + 7.0, 10.0).into(), target: Vec3::new(x, *top, -6.0).into(), fov: 70.0 };
		runs.push(Run { label, environment: outdoor(), camera });
	}
	// walls further under an 8-12 roof than lg-walls' (x 10-16), its open edge at x = 0
	let gx = 500.0;
	floor(&mut model, "Floor-walls", gx + 16.0, 0.0, 48.0, 0.0);
	slab(&mut model, "Roof-walls", Vec3::new(gx, 8.0, -36.0), Vec3::new(gx + 64.0, 12.0, 36.0));
	for (index, x) in [10.0f32, 12.0, 14.0, 16.0].iter().enumerate() {
		let z = -9.0 + index as f32 * 6.0;
		slab(&mut model, &format!("Wall{x}"), Vec3::new(gx + x, 0.0, z - 1.0), Vec3::new(gx + x + 0.2, 8.0, z + 1.0));
		probes.push(Probe { label: format!("wall{x}"), position: [gx + x, 4.0, z], inputs: serde_json::json!({"x": x, "z": z}) });
	}
	let camera = CameraSpec::LookAt { eye: Vec3::new(gx - 26.0, 4.0, 0.0).into(), target: Vec3::new(gx + 10.0, 3.0, 0.0).into(), fov: 50.0 };
	runs.push(Run { label: "walls".into(), environment: outdoor(), camera: camera.clone() });
	runs.push(Run { label: "walls-ambient".into(), environment: ambient(), camera });
	Experiment { name: "lg-steps".into(), model, probes, runs }
}

/// What fills a voxel, by shape: occluders from 8 studs up over floors, a 24-stud block turned 45
/// degrees about Y, a 20-stud ball, a cylinder 12 across along X, a 24 x 12 x 24 wedge, a head
/// MeshPart 24 x 4 x 24, and blocks 24 x 4 x 24 drawn by a SpecialMesh (a sphere; the head mesh);
/// straight down from under each centre. OutdoorAmbient alone.
pub fn lg_shapes() -> Experiment {
	let mut model = ModelBuilder::new("LgShapes");
	let mut probes = Vec::new();
	let mut runs = Vec::new();
	// (label, class, shape, centre height, size, turn about Y)
	let cases: [(&str, &str, Option<u32>, f32, Vec3, f32); 7] = [
		("block45", "Part", Some(1), 8.5, Vec3::new(24.0, 1.0, 24.0), 45.0),
		("ball", "Part", Some(0), 18.0, Vec3::splat(20.0), 0.0),
		("cylinder", "Part", Some(2), 14.0, Vec3::new(24.0, 12.0, 12.0), 0.0),
		("wedge", "Part", Some(3), 14.0, Vec3::new(24.0, 12.0, 24.0), 0.0),
		("meshhead", "MeshPart", None, 10.0, Vec3::new(24.0, 4.0, 24.0), 0.0),
		("special-sphere", "Part", Some(1), 10.0, Vec3::new(24.0, 4.0, 24.0), 0.0),
		("special-head", "Part", Some(1), 10.0, Vec3::new(24.0, 4.0, 24.0), 0.0),
	];
	for (index, (label, class, shape, height, size, turn)) in cases.iter().enumerate() {
		let x = index as f32 * 100.0;
		floor(&mut model, &format!("Floor-{label}"), x, 0.0, 32.0, 0.0);
		let cframe = Affine3A::from_rotation_translation(glam::Quat::from_rotation_y(turn.to_radians()), Vec3::new(x, *height, 0.0));
		let part = model.part(&PartSpec { class, name: format!("Occluder-{label}"), cframe, size: *size, color: [255; 3], shape: *shape, ..Default::default() });
		if *class == "MeshPart" {
			model.set(part, "MeshId", rbx_dom_weak::types::ContentId::from("rbxasset://avatar/heads/head.mesh"));
		}
		if label.starts_with("special") {
			// MeshType Sphere fills the block; the head mesh (2 x 1 x 1 at scale 1) scaled to 24 x 4 x 24
			let mesh = rbx_dom_weak::InstanceBuilder::new("SpecialMesh").with_name("Mesh");
			let mesh = if *label == "special-sphere" {
				mesh.with_property("MeshType", rbx_dom_weak::types::Enum::from_u32(3)).with_property("Scale", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0))
			} else {
				mesh.with_property("MeshType", rbx_dom_weak::types::Enum::from_u32(5))
					.with_property("MeshId", rbx_dom_weak::types::ContentId::from("rbxasset://avatar/heads/head.mesh"))
					.with_property("Scale", rbx_dom_weak::types::Vector3::new(12.0, 4.0, 24.0))
			};
			model.child(part, mesh);
		}
		probes.push(probe(label, x, 0.0, serde_json::json!({"height": height, "size": size.to_array(), "turn": turn})));
		runs.push(Run { label: (*label).into(), environment: outdoor(), camera: down(x, 0.0, 7.5, 90.0) });
	}
	Experiment { name: "lg-shapes".into(), model, probes, runs }
}

/// A white wall with black bars in front of it, at `gaps` studs (bars 2 x 14 x 1, 8 studs apart
/// along X centred on x0), the wall's front face at z = 0.
fn bars(model: &mut ModelBuilder, x0: f32, gaps: &[f32], suffix: &str) {
	let width = gaps.len() as f32 * 8.0 + 16.0;
	model.part(&PartSpec { name: format!("Wall{suffix}"), cframe: Affine3A::from_translation(Vec3::new(x0, 0.0, 0.5)), size: Vec3::new(width, 40.0, 1.0), color: [255; 3], cast_shadow: false, ..Default::default() });
	let first = x0 - (gaps.len() as f32 - 1.0) * 4.0;
	for (index, gap) in gaps.iter().enumerate() {
		let x = first + index as f32 * 8.0;
		model.part(&PartSpec { name: format!("Bar{suffix}-{gap}"), cframe: Affine3A::from_translation(Vec3::new(x, 0.0, -gap - 0.5)), size: Vec3::new(2.0, 14.0, 1.0), color: [0; 3], cast_shadow: false, ..Default::default() });
	}
}

/// Screen-space or world-space: one bar 1 stud in front of the wall seen head-on from 12.5, 25, 50
/// and 100 studs (field of view 40). Ambient alone, no shadows.
pub fn ao_distance() -> Experiment {
	let mut model = ModelBuilder::new("AoDistance");
	bars(&mut model, 0.0, &[1.0], "");
	let environment = Environment { global_shadows: false, ..ambient() };
	let runs = [12.5f32, 25.0, 50.0, 100.0]
		.iter()
		.map(|distance| Run {
			label: format!("d{distance}"),
			environment: environment.clone(),
			camera: CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -distance).into(), target: Vec3::ZERO.into(), fov: 40.0 },
		})
		.collect();
	Experiment { name: "ao-distance".into(), model, probes: vec![probe("bar", 0.0, 0.0, serde_json::json!({"gap": 1.0}))], runs }
}

/// Fading with distance: ao-distance's bar from 100, 200, 400 and 800 studs through fields of view of
/// 40, 20, 10 and 5 (the same picture each time). Ambient alone, no shadows.
pub fn ao_far() -> Experiment {
	let mut experiment = ao_distance();
	experiment.name = "ao-far".into();
	let environment = Environment { global_shadows: false, ..ambient() };
	experiment.runs = [(100.0f32, 40.0f32), (200.0, 20.0), (400.0, 10.0), (800.0, 5.0)]
		.iter()
		.map(|(distance, fov)| Run {
			label: format!("d{distance}"),
			environment: environment.clone(),
			camera: CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -distance).into(), target: Vec3::ZERO.into(), fov: *fov },
		})
		.collect();
	experiment
}

/// Where the occlusion fades between 100 studs (full) and 200 (none, ao-far): ao-distance's bar from
/// 110-190 studs, the field of view narrowed to keep ao-distance's picture from 100; and from 150 and
/// 125 studs through a field of view of 40 (a smaller picture).
pub fn ao_fade() -> Experiment {
	let mut experiment = ao_distance();
	experiment.name = "ao-fade".into();
	let environment = Environment { global_shadows: false, ..ambient() };
	let mut cases: Vec<(String, f32, f32)> = [110.0f32, 120.0, 130.0, 140.0, 150.0, 160.0, 175.0, 190.0]
		.iter()
		.map(|distance| (format!("d{distance}"), *distance, 2.0 * (20f32.to_radians().tan() * 100.0 / distance).atan().to_degrees()))
		.collect();
	cases.push(("d150-fov40".into(), 150.0, 40.0));
	cases.push(("d125-fov40".into(), 125.0, 40.0));
	experiment.runs = cases
		.into_iter()
		.map(|(label, distance, fov)| Run { label, environment: environment.clone(), camera: CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -distance).into(), target: Vec3::ZERO.into(), fov } })
		.collect();
	experiment
}

/// Strength and reach against the gap: bars touching the wall and 0.25-8 studs in front of it, seen
/// head-on from 60 studs, under Ambient, OutdoorAmbient, the sky's diffuse light and a sun from
/// behind the camera 10 degrees up (no shadows) one at a time, and a second set lit by a PointLight
/// alone.
pub fn ao_gap() -> Experiment {
	let mut model = ModelBuilder::new("AoGap");
	let gaps = [0.0f32, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
	bars(&mut model, 0.0, &gaps, "");
	bars(&mut model, 300.0, &gaps, "-lit");
	let carrier = model.part(&PartSpec { name: "Light".into(), cframe: Affine3A::from_translation(Vec3::new(300.0, 0.0, -12.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
	model.child(
		carrier,
		rbx_dom_weak::InstanceBuilder::new("PointLight")
			.with_name("PointLight")
			.with_property("Brightness", 0.5f32)
			.with_property("Range", 60.0f32)
			.with_property("Color", rbx_dom_weak::types::Color3::new(1.0, 1.0, 1.0))
			.with_property("Shadows", false)
			.with_property("Enabled", true),
	);
	let camera = |x: f32| CameraSpec::LookAt { eye: Vec3::new(x, 0.0, -60.0).into(), target: Vec3::new(x, 0.0, 0.0).into(), fov: 40.0 };
	let no_shadows = |environment: Environment| Environment { global_shadows: false, ..environment };
	// GeographicLatitude -56.5 at noon: the sun 10 degrees up in the -Z direction, behind the camera
	let sun = Environment { brightness: 1.0, clock_time: 12.0, geographic_latitude: -56.5, ..off() };
	let runs = vec![
		Run { label: "ambient".into(), environment: no_shadows(ambient()), camera: camera(0.0) },
		Run { label: "outdoor".into(), environment: no_shadows(outdoor()), camera: camera(0.0) },
		Run { label: "sky".into(), environment: no_shadows(sky()), camera: camera(0.0) },
		Run { label: "sun".into(), environment: no_shadows(sun), camera: camera(0.0) },
		Run { label: "light".into(), environment: no_shadows(off()), camera: camera(300.0) },
	];
	let probes = gaps.iter().enumerate().map(|(index, gap)| probe(&format!("gap{gap}"), -24.0 + index as f32 * 8.0, 0.0, serde_json::json!({"gap": gap}))).collect();
	Experiment { name: "ao-gap".into(), model, probes, runs }
}

/// Scale: ao-gap's wall and bars shrunk ten times (bars 0.2 x 1.4 x 0.1, 0-0.8 studs out) seen from 6
/// studs (the same picture as ao-gap from 60), Ambient alone; and ao-gap's own scene under the sky's
/// specular light alone at EV +3 (whether the occlusion darkens it).
pub fn ao_scale() -> Experiment {
	let mut model = ModelBuilder::new("AoScale");
	let gaps = [0.0f32, 0.025, 0.05, 0.1, 0.2, 0.4, 0.8];
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 0.05)), size: Vec3::new(7.2, 4.0, 0.1), color: [255; 3], cast_shadow: false, ..Default::default() });
	for (index, gap) in gaps.iter().enumerate() {
		let x = -2.4 + index as f32 * 0.8;
		model.part(&PartSpec { name: format!("Bar-{gap}"), cframe: Affine3A::from_translation(Vec3::new(x, 0.0, -gap - 0.05)), size: Vec3::new(0.2, 1.4, 0.1), color: [0; 3], cast_shadow: false, ..Default::default() });
	}
	bars(&mut model, 300.0, &[0.0, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0], "-specular");
	let runs = vec![
		Run {
			label: "small".into(),
			environment: Environment { global_shadows: false, ..ambient() },
			camera: CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -6.0).into(), target: Vec3::ZERO.into(), fov: 40.0 },
		},
		Run {
			label: "specular".into(),
			environment: Environment { environment_specular_scale: 1.0, exposure_compensation: 3.0, global_shadows: false, ..off() },
			camera: CameraSpec::LookAt { eye: Vec3::new(300.0, 0.0, -60.0).into(), target: Vec3::new(300.0, 0.0, 0.0).into(), fov: 40.0 },
		},
	];
	Experiment { name: "ao-scale".into(), model, probes: Vec::new(), runs }
}

/// A model under Ambient alone from one place through fields of view of `fov`, half and a quarter
/// of it: a screen-space occlusion's reach in studs halves each time.
pub fn ao_fov(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> anyhow::Result<Experiment> {
	let mut experiment = super::decompose(path)?;
	experiment.name = "ao-fov".into();
	let gray = Vec3::splat(70.0 / 255.0);
	experiment.runs = [1.0f32, 0.5, 0.25]
		.iter()
		.map(|scale| Run {
			label: format!("fov{}", (fov * scale).round()),
			environment: Environment { ambient: gray, global_shadows: false, ..off() },
			camera: CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov: fov * scale },
		})
		.collect();
	Ok(experiment)
}

/// A model from a fixed camera under Ambient alone and the sky's diffuse light alone (with the
/// default sky): the occlusion where a close-up shows it.
pub fn ao_view(path: &std::path::Path, eye: Vec3, look: Vec3, fov: f32) -> anyhow::Result<Experiment> {
	let mut experiment = super::decompose(path)?;
	experiment.name = "ao-view".into();
	let camera = CameraSpec::LookAt { eye: eye.into(), target: (eye + look * 10.0).into(), fov };
	experiment.runs = vec![
		Run { label: "ambient".into(), environment: Environment { ambient: Vec3::splat(70.0 / 255.0), ..off() }, camera: camera.clone() },
		Run { label: "sky".into(), environment: Environment { environment_diffuse_scale: 1.0, ..off() }, camera },
	];
	Ok(experiment)
}

/// Concave corners: a floor with two walls meeting it and each other, a cube standing on the floor
/// and a ledge along a wall, seen from above the open corner. Ambient alone and OutdoorAmbient alone.
pub fn ao_corner() -> Experiment {
	let mut model = ModelBuilder::new("AoCorner");
	floor(&mut model, "Floor", 0.0, 0.0, 16.0, 0.0);
	slab(&mut model, "BackWall", Vec3::new(-16.0, 0.0, 16.0), Vec3::new(16.0, 12.0, 17.0));
	slab(&mut model, "LeftWall", Vec3::new(-17.0, 0.0, -16.0), Vec3::new(-16.0, 12.0, 17.0));
	slab(&mut model, "Cube", Vec3::new(2.0, 0.0, -2.0), Vec3::new(6.0, 4.0, 2.0));
	slab(&mut model, "Ledge", Vec3::new(-16.0, 4.0, 13.0), Vec3::new(8.0, 5.0, 16.0));
	let camera = CameraSpec::LookAt { eye: Vec3::new(14.0, 18.0, -20.0).into(), target: Vec3::new(-6.0, 2.0, 6.0).into(), fov: 50.0 };
	let runs = vec![
		Run { label: "ambient".into(), environment: Environment { global_shadows: false, ..ambient() }, camera: camera.clone() },
		Run { label: "outdoor".into(), environment: outdoor(), camera },
	];
	Experiment { name: "ao-corner".into(), model, probes: Vec::new(), runs }
}

/// A model under Ambient alone and OutdoorAmbient alone (the default 70 gray), framed like
/// `decompose`: what of its darkening is the grid (OutdoorAmbient only) and what occlusion both share.
pub fn ao_model(path: &std::path::Path) -> anyhow::Result<Experiment> {
	let mut experiment = super::decompose(path)?;
	let gray = Vec3::splat(70.0 / 255.0);
	let camera = CameraSpec::default();
	experiment.name = "ao-model".into();
	experiment.runs = vec![
		Run { label: "ambient".into(), environment: Environment { ambient: gray, ..off() }, camera: camera.clone() },
		Run { label: "outdoor".into(), environment: Environment { outdoor_ambient: gray, ..off() }, camera },
	];
	Ok(experiment)
}

/// A box from `min` to `max`, `scale` times its size around `origin`, white (black when `dark`).
fn scaled_box(model: &mut ModelBuilder, name: &str, origin: Vec3, scale: f32, min: Vec3, max: Vec3, dark: bool) {
	model.part(&PartSpec {
		name: name.into(),
		cframe: Affine3A::from_translation(origin + (min + max) * 0.5 * scale),
		size: (max - min) * scale,
		color: if dark { [0; 3] } else { [255; 3] },
		cast_shadow: false,
		..Default::default()
	});
}

/// The crease of ao-depth, ao-dolly and ao-size, `scale` times its size around `origin`: a white floor
/// (its top at y = 0) meeting a white wall (its face at z = 1, facing -Z), a white unit cube on the
/// floor 1 stud in front of the wall, a black bar 0.5 x 3 x 0.5 standing 0.5 off the wall and the
/// floor, and a white slab 0.25 high on the floor (a low step).
fn crease(model: &mut ModelBuilder, origin: Vec3, scale: f32, suffix: &str) {
	scaled_box(model, &format!("Floor{suffix}"), origin, scale, Vec3::new(-8.0, -1.0, -8.0), Vec3::new(8.0, 0.0, 2.0), false);
	scaled_box(model, &format!("Wall{suffix}"), origin, scale, Vec3::new(-8.0, 0.0, 1.0), Vec3::new(8.0, 8.0, 2.0), false);
	scaled_box(model, &format!("Cube{suffix}"), origin, scale, Vec3::new(-1.75, 0.0, -1.0), Vec3::new(-0.75, 1.0, 0.0), false);
	scaled_box(model, &format!("Bar{suffix}"), origin, scale, Vec3::new(0.25, 0.5, 0.0), Vec3::new(0.75, 3.5, 0.5), true);
	scaled_box(model, &format!("Step{suffix}"), origin, scale, Vec3::new(1.25, 0.0, -2.0), Vec3::new(2.75, 0.25, -0.5), false);
}

/// The crease from `distance` studs (times `scale`), 26 degrees down and 15 to the side, through `fov`.
fn crease_view(origin: Vec3, scale: f32, distance: f32, fov: f32) -> CameraSpec {
	let target = origin + Vec3::new(0.0, 0.9, -0.5) * scale;
	let eye = target + Vec3::new(0.3, 0.5, -1.0).normalize() * distance * scale;
	CameraSpec::LookAt { eye: eye.into(), target: target.into(), fov }
}

/// Screen space at close range: the crease seen from 3, 8, 20, 50 and 100 studs, the field of view
/// narrowed with the distance to keep its picture (40 degrees at 8 studs). Ambient alone, no shadows.
pub fn ao_depth() -> Experiment {
	let mut model = ModelBuilder::new("AoDepth");
	crease(&mut model, Vec3::ZERO, 1.0, "");
	let environment = Environment { global_shadows: false, ..ambient() };
	let half = 8.0 * 20f32.to_radians().tan();
	let runs = [3.0f32, 8.0, 20.0, 50.0, 100.0]
		.iter()
		.map(|distance| Run { label: format!("d{distance}"), environment: environment.clone(), camera: crease_view(Vec3::ZERO, 1.0, *distance, 2.0 * (half / distance).atan().to_degrees()) })
		.collect();
	Experiment { name: "ao-depth".into(), model, probes: Vec::new(), runs }
}

/// ao-depth's crease from the same distances through a field of view of 40 (a picture shrinking with
/// the distance; the model is ao-depth's, so its 8-stud view is the same capture).
pub fn ao_dolly() -> Experiment {
	let mut experiment = ao_depth();
	experiment.name = "ao-dolly".into();
	let environment = Environment { global_shadows: false, ..ambient() };
	experiment.runs = [3.0f32, 8.0, 20.0, 50.0, 100.0]
		.iter()
		.map(|distance| Run { label: format!("d{distance}"), environment: environment.clone(), camera: crease_view(Vec3::ZERO, 1.0, *distance, 40.0) })
		.collect();
	experiment
}

/// Scale: ao-depth's crease at 1/16, 1/4, 1 and 4 times its size, each seen from 12 times its scale
/// through a field of view of 40 (the same picture). Ambient alone, no shadows.
pub fn ao_size() -> Experiment {
	let mut model = ModelBuilder::new("AoSize");
	let scales = [0.0625f32, 0.25, 1.0, 4.0];
	let origin = |index: usize| Vec3::new(index as f32 * 200.0, 0.0, 0.0);
	for (index, scale) in scales.iter().enumerate() {
		crease(&mut model, origin(index), *scale, &format!("-x{scale}"));
	}
	let environment = Environment { global_shadows: false, ..ambient() };
	let runs = scales.iter().enumerate().map(|(index, scale)| Run { label: format!("x{scale}"), environment: environment.clone(), camera: crease_view(origin(index), *scale, 12.0, 40.0) }).collect();
	Experiment { name: "ao-size".into(), model, probes: Vec::new(), runs }
}

/// Thin layers (hair cards): stacks of four white plates 2 x 2 x 0.02, each 0.35 lower and 0.35 to the
/// side of the one behind it, 0.05, 0.1 and 0.2 studs apart, in the open; seen from the front and a
/// little above from 10 studs (all three) and from 3 studs (the 0.1 stack). Ambient alone.
pub fn ao_layers() -> Experiment {
	let mut model = ModelBuilder::new("AoLayers");
	let gaps = [0.05f32, 0.1, 0.2];
	let stack = |index: usize| Vec3::new(-4.5 + index as f32 * 3.5, 0.0, 0.0);
	for (index, gap) in gaps.iter().enumerate() {
		for layer in 0..4 {
			let step = layer as f32 * 0.35;
			model.part(&PartSpec {
				name: format!("Plate{gap}-{layer}"),
				cframe: Affine3A::from_translation(stack(index) + Vec3::new(step, -step, -(layer as f32) * (gap + 0.02))),
				size: Vec3::new(2.0, 2.0, 0.02),
				color: [255; 3],
				cast_shadow: false,
				..Default::default()
			});
		}
	}
	let environment = Environment { global_shadows: false, ..ambient() };
	let view = |target: Vec3, distance: f32| CameraSpec::LookAt { eye: (target + Vec3::new(-0.15, 0.25, -1.0).normalize() * distance).into(), target: target.into(), fov: 40.0 };
	let runs = vec![
		Run { label: "all".into(), environment: environment.clone(), camera: view(stack(1) + Vec3::new(0.5, -0.5, 0.0), 10.0) },
		Run { label: "near".into(), environment, camera: view(stack(1) + Vec3::new(0.5, -0.5, -0.1), 3.0) },
	];
	Experiment { name: "ao-layers".into(), model, probes: Vec::new(), runs }
}

/// The occlusion against a crease's angle: white ramps 3 wide rising from a white floor at 15, 30, 45,
/// 60 and 90 degrees (creases of 165 to 90 degrees), and in front of them steps 0.1, 0.25, 0.5 and 1
/// high; seen from the front and above. Ambient alone, no shadows.
pub fn ao_ramps() -> Experiment {
	let mut model = ModelBuilder::new("AoRamps");
	slab(&mut model, "Floor", Vec3::new(-16.0, -1.0, -14.0), Vec3::new(16.0, 0.0, 10.0));
	for (index, angle) in [15.0f32, 30.0, 45.0, 60.0, 90.0].iter().enumerate() {
		// a block 3 x 0.5 x 8 turned about X so its top face rises from the floor line z = 0 at the
		// angle, a stud of its slope below the floor
		let turn = glam::Quat::from_rotation_x(-angle.to_radians());
		let (up, along) = (turn * Vec3::Y, turn * Vec3::Z);
		let centre = Vec3::new(-10.0 + index as f32 * 5.0, 0.0, 0.0) - up * 0.25 + along * 3.0;
		model.part(&PartSpec { name: format!("Ramp{angle}"), cframe: Affine3A::from_rotation_translation(turn, centre), size: Vec3::new(3.0, 0.5, 8.0), color: [255; 3], cast_shadow: false, ..Default::default() });
	}
	for (index, height) in [0.1f32, 0.25, 0.5, 1.0].iter().enumerate() {
		let x = -7.5 + index as f32 * 5.0;
		slab(&mut model, &format!("Step{height}"), Vec3::new(x - 1.5, 0.0, -8.0), Vec3::new(x + 1.5, *height, -5.0));
	}
	let camera = CameraSpec::LookAt { eye: Vec3::new(0.0, 10.0, -20.0).into(), target: Vec3::new(0.0, 1.0, -2.0).into(), fov: 50.0 };
	let runs = vec![Run { label: "ambient".into(), environment: Environment { global_shadows: false, ..ambient() }, camera }];
	Experiment { name: "ao-ramps".into(), model, probes: Vec::new(), runs }
}

/// The relief of a model's panels in isolation (a ship close-up: Studio's occlusion thin
/// lines along grooves and frame edges where ror's is bands as wide as its disk): a white wall facing
/// -Z (its face at z = 0) with vertical features 20 studs tall, 7 studs apart: grooves 1 stud wide and
/// 0.25, 0.5, 1 and 2 deep, raised strips 1 wide and 0.25, 0.5, 1 and 2 high, and strips 1 wide and 0.5
/// and 1 high with 45-degree bevels; seen head-on from 15 studs (each feature), 25 (the grooves, the
/// strips), 40 and 60 (two halves), 80 and 110 (all), field of view 40. Ambient alone, no shadows.
pub fn ao_relief() -> Experiment {
	let mut model = ModelBuilder::new("AoRelief");
	// (label, kind: 0 groove, 1 strip, 2 bevelled strip, size)
	let features: [(&str, u8, f32); 10] = [
		("groove0.25", 0, 0.25),
		("groove0.5", 0, 0.5),
		("groove1", 0, 1.0),
		("groove2", 0, 2.0),
		("strip0.25", 1, 0.25),
		("strip0.5", 1, 0.5),
		("strip1", 1, 1.0),
		("strip2", 1, 2.0),
		("bevel0.5", 2, 0.5),
		("bevel1", 2, 1.0),
	];
	let spacing = 7.0;
	let x_of = |index: usize| (index as f32 - 4.5) * spacing;
	let white = |model: &mut ModelBuilder, name: String, min: Vec3, max: Vec3| {
		model.part(&PartSpec { name, cframe: Affine3A::from_translation((min + max) * 0.5), size: max - min, color: [255; 3], cast_shadow: false, ..Default::default() });
	};
	// the wall's back plate: grooves are cut into a 2-stud layer in front of it
	let half = spacing * 5.5;
	white(&mut model, "Back".into(), Vec3::new(-half, -15.0, 2.0), Vec3::new(half, 15.0, 3.0));
	let mut probes = Vec::new();
	let mut cursor = -half;
	for (index, (label, kind, size)) in features.iter().enumerate() {
		let x = x_of(index);
		// the 2-stud front layer, with the groove's gap 1 stud wide (its floor at z = depth)
		let (gap_lo, gap_hi) = if *kind == 0 { (x - 0.5, x + 0.5) } else { (x, x) };
		if *kind == 0 {
			white(&mut model, format!("Front{index}a"), Vec3::new(cursor, -15.0, 0.0), Vec3::new(gap_lo, 15.0, 2.0));
			// the groove's floor: the front layer is 2 deep, the groove `size` deep (2: the back plate)
			if *size < 2.0 {
				white(&mut model, format!("Floor{index}"), Vec3::new(gap_lo, -15.0, *size), Vec3::new(gap_hi, 15.0, 2.0));
			}
			cursor = gap_hi;
		} else {
			white(&mut model, format!("Front{index}a"), Vec3::new(cursor, -15.0, 0.0), Vec3::new(x + spacing * 0.5, 15.0, 2.0));
			cursor = x + spacing * 0.5;
			// a strip 1 wide and `size` high standing out of the wall's face (z = 0), 20 tall
			white(&mut model, format!("Strip{index}"), Vec3::new(x - 0.5, -10.0, -size), Vec3::new(x + 0.5, 10.0, 0.0));
			if *kind == 2 {
				// 45-degree bevels on both sides: wedges rising from the wall to the strip's top
				for (side, columns) in [(-1.0f32, [Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0)]), (1.0, [Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, 0.0)])] {
					let rotation = glam::Mat3::from_cols(columns[0], columns[1], columns[2]);
					let centre = Vec3::new(x + side * (0.5 + size * 0.5), 0.0, -size * 0.5);
					model.part(&PartSpec { class: "WedgePart", name: format!("Bevel{index}{side}"), cframe: Affine3A::from_mat3_translation(rotation, centre), size: Vec3::new(20.0, *size, *size), color: [255; 3], cast_shadow: false, ..Default::default() });
				}
			}
		}
		probes.push(probe(label, x, 0.0, serde_json::json!({"kind": kind, "size": size, "x": x})));
	}
	white(&mut model, "FrontEnd".into(), Vec3::new(cursor, -15.0, 0.0), Vec3::new(half, 15.0, 2.0));
	let environment = Environment { global_shadows: false, ..ambient() };
	let camera = |x: f32, distance: f32| CameraSpec::LookAt { eye: Vec3::new(x, 0.0, -distance).into(), target: Vec3::new(x, 0.0, 0.0).into(), fov: 40.0 };
	let mut runs: Vec<Run> = features.iter().enumerate().map(|(index, (label, _, _))| Run { label: format!("d15-{label}"), environment: environment.clone(), camera: camera(x_of(index), 15.0) }).collect();
	runs.push(Run { label: "d40-left".into(), environment: environment.clone(), camera: camera(x_of(2) - spacing * 0.5, 40.0) });
	runs.push(Run { label: "d40-right".into(), environment: environment.clone(), camera: camera(x_of(7) - spacing * 0.5, 40.0) });
	runs.push(Run { label: "d80".into(), environment: environment.clone(), camera: camera(0.0, 80.0) });
	runs.push(Run { label: "d25-grooves".into(), environment: environment.clone(), camera: camera((x_of(1) + x_of(2)) * 0.5, 25.0) });
	runs.push(Run { label: "d25-strips".into(), environment: environment.clone(), camera: camera((x_of(6) + x_of(7)) * 0.5, 25.0) });
	runs.push(Run { label: "d60-left".into(), environment: environment.clone(), camera: camera(x_of(2) - spacing * 0.5, 60.0) });
	runs.push(Run { label: "d60-right".into(), environment: environment.clone(), camera: camera(x_of(7) - spacing * 0.5, 60.0) });
	runs.push(Run { label: "d110".into(), environment, camera: camera(0.0, 110.0) });
	Experiment { name: "ao-relief".into(), model, probes, runs }
}

/// The occlusion's resolution: thin features turned 2 degrees in the view so that along their length
/// they cross every phase of a half-resolution grid. A white wall facing -Z (its face at z = 0) with
/// free bars 0.05, 0.1, 0.2 and 0.4 studs wide, 0.25 deep, 1 stud in front of it, and a front plate 1
/// stud thick with through slots 0.01, 0.02, 0.05, 0.1, 0.2 and 0.4 wide (seams to grooves: at 80
/// studs 0.2 to 8 px); seen head-on from 20 studs (the bars; the slots in two halves), 40 (the bars,
/// the slots) and 80 (all), field of view 40. Ambient alone, no shadows.
pub fn ao_thin() -> Experiment {
	let mut model = ModelBuilder::new("AoThin");
	let turn = glam::Mat3::from_rotation_z(2f32.to_radians());
	let white = |model: &mut ModelBuilder, name: String, min: Vec3, max: Vec3| {
		model.part(&PartSpec { name, cframe: Affine3A::from_mat3_translation(turn, turn * ((min + max) * 0.5)), size: max - min, color: [255; 3], cast_shadow: false, ..Default::default() });
	};
	white(&mut model, "Wall".into(), Vec3::new(-34.0, -14.0, 0.0), Vec3::new(34.0, 14.0, 1.0));
	let mut probes = Vec::new();
	let bars = [0.05f32, 0.1, 0.2, 0.4];
	let bar_x = |index: usize| -27.0 + index as f32 * 5.0;
	for (index, width) in bars.iter().enumerate() {
		let x = bar_x(index);
		white(&mut model, format!("Bar{width}"), Vec3::new(x - width * 0.5, -8.0, -1.25), Vec3::new(x + width * 0.5, 8.0, -1.0));
		let at = turn * Vec3::new(x, 0.0, 0.0);
		probes.push(probe(&format!("bar{width}"), at.x, 0.0, serde_json::json!({"kind": "bar", "width": width, "x": x, "y": at.y})));
	}
	let slots = [0.01f32, 0.02, 0.05, 0.1, 0.2, 0.4];
	let slot_x = |index: usize| -2.0 + index as f32 * 5.0;
	let mut cursor = -7.0;
	for (index, width) in slots.iter().enumerate() {
		let x = slot_x(index);
		white(&mut model, format!("Plate{index}"), Vec3::new(cursor, -14.0, -1.0), Vec3::new(x - width * 0.5, 14.0, 0.0));
		cursor = x + width * 0.5;
		let at = turn * Vec3::new(x, 0.0, 0.0);
		probes.push(probe(&format!("slot{width}"), at.x, 0.0, serde_json::json!({"kind": "slot", "width": width, "x": x, "y": at.y})));
	}
	white(&mut model, "PlateEnd".into(), Vec3::new(cursor, -14.0, -1.0), Vec3::new(34.0, 14.0, 0.0));
	let environment = Environment { global_shadows: false, ..ambient() };
	let camera = |x: f32, distance: f32| CameraSpec::LookAt { eye: (turn * Vec3::new(x, 0.0, -distance)).into(), target: (turn * Vec3::new(x, 0.0, 0.0)).into(), fov: 40.0 };
	let views = [
		("d20-bars", (bar_x(0) + bar_x(3)) * 0.5, 20.0),
		("d20-slots-a", slot_x(1), 20.0),
		("d20-slots-b", slot_x(4), 20.0),
		("d40-bars", (bar_x(0) + bar_x(3)) * 0.5, 40.0),
		("d40-slots", (slot_x(0) + slot_x(5)) * 0.5, 40.0),
		("d80", (bar_x(0) + slot_x(5)) * 0.5, 80.0),
	];
	let runs = views.iter().map(|(label, x, distance)| Run { label: (*label).into(), environment: environment.clone(), camera: camera(*x, *distance) }).collect();
	Experiment { name: "ao-thin".into(), model, probes, runs }
}

/// What the occlusion sees: bars 1.5 x 5 x 1 half a stud in front of a white wall facing -Z (its face
/// at z = 0), each a different kind of part (SmoothPlastic, black Neon, Glass, Glass at transparency
/// 0.5, ForceField, transparency 0.01, 0.05, 0.5 and 0.95, CastShadow off); below them, white
/// SmoothPlastic bars in front of tiles of the wall of different kinds (SmoothPlastic, Glass, Glass at
/// 0.5, transparency 0.01 and 0.5, ForceField; the sky behind them). Seen head-on from 35 studs and
/// from 24 (left and right halves), field of view 40. Ambient alone, no shadows.
pub fn ao_sees() -> Experiment {
	let mut model = ModelBuilder::new("AoSees");
	let mut probes = Vec::new();
	// (label, material, transparency, cast shadow, colour)
	let occluders: [(&str, u32, f32, bool, [u8; 3]); 10] = [
		("plastic", 272, 0.0, true, [255; 3]),
		("neon", 288, 0.0, true, [0; 3]),
		("glass", 1568, 0.0, true, [255; 3]),
		("glass0.5", 1568, 0.5, true, [255; 3]),
		("forcefield", 1584, 0.0, true, [255; 3]),
		("t0.01", 272, 0.01, true, [255; 3]),
		("t0.05", 272, 0.05, true, [255; 3]),
		("t0.5", 272, 0.5, true, [255; 3]),
		("t0.95", 272, 0.95, true, [255; 3]),
		("noshadow", 272, 0.0, false, [255; 3]),
	];
	let spacing = 3.6;
	let x_of = |index: usize, count: usize| (index as f32 - (count as f32 - 1.0) * 0.5) * spacing;
	// the upper wall, behind the occluders
	model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 4.25, 0.5)), size: Vec3::new(40.0, 7.5, 1.0), color: [255; 3], ..Default::default() });
	for (index, (label, material, transparency, cast_shadow, color)) in occluders.iter().enumerate() {
		let x = x_of(index, occluders.len());
		model.part(&PartSpec { name: format!("Occluder-{label}"), cframe: Affine3A::from_translation(Vec3::new(x, 4.0, -1.0)), size: Vec3::new(1.5, 5.0, 1.0), color: *color, material: *material, transparency: *transparency, cast_shadow: *cast_shadow, ..Default::default() });
		probes.push(Probe { label: format!("occluder-{label}"), position: [x, 4.0, 0.0], inputs: serde_json::json!({"row": "occluder", "x": x, "y": 4.0}) });
	}
	// the lower wall: tiles 3.6 wide of each kind (0.5 thick), a white bar half a stud in front of each
	let receivers: [(&str, u32, f32); 6] = [("plastic", 272, 0.0), ("glass", 1568, 0.0), ("glass0.5", 1568, 0.5), ("t0.01", 272, 0.01), ("t0.5", 272, 0.5), ("forcefield", 1584, 0.0)];
	for (index, (label, material, transparency)) in receivers.iter().enumerate() {
		let x = x_of(index, receivers.len()) * 1.6;
		model.part(&PartSpec { name: format!("Tile-{label}"), cframe: Affine3A::from_translation(Vec3::new(x, -4.25, 0.25)), size: Vec3::new(5.0, 7.5, 0.5), color: [255; 3], material: *material, transparency: *transparency, ..Default::default() });
		model.part(&PartSpec { name: format!("Bar-{label}"), cframe: Affine3A::from_translation(Vec3::new(x, -4.0, -1.0)), size: Vec3::new(1.5, 5.0, 1.0), color: [255; 3], ..Default::default() });
		probes.push(Probe { label: format!("receiver-{label}"), position: [x, -4.0, 0.0], inputs: serde_json::json!({"row": "receiver", "x": x, "y": -4.0}) });
	}
	let environment = Environment { global_shadows: false, ..ambient() };
	let camera = |x: f32, distance: f32| CameraSpec::LookAt { eye: Vec3::new(x, 0.0, -distance).into(), target: Vec3::new(x, 0.0, 0.0).into(), fov: 40.0 };
	let runs = vec![
		Run { label: "d35".into(), environment: environment.clone(), camera: camera(0.0, 35.0) },
		Run { label: "d24-left".into(), environment: environment.clone(), camera: camera(-9.0, 24.0) },
		Run { label: "d24-right".into(), environment, camera: camera(9.0, 24.0) },
	];
	Experiment { name: "ao-sees".into(), model, probes, runs }
}

/// Where a part stops being opaque to the occlusion (ao-sees: Transparency 0.01 occludes and takes it,
/// 0.05 neither): white SmoothPlastic bars 1.2 x 5 x 1 half a stud in front of a white wall at
/// Transparency 0 to 0.04 (top row); white bars half a stud in front of tiles at those transparencies,
/// the sky behind them (middle row); and the same bars 2 studs in front of a black Neon panel, whose
/// faces show how much of what is behind them they let through (bottom row). Seen head-on from 30
/// studs, field of view 40. Ambient alone, no shadows.
pub fn ao_transparency() -> Experiment {
	let mut model = ModelBuilder::new("AoTransparency");
	let mut probes = Vec::new();
	let levels = [0.0f32, 0.005, 0.01, 0.015, 0.02, 0.03, 0.04];
	let x_of = |index: usize| (index as f32 - 3.0) * 4.0;
	let white = |model: &mut ModelBuilder, name: String, centre: Vec3, size: Vec3, transparency: f32| {
		model.part(&PartSpec { name, cframe: Affine3A::from_translation(centre), size, color: [255; 3], transparency, ..Default::default() });
	};
	white(&mut model, "Wall".into(), Vec3::new(0.0, 6.0, 0.5), Vec3::new(32.0, 7.0, 1.0), 0.0);
	model.part(&PartSpec { name: "Black".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -6.5, 2.5)), size: Vec3::new(32.0, 6.0, 1.0), color: [0; 3], material: 288, ..Default::default() });
	for (index, transparency) in levels.iter().enumerate() {
		let x = x_of(index);
		white(&mut model, format!("Occluder{transparency}"), Vec3::new(x, 6.0, -1.0), Vec3::new(1.2, 5.0, 1.0), *transparency);
		white(&mut model, format!("Tile{transparency}"), Vec3::new(x, 0.0, 0.25), Vec3::new(3.6, 5.5, 0.5), *transparency);
		white(&mut model, format!("Bar{transparency}"), Vec3::new(x, 0.0, -1.0), Vec3::new(1.2, 4.0, 1.0), 0.0);
		white(&mut model, format!("Blend{transparency}"), Vec3::new(x, -6.5, 0.0), Vec3::new(1.2, 4.0, 1.0), *transparency);
		for (row, y) in [("occluder", 6.0f32), ("receiver", 0.0), ("blend", -6.5)] {
			probes.push(Probe { label: format!("{row}{transparency}"), position: [x, y, 0.0], inputs: serde_json::json!({"row": row, "transparency": transparency, "x": x, "y": y}) });
		}
	}
	let environment = Environment { global_shadows: false, ..ambient() };
	let runs = vec![Run { label: "d30".into(), environment, camera: CameraSpec::LookAt { eye: Vec3::new(0.0, 0.0, -30.0).into(), target: Vec3::ZERO.into(), fov: 40.0 } }];
	Experiment { name: "ao-transparency".into(), model, probes, runs }
}

