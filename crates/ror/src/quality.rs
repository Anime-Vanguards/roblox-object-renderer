//! What a Roblox client draws at each graphics quality level, 1 to 21 (notes/render.md "Graphics quality
//! levels"): measured in a rendered Roblox client (0.747, desktop 1600 x 900, one level pinned a
//! session) with `fixtures/quality-levels.luau`, under `LightingStyle` Realistic or Soft and
//! `PrioritizeLightingQuality` true or false.
//!
//! A view asks for a level with `ViewRequest::quality` (`--quality`); without one it draws as level 21,
//! everything at its best and nothing culled. [`Quality`] holds every switch the level moves. The renderer
//! follows the sun's shadow maps' reach and the voxel sun past it, local lights' per-pixel reach, the shading's
//! tiers (per pixel round the focus, high quality round the camera), the built-in glow, BloomEffect, ambient
//! occlusion, anti-aliasing, SunRays and DepthOfField, Glass refraction, the view distance and size cull
//! (`crate::lod`) and the particles' Rate; the clouds' tiers are kept here, measured, for when they are followed
//! (its field says so).
//!
//! Studio's edit quality is not the client's run-time level: `ror cost` and `ror cost --check` keep Studio's
//! edit table (`crate::lod::Lod::view_distance`, notes/cost.md "Quality").

use serde::Serialize;

/// The highest level, what every picture draws without a level.
pub const TOP: u32 = 21;

/// Where the sun's shadow maps reach from the camera: full to `full`, fading linearly to nothing at `gone`
/// (studs, by a point's distance from the camera, not its depth); past it the light grid's voxel sun.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SunReach {
	pub full: f32,
	pub gone: f32,
}

/// What a graphics quality level draws.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
	pub level: u32,
	/// Whether any part is lit per pixel, with the direct specular: those within `lit_radius` of the focus (from
	/// level 5; 7 under PrioritizeLightingQuality false). Below, every part takes the lowest shading.
	pub specular: bool,
	/// The sun's shadow maps (from level 8; 14 under PrioritizeLightingQuality false), `None` below:
	/// the voxel sun everywhere.
	pub sun_shadows: Option<SunReach>,
	/// Local lights per pixel, with their shadows, within this distance of the camera; through the light
	/// grid's voxels past it and everywhere at 0 (below 14, 17 under PrioritizeLightingQuality false, every
	/// level under Soft). Infinite: as level 21 (ror's own handover at 580-600 studs).
	pub light_reach: f32,
	/// The high-quality shading's radius from the camera (metals' and Foil's reflections, the sky's specular
	/// on SmoothPlastic, textures' detail), halfway through its blend of a quarter of it each way: 0 none,
	/// infinite everywhere. Past it, within `lit_radius` of the focus, the low-quality shading: per pixel with the
	/// direct specular, without the sky's specular, normal maps or metalness.
	pub shading_radius: f32,
	/// How far a part's box may be from the view's focus (Camera.Focus) and still be lit per pixel, with the
	/// direct specular (the low-quality shading, and the high-quality one within `shading_radius`): none at
	/// levels 1-4, 10 studs at 5, 36 at 6, 70 at 7 ... 581 at 14, 1,180 at 21; past it the lowest shading (lit by
	/// vertex: no specular per pixel, the sky's light the same from every side).
	pub lit_radius: f32,
	/// The built-in glow round Neon: 0 at levels 1-5, 1 at 6-11, 2 from 12 (render/bloom.rs GLOW_TIERS).
	pub glow: u8,
	/// BloomEffect (from 12).
	pub bloom_effect: bool,
	/// Ambient occlusion (from 20).
	pub ambient_occlusion: bool,
	/// Anti-aliasing samples: 1 at levels 1-16, 2 at 17, 4 from 18 (17 draws 4: render::view_samples).
	pub msaa: u32,
	/// Glass refracts what is behind it (from 16; plain blending below; mesh.wgsl `fs_glass`).
	pub glass_refraction: bool,
	/// SunRaysEffect and DepthOfFieldEffect (from 16).
	pub sun_rays_depth_of_field: bool,
	/// Terrain Clouds' look: 0 at levels 1-7, 1 at 8-10, 2 at 11-13, 3 from 14. Not followed yet.
	pub clouds: u8,
	/// A part farther from the camera (to its world box) is not drawn.
	pub view_distance: f32,
	/// A part whose Size's length · 3,780 is under its centre's distance · tan(fov / 2) times this is not
	/// drawn.
	pub size_cull: f32,
	/// The share of what an emitter's Rate emits that it makes (`Emit` bursts in full).
	pub particle_rate: f32,
}

