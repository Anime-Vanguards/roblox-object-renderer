//! ParticleEmitters: where particles spawn and every property of their look and motion (notes/render.md
//! "Particles"). The renderer simulates them; nothing here is random.

use glam::{Affine3A, Vec2, Vec3};
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::{Ref, Variant};

use super::load::Document;
use super::props;

/// A NumberSequence's keypoints: (time 0..1, value, envelope).
pub type NumberKeys = Vec<(f32, f32, f32)>;

/// An emitter makes at most this many particles a second whatever its Rate, as Roblox documents
/// (vfx-emitter: an effect's interior at Rate 400, 2000 and 11250 for 0.3 s leaves the same
/// 1,344 / 1,356 / 1,522 dark pixels in Studio, where ror drew 1,304 / 6,447 / 20,818; at 200, 300,
/// 400, 500 and 800 Studio's grow to 400-500 and stop). 238 of one game's running emitters ask for more.
const MAX_RATE: f32 = 400.0;

/// The most columns or rows a Custom flipbook grid is read with.
const MAX_FLIPBOOK_SIZE: i32 = 1024;

/// The most light a ParticleEmitter's particle emits per channel, as transparent Neon's
/// (notes/render.md "Particles": particle-bright).
pub const EMISSION_CAP: f32 = 7.84;

#[derive(Clone, Debug, PartialEq)]
pub struct ParticleEmitter {
	pub path: String,
	/// Where particles spawn: the parent part's CFrame (its shape spans `size`), or an Attachment's
	/// world CFrame with a zero size.
	pub cframe: Affine3A,
	pub size: Vec3,
	/// Parented to an Attachment (or Bone): particles leave from a point.
	pub attachment: bool,
	pub enabled: bool,
	/// Particles per second.
	pub rate: f32,
	/// NumberRange (min, max), seconds.
	pub lifetime: (f32, f32),
	/// Studs per second.
	pub speed: (f32, f32),
	/// Degrees about the two axes across the emission direction.
	pub spread_angle: Vec2,
	/// World studs per second².
	pub acceleration: Vec3,
	pub drag: f32,
	pub velocity_inheritance: f32,
	pub locked_to_part: bool,
	/// `Enum.NormalId` of the face particles leave from.
	pub emission_direction: u32,
	/// `Enum.ParticleEmitterShape`: 0 Box, 1 Sphere, 2 Cylinder, 3 Disc.
	pub shape: u32,
	/// `Enum.ParticleEmitterShapeInOut`: 0 Outward, 1 Inward, 2 InAndOut.
	pub shape_in_out: u32,
	/// `Enum.ParticleEmitterShapeStyle`: 0 Volume, 1 Surface.
	pub shape_style: u32,
	pub shape_partial: f32,
	/// Degrees, and degrees per second.
	pub rotation: (f32, f32),
	pub rot_speed: (f32, f32),
	/// Size, Squash and Transparency over a particle's life (age / lifetime).
	pub size_keys: NumberKeys,
	pub squash: NumberKeys,
	pub transparency: NumberKeys,
	/// Color keypoints: (time, sRGB colour 0..1).
	pub color: Vec<(f32, Vec3)>,
	pub light_emission: f32,
	pub light_influence: f32,
	pub brightness: f32,
	pub texture: Option<String>,
	/// `Enum.ParticleOrientation`: 0 FacingCamera, 1 FacingCameraWorldUp, 2 VelocityParallel,
	/// 3 VelocityPerpendicular.
	pub orientation: u32,
	pub z_offset: f32,
	pub time_scale: f32,
	/// `Enum.ParticleFlipbookLayout`: 0 None, 1 Grid2x2, 2 Grid4x4, 3 Grid8x8, 4 Custom.
	pub flipbook_layout: u32,
	/// `Enum.ParticleFlipbookMode`: 0 Loop, 1 OneShot, 2 PingPong, 3 Random.
	pub flipbook_mode: u32,
	/// Frames per second (min, max).
	pub flipbook_framerate: (f32, f32),
	pub flipbook_start_random: bool,
	pub flipbook_blend_frames: bool,
	/// Columns and rows of a Custom layout.
	pub flipbook_size: (u32, u32),
	/// Calibration only: particles emitted at once (`Emit`), as (seconds before the render, count).
	pub bursts: Vec<(f32, u32)>,
	/// The ParticleEmitter instance (its playback's events).
	pub referent: Option<Ref>,
	/// A playback's particles (when and where each was emitted) instead of the steady state.
	pub births: Option<Vec<super::playback::Birth>>,
	/// It rides on a part the game keeps under the camera (scene::follow): its steady state is drawn
	/// there, and a clip moves the part with its camera.
	pub follow: Option<super::follow::Follow>,
	/// Seeds the snapshot's random choices (from the path: repeatable, different per emitter).
	pub seed: u64,
	/// A legacy effect's system: the particles one emit sends (an Explosion going off), in place of
	/// the action's count.
	pub emit_count: Option<u32>,
	/// A legacy effect's system: the part, attachment or bone whose world CFrame it emits from in a
	/// playback (`Ref::none()`: a fixed place in the world); a ParticleEmitter's parent when None.
	pub mount: Option<Ref>,
	/// The most light a particle emits per channel: transparent Neon's 7.84 for a ParticleEmitter
	/// (particle-bright); a legacy effect's flames go beyond it (legacy-fire-blend).
	pub cap: f32,
	/// Blends in linear light over what is behind it, as Studio's legacy effects do (a
	/// ParticleEmitter blends in the √ space: notes/render.md "Particles").
	pub linear: bool,
	/// Its texture's colour is premultiplied by its alpha (Studio's legacy effect textures): the
	/// texel's colour is the light it gives (with `linear`).
	pub premultiplied: bool,
	/// A legacy effect's texels show where their alpha is above a threshold, over a width: (threshold,
	/// width), the alpha as is for a width 0.
	pub alpha_ramp: (f32, f32),
	/// A legacy effect's rotation turns its texture within the quad, which keeps facing the camera
	/// upright (the ForceField's rings spin flattened).
	pub spin_texture: bool,
	/// World studs per second its Drag pulls the particles' velocity towards: Workspace.GlobalWind with
	/// WindAffectsDrag, else none (and none in a file without a Workspace).
	pub wind: Vec3,
}

