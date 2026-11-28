//! A MeshPart's or SpecialMesh's own texture: its alpha is the part's coverage only past
//! `scene::OPAQUE_TRANSPARENCY`, in its colour and its shadow alike, and a MeshPart showing an EditableMesh
//! casts no shadow (fixtures/texture-alpha.luau in a Roblox 0.747 client; docs/materials.md "Texture alpha on
//! MeshParts and SpecialMeshes").

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};
use rbx_dom_weak::InstanceBuilder;
use rbx_dom_weak::types::{Content, ContentId, Enum, Vector3};

struct Kit {
	renderer: Renderer,
	state: std::path::PathBuf,
	/// a 4 x 4 stud quad, both faces (vertex_colour_tests::quad_mesh)
	mesh: String,
	/// the same quad as the engine emulator writes an EditableMesh's snapshot
	editable: String,
	/// the quad with every vertex at alpha 0
	clear: String,
}

/// How the card draws its texture.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
	MeshPart,
	SpecialMesh,
	/// a MeshPart of a mesh whose vertices are at alpha 0
	Clear,
}

impl Kit {
	fn new(name: &str) -> Self {
		let state = std::env::temp_dir().join(format!("ror-texture-alpha-{name}-{}", std::process::id()));
		std::fs::create_dir_all(state.join("editable-meshes")).unwrap();
		let gpu = crate::render::test_gpu();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let bytes = super::vertex_colour_tests::quad_mesh([255; 4]);
		let mesh_path = state.join("card.mesh");
		std::fs::write(&mesh_path, &bytes).unwrap();
		let editable_path = state.join("editable-meshes").join("0123456789abcdef-232.mesh");
		std::fs::write(&editable_path, &bytes).unwrap();
		let clear_path = state.join("clear.mesh");
		std::fs::write(&clear_path, super::vertex_colour_tests::quad_mesh([255, 255, 255, 0])).unwrap();
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), mesh: Self::uri(&mesh_path), editable: Self::uri(&editable_path), clear: Self::uri(&clear_path), state }
	}

	fn uri(path: &std::path::Path) -> String {
		format!("file:///{}", path.display().to_string().replace('\\', "/"))
	}

	/// A 64 x 64 image of (204, 153, 102): its left half (u < 0.5) at alpha `left`, its right half at `right`.
	fn map(&self, left: u8, right: u8) -> String {
		let path = self.state.join(format!("tan-{left}-{right}.png"));
		let rgba: Vec<u8> = (0..64u32 * 64).flat_map(|index| [204, 153, 102, if index % 64 < 32 { left } else { right }]).collect();
		crate::output::write_png(&path, 64, 64, &rgba, false).unwrap();
		Self::uri(&path)
	}

	/// A red 4 x 4 card at `cframe` drawing the quad as `kind` with `texture` (or the EditableMesh snapshot's
	/// quad, untextured, when `editable`; the quad at vertex alpha 0 for `Kind::Clear`), at `transparency`.
	fn card(&self, model: &mut ModelBuilder, kind: Kind, texture: Option<&str>, transparency: f32, cframe: Affine3A, editable: bool) {
		let color = [220, 40, 40];
		match kind {
			Kind::MeshPart | Kind::Clear => {
				let part = model.part(&PartSpec { class: "MeshPart", name: "Card".into(), cframe, size: Vec3::new(4.0, 4.0, 0.05), color, material: 272, transparency, ..Default::default() });
				let mesh = if editable { &self.editable } else if kind == Kind::Clear { &self.clear } else { &self.mesh };
				model.set(part, "MeshContent", Content::from_uri(mesh.as_str()));
				model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
				if let Some(texture) = texture {
					model.set(part, "TextureContent", Content::from_uri(texture));
				}
			}
			Kind::SpecialMesh => {
				let part = model.part(&PartSpec { name: "Card".into(), cframe, size: Vec3::new(4.0, 4.0, 0.05), color, material: 272, transparency, ..Default::default() });
				let mut mesh = InstanceBuilder::new("SpecialMesh").with_property("MeshType", Enum::from_u32(5)).with_property("MeshId", ContentId::from(self.mesh.as_str()));
				if let Some(texture) = texture {
					mesh = mesh.with_property("TextureId", ContentId::from(texture));
				}
				model.child(part, mesh);
			}
		}
	}

	fn render(&self, scene: &Scene, request: &ViewRequest) -> Vec<f32> {
		let (images, stats) = self.renderer.render(scene, std::slice::from_ref(request)).unwrap();
		assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "missing {:?}", stats.missing);
		images[0].rgba.chunks(4).flat_map(|pixel| pixel[..3].iter().map(|&channel| channel as f32).collect::<Vec<_>>()).collect()
	}
}