/// The sun's shadow maps under PrioritizeLightingQuality true (the `coverage` station: rails 6 studs up
/// running away from the camera, three cameras looking 10, 45 and 0 degrees down putting each strip's end
/// at the same distance): full to, gone by (studs) at levels 8-18; 587.5 with no fade from 19, as 21
/// (render::SUN_SHADOWS). "Gone by" follows 2.5 L² - 15.5 L + 3 within 6 studs; level 8's is that formula
/// (no strip is that near: a column 10 studs off casts its shadow at 8 and none at 7), 18's full-to
/// unmeasured (its strip's last 15 studs taken as the fade, the levels below fading over 12-17; 9's over 7).
const SUN_REACH: [(f32, f32); 11] = [
	(26.0, 40.0),
	(59.0, 66.0),
	(86.0, 98.0),
	(122.0, 135.0),
	(163.0, 177.0),
	(208.0, 224.0),
	(260.0, 276.0),
	(316.0, 331.0),
	(376.0, 391.0),
	(439.0, 456.0),
	(520.0, 535.0),
];

/// Under PrioritizeLightingQuality false the maps start at 14: gone by 84, 136, 208, ~308 and ~440 studs at
/// 14-18 (the fade not measured: 14 studs taken, as true's), 587.5 from 19.
const SUN_REACH_UNPRIORITIZED: [f32; 5] = [84.0, 136.0, 208.0, 308.0, 440.0];

/// Level 21's reach (render::SUN_SHADOWS: the client's shadow stops 587.5 studs from the camera).
const SUN_REACH_TOP: f32 = crate::render::SUN_SHADOWS.reach;

/// The view distance by level (the `far` and `farfine` stations: pillars at eye height whose world box's
/// nearest point is the distance named), between the last drawn and the first not: 490 / 500 at 1, 760 / 800,
/// 1,160 / 1,200, 1,590 / 1,610 at 4-12, 2,200 / 2,230 at 13, 3,000 / 3,100, 3,850 / 4,000, 5,700 / 6,000 and
/// 10,000 / 10,400 at 14-17; 19,000 drawn from 18 (nothing farther measured: Studio's 100,000 taken). Under
/// PrioritizeLightingQuality false the same.
fn view_distance(level: u32) -> f32 {
	match level {
		0 | 1 => 495.0,
		2 => 780.0,
		3 => 1180.0,
		4..=12 => 1600.0,
		13 => 2215.0,
		14 => 3050.0,
		15 => 3925.0,
		16 => 5850.0,
		17 => 10200.0,
		_ => 100_000.0,
	}
}

/// The size cull by level (the `tiny` station: cubes 0.05-0.6 studs 120 studs off at a field of view of 70),
/// halfway between the last cube drawn and the first culled in Size length · 3,780 / (distance · tan(fov / 2)):
/// none of the cubes at 1-3 (taken a step above 4-5's), the 0.6 at 4-5, 0.54 at 6-7, 0.48 at 8-9, 0.43 at
/// 10-11, 0.38, 0.34, 0.30, 0.27, 0.24, 0.21, 0.18, 0.15 and 0.13 at 12-20, 0.09 at 21 (above 13 the smallest
/// are under 2 pixels: the culled one may only miss every pixel, so those are at most these). About 1.75
/// times Studio's 28 - level through 13.
fn size_cull(level: u32) -> f32 {
	match level {
		0..=3 => 50.0,
		4 | 5 => 44.4,
		6 | 7 => 39.7,
		8 | 9 => 35.4,
		10 | 11 => 31.5,
		12 => 28.0,
		13 => 24.9,
		14 => 22.2,
		15 => 19.8,
		16 => 17.5,
		17 => 15.2,
		18 => 12.9,
		19 => 10.9,
		20 => 9.3,
		_ => 6.2,
	}
}