impl ParticleEmitter {
	/// Columns and rows of the flipbook grid (1 x 1 without one).
	pub fn flipbook_grid(&self) -> (u32, u32) {
		match self.flipbook_layout {
			1 => (2, 2),
			2 => (4, 4),
			3 => (8, 8),
			4 => (self.flipbook_size.0.max(1), self.flipbook_size.1.max(1)),
			_ => (1, 1),
		}
	}

	/// The largest Size any particle can reach: the Size sequence's value plus its envelope at its
	/// largest of the 32 samples a particle's sequences are drawn from (k / 31 of its life), not at
	/// a keypoint between two of them (vfx-sibling-order end-key: two squares at one place peaking
	/// at 1 at 0.2 of their life, one ending at 0, the other at 0.8, hold 0.968 and 0.994 at 7 / 31;
	/// Studio drew the one ending at 0.8 first, under the other, in either order in the file, where
	/// ror drew them in the file's order; an effect's SpeedLines alike, 1.687 and 1.707: its black
	/// streaks over the white ones).
	pub fn largest_size(&self) -> f32 {
		let envelopes: Vec<f32> = self.size_keys.iter().map(|key| key.2.abs()).collect();
		(0..32).map(|k| number_at(&self.size_keys, k as f32 / 31.0, &envelopes)).fold(0.0, f32::max)
	}

