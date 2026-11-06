//! GUI experiments: SurfaceGuis on part faces and BillboardGuis over parts, drawing test patterns of
//! Frames and TextLabels (notes/render.md "GUIs").

use glam::{Affine3A, Quat, Vec3};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Color3, Enum, Font, Ref, UDim, UDim2, Vector2, Vector3};

use super::{Experiment, Probe, Run, sun_only};
use crate::render::camera::CameraSpec;
use crate::render::environment::sun_direction;
use crate::scene::Environment;
use crate::scene::build::{ModelBuilder, PartSpec, facing};

/// Flat ambient light alone (Ambient 0.7, no sun or sky light).
fn flat() -> Environment {
	Environment {
		brightness: 0.0,
		ambient: Vec3::splat(0.7),
		outdoor_ambient: Vec3::ZERO,
		environment_diffuse_scale: 0.0,
		environment_specular_scale: 0.0,
		global_shadows: false,
		..Environment::default()
	}
}

fn udim2(x_scale: f32, x_offset: i32, y_scale: f32, y_offset: i32) -> UDim2 {
	UDim2::new(UDim::new(x_scale, x_offset), UDim::new(y_scale, y_offset))
}

fn rgb(color: [u8; 3]) -> Color3 {
	Color3::new(color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0)
}

/// SurfaceGui settings (the properties the renderer reads).
#[derive(Clone)]
pub struct Surface {
	pub face: u32,
	/// `Enum.SurfaceGuiSizingMode`: 0 FixedSize (CanvasSize), 1 PixelsPerStud.
	pub sizing: u32,
	pub canvas: [f32; 2],
	pub pixels_per_stud: f32,
	pub light_influence: f32,
	pub brightness: f32,
	pub always_on_top: bool,
	pub z_offset: f32,
	pub clips: bool,
}

impl Default for Surface {
	fn default() -> Self {
		Self { face: 5, sizing: 0, canvas: [200.0, 150.0], pixels_per_stud: 50.0, light_influence: 0.0, brightness: 1.0, always_on_top: false, z_offset: 0.0, clips: true }
	}
}

pub fn surface_gui(model: &mut ModelBuilder, part: Ref, name: &str, surface: &Surface) -> Ref {
	let builder = InstanceBuilder::new("SurfaceGui")
		.with_name(name)
		.with_property("Face", Enum::from_u32(surface.face))
		.with_property("SizingMode", Enum::from_u32(surface.sizing))
		.with_property("CanvasSize", Vector2::new(surface.canvas[0], surface.canvas[1]))
		.with_property("PixelsPerStud", surface.pixels_per_stud)
		.with_property("LightInfluence", surface.light_influence)
		.with_property("Brightness", surface.brightness)
		.with_property("AlwaysOnTop", surface.always_on_top)
		.with_property("ZOffset", surface.z_offset)
		.with_property("ClipsDescendants", surface.clips)
		.with_property("ZIndexBehavior", Enum::from_u32(1));
	model.child(part, builder)
}

/// BillboardGui settings.
#[derive(Clone)]
pub struct Billboard {
	/// Size: (x studs, x pixels, y studs, y pixels).
	pub size: (f32, i32, f32, i32),
	pub size_offset: [f32; 2],
	pub studs_offset: Vec3,
	pub studs_offset_world: Vec3,
	pub extents_offset: Vec3,
	pub extents_offset_world: Vec3,
	pub light_influence: f32,
	pub brightness: f32,
	pub always_on_top: bool,
	pub clips: bool,
	pub distance_lower: f32,
	pub distance_upper: f32,
	pub distance_step: f32,
}

impl Default for Billboard {
	fn default() -> Self {
		Self {
			size: (4.0, 0, 1.0, 0),
			size_offset: [0.0; 2],
			studs_offset: Vec3::ZERO,
			studs_offset_world: Vec3::ZERO,
			extents_offset: Vec3::ZERO,
			extents_offset_world: Vec3::ZERO,
			light_influence: 0.0,
			brightness: 1.0,
			always_on_top: false,
			clips: false,
			distance_lower: 0.0,
			distance_upper: -1.0,
			distance_step: 0.0,
		}
	}
}

fn vector3(value: Vec3) -> Vector3 {
	Vector3::new(value.x, value.y, value.z)
}

pub fn billboard_gui(model: &mut ModelBuilder, parent: Ref, name: &str, billboard: &Billboard) -> Ref {
	let (x_scale, x_offset, y_scale, y_offset) = billboard.size;
	let builder = InstanceBuilder::new("BillboardGui")
		.with_name(name)
		.with_property("Size", udim2(x_scale, x_offset, y_scale, y_offset))
		.with_property("SizeOffset", Vector2::new(billboard.size_offset[0], billboard.size_offset[1]))
		.with_property("StudsOffset", vector3(billboard.studs_offset))
		.with_property("StudsOffsetWorldSpace", vector3(billboard.studs_offset_world))
		.with_property("ExtentsOffset", vector3(billboard.extents_offset))
		.with_property("ExtentsOffsetWorldSpace", vector3(billboard.extents_offset_world))
		.with_property("LightInfluence", billboard.light_influence)
		.with_property("Brightness", billboard.brightness)
		.with_property("AlwaysOnTop", billboard.always_on_top)
		.with_property("ClipsDescendants", billboard.clips)
		.with_property("DistanceLowerLimit", billboard.distance_lower)
		.with_property("DistanceUpperLimit", billboard.distance_upper)
		.with_property("DistanceStep", billboard.distance_step)
		.with_property("ZIndexBehavior", Enum::from_u32(1));
	model.child(parent, builder)
}

