//! HTTP for assetdelivery, the thumbnails API and the CDN (ureq 3, rustls, no OpenSSL).
//!
//! - Redirects are followed here, not by ureq, so no request header crosses a redirect. The
//!   cookie is sent only on the first hop, and only when that URL's origin is exactly the
//!   assetdelivery origin (`https://assetdelivery.roblox.com` in production).
//! - Errors carry status codes and short transport descriptions, never header values.
//! - 429 / 5xx / transport failures are retried with exponential backoff (`Retry-After` honoured).
//! - Concurrent requests per client are bounded.

use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::cookie::Secret;

/// Largest accepted response body.
const MAX_BODY: u64 = 1 << 30;
const MAX_REDIRECTS: usize = 5;
const MAX_ATTEMPTS: u32 = 5;
const USER_AGENT: &str = "ror-assets/0.1 (Roblox object renderer)";

/// Service origins (scheme://host[:port], no trailing slash).
#[derive(Clone, Debug)]
pub(crate) struct Endpoints {
	pub assetdelivery: String,
	pub thumbnails: String,
	pub universes: String,
	/// Plain http allowed (tests only)
	pub allow_http: bool,
}

impl Endpoints {
	pub(crate) fn production() -> Endpoints {
		Endpoints {
			assetdelivery: "https://assetdelivery.roblox.com".into(),
			thumbnails: "https://thumbnails.roblox.com".into(),
			universes: "https://apis.roblox.com".into(),
			allow_http: false,
		}
	}
}

/// A response.
pub(crate) struct Reply {
	pub status: u16,
	pub body: Vec<u8>,
	/// Host that sent this response (after redirects)
	pub host: String,
	pub retry_after: Option<Duration>,
}

/// A failed request (no response).
#[derive(Debug, Clone)]
pub(crate) enum Failure {
	/// Connection, TLS, timeout, ... with a short description
	Transport(String),
	/// The URL is unusable (not http(s), userinfo, http where https is required)
	BadUrl(&'static str),
}

impl Failure {
	pub(crate) fn describe(&self) -> String {
		match self {
			Failure::Transport(d) => d.clone(),
			Failure::BadUrl(d) => format!("unusable URL ({d})"),
		}
	}
}

/// `scheme://host[:port]` of an http(s) URL, lowercased, default port dropped. `None` for
/// anything else, including URLs with userinfo (`user@host`).
pub(crate) fn origin_of(url: &str) -> Option<String> {
	let (scheme, rest) = url.split_once("://")?;
	let scheme = scheme.to_ascii_lowercase();
	if scheme != "http" && scheme != "https" {
		return None;
	}
	let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
	let authority = &rest[..authority_end];
	if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
		return None;
	}
	let authority = authority.to_ascii_lowercase();
	let (host, port) = match authority.rsplit_once(':') {
		Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h.to_string(), Some(p.parse::<u16>().ok()?)),
		_ => (authority.clone(), None),
	};
	let default = if scheme == "https" { 443 } else { 80 };
	Some(match port {
		Some(p) if p != default => format!("{scheme}://{host}:{p}"),
		_ => format!("{scheme}://{host}"),
	})
}

fn host_of(url: &str) -> String {
	origin_of(url).and_then(|o| o.split_once("://").map(|(_, h)| h.to_string())).unwrap_or_default()
}

/// Resolves a `Location` header against the URL that returned it.
fn resolve_location(base: &str, location: &str) -> Option<String> {
	let location = location.trim();
	if location.contains("://") {
		return Some(location.to_string());
	}
	let origin = origin_of(base)?;
	if let Some(rest) = location.strip_prefix("//") {
		let scheme = origin.split("://").next()?;
		return Some(format!("{scheme}://{rest}"));
	}
	if location.starts_with('/') {
		return Some(format!("{origin}{location}"));
	}
	// Relative to the base path's directory
	let path = base.splitn(4, '/').nth(3).unwrap_or("");
	let path = path.split(['?', '#']).next().unwrap_or("");
	let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
	Some(if dir.is_empty() { format!("{origin}/{location}") } else { format!("{origin}/{dir}/{location}") })
}

