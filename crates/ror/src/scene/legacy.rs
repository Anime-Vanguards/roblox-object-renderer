//! Legacy effects (notes/render.md "Legacy effects"): Fire, Smoke, Sparkles, Explosion and the
//! ForceField instance are Studio's built-in particle effects. Each becomes the ParticleEmitters
//! that draw it, with Studio's textures (`content/textures/particles`) and the measured rates,
//! sizes, motion and light, scaled by the instance's properties; the renderer simulates and draws
//! them as any emitter's.

use glam::{Affine3A, Vec2, Vec3};
use rbx_dom_weak::Instance;
use rbx_dom_weak::types::Ref;

use super::load::Document;
use super::particles::{NumberKeys, ParticleEmitter};
use super::props;

/// One particle system of a legacy effect at the effect's reference settings (Fire Size 5 and Heat
/// 9, Smoke Size 1 and RiseVelocity 1, ...); `read` scales it by the instance's properties.
#[derive(Clone, Debug, PartialEq)]
pub struct System {
	pub name: &'static str,
	pub texture: &'static str,
	/// Particles per second (0: only emitted, as an Explosion's).
	pub rate: f32,
	/// Particles an emit sends (an Explosion going off).
	pub count: u32,
	pub lifetime: (f32, f32),
	/// Studs per second along the emitter's up axis, spread within ±`spread` degrees.
	pub speed: (f32, f32),
	pub spread: f32,
	/// Spawn: a cube of this half-side about the emitter (0: its point), or a box of these half-sides
	/// (`spawn`, when not zero).
	pub radius: f32,
	pub spawn: Vec3,
	/// Studs the spawn sits along the emitter's up axis.
	pub offset: f32,
	/// Studs per second² along the emitter's up axis, and Drag.
	pub rise: f32,
	pub drag: f32,
	/// Half the quad's side (ParticleEmitter Size) over the particle's life: (time 0..1, size), and its
	/// Squash (negative: wider than tall).
	pub size: Vec<(f32, f32)>,
	pub squash: f32,
	/// Transparency over life.
	pub transparency: Vec<(f32, f32)>,
	/// Emitted light: Brightness (x = 1.935 · Brightness · colour² · texel²), LightEmission,
	/// LightInfluence.
	pub brightness: f32,
	pub light_emission: f32,
	pub light_influence: f32,
	/// Degrees, and degrees per second.
	pub rotation: (f32, f32),
	pub rot_speed: (f32, f32),
	/// Particles leave outwards, across the emitter's up axis (a ring), instead of along it, or in
	/// every direction (`sphere`).
	pub outwards: bool,
	pub sphere: bool,
	/// Colour over life (time 0..1, sRGB), times the effect's colour; none: the effect's colour.
	pub colors: Vec<(f32, Vec3)>,
	/// The texture's colour is premultiplied by its alpha (fire_main.dds).
	pub premultiplied: bool,
	/// Texels show above this alpha threshold, over this width (width 0: the alpha as is).
	pub alpha_ramp: (f32, f32),
	/// Blends in linear light in the legacy effects' layer (Fire, the Explosion, the ForceField's rings
	/// and stars); else in the √ space with the ParticleEmitters (Smoke, Sparkles and the ForceField's
	/// shield: legacy-smoke-cover, legacy-sparkles-panel, legacy-forcefield-panel).
	pub linear: bool,
	/// The spin turns the texture within the quad, which stays upright.
	pub spin_texture: bool,
}

impl System {
	fn new(name: &'static str, texture: &'static str) -> Self {
		Self {
			name,
			texture,
			rate: 0.0,
			count: 0,
			lifetime: (1.0, 1.0),
			speed: (0.0, 0.0),
			spread: 0.0,
			radius: 0.0,
			spawn: Vec3::ZERO,
			offset: 0.0,
			rise: 0.0,
			drag: 0.0,
			size: vec![(0.0, 1.0), (1.0, 1.0)],
			squash: 0.0,
			transparency: vec![(0.0, 0.0), (1.0, 0.0)],
			brightness: 1.0,
			light_emission: 0.0,
			light_influence: 0.0,
			rotation: (0.0, 360.0),
			rot_speed: (0.0, 0.0),
			outwards: false,
			sphere: false,
			colors: Vec::new(),
			premultiplied: false,
			alpha_ramp: (0.0, 0.0),
			linear: true,
			spin_texture: false,
		}
	}
}