/// A borderless Frame.
pub fn frame(model: &mut ModelBuilder, parent: Ref, name: &str, position: UDim2, size: UDim2, color: [u8; 3], transparency: f32) -> Ref {
	let builder = InstanceBuilder::new("Frame")
		.with_name(name)
		.with_property("Position", position)
		.with_property("Size", size)
		.with_property("BackgroundColor3", rgb(color))
		.with_property("BackgroundTransparency", transparency)
		.with_property("BorderSizePixel", 0i32);
	model.child(parent, builder)
}

/// A TextLabel without a background.
pub fn label(model: &mut ModelBuilder, parent: Ref, text: &str, position: UDim2, size: UDim2, color: [u8; 3], text_size: f32) -> Ref {
	let builder = InstanceBuilder::new("TextLabel")
		.with_name(text)
		.with_property("Position", position)
		.with_property("Size", size)
		.with_property("BackgroundTransparency", 1.0f32)
		.with_property("BorderSizePixel", 0i32)
		.with_property("Text", text.to_string())
		.with_property("TextColor3", rgb(color))
		.with_property("TextSize", text_size)
		.with_property("FontFace", Font::regular("rbxasset://fonts/families/BuilderSans.json"));
	model.child(parent, builder)
}

/// Orientation pattern: red, green, blue and white quarter-size squares in the top-left, top-right,
/// bottom-left and bottom-right corners, a yellow bar along the top edge's middle and a label.
pub fn pattern(model: &mut ModelBuilder, gui: Ref) {
	frame(model, gui, "TL", udim2(0.0, 0, 0.0, 0), udim2(0.25, 0, 0.25, 0), [255, 0, 0], 0.0);
	frame(model, gui, "TR", udim2(0.75, 0, 0.0, 0), udim2(0.25, 0, 0.25, 0), [0, 255, 0], 0.0);
	frame(model, gui, "BL", udim2(0.0, 0, 0.75, 0), udim2(0.25, 0, 0.25, 0), [0, 0, 255], 0.0);
	frame(model, gui, "BR", udim2(0.75, 0, 0.75, 0), udim2(0.25, 0, 0.25, 0), [255, 255, 255], 0.0);
	frame(model, gui, "Top", udim2(0.375, 0, 0.0, 0), udim2(0.25, 0, 0.0625, 0), [255, 255, 0], 0.0);
	label(model, gui, "GUI", udim2(0.25, 0, 0.375, 0), udim2(0.5, 0, 0.25, 0), [255, 255, 255], 24.0);
}

/// The swatches' colours: grays in the top row, colours in the bottom one.
const SWATCHES: [[u8; 3]; 12] = [
	[0, 0, 0],
	[51, 51, 51],
	[102, 102, 102],
	[153, 153, 153],
	[204, 204, 204],
	[255, 255, 255],
	[255, 0, 0],
	[0, 255, 0],
	[0, 0, 255],
	[255, 128, 0],
	[128, 0, 255],
	[40, 180, 120],
];

/// Swatches over a canvas (two rows of six); returns each swatch's centre in canvas fractions.
pub fn swatches(model: &mut ModelBuilder, gui: Ref) -> Vec<([u8; 3], [f32; 2])> {
	let mut centres = Vec::new();
	for (index, color) in SWATCHES.iter().enumerate() {
		let (column, row) = ((index % 6) as f32, (index / 6) as f32);
		frame(model, gui, &format!("Swatch{index}"), udim2(column / 6.0, 0, row * 0.5, 0), udim2(1.0 / 6.0, 0, 0.5, 0), *color, 0.0);
		centres.push((*color, [(column + 0.5) / 6.0, row * 0.5 + 0.25]));
	}
	centres
}

/// Probes at a SurfaceGui's swatches on a part's Front face (canvas right is -X, down is -Y).
fn surface_probes(probes: &mut Vec<Probe>, label: &str, cframe: Affine3A, size: Vec3, centres: &[([u8; 3], [f32; 2])], inputs: serde_json::Value) {
	for (index, (color, [u, v])) in centres.iter().enumerate() {
		let local = Vec3::new((0.5 - u) * size.x, (0.5 - v) * size.y, -size.z * 0.5);
		let mut inputs = inputs.clone();
		inputs["color"] = serde_json::json!(color);
		inputs["normal"] = serde_json::json!(cframe.transform_vector3(Vec3::NEG_Z).to_array());
		probes.push(Probe { label: format!("{label}-{index}"), position: cframe.transform_point3(local).into(), inputs });
	}
}

/// The view of a part's face head-on from `distance` studs (up along the face's own up direction).
fn head_on(cframe: Affine3A, face: u32, half_depth: f32, distance: f32, fov: f32) -> CameraSpec {
	let normals = [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X, Vec3::NEG_Y, Vec3::NEG_Z];
	let normal = cframe.transform_vector3(normals[face as usize]).normalize();
	let center = cframe.transform_point3(normals[face as usize] * half_depth);
	// a hair off the face normal for top and bottom views so the camera has an up direction
	let eye = center + normal * distance + if normal.y.abs() > 0.99 { Vec3::new(0.0, 0.0, -0.001 * distance) } else { Vec3::ZERO };
	CameraSpec::LookAt { eye: eye.into(), target: center.into(), fov }
}