/// Counting semaphore bounding concurrent requests.
struct Permits {
	free: Mutex<usize>,
	cond: Condvar,
}

impl Permits {
	fn acquire(&self) -> PermitGuard<'_> {
		let mut free = self.free.lock();
		while *free == 0 {
			self.cond.wait(&mut free);
		}
		*free -= 1;
		PermitGuard(self)
	}
}

struct PermitGuard<'a>(&'a Permits);

impl Drop for PermitGuard<'_> {
	fn drop(&mut self) {
		*self.0.free.lock() += 1;
		self.0.cond.notify_one();
	}
}

pub(crate) struct Client {
	agent: ureq::Agent,
	endpoints: Endpoints,
	permits: Permits,
	/// Extra headers for assetdelivery requests (first hop only)
	assetdelivery_headers: Vec<(String, String)>,
}

impl Client {
	pub(crate) fn new(endpoints: Endpoints, max_concurrent: usize, assetdelivery_headers: &[(String, String)]) -> Client {
		let assetdelivery_headers = assetdelivery_headers
			.iter()
			.filter(|(name, value)| {
				let name_ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
				let value_ok = value.bytes().all(|b| b == b'\t' || (b' '..=b'~').contains(&b));
				let reserved = ["cookie", "authorization", "host", "content-length"].iter().any(|r| name.eq_ignore_ascii_case(r));
				name_ok && value_ok && !reserved
			})
			.cloned()
			.collect();
		let mut builder = ureq::Agent::config_builder()
			.max_redirects(0)
			.http_status_as_error(false)
			.https_only(!endpoints.allow_http)
			.user_agent(USER_AGENT)
			.timeout_connect(Some(Duration::from_secs(15)))
			.timeout_send_request(Some(Duration::from_secs(30)))
			.timeout_recv_response(Some(Duration::from_secs(60)))
			.timeout_recv_body(Some(Duration::from_secs(300)));
		if endpoints.allow_http {
			// Tests talk to 127.0.0.1: never through a proxy from the environment
			builder = builder.proxy(None);
		}
		let agent: ureq::Agent = builder.build().into();
		Client { agent, endpoints, permits: Permits { free: Mutex::new(max_concurrent.max(1)), cond: Condvar::new() }, assetdelivery_headers }
	}

	pub(crate) fn endpoints(&self) -> &Endpoints {
		&self.endpoints
	}

	/// Whether the cookie may be sent to `url`: exactly the assetdelivery origin.
	fn cookie_allowed(&self, url: &str) -> bool {
		match (origin_of(url), origin_of(&self.endpoints.assetdelivery)) {
			(Some(a), Some(b)) => a == b,
			_ => false,
		}
	}