/// Per-pixel local lights' reach under PrioritizeLightingQuality true (`lightline`: 22 shadowed lights 20
/// studs apart along a rail, ±10 studs): ~55 at 14, 104, 175, 275, 410 at 15-18; ror's 580-600 from 19
/// (render::local_light_far starts the handover to the voxels 5 studs short of it).
const LIGHT_REACH: [f32; 5] = [55.0, 104.0, 175.0, 275.0, 410.0];
/// Under false: ~93 at 17, ~170 at 18, ~284 from 19.
const LIGHT_REACH_UNPRIORITIZED: [f32; 3] = [93.0, 170.0, 284.0];

/// The high-quality shading's radius under PrioritizeLightingQuality true (`hqline`: Metal and Foil strips
/// 800 studs long): 55 studs at 7 to 756 at 17, past the strip from 18.
const SHADING_RADIUS: [f32; 11] = [55.0, 94.0, 142.0, 191.0, 246.0, 310.0, 382.0, 449.0, 547.0, 657.0, 756.0];
/// Under false, from 12: ~73, 126, 186 at 12-14, 15 not measured (between 14's and 16's), 367, 525, 755 at 16-18.
const SHADING_RADIUS_UNPRIORITIZED: [f32; 7] = [73.0, 126.0, 186.0, 276.0, 367.0, 525.0, 755.0];

/// The per-pixel lighting's radius around the focus by level (the `shading` station under the sun alone with
/// Camera.Focus moved along the view and up to 3,000 studs behind it: halfway between the farthest part's box lit per
/// pixel and the nearest lit by vertex): none at 1-4; 10.4 studs at 5 (9.6 / 11.2), 36 at 6 (34.1 / 38.0), 70 at 7
/// (67.6 / 72.1), 122 at 8 (117 / 127), 177 at 9 (167 / 187), 257 at 10 (237 / 277), 307 at 11 (287 / 327), 382 at 12
/// (337 / 427), 482 at 13 (437 / 527), 581 at 14 (537 / 625), 676 at 15 (625 / 727), 775 at 16 (724 / 827), 1,075 at 18
/// (1,022 / 1,127) and 1,180 at 21 (1,137 / 1,222): about 11.5 (level - 4)^1.7 through 18, levelling off by 21. 17,
/// 19 and 20 between their neighbours (not measured). Under PrioritizeLightingQuality false two levels later (the
/// floor 13 studs from the focus by vertex at 6 and 7, per pixel at 12; the specular's start moves from 5 to 7 alike).
const LIT_RADIUS: [f32; 17] = [10.4, 36.0, 70.0, 122.0, 177.0, 257.0, 307.0, 382.0, 482.0, 581.0, 676.0, 775.0, 925.0, 1075.0, 1110.0, 1145.0, 1180.0];

fn lit_radius(level: u32) -> f32 {
	match level {
		0..=4 => 0.0,
		_ => LIT_RADIUS[(level.min(TOP) - 5) as usize],
	}
}

impl Quality {
	/// What `level` (1-21; 0 taken as 1, above 21 as 21) draws under a LightingStyle (`soft`) and
	/// PrioritizeLightingQuality (`prioritize`).
	pub fn new(level: u32, soft: bool, prioritize: bool) -> Self {
		let level = level.clamp(1, TOP);
		let sun_shadows = if prioritize {
			match level {
				0..=7 => None,
				8..=18 => {
					let (full, gone) = SUN_REACH[(level - 8) as usize];
					Some(SunReach { full, gone })
				}
				_ => Some(SunReach { full: SUN_REACH_TOP, gone: SUN_REACH_TOP }),
			}
		} else {
			match level {
				0..=13 => None,
				14..=18 => {
					let gone = SUN_REACH_UNPRIORITIZED[(level - 14) as usize];
					Some(SunReach { full: gone - 14.0, gone })
				}
				_ => Some(SunReach { full: SUN_REACH_TOP, gone: SUN_REACH_TOP }),
			}
		};
		let light_reach = match (soft, prioritize, level) {
			(true, ..) => 0.0,
			(false, true, 0..=13) | (false, false, 0..=16) => 0.0,
			(false, true, 14..=18) => LIGHT_REACH[(level - 14) as usize],
			(false, true, _) => f32::INFINITY,
			(false, false, 17 | 18) => LIGHT_REACH_UNPRIORITIZED[(level - 17) as usize],
			(false, false, _) => LIGHT_REACH_UNPRIORITIZED[2],
		};
		let shading_radius = match (prioritize, level) {
			(true, 0..=6) | (false, 0..=11) => 0.0,
			(true, 7..=17) => SHADING_RADIUS[(level - 7) as usize],
			(false, 12..=18) => SHADING_RADIUS_UNPRIORITIZED[(level - 12) as usize],
			_ => f32::INFINITY,
		};
		Self {
			level,
			specular: level >= if prioritize { 5 } else { 7 },
			sun_shadows,
			light_reach,
			shading_radius,
			lit_radius: lit_radius(if prioritize { level } else { level.saturating_sub(2) }),
			glow: match level {
				0..=5 => 0,
				6..=11 => 1,
				_ => 2,
			},
			bloom_effect: level >= 12,
			ambient_occlusion: level >= 20,
			msaa: match level {
				0..=16 => 1,
				17 => 2,
				_ => 4,
			},
			glass_refraction: level >= 16,
			sun_rays_depth_of_field: level >= 16,
			clouds: match level {
				0..=7 => 0,
				8..=10 => 1,
				11..=13 => 2,
				_ => 3,
			},
			view_distance: view_distance(level),
			size_cull: size_cull(level),
			particle_rate: level.max(2) as f32 / TOP as f32,
		}
	}

