//! An Open Cloud API key: a way to download assets that need authentication, narrower than the
//! account cookie (it reaches only what its owner gave it, through one route).
//!
//! Sources, first match wins: env `ROBLOX_ASSET_DELIVERY_API_KEY`, then a line
//! `[export ]ROBLOX_ASSET_DELIVERY_API_KEY=<key>` in the env files the cookie is read from (the files
//! named by `ROR_COOKIE_ENV` and `RUR_COOKIE_ENV`, then `<repo>/.env`: the `cookie` module), in the
//! same line format. A value that is not a usable header value is skipped, as a cookie's is, and
//! diagnosed as `invalid value`, never shown.
//!
//! The key is sent only in the `x-api-key` header, only on the first hop of a GET to
//! `https://apis.roblox.com/asset-delivery-api/v1/assetId/{id}[/version/{n}]` (with the transcode
//! query or none: [`route_allowed`]), and only after the public route refused the asset. No redirect
//! carries it. It is tried before the account (a broker's token, else the cookie), which is tried
//! after it when it is refused too. It is held in [`Secret`] and never logged, printed, cached or put
//! in an error; server text from its route that could echo it is dropped ([`may_echo`]).
//!
//! Child processes ror starts never inherit it ([`crate::hide_api_key`]), but for ror's own render
//! service, which downloads for it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::cookie::{self, Secret};

/// The environment variable, and env file line, holding the key.
pub(crate) const KEY: &str = "ROBLOX_ASSET_DELIVERY_API_KEY";

/// The only origin the key is sent to (tests aside: [`crate::http::Endpoints`]).
pub(crate) const ORIGIN: &str = "https://apis.roblox.com";

/// The route's path below the origin: `/asset-delivery-api` and assetdelivery's own path after it.
pub(crate) const PREFIX: &str = "/asset-delivery-api";

/// The query parameters the route may carry: a transcode's ([`crate::Transcode`]).
const QUERY: [&str; 2] = ["contentRepresentationPriorityList", "doNotFallbackToBaselineRepresentation"];

/// Where the key came from (never the value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeySource {
	Env,
	File(PathBuf),
}

impl KeySource {
	pub(crate) fn label(&self) -> String {
		match self {
			KeySource::Env => format!("env {KEY}"),
			KeySource::File(path) => format!("file {}", path.display()),
		}
	}
}

/// The key of this process's environment and env files, `None` when there is none.
pub(crate) fn load(repo_dir: Option<&Path>) -> Option<(Secret, KeySource)> {
	discover(std::env::var_os(KEY), &cookie::env_files(repo_dir))
}

/// [`load`] over a value of the variable and a list of env files.
pub(crate) fn discover(env: Option<OsString>, files: &[PathBuf]) -> Option<(Secret, KeySource)> {
	if let Some(secret) = env.as_deref().and_then(|value| value.to_str()).and_then(cookie::unquote).and_then(Secret::plain) {
		return Some((secret, KeySource::Env));
	}
	files.iter().find_map(|path| cookie::read_env_file(path).as_deref().and_then(parse_env_file).map(|secret| (secret, KeySource::File(path.clone()))))
}

/// One line per source with its status (`not set`, `missing`, `unreadable`, `no key line`,
/// `invalid value`, `ok`). Never contains a value.
pub(crate) fn diagnose(env: Option<OsString>, files: &[PathBuf]) -> Vec<String> {
	let status = match &env {
		None => "not set",
		Some(value) if value.to_str().and_then(cookie::unquote).and_then(Secret::plain).is_some() => "ok",
		Some(_) => "invalid value",
	};
	let mut lines = vec![format!("API key, env {KEY}: {status}")];
	for path in files {
		let status = if !path.is_file() {
			"missing"
		} else {
			match cookie::read_env_file(path) {
				None => "unreadable",
				Some(text) => match (text.lines().any(|line| line_value(line).is_some()), parse_env_file(&text).is_some()) {
					(_, true) => "ok",
					(true, false) => "invalid value",
					(false, false) => "no key line",
				},
			}
		};
		lines.push(format!("API key, file {}: {status}", path.display()));
	}
	lines
}