	/// One GET, following redirects without any of the original headers. The cookie goes only on
	/// the first hop and only to the assetdelivery origin.
	pub(crate) fn get(&self, url: &str, cookie: Option<&Secret>, accept: &str) -> Result<Reply, Failure> {
		let mut current = url.to_string();
		for hop in 0..=MAX_REDIRECTS {
			let origin = origin_of(&current).ok_or(Failure::BadUrl("not an http(s) URL"))?;
			if !self.endpoints.allow_http && !origin.starts_with("https://") {
				return Err(Failure::BadUrl("plain http"));
			}

			let mut request = self.agent.get(&current).header("Accept", accept);
			// Only the first hop to the assetdelivery origin carries the cookie and extra headers
			let to_assetdelivery = hop == 0 && self.cookie_allowed(&current);
			if to_assetdelivery {
				for (name, value) in &self.assetdelivery_headers {
					request = request.header(name.as_str(), value.as_str());
				}
				if let Some(secret) = cookie {
					request = request.header("Cookie", format!(".ROBLOSECURITY={}", secret.expose()));
				}
			}

			let _permit = self.permits.acquire();
			let mut response = request.call().map_err(|e| Failure::Transport(describe(&e)))?;
			let status = response.status().as_u16();
			let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);

			if matches!(status, 301 | 302 | 303 | 307 | 308) {
				let Some(location) = header("location") else {
					return Ok(Reply { status, body: Vec::new(), host: host_of(&current), retry_after: None });
				};
				current = resolve_location(&current, &location).ok_or(Failure::BadUrl("redirect target"))?;
				continue;
			}

			let retry_after = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
			let body = response
				.body_mut()
				.with_config()
				.limit(MAX_BODY)
				.read_to_vec()
				.map_err(|e| Failure::Transport(describe(&e)))?;
			return Ok(Reply { status, body, host: host_of(&current), retry_after });
		}
		Err(Failure::Transport("too many redirects".into()))
	}

	/// One POST of a JSON body to the assetdelivery origin, the only place it may go (the cookie with
	/// it), no redirects followed. A 403 carrying `x-csrf-token` is answered once with the token, as
	/// Roblox's web APIs ask of cookie-authenticated POSTs.
	pub(crate) fn post_json(&self, url: &str, body: &[u8], cookie: Option<&Secret>) -> Result<Reply, Failure> {
		if !self.cookie_allowed(url) {
			return Err(Failure::BadUrl("POST only to assetdelivery"));
		}
		let mut csrf: Option<String> = None;
		for _ in 0..2 {
			let mut request = self.agent.post(url).header("Accept", "application/json").header("Content-Type", "application/json");
			for (name, value) in &self.assetdelivery_headers {
				request = request.header(name.as_str(), value.as_str());
			}
			if let Some(secret) = cookie {
				request = request.header("Cookie", format!(".ROBLOSECURITY={}", secret.expose()));
			}
			if let Some(token) = &csrf {
				request = request.header("X-CSRF-TOKEN", token.as_str());
			}
			let _permit = self.permits.acquire();
			let mut response = request.send(body).map_err(|e| Failure::Transport(describe(&e)))?;
			let status = response.status().as_u16();
			let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
			if status == 403
				&& csrf.is_none()
				&& let Some(token) = header("x-csrf-token").filter(|token| !token.is_empty())
			{
				csrf = Some(token);
				continue;
			}
			let retry_after = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
			let body = response.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| Failure::Transport(describe(&e)))?;
			return Ok(Reply { status, body, host: host_of(url), retry_after });
		}
		Err(Failure::Transport("CSRF token refused".into()))
	}

	/// [`Client::post_json`] with [`Client::get_retrying`]'s retries.
	pub(crate) fn post_json_retrying(&self, url: &str, body: &[u8], cookie: Option<&Secret>) -> Result<Reply, Failure> {
		self.retrying(|| self.post_json(url, body, cookie))
	}

	/// [`Client::get`] with retries: 429 and 5xx responses and transport failures are retried with
	/// exponential backoff (0.25 s doubling, `Retry-After` honoured up to 30 s), 5 attempts.
	pub(crate) fn get_retrying(&self, url: &str, cookie: Option<&Secret>, accept: &str) -> Result<Reply, Failure> {
		self.retrying(|| self.get(url, cookie, accept))
	}

	fn retrying(&self, request: impl Fn() -> Result<Reply, Failure>) -> Result<Reply, Failure> {
		let mut attempt = 0u32;
		loop {
			attempt += 1;
			let result = request();
			let retry_after = match &result {
				Ok(reply) if matches!(reply.status, 429 | 500 | 502 | 503 | 504) => Some(reply.retry_after),
				Err(Failure::Transport(_)) => Some(None),
				_ => None,
			};
			let Some(retry_after) = retry_after else {
				return result;
			};
			if attempt >= MAX_ATTEMPTS {
				return result;
			}
			let backoff = Duration::from_millis(250u64 << (attempt - 1).min(6)) + jitter();
			std::thread::sleep(retry_after.map_or(backoff, |d| d.min(Duration::from_secs(30))));
		}
	}
}