/// `ROR_TUNE_LEGACY="fire.flame.rate=50,..."` overrides a system's numbers (keys `<class>.<system>.
/// <field>`: rate, count, lifetime (both ends), lifetime0, lifetime1, speed, speed0, speed1, spread,
/// radius, spawnx, spawny, spawnz, offset, rise, drag, brightness, emission, influence, size (scales
/// every key), start and grow (a two-key size's first value and growth), squash, opacity (scales
/// every key's opacity), spin, premultiplied, threshold, width, linear); the calibration fits read
/// through it.
fn tune(class: &str, mut system: System) -> System {
	let Ok(list) = std::env::var("ROR_TUNE_LEGACY") else {
		return system;
	};
	let prefix = format!("{class}.{}.", system.name);
	for pair in list.split(',') {
		let Some((key, value)) = pair.split_once('=') else {
			continue;
		};
		let Some(field) = key.trim().strip_prefix(prefix.as_str()) else {
			continue;
		};
		let Ok(value) = value.trim().parse::<f32>() else {
			continue;
		};
		match field {
			"rate" => system.rate = value,
			"count" => system.count = value.max(0.0) as u32,
			"lifetime" => system.lifetime = (value, value),
			"lifetime0" => system.lifetime.0 = value,
			"lifetime1" => system.lifetime.1 = value,
			"speed" => system.speed = (value, value),
			"speed0" => system.speed.0 = value,
			"speed1" => system.speed.1 = value,
			"spread" => system.spread = value,
			"radius" => system.radius = value,
			"rise" => system.rise = value,
			"drag" => system.drag = value,
			"brightness" => system.brightness = value,
			"emission" => system.light_emission = value,
			"influence" => system.light_influence = value,
			"size" => system.size.iter_mut().for_each(|key| key.1 *= value),
			"opacity" => system.transparency.iter_mut().for_each(|key| key.1 = 1.0 - (1.0 - key.1) * value),
			"spin" => system.rot_speed = (-value, value),
			"premultiplied" => system.premultiplied = value > 0.5,
			"threshold" => system.alpha_ramp.0 = value,
			"width" => system.alpha_ramp.1 = value,
			"linear" => system.linear = value > 0.5,
			"squash" => system.squash = value,
			"spawnx" => system.spawn.x = value,
			"spawny" => system.spawn.y = value,
			"spawnz" => system.spawn.z = value,
			"offset" => system.offset = value,
			"grow" => {
				let first = system.size.first().map_or(0.0, |key| key.1);
				system.size = vec![(0.0, first), (1.0, first + value)];
			}
			"start" => {
				let growth = system.size.last().map_or(0.0, |key| key.1) - system.size.first().map_or(0.0, |key| key.1);
				system.size = vec![(0.0, value), (1.0, value + growth)];
			}
			_ => {}
		}
	}
	system
}

/// Fire's systems at Size 5, Heat 9 (legacy-fire-*): a flame of Color rising from the emitter, which
/// covers what is behind it where its texture is opaque (legacy-fire-cover: a black fire hides a
/// white Neon panel), and wisps of SecondaryColor rising above it, adding their light over it; in
/// linear light, their texture's colour premultiplied by its alpha.
fn fire_systems() -> Vec<System> {
	// legacy-fire-flame: one particle a frame, gone by 1.2 s; each one's light-weighted spread (its
	// size) and light over age, the opacity reaching its plateau by 0.3 s; legacy-fire-steady and
	// legacy-fire-cover: the steady flame's light, spread and coverage (sizes 0.75 of the single
	// particles': the opaque union of many)
	let flame = System {
		rate: 60.0,
		lifetime: (1.2, 1.2),
		speed: (0.97, 0.97),
		spread: 5.0,
		rise: 2.49,
		size: vec![(0.0, 1.07), (0.135, 1.07), (0.215, 0.95), (0.31, 0.86), (0.384, 0.80), (0.4625, 0.72), (0.635, 0.53), (0.808, 0.38), (0.9675, 0.20), (1.0, 0.17)],
		transparency: vec![(0.0, 1.0), (0.042, 0.854), (0.083, 0.525), (0.125, 0.30), (0.167, 0.17), (0.215, 0.03), (0.25, 0.0), (0.31, 0.02), (0.384, 0.11), (0.4625, 0.16), (0.635, 0.21), (0.808, 0.52), (0.9675, 0.83), (1.0, 1.0)],
		brightness: 18.0,
		premultiplied: true,
		..System::new("flame", "rbxasset://textures/particles/fire_main.dds")
	};
	// legacy-fire-secondary: faint and additive (legacy-fire-cover: white ones glow over a black
	// flame, black ones hide nothing), fading in over their first half second, rising as the flame
	let secondary = System {
		rate: 30.0,
		lifetime: (1.15, 1.15),
		speed: (0.89, 0.89),
		spread: 5.0,
		rise: 2.23,
		size: vec![(0.0, 0.65), (1.0, 0.65)],
		transparency: vec![(0.0, 1.0), (0.2, 0.925), (0.35, 0.91), (0.6, 0.955), (1.0, 1.0)],
		brightness: 4.0,
		light_emission: 1.0,
		premultiplied: true,
		..System::new("secondary", "rbxasset://textures/particles/fire_main.dds")
	};
	vec![flame, secondary]
}