/// SurfaceGuis: the pattern on every face of a block (and of a turned block), a small canvas seen
/// magnified, gray and colour swatches at LightInfluence 0 / 0.5 / 1 and Brightness 1 / 2 / 5 under
/// flat ambient light, the sun alone and the default lighting, text, AlwaysOnTop behind a wall, and
/// the pattern on a ball and a cylinder.
pub fn gui_surface_basics() -> Experiment {
	let mut model = ModelBuilder::new("GuiSurfaceBasics");
	let mut runs = Vec::new();
	let mut probes = Vec::new();
	let gray = [100u8, 100, 100];
	// every face of an 8 x 6 x 4 block (canvas 160 x 120, stretched to each face)
	let size = Vec3::new(8.0, 6.0, 4.0);
	for (index, (label, cframe)) in [
		("block", Affine3A::from_translation(Vec3::ZERO)),
		("turned", Affine3A::from_rotation_translation(Quat::from_euler(glam::EulerRot::YXZ, 35f32.to_radians(), 20f32.to_radians(), 10f32.to_radians()), Vec3::new(40.0, 0.0, 0.0))),
	]
	.into_iter()
	.enumerate()
	{
		let part = model.part(&PartSpec { name: label.into(), cframe, size, color: gray, ..Default::default() });
		for face in 0..6 {
			let gui = surface_gui(&mut model, part, &format!("Face{face}"), &Surface { face, canvas: [160.0, 120.0], ..Default::default() });
			pattern(&mut model, gui);
		}
		let faces: &[u32] = if index == 0 { &[0, 1, 2, 3, 4, 5] } else { &[5, 1] };
		for &face in faces {
			let half = [size.x, size.y, size.z, size.x, size.y, size.z][face as usize] * 0.5;
			runs.push(Run { label: format!("{label}-face{face}"), environment: flat(), camera: head_on(cframe, face, half, 24.0, 30.0) });
		}
	}
	// a 32 x 24 canvas on an 8 x 6 face, magnified (51 px per canvas pixel): texel filtering
	let magnified = Affine3A::from_translation(Vec3::new(80.0, 0.0, 0.0));
	let part = model.part(&PartSpec { name: "magnified".into(), cframe: magnified, size: Vec3::new(8.0, 6.0, 1.0), color: gray, ..Default::default() });
	let gui = surface_gui(&mut model, part, "Small", &Surface { canvas: [32.0, 24.0], ..Default::default() });
	pattern(&mut model, gui);
	for index in 0..8 {
		frame(&mut model, gui, &format!("Dot{index}"), udim2(0.0, 10 + 2 * index, 0.0, 9 + (index % 2)), udim2(0.0, 1, 0.0, 1), [255, 255, 255], 0.0);
	}
	runs.push(Run { label: "magnified".into(), environment: flat(), camera: head_on(magnified, 5, 0.5, 7.0, 60.0) });
	// swatches facing the sun: (LightInfluence, Brightness) per block, stacked
	let environment = sun_only(1.0);
	let sun = sun_direction(environment.clock_time, environment.geographic_latitude);
	let lights: [(f32, f32); 6] = [(0.0, 1.0), (0.5, 1.0), (1.0, 1.0), (0.0, 2.0), (0.0, 5.0), (0.5, 3.0)];
	let board = Vec3::new(120.0, 0.0, 0.0);
	let up = Vec3::Y.cross(sun).cross(sun).normalize() * -1.0;
	for (index, (light_influence, brightness)) in lights.iter().enumerate() {
		let center = board + up * (index as f32 - 2.5) * 2.5;
		let cframe = facing(center, sun);
		let size = Vec3::new(12.0, 2.0, 0.5);
		let part = model.part(&PartSpec { name: format!("light{index}"), cframe, size, color: [128, 128, 128], ..Default::default() });
		let gui = surface_gui(&mut model, part, "Swatches", &Surface { canvas: [600.0, 100.0], light_influence: *light_influence, brightness: *brightness, ..Default::default() });
		let centres = swatches(&mut model, gui);
		surface_probes(&mut probes, &format!("light{index}"), cframe, size, &centres, serde_json::json!({"lightInfluence": light_influence, "brightness": brightness}));
	}
	// seen from 20 degrees off the sun direction (no mirror highlight)
	let side = sun.cross(Vec3::Y).normalize();
	let eye = board + (sun * 20f32.to_radians().cos() + side * 20f32.to_radians().sin()) * 60.0;
	let camera = CameraSpec::LookAt { eye: eye.into(), target: board.into(), fov: 20.0 };
	runs.push(Run { label: "light-flat".into(), environment: flat(), camera: camera.clone() });
	runs.push(Run { label: "light-sun".into(), environment: environment.clone(), camera: camera.clone() });
	runs.push(Run { label: "light-default".into(), environment: Environment::default(), camera: camera.clone() });
	runs.push(Run { label: "light-dark".into(), environment: Environment { ambient: Vec3::ZERO, ..sun_only(0.0) }, camera });
	// text
	let text = Affine3A::from_translation(Vec3::new(160.0, 0.0, 0.0));
	let part = model.part(&PartSpec { name: "text".into(), cframe: text, size: Vec3::new(12.0, 4.0, 1.0), color: [30, 30, 30], ..Default::default() });
	let gui = surface_gui(&mut model, part, "Text", &Surface { canvas: [600.0, 200.0], ..Default::default() });
	for (index, text_size) in [14.0f32, 24.0, 48.0].iter().enumerate() {
		label(&mut model, gui, &format!("Size {text_size} Hello, world"), udim2(0.0, 10, 0.0, 10 + index as i32 * 60), udim2(1.0, -20, 0.0, 56), [255, 255, 255], *text_size);
	}
	runs.push(Run { label: "text".into(), environment: flat(), camera: head_on(text, 5, 0.5, 16.0, 40.0) });
	// AlwaysOnTop: two patterned blocks behind a wall, the left one on top
	let hidden = Vec3::new(200.0, 0.0, 0.0);
	for (index, on_top) in [true, false].iter().enumerate() {
		let cframe = Affine3A::from_translation(hidden + Vec3::new(index as f32 * 9.0 - 4.5, 0.0, 0.0));
		let part = model.part(&PartSpec { name: format!("ontop{index}"), cframe, size: Vec3::new(8.0, 6.0, 1.0), color: gray, ..Default::default() });
		let gui = surface_gui(&mut model, part, "OnTop", &Surface { always_on_top: *on_top, ..Default::default() });
		pattern(&mut model, gui);
	}
	model.part(&PartSpec { name: "wall".into(), cframe: Affine3A::from_translation(hidden + Vec3::new(0.0, -1.5, -3.0)), size: Vec3::new(20.0, 3.0, 1.0), color: [200, 120, 60], ..Default::default() });
	runs.push(Run { label: "alwaysontop".into(), environment: flat(), camera: CameraSpec::LookAt { eye: (hidden + Vec3::new(0.0, 0.0, -40.0)).into(), target: hidden.into(), fov: 30.0 } });
	// a ball and a cylinder (along X): where does the canvas go?
	let shapes = Vec3::new(240.0, 0.0, 0.0);
	for (index, (name, shape, size)) in [("ball", 0u32, Vec3::splat(6.0)), ("cylinder", 2, Vec3::new(8.0, 6.0, 6.0))].iter().enumerate() {
		let cframe = Affine3A::from_translation(shapes + Vec3::new(index as f32 * 10.0 - 5.0, 0.0, 0.0));
		let part = model.part(&PartSpec { name: (*name).into(), cframe, size: *size, color: gray, shape: Some(*shape), ..Default::default() });
		let gui = surface_gui(&mut model, part, "Shape", &Surface::default());
		pattern(&mut model, gui);
	}
	runs.push(Run { label: "shapes".into(), environment: flat(), camera: CameraSpec::LookAt { eye: (shapes + Vec3::new(0.0, 0.0, -40.0)).into(), target: shapes.into(), fov: 30.0 } });
	runs.push(Run { label: "shapes-oblique".into(), environment: flat(), camera: CameraSpec::LookAt { eye: (shapes + Vec3::new(25.0, 12.0, -30.0)).into(), target: shapes.into(), fov: 30.0 } });
	Experiment { name: "gui-surface-basics".into(), model, probes, runs }
}