/// The value of a `[export ]ROBLOX_ASSET_DELIVERY_API_KEY=...` line (`#` lines are comments).
fn line_value(line: &str) -> Option<&str> {
	let line = line.trim();
	if line.starts_with('#') {
		return None;
	}
	let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
	let (name, value) = line.split_once('=')?;
	(name.trim() == KEY).then_some(value)
}

/// The key from env file text: the first usable line.
pub(crate) fn parse_env_file(text: &str) -> Option<Secret> {
	text.lines().filter_map(line_value).find_map(|value| cookie::unquote(value).and_then(Secret::plain))
}

/// Whether the path and query of a URL (`/asset-delivery-api/v1/assetId/{id}[/version/{n}][?query]`)
/// is the route the key may be sent to: digits for the id and version, no other path, no fragment, and
/// a query of a transcode's parameters alone.
pub(crate) fn route_allowed(path_and_query: &str) -> bool {
	if path_and_query.contains('#') {
		return false;
	}
	let (path, query) = path_and_query.split_once('?').unwrap_or((path_and_query, ""));
	let number = |part: &str| !part.is_empty() && part.len() <= 20 && part.bytes().all(|b| b.is_ascii_digit());
	let path_ok = match path.strip_prefix(PREFIX).map(|rest| rest.split('/').collect::<Vec<_>>()).as_deref() {
		Some(["", "v1", "assetId", id]) => number(id),
		Some(["", "v1", "assetId", id, "version", version]) => number(id) && number(version),
		_ => false,
	};
	let query_ok = query.is_empty() || query.split('&').all(|pair| QUERY.contains(&pair.split_once('=').map_or(pair, |(name, _)| name)));
	path_ok && query_ok
}

/// Whether server text could hold part of `key`: any 12 of its characters in a row. Such text (an
/// error message from the key's route) is never kept, printed or remembered.
pub(crate) fn may_echo(text: &str, key: &str) -> bool {
	const RUN: usize = 12;
	let (text, key) = (text.as_bytes(), key.as_bytes());
	if key.len() < RUN {
		// a key this short: only the whole of it
		return !key.is_empty() && text.windows(key.len()).any(|part| part == key);
	}
	text.windows(RUN).any(|run| key.windows(RUN).any(|part| part == run))
}

#[cfg(test)]
mod tests {
	use super::*;

	const TEST_KEY: &str = "TEST-API-KEY-0123456789abcdefghij";