/// Sparkles' systems (legacy-sparkles-mean): stars of SparkleColor thrown outwards from the
/// emitter, mostly across its up axis (from above the cloud is round, 2.1 studs of spread; from
/// the front 1.4 tall, centred 1.5 studs up), slowing and rising, adding their light in the √ space
/// as ParticleEmitters do (legacy-sparkles-panel: over a panel as bright as a daytime sky Studio's
/// add 2.0 to 2.4 times what linear light would; fewer and brighter than they would be in linear
/// light: Studio's frames over black show 76 to 111 stars, the brightest 9 to 11.5); and a few
/// white dots that do not take the colour (black-ev-3: 4 % of a white one's light). The opacities
/// are the square roots of the linear fit's (a star's light goes as its opacity squared).
fn sparkles_systems() -> Vec<System> {
	let stars = System {
		rate: 22.0,
		lifetime: (1.0, 1.6),
		speed: (4.5, 8.0),
		spread: 30.0,
		rise: 8.5,
		drag: 1.5,
		outwards: true,
		size: vec![(0.0, 0.6), (0.3, 1.0), (1.0, 0.8)],
		transparency: vec![(0.0, 0.29), (0.2, 0.0), (0.8, 0.16), (1.0, 1.0)],
		brightness: 0.95,
		light_emission: 1.0,
		rot_speed: (-90.0, 90.0),
		linear: false,
		..System::new("stars", "rbxasset://textures/particles/sparkles_main.dds")
	};
	let dots = System { rate: 4.0, size: vec![(0.0, 0.15), (1.0, 0.15)], brightness: 7.8, ..System::new("dots", "rbxasset://textures/particles/sparkles_main.dds") };
	let dots = System { lifetime: stars.lifetime, speed: stars.speed, spread: stars.spread, rise: stars.rise, drag: stars.drag, outwards: true, transparency: stars.transparency.clone(), light_emission: 1.0, linear: false, ..dots };
	vec![stars, dots]
}

/// Smoke's puffs at RiseVelocity 1 for `size` and `opacity`: white (Color tints them), lit, blending
/// as ParticleEmitters do (legacy-smoke-cover: the √-space blend, inverted per pixel, gives the same
/// coverage for white puffs over a dark wall and black ones over a lit panel; in linear light a black
/// puff would hide 1.9 stud² where it hides 16). One puff (legacy-smoke-single) starts 0.18 studs
/// wider than Size and grows 1.9 studs a second whatever the Size (legacy-smoke-mean: Size 5 and 20
/// within 1-3 % of Studio's spread); its coverage peaks at 0.18 for Opacity 0.5 (the texture's alpha
/// is at most 1/3), 0.52 of that at 0.145 s, 0.88 at 0.445 s and all of it from 0.85 to 3.35 s, then
/// fades linearly to none at 5.3 s (0.78, 0.51 and 0.24 at 3.85, 4.35 and 4.85 s). Seven a second
/// (legacy-smoke-late: the cover built up over 2 to 6 s within 5 %), rising 0.63 studs a second
/// after a quick first 0.3 (legacy-smoke-white: the steady state's light within 0.02 at every height).
fn smoke_system(size: f32, opacity: f32) -> System {
	let alpha = (1.04 * opacity).min(1.0);
	let fade = |share: f32| 1.0 - alpha * share;
	System {
		rate: 7.0,
		lifetime: (5.3, 5.3),
		speed: (2.46, 2.46),
		rise: 3.8,
		drag: 8.66,
		size: vec![(0.0, size + 0.18), (1.0, size + 10.25)],
		transparency: vec![(0.0, 1.0), (0.0274, fade(0.61)), (0.084, fade(0.91)), (0.1594, fade(1.0)), (0.649, fade(1.0)), (1.0, 1.0)],
		light_influence: 1.0,
		linear: false,
		..System::new("puffs", "rbxasset://textures/particles/smoke_main.dds")
	}
}