/// How GUIs are lit: white SmoothPlastic slabs turned from facing the sun (n·l 1, 0.87, 0.5, 0.26,
/// 0, -0.5), each with four SurfaceGuis on its front (LightInfluence 1, 0.75, 0.5, 0.25: a white
/// and a mid-gray frame each, the left fifth bare as the part's own reference), and a column of
/// billboards (the same four), under each lighting term alone and the default lighting, the
/// billboards seen from three directions around the sun.
pub fn gui_light() -> Experiment {
	let mut model = ModelBuilder::new("GuiLight");
	let mut runs = Vec::new();
	let mut probes = Vec::new();
	let reference = sun_only(1.0);
	let sun = sun_direction(reference.clock_time, reference.geographic_latitude);
	// p: perpendicular to the sun, towards the camera side
	let p = Vec3::Y.cross(sun).normalize();
	let influences = [1.0f32, 0.75, 0.5, 0.25];
	let size = Vec3::new(10.0, 2.0, 0.5);
	for (index, angle) in [0.0f32, 30.0, 60.0, 75.0, 90.0, 120.0].iter().enumerate() {
		let normal = sun * angle.to_radians().cos() + p * angle.to_radians().sin();
		let center = Vec3::new(0.0, index as f32 * 3.0, 0.0);
		let cframe = facing(center, normal);
		let part = model.part(&PartSpec { name: format!("slab{angle}"), cframe, size, color: [255, 255, 255], ..Default::default() });
		for (column, light_influence) in influences.iter().enumerate() {
			let gui = surface_gui(&mut model, part, &format!("Light{column}"), &Surface { canvas: [500.0, 100.0], light_influence: *light_influence, ..Default::default() });
			let x = 100 * (column as i32 + 1) + 10;
			frame(&mut model, gui, "White", udim2(0.0, x, 0.0, 10), udim2(0.0, 80, 0.0, 35), [255, 255, 255], 0.0);
			frame(&mut model, gui, "Gray", udim2(0.0, x, 0.0, 55), udim2(0.0, 80, 0.0, 35), [128, 128, 128], 0.0);
			for (row, (color, v)) in [([255u8; 3], 0.275f32), ([128; 3], 0.725)].iter().enumerate() {
				let u = (x as f32 + 40.0) / 500.0;
				let local = Vec3::new((0.5 - u) * size.x, (0.5 - v) * size.y, -size.z * 0.5);
				let inputs = serde_json::json!({"kind": "surface", "lightInfluence": light_influence, "color": color, "ndotl": angle.to_radians().cos(), "normal": normal.to_array(), "row": row});
				probes.push(Probe { label: format!("surface{angle}-{light_influence}-{row}"), position: cframe.transform_point3(local).into(), inputs });
			}
		}
		// the bare part beside them
		let local = Vec3::new(0.4 * size.x, 0.0, -size.z * 0.5);
		probes.push(Probe { label: format!("part{angle}"), position: cframe.transform_point3(local).into(), inputs: serde_json::json!({"kind": "part", "ndotl": angle.to_radians().cos(), "normal": normal.to_array()}) });
	}
	// billboards: four columns over small anchors, each a white and a gray half
	let column_origin = Vec3::new(0.0, -6.0, 0.0);
	for (column, light_influence) in influences.iter().enumerate() {
		let center = column_origin + p * (column as f32 * 3.0 - 4.5);
		let part = model.part(&PartSpec { name: format!("anchor{column}"), cframe: Affine3A::from_translation(center), size: Vec3::splat(0.2), color: [0, 0, 0], ..Default::default() });
		let gui = billboard_gui(&mut model, part, "Light", &Billboard { size: (2.0, 0, 2.0, 0), light_influence: *light_influence, ..Default::default() });
		frame(&mut model, gui, "White", udim2(0.0, 0, 0.0, 0), udim2(1.0, 0, 0.5, 0), [255, 255, 255], 0.0);
		frame(&mut model, gui, "Gray", udim2(0.0, 0, 0.5, 0), udim2(1.0, 0, 0.5, 0), [128, 128, 128], 0.0);
	}
	// the camera sees every slab's front: between the sun and p
	let toward = (sun + p).normalize();
	let target = Vec3::new(0.0, 4.0, 0.0);
	let camera = CameraSpec::LookAt { eye: (target + toward * 70.0).into(), target: target.into(), fov: 25.0 };
	let environments: [(&str, Environment); 7] = [
		("sun", reference.clone()),
		("sun-shadows", Environment { global_shadows: true, ..reference.clone() }),
		("sky", Environment { brightness: 0.0, environment_diffuse_scale: 1.0, ..sun_only(0.0) }),
		("ambient", Environment { brightness: 0.0, ambient: Vec3::splat(0.5), ..sun_only(0.0) }),
		("outdoor", Environment { brightness: 0.0, outdoor_ambient: Vec3::splat(0.5), ..sun_only(0.0) }),
		("specular", Environment { brightness: 0.0, environment_specular_scale: 1.0, ..sun_only(0.0) }),
		("default", Environment::default()),
	];
	for (label, environment) in &environments {
		runs.push(Run { label: (*label).into(), environment: environment.clone(), camera: camera.clone() });
	}
	// billboards from the sun's side, across it and from behind (their normal is the view direction)
	for (label, direction) in [("bb-front", (sun + p * 0.2).normalize()), ("bb-across", (p + sun * 0.1).normalize()), ("bb-behind", (p - sun * 0.8).normalize())] {
		for (light, environment) in [("sun", &environments[0].1), ("default", &environments[6].1)] {
			let eye = column_origin + direction * 40.0;
			runs.push(Run { label: format!("{label}-{light}"), environment: environment.clone(), camera: CameraSpec::LookAt { eye: eye.into(), target: column_origin.into(), fov: 25.0 } });
		}
	}
	Experiment { name: "gui-light".into(), model, probes, runs }
}

