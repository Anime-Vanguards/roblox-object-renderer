//! Terrain Clouds' generic model (clouds.wgsl): its figures and the sun's dimming near the camera. A
//! plain single-scattering march, not calibrated against Studio (docs/rendering.md "Clouds").

use super::{CpuVolume, Tuning, cpu_volumes, threshold, tuned};
use crate::scene::environment::Clouds;

/// The generic model's figures (clouds.wgsl), chosen by eye; Cover's thresholds and Color's exponent
/// as measured against Studio (`clouds`, `clouds-horizon`: each Cover's clouded share of the views).
pub const TUNING: Tuning = Tuning {
	steps: 24.0,
	detail: 0.13,
	detail_scale: 1.0,
	jitter: 0.5,
	cutoff: 0.02,
	blend: 1.0,
	sun: 1.0,
	sky: 1.0,
	ambient: 0.9,
	shadow: 1.0,
	sky_floor: 0.5,
	sky_low: 1.0,
	multiple: 0.35,
	power: 1.2,
	thresholds: [0.15, 0.33, 0.39, 0.59, 0.8],
	extinction: 1.0,
	density_power: 1.0,
};

/// The layer as clouds.wgsl has it: studs a unit, the planet's radius and the shell round its centre
/// (in units), the shape volume's tile
const STUDS: f32 = 4000.0;
const PLANET: f32 = 1000.0;
const BOTTOM: f32 = 1001.5;
const TOP: f32 = 1004.0;
const TILE: f32 = 32.0;
/// The share of the sun's light the clouds leave however thick they are (`cloud-sun-cover`: 0.50-0.52
/// of the light under any cloud thick enough, in Studio)
const FLOOR: f32 = 0.5;
/// Steps through the layer along the light
const STEPS: usize = 16;

/// The field at a point of the layer's space (x and z across in units, y the height through it), as
/// clouds.wgsl's `cloud_field` reads it at its finest level
fn field(volumes: &(CpuVolume, CpuVolume), p: glam::Vec3, tuning: &Tuning) -> f32 {
	let (shape, detail) = volumes;
	shape.sample(glam::Vec3::new(p.x / TILE, p.z / TILE, p.y)) + tuning.detail * (detail.sample(p * tuning.detail_scale) * 2.0 - 1.0)
}

/// The share of the sun's (or moon's) light Terrain Clouds take off for a camera at `camera` (studs)
/// with the light coming from `towards`: the cloud the light's ray meets on its way through the layer
/// to the camera's place, by Beer-Lambert as the sky's march dims its own light, the light keeping at
/// least [`FLOOR`] of itself; 0 without clouds or volumes. It dims only what is near the camera
/// (shadow.wgsl `cloud_sun`).
pub fn sun_cover(assets: &ror_assets::Assets, camera: glam::Vec3, towards: glam::Vec3, clouds: Option<&Clouds>) -> f32 {
	let (Some(clouds), Some(towards)) = (clouds, towards.try_normalize()) else {
		return 0.0;
	};
	let Some(volumes) = cpu_volumes(assets) else {
		return 0.0;
	};
	let tuning = tuned();
	let threshold = threshold(clouds.cover);
	// thinning out as the camera nears the layer, as the sky's march does
	let below = (BOTTOM - PLANET) * STUDS;
	let density = clouds.density.clamp(0.0, 1.0).powf(tuning.density_power) * tuning.extinction * ((below - camera.y) / (0.25 * below)).clamp(0.0, 1.0);
	if density <= 0.0 {
		return 0.0;
	}
	// up through the layer towards the light, kept at least 0.3 off level
	let mut up = towards;
	up.y = up.y.max(0.3);
	let up = up.normalize();
	let origin = camera / STUDS + glam::Vec3::new(0.0, PLANET, 0.0);
	let b = up.dot(origin);
	let c = origin.dot(origin);
	let enter = (-b + (b * b - (c - BOTTOM * BOTTOM)).max(0.0).sqrt()).max(0.0);
	let leave = -b + (b * b - (c - TOP * TOP)).max(0.0).sqrt();
	let step = (leave - enter).max(0.0) / STEPS as f32;
	let mut depth = 0.0;
	for index in 0..STEPS {
		let point = origin + up * (enter + (index as f32 + 0.5) * step);
		let p = glam::Vec3::new(point.x, (point.length() - BOTTOM) / (TOP - BOTTOM), point.z);
		depth += (threshold - field(volumes, p, &tuning)).max(0.0) * step;
	}
	let through = (-density * 40.0 * depth * tuning.shadow).exp();
	(1.0 - FLOOR) * (1.0 - through)
}
