//! Content type sniffing from the first bytes.

/// A short name for the content type: `png`, `jpeg`, `webp`, `gif`, `bmp`, `dds`, `ktx2`,
/// `mesh v4.01` (any `version x.yy` FileMesh), `rbxm`, `rbxmx`, `texturepack`, `ogg`, `mp3`, `wav`,
/// `font`, `json`, `zstd`, `gzip`, `rbx` (a Roblox archive: a ZIP that holds a `manifest.jsonl`),
/// `zip`, `slim` / `slmd` (Roblox binary formats seen in the client cache), or `unknown`. Telling
/// `rbx` from `zip` can take the ZIP's directory at the end of the file, so pass the whole file.
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
	} else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
		if zip_lists_manifest(bytes) { "rbx" } else { "zip" }
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

/// Whether a ZIP holds a member named `manifest.jsonl`, which makes it a Roblox archive. The
/// writers seen so far put it first, where the name is 30 bytes in; the format does not say they
/// must, so the directory at the end of the file is read when another member comes first.
fn zip_lists_manifest(bytes: &[u8]) -> bool {
	const NAME: &[u8] = b"manifest.jsonl";
	let u16_at = |at: usize| bytes.get(at..)?.first_chunk::<2>().map(|raw| usize::from(u16::from_le_bytes(*raw)));
	let u32_at = |at: usize| bytes.get(at..)?.first_chunk::<4>().map(|raw| u64::from(u32::from_le_bytes(*raw)));
	let u64_at = |at: usize| bytes.get(at..)?.first_chunk::<8>().map(|raw| u64::from_le_bytes(*raw));
	let names = |at: usize, length: usize| length == NAME.len() && bytes.get(at..).is_some_and(|rest| rest.starts_with(NAME));
	if u16_at(26).is_some_and(|length| names(30, length)) {
		return true;
	}
	// the end record: 22 bytes and a comment of at most 65,535, so it starts in the last 65,557
	let tail = bytes.len().saturating_sub(22 + 65_535);
	let Some(end) = bytes[tail..].windows(4).rposition(|window| window == b"PK\x05\x06").map(|at| tail + at) else {
		return false;
	};
	let (Some(mut size), Some(mut offset)) = (u32_at(end + 12), u32_at(end + 16)) else {
		return false;
	};
	if size == u64::from(u32::MAX) || offset == u64::from(u32::MAX) {
		// ZIP64: a locator before the end record points at the record that holds the real numbers
		let record = end.checked_sub(20).filter(|&locator| bytes[locator..].starts_with(b"PK\x06\x07")).and_then(|locator| u64_at(locator + 8)).and_then(|at| usize::try_from(at).ok()).filter(|&at| bytes.get(at..).is_some_and(|rest| rest.starts_with(b"PK\x06\x06")));
		let Some((wide_size, wide_offset)) = record.and_then(|at| Some((u64_at(at + 40)?, u64_at(at + 48)?))) else {
			return false;
		};
		(size, offset) = (wide_size, wide_offset);
	}
	let length = bytes.len() as u64;
	let stop = offset.saturating_add(size).min(length) as usize;
	let mut at = offset.min(length) as usize;
	while stop - at >= 46 && bytes[at..].starts_with(b"PK\x01\x02") {
		let (Some(name), Some(extra), Some(comment)) = (u16_at(at + 28), u16_at(at + 30), u16_at(at + 32)) else {
			return false;
		};
		if names(at + 46, name) {
			return true;
		}
		at = (at + 46 + name + extra + comment).min(stop);
	}
	false
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
		// a ZIP's local header: 26 bytes, the first member's name length, 2 more, the name
		let zip = |name: &str| [b"PK\x03\x04".as_slice(), &[0; 22], &(name.len() as u16).to_le_bytes(), &[0; 2], name.as_bytes(), b"{"].concat();
		assert_eq!(sniff_kind(&zip("manifest.jsonl")), "rbx");
		assert_eq!(sniff_kind(&zip("manifest.jsonl.bak")), "zip");
		assert_eq!(sniff_kind(&zip("mimetype")), "zip");
		assert_eq!(sniff_kind(b"PK\x05\x06"), "zip");
		assert_eq!(sniff_kind(b""), "unknown");
	}

	/// A ZIP of empty stored members in this order, with the directory a reader needs; `wide` writes
	/// the ZIP64 records the way a writer does once a size or an offset no longer fits 32 bits.
	fn zip_of(names: &[&str], wide: bool) -> Vec<u8> {
		let mut out = Vec::new();
		let mut directory = Vec::new();
		for name in names {
			let offset = out.len() as u32;
			let length = (name.len() as u16).to_le_bytes();
			out.extend([b"PK\x03\x04".as_slice(), &[0; 22], &length, &[0; 2], name.as_bytes()].concat());
			directory.extend([b"PK\x01\x02".as_slice(), &[0; 24], &length, &[0; 12], &offset.to_le_bytes(), name.as_bytes()].concat());
		}
		let (start, size, count) = (out.len() as u64, directory.len() as u64, names.len() as u64);
		out.extend(&directory);
		if wide {
			let record = out.len() as u64;
			out.extend([b"PK\x06\x06".as_slice(), &44u64.to_le_bytes(), &[0; 12], &count.to_le_bytes(), &count.to_le_bytes(), &size.to_le_bytes(), &start.to_le_bytes()].concat());
			out.extend([b"PK\x06\x07".as_slice(), &[0; 4], &record.to_le_bytes(), &1u32.to_le_bytes()].concat());
			out.extend([b"PK\x05\x06".as_slice(), &[0; 4], &[0xFF; 12], &[0; 2]].concat());
		} else {
			let count = (count as u16).to_le_bytes();
			out.extend([b"PK\x05\x06".as_slice(), &[0; 4], &count, &count, &(size as u32).to_le_bytes(), &(start as u32).to_le_bytes(), &[0; 2]].concat());
		}
		out
	}

	#[test]
	fn an_archive_is_told_from_a_zip_whatever_its_member_order() {
		let digest = "0123456789abcdef0123456789abcdef";
		let object = format!("objects/{digest}");
		for wide in [false, true] {
			assert_eq!(sniff_kind(&zip_of(&["manifest.jsonl", &object], wide)), "rbx");
			assert_eq!(sniff_kind(&zip_of(&[&object, "manifest.jsonl"], wide)), "rbx", "the manifest written last, wide {wide}");
			assert_eq!(sniff_kind(&zip_of(&[&object, "manifest.jsonl.bak", "notes/manifest.jsonl"], wide)), "zip");
		}
		assert_eq!(sniff_kind(&zip_of(&[], false)), "zip");
		// what the archive crate writes
		let scene = [b"<roblox!\x89\xff\x0d\x0a\x1a\x0a".as_slice(), &[0; 18], b"END\0", &[0; 4], &9u32.to_le_bytes(), &[0; 4], b"</roblox>"].concat();
		assert_eq!(sniff_kind(&ror_archive::Builder::model(scene).build().unwrap()), "rbx");
		// a file cut short or damaged reads as something, never a panic; without its end record the
		// directory cannot be found, so it is a plain ZIP
		for wide in [false, true] {
			let late = zip_of(&[&object, "manifest.jsonl"], wide);
			for cut in 0..late.len() {
				let kind = sniff_kind(&late[..cut]);
				assert!(cut > late.len() - 22 || matches!(kind, "zip" | "unknown"), "{cut} bytes read as {kind}");
			}
			for at in 0..late.len() {
				for value in [0x00, 0x7F, 0xFF] {
					let mut damaged = late.clone();
					damaged[at] = value;
					sniff_kind(&damaged);
				}
			}
		}
	}
}
