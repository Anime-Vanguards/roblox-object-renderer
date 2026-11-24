//! Content URI parsing and normalisation.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::AssetError;

/// A parsed content URI.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentId {
	/// `rbxasset://<path>`: a file of the Studio install. The path uses `/` separators, has no
	/// leading slash and no `..` components.
	StudioAsset(String),
	/// An asset on assetdelivery: `rbxassetid://N`, `rbxassetid://N&version=V` (how Roblox pins a
	/// version), `rbxassetid://N?version=V`, the `roblox.com/asset/?id=N` URL spellings, assetdelivery
	/// URLs, bare numeric ids.
	Asset { id: u64, version: Option<u64> },
	/// `rbxthumb://type=…&id=…&w=…&h=…[&filters=circular]`
	Thumbnail(Thumbnail),
	/// A local file (absolute, or relative to the current directory).
	File(PathBuf),
}

/// An `rbxthumb://` thumbnail request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Thumbnail {
	pub kind: ThumbnailKind,
	pub id: u64,
	pub width: u32,
	pub height: u32,
	pub circular: bool,
}

/// Thumbnail types of `rbxthumb://` (the `type=` value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbnailKind {
	Asset,
	Avatar,
	AvatarBust,
	AvatarHeadShot,
	BadgeIcon,
	BundleThumbnail,
	FontFamily,
	/// `id` is a universe id (`DataModel.GameId`)
	GameIcon,
	GamePass,
	/// `id` is a place id (`DataModel.PlaceId`)
	GameThumbnail,
	GroupIcon,
	Outfit,
}

impl ThumbnailKind {
	const ALL: [(ThumbnailKind, &'static str); 12] = [
		(ThumbnailKind::Asset, "Asset"),
		(ThumbnailKind::Avatar, "Avatar"),
		(ThumbnailKind::AvatarBust, "AvatarBust"),
		(ThumbnailKind::AvatarHeadShot, "AvatarHeadShot"),
		(ThumbnailKind::BadgeIcon, "BadgeIcon"),
		(ThumbnailKind::BundleThumbnail, "BundleThumbnail"),
		(ThumbnailKind::FontFamily, "FontFamily"),
		(ThumbnailKind::GameIcon, "GameIcon"),
		(ThumbnailKind::GamePass, "GamePass"),
		(ThumbnailKind::GameThumbnail, "GameThumbnail"),
		(ThumbnailKind::GroupIcon, "GroupIcon"),
		(ThumbnailKind::Outfit, "Outfit"),
	];

	pub fn name(self) -> &'static str {
		Self::ALL.iter().find(|(k, _)| *k == self).map_or("Asset", |(_, n)| n)
	}

	fn parse(text: &str) -> Option<ThumbnailKind> {
		Self::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(text)).map(|(k, _)| *k)
	}
}

impl ContentId {
	/// Parses a content URI. Unknown and empty URIs are [`AssetError::InvalidUri`]; strings that look
	/// like file paths become [`ContentId::File`] (whether or not the file exists).
	pub fn parse(uri: &str) -> Result<ContentId, AssetError> {
		let invalid = || AssetError::InvalidUri(uri.to_string());
		let text = uri.trim();
		if text.is_empty() {
			return Err(invalid());
		}

		if let Some(rest) = strip_prefix_ci(text, "rbxasset://") {
			return studio_path(rest).map(ContentId::StudioAsset).ok_or_else(invalid);
		}
		if let Some(rest) = strip_prefix_ci(text, "rbxassetid://") {
			return parse_assetid(rest).ok_or_else(invalid);
		}
		if let Some(rest) = strip_prefix_ci(text, "rbxthumb://") {
			return parse_thumb(rest).map(ContentId::Thumbnail).ok_or_else(invalid);
		}
		if let Some(rest) = strip_prefix_ci(text, "file://") {
			let decoded = percent_decode(rest);
			// file:///C:/x and file://C:/x
			let path = decoded.strip_prefix('/').filter(|p| p.as_bytes().get(1) == Some(&b':')).unwrap_or(&decoded);
			return if path.is_empty() { Err(invalid()) } else { Ok(ContentId::File(PathBuf::from(path))) };
		}
		if text.bytes().all(|b| b.is_ascii_digit()) {
			return parse_id(text).map(|id| ContentId::Asset { id, version: None }).ok_or_else(invalid);
		}
		if let Some(id) = parse_web_url(text) {
			return id.ok_or_else(invalid);
		}

		// Anything else: a local path
		if looks_like_path(text) || Path::new(text).is_file() {
			return Ok(ContentId::File(PathBuf::from(text)));
		}
		Err(invalid())
	}