	/// How far beyond its part's half diagonal (`size`) the emitter's particles can start: a Cylinder
	/// past ShapePartial 1 or below 0 carries on past its wider end ([`cylinder_beyond`]), wider as it
	/// goes; 0 for every other emitter.
	pub fn spawn_spill(&self, size: glam::Vec3) -> f32 {
		let partial = self.shape_partial;
		if self.attachment || self.shape != 2 || (0.0..=1.0).contains(&partial) {
			return 0.0;
		}
		// the Cylinder's axis by EmissionDirection (render::particles' SHAPE_FRAMES: Right and Left run
		// along Z, the others along X), round across the other two sides
		let axis = if matches!(self.emission_direction, 0 | 3) { 2 } else { 0 };
		let length = size[axis];
		let across = (0..3).filter(|&side| side != axis).map(|side| size[side] * 0.5).fold(0.0, f32::max);
		let surface = self.shape_style == 1;
		let reach = [cylinder_beyond(partial, surface, 0.0), cylinder_beyond(partial, surface, 1.0), 0.0, 1.0]
			.iter()
			.map(|&along| ((along - 0.5) * length).hypot((1.0 + (partial - 1.0) * along).abs() * across))
			.fold(0.0, f32::max);
		(reach - size.length() * 0.5).max(0.0)
	}
}

/// Where a Roblox client puts a Cylinder's particles along its axis past ShapePartial 1 and below 0:
/// the share k / 16 of them lies before each entry, in lengths from the full end (1 at the end
/// ShapePartial scales), the radius growing on linearly from 1 to ShapePartial and on past it
/// (`fixtures/particle-probes.luau` "taper": 1000 or 3000 particles in each of 29 12 x 2 x 2
/// Cylinders, seen from 400 studs in four sessions of the client, 0.747, that agree within 0.05;
/// ror's renders through the same reading within 0.02-0.1 of these, the most for the slabs of 1000).
/// It does not stop at the ends: past 1 it carries on past the wide end, crowding a slab there
/// densest at its far side (ShapePartial 2: 39 % of the particles, up to 0.1 of the length out; 3:
/// 63 %, 0.43; 5: 79 %, 1.18), and below 0 past the full end (-0.5: 57 %, 0.12; -1: 69 %, 0.28; -2:
/// 82 %, 0.64). The surface style carries on less (2: 2-4 %, 0.01; 3: 52 %, 0.14; 5: 75 %, 0.59).
/// No closed law found; between rows the quantiles blend, towards the in-range law at 1 (an even
/// spread) and at 0 (u³, the side's u²), and past the last row they hold.
const CYLINDER_BEYOND_VOLUME: [(f32, [f32; 17]); 16] = [
	(-2.00, [-0.639, -0.623, -0.611, -0.594, -0.564, -0.529, -0.483, -0.432, -0.373, -0.316, -0.247, -0.171, -0.090, -0.013, 0.238, 0.563, 1.000]),
	(-1.50, [-0.459, -0.444, -0.434, -0.418, -0.398, -0.370, -0.337, -0.296, -0.253, -0.202, -0.147, -0.088, -0.028, 0.109, 0.348, 0.628, 1.000]),
	(-1.00, [-0.280, -0.268, -0.261, -0.250, -0.240, -0.220, -0.195, -0.164, -0.123, -0.088, -0.043, -0.004, 0.118, 0.272, 0.468, 0.720, 1.000]),
	(-0.75, [-0.199, -0.187, -0.180, -0.170, -0.158, -0.143, -0.124, -0.103, -0.082, -0.057, -0.034, -0.005, 0.103, 0.252, 0.445, 0.717, 1.000]),
	(-0.50, [-0.120, -0.108, -0.102, -0.096, -0.087, -0.072, -0.056, -0.043, -0.027, -0.005, 0.049, 0.145, 0.246, 0.423, 0.594, 0.756, 1.000]),
	(-0.25, [-0.050, -0.046, -0.041, -0.036, -0.030, -0.023, -0.015, -0.006, 0.012, 0.059, 0.118, 0.207, 0.314, 0.443, 0.585, 0.730, 1.000]),
	(1.10, [0.000, 0.067, 0.140, 0.210, 0.273, 0.341, 0.406, 0.471, 0.531, 0.596, 0.659, 0.718, 0.780, 0.833, 0.887, 0.941, 0.998]),
	(1.25, [0.000, 0.079, 0.160, 0.236, 0.306, 0.378, 0.446, 0.523, 0.591, 0.662, 0.724, 0.781, 0.834, 0.884, 0.927, 0.966, 0.999]),
	(1.40, [0.000, 0.095, 0.180, 0.266, 0.347, 0.428, 0.506, 0.582, 0.652, 0.719, 0.779, 0.835, 0.883, 0.926, 0.958, 0.983, 1.002]),
	(1.50, [0.000, 0.091, 0.193, 0.289, 0.376, 0.466, 0.556, 0.636, 0.712, 0.776, 0.832, 0.884, 0.925, 0.956, 0.981, 0.993, 1.008]),
	(1.75, [0.000, 0.107, 0.213, 0.307, 0.407, 0.497, 0.595, 0.689, 0.774, 0.859, 0.919, 0.965, 0.997, 1.012, 1.021, 1.027, 1.039]),
	(2.00, [0.000, 0.134, 0.266, 0.401, 0.512, 0.632, 0.735, 0.828, 0.903, 0.969, 1.010, 1.032, 1.051, 1.068, 1.081, 1.087, 1.099]),
	(2.50, [0.000, 0.160, 0.313, 0.473, 0.624, 0.758, 0.885, 0.994, 1.048, 1.093, 1.134, 1.165, 1.194, 1.214, 1.231, 1.240, 1.250]),
	(3.00, [0.000, 0.194, 0.373, 0.555, 0.722, 0.882, 1.015, 1.093, 1.160, 1.218, 1.272, 1.317, 1.352, 1.381, 1.401, 1.409, 1.425]),
	(4.00, [0.000, 0.244, 0.503, 0.736, 0.959, 1.090, 1.211, 1.313, 1.406, 1.494, 1.563, 1.627, 1.684, 1.725, 1.754, 1.770, 1.797]),
	(5.00, [0.000, 0.317, 0.641, 0.928, 1.116, 1.270, 1.422, 1.558, 1.676, 1.792, 1.883, 1.963, 2.030, 2.083, 2.120, 2.144, 2.184]),
];