	/// Level 21 under Realistic and PrioritizeLightingQuality true: what every view draws without a level.
	pub fn top() -> Self {
		Self::new(TOP, false, true)
	}

	/// What a scene's Lighting (its LightingStyle and PrioritizeLightingQuality) draws at `level`.
	pub fn for_environment(level: u32, environment: &crate::scene::Environment) -> Self {
		Self::new(level, environment.lighting_style == crate::scene::environment::LIGHTING_STYLE_SOFT, environment.prioritize_lighting_quality)
	}

	/// Whether a part with this box, centre and Size length is drawn from `eye` under a vertical field of
	/// view of `fov` degrees (the view distance and the size cull).
	pub fn drawn(&self, eye: glam::Vec3, fov: f32, (min, max): (glam::Vec3, glam::Vec3), center: glam::Vec3, diameter: f32) -> bool {
		if crate::lod::box_distance(eye, min, max) > self.view_distance {
			return false;
		}
		let distance = center.distance(eye) * (fov.to_radians() * 0.5).tan();
		distance <= 0.0 || diameter * 3780.0 >= distance * self.size_cull
	}
}

impl Default for Quality {
	fn default() -> Self {
		Self::top()
	}
}

#[cfg(test)]
mod tests {
	use glam::{Affine3A, Vec3};

	use super::*;

	/// The `tiny` station in the client: cubes 0.05-0.6 studs side by side 120 studs out (x -16 to 16), the
	/// camera at their height, fields of view 70 and 8. The smallest drawn per level, and the next one
	/// culled through 13 (above, the culled ones are under 2 pixels and may only have missed every pixel).
	#[test]
	fn the_size_cull_leaves_out_the_cubes_the_client_did() {
		let sizes = [0.05, 0.07, 0.09, 0.11, 0.13, 0.15, 0.18, 0.21, 0.24, 0.27, 0.3, 0.34, 0.38, 0.43, 0.48, 0.54, 0.6];
		let eye = Vec3::new(0.0, 50.0, 0.0);
		let drawn = |quality: &Quality, index: usize, fov: f32| {
			let size = Vec3::splat(sizes[index]);
			let position = Vec3::new(-16.0 + 32.0 * index as f32 / (sizes.len() - 1) as f32, 50.0, -120.0);
			let bounds = crate::lod::world_box(&Affine3A::from_translation(position), size);
			quality.drawn(eye, fov, bounds, position, size.length())
		};
		let smallest = [(3, None), (4, Some(0.6)), (5, Some(0.6)), (6, Some(0.54)), (7, Some(0.54)), (8, Some(0.48)), (9, Some(0.48)), (10, Some(0.43)), (11, Some(0.43)), (12, Some(0.38)), (13, Some(0.34)), (14, Some(0.3)), (15, Some(0.27)), (16, Some(0.24)), (17, Some(0.21)), (18, Some(0.18)), (19, Some(0.15)), (20, Some(0.13)), (21, Some(0.09))];
		for level in 1..=21u32 {
			let quality = Quality::new(level, false, true);
			let expected = smallest.iter().find(|(at, _)| *at >= level).unwrap().1;
			let first = sizes.iter().position(|&size| Some(size) == expected);
			for index in 0..sizes.len() {
				let shown = drawn(&quality, index, 70.0);
				match first {
					None => assert!(!shown, "level {level}: the {} cube is culled", sizes[index]),
					Some(first) if index >= first => assert!(shown, "level {level}: the {} cube is drawn", sizes[index]),
					Some(first) if index + 1 == first && level <= 13 => assert!(!shown, "level {level}: the {} cube is culled", sizes[index]),
					_ => {}
				}
				// (at 8 the cubes at the row's ends, 0.05 and 0.6, are out of the client's view)
				if (1..sizes.len() - 1).contains(&index) {
					assert!(drawn(&quality, index, 8.0), "level {level}: every cube at a field of view of 8");
				}
			}
		}
	}

