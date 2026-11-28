//! Roblox's GPU texture transcodes: the `ktx2` content representation (version `7rdo`) of image and
//! TexturePack assets, BC1 / BC3 blocks with Roblox's own mips, split into progressive packs. Roblox
//! clients draw these, never the uploaded PNG (notes/assets.md "Texture transcodes").
//!
//! assetdelivery returns one when the request names it in `contentRepresentationPriorityList` (base64
//! of the JSON `[{"format":"ktx2","majorVersion":"7rdo","fidelity":...}]`) with
//! `doNotFallbackToBaselineRepresentation=true`; the reply then carries `contentRepresentationSpecifier`
//! and the location of one pack file. The fidelity names what the texture is used as and which pack
//! (bit fields read from the first byte's most significant bit):
//!
//! - an image asset (SurfaceAppearance maps without a TexturePack, decals, MeshPart textures):
//!   `FF FF FF FC`, then pack + 1 in the fifth byte (`/////AE=` for pack 0, Studio 0.741's request);
//! - a TexturePack's layer: 3 bits of layer (0 colour, 1 normal, 2 metalness / roughness), 4 option
//!   bits (0), 3 bits of pack + 1, 6 bits of 0: `AEA=` colour pack 0, `IEA=` normal, `QEA=` metalness /
//!   roughness, `AIA=` colour pack 1, `AQA=` colour pack 3.
//!
//! Pack 0 holds the smallest levels and lists every pack (`pack_N = "first level,count"`). A transcode
//! that does not exist yet answers `406` ("being generated"): assetdelivery starts making it, one
//! fidelity at a time (each pack is made when it is first asked for).
//!
//! Phones draw ETC2 transcodes instead ([`TranscodeTarget::Phone`]): an image's fidelity with `0x20`
//! in its fifth byte, a layer's with option `0x08` (notes/assets.md "Phone transcodes").

/// Which transcode of an asset to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transcode {
	/// An image asset as is: BC1 (BC3 with alpha), `colorSpace = Linear` (the uploaded texel values,
	/// mips box-filtered on them).
	Image,
	/// A TexturePack's colour layer: BC1, `colorSpace = Gamma2RGB` (the texels hold the square root of
	/// the linear colour).
	PackColor,
	/// A TexturePack's normal layer: BC3 with X in alpha and Y in green.
	PackNormal,
	/// A TexturePack's metalness (red) and roughness (green) layer: BC1, `Linear`; the built-in
	/// materials' `<usage>` 3 packs add the emissive mask in blue and a height map in alpha (BC3).
	PackMetalnessRoughness,
}

/// Which GPU formats a transcode is made for: a client asks for the ones its GPU takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TranscodeTarget {
	/// BC1 / BC3 (BC3 with alpha; a layer's normal map as DXT5nm): what Studio and the desktop clients
	/// draw.
	#[default]
	Desktop,
	/// ETC2 RGB (vkFormat 147) or RGBA (151, with alpha), every layer included, at the desktop
	/// transcode's sizes: what phones (GPUs without BC formats) draw.
	Phone,
}

/// The fifth byte's bit of an image fidelity asking for ETC2 (`/////CE=` pack 0); `0x10` asks for
/// ETC2 resized to powers of two, which no client was seen asking for.
const IMAGE_ETC2: u8 = 0x20;
/// A layer fidelity's option bit asking for ETC2 (`CEA=` colour pack 0).
const LAYER_ETC2: u8 = 0x08;

/// The representation's format and version (`FStringTextureTranscode7Version` in Studio's settings).
pub const FORMAT: &str = "ktx2";
pub const VERSION: &str = "7rdo";

/// Packs a fidelity can name (a 3-bit pack + 1).
pub const MAX_PACKS: u32 = 7;

impl Transcode {
	/// The fidelity of `pack` (0: the smallest levels) of this transcode for a desktop, base64 as sent.
	pub fn fidelity(self, pack: u32) -> String {
		self.fidelity_for(pack, TranscodeTarget::Desktop)
	}

	/// The fidelity of `pack` of this transcode made for `target`, base64 as sent.
	pub fn fidelity_for(self, pack: u32, target: TranscodeTarget) -> String {
		let next = pack.min(MAX_PACKS - 1) as u8 + 1;
		let phone = target == TranscodeTarget::Phone;
		let layer = match self {
			Transcode::Image => return base64(&[0xff, 0xff, 0xff, 0xfc, next | if phone { IMAGE_ETC2 } else { 0 }]),
			Transcode::PackColor => 0u8,
			Transcode::PackNormal => 1,
			Transcode::PackMetalnessRoughness => 2,
		};
		base64(&[(layer << 5) | if phone { LAYER_ETC2 } else { 0 } | (next >> 2), (next & 3) << 6])
	}

