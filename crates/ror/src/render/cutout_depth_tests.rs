//! A cut-out (a part at Transparency 0 whose SurfaceAppearance is in AlphaMode Transparency) is drawn
//! with depth, so two of them cannot swap whatever order the see-through draws take; a part at
//! Transparency 0.02 is a see-through draw and sorts (notes/render.md, "Cut-outs and the blend order").

use super::*;
use crate::scene::build::{ModelBuilder, PartSpec};
use rbx_dom_weak::types::{Content, Vector3};

const SIZE: (u32, u32) = (320, 180);
/// The eye: 10 studs before the near card, level with the lower card's middle
const EYE: Vec3 = Vec3::new(0.0, -6.0, -10.0);
/// On the near card where both cards overlap: its opaque half (u < 0.5) and its other half
const OPAQUE_SIDE: Vec3 = Vec3::new(-4.0, -5.0, 0.0);
const OTHER_SIDE: Vec3 = Vec3::new(4.0, -5.0, 0.0);

struct Cards {
	renderer: Renderer,
	state: std::path::PathBuf,
	mesh: String,
}

impl Cards {
	fn new(name: &str) -> Self {
		let state = std::env::temp_dir().join(format!("ror-cutout-depth-{name}-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let gpu = Arc::new(Gpu::new().expect("a GPU (or the software adapter)"));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let mesh_path = state.join("card.mesh");
		std::fs::write(&mesh_path, super::vertex_colour_tests::quad_mesh([255; 4])).unwrap();
		Self { renderer: Renderer::new(gpu, assets).expect("a renderer"), mesh: Self::uri(&mesh_path), state }
	}

	fn uri(path: &std::path::Path) -> String {
		format!("file:///{}", path.display().to_string().replace('\\', "/"))
	}

	/// A colour map of one colour: its left half (u < 0.5) at alpha `left`, its right half at `right`.
	fn map(&self, color: [u8; 3], left: u8, right: u8) -> String {
		let path = self.state.join(format!("map-{}-{}-{}-{left}-{right}.png", color[0], color[1], color[2]));
		let rgba: Vec<u8> = (0..64u32 * 64).flat_map(|index| [color[0], color[1], color[2], if index % 64 < 32 { left } else { right }]).collect();
		crate::output::write_png(&path, 64, 64, &rgba, false).unwrap();
		Self::uri(&path)
	}

	/// Two upright cards 16 x 30 studs facing the eye, 4 studs apart (the near one at z 0), their middles
	/// 12 studs apart in height (the near one's the higher when `near_high`), each under a
	/// SurfaceAppearance in AlphaMode Transparency: the near one red with its right half at alpha
	/// `near_right`, the far one blue and opaque. Flat white light, no sun.
	fn scene(&self, near_high: bool, near_right: u8, transparency: f32) -> Scene {
		let lift = if near_high { 6.0 } else { -6.0 };
		let mut model = ModelBuilder::new("Cards");
		for (name, at) in [("Near", Vec3::new(0.0, lift, 0.0)), ("Far", Vec3::new(0.0, -lift, 4.0))] {
			let part = model.part(&PartSpec { class: "MeshPart", name: name.into(), cframe: Affine3A::from_translation(at), size: Vec3::new(16.0, 30.0, 0.05), material: 272, transparency, cast_shadow: false, ..Default::default() });
			model.set(part, "MeshContent", Content::from_uri(self.mesh.as_str()));
			model.set(part, "InitialSize", Vector3::new(4.0, 4.0, 0.05));
		}
		let mut scene = crate::scene::extract::extract(&model.into_document());
		let maps = [self.map([255, 0, 0], 255, near_right), self.map([0, 0, 255], 255, 255)];
		let cards = scene.parts.iter_mut().filter(|part| matches!(part.geometry, crate::scene::Geometry::MeshPart { .. }));
		for (part, map) in cards.zip(maps) {
			Arc::make_mut(part).surface_appearance = Some(crate::scene::SurfaceAppearance {
				color_map: Some(map),
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
		let environment = &mut scene.environment;
		(environment.ambient, environment.outdoor_ambient, environment.brightness) = (Vec3::ONE, Vec3::ONE, 0.0);
		(environment.environment_diffuse_scale, environment.environment_specular_scale) = (0.0, 0.0);
		scene
	}

	/// The scene from [`EYE`] looking `pitch` degrees up: the mean colour of the 3 x 3 pixels at each point.
	fn read(&self, scene: &Scene, pitch: f32, samples: u32, points: &[Vec3]) -> Vec<[f32; 3]> {
		let forward = Self::forward(pitch);
		let spec = CameraSpec::LookAt { eye: EYE.into(), target: (EYE + forward * 10.0).into(), fov: 70.0 };
		let view = ViewRequest { width: SIZE.0, height: SIZE.1, camera: spec.clone(), background: Background::Color([0.0, 0.0, 0.0]), bloom: false, msaa: samples, ..Default::default() };
		let (images, stats) = self.renderer.render(scene, &[view]).unwrap();
		assert!(stats.missing.iter().all(|reason| reason.contains("rbxasset://")), "missing {:?}", stats.missing);
		let project = spec.place(None, SIZE.0, SIZE.1).view_projection(SIZE.0 as f32 / SIZE.1 as f32);
		points
			.iter()
			.map(|point| {
				let clip = project * point.extend(1.0);
				let (column, row) = (((clip.x / clip.w * 0.5 + 0.5) * SIZE.0 as f32) as i32, ((0.5 - clip.y / clip.w * 0.5) * SIZE.1 as f32) as i32);
				assert!(column > 0 && row > 0 && column < SIZE.0 as i32 - 1 && row < SIZE.1 as i32 - 1, "{point:?} is off the image at pitch {pitch}");
				let mut sum = [0.0f32; 3];
				for (dx, dy) in (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))) {
					let pixel = &images[0].rgba[(((row + dy) as u32 * SIZE.0 + (column + dx) as u32) * 4) as usize..][..3];
					(0..3).for_each(|channel| sum[channel] += pixel[channel] as f32 / 9.0);
				}
				sum
			})
			.collect()
	}

	fn forward(pitch: f32) -> Vec3 {
		Vec3::new(0.0, pitch.to_radians().sin(), pitch.to_radians().cos())
	}

	/// Whether the near card comes first in a back-to-front order of the two cards' middles from this
	/// eye: (by distance from the eye, by depth along the camera's forward axis).
	fn near_first(near_high: bool, pitch: f32) -> (bool, bool) {
		let lift = if near_high { 6.0 } else { -6.0 };
		let (near, far) = (Vec3::new(0.0, lift, 0.0) - EYE, Vec3::new(0.0, -lift, 4.0) - EYE);
		(near.length() > far.length(), near.dot(Self::forward(pitch)) > far.dot(Self::forward(pitch)))
	}
}

impl Drop for Cards {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.state);
	}
}

fn red(color: [f32; 3]) -> bool {
	color[0] > 80.0 && color[2] < color[0] / 3.0
}

fn blue(color: [f32; 3]) -> bool {
	color[2] > 80.0 && color[0] < color[2] / 3.0
}

/// The near card's opaque texels cover the far card's and its clear texels show it, from eyes looking
/// 20 degrees up, level and 20 degrees down, with either card's middle the higher: by either order key
/// (distance from the eye, depth along the look) the near card comes first in some of the six and last
/// in the others, and none of the twelve pictures (4 samples and 1) follows that order.
#[test]
fn two_cutout_cards_keep_their_depth_whatever_the_blend_order() {
	let cards = Cards::new("order");
	let mut orders = Vec::new();
	for near_high in [true, false] {
		let scene = cards.scene(near_high, 0, 0.0);
		for pitch in [20.0, 0.0, -20.0] {
			orders.push(Cards::near_first(near_high, pitch));
			for samples in [4, 1] {
				let read = cards.read(&scene, pitch, samples, &[OPAQUE_SIDE, OTHER_SIDE]);
				assert!(red(read[0]), "near card's middle {}, looking {pitch} degrees up, {samples} samples: the near card's opaque texels read {:?}, its red expected", if near_high { "higher" } else { "lower" }, read[0]);
				assert!(blue(read[1]), "near card's middle {}, looking {pitch} degrees up, {samples} samples: through the near card's clear texels {:?}, the far card's blue expected", if near_high { "higher" } else { "lower" }, read[1]);
			}
		}
	}
	// the six cases hold both orders under each key, so neither key can make the test pass by itself
	for (key, by) in [("distance", orders.iter().map(|order| order.0).collect::<Vec<_>>()), ("depth along the look", orders.iter().map(|order| order.1).collect())] {
		assert!(by.contains(&true) && by.contains(&false), "by {key} the near card is first in {by:?}: both orders expected among the six cases");
	}
}

/// A soft texel (alpha 64 of 255) of a cut-out blends over what is behind it: a quarter of the near
/// card's red over three quarters of the far card's blue, by coverage at 4 samples (1 of the 4) and by
/// blending at 1 (both in the target's square-root space: 0.25 squared of the red, 0.75 squared of the blue).
#[test]
fn a_cutouts_soft_texel_blends_over_what_is_behind_it() {
	let cards = Cards::new("soft");
	for samples in [4, 1] {
		let alone = cards.read(&cards.scene(false, 0, 0.0), 0.0, samples, &[OPAQUE_SIDE, OTHER_SIDE]);
		assert!(red(alone[0]) && blue(alone[1]), "{samples} samples: the near card's red {:?} and the far card's blue {:?}", alone[0], alone[1]);
		let soft = cards.read(&cards.scene(false, 64, 0.0), 0.0, samples, &[OTHER_SIDE])[0];
		let (share_red, share_blue) = (soft[0] / alone[0][0], soft[2] / alone[1][2]);
		assert!((0.12..0.6).contains(&share_red) && (0.5..0.97).contains(&share_blue), "{samples} samples: a texel of alpha 64 reads {soft:?}: {share_red} of the near card's red and {share_blue} of the far card's blue, about 0.28 and 0.77 expected");
	}
}

/// At Transparency 0.02 the same cards are see-through draws: they sort whole, by one point each, and
/// write no depth, so where the order puts the near card first the far card is drawn over it (looking
/// 20 degrees up with the near card's middle the higher, both keys put it first). At Transparency 0
/// the same view shows the near card.
#[test]
fn a_card_at_transparency_0_02_sorts_where_a_cutout_keeps_its_depth() {
	let cards = Cards::new("sorted");
	assert_eq!(Cards::near_first(true, 20.0), (true, true));
	for samples in [4, 1] {
		let cutout = cards.read(&cards.scene(true, 255, 0.0), 20.0, samples, &[OPAQUE_SIDE])[0];
		assert!(red(cutout), "{samples} samples, Transparency 0: {cutout:?}, the near card's red expected");
		let sorted = cards.read(&cards.scene(true, 255, 0.02), 20.0, samples, &[OPAQUE_SIDE])[0];
		assert!(blue(sorted), "{samples} samples, Transparency 0.02: {sorted:?}, the far card's blue drawn over the near card expected");
	}
}