	/// The `far` and `farfine` stations: the last pillar drawn and the first not, by the distance from the eye
	/// to its box.
	#[test]
	fn the_view_distance_ends_where_the_clients_did() {
		let cases: [(&[u32], f32, Option<f32>); 10] = [
			(&[1], 490.0, Some(500.0)),
			(&[2], 760.0, Some(800.0)),
			(&[3], 1160.0, Some(1200.0)),
			(&[4, 8, 12], 1590.0, Some(1610.0)),
			(&[13], 2200.0, Some(2230.0)),
			(&[14], 3000.0, Some(3100.0)),
			(&[15], 3850.0, Some(4000.0)),
			(&[16], 5700.0, Some(6000.0)),
			(&[17], 10000.0, Some(10400.0)),
			(&[18, 21], 19000.0, None),
		];
		let eye = Vec3::new(0.0, 50.0, 0.0);
		let pillar = |distance: f32| {
			let width = 0.02 * distance;
			let center = Vec3::new(0.0, 50.0, -(distance + width * 0.5));
			(crate::lod::world_box(&Affine3A::from_translation(center), Vec3::new(width, 0.25 * distance, width)), center, Vec3::new(width, 0.25 * distance, width).length())
		};
		for (levels, shown, hidden) in cases {
			for &level in levels {
				for prioritize in [true, false] {
					let quality = Quality::new(level, false, prioritize);
					let (bounds, center, diameter) = pillar(shown);
					assert!(quality.drawn(eye, 70.0, bounds, center, diameter), "level {level}: {shown} drawn");
					if let Some(hidden) = hidden {
						let (bounds, center, diameter) = pillar(hidden);
						assert!(!quality.drawn(eye, 70.0, bounds, center, diameter), "level {level}: {hidden} not drawn");
					}
				}
			}
		}
	}