	/// Canonical spelling, used as the cache key: `rbxasset://<lowercase path>`,
	/// `rbxassetid://N[?version=V]`, `rbxthumb://type=…&id=…&w=…&h=…[&filters=circular]`,
	/// `file://<absolute path>`.
	pub fn canonical(&self) -> String {
		match self {
			ContentId::StudioAsset(path) => format!("rbxasset://{}", path.to_ascii_lowercase()),
			ContentId::Asset { id, version: None } => format!("rbxassetid://{id}"),
			ContentId::Asset { id, version: Some(v) } => format!("rbxassetid://{id}?version={v}"),
			ContentId::Thumbnail(t) => format!(
				"rbxthumb://type={}&id={}&w={}&h={}{}",
				t.kind.name(),
				t.id,
				t.width,
				t.height,
				if t.circular { "&filters=circular" } else { "" }
			),
			ContentId::File(path) => {
				let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
				format!("file://{}", absolute.display())
			}
		}
	}

	/// True for assets fetched over the network (and kept in the disk cache).
	pub fn is_network(&self) -> bool {
		matches!(self, ContentId::Asset { .. } | ContentId::Thumbnail(_))
	}
}

impl fmt::Display for ContentId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.canonical())
	}
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
	let head = text.get(..prefix.len())?;
	head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

fn parse_id(text: &str) -> Option<u64> {
	if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	text.parse::<u64>().ok().filter(|&id| id != 0)
}

/// Normalised `rbxasset://` path, `None` when empty or escaping the content folders.
fn studio_path(rest: &str) -> Option<String> {
	let rest = rest.split(['?', '#']).next().unwrap_or("");
	let mut parts = Vec::new();
	for part in rest.split(['/', '\\']) {
		match part {
			"" | "." => continue,
			".." => return None,
			p if p.contains(':') => return None,
			p => parts.push(p),
		}
	}
	if parts.is_empty() { None } else { Some(parts.join("/")) }
}

/// `key=value` pairs of a query string (keys lowercased).
fn query_pairs(query: &str) -> Vec<(String, String)> {
	query
		.split('&')
		.filter(|p| !p.is_empty())
		.map(|p| {
			let (k, v) = p.split_once('=').unwrap_or((p, ""));
			(k.trim().to_ascii_lowercase(), percent_decode(v.trim()))
		})
		.collect()
}

fn parse_assetid(rest: &str) -> Option<ContentId> {
	// An `&` before any `?`: how Roblox spells a pinned version. The `?` spelling below never read these.
	if let Some(amp) = rest.find('&')
		&& rest.find('?').is_none_or(|question| amp < question)
	{
		return parse_pinned(&rest[..amp], &rest[amp + 1..]);
	}
	let (id_part, query) = rest.split_once('?').unwrap_or((rest, ""));
	let id = parse_id(id_part.trim_end_matches('/'))?;
	let mut version = None;
	for (k, v) in query_pairs(query) {
		if k == "version" {
			version = Some(v.parse::<u64>().ok()?);
		}
	}
	Some(ContentId::Asset { id, version })
}

