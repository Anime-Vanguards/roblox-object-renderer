//! Roblox's KTX2 texture transcodes (notes/image.md "Texture transcodes"): Studio draws image and
//! TexturePack assets from them, BC1 / BC3 with Roblox's own mips, never from the uploaded PNG. They
//! upload as they are (BC on devices with BC, else expanded to RGBA8).
//!
//! - An image asset (a SurfaceAppearance map without a TexturePack, a MeshPart texture, a decal, a
//!   beam or particle texture): its `Image` transcode, the uploaded texels (`colorSpace = Linear`)
//!   with mips box-filtered on them.
//! - A SurfaceAppearance with a TexturePack: the pack's three layers, colour (`Gamma2RGB`: the texels
//!   hold the square root of the linear colour, so the shader squares them), normal (DXT5nm) and
//!   metalness / roughness (red / green of one texture).
//! - The built-in materials' packs and MaterialVariant TexturePacks: the same three layers, the third
//!   with the emissive mask in blue (notes/materials.md "Transcodes").

use anyhow::{Result, anyhow};
use ror_assets::{Transcode, TranscodeTarget};

use super::resources::TextureUse;
use super::{MaterialKey, Renderer};
use crate::scene::{Geometry, Scene};

/// Material texture keys of a TexturePack's layers: `ror-transcode:<layer>:<pack URI>`, a phone's
/// `ror-transcode:<layer>@phone:<pack URI>`.
const LAYER_PREFIX: &str = "ror-transcode:";
const PHONE: &str = "@phone";

/// `ROR_TRANSCODES`: unset, the transcodes a desktop draws; `0` the uploaded images instead (the path
/// before transcodes, for comparisons); `phone` the transcodes a phone draws (ETC2, decoded on the CPU).
fn setting() -> Option<TranscodeTarget> {
	static SETTING: std::sync::OnceLock<Option<TranscodeTarget>> = std::sync::OnceLock::new();
	*SETTING.get_or_init(|| match std::env::var("ROR_TRANSCODES").map(|value| value.trim().to_ascii_lowercase()) {
		Ok(value) if value == "0" => None,
		Ok(value) if value == "phone" => Some(TranscodeTarget::Phone),
		_ => Some(TranscodeTarget::Desktop),
	})
}

/// Whether transcodes are drawn at all (`ROR_TRANSCODES=0`: no).
pub(super) fn enabled() -> bool {
	setting().is_some()
}

/// The GPU formats the transcodes drawn are made for (`ROR_TRANSCODES=phone`: a phone's ETC2), as
/// ror's asset fetching asks for them (`output::asset_config`).
pub fn transcode_target() -> TranscodeTarget {
	setting().unwrap_or_default()
}

fn layer_name(layer: Transcode) -> &'static str {
	match layer {
		Transcode::Image => "image",
		Transcode::PackColor => "color",
		Transcode::PackNormal => "normal",
		Transcode::PackMetalnessRoughness => "mr",
	}
}

/// The material texture key of a TexturePack's layer (a phone's apart: their texture files differ).
pub(super) fn layer_key(layer: Transcode, pack: &str) -> String {
	let phone = if transcode_target() == TranscodeTarget::Phone { PHONE } else { "" };
	format!("{LAYER_PREFIX}{}{phone}:{pack}", layer_name(layer))
}

/// The texture key of an image asset drawn as Studio draws it: its `Image` transcode (the upload
/// when it has none, `Renderer::load_texture`); other content as is. For SurfaceAppearance maps
/// without a TexturePack, MeshPart / SpecialMesh textures, decals, beams and particles: after their
/// captures Roblox's HTTP cache holds the transcode's packs and not the upload (a character's decal,
/// another's TextureID, a beam's and a particle's textures; the face of `texture-decode`'s
/// SurfaceAppearances all five packs). Not for MaterialVariant maps (notes/materials.md
/// "Transcodes") nor clothing templates (their uploads are cached instead).
pub(super) fn image_key(uri: &str) -> String {
	let asset = matches!(ror_assets::ContentId::parse(uri), Ok(ror_assets::ContentId::Asset { .. }));
	if asset && enabled() { layer_key(Transcode::Image, uri) } else { uri.to_string() }
}

/// (layer, pack URI) of a [`layer_key`].
pub(super) fn parse_layer_key(key: &str) -> Option<(Transcode, &str)> {
	let (name, pack) = key.strip_prefix(LAYER_PREFIX)?.split_once(':')?;
	let name = name.strip_suffix(PHONE).unwrap_or(name);
	let layer = [Transcode::Image, Transcode::PackColor, Transcode::PackNormal, Transcode::PackMetalnessRoughness].into_iter().find(|&layer| layer_name(layer) == name)?;
	Some((layer, pack))
}