	#[test]
	fn each_level_holds_its_switches() {
		let top = Quality::top();
		assert_eq!(top, Quality::default());
		assert_eq!(top.sun_shadows, Some(SunReach { full: 587.5, gone: 587.5 }));
		assert_eq!((top.light_reach, top.shading_radius), (f32::INFINITY, f32::INFINITY));
		assert!(top.specular && top.bloom_effect && top.ambient_occlusion && top.glass_refraction && top.sun_rays_depth_of_field);
		assert_eq!((top.glow, top.msaa, top.clouds, top.particle_rate), (2, 4, 3, 1.0));
		// the shadow maps: none below 8, gone by 66 at 9 (full to 59), 587.5 from 19
		assert_eq!(Quality::new(7, false, true).sun_shadows, None);
		assert_eq!(Quality::new(9, false, true).sun_shadows, Some(SunReach { full: 59.0, gone: 66.0 }));
		for level in 9..=18 {
			let gone = Quality::new(level, false, true).sun_shadows.unwrap().gone;
			let fit = 2.5 * (level * level) as f32 - 15.5 * level as f32 + 3.0;
			assert!((gone - fit).abs() <= 6.0, "level {level}: {gone} against the fit's {fit}");
		}
		assert_eq!(Quality::new(19, false, true).sun_shadows.unwrap().gone, 587.5);
		assert_eq!(Quality::new(13, false, false).sun_shadows, None);
		assert_eq!(Quality::new(14, false, false).sun_shadows.unwrap().gone, 84.0);
		// specular from 5 (7 under false); local lights per pixel from 14 (17 under false; never under Soft)
		assert!(!Quality::new(4, false, true).specular && Quality::new(5, false, true).specular);
		assert!(!Quality::new(6, false, false).specular && Quality::new(7, false, false).specular);
		assert_eq!(Quality::new(13, false, true).light_reach, 0.0);
		assert_eq!(Quality::new(14, false, true).light_reach, 55.0);
		assert_eq!(Quality::new(16, false, false).light_reach, 0.0);
		assert_eq!(Quality::new(21, false, false).light_reach, 284.0);
		assert_eq!(Quality::new(21, true, true).light_reach, 0.0);
		// the shading radius: none at 5-6, 55 at 7, everywhere from 18; under false from 12
		assert_eq!(Quality::new(6, false, true).shading_radius, 0.0);
		assert_eq!(Quality::new(7, false, true).shading_radius, 55.0);
		assert_eq!(Quality::new(18, false, true).shading_radius, f32::INFINITY);
		assert_eq!(Quality::new(11, false, false).shading_radius, 0.0);
		assert_eq!(Quality::new(18, false, false).shading_radius, 755.0);
		// per pixel within 10.4 studs of the focus at 5, 36 at 6, 581 at 14, 1,180 at 21; under false two levels
		// later; none below
		assert_eq!([4, 5, 6, 14, 15, 21].map(|level| Quality::new(level, false, true).lit_radius), [0.0, 10.4, 36.0, 581.0, 676.0, 1180.0]);
		assert_eq!([6, 7, 8, 16, 21].map(|level| Quality::new(level, false, false).lit_radius), [0.0, 10.4, 36.0, 581.0, 1110.0]);
		for level in 1..=21 {
			for prioritize in [true, false] {
				let quality = Quality::new(level, false, prioritize);
				assert_eq!(quality.specular, quality.lit_radius > 0.0, "level {level}: specular where any part is lit per pixel");
			}
		}
		// about 11.5 (level - 4)^1.7 studs, the measured levels within 11 %
		for (level, radius) in [(5, 10.4), (6, 36.0), (7, 70.0), (8, 122.0), (9, 177.0), (10, 257.0), (11, 307.0), (12, 382.0), (13, 482.0), (14, 581.0), (15, 676.0), (16, 775.0), (18, 1075.0)] {
			let fit = 11.5 * ((level - 4) as f32).powf(1.7);
			assert!((fit / radius - 1.0).abs() < 0.11, "level {level}: {radius} against {fit}");
			assert_eq!(Quality::new(level, false, true).lit_radius, radius);
		}
		// glow 1-5 / 6-11 / 12+, BloomEffect from 12, AO from 20, MSAA 1 / 2 / 4, refraction and SunRays from 16
		assert_eq!([5, 6, 11, 12].map(|level| Quality::new(level, false, true).glow), [0, 1, 1, 2]);
		assert!(!Quality::new(11, false, true).bloom_effect && Quality::new(12, false, true).bloom_effect);
		assert!(!Quality::new(19, false, true).ambient_occlusion && Quality::new(20, false, true).ambient_occlusion);
		assert_eq!([16, 17, 18].map(|level| Quality::new(level, false, true).msaa), [1, 2, 4]);
		assert!(!Quality::new(15, false, true).glass_refraction && Quality::new(16, false, true).sun_rays_depth_of_field);
		assert_eq!([7, 8, 10, 11, 13, 14].map(|level| Quality::new(level, false, true).clouds), [0, 1, 1, 2, 2, 3]);
		// Rate: max(level, 2) / 21 (Rate 300 for 2.7 s: 0.100 of 21's at 1 and 2, 0.240 at 5, 0.480 at 10)
		for (level, share) in [(1, 0.0952), (2, 0.0952), (5, 0.238), (10, 0.476), (16, 0.762)] {
			assert!((Quality::new(level, false, true).particle_rate - share).abs() < 0.001, "level {level}");
		}
		// out of range: 0 as 1, above 21 as 21
		assert_eq!(Quality::new(0, false, true), Quality::new(1, false, true));
		assert_eq!(Quality::new(30, false, true), top);
	}
}