impl Drop for Kit {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

/// Up to Transparency 0.01 a MeshPart's or SpecialMesh's texture draws its stored colour under any alpha,
/// alpha 0 included, and the part's Color nowhere; past it the part's opacity is (1 - Transparency) x alpha
/// (fixtures/texture-alpha.luau in a Roblox 0.747 client: a tan image at alpha 0 on a red part reads the
/// opaque image's 147 / 104 / 56 at Transparency 0 and 0.01 and the green wall behind it at 0.02 and 0.5;
/// at alpha 128 and 0.5 as alpha 255 at 0.75). ror revealed a MeshPart's Color under alpha (red at alpha 0)
/// and drew a SpecialMesh's texture alpha at any Transparency (the wall at alpha 0).
#[test]
fn a_parts_own_texture_alpha_is_coverage_only_past_opaque_transparency() {
	let kit = Kit::new("colour");
	let size = 32u32;
	let request = ViewRequest { width: size, height: size, camera: CameraSpec::LookAt { eye: [0.0, 0.0, -10.0], target: [0.0; 3], fov: 20.0 }, background: Background::Color([0.0; 3]), bloom: false, ..Default::default() };
	let draw = |kind: Kind, texture: Option<&str>, transparency: f32| -> [f32; 3] {
		let mut model = ModelBuilder::new("Model");
		if let Some(texture) = texture {
			kit.card(&mut model, kind, Some(texture), transparency, Affine3A::IDENTITY, false);
		}
		model.part(&PartSpec { name: "Wall".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, 0.0, 3.0)), size: Vec3::new(20.0, 20.0, 1.0), color: [40, 150, 40], material: 272, cast_shadow: false, ..Default::default() });
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let environment = &mut scene.environment;
		(environment.brightness, environment.ambient, environment.outdoor_ambient) = (0.0, Vec3::splat(0.7), Vec3::ZERO);
		(environment.environment_diffuse_scale, environment.environment_specular_scale, environment.global_shadows) = (0.0, 0.0, false);
		let pixels = kit.render(&scene, &request);
		let index = ((size / 2 * size + size / 2) * 3) as usize;
		[pixels[index], pixels[index + 1], pixels[index + 2]]
	};
	let same = |label: &str, got: [f32; 3], want: [f32; 3]| assert!(got.iter().zip(want).all(|(got, want)| (got - want).abs() <= 2.0), "{label}: {got:?}, expected {want:?}");
	let wall = draw(Kind::MeshPart, None, 0.0);
	let (zero, half, opaque) = (kit.map(0, 0), kit.map(128, 128), kit.map(255, 255));
	for kind in [Kind::MeshPart, Kind::SpecialMesh] {
		let name = if kind == Kind::MeshPart { "MeshPart" } else { "SpecialMesh" };
		let solid = draw(kind, Some(&opaque), 0.0);
		assert!((solid[0] - wall[0]).abs() > 30.0, "{name}: the opaque texture drawn {solid:?} over the wall {wall:?}");
		for transparency in [0.0, 0.01] {
			same(&format!("{name}, alpha 0 at Transparency {transparency}"), draw(kind, Some(&zero), transparency), solid);
			same(&format!("{name}, alpha 128 at Transparency {transparency}"), draw(kind, Some(&half), transparency), solid);
		}
		for transparency in [0.02, 0.5] {
			same(&format!("{name}, alpha 0 at Transparency {transparency}"), draw(kind, Some(&zero), transparency), wall);
		}
		same(&format!("{name}, alpha 128 at Transparency 0.5"), draw(kind, Some(&half), 0.5), draw(kind, Some(&opaque), 0.75));
	}
}

/// The sun's floor under a card 6 studs up (from between the card and its shadow), or a shadowed PointLight's
/// 4 studs over the card; the levels of a view of that floor.
fn shadow_scene(kit: &Kit, card: Option<(Kind, Option<&str>, f32, bool)>, lamp: bool) -> (Scene, ViewRequest) {
	let height = 6.0;
	let mut model = ModelBuilder::new("Cards");
	model.part(&PartSpec { name: "Floor".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, -0.5, 0.0)), size: Vec3::new(200.0, 1.0, 200.0), color: [255; 3], material: 272, ..Default::default() });
	if let Some((kind, texture, transparency, editable)) = card {
		let cframe = Affine3A::from_translation(Vec3::new(0.0, height, 0.0)) * Affine3A::from_rotation_x(-std::f32::consts::FRAC_PI_2);
		kit.card(&mut model, kind, texture, transparency, cframe, editable);
	}
	if lamp {
		let carrier = model.part(&PartSpec { name: "Lamp".into(), cframe: Affine3A::from_translation(Vec3::new(0.0, height + 4.0, 0.0)), size: Vec3::splat(0.2), transparency: 1.0, cast_shadow: false, ..Default::default() });
		model.child(carrier, InstanceBuilder::new("PointLight").with_property("Shadows", true).with_property("Range", 30.0f32).with_property("Brightness", 3.0f32));
	}
	let mut scene = crate::scene::extract::extract(&model.into_document());
	let environment = &mut scene.environment;
	(environment.ambient, environment.outdoor_ambient, environment.environment_diffuse_scale, environment.environment_specular_scale) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0);
	(environment.clock_time, environment.geographic_latitude) = (12.0, 30.0);
	if lamp {
		environment.brightness = 0.0;
	}
	let sun = environment::sun_direction(12.0, 30.0);
	let target = if lamp { Vec3::ZERO } else { -Vec3::new(sun.x, 0.0, sun.z) * (height / sun.y) };
	let request = ViewRequest { width: 96, height: 96, camera: CameraSpec::LookAt { eye: (target + Vec3::new(0.0, 2.5, -0.5)).into(), target: target.into(), fov: 110.0 }, background: Background::Color([0.0, 0.0, 1.0]), bloom: false, ..Default::default() };
	(scene, request)
}