/// [`CYLINDER_BEYOND_VOLUME`]'s rows for the surface style.
const CYLINDER_BEYOND_SURFACE: [(f32, [f32; 17]); 8] = [
	(-1.00, [-0.130, -0.125, -0.119, -0.111, -0.100, -0.090, -0.073, -0.052, -0.031, 0.010, 0.088, 0.181, 0.320, 0.460, 0.613, 0.785, 1.000]),
	(-0.50, [-0.050, -0.043, -0.037, -0.031, -0.023, -0.011, 0.007, 0.045, 0.094, 0.170, 0.274, 0.389, 0.492, 0.617, 0.711, 0.850, 1.000]),
	(1.25, [0.000, 0.077, 0.155, 0.227, 0.295, 0.362, 0.427, 0.493, 0.555, 0.618, 0.679, 0.738, 0.794, 0.852, 0.905, 0.953, 0.999]),
	(1.40, [0.000, 0.091, 0.175, 0.253, 0.332, 0.407, 0.476, 0.542, 0.606, 0.671, 0.728, 0.783, 0.830, 0.877, 0.919, 0.959, 0.999]),
	(1.50, [0.000, 0.094, 0.181, 0.264, 0.346, 0.413, 0.473, 0.534, 0.601, 0.669, 0.722, 0.779, 0.836, 0.877, 0.920, 0.956, 1.000]),
	(2.00, [0.000, 0.130, 0.237, 0.344, 0.434, 0.531, 0.620, 0.687, 0.753, 0.810, 0.863, 0.903, 0.942, 0.968, 0.986, 0.995, 1.009]),
	(3.00, [0.000, 0.193, 0.370, 0.520, 0.669, 0.800, 0.890, 0.975, 1.014, 1.042, 1.064, 1.087, 1.099, 1.110, 1.117, 1.124, 1.138]),
	(5.00, [0.000, 0.333, 0.596, 0.832, 1.011, 1.111, 1.182, 1.252, 1.312, 1.360, 1.408, 1.456, 1.496, 1.525, 1.544, 1.559, 1.587]),
];