/// Brightness at LightInfluence 0: gray frames (0.2-1.0) at Brightness 0.5-8, spaced apart on a dark
/// slab, with no light and ExposureCompensation -3 (radiance up to 30 stays under the bloom
/// threshold and the tone curve's knee), and the same at 0 EV.
pub fn gui_brightness() -> Experiment {
	let mut model = ModelBuilder::new("GuiBrightness");
	let mut probes = Vec::new();
	let brightness = [0.5f32, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];
	let grays = [51u8, 102, 153, 204, 255];
	let size = Vec3::new(10.0, 2.0, 0.5);
	for (row, value) in brightness.iter().enumerate() {
		let cframe = Affine3A::from_translation(Vec3::new(0.0, row as f32 * 2.5, 0.0));
		let part = model.part(&PartSpec { name: format!("row{value}"), cframe, size, color: [0, 0, 0], ..Default::default() });
		let gui = surface_gui(&mut model, part, "Brightness", &Surface { canvas: [500.0, 100.0], brightness: *value, ..Default::default() });
		for (column, gray) in grays.iter().enumerate() {
			frame(&mut model, gui, &format!("Gray{gray}"), udim2(0.0, column as i32 * 100 + 30, 0.0, 30), udim2(0.0, 40, 0.0, 40), [*gray; 3], 0.0);
			let u = (column as f32 * 100.0 + 50.0) / 500.0;
			let local = Vec3::new((0.5 - u) * size.x, 0.0, -size.z * 0.5);
			probes.push(Probe { label: format!("b{value}-{gray}"), position: cframe.transform_point3(local).into(), inputs: serde_json::json!({"brightness": value, "color": [gray, gray, gray]}) });
		}
	}
	let dark = Environment { ambient: Vec3::ZERO, ..sun_only(0.0) };
	let camera = CameraSpec::LookAt { eye: [0.0, 8.75, -60.0], target: [0.0, 8.75, 0.0], fov: 25.0 };
	let runs = [-3.0f32, -2.0, 0.0]
		.iter()
		.map(|exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: *exposure, ..dark.clone() }, camera: camera.clone() })
		.collect();
	Experiment { name: "gui-brightness".into(), model, probes, runs }
}