/// Of the floor an opaque card shades, the shares a case leaves dark (over 0.8 of that card's shadow) and lit
/// (under 0.2).
fn split(none: &[f32], opaque: &[f32], case: &[f32]) -> (f32, f32) {
	let level = |pixels: &[f32], index: usize| (pixels[index * 3] + pixels[index * 3 + 1] + pixels[index * 3 + 2]) / 3.0;
	let footprint: Vec<usize> = (0..none.len() / 3).filter(|&index| level(none, index) - level(opaque, index) > 30.0).collect();
	assert!(footprint.len() > 400, "the opaque card's shadow covers {} pixels", footprint.len());
	let share = |index: usize| (level(none, index) - level(case, index)) / (level(none, index) - level(opaque, index));
	let dark = footprint.iter().filter(|&&index| share(index) > 0.8).count() as f32 / footprint.len() as f32;
	let lit = footprint.iter().filter(|&&index| share(index) < 0.2).count() as f32 / footprint.len() as f32;
	(dark, lit)
}

/// Up to Transparency 0.01 a MeshPart's or SpecialMesh's textured card casts its whole shadow whatever its
/// texture's alpha; past it the texture's alpha times 1 - Transparency cuts it at 0.252, as a
/// SurfaceAppearance's in AlphaMode Transparency does (fixtures/texture-alpha.luau in a Roblox 0.747 client,
/// from the sun and a shadowed PointLight: alpha 0 casts the full shadow at Transparency 0, none at 0.3; a card
/// opaque on one half at 0.3 shades the floor under that half; a MeshPart's vertex alpha cuts it alike). A
/// MeshPart showing an EditableMesh casts none. ror cast no sun shadow from a SpecialMesh whose texture has
/// alpha, and a MeshPart's full shadow at any texture or vertex alpha.
#[test]
fn a_parts_own_texture_cuts_its_shadow_past_opaque_transparency_and_editable_meshes_cast_none() {
	let kit = Kit::new("shadow");
	let (zero, halves) = (kit.map(0, 0), kit.map(255, 0));
	for lamp in [false, true] {
		let by = if lamp { "a PointLight" } else { "the sun" };
		let draw = |card: Option<(Kind, Option<&str>, f32, bool)>| {
			let (scene, request) = shadow_scene(&kit, card, lamp);
			kit.render(&scene, &request)
		};
		let none = draw(None);
		let opaque = draw(Some((Kind::MeshPart, None, 0.0, false)));
		for kind in [Kind::MeshPart, Kind::SpecialMesh] {
			let name = if kind == Kind::MeshPart { "MeshPart" } else { "SpecialMesh" };
			let (dark, _) = split(&none, &opaque, &draw(Some((kind, Some(&zero), 0.0, false))));
			assert!(dark > 0.95, "{by}, {name} at alpha 0, Transparency 0: {dark} of the card's shadow dark; all of it expected");
			let (_, lit) = split(&none, &opaque, &draw(Some((kind, Some(&zero), 0.3, false))));
			assert!(lit > 0.95, "{by}, {name} at alpha 0, Transparency 0.3: {lit} of the card's shadow lit; all of it expected");
			let (dark, lit) = split(&none, &opaque, &draw(Some((kind, Some(&halves), 0.3, false))));
			assert!((0.35..0.65).contains(&dark) && (0.35..0.65).contains(&lit), "{by}, {name} at alpha 255 | 0, Transparency 0.3: {dark} dark, {lit} lit; half of it each expected");
		}
		let (_, lit) = split(&none, &opaque, &draw(Some((Kind::MeshPart, None, 0.0, true))));
		assert!(lit > 0.95, "{by}, an EditableMesh's MeshPart: {lit} of the card's shadow lit; all of it expected");
		// a MeshPart's vertex alpha the same way: whole up to 0.01, cut past it
		let (dark, _) = split(&none, &opaque, &draw(Some((Kind::Clear, None, 0.0, false))));
		assert!(dark > 0.95, "{by}, vertex alpha 0 at Transparency 0: {dark} of the card's shadow dark; all of it expected");
		let (_, lit) = split(&none, &opaque, &draw(Some((Kind::Clear, None, 0.3, false))));
		assert!(lit > 0.95, "{by}, vertex alpha 0 at Transparency 0.3: {lit} of the card's shadow lit; all of it expected");
	}
}