/// Where along a Cylinder's axis a particle starts for a uniform `u`, past ShapePartial 1 or below 0
/// (`partial` outside 0..=1; [`CYLINDER_BEYOND_VOLUME`]): in lengths from the full end, outside
/// 0..1 for the particles carried past an end.
pub fn cylinder_beyond(partial: f32, surface: bool, u: f32) -> f32 {
	let rows: &[(f32, [f32; 17])] = if surface { &CYLINDER_BEYOND_SURFACE } else { &CYLINDER_BEYOND_VOLUME };
	let x = u.clamp(0.0, 1.0) * 16.0;
	let index = (x as usize).min(15);
	let quantile = |row: &[f32; 17]| row[index] + (row[index + 1] - row[index]) * (x - index as f32);
	// the in-range law where this side meets it: an even spread at 1, u³ (the side's u²) at 0
	let above = partial > 1.0;
	let meeting = if above { (1.0, u) } else { (0.0, u.powi(if surface { 2 } else { 3 })) };
	let mut below: Option<(f32, f32)> = None;
	let mut over: Option<(f32, f32)> = None;
	for (p, t) in rows.iter().filter(|(p, _)| (*p > 1.0) == above).map(|(p, row)| (*p, quantile(row))).chain(std::iter::once(meeting)) {
		if p <= partial {
			if below.is_none_or(|(nearest, _)| p > nearest) {
				below = Some((p, t));
			}
		} else if over.is_none_or(|(nearest, _)| p < nearest) {
			over = Some((p, t));
		}
	}
	match (below, over) {
		(Some((a, from)), Some((b, to))) => from + (to - from) * (partial - a) / (b - a),
		(Some((_, t)), None) | (None, Some((_, t))) => t,
		(None, None) => u,
	}
}

/// A NumberSequence at `t` (0..1) with each keypoint moved by `offsets[k]` (its envelope's random
/// share, 0 without one): linear between keypoints.
pub fn number_at(keys: &[(f32, f32, f32)], t: f32, offsets: &[f32]) -> f32 {
	let value = |index: usize| keys[index].1 + offsets.get(index).copied().unwrap_or(0.0);
	let Some(first) = keys.first() else {
		return 0.0;
	};
	if t <= first.0 {
		return value(0);
	}
	for index in 1..keys.len() {
		if t <= keys[index].0 {
			let span = keys[index].0 - keys[index - 1].0;
			let f = if span > 1e-6 { (t - keys[index - 1].0) / span } else { 1.0 };
			return value(index - 1) + (value(index) - value(index - 1)) * f;
		}
	}
	value(keys.len() - 1)
}

/// A ColorSequence at `t`: linear between keypoints, sRGB.
pub fn color_at(keys: &[(f32, Vec3)], t: f32) -> Vec3 {
	let Some(first) = keys.first() else {
		return Vec3::ONE;
	};
	if t <= first.0 {
		return first.1;
	}
	for pair in keys.windows(2) {
		if t <= pair[1].0 {
			let span = pair[1].0 - pair[0].0;
			let f = if span > 1e-6 { (t - pair[0].0) / span } else { 1.0 };
			return pair[0].1.lerp(pair[1].1, f);
		}
	}
	keys[keys.len() - 1].1
}

fn path_hash(path: &str) -> u64 {
	path.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3))
}