	/// `contentRepresentationPriorityList` naming `pack` of this transcode only.
	pub(crate) fn priority_list(self, pack: u32, target: TranscodeTarget) -> String {
		let json = format!(r#"[{{"format":"{FORMAT}","majorVersion":"{VERSION}","fidelity":"{}"}}]"#, self.fidelity_for(pack, target));
		base64(json.as_bytes())
	}

	/// The cache key of `pack` of this transcode (made for `target`) of the content with canonical URI
	/// `canonical`: never the uploaded image's key.
	pub(crate) fn cache_key(self, canonical: &str, pack: u32, target: TranscodeTarget) -> String {
		format!("{canonical}#{FORMAT}/{VERSION}/{}", self.fidelity_for(pack, target))
	}
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
	const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
	for chunk in bytes.chunks(3) {
		let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
		let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
		for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
			if i <= chunk.len() {
				out.push(ALPHABET[(n >> shift & 63) as usize] as char);
			} else {
				out.push('=');
			}
		}
	}
	out
}

/// Percent-encodes a base64 string for a query parameter.
pub(crate) fn query_escape(text: &str) -> String {
	text.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fidelities_as_studio_sends_them() {
		// Studio 0.741's requests (its logs) and the packs measured against assetdelivery
		assert_eq!(Transcode::Image.fidelity(0), "/////AE=");
		assert_eq!(Transcode::Image.fidelity(1), "/////AI=");
		assert_eq!(Transcode::Image.fidelity(3), "/////AQ=");
		assert_eq!(Transcode::PackColor.fidelity(0), "AEA=");
		assert_eq!(Transcode::PackNormal.fidelity(0), "IEA=");
		assert_eq!(Transcode::PackMetalnessRoughness.fidelity(0), "QEA=");
		assert_eq!(Transcode::PackColor.fidelity(1), "AIA=");
		assert_eq!(Transcode::PackColor.fidelity(2), "AMA=");
		assert_eq!(Transcode::PackColor.fidelity(3), "AQA=");
		assert_eq!(Transcode::PackNormal.fidelity(4), "IUA=");
		assert_eq!(Transcode::PackMetalnessRoughness.fidelity(5), "QYA=");
	}

	#[test]
	fn phone_fidelities_ask_for_etc2() {
		// what assetdelivery had for every texture of three close-up models before this was asked
		// (phones had asked for them), each ETC2: images vkFormat 147 / 151, layers 147
		let phone = TranscodeTarget::Phone;
		assert_eq!(Transcode::Image.fidelity_for(0, phone), "/////CE=");
		assert_eq!(Transcode::Image.fidelity_for(1, phone), "/////CI=");
		assert_eq!(Transcode::PackColor.fidelity_for(0, phone), "CEA=");
		assert_eq!(Transcode::PackNormal.fidelity_for(0, phone), "KEA=");
		assert_eq!(Transcode::PackMetalnessRoughness.fidelity_for(0, phone), "SEA=");
		assert_eq!(Transcode::PackColor.fidelity_for(3, phone), "CQA=");
		assert_eq!(Transcode::Image.fidelity_for(2, TranscodeTarget::Desktop), Transcode::Image.fidelity(2));
		assert_ne!(Transcode::Image.cache_key("rbxassetid://5", 0, phone), Transcode::Image.cache_key("rbxassetid://5", 0, TranscodeTarget::Desktop));
	}

	#[test]
	fn priority_list_is_base64_json() {
		assert_eq!(Transcode::Image.priority_list(0, TranscodeTarget::Desktop), "W3siZm9ybWF0Ijoia3R4MiIsIm1ham9yVmVyc2lvbiI6IjdyZG8iLCJmaWRlbGl0eSI6Ii8vLy8vQUU9In1d");
		assert_eq!(query_escape("a+b/c="), "a%2Bb%2Fc%3D");
		assert_eq!(base64(b"ab"), "YWI=");
		assert_eq!(base64(b"a"), "YQ==");
		assert_eq!(base64(b""), "");
	}

	#[test]
	fn cache_keys_differ_from_the_image() {
		let desktop = TranscodeTarget::Desktop;
		let key = Transcode::PackColor.cache_key("rbxassetid://5", 1, desktop);
		assert_eq!(key, "rbxassetid://5#ktx2/7rdo/AIA=");
		assert_ne!(key, Transcode::PackColor.cache_key("rbxassetid://5", 0, desktop));
		assert_ne!(Transcode::Image.cache_key("rbxassetid://5", 0, desktop), Transcode::PackColor.cache_key("rbxassetid://5", 0, desktop));
	}
}