/// Everything drawing `scene` downloads: meshes and unions, SurfaceAppearance TexturePacks (their
/// description and transcoded layers) or maps, textures, decals, clothing templates, the built-in
/// materials' packs (or the public maps standing in for a private one), particle, beam and trail
/// textures, as [`ror_assets::Assets::warm`] takes them, to fetch ahead of a render. Studio's own
/// content and local files are left out.
pub fn downloads(scene: &Scene) -> Vec<ror_assets::Download> {
	use ror_assets::Download;
	let mut out: Vec<Download> = Vec::new();
	// an image drawn as its transcode (or, without one, its upload)
	let image = |uri: &str, out: &mut Vec<Download>| {
		if matches!(ror_assets::ContentId::parse(uri), Ok(ror_assets::ContentId::Asset { .. })) {
			out.push(if enabled() { Download::Transcode(uri.to_string(), Transcode::Image) } else { Download::Asset(uri.to_string()) });
		}
	};
	for part in &scene.parts {
		match &part.geometry {
			Geometry::MeshPart { mesh, .. } => out.push(Download::Asset(mesh.clone())),
			Geometry::Special { mesh: Some(mesh), .. } => out.push(Download::Asset(mesh.clone())),
			Geometry::Union { asset: Some(asset), .. } => out.push(Download::Asset(asset.clone())),
			_ => {}
		}
		if let Some(appearance) = &part.surface_appearance {
			match appearance.texture_pack.as_ref().filter(|_| enabled()) {
				// the pack's layers are drawn (Renderer::pack_layers)
				Some(pack) => {
					out.push(Download::Asset(pack.clone()));
					for layer in [Transcode::PackColor, Transcode::PackNormal, Transcode::PackMetalnessRoughness] {
						out.push(Download::Transcode(pack.clone(), layer));
					}
				}
				None => {
					for map in [&appearance.color_map, &appearance.normal_map, &appearance.metalness_map, &appearance.roughness_map, &appearance.emissive_mask].into_iter().flatten() {
						image(map, &mut out);
					}
				}
			}
		}
		if let Some(texture) = &part.texture {
			image(texture, &mut out);
		}
		for decal in &part.decals {
			image(&decal.texture, &mut out);
		}
		if let Some(composite) = &part.composite {
			for layer in composite.layers() {
				out.push(Download::Asset(layer.image.clone()));
			}
		}
		// a plain part's built-in material: its pack's description and layers, and the public maps a
		// private pack lists (shared by every scene: fetched once)
		let plain = part.surface_appearance.is_none() && part.texture.is_none() && part.composite.is_none() && !matches!(part.geometry, Geometry::Special { .. });
		if let Some(textures) = super::materials::built_in_textures(part.material, scene.materials.use_2022).filter(|_| plain) {
			let pack = format!("rbxassetid://{}", textures.pack);
			if enabled() {
				for layer in [Transcode::PackColor, Transcode::PackNormal, Transcode::PackMetalnessRoughness] {
					out.push(Download::Transcode(pack.clone(), layer));
				}
			}
			for map in super::materials::public_maps(textures.pack).into_iter().flatten().flatten() {
				out.push(Download::Asset(format!("rbxassetid://{map}")));
			}
			out.push(Download::Asset(pack));
		}
	}
	let effects = scene.particles.iter().map(|emitter| &emitter.texture).chain(scene.beams.iter().map(|beam| &beam.texture)).chain(scene.trails.iter().map(|trail| &trail.texture));
	for texture in effects.flatten() {
		image(texture, &mut out);
	}
	let mut seen = std::collections::HashSet::new();
	out.retain(|download| seen.insert(download.clone()));
	out
}

/// How many packs a transcode's pack 0 lists (0 for a file without a pack table), for
/// [`ror_assets::Assets::warm`].
pub fn transcode_packs(pack0: &[u8]) -> u32 {
	ror_image::decode(pack0).map(|image| ror_image::ktx2_pack_table(&image).len() as u32).unwrap_or(0)
}

