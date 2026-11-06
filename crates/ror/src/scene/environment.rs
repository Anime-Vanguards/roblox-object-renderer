//! Lighting, Sky, Atmosphere and post effects, as Roblox property values.
//!
//! A model has no Lighting of its own, so renders use [`Environment::default`] unless a place file
//! (its Lighting service) or an environment file provides one. The default is ror's capture place
//! (`ror studio open`): what parity is measured against.

use glam::Vec3;
use rbx_dom_weak::{Instance, WeakDom};

use super::props;

#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
	/// `Enum.Technology`; Studio no longer exposes it to scripts (`LightingStyle` replaced it), kept
	/// for files that still carry it.
	pub technology: u32,
	/// `Enum.LightingStyle`: 0 Realistic, 1 Soft.
	pub lighting_style: u32,
	pub prioritize_lighting_quality: bool,
	pub ambient: Vec3,
	pub outdoor_ambient: Vec3,
	pub brightness: f32,
	pub clock_time: f32,
	pub geographic_latitude: f32,
	pub color_shift_top: Vec3,
	pub color_shift_bottom: Vec3,
	pub environment_diffuse_scale: f32,
	pub environment_specular_scale: f32,
	pub exposure_compensation: f32,
	pub shadow_softness: f32,
	pub global_shadows: bool,
	pub fog_color: Vec3,
	pub fog_start: f32,
	pub fog_end: f32,
	pub sky: Option<Sky>,
	pub atmosphere: Option<Atmosphere>,
	pub effects: Vec<PostEffect>,
	/// `Workspace.Terrain`'s Clouds, when enabled (not part of Lighting, but a place's look).
	pub clouds: Option<Clouds>,
	/// What a ViewportFrame draws is lit by the frame's own light instead of Lighting's
	/// ([`Environment::viewport_frame`]; notes/render.md "ViewportFrames").
	pub viewport: Option<ViewportLighting>,
}

/// A ViewportFrame's lighting: its `Ambient`, `LightColor` and `LightDirection`, and a Sky child.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportLighting {
	pub ambient: Vec3,
	pub light_color: Vec3,
	/// `LightDirection` as set (any length): the way the light travels.
	pub light_direction: Vec3,
	/// A Sky child of the frame: its six images as the reflections' cubemap, the sky's light on.
	pub sky: Option<Sky>,
}

impl Environment {
	/// What a ViewportFrame draws: one directional light (`LightColor` along `LightDirection`), a flat
	/// `Ambient`, no sky light (EnvironmentDiffuseScale and EnvironmentSpecularScale 0, Roblox's
	/// ViewportFrame reference) unless the frame holds a Sky, no shadows, fog, Atmosphere, clouds or
	/// post effects; the render leaves out its local lights, ambient occlusion and bloom too.
	pub fn viewport_frame(lighting: ViewportLighting) -> Self {
		let environment_scale = if lighting.sky.is_some() { 1.0 } else { 0.0 };
		Self {
			ambient: lighting.ambient,
			outdoor_ambient: Vec3::ZERO,
			brightness: 1.0,
			color_shift_top: Vec3::ZERO,
			color_shift_bottom: Vec3::ZERO,
			environment_diffuse_scale: environment_scale,
			environment_specular_scale: environment_scale,
			exposure_compensation: 0.0,
			global_shadows: false,
			sky: lighting.sky.clone(),
			viewport: Some(lighting),
			..Self::default()
		}
	}
}

