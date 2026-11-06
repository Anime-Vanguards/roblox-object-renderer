//! Roblox's levels of detail and distance culling for a camera, as the capture Studio measured them
//! (notes/cost.md "Levels of detail", cost-probes l4-l17; Studio 0.742, a place with
//! `Workspace.MeshStreamingAndImprovedLods` Disabled, `MeshPartDetailLevel` DistanceBased as games
//! draw):
//!
//! - A MeshPart's level comes from the straight-line distance from the camera to the part's box in
//!   world axes (0 inside it), scaled by the field of view: `d · tan(fov / 2) / tan(35°)`. Full detail
//!   below 250, then the file's next level below 500, 2,000 and 3,000, the fifth beyond (l5-l7: the
//!   switch moved by the box's half extent along the line of sight, by its world box when turned,
//!   with the field of view as above and not with the viewport's size or shape, Camera.Focus or the
//!   quality level; no hysteresis, one frame late).
//! - RenderFidelity Performance takes every level the file has; Automatic only its high-quality
//!   levels (the header's count: uploads that mark one stay at full detail); Precise, and
//!   SpecialMeshes, full detail (l12, l13, l15). A Performance mesh without a chain gets levels made at
//!   run time (8,192 triangles: 4,016 / 1,962 / 812 / 394), which ror cannot make: it draws those at
//!   full detail and says so. Shadows are drawn at full detail whatever level the view draws a mesh
//!   at (render::levels keeps the shadow maps on the shared preparation).
//! - Below quality 19 a part farther from the camera than the quality's view distance (to its box,
//!   whatever the field of view) is not drawn, and at every quality a part too small on screen is
//!   not: its Size's length over the distance to its centre times tan(fov / 2) under (28 - quality)
//!   / 3,780 (l9, l10, l12, l16: about a pixel of a 1080-pixel-tall view at quality 21, four at 1).

use glam::{Affine3A, Vec3};
use serde::{Deserialize, Serialize};

/// `Enum.RenderFidelity`
pub const AUTOMATIC: u32 = 0;
pub const PRECISE: u32 = 1;
pub const PERFORMANCE: u32 = 2;

/// Where the levels change, in studs from the camera to the part's box at a 70° field of view.
pub const LEVEL_DISTANCES: [f32; 4] = [250.0, 500.0, 2000.0, 3000.0];

/// The share of its triangles a Performance mesh without a chain keeps at each level Roblox makes for
/// it at run time (l12: 8,192 triangles drawn as 4,016, 1,962, 812, 394).
pub const RUNTIME_LEVELS: [f32; 5] = [1.0, 4016.0 / 8192.0, 1962.0 / 8192.0, 812.0 / 8192.0, 394.0 / 8192.0];

/// Draw what Roblox draws at a graphics quality level (1-21): each mesh's level of detail for the
/// view's camera, and (`cull`) none of the parts the quality's view distance and size cull leave out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lod {
	pub quality: u32,
	/// Leave out what the quality culls; without, only the meshes' levels change (`levels`: an A/B
	/// of levels of detail against a render at full detail, everything else alike)
	pub cull: bool,
}

impl Default for Lod {
	fn default() -> Self {
		Self { quality: 21, cull: true }
	}
}

impl Lod {
	/// `roblox` (quality 21), `roblox:<quality>`, or `levels` (the levels alone, nothing culled).
	pub fn parse(text: &str) -> anyhow::Result<Self> {
		let text = text.trim();
		if text.eq_ignore_ascii_case("levels") {
			return Ok(Self { quality: 21, cull: false });
		}
		let (model, quality) = text.split_once(':').unwrap_or((text, "21"));
		if !model.eq_ignore_ascii_case("roblox") {
			anyhow::bail!("--lod takes roblox, roblox:<quality 1-21> or levels (got {text:?})");
		}
		let quality: u32 = quality.trim().parse().map_err(|_| anyhow::anyhow!("--lod roblox:<quality>: {quality:?} is not a quality level 1-21"))?;
		if !(1..=21).contains(&quality) {
			anyhow::bail!("--lod roblox:<quality>: quality levels are 1-21 (got {quality})");
		}
		Ok(Self { quality, cull: true })
	}

