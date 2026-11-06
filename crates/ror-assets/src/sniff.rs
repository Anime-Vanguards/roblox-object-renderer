//! Content type sniffing from the first bytes.

/// A short name for the content type: `png`, `jpeg`, `webp`, `gif`, `bmp`, `dds`, `ktx2`,
/// `mesh v4.01` (any `version x.yy` FileMesh), `rbxm`, `rbxmx`, `texturepack`, `ogg`, `mp3`, `wav`,
/// `font`, `json`, `zstd`, `gzip`, `slim` / `slmd` (Roblox binary formats seen in the client cache),
/// or `unknown`.
pub fn sniff_kind(bytes: &[u8]) -> &'static str {
	let starts = |prefix: &[u8]| bytes.starts_with(prefix);
	if starts(b"\x89PNG\r\n\x1a\n") {
		"png"
	} else if starts(&[0xff, 0xd8, 0xff]) {
		"jpeg"
	} else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
		"webp"
	} else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
		"wav"
	} else if starts(b"GIF87a") || starts(b"GIF89a") {
		"gif"
	} else if starts(b"BM") && bytes.len() > 26 {
		"bmp"
	} else if starts(b"DDS ") {
		"dds"
	} else if starts(&[0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb]) {
		"ktx2"
	} else if starts(b"version ") {
		mesh_version(bytes)
	} else if starts(b"<roblox!") {
		"rbxm"
	} else if starts(b"<roblox") {
		if bytes.windows(19).take(256).any(|w| w == b"<texturepack_versio") { "texturepack" } else { "rbxmx" }
	} else if starts(b"OggS") {
		"ogg"
	} else if starts(b"ID3") || (bytes.len() > 1 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0) {
		"mp3"
	} else if starts(&[0, 1, 0, 0]) || starts(b"OTTO") || starts(b"true") || starts(b"ttcf") {
		"font"
	} else if starts(&[0x28, 0xb5, 0x2f, 0xfd]) {
		"zstd"
	} else if starts(&[0x1f, 0x8b]) {
		"gzip"
	} else if starts(b"SLIM") {
		"slim"
	} else if starts(b"SLMD") {
		"slmd"
	} else if matches!(bytes.iter().find(|b| !b.is_ascii_whitespace()), Some(b'{') | Some(b'[')) {
		"json"
	} else {
		"unknown"
	}
}

fn mesh_version(bytes: &[u8]) -> &'static str {
	match bytes.get(8..12) {
		Some(b"1.00") => "mesh v1.00",
		Some(b"1.01") => "mesh v1.01",
		Some(b"2.00") => "mesh v2.00",
		Some(b"3.00") => "mesh v3.00",
		Some(b"3.01") => "mesh v3.01",
		Some(b"4.00") => "mesh v4.00",
		Some(b"4.01") => "mesh v4.01",
		Some(b"5.00") => "mesh v5.00",
		Some(b"6.00") => "mesh v6.00",
		Some(b"7.00") => "mesh v7.00",
		_ => "mesh",
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn kinds() {
		assert_eq!(sniff_kind(b"\x89PNG\r\n\x1a\n...."), "png");
		assert_eq!(sniff_kind(b"version 4.01\n..."), "mesh v4.01");
		assert_eq!(sniff_kind(b"version 9.99\n..."), "mesh");
		assert_eq!(sniff_kind(b"<roblox!\x89\xff"), "rbxm");
		assert_eq!(sniff_kind(b"<roblox>\n  <texturepack_version>2</texturepack_version>"), "texturepack");
		assert_eq!(sniff_kind(b"<roblox xmlns:xmime=\"x\" version=\"4\">"), "rbxmx");
		assert_eq!(sniff_kind(b"  {\"a\":1}"), "json");
		assert_eq!(sniff_kind(b""), "unknown");
	}
}
