//! The capture place: a local `.rbxl` that only ror opens in Studio. Every parity capture loads its
//! model and lighting into this place, never into a game place.

use anyhow::{Result, anyhow};
use rbx_dom_weak::types::{Attributes, Color3, Enum, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use crate::scene::Environment;

/// Name of the default capture place, shown in Studio's title bar and the MCP's Studio list.
pub const PLACE_NAME: &str = "ror-capture";

/// The capture place this process uses: `ROR_CAPTURE_PLACE` (e.g. `ror-capture-pre2022` for a second
/// capture Studio running alongside the first), else [`PLACE_NAME`].
pub fn place_name() -> String {
	std::env::var("ROR_CAPTURE_PLACE").ok().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| PLACE_NAME.to_string())
}

/// What identifies the capture Studio in the MCP's list and its window title: the place file's name
/// (`ror-capture.rbxl` does not match `ror-capture-pre2022.rbxl`).
pub fn place_file_name() -> String {
	format!("{}.rbxl", place_name())
}

fn color(value: glam::Vec3) -> Variant {
	Variant::Color3(Color3::new(value.x, value.y, value.z))
}

fn time_of_day(clock_time: f32) -> String {
	let total = (clock_time.rem_euclid(24.0) * 3600.0).round() as u32;
	format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

/// The Lighting service of the capture place for an environment (post effects are added by the
/// capture script, which also replaces these values per capture).
pub fn lighting_builder(environment: &Environment) -> InstanceBuilder {
	InstanceBuilder::new("Lighting")
		.with_name("Lighting")
		.with_property("Technology", Variant::Enum(Enum::from_u32(environment.technology)))
		.with_property("LightingStyle", Variant::Enum(Enum::from_u32(environment.lighting_style)))
		.with_property("PrioritizeLightingQuality", environment.prioritize_lighting_quality)
		.with_property("Ambient", color(environment.ambient))
		.with_property("OutdoorAmbient", color(environment.outdoor_ambient))
		.with_property("Brightness", environment.brightness)
		.with_property("TimeOfDay", time_of_day(environment.clock_time))
		.with_property("GeographicLatitude", environment.geographic_latitude)
		.with_property("ColorShift_Top", color(environment.color_shift_top))
		.with_property("ColorShift_Bottom", color(environment.color_shift_bottom))
		.with_property("EnvironmentDiffuseScale", environment.environment_diffuse_scale)
		.with_property("EnvironmentSpecularScale", environment.environment_specular_scale)
		.with_property("ExposureCompensation", environment.exposure_compensation)
		.with_property("ShadowSoftness", environment.shadow_softness)
		.with_property("GlobalShadows", environment.global_shadows)
		.with_property("FogColor", color(environment.fog_color))
		.with_property("FogStart", environment.fog_start)
		.with_property("FogEnd", environment.fog_end)
}

/// Workspace attribute naming the place's material generation ("2022" / "pre2022"): scripts cannot
/// read `MaterialService.Use2022Materials`, so captures key their cache on this instead.
pub const MATERIALS_ATTRIBUTE: &str = "RorMaterials";

/// A place file with an empty Workspace, the given Lighting, and a MaterialService using the 2022
/// materials (as every new place does) or the pre-2022 ones (places saved without the setting).
pub fn build(environment: &Environment, materials_2022: bool) -> Result<Vec<u8>> {
	build_with(environment, materials_2022, None)
}

/// [`build`], with `Workspace.MeshStreamingAndImprovedLods` set (0 Default, 1 Disabled, 2 Enabled; scripts
/// cannot set it): many games' places have it Disabled, under which only RenderFidelity Performance steps down
/// its chain (notes/cost.md "Levels of detail"). `None` leaves the property out, as before.
pub fn build_with(environment: &Environment, materials_2022: bool, mesh_lods: Option<u32>) -> Result<Vec<u8>> {
	let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
	let root = dom.root_ref();
	let mut attributes = Attributes::new();
	attributes.insert(MATERIALS_ATTRIBUTE.to_string(), Variant::String(if materials_2022 { "2022" } else { "pre2022" }.to_string()));
	let mut workspace = InstanceBuilder::new("Workspace").with_name("Workspace").with_property("Attributes", attributes);
	if let Some(value) = mesh_lods {
		workspace = workspace.with_property("MeshStreamingAndImprovedLods", Variant::Enum(Enum::from_u32(value)));
	}
	let workspace = dom.insert(root, workspace);
	let lighting = dom.insert(root, lighting_builder(environment));
	let materials = dom.insert(root, InstanceBuilder::new("MaterialService").with_name("MaterialService").with_property("Use2022MaterialsXml", materials_2022));
	let mut bytes = Vec::new();
	rbx_binary::to_writer(&mut bytes, &dom, &[workspace, lighting, materials]).map_err(|error| anyhow!("writing the capture place: {error}"))?;
	Ok(bytes)
}