/// An emitter's random seed: from its path, and for the second and later of same-named emitters under
/// one parent their place among them too. Studio draws each copy from a stream of its own: a dragon
/// fire is three copies of one emitter on one attachment, three times the flames in Studio where ror drew
/// the copies on top of each other (an auto run's Dragon, only Fire, still and
/// bare: 0.65 / 0.57 / 0.41 of Studio's light at 0.1 / 0.3 / 0.6 s; a copy alone, 1.24).
fn seed(document: &Document, referent: Ref) -> u64 {
	let path = document.path_of(referent);
	let instance = document.dom.get_by_ref(referent);
	let siblings = instance.and_then(|instance| document.dom.get_by_ref(instance.parent())).map(|parent| parent.children()).unwrap_or_default();
	let copy = siblings
		.iter()
		.take_while(|&&sibling| sibling != referent)
		.filter(|&&sibling| document.dom.get_by_ref(sibling).zip(instance).is_some_and(|(sibling, instance)| sibling.class == instance.class && sibling.name == instance.name))
		.count();
	// a playback's salt draws every emitter again (how far one draw strays from another: notes/render.md "Particles")
	let salt = &document.salt;
	if copy == 0 { path_hash(&format!("{path}{salt}")) } else { path_hash(&format!("{path}#{copy}{salt}")) }
}

/// A ColorSequence's keypoints (time 0..1, sRGB colour). One with a channel outside 0..1 does not load
/// in Studio, which keeps the property's default, white (vfx-color-range: particles and beams of (3, 0,
/// 0), (-1, 0, 0), (-1, -1, -1) and (0.5, -2, 1) read back white and draw white; a script's
/// ColorSequence.new clamps to 0..1 instead, so files Studio saves never hold such values). Particles,
/// beams and trails alike.
pub fn color_keys(instance: &Instance, name: &str) -> Vec<(f32, Vec3)> {
	let keys: Vec<(f32, Vec3)> = match props::get(instance, name) {
		Some(Variant::ColorSequence(sequence)) => sequence.keypoints.iter().map(|key| (key.time, Vec3::new(key.color.r, key.color.g, key.color.b))).collect(),
		Some(Variant::Color3(color)) => vec![(0.0, Vec3::new(color.r, color.g, color.b))],
		_ => Vec::new(),
	};
	// written so that a NaN fails too
	let in_range = |color: &Vec3| color.cmpge(Vec3::ZERO).all() && color.cmple(Vec3::ONE).all();
	if keys.is_empty() || !keys.iter().all(|(_, color)| in_range(color)) {
		return vec![(0.0, Vec3::ONE)];
	}
	keys
}

fn number_keys(instance: &Instance, name: &str, fallback: f32) -> NumberKeys {
	match props::get(instance, name) {
		Some(Variant::NumberSequence(sequence)) => sequence.keypoints.iter().map(|key| (key.time, key.value, key.envelope)).collect(),
		Some(Variant::Float32(value)) => vec![(0.0, *value, 0.0), (1.0, *value, 0.0)],
		_ => vec![(0.0, fallback, 0.0), (1.0, fallback, 0.0)],
	}
}

fn range(instance: &Instance, name: &str, fallback: (f32, f32)) -> (f32, f32) {
	match props::get(instance, name) {
		Some(Variant::NumberRange(range)) => (range.min, range.max),
		Some(Variant::Float32(value)) => (*value, *value),
		_ => fallback,
	}
}

/// The volume an emitter outside any part or attachment spawns in, about the world's origin
/// (vfx-model-emitter: 30 still particles of an emitter under a Model spread over ±1 stud on every
/// axis, std 0.55-0.64 against a uniform box's 0.58).
const ORPHAN_SIZE: Vec3 = Vec3::splat(2.0);

