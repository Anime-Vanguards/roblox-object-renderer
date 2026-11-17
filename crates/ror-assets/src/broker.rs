//! An account broker: a service that holds a Roblox account's session and forwards an allowlist of
//! asset requests for personal bearer tokens, so that no machine running ror holds the cookie.
//!
//! Credentials, first match wins: env `ROBLOX_BROKER_TOKEN` with env `ROBLOX_BROKER_URL`, then the
//! JSON file named by env `ROBLOX_BROKER_CREDENTIALS`, else `~/.config/roblox-broker/credentials.json`
//! (under the user profile on Windows). The file is `{ "url": "https://...", "token": "..." }`; other
//! keys are ignored. Credentials that are found win over every cookie source: the cookie is then
//! never read.
//!
//! A request that would carry the cookie to `https://assetdelivery.roblox.com/<path>?<query>` goes to
//! `<url>/rbx/assetdelivery.roblox.com/<path>?<query>` instead, with `Authorization: Bearer <token>`
//! and no cookie or CSRF token (the broker answers Roblox's CSRF challenge itself). The answer is
//! Roblox's own, unless it carries an `X-Broker-Error` header: then the broker refused, and its JSON
//! body `{ "error", "message" }` says why.
//!
//! The token is the whole credential, so the URL must be https, or http on this machine only
//! (`127.0.0.1`, `localhost`: a tunnel). It is held in [`Secret`] and never logged, printed or put in
//! an error.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::Deserialize;

use crate::cookie::Secret;
use crate::http::origin_of;

const URL_KEY: &str = "ROBLOX_BROKER_URL";
const TOKEN_KEY: &str = "ROBLOX_BROKER_TOKEN";
const FILE_KEY: &str = "ROBLOX_BROKER_CREDENTIALS";

/// A broker to send authenticated requests through.
#[derive(Debug)]
pub(crate) struct Broker {
	/// `scheme://host[:port][/prefix]` without a trailing slash, as [`base_url`] accepts it
	pub base: String,
	pub token: Secret,
}

/// Where broker credentials came from (never a value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BrokerSource {
	Env,
	File(PathBuf),
}

impl BrokerSource {
	pub(crate) fn label(&self) -> String {
		match self {
			BrokerSource::Env => format!("env {TOKEN_KEY}"),
			BrokerSource::File(path) => format!("file {}", path.display()),
		}
	}
}

/// Credentials that were found but cannot be used: where, and why (never a value).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unusable {
	pub source: BrokerSource,
	pub why: &'static str,
}

/// Broker credentials that were found: usable, or not and why.
pub(crate) type Found = Result<(Broker, BrokerSource), Unusable>;

#[derive(Deserialize)]
struct Saved {
	url: Option<String>,
	token: Option<String>,
}

/// The broker credentials of this process's environment, `None` when there are none.
pub(crate) fn load() -> Option<Found> {
	discover(&|name| std::env::var_os(name))
}

/// [`load`] over any environment (`env` answers a variable's value).
pub(crate) fn discover(env: &dyn Fn(&str) -> Option<OsString>) -> Option<Found> {
	if let Some(token) = set(env, TOKEN_KEY) {
		let url = set(env, URL_KEY);
		return Some(usable(url.as_deref().and_then(|url| url.to_str()), token.to_str().unwrap_or(""), BrokerSource::Env));
	}
	let path = credentials_file(env)?;
	let text = std::fs::read_to_string(&path).ok()?;
	let saved: Saved = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
	let token = saved.token.filter(|token| !token.trim().is_empty())?;
	Some(usable(saved.url.as_deref(), &token, BrokerSource::File(path)))
}

/// One line per broker credential source with its status. Never contains a value.
pub(crate) fn diagnose(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<String> {
	let status = |missing: &'static str| match discover(env) {
		Some(Ok(_)) => "ok",
		Some(Err(unusable)) => unusable.why,
		None => missing,
	};
	if set(env, TOKEN_KEY).is_some() {
		return vec![format!("account broker, env {TOKEN_KEY}: {}", status("not set"))];
	}
	let mut lines = vec![format!("account broker, env {TOKEN_KEY}: not set")];
	if let Some(path) = credentials_file(env) {
		let status = if path.is_file() { status("no token") } else { "missing" };
		lines.push(format!("account broker, file {}: {status}", path.display()));
	}
	lines
}