/// `rbxassetid://N&version=V[&…]`, the spelling Roblox saves a pinned version in, read the way a Roblox
/// client reads it: the id with spaces around it at most; no `#` anywhere; keys and values
/// percent-decoded (`+` a space) and trimmed. Each `version` key, in any case, whose value is an
/// [`integer`] replaces the one before it, 0 or less meaning the latest version; the first that is
/// not one (empty, `abc`, `4/`, past i64) ends the reading, keeping what was read. A lowercase `id`
/// key holding an id names that asset instead. `assetversionid` names an asset version, which this
/// resolver does not fetch: invalid.
fn parse_pinned(id_part: &str, query: &str) -> Option<ContentId> {
	if query.contains('#') {
		return None;
	}
	let mut id = parse_id(id_part.trim())?;
	let (mut version, mut reading) = (None, true);
	for pair in query.split('&').filter(|p| !p.is_empty()) {
		let (key, value) = pair.split_once('=').map_or((pair, None), |(k, v)| (k, Some(form_decode(v))));
		let key = form_decode(key);
		let (key, value) = (key.trim(), value.as_deref().map(str::trim));
		if key.eq_ignore_ascii_case("assetversionid") {
			return None;
		}
		if key == "id" {
			id = value.and_then(parse_id).unwrap_or(id);
		} else if reading && key.eq_ignore_ascii_case("version") {
			match value.and_then(integer) {
				Some(v) => version = u64::try_from(v).ok().filter(|&v| v > 0),
				None => reading = false,
			}
		}
	}
	Some(ContentId::Asset { id, version })
}

/// A pinned version's integer: an optional sign, then decimal digits (a leading zero is not octal) or
/// `0x` and hex digits, within i64.
fn integer(text: &str) -> Option<i64> {
	let (sign, digits) = match text.strip_prefix('-') {
		Some(rest) => ("-", rest),
		None => ("", text.strip_prefix('+').unwrap_or(text)),
	};
	let (radix, digits) = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
		Some(hex) => (16, hex),
		None => (10, digits),
	};
	if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
		return None;
	}
	i64::from_str_radix(&format!("{sign}{digits}"), radix).ok()
}

/// A query key or value as a form decodes it: `+` is a space, then percent escapes.
fn form_decode(text: &str) -> String {
	percent_decode(&text.replace('+', " "))
}

fn parse_thumb(rest: &str) -> Option<Thumbnail> {
	let mut kind = None;
	let (mut id, mut width, mut height, mut circular) = (None, None, None, false);
	for (k, v) in query_pairs(rest.trim_start_matches('?')) {
		match k.as_str() {
			"type" => kind = ThumbnailKind::parse(&v),
			"id" => id = parse_id(&v),
			"w" => width = v.parse::<u32>().ok(),
			"h" => height = v.parse::<u32>().ok(),
			"filters" => circular = v.eq_ignore_ascii_case("circular"),
			_ => {}
		}
	}
	Some(Thumbnail { kind: kind?, id: id?, width: width?, height: height?, circular })
}

/// `Some(Some(id))` for a Roblox asset URL; `Some(None)` for an http(s) URL that names no asset
/// (another Roblox page, another host); `None` when the text is not a web URL (maybe a path).
fn parse_web_url(text: &str) -> Option<Option<ContentId>> {
	let without_scheme = strip_prefix_ci(text, "https://").or_else(|| strip_prefix_ci(text, "http://"));
	let has_scheme = without_scheme.is_some();
	let rest = without_scheme.unwrap_or(text);
	let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	let host = rest[..authority_end].to_ascii_lowercase();
	let host = host.split(':').next().unwrap_or("");
	let known = matches!(host, "www.roblox.com" | "roblox.com" | "web.roblox.com" | "m.roblox.com" | "assetgame.roblox.com" | "assetdelivery.roblox.com");
	if !known {
		// An unknown http(s) URL is not content we fetch
		return has_scheme.then_some(None);
	}

	let after = &rest[authority_end..];
	let after = after.split('#').next().unwrap_or("");
	let (path, query) = after.split_once('?').unwrap_or((after, ""));
	// repeated slashes count as one: Studio saves union assets as `https://www.roblox.com//asset/?id=N`
	let mut path = path.to_ascii_lowercase();
	while path.contains("//") {
		path = path.replace("//", "/");
	}
	let path = path.trim_end_matches('/');
	let pairs = query_pairs(query);
	let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());

	let version = match get("version") {
		Some(v) => match v.parse::<u64>() {
			Ok(v) => Some(v),
			Err(_) => return Some(None),
		},
		None => None,
	};

	// assetdelivery JSON routes: /v1/assetId/N[/version/V], /v2/assetId/N
	for prefix in ["/v1/assetid/", "/v2/assetid/"] {
		if let Some(tail) = path.strip_prefix(prefix) {
			let mut segments = tail.split('/');
			let id = segments.next().and_then(parse_id);
			let version = match (segments.next(), segments.next()) {
				(Some("version"), Some(v)) => v.parse::<u64>().ok(),
				_ => version,
			};
			return Some(id.map(|id| ContentId::Asset { id, version }));
		}
	}

	if matches!(path, "/asset" | "/asset.ashx" | "/v1/asset" | "/v2/asset") {
		return Some(get("id").and_then(parse_id).map(|id| ContentId::Asset { id, version }));
	}
	Some(None)
}