/// The ForceField's systems (legacy-forcefield-mean, Studio's mean over 24 frames of a shield around
/// an invisible Torso, from the front and above): the shield, forcefield_glow_main's rim-lit blue
/// disc, its colour its light (the rim's G / B 0.25 is the texture's own; its alpha, under 0.06,
/// only marks the disc), steady (2 % frame to frame), as two sprites (the mean rim: 0.40 at 3.2 studs
/// from the centre, none past 4.7; the outer 5.1 studs, the inner 4.4); spinning rings
/// (forcefield_vortex_main, its light at 0.8 of the quad: flattened 4 : 1, wider than tall from
/// every side, their light gathering at the ends 2.3 studs out), white above the middle and pink
/// under it (what the mean adds over the shield: red, green and blue 1 : 0.87 : 1.1 in the upper
/// half, 1 : 0.48 : 0.47 in the lower); and warm stars rising out of the rings (1 : 0.83 : 0.57).
/// Their colours: FORCEFIELD_COLORS.
fn forcefield_systems() -> Vec<System> {
	// its light added in the √ space over the disc its alpha marks (legacy-forcefield-panel: over a
	// panel as bright as a daytime sky the shield adds 3 times what linear light gives)
	let glow = |name: &'static str, size: f32, brightness: f32| System {
		rate: 10.0,
		lifetime: (0.1, 0.1),
		size: vec![(0.0, size), (1.0, size)],
		brightness,
		light_emission: 1.0,
		alpha_ramp: (0.003, 0.002),
		linear: false,
		..System::new(name, "rbxasset://textures/particles/forcefield_glow_main.dds")
	};
	let rings = |name: &'static str, offset: f32| System {
		rate: 10.0,
		lifetime: (1.0, 1.4),
		spawn: Vec3::new(0.15, 0.35, 0.15),
		offset,
		size: vec![(0.0, 1.44), (1.0, 1.44)],
		squash: -1.0,
		transparency: vec![(0.0, 0.0), (0.6, 0.0), (0.85, 1.0), (1.0, 1.0)],
		brightness: 0.18,
		light_emission: 1.0,
		rot_speed: (-180.0, 180.0),
		spin_texture: true,
		..System::new(name, "rbxasset://textures/particles/forcefield_vortex_main.dds")
	};
	let stars = System {
		rate: 8.0,
		lifetime: (1.5, 2.0),
		// from inside the rings (over a character, only those above it show)
		spawn: Vec3::new(0.5, 0.5, 0.5),
		speed: (1.0, 2.0),
		spread: 10.0,
		size: vec![(0.0, 0.2), (1.0, 0.2)],
		transparency: vec![(0.0, 0.3), (0.7, 0.3), (1.0, 1.0)],
		brightness: 1.5,
		light_emission: 1.0,
		..System::new("stars", "rbxasset://textures/particles/sparkles_main.dds")
	};
	vec![glow("glow", 5.1, 0.29), glow("inner", 4.4, 0.12), rings("rings", 0.2), rings("pink", -0.5), stars]
}

/// The colours of `forcefield_systems` (sRGB: the square roots of the measured light's ratios).
const FORCEFIELD_COLORS: [Vec3; 5] = [Vec3::ONE, Vec3::ONE, Vec3::new(0.96, 0.9, 1.0), Vec3::new(1.0, 0.69, 0.68), Vec3::new(1.0, 0.91, 0.75)];