fn jitter() -> Duration {
	let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
	Duration::from_millis((nanos % 100) as u64)
}

/// Short description of a transport error: no URLs, no header values.
fn describe(error: &ureq::Error) -> String {
	match error {
		ureq::Error::Timeout(which) => format!("timed out ({which:?})"),
		ureq::Error::Io(e) => format!("I/O error ({:?})", e.kind()),
		ureq::Error::HostNotFound => "host not found".into(),
		ureq::Error::ConnectionFailed => "connection failed".into(),
		ureq::Error::BodyExceedsLimit(limit) => format!("response larger than {limit} bytes"),
		ureq::Error::TooManyRedirects => "too many redirects".into(),
		ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "TLS error".into(),
		ureq::Error::Decompress(kind, _) => format!("{kind} decompression failed"),
		ureq::Error::BodyStalled => "response body stalled".into(),
		ureq::Error::StatusCode(code) => format!("HTTP {code}"),
		_ => "request failed".into(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn origins() {
		assert_eq!(origin_of("https://assetdelivery.roblox.com/v1/assetId/1").as_deref(), Some("https://assetdelivery.roblox.com"));
		assert_eq!(origin_of("HTTPS://AssetDelivery.Roblox.com:443/x").as_deref(), Some("https://assetdelivery.roblox.com"));
		assert_eq!(origin_of("http://127.0.0.1:8080/x?y").as_deref(), Some("http://127.0.0.1:8080"));
		// Lookalikes and tricks are different origins or rejected
		assert_eq!(origin_of("https://assetdelivery.roblox.com.evil.com/").as_deref(), Some("https://assetdelivery.roblox.com.evil.com"));
		assert_eq!(origin_of("https://assetdelivery.roblox.com@evil.com/"), None);
		assert_eq!(origin_of("https://assetdelivery.roblox.com:8443/").as_deref(), Some("https://assetdelivery.roblox.com:8443"));
		assert_eq!(origin_of("ftp://assetdelivery.roblox.com/"), None);
		assert_eq!(origin_of("assetdelivery.roblox.com/"), None);
	}

	#[test]
	fn locations() {
		let base = "https://a.example/one/two?q=1";
		assert_eq!(resolve_location(base, "https://b.example/x").as_deref(), Some("https://b.example/x"));
		assert_eq!(resolve_location(base, "//c.example/y").as_deref(), Some("https://c.example/y"));
		assert_eq!(resolve_location(base, "/z").as_deref(), Some("https://a.example/z"));
		assert_eq!(resolve_location(base, "w").as_deref(), Some("https://a.example/one/w"));
	}

	#[test]
	fn cookie_only_for_the_exact_origin() {
		let client = Client::new(Endpoints::production(), 1, &[]);
		assert!(client.cookie_allowed("https://assetdelivery.roblox.com/v1/assetId/1"));
		assert!(client.cookie_allowed("https://ASSETDELIVERY.roblox.com:443/v1/assetId/1"));
		assert!(!client.cookie_allowed("http://assetdelivery.roblox.com/v1/assetId/1"));
		assert!(!client.cookie_allowed("https://assetdelivery.roblox.com.evil.com/"));
		assert!(!client.cookie_allowed("https://assetdelivery.roblox.com@evil.com/"));
		assert!(!client.cookie_allowed("https://fts.rbxcdn.com/sc5/abc"));
		assert!(!client.cookie_allowed("https://thumbnails.roblox.com/v1/assets"));
		assert!(!client.cookie_allowed("https://www.roblox.com/asset/?id=1"));
	}
}