/// A ParticleEmitter instance, or `None` when it has no parent.
pub fn read(document: &Document, referent: Ref, instance: &Instance, _warnings: &mut Vec<String>) -> Option<ParticleEmitter> {
	let path = document.path_of(referent);
	let parent = document.dom.get_by_ref(instance.parent())?;
	// outside a part or an attachment (a Model's, a Folder's) it emits from a 2-stud box at the world's
	// origin along the world's axes (vfx-model-emitter: emitters under a Model whose part stands 8
	// studs aside, under a Folder and under the model itself sent their particles off the origin along
	// their EmissionDirection; an effect's BackWind crack, a child of its Model, showed there in Studio)
	let (cframe, size, attachment) = if props::is_a(parent, "Attachment") {
		(super::beams::world_cframe(document, instance.parent())?, Vec3::ZERO, true)
	} else if props::is_a(parent, "BasePart") {
		(props::cframe(parent, "CFrame")?, props::vec3(parent, "Size", Vec3::new(4.0, 1.0, 2.0)), false)
	} else {
		(glam::Affine3A::IDENTITY, ORPHAN_SIZE, false)
	};
	let color = color_keys(instance, "Color");
	let spread = match props::get(instance, "SpreadAngle") {
		Some(Variant::Vector2(value)) => Vec2::new(value.x, value.y),
		_ => Vec2::ZERO,
	};
	Some(ParticleEmitter {
		path,
		cframe,
		size,
		attachment,
		enabled: props::bool(instance, "Enabled", true),
		rate: props::f32(instance, "Rate", 20.0).min(MAX_RATE),
		lifetime: range(instance, "Lifetime", (5.0, 10.0)),
		speed: range(instance, "Speed", (5.0, 5.0)),
		spread_angle: spread,
		acceleration: props::vec3(instance, "Acceleration", Vec3::ZERO),
		drag: props::f32(instance, "Drag", 0.0),
		velocity_inheritance: props::f32(instance, "VelocityInheritance", 0.0),
		locked_to_part: props::bool(instance, "LockedToPart", false),
		emission_direction: props::enum_value(instance, "EmissionDirection", 1),
		shape: props::enum_value(instance, "Shape", 0),
		shape_in_out: props::enum_value(instance, "ShapeInOut", 0),
		shape_style: props::enum_value(instance, "ShapeStyle", 0),
		shape_partial: props::f32(instance, "ShapePartial", 1.0),
		rotation: range(instance, "Rotation", (0.0, 0.0)),
		rot_speed: range(instance, "RotSpeed", (0.0, 0.0)),
		size_keys: number_keys(instance, "Size", 1.0),
		squash: number_keys(instance, "Squash", 0.0),
		transparency: number_keys(instance, "Transparency", 0.0),
		color,
		light_emission: props::f32(instance, "LightEmission", 0.0),
		light_influence: props::f32(instance, "LightInfluence", 0.0),
		brightness: props::f32(instance, "Brightness", 1.0),
		texture: props::content(instance, &["TextureContent", "Texture"]),
		orientation: props::enum_value(instance, "Orientation", 0),
		z_offset: props::f32(instance, "ZOffset", 0.0),
		time_scale: props::f32(instance, "TimeScale", 1.0),
		flipbook_layout: props::enum_value(instance, "FlipbookLayout", 0),
		flipbook_mode: props::enum_value(instance, "FlipbookMode", 0),
		flipbook_framerate: range(instance, "FlipbookFramerate", (1.0, 1.0)),
		flipbook_start_random: props::bool(instance, "FlipbookStartRandom", false),
		flipbook_blend_frames: props::bool(instance, "FlipbookBlendFrames", true),
		// a Custom grid's columns and rows, at most MAX_FLIPBOOK_SIZE each (frames past a pixel of a
		// 1024-pixel sheet; their product stays a count)
		flipbook_size: (props::i32(instance, "FlipbookSizeX", 1).clamp(1, MAX_FLIPBOOK_SIZE) as u32, props::i32(instance, "FlipbookSizeY", 1).clamp(1, MAX_FLIPBOOK_SIZE) as u32),
		bursts: Vec::new(),
		referent: Some(referent),
		births: None,
		follow: super::follow::of(document, referent),
		seed: seed(document, referent),
		emit_count: None,
		mount: None,
		cap: EMISSION_CAP,
		linear: false,
		premultiplied: false,
		alpha_ramp: (0.0, 0.0),
		spin_texture: false,
		wind: if props::bool(instance, "WindAffectsDrag", false) { global_wind(document) } else { Vec3::ZERO },
	})
}