/// An Explosion's systems (whatever its BlastRadius), all sent at once when it goes off
/// (legacy-explosion-live: three explosions' frames at 0.02 to 0.6 s at exposure -3, as they play:
/// frozen with TimeScale 0, Studio draws more than there was, legacy-explosion-ages). A fireball
/// (explosion01_core_main, its colour straight: the centre 26.6 : 6.9 : 2.0 red, green and blue at
/// 0.02 s, its edge 3.2 studs out, 4.6 at 0.155 s and fading), a hollow orange shell after it (its
/// light in a ring 3 to 5 studs out from 0.2 s, peaking 5.7 red, gone by 0.6 s), a bluish halo
/// (0.54 : 0.84 : 1 at 5 studs, out to 10, gone by 0.15 s), debris thrown out in every direction
/// (on screen 4, 7 and 11 studs out at 0.067, 0.155 and 0.3 s: 80 to 110 studs a second slowed by
/// Drag 7.2) and a faint ring rushing out (11 studs at 0.155 s, 14 at 0.2 s).
fn explosion_systems() -> Vec<System> {
	let opacity = |keys: &[(f32, f32)]| keys.iter().map(|&(time, value)| (time, 1.0 - value)).collect::<Vec<_>>();
	// texels under 0.01 alpha drop out (the textures keep 0.001 to 0.004 out to their corners, which at
	// these brightnesses would show each quad's edge; Studio's show none)
	let cutoff = (0.01, 0.99);
	// four overlapping (the centre flat at 0.155 s), drawn over the rest (its dark rim hides the halo)
	let core = System {
		count: 4,
		lifetime: (0.55, 0.55),
		radius: 0.8,
		size: vec![(0.0, 5.0), (0.042, 5.3), (0.12, 6.7), (0.18, 7.8), (0.28, 10.0), (0.55, 11.0), (1.0, 12.0)],
		// the four sprites' cover 1 - (1 - 0.655 o)^4 through the centre's light: 0.97, 0.76, 0.27, 0.12,
		// 0.034 and 0.007 of the first at 0.067, 0.1, 0.155, 0.2, 0.3 and 0.5 s
		transparency: opacity(&[(0.0, 1.0), (0.042, 1.0), (0.12, 0.83), (0.18, 0.45), (0.28, 0.113), (0.36, 0.047), (0.45, 0.025), (0.55, 0.013), (0.73, 0.008), (0.91, 0.003), (1.0, 0.0)]),
		brightness: 21.0,
		alpha_ramp: cutoff,
		..System::new("core", "rbxasset://textures/particles/explosion01_core_main.dds")
	};
	let shell = System {
		count: 1,
		lifetime: (0.65, 0.65),
		size: vec![(0.0, 6.0), (0.31, 8.5), (0.38, 9.6), (0.46, 10.5), (0.62, 11.2), (0.77, 12.5), (1.0, 13.0)],
		transparency: opacity(&[(0.0, 0.0), (0.23, 0.3), (0.31, 1.0), (0.38, 0.6), (0.46, 0.65), (0.62, 0.19), (0.77, 0.06), (0.92, 0.02), (1.0, 0.0)]),
		brightness: 12.9,
		light_emission: 1.0,
		alpha_ramp: cutoff,
		..System::new("shell", "rbxasset://textures/particles/explosion01_shockwave_main.dds")
	};
	let halo = System {
		count: 1,
		lifetime: (0.15, 0.15),
		size: vec![(0.0, 15.0), (1.0, 20.6)],
		transparency: opacity(&[(0.0, 1.0), (0.45, 0.8), (0.67, 0.4), (1.0, 0.0)]),
		brightness: 3.75,
		light_emission: 1.0,
		alpha_ramp: cutoff,
		..System::new("halo", "rbxasset://textures/particles/explosion01_implosion_main.dds")
	};
	let debris = System {
		count: 40,
		lifetime: (0.4, 0.5),
		speed: (80.0, 110.0),
		drag: 7.2,
		sphere: true,
		size: vec![(0.0, 0.7), (1.0, 0.7)],
		transparency: opacity(&[(0.0, 0.0), (0.12, 1.0), (0.5, 0.8), (1.0, 0.0)]),
		brightness: 1.8,
		// adding their light (Studio's show bright over the fireball, never dark)
		light_emission: 1.0,
		alpha_ramp: cutoff,
		..System::new("debris", "rbxasset://textures/particles/explosion01_core_main.dds")
	};
	let wave = System {
		count: 1,
		lifetime: (0.25, 0.25),
		size: vec![(0.0, 10.0), (1.0, 45.0)],
		transparency: opacity(&[(0.0, 0.0), (0.4, 0.0), (0.6, 1.0), (1.0, 0.0)]),
		brightness: 0.25,
		light_emission: 1.0,
		alpha_ramp: cutoff,
		..System::new("wave", "rbxasset://textures/particles/explosion01_shockwave_main.dds")
	};
	// in drawing order (the same centre: back to front as listed)
	vec![halo, wave, debris, shell, core]
}