/// Brightness through partial transparency (an effect's rift: Studio's faint glow under
/// a Brightness-15 SurfaceGui stays red where ror's turns peach): frames of BackgroundTransparency 0,
/// 0.3, 0.6 and 0.9 in an effect's orange-red and in white, at Brightness 1, 5 and 15, on black, gray and
/// white panels under flat light, at ExposureCompensation -3 (under the bloom and the tone curve's
/// knee) and 0. Probes on every frame and on each panel between its frames.
pub fn gui_brightness_alpha() -> Experiment {
	let mut model = ModelBuilder::new("GuiBrightnessAlpha");
	let mut probes = Vec::new();
	let size = Vec3::new(16.0, 2.0, 0.5);
	let transparencies = [0.0f32, 0.3, 0.6, 0.9];
	let colors: [(&str, [u8; 3]); 2] = [("orange", [255, 42, 0]), ("white", [255, 255, 255])];
	let panels: [(&str, [u8; 3]); 3] = [("black", [0, 0, 0]), ("gray", [128, 128, 128]), ("white", [230, 230, 230])];
	let mut row = 0;
	for (panel, panel_color) in panels {
		for brightness in [1.0f32, 5.0, 15.0] {
			let cframe = Affine3A::from_translation(Vec3::new(0.0, row as f32 * 2.5, 0.0));
			row += 1;
			let part = model.part(&PartSpec { name: format!("{panel}-b{brightness}"), cframe, size, color: panel_color, ..Default::default() });
			let gui = surface_gui(&mut model, part, "Alpha", &Surface { canvas: [800.0, 100.0], brightness, ..Default::default() });
			let point = |canvas_x: f32| cframe.transform_point3(Vec3::new((0.5 - canvas_x / 800.0) * size.x, 0.0, -size.z * 0.5));
			let cells = colors.iter().flat_map(|color| transparencies.iter().map(move |transparency| (color, *transparency)));
			for (index, ((name, color), transparency)) in cells.enumerate() {
				frame(&mut model, gui, &format!("{name}{transparency}"), udim2(0.0, index as i32 * 100 + 30, 0.0, 30), udim2(0.0, 40, 0.0, 40), *color, transparency);
				probes.push(Probe {
					label: format!("{panel}-b{brightness}-{name}-t{transparency}"),
					position: point(index as f32 * 100.0 + 50.0).into(),
					inputs: serde_json::json!({"panel": panel_color, "brightness": brightness, "color": color, "transparency": transparency}),
				});
			}
			probes.push(Probe { label: format!("{panel}-b{brightness}-panel"), position: point(100.0).into(), inputs: serde_json::json!({"panel": panel_color, "brightness": brightness}) });
		}
	}
	let camera = CameraSpec::LookAt { eye: [0.0, 10.0, -80.0], target: [0.0, 10.0, 0.0], fov: 25.0 };
	let runs = [-3.0f32, 0.0].iter().map(|exposure| Run { label: format!("ev{exposure}"), environment: Environment { exposure_compensation: *exposure, ..flat() }, camera: camera.clone() }).collect();
	Experiment { name: "gui-brightness-alpha".into(), model, probes, runs }
}

/// What a half-transparent GUI does to the bloom of a bright opaque one under it (gui-brightness-alpha:
/// half-transparent frames feed none, opaque ones do): a white Brightness-15 frame, its right half under
/// a black frame at BackgroundTransparency 0.5 in the same SurfaceGui, in a second one above it
/// (ZOffset 1) and, as the control, uncovered; on black panels in the dark at ExposureCompensation 0.
/// Probes on each half and 12 canvas pixels out on each side.
pub fn gui_bloom_layers() -> Experiment {
	let mut model = ModelBuilder::new("GuiBloomLayers");
	let mut probes = Vec::new();
	let size = Vec3::new(10.0, 2.0, 0.5);
	for (row, case) in ["control", "same-gui", "gui-above"].iter().enumerate() {
		let cframe = Affine3A::from_translation(Vec3::new(0.0, row as f32 * 3.0, 0.0));
		let part = model.part(&PartSpec { name: (*case).into(), cframe, size, color: [0, 0, 0], ..Default::default() });
		let bright = surface_gui(&mut model, part, "Bright", &Surface { canvas: [500.0, 100.0], brightness: 15.0, ..Default::default() });
		frame(&mut model, bright, "White", udim2(0.0, 150, 0.0, 30), udim2(0.0, 200, 0.0, 40), [255, 255, 255], 0.0);
		match *case {
			"same-gui" => {
				frame(&mut model, bright, "Cover", udim2(0.0, 250, 0.0, 20), udim2(0.0, 120, 0.0, 60), [0, 0, 0], 0.5);
			}
			"gui-above" => {
				let above = surface_gui(&mut model, part, "Above", &Surface { canvas: [500.0, 100.0], z_offset: 1.0, ..Default::default() });
				frame(&mut model, above, "Cover", udim2(0.0, 250, 0.0, 20), udim2(0.0, 120, 0.0, 60), [0, 0, 0], 0.5);
			}
			_ => {}
		}
		let point = |canvas_x: f32| cframe.transform_point3(Vec3::new((0.5 - canvas_x / 500.0) * size.x, 0.0, -size.z * 0.5));
		for (label, x) in [("out-left", 138.0), ("left-half", 200.0), ("right-half", 300.0), ("out-right", 362.0)] {
			probes.push(Probe { label: format!("{case}-{label}"), position: point(x).into(), inputs: serde_json::json!({"case": case, "canvasX": x}) });
		}
	}
	let dark = Environment { ambient: Vec3::ZERO, ..sun_only(0.0) };
	let camera = CameraSpec::LookAt { eye: [0.0, 3.0, -40.0], target: [0.0, 3.0, 0.0], fov: 25.0 };
	Experiment { name: "gui-bloom-layers".into(), model, probes, runs: vec![Run { label: "ev0".into(), environment: dark, camera }] }
}