fn set(env: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<OsString> {
	env(name).filter(|value| !value.is_empty())
}

/// The credentials file: the one `ROBLOX_BROKER_CREDENTIALS` names, else the one in the home folder.
fn credentials_file(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
	if let Some(path) = set(env, FILE_KEY) {
		return Some(PathBuf::from(path));
	}
	let home = set(env, if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
	Some(PathBuf::from(home).join(".config").join("roblox-broker").join("credentials.json"))
}

fn usable(url: Option<&str>, token: &str, source: BrokerSource) -> Found {
	let unusable = |why| Unusable { source: source.clone(), why };
	let token = Secret::new(token).ok_or_else(|| unusable("invalid token"))?;
	let url = url.map(str::trim).filter(|url| !url.is_empty()).ok_or_else(|| unusable("a token without a broker URL"))?;
	let base = base_url(url).map_err(unusable)?;
	Ok((Broker { base, token }, source))
}

/// A broker URL as requests are built on it: its origin and path prefix, no trailing slash. Refused
/// unless it is https, or http on `127.0.0.1` or `localhost`.
pub(crate) fn base_url(raw: &str) -> Result<String, &'static str> {
	let raw = raw.trim();
	let origin = origin_of(raw).ok_or("the broker URL is not an http(s) URL")?;
	let (scheme, authority) = origin.split_once("://").ok_or("the broker URL is not an http(s) URL")?;
	let host = match authority.rsplit_once(':') {
		Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
		_ => authority,
	};
	if scheme != "https" && host != "127.0.0.1" && host != "localhost" {
		return Err("the broker URL must be https (plain http only for 127.0.0.1 or localhost)");
	}
	let path = tail_of(raw);
	if !path.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'#' | b'\\')) {
		return Err("the broker URL has a query, a fragment or unusable characters");
	}
	Ok(format!("{origin}{}", path.trim_end_matches('/')))
}

/// What follows the authority of an http(s) URL: path, query and fragment.
fn tail_of(url: &str) -> &str {
	let rest = url.split_once("://").map_or("", |(_, rest)| rest);
	rest.find(['/', '?', '#']).map_or("", |at| &rest[at..])
}

impl Broker {
	/// Where the broker takes a request for `url`: `<base>/rbx/<host>/<path>?<query>`.
	pub(crate) fn forward(&self, url: &str) -> Option<String> {
		let origin = origin_of(url)?;
		let (_, authority) = origin.split_once("://")?;
		let tail = tail_of(url).split('#').next().unwrap_or("");
		let slash = if tail.starts_with('/') { "" } else { "/" };
		Some(format!("{}/rbx/{authority}{slash}{tail}", self.base))
	}

