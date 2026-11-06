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
//! that does not exist yet answers `406` ("being generated"): assetdelivery starts making it.

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

/// The representation's format and version (`FStringTextureTranscode7Version` in Studio's settings).
pub const FORMAT: &str = "ktx2";
pub const VERSION: &str = "7rdo";

/// Packs a fidelity can name (a 3-bit pack + 1).
pub const MAX_PACKS: u32 = 7;

impl Transcode {
	/// The fidelity of `pack` (0: the smallest levels) of this transcode, base64 as sent.
	pub fn fidelity(self, pack: u32) -> String {
		let next = pack.min(MAX_PACKS - 1) as u8 + 1;
		let layer = match self {
			Transcode::Image => return base64(&[0xff, 0xff, 0xff, 0xfc, next]),
			Transcode::PackColor => 0u8,
			Transcode::PackNormal => 1,
			Transcode::PackMetalnessRoughness => 2,
		};
		base64(&[(layer << 5) | (next >> 2), (next & 3) << 6])
	}

	/// `contentRepresentationPriorityList` naming `pack` of this transcode only.
	pub(crate) fn priority_list(self, pack: u32) -> String {
		let json = format!(r#"[{{"format":"{FORMAT}","majorVersion":"{VERSION}","fidelity":"{}"}}]"#, self.fidelity(pack));
		base64(json.as_bytes())
	}

	/// The cache key of `pack` of this transcode of the content with canonical URI `canonical`: never
	/// the uploaded image's key.
	pub(crate) fn cache_key(self, canonical: &str, pack: u32) -> String {
		format!("{canonical}#{FORMAT}/{VERSION}/{}", self.fidelity(pack))
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
	fn priority_list_is_base64_json() {
		assert_eq!(Transcode::Image.priority_list(0), "W3siZm9ybWF0Ijoia3R4MiIsIm1ham9yVmVyc2lvbiI6IjdyZG8iLCJmaWRlbGl0eSI6Ii8vLy8vQUU9In1d");
		assert_eq!(query_escape("a+b/c="), "a%2Bb%2Fc%3D");
		assert_eq!(base64(b"ab"), "YWI=");
		assert_eq!(base64(b"a"), "YQ==");
		assert_eq!(base64(b""), "");
	}

	#[test]
	fn cache_keys_differ_from_the_image() {
		let key = Transcode::PackColor.cache_key("rbxassetid://5", 1);
		assert_eq!(key, "rbxassetid://5#ktx2/7rdo/AIA=");
		assert_ne!(key, Transcode::PackColor.cache_key("rbxassetid://5", 0));
		assert_ne!(Transcode::Image.cache_key("rbxassetid://5", 0), Transcode::PackColor.cache_key("rbxassetid://5", 0));
	}
}