/// BillboardGui distance settings: DistanceLowerLimit, DistanceUpperLimit and DistanceStep on billboards
/// in stud and pixel sizes, seen from 12 and 40 studs.
pub fn gui_billboard_limits() -> Experiment {
	let mut model = ModelBuilder::new("GuiBillboardLimits");
	let mut runs = Vec::new();
	let cases: [(&str, Billboard); 5] = [
		("plain", Billboard::default()),
		("lower30", Billboard { distance_lower: 30.0, ..Default::default() }),
		("upper20", Billboard { distance_upper: 20.0, ..Default::default() }),
		("step15", Billboard { distance_step: 15.0, ..Default::default() }),
		("pixels-upper20", Billboard { size: (0.0, 160, 0.0, 40), distance_upper: 20.0, ..Default::default() }),
	];
	for (index, (label, billboard)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 60.0, 0.0, 0.0);
		let part = model.part(&PartSpec { name: (*label).into(), cframe: Affine3A::from_translation(center), size: Vec3::splat(0.3), color: [100, 100, 100], ..Default::default() });
		let gui = billboard_gui(&mut model, part, "Billboard", billboard);
		pattern(&mut model, gui);
		for distance in [12.0f32, 40.0] {
			runs.push(Run { label: format!("{label}-{distance}"), environment: flat(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -distance)).into(), target: center.into(), fov: 40.0 } });
		}
	}
	Experiment { name: "gui-billboard-limits".into(), model, probes: Vec::new(), runs }
}

/// Highlights and GUIs: three highlighted slabs (red fill at half transparency, white outline) with
/// the pattern on a SurfaceGui on the left one's front, on an AlwaysOnTop BillboardGui over the middle
/// one, and on a plain BillboardGui two studs in front of the right one; which draws over which.
pub fn gui_highlight() -> Experiment {
	let mut model = ModelBuilder::new("GuiHighlight");
	let slab = |model: &mut ModelBuilder, name: &str, x: f32| {
		let part = model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)), size: Vec3::new(8.0, 6.0, 1.0), color: [128, 128, 128], ..Default::default() });
		model.child(
			part,
			InstanceBuilder::new("Highlight")
				.with_property("FillColor", Color3::new(1.0, 0.0, 0.0))
				.with_property("FillTransparency", 0.5f32)
				.with_property("OutlineColor", Color3::new(1.0, 1.0, 1.0))
				.with_property("OutlineTransparency", 0.0f32)
				.with_property("DepthMode", Enum::from_u32(0)),
		);
		part
	};
	let left = slab(&mut model, "surface", -12.0);
	let gui = surface_gui(&mut model, left, "Board", &Surface::default());
	pattern(&mut model, gui);
	let middle = slab(&mut model, "ontop", 0.0);
	let gui = billboard_gui(&mut model, middle, "OnTop", &Billboard { size: (4.0, 0, 2.0, 0), always_on_top: true, ..Default::default() });
	pattern(&mut model, gui);
	let right = slab(&mut model, "front", 12.0);
	let gui = billboard_gui(&mut model, right, "Front", &Billboard { size: (4.0, 0, 2.0, 0), studs_offset: Vec3::new(0.0, 0.0, 2.0), ..Default::default() });
	pattern(&mut model, gui);
	let runs = vec![Run { label: "head".into(), environment: flat(), camera: CameraSpec::LookAt { eye: [0.0, 0.0, -30.0], target: [0.0; 3], fov: 50.0 } }];
	Experiment { name: "gui-highlight".into(), model, probes: Vec::new(), runs }
}


