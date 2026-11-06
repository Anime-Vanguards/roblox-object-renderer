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
	/// An asset on assetdelivery: `rbxassetid://N`, `rbxassetid://N?version=V`, the
	/// `roblox.com/asset/?id=N` URL spellings, assetdelivery URLs, bare numeric ids.
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
		];
		for (text, expected) in cases {
			assert_eq!(ContentId::parse(text).unwrap(), expected, "{text}");
		}
		assert_eq!(asset(5, Some(2)).canonical(), "rbxassetid://5?version=2");
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