impl Default for Environment {
	fn default() -> Self {
		Self {
			technology: 5,
			lighting_style: 0,
			prioritize_lighting_quality: true,
			ambient: Vec3::splat(70.0 / 255.0),
			outdoor_ambient: Vec3::splat(70.0 / 255.0),
			brightness: 2.0,
			clock_time: 14.5,
			geographic_latitude: 0.0,
			color_shift_top: Vec3::ZERO,
			color_shift_bottom: Vec3::ZERO,
			environment_diffuse_scale: 1.0,
			environment_specular_scale: 1.0,
			exposure_compensation: 0.0,
			shadow_softness: 0.2,
			global_shadows: true,
			fog_color: Vec3::splat(0.75),
			fog_start: 0.0,
			fog_end: 100000.0,
			sky: None,
			atmosphere: None,
			effects: Vec::new(),
			clouds: None,
			viewport: None,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clouds {
	pub cover: f32,
	pub density: f32,
	pub color: Vec3,
}

impl Clouds {
	/// A Clouds instance's properties; None when it is disabled.
	pub fn read(instance: &Instance) -> Option<Self> {
		if !props::bool(instance, "Enabled", true) {
			return None;
		}
		Some(Self {
			cover: props::f32(instance, "Cover", 0.5),
			density: props::f32(instance, "Density", 0.7),
			color: props::color(instance, "Color", Vec3::ONE),
		})
	}
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sky {
	/// Back, Down, Front, Left, Right, Up.
	pub skybox: [String; 6],
	pub skybox_orientation: Vec3,
	pub celestial_bodies_shown: bool,
	pub sun_texture: String,
	pub sun_angular_size: f32,
	pub moon_texture: String,
	pub moon_angular_size: f32,
	pub star_count: i32,
}

impl Default for Sky {
	fn default() -> Self {
		Self {
			skybox: [
				"rbxasset://textures/sky/sky512_bk.tex".into(),
				"rbxasset://textures/sky/sky512_dn.tex".into(),
				"rbxasset://textures/sky/sky512_ft.tex".into(),
				"rbxasset://textures/sky/sky512_lf.tex".into(),
				"rbxasset://textures/sky/sky512_rt.tex".into(),
				"rbxasset://textures/sky/sky512_up.tex".into(),
			],
			skybox_orientation: Vec3::ZERO,
			celestial_bodies_shown: true,
			sun_texture: "rbxasset://sky/sun.jpg".into(),
			sun_angular_size: 21.0,
			moon_texture: "rbxasset://sky/moon.jpg".into(),
			moon_angular_size: 11.0,
			star_count: 3000,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Atmosphere {
	pub density: f32,
	pub offset: f32,
	pub color: Vec3,
	pub decay: Vec3,
	pub glare: f32,
	pub haze: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PostEffect {
	Bloom { intensity: f32, size: f32, threshold: f32 },
	ColorCorrection { brightness: f32, contrast: f32, saturation: f32, tint: Vec3 },
	/// `TonemapperPreset`: 0 Default, 1 Retro.
	ColorGrading { tonemapper: u32 },
	SunRays { intensity: f32, spread: f32 },
	DepthOfField { far_intensity: f32, focus_distance: f32, in_focus_radius: f32, near_intensity: f32 },
	Blur { size: f32 },
}

impl Environment {
	/// Reads a Lighting instance and its children (Sky, Atmosphere, enabled post effects).
	pub fn from_lighting(dom: &WeakDom, lighting: &Instance) -> Self {
		let fallback = Self::default();
		let mut environment = Self {
			technology: props::enum_value(lighting, "Technology", fallback.technology),
			lighting_style: props::enum_value(lighting, "LightingStyle", fallback.lighting_style),
			prioritize_lighting_quality: props::bool(lighting, "PrioritizeLightingQuality", fallback.prioritize_lighting_quality),
			ambient: props::color(lighting, "Ambient", fallback.ambient),
			outdoor_ambient: props::color(lighting, "OutdoorAmbient", fallback.outdoor_ambient),
			brightness: props::f32(lighting, "Brightness", fallback.brightness),
			clock_time: clock_time(lighting).unwrap_or(fallback.clock_time),
			geographic_latitude: props::f32(lighting, "GeographicLatitude", fallback.geographic_latitude),
			color_shift_top: props::color(lighting, "ColorShift_Top", fallback.color_shift_top),
			color_shift_bottom: props::color(lighting, "ColorShift_Bottom", fallback.color_shift_bottom),
			environment_diffuse_scale: props::f32(lighting, "EnvironmentDiffuseScale", fallback.environment_diffuse_scale),
			environment_specular_scale: props::f32(lighting, "EnvironmentSpecularScale", fallback.environment_specular_scale),
			exposure_compensation: props::f32(lighting, "ExposureCompensation", fallback.exposure_compensation),
			shadow_softness: props::f32(lighting, "ShadowSoftness", fallback.shadow_softness),
			global_shadows: props::bool(lighting, "GlobalShadows", fallback.global_shadows),
			fog_color: props::color(lighting, "FogColor", fallback.fog_color),
			fog_start: props::f32(lighting, "FogStart", fallback.fog_start),
			fog_end: props::f32(lighting, "FogEnd", fallback.fog_end),
			sky: None,
			atmosphere: None,
			effects: Vec::new(),
			clouds: None,
			viewport: None,
		};
		for &child in lighting.children() {
			let Some(child) = dom.get_by_ref(child) else {
				continue;
			};
			environment.apply_child(child);
		}
		environment
	}

	/// The Lighting properties this environment reads from ([`Environment::from_lighting`] of a
	/// Lighting holding them gives it back, but for its children): what a model's Lighting holds when
	/// a playback changes ror's default one.
	pub fn lighting_properties(&self) -> Vec<(&'static str, rbx_dom_weak::types::Variant)> {
		use rbx_dom_weak::types::{Color3, Enum, Variant};
		let color = |value: Vec3| Variant::Color3(Color3::new(value.x, value.y, value.z));
		vec![
			("Technology", Enum::from_u32(self.technology).into()),
			("LightingStyle", Enum::from_u32(self.lighting_style).into()),
			("PrioritizeLightingQuality", self.prioritize_lighting_quality.into()),
			("Ambient", color(self.ambient)),
			("OutdoorAmbient", color(self.outdoor_ambient)),
			("Brightness", self.brightness.into()),
			("ClockTime", self.clock_time.into()),
			("GeographicLatitude", self.geographic_latitude.into()),
			("ColorShift_Top", color(self.color_shift_top)),
			("ColorShift_Bottom", color(self.color_shift_bottom)),
			("EnvironmentDiffuseScale", self.environment_diffuse_scale.into()),
			("EnvironmentSpecularScale", self.environment_specular_scale.into()),
			("ExposureCompensation", self.exposure_compensation.into()),
			("ShadowSoftness", self.shadow_softness.into()),
			("GlobalShadows", self.global_shadows.into()),
			("FogColor", color(self.fog_color)),
			("FogStart", self.fog_start.into()),
			("FogEnd", self.fog_end.into()),
		]
	}

	/// Applies a Lighting child (Sky, Atmosphere, a post effect); other classes are ignored, and so is
	/// one whose `Enabled` is false (a playback's way to keep a Sky or an Atmosphere, which have no
	/// such property in Roblox, out until an effect brings it).
	pub fn apply_child(&mut self, child: &Instance) {
		if !props::bool(child, "Enabled", true) {
			return;
		}
		match child.class.as_str() {
			"Sky" => self.sky = Some(read_sky(child)),
			"Atmosphere" => {
				self.atmosphere = Some(Atmosphere {
					density: props::f32(child, "Density", 0.395),
					offset: props::f32(child, "Offset", 0.0),
					color: props::color(child, "Color", Vec3::new(0.7843, 0.6667, 0.4235)),
					decay: props::color(child, "Decay", Vec3::new(0.3608, 0.2353, 0.0549)),
					glare: props::f32(child, "Glare", 0.0),
					haze: props::f32(child, "Haze", 0.0),
				})
			}
			class => {
				let effect = match class {
					"BloomEffect" => PostEffect::Bloom {
						intensity: props::f32(child, "Intensity", 0.4),
						size: props::f32(child, "Size", 24.0),
						threshold: props::f32(child, "Threshold", 0.95),
					},
					"ColorCorrectionEffect" => PostEffect::ColorCorrection {
						brightness: props::f32(child, "Brightness", 0.0),
						contrast: props::f32(child, "Contrast", 0.0),
						saturation: props::f32(child, "Saturation", 0.0),
						tint: props::color(child, "TintColor", Vec3::ONE),
					},
					"ColorGradingEffect" => PostEffect::ColorGrading { tonemapper: props::enum_value(child, "TonemapperPreset", 0) },
					"SunRaysEffect" => PostEffect::SunRays {
						intensity: props::f32(child, "Intensity", 0.25),
						spread: props::f32(child, "Spread", 1.0),
					},
					"DepthOfFieldEffect" => PostEffect::DepthOfField {
						far_intensity: props::f32(child, "FarIntensity", 0.75),
						focus_distance: props::f32(child, "FocusDistance", 0.05),
						in_focus_radius: props::f32(child, "InFocusRadius", 10.0),
						near_intensity: props::f32(child, "NearIntensity", 0.75),
					},
					"BlurEffect" => PostEffect::Blur { size: props::f32(child, "Size", 24.0) },
					_ => return,
				};
				self.effects.push(effect);
			}
		}
	}
}

/// `ClockTime` if stored, else parsed from `TimeOfDay` ("14:30:00").
fn clock_time(lighting: &Instance) -> Option<f32> {
	if lighting.properties.contains_key(&rbx_dom_weak::ustr("ClockTime")) {
		return Some(props::f32(lighting, "ClockTime", 14.0));
	}
	let text = props::string(lighting, "TimeOfDay")?;
	let mut parts = text.split(':').map(|part| part.trim().parse::<f32>().unwrap_or(0.0));
	let hours = parts.next()?;
	let minutes = parts.next().unwrap_or(0.0);
	let seconds = parts.next().unwrap_or(0.0);
	Some(hours + minutes / 60.0 + seconds / 3600.0)
}

fn read_sky(sky: &Instance) -> Sky {
	let fallback = Sky::default();
	let face = |names: &[&str], index: usize| props::content(sky, names).unwrap_or_else(|| fallback.skybox[index].clone());
	Sky {
		skybox: [
			face(&["SkyboxBackContent", "SkyboxBk"], 0),
			face(&["SkyboxDownContent", "SkyboxDn"], 1),
			face(&["SkyboxFrontContent", "SkyboxFt"], 2),
			face(&["SkyboxLeftContent", "SkyboxLf"], 3),
			face(&["SkyboxRightContent", "SkyboxRt"], 4),
			face(&["SkyboxUpContent", "SkyboxUp"], 5),
		],
		skybox_orientation: props::vec3(sky, "SkyboxOrientation", Vec3::ZERO),
		celestial_bodies_shown: props::bool(sky, "CelestialBodiesShown", true),
		sun_texture: props::content(sky, &["SunTextureContent", "SunTextureId"]).unwrap_or(fallback.sun_texture),
		sun_angular_size: props::f32(sky, "SunAngularSize", 21.0),
		moon_texture: props::content(sky, &["MoonTextureContent", "MoonTextureId"]).unwrap_or(fallback.moon_texture),
		moon_angular_size: props::f32(sky, "MoonAngularSize", 11.0),
		star_count: props::i32(sky, "StarCount", 3000),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_viewport_frame_lights_its_parts_alone() {
		let lighting = ViewportLighting { ambient: Vec3::ONE, light_color: Vec3::ONE, light_direction: Vec3::new(1.0, 1.0, 0.5), sky: None };
		let mut scene = crate::scene::Scene { kept_glass: vec![0], ..Default::default() };
		assert!(scene.has_more_than_parts());
		scene.in_viewport_frame(lighting.clone());
		assert!(!scene.has_more_than_parts(), "what a ViewportFrame does not draw is left out");
		let environment = &scene.environment;
		assert_eq!((environment.environment_diffuse_scale, environment.environment_specular_scale, environment.global_shadows), (0.0, 0.0, false));
		assert_eq!((environment.ambient, environment.outdoor_ambient, environment.brightness), (Vec3::ONE, Vec3::ZERO, 1.0));
		assert!(environment.viewport.is_some() && environment.sky.is_none() && environment.atmosphere.is_none() && environment.effects.is_empty() && environment.clouds.is_none());
		// a Sky child: its images the reflections' cubemap, the sky's light on
		let with_sky = Environment::viewport_frame(ViewportLighting { sky: Some(Sky::default()), ..lighting });
		assert_eq!((with_sky.environment_diffuse_scale, with_sky.environment_specular_scale), (1.0, 1.0));
		assert!(with_sky.sky.is_some());
	}
}