	/// What the broker said when it refused (`X-Broker-Error: <code>`, body `{ "error", "message" }`):
	/// its message, else the code, as printable ASCII of at most 200 characters, without the token
	/// should the broker echo it.
	pub(crate) fn refusal(&self, code: &str, body: &[u8]) -> String {
		#[derive(Deserialize)]
		struct Refusal {
			message: Option<String>,
		}
		let printable = |text: &str| text.replace(self.token.expose(), "<redacted>").chars().filter(|c| (' '..='~').contains(c)).take(200).collect::<String>().trim().to_string();
		let message = serde_json::from_slice::<Refusal>(body).ok().and_then(|refusal| refusal.message).map(|message| printable(&message)).filter(|message| !message.is_empty());
		let said = message.unwrap_or_else(|| printable(code));
		if said.is_empty() { "refused".into() } else { said }
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;
	use std::path::Path;

	use super::*;

	const TOKEN: &str = "test-token-0123456789";
	const HOME: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
	const AT: char = '@';

	fn found(vars: &[(&str, &str)]) -> Option<Found> {
		let vars: HashMap<String, OsString> = vars.iter().map(|(name, value)| (name.to_string(), OsString::from(value))).collect();
		discover(&|name| vars.get(name).cloned())
	}

	fn write(path: &Path, json: &str) {
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(path, json).unwrap();
	}

	#[test]
	fn discovery_order() {
		let dir = std::env::temp_dir().join(format!("ror-assets-broker-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		let named = dir.join("named.json");
		let home = dir.join("home");
		let default = home.join(".config").join("roblox-broker").join("credentials.json");
		let (named_arg, home_arg) = (named.to_string_lossy().into_owned(), home.to_string_lossy().into_owned());
		write(&named, r#"{ "url": "https://named.example/", "token": "named-token", "name": "someone", "scopes": ["read"] }"#);
		write(&default, "\u{feff}{ \"url\": \"https://home.example\", \"token\": \"home-token\" }");
		let all = [("ROBLOX_BROKER_TOKEN", TOKEN), ("ROBLOX_BROKER_URL", "https://env.example"), ("ROBLOX_BROKER_CREDENTIALS", named_arg.as_str()), (HOME, home_arg.as_str())];

		// the environment's pair first
		let (broker, source) = found(&all).unwrap().unwrap();
		assert_eq!((broker.base.as_str(), broker.token.expose(), &source), ("https://env.example", TOKEN, &BrokerSource::Env));
		// then the named file (other keys ignored), a lone ROBLOX_BROKER_URL changing nothing
		let (broker, source) = found(&all[1..]).unwrap().unwrap();
		assert_eq!((broker.base.as_str(), broker.token.expose(), &source), ("https://named.example", "named-token", &BrokerSource::File(named.clone())));
		// then the home folder's
		let (broker, source) = found(&all[3..]).unwrap().unwrap();
		assert_eq!((broker.base.as_str(), broker.token.expose(), &source), ("https://home.example", "home-token", &BrokerSource::File(default.clone())));
		// a named file that is not there does not fall back to the home folder's
		let missing = dir.join("missing.json").to_string_lossy().into_owned();
		assert!(found(&[("ROBLOX_BROKER_CREDENTIALS", missing.as_str()), (HOME, home_arg.as_str())]).is_none());
		assert!(found(&[]).is_none());

		// no token, or no JSON: no broker
		write(&named, r#"{ "url": "https://named.example" }"#);
		assert!(found(&all[2..]).is_none());
		write(&named, "not json");
		assert!(found(&all[2..]).is_none());

		// found but unusable: said so, never silently skipped for a cookie
		let unusable = |found: Option<Found>| found.unwrap().unwrap_err();
		assert_eq!(unusable(found(&[("ROBLOX_BROKER_TOKEN", TOKEN)])), Unusable { source: BrokerSource::Env, why: "a token without a broker URL" });
		assert_eq!(unusable(found(&[("ROBLOX_BROKER_TOKEN", "two words"), ("ROBLOX_BROKER_URL", "https://env.example")])).why, "invalid token");
		assert!(unusable(found(&[("ROBLOX_BROKER_TOKEN", TOKEN), ("ROBLOX_BROKER_URL", "http://env.example")])).why.contains("must be https"));
		write(&named, &format!(r#"{{ "url": "http://named.example", "token": "{TOKEN}" }}"#));
		let refused = unusable(found(&all[2..]));
		assert_eq!(refused.source, BrokerSource::File(named.clone()));
		assert!(refused.why.contains("must be https"));

		// the statuses and the label name the source, never the token
		let vars: HashMap<String, OsString> = all[2..].iter().map(|(name, value)| (name.to_string(), OsString::from(value))).collect();
		let lines = diagnose(&|name| vars.get(name).cloned());
		assert_eq!(lines.len(), 2);
		assert!(lines[1].contains("named.json") && lines[1].contains("must be https"), "{lines:?}");
		assert!(!format!("{lines:?} {refused:?} {}", refused.source.label()).contains(TOKEN));
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn https_or_this_machine() {
		assert_eq!(base_url("https://broker.example").as_deref(), Ok("https://broker.example"));
		assert_eq!(base_url(" HTTPS://Broker.Example:443/ ").as_deref(), Ok("https://broker.example"));
		assert_eq!(base_url("https://broker.example:8443/prefix//").as_deref(), Ok("https://broker.example:8443/prefix"));
		assert_eq!(base_url("http://127.0.0.1:18409").as_deref(), Ok("http://127.0.0.1:18409"));
		assert_eq!(base_url("http://localhost:8080/").as_deref(), Ok("http://localhost:8080"));
		assert_eq!(base_url("http://LOCALHOST").as_deref(), Ok("http://localhost"));
		for refused in [
			"http://broker.example",
			"http://127.0.0.1.example.com",
			"http://localhost.example.com:8080",
			"http://127.0.0.2",
			"http://192.168.1.10:8080",
			"ftp://127.0.0.1",
			"broker.example",
			"https://broker.example/?x=1",
			"https://broker.example/#x",
			"https://broker.example/a b",
			"",
		] {
			assert!(base_url(refused).is_err(), "{refused}");
		}
		// userinfo names no host
		for refused in [format!("http://localhost{AT}broker.example"), format!("https://user{AT}broker.example")] {
			assert!(base_url(&refused).is_err(), "{refused}");
		}
	}

	#[test]
	fn requests_move_behind_the_broker() {
		let broker = Broker { base: "https://broker.example".into(), token: Secret::new(TOKEN).unwrap() };
		assert_eq!(broker.forward("https://assetdelivery.roblox.com/v1/assetId/123").as_deref(), Some("https://broker.example/rbx/assetdelivery.roblox.com/v1/assetId/123"));
		assert_eq!(
			broker.forward("https://AssetDelivery.roblox.com:443/v1/assetId/123/version/4?contentRepresentationPriorityList=abc%3D&doNotFallbackToBaselineRepresentation=true").as_deref(),
			Some("https://broker.example/rbx/assetdelivery.roblox.com/v1/assetId/123/version/4?contentRepresentationPriorityList=abc%3D&doNotFallbackToBaselineRepresentation=true")
		);
		assert_eq!(broker.forward("https://assetdelivery.roblox.com/v1/assets/batch").as_deref(), Some("https://broker.example/rbx/assetdelivery.roblox.com/v1/assets/batch"));
		assert_eq!(broker.forward("https://assetdelivery.roblox.com/v1/asset/?id=5#top").as_deref(), Some("https://broker.example/rbx/assetdelivery.roblox.com/v1/asset/?id=5"));
		assert_eq!(broker.forward(&format!("https://user{AT}assetdelivery.roblox.com/v1/assetId/1")), None);

		let prefixed = Broker { base: base_url("http://127.0.0.1:18409/broker/").unwrap(), token: Secret::new(TOKEN).unwrap() };
		assert_eq!(prefixed.forward("https://assetdelivery.roblox.com/v1/assetId/1").as_deref(), Some("http://127.0.0.1:18409/broker/rbx/assetdelivery.roblox.com/v1/assetId/1"));
		assert!(!format!("{prefixed:?}").contains(TOKEN));
	}

	#[test]
	fn refusals_are_the_brokers_words() {
		let broker = Broker { base: "https://broker.example".into(), token: Secret::new(TOKEN).unwrap() };
		assert_eq!(broker.refusal("account_signed_out", br#"{"error":"account_signed_out","message":"the shared account is signed out"}"#), "the shared account is signed out");
		// no message: the code; nothing at all: still an answer
		assert_eq!(broker.refusal("route_not_allowed", b"<html>"), "route_not_allowed");
		assert_eq!(broker.refusal("rate_limited", br#"{"message":"  "}"#), "rate_limited");
		assert_eq!(broker.refusal("", b""), "refused");
		// printable, bounded, and never the token
		let long = format!(r#"{{"message":"line one\nline two {} {}"}}"#, TOKEN, "x".repeat(400));
		let said = broker.refusal("token_invalid", long.as_bytes());
		assert!(said.starts_with("line oneline two <redacted> xxx"), "{said}");
		assert!(said.len() <= 200 && !said.contains(TOKEN));
	}
}