impl Renderer {
	/// An asset's transcode as one image with the whole mip chain: pack 0, then the packs it lists
	/// (fetched side by side on threads of their own: this runs inside the texture cache's one-time
	/// load, where a rayon worker could steal a render task waiting on that same load), joined.
	/// Errors when the asset has none (not an asset id, not an image, still being made).
	pub(super) fn transcode_image(&self, uri: &str, transcode: Transcode) -> Result<ror_image::Image> {
		let decode = |pack: u32| -> Result<ror_image::Image> {
			let fetched = self.assets.fetch_transcode(uri, transcode, pack).map_err(|error| anyhow!("{error}"))?;
			ror_image::decode(&fetched.bytes).map_err(|error| anyhow!("{uri}: transcode pack {pack}: {error}"))
		};
		let first = decode(0)?;
		let packs = ror_image::ktx2_pack_table(&first).len() as u32;
		let mut image = if packs == 0 {
			first
		} else {
			let rest = std::thread::scope(|scope| {
				let handles: Vec<_> = (1..packs).map(|pack| scope.spawn(move || decode(pack))).collect();
				handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| Err(anyhow!("{uri}: transcode fetch panicked")))).collect::<Result<Vec<_>>>()
			})?;
			ror_image::join_packs(std::iter::once(first).chain(rest).collect()).map_err(|error| anyhow!("{uri}: transcode: {error}"))?
		};
		// an image transcode is BC3 only for an image with alpha, never a DXT5nm normal map (those come
		// as BC1 RGB): a texture whose blocks are all full red without blue only looks like one
		if transcode == Transcode::Image && image.dxt5nm {
			image.dxt5nm = false;
			image.has_alpha = image.meta("avgAlpha").and_then(|alpha| alpha.trim().parse::<u32>().ok()).is_none_or(|alpha| alpha < 255);
		}
		Ok(image)
	}

	/// A SurfaceAppearance TexturePack's three transcoded layers as material textures (metalness and
	/// roughness both the one layer, roughness read from green), when all of them load. Such a pack
	/// (`<usage>0</usage>`) always has all three: maps it lacks are 4 x 4 constants (a flat normal;
	/// metalness 0, roughness 127 / 255), which Studio draws (a ship close-up within 2 of 255 on
	/// 79.5 % of pixels with them, 77.5 % with the part material's metalness and roughness instead).
	/// None for the built-in materials' kind of pack (usage 3, [`Renderer::material_layers`]), whose
	/// third layer differs.
	pub(super) fn pack_layers(&self, pack: &str) -> Option<MaterialKey> {
		if self.texture_pack(pack).is_ok_and(|xml| xml.usage != 0) {
			return None;
		}
		let color = layer_key(Transcode::PackColor, pack);
		let normal = layer_key(Transcode::PackNormal, pack);
		let mr = layer_key(Transcode::PackMetalnessRoughness, pack);
		let loaded = [(&color, TextureUse::Color), (&normal, TextureUse::Normal), (&mr, TextureUse::Scalar)].into_iter().all(|(key, usage)| self.load_texture(key, usage).is_ok());
		loaded.then(|| MaterialKey { color: Some(color), normal: Some(normal), metalness: Some(mr.clone()), roughness: Some(mr), emissive: None, clamped: false })
	}

	/// A material's pack (a built-in material's: `<usage>` 1 on the pre-2022 classic packs, 3 on the
	/// others; a MaterialVariant's TexturePack) as material textures from its transcoded layers:
	/// colour (gamma 2, with the colour map's alpha), normal (DXT5nm) and the third layer, metalness in
	/// red, roughness in green and the emissive mask in blue (usage 3: CrackedLava's, the emissive map's
	/// texels; the alpha is a height map parts do not show). Only the maps the pack lists: a pack
	/// without a colour map has a white 4 x 4 colour layer, one without a roughness map a green of 127,
	/// where the material's own values apply as with the PNGs. None unless every layer loads.
	pub(super) fn material_layers(&self, pack: &str) -> Option<MaterialKey> {
		let xml = self.texture_pack(pack).ok()?;
		let third = layer_key(Transcode::PackMetalnessRoughness, pack);
		let listed = |map: &Option<String>| map.as_ref().map(|_| third.clone());
		let key = MaterialKey {
			color: xml.color.as_ref().map(|_| layer_key(Transcode::PackColor, pack)),
			normal: xml.normal.as_ref().map(|_| layer_key(Transcode::PackNormal, pack)),
			metalness: listed(&xml.metalness),
			roughness: listed(&xml.roughness),
			emissive: listed(&xml.emissive),
			clamped: false,
		};
		let loaded = key.textures().all(|(uri, usage)| self.load_texture(uri, usage).is_ok());
		loaded.then_some(key)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn layer_keys_round_trip() {
		// a phone's layers' keys (ROR_TRANSCODES=phone) read the same
		assert_eq!(parse_layer_key("ror-transcode:normal@phone:rbxassetid://1"), Some((Transcode::PackNormal, "rbxassetid://1")));
		for layer in [Transcode::Image, Transcode::PackColor, Transcode::PackNormal, Transcode::PackMetalnessRoughness] {
			let key = layer_key(layer, "rbxassetid://123456789");
			assert_eq!(parse_layer_key(&key), Some((layer, "rbxassetid://123456789")));
		}
		assert_eq!(parse_layer_key("rbxassetid://1"), None);
		assert_eq!(parse_layer_key("ror-transcode:other:rbxassetid://1"), None);
		// image keys only for asset ids
		assert_eq!(image_key("rbxasset://textures/face.png"), "rbxasset://textures/face.png");
		if enabled() {
			assert_eq!(parse_layer_key(&image_key("http://www.roblox.com/asset/?id=12224218")), Some((Transcode::Image, "http://www.roblox.com/asset/?id=12224218")));
		}
	}
}