/// BillboardGuis: the pattern over parts at several distances, with stud and pixel sizes,
/// StudsOffset, SizeOffset and ExtentsOffset, a wall crossing a billboard (with and without
/// AlwaysOnTop), and swatches at LightInfluence 0 / 1 and Brightness 1 / 3 under the sun.
pub fn gui_billboard_basics() -> Experiment {
	let mut model = ModelBuilder::new("GuiBillboardBasics");
	let mut runs = Vec::new();
	let cube = |model: &mut ModelBuilder, name: &str, center: Vec3| model.part(&PartSpec { name: name.into(), cframe: Affine3A::from_translation(center), size: Vec3::new(2.0, 4.0, 6.0), color: [100, 100, 100], ..Default::default() });
	// sizes: studs, pixels, both; offsets
	let cases: [(&str, Billboard); 8] = [
		("studs", Billboard::default()),
		("pixels", Billboard { size: (0.0, 200, 0.0, 50), ..Default::default() }),
		("mixed", Billboard { size: (2.0, 100, 1.0, 10), ..Default::default() }),
		("studsoffset", Billboard { studs_offset: Vec3::new(1.0, 3.0, 0.0), ..Default::default() }),
		("sizeoffset", Billboard { size_offset: [0.5, 0.25], ..Default::default() }),
		("extents", Billboard { extents_offset: Vec3::new(0.0, 1.0, 0.0), ..Default::default() }),
		("extentsworld", Billboard { extents_offset_world: Vec3::new(1.0, 0.0, 0.0), ..Default::default() }),
		("worldoffset", Billboard { studs_offset_world: Vec3::new(0.0, 0.0, -3.0), ..Default::default() }),
	];
	for (index, (label, billboard)) in cases.iter().enumerate() {
		let center = Vec3::new(index as f32 * 40.0, 0.0, 0.0);
		let part = cube(&mut model, label, center);
		let gui = billboard_gui(&mut model, part, "Billboard", billboard);
		pattern(&mut model, gui);
		for distance in [12.0f32, 30.0] {
			runs.push(Run { label: format!("{label}-{distance}"), environment: flat(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(0.0, 0.0, -distance)).into(), target: center.into(), fov: 40.0 } });
		}
		if index == 0 {
			runs.push(Run { label: format!("{label}-turned"), environment: flat(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(14.0, 8.0, -18.0)).into(), target: center.into(), fov: 40.0 } });
			runs.push(Run { label: format!("{label}-offaxis"), environment: flat(), camera: CameraSpec::LookAt { eye: (center + Vec3::new(8.0, 3.0, -20.0)).into(), target: (center + Vec3::new(8.0, 3.0, 0.0)).into(), fov: 40.0 } });
		}
	}
	// a wall crossing two billboards' planes diagonally, AlwaysOnTop on the right one
	let crossing = Vec3::new(400.0, 0.0, 0.0);
	for (index, on_top) in [false, true].iter().enumerate() {
		let center = crossing + Vec3::new(index as f32 * 12.0 - 6.0, 0.0, 0.0);
		let part = cube(&mut model, &format!("cross{index}"), center);
		let gui = billboard_gui(&mut model, part, "Billboard", &Billboard { size: (8.0, 0, 4.0, 0), always_on_top: *on_top, ..Default::default() });
		pattern(&mut model, gui);
		let wall = Affine3A::from_rotation_translation(Quat::from_rotation_y(50f32.to_radians()), center + Vec3::new(0.0, -1.0, 0.0));
		model.part(&PartSpec { name: format!("wall{index}"), cframe: wall, size: Vec3::new(10.0, 1.5, 0.5), color: [200, 120, 60], ..Default::default() });
	}
	runs.push(Run { label: "crossing".into(), environment: flat(), camera: CameraSpec::LookAt { eye: (crossing + Vec3::new(0.0, 0.0, -36.0)).into(), target: crossing.into(), fov: 40.0 } });
	// lighting: swatches under the sun, the default lighting and flat light
	let lit = Vec3::new(480.0, 0.0, 0.0);
	let mut probes = Vec::new();
	for (index, (light_influence, brightness)) in [(0.0f32, 1.0f32), (1.0, 1.0), (0.0, 3.0), (0.5, 1.0)].iter().enumerate() {
		let center = lit + Vec3::new(0.0, index as f32 * 3.0 - 4.5, 0.0);
		let part = model.part(&PartSpec { name: format!("lit{index}"), cframe: Affine3A::from_translation(center), size: Vec3::splat(0.5), color: [100, 100, 100], ..Default::default() });
		let gui = billboard_gui(&mut model, part, "Billboard", &Billboard { size: (12.0, 0, 2.0, 0), light_influence: *light_influence, brightness: *brightness, ..Default::default() });
		let centres = swatches(&mut model, gui);
		// seen from -Z: the billboard's right is -X and its down -Y
		for (swatch, (color, [u, v])) in centres.iter().enumerate() {
			let position = center + Vec3::new((0.5 - u) * 12.0, (0.5 - v) * 2.0, 0.0);
			let inputs = serde_json::json!({"lightInfluence": light_influence, "brightness": brightness, "color": color, "normal": [0.0, 0.0, -1.0]});
			probes.push(Probe { label: format!("lit{index}-{swatch}"), position: position.into(), inputs });
		}
	}
	let camera = CameraSpec::LookAt { eye: (lit + Vec3::new(0.0, 0.0, -40.0)).into(), target: lit.into(), fov: 30.0 };
	runs.push(Run { label: "light-flat".into(), environment: flat(), camera: camera.clone() });
	runs.push(Run { label: "light-sun".into(), environment: sun_only(1.0), camera: camera.clone() });
	runs.push(Run { label: "light-default".into(), environment: Environment::default(), camera });
	Experiment { name: "gui-billboard-basics".into(), model, probes, runs }
}