/// Workspace.GlobalWind of a place (a model file has no Workspace: no wind).
fn global_wind(document: &Document) -> Vec3 {
	let dom = &document.dom;
	dom.root().children().iter().filter_map(|&child| dom.get_by_ref(child)).find(|instance| instance.class == "Workspace").map_or(Vec3::ZERO, |workspace| props::vec3(workspace, "GlobalWind", Vec3::ZERO))
}

#[cfg(test)]
mod tests {
	use rbx_dom_weak::InstanceBuilder;
	use rbx_dom_weak::types::{Color3, ColorSequence, ColorSequenceKeypoint};

	use super::*;

	/// A ColorSequence with a channel outside 0..1 (or not a number) loads as white, as in Studio
	/// (vfx-color-range); one inside keeps its keypoints
	#[test]
	fn out_of_range_colour_sequences_load_white() {
		let keys = |colors: &[[f32; 3]]| {
			let keypoints = colors.iter().enumerate().map(|(index, c)| ColorSequenceKeypoint::new(index as f32 / (colors.len() - 1).max(1) as f32, Color3::new(c[0], c[1], c[2]))).collect();
			let dom = rbx_dom_weak::WeakDom::new(InstanceBuilder::new("ParticleEmitter").with_property("Color", ColorSequence { keypoints }));
			color_keys(dom.root(), "Color")
		};
		assert_eq!(keys(&[[0.5, 0.25, 1.0], [0.0, 1.0, 0.0]]), vec![(0.0, Vec3::new(0.5, 0.25, 1.0)), (1.0, Vec3::new(0.0, 1.0, 0.0))]);
		for bad in [[3.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.5, -2.0, 1.0], [f32::NAN, 0.0, 0.0]] {
			assert_eq!(keys(&[[0.5; 3], bad]), vec![(0.0, Vec3::ONE)], "{bad:?}");
		}
	}

	/// A Cylinder past ShapePartial 1 or below 0 spreads as the client's measured quantiles at the
	/// measured values, blends between them and towards the in-range laws next to 1 and 0, and holds
	/// past the last row
	#[test]
	fn cylinders_beyond_shape_partial_one_follow_the_measured_quantiles() {
		// at a measured value: the row itself (ShapePartial 2's median 0.903, its far side 1.099)
		assert!((cylinder_beyond(2.0, false, 0.5) - 0.903).abs() < 1e-6);
		assert!((cylinder_beyond(2.0, false, 1.0) - 1.099).abs() < 1e-6);
		// between rows, halfway
		let half = (cylinder_beyond(2.0, false, 0.75) + cylinder_beyond(2.5, false, 0.75)) * 0.5;
		assert!((cylinder_beyond(2.25, false, 0.75) - half).abs() < 1e-6);
		// next to 1 towards an even spread, next to 0 towards u³ (the surface style's u²)
		let near_one = cylinder_beyond(1.05, false, 0.3);
		assert!((near_one - (0.3 + cylinder_beyond(1.1, false, 0.3)) * 0.5).abs() < 1e-6);
		let near_zero = cylinder_beyond(-0.125, true, 0.6);
		assert!((near_zero - (0.6f32.powi(2) + cylinder_beyond(-0.25, true, 0.6)) * 0.5).abs() < 1e-3);
		// past the last rows they hold
		assert_eq!(cylinder_beyond(8.0, false, 0.9), cylinder_beyond(5.0, false, 0.9));
		assert_eq!(cylinder_beyond(-3.0, false, 0.1), cylinder_beyond(-2.0, false, 0.1));
	}
}