	/// Beyond this distance from the camera to a part's box, the quality draws nothing (l10, l16).
	pub fn view_distance(self) -> f32 {
		match self.quality {
			0..=3 => 300.0,
			4 => 450.0,
			5 => 470.0,
			6 => 550.0,
			7 => 570.0,
			8 | 9 => 600.0,
			10 => 700.0,
			11 => 1131.0,
			12 => 1600.0,
			13 | 14 => 2263.0,
			15 => 3200.0,
			16 => 4525.0,
			17 => 6400.0,
			18 => 9051.0,
			_ => 100_000.0,
		}
	}

	/// Whether a part with this box, centre and Size length shows from `eye` under a vertical field of
	/// view of `fov` degrees.
	pub fn drawn(self, eye: Vec3, fov: f32, (min, max): (Vec3, Vec3), center: Vec3, diameter: f32) -> bool {
		if !self.cull {
			return true;
		}
		if box_distance(eye, min, max) > self.view_distance() {
			return false;
		}
		let distance = center.distance(eye) * (fov.to_radians() * 0.5).tan();
		distance <= 0.0 || diameter * 3780.0 >= distance * (28.0 - self.quality.min(21) as f32)
	}
}

impl Serialize for Lod {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self.cull {
			true => serializer.serialize_str(&format!("roblox:{}", self.quality)),
			false => serializer.serialize_str("levels"),
		}
	}
}

impl<'de> Deserialize<'de> for Lod {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = String::deserialize(deserializer)?;
		Self::parse(&text).map_err(serde::de::Error::custom)
	}
}

/// The level of detail at a distance from the camera to a part's box, under a vertical field of view
/// in degrees: 0 (full detail) to 4.
pub fn level_at(distance: f32, fov: f32) -> usize {
	let scaled = distance * (fov.to_radians() * 0.5).tan() / 35f32.to_radians().tan();
	LEVEL_DISTANCES.iter().filter(|edge| scaled >= **edge).count()
}

/// The level a mesh draws: `level` as the distance gives it, kept to what its fidelity takes of a
/// chain of `levels` with `high_quality` of them marked high quality.
pub fn fidelity_level(fidelity: u32, level: usize, levels: usize, high_quality: u8) -> usize {
	match fidelity {
		PERFORMANCE => level.min(levels.max(1) - 1),
		AUTOMATIC => level.min(usize::from(high_quality.max(1)) - 1).min(levels.max(1) - 1),
		_ => 0,
	}
}

/// The straight-line distance from `eye` to a box (0 inside it).
pub fn box_distance(eye: Vec3, min: Vec3, max: Vec3) -> f32 {
	(min - eye).max(eye - max).max(Vec3::ZERO).length()
}