/// The colours of `explosion_systems` (sRGB; the debris their texture's own, the fireball a little
/// redder: its centre's green and blue 0.24 and 0.07 of its red).
const EXPLOSION_COLORS: [Vec3; 5] = [Vec3::new(0.73, 0.92, 1.0), Vec3::new(1.0, 0.95, 0.92), Vec3::ONE, Vec3::new(1.0, 0.47, 0.24), Vec3::new(1.0, 0.92, 0.8)];

/// Where a ForceField draws (legacy-forcefield-where): about the Torso (R6) or UpperTorso (R15) of
/// the Humanoid model it is in, 0.3 studs under its centre (legacy-forcefield-mean: the shield's
/// centre 0.25 to 0.4 below, in step left and right); else at the world's origin (a ForceField in a
/// model without a Humanoid, or in a part). The torso, or none.
fn forcefield_mount(document: &Document, parent: Ref) -> (Vec3, Ref) {
	let torso = document.dom.get_by_ref(parent).filter(|model| props::is_a(model, "Model")).and_then(|model| {
		let children: Vec<&Instance> = model.children().iter().filter_map(|child| document.dom.get_by_ref(*child)).collect();
		if !children.iter().any(|child| child.class == "Humanoid") {
			return None;
		}
		children.into_iter().find(|child| matches!(child.name.as_str(), "Torso" | "UpperTorso") && props::is_a(child, "BasePart")).map(|torso| torso.referent())
	});
	match torso.and_then(|torso| Some((props::cframe(document.dom.get_by_ref(torso)?, "CFrame")?, torso))) {
		Some((cframe, torso)) => (Vec3::from(cframe.translation) - Vec3::Y * 0.3, torso),
		None => (Vec3::ZERO, Ref::none()),
	}
}

/// The world CFrame an effect parented to `parent` emits from: a part's CFrame or an attachment's
/// (bone's) world CFrame; None elsewhere.
fn emitter_frame(document: &Document, parent: Ref) -> Option<Affine3A> {
	let instance = document.dom.get_by_ref(parent)?;
	if props::is_a(instance, "Attachment") {
		super::beams::world_cframe(document, parent)
	} else if props::is_a(instance, "BasePart") {
		props::cframe(instance, "CFrame")
	} else {
		None
	}
}

fn keys(points: &[(f32, f32)]) -> NumberKeys {
	points.iter().map(|&(time, value)| (time, value, 0.0)).collect()
}

/// Where and how an effect's systems emit: the frame (its up axis the emission direction), what it
/// follows in a playback, its quads `scale` times the reference size, its speed and rise those
/// factors times the reference's, and its switches.
#[derive(Clone, Copy)]
struct Placement {
	frame: Affine3A,
	mount: Ref,
	scale: f32,
	speed: f32,
	rise: f32,
	enabled: bool,
	time_scale: f32,
}

