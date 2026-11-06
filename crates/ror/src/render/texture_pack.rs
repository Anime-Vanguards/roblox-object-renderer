//! Texture packs: the small XML assets behind `SurfaceAppearance.TexturePack`,
//! `MaterialVariant.TexturePack` and the built-in materials' textures (notes/materials.md).
//!
//! ```xml
//! <roblox>
//!   <texturepack_version>2</texturepack_version>
//!   <alphamode>2</alphamode>
//!   <tiling>1</tiling>
//!   <color>85077012310177</color>
//!   <normal>...</normal>
//!   <roughness>...</roughness>
//! </roblox>
//! ```

use anyhow::{Result, bail};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TexturePack {
	pub color: Option<String>,
	pub normal: Option<String>,
	pub metalness: Option<String>,
	pub roughness: Option<String>,
	pub emissive: Option<String>,
	pub alpha_mode: u32,
	pub tiling: u32,
	/// `<usage>`: 0 on SurfaceAppearance packs, 3 (`<usage version="1">`) on the built-in materials'
	/// (1 on the pre-2022 classic ones). Their third transcoded layers differ: usage 0 and 1 hold
	/// metalness and roughness (BC1, red and green); usage 3 adds the emissive mask in blue and a
	/// height map in alpha (BC3; notes/image.md "Texture transcodes").
	pub usage: u32,
}

/// A tag's text (the tag may carry attributes: `<usage version="1">3</usage>`).
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
	let open = format!("<{name}");
	let mut from = 0;
	let start = loop {
		let at = text[from..].find(&open)? + from + open.len();
		match text[at..].chars().next() {
			Some('>') => break at + 1,
			Some(' ') => break text[at..].find('>')? + at + 1,
			_ => from = at,
		}
	};
	let end = text[start..].find('<')? + start;
	Some(text[start..end].trim()).filter(|value| !value.is_empty())
}

/// A map reference: asset ids become `rbxassetid://` URIs, anything else is kept as written.
fn map(text: &str, name: &str) -> Option<String> {
	let value = tag(text, name)?;
	Some(if value.bytes().all(|byte| byte.is_ascii_digit()) { format!("rbxassetid://{value}") } else { value.to_string() })
}

impl super::resources::Weighed for TexturePack {
	fn bytes(&self) -> u64 {
		256
	}
}

pub fn parse(bytes: &[u8]) -> Result<TexturePack> {
	let text = String::from_utf8_lossy(bytes);
	if !text.contains("<texturepack_version>") {
		bail!("not a texture pack");
	}
	Ok(TexturePack {
		color: map(&text, "color"),
		normal: map(&text, "normal"),
		metalness: map(&text, "metalness"),
		roughness: map(&text, "roughness"),
		emissive: map(&text, "emissive"),
		alpha_mode: tag(&text, "alphamode").and_then(|value| value.parse().ok()).unwrap_or(0),
		tiling: tag(&text, "tiling").and_then(|value| value.parse().ok()).unwrap_or(0),
		usage: tag(&text, "usage").and_then(|value| value.parse().ok()).unwrap_or(0),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_maps_and_fields() {
		let pack = parse(
			b"<roblox>\n  <texturepack_version>2</texturepack_version>\n  <alphamode>2</alphamode>\n  <tiling>1</tiling>\n  <color>85077012310177</color>\n  <normal>123</normal>\n  <roughness>456</roughness>\n</roblox>",
		)
		.unwrap();
		assert_eq!(pack.color.as_deref(), Some("rbxassetid://85077012310177"));
		assert_eq!(pack.normal.as_deref(), Some("rbxassetid://123"));
		assert_eq!(pack.metalness, None);
		assert_eq!(pack.roughness.as_deref(), Some("rbxassetid://456"));
		assert_eq!((pack.alpha_mode, pack.tiling, pack.usage), (2, 1, 0));
		assert!(parse(b"<roblox></roblox>").is_err());
		// a built-in material's pack (Foil's): a usage with an attribute, a normal map only
		let pack = parse(b"<roblox>\n  <texturepack_version>2</texturepack_version>\n  <usage version=\"1\">3</usage>\n  <alphamode>2</alphamode>\n  <tiling>1</tiling>\n  <normal>138455279844761</normal>\n</roblox>").unwrap();
		assert_eq!((pack.usage, pack.alpha_mode, pack.tiling), (3, 2, 1));
		assert_eq!(pack.normal.as_deref(), Some("rbxassetid://138455279844761"));
		assert_eq!(pack.color, None);
	}
}