/// A part's box in world axes: its Size around its CFrame.
pub fn world_box(cframe: &Affine3A, size: Vec3) -> (Vec3, Vec3) {
	let extent = Vec3::from(cframe.matrix3.abs() * glam::Vec3A::from(size.abs() * 0.5));
	let center = Vec3::from(cframe.translation);
	(center - extent, center + extent)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The capture Studio's switches (cost-probes l5, l6, l7): the probe mesh at Size magnitude 10
	/// (half depth 3.103 studs) switched at 253.10 / 503.10 / 2003.10 / 3003.10 studs to its centre.
	#[test]
	fn levels_switch_where_studio_switched() {
		let size = Vec3::new(6.209, 4.789, 6.206);
		let at = |distance: f32| {
			let (min, max) = world_box(&Affine3A::IDENTITY, size);
			level_at(box_distance(Vec3::new(0.0, 0.0, distance), min, max), 70.0)
		};
		for (edge, level) in [(253.10, 1), (503.10, 2), (2003.10, 3), (3003.10, 4)] {
			assert_eq!(at(edge - 0.05), level - 1, "below {edge}");
			assert_eq!(at(edge + 0.05), level, "beyond {edge}");
		}
		// a 100 x 4 x 10 slab turned 45 degrees about Y: its world box's face, 38.89 studs out (l6)
		let turned = Affine3A::from_rotation_y(45f32.to_radians());
		let (min, max) = world_box(&turned, Vec3::new(100.0, 4.0, 10.0));
		assert_eq!(level_at(box_distance(Vec3::new(0.0, 0.0, 288.84), min, max), 70.0), 0);
		assert_eq!(level_at(box_distance(Vec3::new(0.0, 0.0, 288.94), min, max), 70.0), 1);
		// 100 and 20 degrees (l6, l7: a size-1 mesh at 147.22 / 294.09 and 993.12 / 1985.89)
		assert_eq!(level_at(147.0 - 0.31, 100.0), 0);
		assert_eq!(level_at(147.5 - 0.31, 100.0), 1);
		assert_eq!(level_at(1985.7 - 0.31, 20.0), 1);
		assert_eq!(level_at(1986.1 - 0.31, 20.0), 2);
	}

	#[test]
	fn fidelities_take_their_levels() {
		// Automatic keeps to the high-quality levels (the bush's two; previewMesh's five), Precise none
		assert_eq!(fidelity_level(AUTOMATIC, 3, 5, 1), 0);
		assert_eq!(fidelity_level(AUTOMATIC, 3, 5, 2), 1);
		assert_eq!(fidelity_level(AUTOMATIC, 3, 5, 5), 3);
		assert_eq!(fidelity_level(PRECISE, 3, 5, 5), 0);
		assert_eq!(fidelity_level(PERFORMANCE, 4, 4, 1), 3);
		assert_eq!(fidelity_level(PERFORMANCE, 4, 1, 1), 0);
		assert_eq!(fidelity_level(AUTOMATIC, 2, 5, 0), 0);
	}

	/// l10 and l12: cull distances of the probe mesh to its centre (size-1 at quality 21 771.21, 1
	/// 199.95; the size-100 one's box at quality 4 450 studs away, at 481.01 to its centre)
	#[test]
	fn culls_where_studio_culled() {
		let mesh = |magnitude: f32| Vec3::new(0.6209, 0.4789, 0.6206) * magnitude;
		let drawn = |quality: u32, magnitude: f32, distance: f32, fov: f32| {
			let size = mesh(magnitude);
			Lod { quality, cull: true }.drawn(Vec3::new(0.0, 0.0, distance), fov, world_box(&Affine3A::IDENTITY, size), Vec3::ZERO, size.length())
		};
		assert!(drawn(21, 1.0, 771.0, 70.0) && !drawn(21, 1.0, 771.5, 70.0));
		assert!(drawn(1, 1.0, 199.8, 70.0) && !drawn(1, 1.0, 200.1, 70.0));
		assert!(drawn(21, 1.0, 3062.0, 20.0) && !drawn(21, 1.0, 3063.0, 20.0));
		assert!(drawn(4, 100.0, 480.9, 70.0) && !drawn(4, 100.0, 481.2, 70.0));
		assert!(drawn(4, 100.0, 480.9, 20.0) && !drawn(4, 100.0, 481.2, 100.0));
		assert!(drawn(17, 100.0, 6431.0, 70.0) && !drawn(17, 100.0, 6431.2, 70.0));
	}

	#[test]
	fn parses_the_option() {
		assert_eq!(Lod::parse("roblox").unwrap(), Lod { quality: 21, cull: true });
		assert_eq!(Lod::parse("roblox:4").unwrap(), Lod { quality: 4, cull: true });
		assert_eq!(Lod::parse("levels").unwrap(), Lod { quality: 21, cull: false });
		assert!(Lod::parse("roblox:22").is_err());
		assert!(Lod::parse("studio").is_err());
	}
}