/// A ParticleEmitter drawing `system` of the effect `referent` at `placement` in `color` (sRGB).
fn emitter(document: &Document, referent: Ref, class: &str, system: System, placement: &Placement, color: Vec3) -> ParticleEmitter {
	let system = tune(class, system);
	let path = format!("{}#{}", document.path_of(referent), system.name);
	let Placement { frame, mount, scale, speed, rise, enabled, time_scale } = *placement;
	let up = Vec3::from(frame.matrix3.y_axis).normalize_or(Vec3::Y);
	let frame = frame * Affine3A::from_translation(Vec3::Y * system.offset);
	// outwards: a cylinder about the frame's up axis (a ParticleEmitter's cylinder runs along its
	// part's X axis) of no size, its particles leaving radially
	let frame = if system.outwards { frame * Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2) } else { frame };
	let seed = path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3));
	ParticleEmitter {
		path,
		cframe: frame,
		size: if system.outwards || system.sphere {
			Vec3::splat(1e-3)
		} else if system.spawn != Vec3::ZERO {
			2.0 * system.spawn * scale
		} else {
			Vec3::splat(2.0 * system.radius * scale)
		},
		attachment: system.radius <= 0.0 && system.spawn == Vec3::ZERO && !system.outwards && !system.sphere,
		enabled,
		rate: system.rate,
		lifetime: system.lifetime,
		speed: (system.speed.0 * speed, system.speed.1 * speed),
		spread_angle: Vec2::splat(system.spread),
		acceleration: up * system.rise * rise,
		drag: system.drag,
		velocity_inheritance: 0.0,
		locked_to_part: false,
		emission_direction: 1,
		shape: if system.sphere {
			1
		} else if system.outwards {
			2
		} else {
			0
		},
		shape_in_out: 0,
		shape_style: 0,
		shape_partial: 1.0,
		rotation: system.rotation,
		rot_speed: system.rot_speed,
		size_keys: system.size.iter().map(|&(time, value)| (time, value * scale, 0.0)).collect(),
		squash: keys(&[(0.0, system.squash), (1.0, system.squash)]),
		transparency: keys(&system.transparency),
		color: if system.colors.is_empty() { vec![(0.0, color)] } else { system.colors.iter().map(|&(time, value)| (time, value * color)).collect() },
		light_emission: system.light_emission,
		light_influence: system.light_influence,
		brightness: system.brightness,
		texture: Some(system.texture.to_string()),
		orientation: 0,
		z_offset: 0.0,
		time_scale,
		flipbook_layout: 0,
		flipbook_mode: 0,
		flipbook_framerate: (1.0, 1.0),
		flipbook_start_random: false,
		flipbook_blend_frames: false,
		flipbook_size: (1, 1),
		bursts: Vec::new(),
		referent: Some(referent),
		births: None,
		follow: None,
		seed,
		emit_count: (system.count > 0).then_some(system.count),
		mount: Some(mount),
		cap: f32::MAX,
		linear: system.linear,
		premultiplied: system.premultiplied,
		alpha_ramp: system.alpha_ramp,
		spin_texture: system.spin_texture,
	}
}