	fn temp(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("ror-assets-apikey-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	#[test]
	fn the_environment_then_the_env_files() {
		let dir = temp("order");
		let (first, second, missing) = (dir.join("first.env"), dir.join("second.env"), dir.join("missing.env"));
		std::fs::write(&first, "ROBLOX_ASSET_DELIVERY_COOKIE=cookie-only\n").unwrap();
		std::fs::write(&second, format!("# {KEY}=commented\nexport {KEY} = \"{TEST_KEY}\" \n")).unwrap();
		let files = [missing.clone(), first.clone(), second.clone()];

		// the variable wins over every file
		let (secret, source) = discover(Some(OsString::from("FROM-ENV-0123456789")), &files).unwrap();
		assert_eq!((secret.expose(), source), ("FROM-ENV-0123456789", KeySource::Env));
		// then the first file holding a key line, past a missing one and one with a cookie alone
		let (secret, source) = discover(None, &files).unwrap();
		assert_eq!((secret.expose(), source.clone()), (TEST_KEY, KeySource::File(second.clone())));
		assert_eq!(source.label(), format!("file {}", second.display()));
		// an unusable variable is skipped, as a cookie's is
		assert_eq!(discover(Some(OsString::from("two words")), &files).unwrap().1, KeySource::File(second.clone()));
		assert!(discover(None, &files[..2]).is_none());
		// UTF-16 with a BOM (what Windows PowerShell 5 writes)
		let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain(format!("{KEY}={TEST_KEY}\r\n").encode_utf16().flat_map(|unit| unit.to_le_bytes())).collect();
		std::fs::write(&first, utf16).unwrap();
		assert_eq!(discover(None, &files).unwrap().1, KeySource::File(first.clone()));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_bad_key_file_is_diagnosed_without_its_content() {
		let dir = temp("diagnose");
		let marker = "SECRET-MARKER-0123456789";
		let (bad, other, good, missing) = (dir.join("bad.env"), dir.join("other.env"), dir.join("good.env"), dir.join("missing.env"));
		std::fs::write(&bad, format!("{KEY}={marker} {marker}\n")).unwrap();
		std::fs::write(&other, format!("SOMETHING={marker}\n")).unwrap();
		std::fs::write(&good, format!("{KEY}={TEST_KEY}\n")).unwrap();
		let files = [bad.clone(), other.clone(), good.clone(), missing.clone()];
		assert!(discover(None, &files[..2]).is_none());
		let lines = diagnose(Some(OsString::from(format!("{marker};"))), &files);
		let statuses: Vec<&str> = lines.iter().map(|line| line.rsplit(": ").next().unwrap()).collect();
		assert_eq!(statuses, ["invalid value", "invalid value", "no key line", "ok", "missing"], "{lines:?}");
		assert!(lines[1].contains("bad.env"));
		let shown = format!("{lines:?}");
		assert!(!shown.contains(marker) && !shown.contains(TEST_KEY), "{shown}");
		assert_eq!(diagnose(None, &[])[0], format!("API key, env {KEY}: not set"));
		// the value is redacted in debug output
		let (secret, _) = discover(None, &files).unwrap();
		assert!(!format!("{secret:?}").contains(TEST_KEY));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn only_the_asset_delivery_route() {
		for allowed in [
			"/asset-delivery-api/v1/assetId/123",
			"/asset-delivery-api/v1/assetId/123/version/4",
			"/asset-delivery-api/v1/assetId/123?contentRepresentationPriorityList=W3si&doNotFallbackToBaselineRepresentation=true",
			"/asset-delivery-api/v1/assetId/123/version/4?doNotFallbackToBaselineRepresentation=true",
		] {
			assert!(route_allowed(allowed), "{allowed}");
		}
		for refused in [
			"",
			"/",
			"/v1/assetId/123",
			"/asset-delivery-api/v1/assetId/",
			"/asset-delivery-api/v1/assetId/abc",
			"/asset-delivery-api/v1/assetId/123/",
			"/asset-delivery-api/v1/assetId/123/version/",
			"/asset-delivery-api/v1/assetId/123/version/4/x",
			"/asset-delivery-api/v1/assetId/123/../../../universes/v1/places/1/universe",
			"/asset-delivery-api/v1/assetId/%31",
			"/asset-delivery-api/v1/assets/batch",
			"/asset-delivery-api/v2/assetId/123",
			"/ASSET-DELIVERY-API/v1/assetId/123",
			"/asset-delivery-api/v1/assetId/123#x",
			"/asset-delivery-api/v1/assetId/123?id=5",
			"/asset-delivery-api/v1/assetId/123?contentRepresentationPriorityList=a&callback=x",
			"/asset-delivery-api/v1/assetId/123456789012345678901",
			"/cloud/v2/users/1",
		] {
			assert!(!route_allowed(refused), "{refused}");
		}
	}

	#[test]
	fn text_that_could_echo_the_key_is_spotted() {
		assert!(may_echo(&format!("Invalid API key {TEST_KEY}"), TEST_KEY));
		// a cut-off part of it, as a message bounded to 100 characters would hold
		assert!(may_echo(&format!("key: {}", &TEST_KEY[..14]), TEST_KEY));
		assert!(may_echo(&TEST_KEY[TEST_KEY.len() - 12..], TEST_KEY));
		assert!(!may_echo("Asset is not approved for the requester", TEST_KEY));
		assert!(!may_echo("", TEST_KEY));
		assert!(!may_echo(&TEST_KEY[..11], TEST_KEY), "fewer than 12 characters in a row prove nothing");
	}
}