fn looks_like_path(text: &str) -> bool {
	let bytes = text.as_bytes();
	let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
	let unc = text.starts_with("\\\\");
	let relative = text.starts_with("./") || text.starts_with(".\\") || text.starts_with("../") || text.starts_with("..\\");
	let has_separator = !text.contains("://") && (text.contains('/') || text.contains('\\'));
	drive || unc || relative || has_separator
}

fn percent_decode(text: &str) -> String {
	let bytes = text.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' && i + 2 < bytes.len() {
			let hex = |b: u8| (b as char).to_digit(16);
			if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
				out.push((h * 16 + l) as u8);
				i += 3;
				continue;
			}
		}
		out.push(bytes[i]);
		i += 1;
	}
	String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn asset(id: u64, version: Option<u64>) -> ContentId {
		ContentId::Asset { id, version }
	}

	#[test]
	fn asset_spellings() {
		let cases = [
			("rbxassetid://123", asset(123, None)),
			("  RBXASSETID://123/  ", asset(123, None)),
			("rbxassetid://123?version=4", asset(123, Some(4))),
			("http://www.roblox.com/asset/?id=123", asset(123, None)),
			("https://www.roblox.com/asset/?id=123&version=2", asset(123, Some(2))),
			("http://www.roblox.com/asset?id=123", asset(123, None)),
			("http://www.roblox.com/Asset/?ID=12064107", asset(12064107, None)),
			("www.roblox.com/asset/?id=123", asset(123, None)),
			("roblox.com/asset/?id=123", asset(123, None)),
			("https://assetdelivery.roblox.com/v1/asset/?id=123", asset(123, None)),
			("https://assetdelivery.roblox.com/v1/asset?id=123&version=9", asset(123, Some(9))),
			("assetdelivery.roblox.com/v1/asset/?id=123", asset(123, None)),
			("https://assetdelivery.roblox.com/v1/assetId/123", asset(123, None)),
			("https://assetdelivery.roblox.com/v1/assetId/123/version/5", asset(123, Some(5))),
			("http://assetgame.roblox.com/asset/?id=123", asset(123, None)),
			// how Studio saves a union's Content
			("https://www.roblox.com//asset/?id=123456789", asset(123456789, None)),
			("123", asset(123, None)),
			// `?` before any `&` reads as it always did
			("rbxassetid://123?version=4&foo=bar", asset(123, Some(4))),
			("rbxassetid://123?foo=bar&version=4", asset(123, Some(4))),
			("rbxassetid://123/?version=4&version=5", asset(123, Some(5))),
		];
		for (text, expected) in cases {
			assert_eq!(ContentId::parse(text).unwrap(), expected, "{text}");
		}
		assert_eq!(asset(5, Some(2)).canonical(), "rbxassetid://5?version=2");
	}

	/// `rbxassetid://N&version=V`, how Roblox saves a pinned version, read as a Roblox client loads it
	/// (Studio 0.747's client, 2026-11-24: a mesh and an image whose only version is 1, so a version
	/// read loads at 1 and fails at 2, and a version not read loads the latest).
	#[test]
	fn pinned_version_spellings() {
		let cases = [
			("rbxassetid://123&version=4", asset(123, Some(4))),
			("rbxassetid://123&Version=4", asset(123, Some(4))),
			("rbxassetid://123&VERSION=4", asset(123, Some(4))),
			// other keys before and after it
			("rbxassetid://123&version=4&foo=bar", asset(123, Some(4))),
			("rbxassetid://123&foo=bar&version=4", asset(123, Some(4))),
			("rbxassetid://123&foo&version=4", asset(123, Some(4))),
			("rbxassetid://123&&version=4&", asset(123, Some(4))),
			("rbxassetid://123&amp;version=4", asset(123, None)),
			("rbxassetid://123&=4", asset(123, None)),
			("rbxassetid://123&", asset(123, None)),
			("rbxassetid://123&ID=456", asset(123, None)),
			("rbxassetid://123&id=456&version=4", asset(456, Some(4))),
			("rbxassetid://123& id=456", asset(456, None)),
			("rbxassetid://123&id=abc", asset(123, None)),
			// spaces around the id
			("rbxassetid://123 &version=4", asset(123, Some(4))),
			("rbxassetid:// 123\t&version=4", asset(123, Some(4))),
			("rbxassetid://00123&version=4", asset(123, Some(4))),
			// what reads as the integer
			("rbxassetid://123&version=04", asset(123, Some(4))),
			("rbxassetid://123&version=08", asset(123, Some(8))),
			("rbxassetid://123&version= 4 ", asset(123, Some(4))),
			("rbxassetid://123&version=+4", asset(123, Some(4))),
			("rbxassetid://123&version=4+", asset(123, Some(4))),
			("rbxassetid://123&version=%2B4", asset(123, Some(4))),
			("rbxassetid://123&version=%34", asset(123, Some(4))),
			("rbxassetid://123&ver%73ion=4", asset(123, Some(4))),
			("rbxassetid://123& version =4", asset(123, Some(4))),
			("rbxassetid://123&version=0x4", asset(123, Some(4))),
			("rbxassetid://123&version=+0X4", asset(123, Some(4))),
			("rbxassetid://123&version=9223372036854775807", asset(123, Some(9223372036854775807))),
			// the latest version: no integer, or one of 0 or less, or past i64
			("rbxassetid://123&version=", asset(123, None)),
			("rbxassetid://123&version", asset(123, None)),
			("rbxassetid://123&version=abc", asset(123, None)),
			("rbxassetid://123&version=4abc", asset(123, None)),
			("rbxassetid://123&version=4/", asset(123, None)),
			("rbxassetid://123&version=4.0", asset(123, None)),
			("rbxassetid://123&version=4?x=1", asset(123, None)),
			("rbxassetid://123&version=4=5", asset(123, None)),
			("rbxassetid://123&version=4%23", asset(123, None)),
			("rbxassetid://123&version=0x", asset(123, None)),
			("rbxassetid://123&version=0xg", asset(123, None)),
			("rbxassetid://123&version=0", asset(123, None)),
			("rbxassetid://123&version=-4", asset(123, None)),
			("rbxassetid://123&version=-0x4", asset(123, None)),
			("rbxassetid://123&version=9223372036854775808", asset(123, None)),
			("rbxassetid://123&version=18446744073709551620", asset(123, None)),
			// repeated: each integer replaces the one before; the first that is none ends the reading
			("rbxassetid://123&version=3&version=4", asset(123, Some(4))),
			("rbxassetid://123&version=4&version=3", asset(123, Some(3))),
			("rbxassetid://123&version=4&version=-1", asset(123, None)),
			("rbxassetid://123&version=0&version=4", asset(123, Some(4))),
			("rbxassetid://123&version=4&version=abc&version=3", asset(123, Some(4))),
			("rbxassetid://123&version=4&version&version=3", asset(123, Some(4))),
			("rbxassetid://123&version=4&version=", asset(123, Some(4))),
			("rbxassetid://123&version=abc&version=4", asset(123, None)),
			("rbxassetid://123&version=%20&version=4", asset(123, None)),
			("rbxassetid://123&version=99999999999999999999&version=4", asset(123, None)),
		];
		for (text, expected) in cases {
			assert_eq!(ContentId::parse(text).unwrap(), expected, "{text}");
		}
		// the same asset as the `?` spelling, under one cache key
		for (pinned, question) in [("rbxassetid://5&version=2", "rbxassetid://5?version=2"), ("rbxassetid://5&Version=02&x=y", "rbxassetid://5?version=2")] {
			assert_eq!(ContentId::parse(pinned).unwrap().canonical(), ContentId::parse(question).unwrap().canonical(), "{pinned}");
		}
		assert_eq!(ContentId::parse("rbxassetid://5&version=2").unwrap().canonical(), "rbxassetid://5?version=2");
	}

	#[test]
	fn invalid_uris() {
		for text in [
			"",
			"   ",
			"rbxassetid://",
			"rbxassetid://abc",
			"rbxassetid://0",
			"rbxassetid://99999999999999999999999",
			"rbxassetid://123?version=abc",
			// a pinned version where a Roblox client loads nothing
			"rbxassetid://123/&version=4",
			"rbxassetid:///123&version=4",
			"rbxassetid://123%26version=4",
			"rbxassetid://123&version=4#x",
			"rbxassetid://123&#version=4",
			"rbxassetid://123#&version=4",
			"rbxassetid://&version=4",
			"rbxassetid://0&version=4",
			"rbxassetid://abc&version=4",
			// an asset version id names another asset's version: not fetched
			"rbxassetid://123&assetversionid=5",
			"rbxassetid://123&version=4&AssetVersionId=5",
			"http://www.roblox.com/asset/?id=",
			"http://www.roblox.com/catalog/123",
			"https://example.com/image.png",
			"rbxasset://",
			"rbxasset://../secret.txt",
			"rbxasset://textures/../../x",
			"rbxasset://C:/Windows/win.ini",
			"rbxthumb://type=Asset&id=1",
			"rbxthumb://type=Nope&id=1&w=150&h=150",
			"rbxhttp://Thumbs/Avatar.ashx?x=100",
			"just-words",
		] {
			assert!(matches!(ContentId::parse(text), Err(AssetError::InvalidUri(_))), "{text:?} should be invalid");
		}
	}

	#[test]
	fn studio_paths() {
		assert_eq!(ContentId::parse("rbxasset://textures/studs.dds").unwrap(), ContentId::StudioAsset("textures/studs.dds".into()));
		assert_eq!(ContentId::parse("rbxasset://\\textures\\Sky\\sky512_bk.tex").unwrap(), ContentId::StudioAsset("textures/Sky/sky512_bk.tex".into()));
		assert_eq!(ContentId::parse("RBXASSET://sky//./moon.jpg").unwrap().canonical(), "rbxasset://sky/moon.jpg");
	}

	#[test]
	fn thumbnails() {
		let t = ContentId::parse("rbxthumb://type=AvatarHeadShot&id=1&w=48&h=48&filters=circular").unwrap();
		assert_eq!(
			t,
			ContentId::Thumbnail(Thumbnail { kind: ThumbnailKind::AvatarHeadShot, id: 1, width: 48, height: 48, circular: true })
		);
		assert_eq!(t.canonical(), "rbxthumb://type=AvatarHeadShot&id=1&w=48&h=48&filters=circular");
		let t = ContentId::parse("rbxthumb://type=asset&id=123456&w=150&h=150").unwrap();
		assert_eq!(t.canonical(), "rbxthumb://type=Asset&id=123456&w=150&h=150");
	}

	#[test]
	fn files() {
		assert_eq!(ContentId::parse("C:\\models\\a.png").unwrap(), ContentId::File(PathBuf::from("C:\\models\\a.png")));
		assert_eq!(ContentId::parse("./a.png").unwrap(), ContentId::File(PathBuf::from("./a.png")));
		assert_eq!(ContentId::parse("file:///C:/x%20y/a.png").unwrap(), ContentId::File(PathBuf::from("C:/x y/a.png")));
		// An existing file without separators (cargo runs tests from the package root)
		assert_eq!(ContentId::parse("Cargo.toml").unwrap(), ContentId::File(PathBuf::from("Cargo.toml")));
	}
}