/// The ParticleEmitters drawing a Fire, Smoke, Sparkles, Explosion or ForceField instance (none for
/// one Studio does not draw).
pub fn read(document: &Document, referent: Ref, instance: &Instance, warnings: &mut Vec<String>) -> Vec<ParticleEmitter> {
	let class = instance.class.as_str();
	match class {
		"Fire" => {
			let parent = instance.parent();
			let Some(frame) = emitter_frame(document, parent) else {
				warnings.push(format!("{}: Fire outside a part or attachment (not drawn)", document.path_of(referent)));
				return Vec::new();
			};
			// Size scales the particles (legacy-fire-grid: light ∝ Size², spread ∝ Size); Heat their rise,
			// 0.083 studs a second and 0.277 a second² per unit whatever the Size, which adds 0.009 · Size²
			// a second (legacy-fire-grid: Sizes 2, 3, 5 and 10 at Heat -10 to 25)
			let size = props::f32(instance, "Size", 5.0).clamp(2.0, 30.0);
			let heat = props::f32(instance, "Heat", 9.0).clamp(-25.0, 25.0);
			let placement = Placement {
				frame,
				mount: parent,
				scale: size / 5.0,
				speed: (0.083 * heat + 0.009 * size * size) / (0.083 * 9.0 + 0.009 * 25.0),
				rise: heat / 9.0,
				enabled: props::bool(instance, "Enabled", true),
				time_scale: props::f32(instance, "TimeScale", 1.0),
			};
			let colors = [props::color(instance, "Color", Vec3::new(236.0, 139.0, 70.0) / 255.0), props::color(instance, "SecondaryColor", Vec3::new(139.0, 80.0, 55.0) / 255.0)];
			let mut systems = fire_systems();
			// the flame lives shorter and gives less light the more Heat it has for its Size, by u =
			// Heat / Size past the default's 1.8 (legacy-fire-life: steady white flames at Size 2-10 and
			// Heat 9-25 at exposure -3; with the default's 1.2 s ror's plume was up to 1.18 x as tall as
			// Studio's, sigma y, and 1.85 x as bright, at Size 2 and Heat 25; the lifetimes that give
			// Studio's spread follow 1 - 0.05 ln(u / 1.8) of the default's within 1.1 %, and the light
			// left over then exp(0.043 (u - 1.8)) times the default's within 1.5 %)
			let u = (heat / size).max(1.8);
			let flame = &mut systems[0];
			let lifetime = flame.lifetime.0 * (1.0 - 0.05 * (u / 1.8).ln());
			flame.lifetime = (lifetime, lifetime);
			flame.brightness *= (-0.043 * (u - 1.8)).exp();
			systems.into_iter().zip(colors).map(|(system, color)| emitter(document, referent, "fire", system, &placement, color)).collect()
		}
		"Sparkles" => {
			let parent = instance.parent();
			let Some(frame) = emitter_frame(document, parent) else {
				warnings.push(format!("{}: Sparkles outside a part or attachment (not drawn)", document.path_of(referent)));
				return Vec::new();
			};
			let placement = Placement {
				frame,
				mount: parent,
				scale: 1.0,
				speed: 1.0,
				rise: 1.0,
				enabled: props::bool(instance, "Enabled", true),
				time_scale: props::f32(instance, "TimeScale", 1.0),
			};
			// SparkleColor tints the stars; the dots stay white (legacy-sparkles-mean black-ev-3)
			let colors = [props::color(instance, "SparkleColor", Vec3::new(144.0, 25.0, 255.0) / 255.0), Vec3::ONE];
			sparkles_systems().into_iter().zip(colors).map(|(system, color)| emitter(document, referent, "sparkles", system, &placement, color)).collect()
		}
		"Smoke" => {
			let parent = instance.parent();
			let Some(frame) = emitter_frame(document, parent) else {
				warnings.push(format!("{}: Smoke outside a part or attachment (not drawn)", document.path_of(referent)));
				return Vec::new();
			};
			// RiseVelocity scales the rise (legacy-smoke-mean: the light's centre 2.4 studs up at 1, 11.7 at
			// 5, none at 0)
			let rise = props::f32(instance, "RiseVelocity", 1.0).clamp(-25.0, 25.0);
			let placement = Placement {
				frame,
				mount: parent,
				scale: 1.0,
				speed: rise,
				rise,
				enabled: props::bool(instance, "Enabled", true),
				time_scale: props::f32(instance, "TimeScale", 1.0),
			};
			let system = smoke_system(props::f32(instance, "Size", 1.0).clamp(0.1, 100.0), props::f32(instance, "Opacity", 0.5).clamp(0.0, 1.0));
			vec![emitter(document, referent, "smoke", system, &placement, props::color(instance, "Color", Vec3::ONE))]
		}
		"ForceField" => {
			// Visible false draws nothing (legacy-forcefield-look invisible)
			if !props::bool(instance, "Visible", true) {
				return Vec::new();
			}
			let (center, mount) = forcefield_mount(document, instance.parent());
			let placement = Placement { frame: Affine3A::from_translation(center), mount, scale: 1.0, speed: 1.0, rise: 1.0, enabled: true, time_scale: 1.0 };
			forcefield_systems().into_iter().zip(FORCEFIELD_COLORS).map(|(system, color)| emitter(document, referent, "forcefield", system, &placement, color)).collect()
		}
		"Explosion" => {
			// Visible false shows nothing (it still pushes and breaks joints)
			if !props::bool(instance, "Visible", true) {
				return Vec::new();
			}
			// the same whatever the BlastRadius (legacy-explosion-radius1, -radius16: at 1 and 16 the light
			// per stud² within 10 % of 4's, the fireball as wide), where it went off
			let position = props::vec3(instance, "Position", Vec3::ZERO);
			let placement = Placement {
				frame: Affine3A::from_translation(position),
				mount: Ref::none(),
				scale: 1.0,
				speed: 1.0,
				rise: 1.0,
				enabled: false,
				time_scale: props::f32(instance, "TimeScale", 1.0),
			};
			explosion_systems().into_iter().zip(EXPLOSION_COLORS).map(|(system, color)| emitter(document, referent, "explosion", system, &placement, color)).collect()
		}
		_ => Vec::new(),
	}
}
