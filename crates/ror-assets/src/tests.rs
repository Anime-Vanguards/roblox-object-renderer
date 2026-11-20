//! Resolver tests against local mock HTTP servers (std `TcpListener`), with a fake cookie, broker token
//! and API key. Every test presets the credentials it uses: none is read from the environment.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::apikey::KeySource;
use crate::broker::{Broker, BrokerSource, Unusable};
use crate::cookie::{CookieSource, Secret};
use crate::http::Endpoints;
use crate::{Account, AssetConfig, AssetError, Assets, Key, Source, Transcode};

const TEST_COOKIE: &str = "TEST-COOKIE-0123456789";
const TEST_TOKEN: &str = "TEST-TOKEN-0123456789";
const TEST_KEY: &str = "TEST-API-KEY-0123456789abcdefghij";

#[derive(Clone, Debug)]
struct Recorded {
	method: String,
	path: String,
	headers: Vec<(String, String)>,
	body: Vec<u8>,
	/// When the request arrived
	at: Instant,
}

impl Recorded {
	fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
	}
}

struct Reply {
	status: u16,
	headers: Vec<(String, String)>,
	body: Vec<u8>,
	delay: Duration,
}

impl Reply {
	fn json(body: &str) -> Reply {
		Reply { status: 200, headers: vec![("Content-Type".into(), "application/json".into())], body: body.as_bytes().to_vec(), delay: Duration::ZERO }
	}

	fn bytes(body: &[u8]) -> Reply {
		Reply { status: 200, headers: Vec::new(), body: body.to_vec(), delay: Duration::ZERO }
	}

	fn redirect(location: &str) -> Reply {
		Reply { status: 302, headers: vec![("Location".into(), location.into())], body: Vec::new(), delay: Duration::ZERO }
	}

	fn status(status: u16) -> Reply {
		Reply { status, headers: Vec::new(), body: Vec::new(), delay: Duration::ZERO }
	}

	fn delayed(mut self, ms: u64) -> Reply {
		self.delay = Duration::from_millis(ms);
		self
	}
}

type Handler = dyn Fn(&Recorded) -> Reply + Send + Sync;

struct MockServer {
	port: u16,
	requests: Arc<Mutex<Vec<Recorded>>>,
	stop: Arc<AtomicBool>,
	thread: Option<std::thread::JoinHandle<()>>,
}

impl MockServer {
	fn start(handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> MockServer {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let port = listener.local_addr().unwrap().port();
		let requests = Arc::new(Mutex::new(Vec::new()));
		let stop = Arc::new(AtomicBool::new(false));
		let handler: Arc<Handler> = Arc::new(handler);
		let thread = {
			let requests = requests.clone();
			let stop = stop.clone();
			std::thread::spawn(move || {
				for stream in listener.incoming() {
					if stop.load(Ordering::SeqCst) {
						break;
					}
					let Ok(stream) = stream else { continue };
					let requests = requests.clone();
					let handler = handler.clone();
					std::thread::spawn(move || serve(stream, &requests, &*handler));
				}
			})
		};
		MockServer { port, requests, stop, thread: Some(thread) }
	}

	fn origin(&self) -> String {
		format!("http://127.0.0.1:{}", self.port)
	}

	fn url(&self, path: &str) -> String {
		format!("{}{path}", self.origin())
	}

	fn requests(&self) -> Vec<Recorded> {
		self.requests.lock().clone()
	}
}

impl Drop for MockServer {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		let _ = TcpStream::connect(("127.0.0.1", self.port));
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

fn serve(mut stream: TcpStream, requests: &Mutex<Vec<Recorded>>, handler: &Handler) {
	let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
	let mut head = Vec::new();
	let mut buf = [0u8; 4096];
	while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65536 {
		match stream.read(&mut buf) {
			Ok(0) | Err(_) => return,
			Ok(n) => head.extend_from_slice(&buf[..n]),
		}
	}
	let split = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |at| at + 4);
	let text = String::from_utf8_lossy(&head[..split]).into_owned();
	let mut lines = text.split("\r\n");
	let request_line = lines.next().unwrap_or("");
	let method = request_line.split(' ').next().unwrap_or("").to_string();
	let path = request_line.split(' ').nth(1).unwrap_or("").to_string();
	let headers: Vec<(String, String)> = lines
		.take_while(|l| !l.is_empty())
		.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
		.collect();
	// a body of Content-Length bytes (POSTs)
	let length = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
	let mut body = head[split..].to_vec();
	while body.len() < length {
		match stream.read(&mut buf) {
			Ok(0) | Err(_) => break,
			Ok(n) => body.extend_from_slice(&buf[..n]),
		}
	}
	let recorded = Recorded { method, path, headers, body, at: Instant::now() };
	requests.lock().push(recorded.clone());

	let reply = handler(&recorded);
	if !reply.delay.is_zero() {
		std::thread::sleep(reply.delay);
	}
	let mut response = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
	for (k, v) in &reply.headers {
		response.push_str(&format!("{k}: {v}\r\n"));
	}
	response.push_str("\r\n");
	let _ = stream.write_all(response.as_bytes());
	let _ = stream.write_all(&reply.body);
	let _ = stream.flush();
}

static DIRS: AtomicUsize = AtomicUsize::new(0);

fn temp_state(name: &str) -> std::path::PathBuf {
	let dir = std::env::temp_dir().join(format!("ror-assets-test-{name}-{}-{}", std::process::id(), DIRS.fetch_add(1, Ordering::Relaxed)));
	let _ = std::fs::remove_dir_all(&dir);
	dir
}

fn endpoints(assetdelivery: &MockServer, thumbnails: &MockServer) -> Endpoints {
	Endpoints { assetdelivery: assetdelivery.origin(), thumbnails: thumbnails.origin(), universes: thumbnails.origin(), open_cloud: thumbnails.origin(), allow_http: true }
}

fn assets(config: AssetConfig, endpoints: Endpoints, cookie: bool) -> Assets {
	let account = Account::find(None, || cookie.then(|| (Secret::new(TEST_COOKIE).unwrap(), CookieSource::Env("TEST"))));
	Assets::with_endpoints(config, endpoints, Some(account), Some(Key::find(None)))
}

/// Assets whose account is used through `broker` with a fake token; reading a cookie source fails the test.
fn via_broker(config: AssetConfig, endpoints: Endpoints, broker: &MockServer) -> Assets {
	let found = Ok((Broker { base: broker.origin(), token: Secret::new(TEST_TOKEN).unwrap() }, BrokerSource::Env));
	let account = Account::find(Some(found), || panic!("a cookie source was read with a broker's credentials"));
	Assets::with_endpoints(config, endpoints, Some(account), Some(Key::find(None)))
}

/// The broker's path for a request to `path` on the mock assetdelivery.
fn behind(assetdelivery: &MockServer, path: &str) -> String {
	format!("/rbx/127.0.0.1:{}{path}", assetdelivery.port)
}

fn bearer() -> String {
	format!("Bearer {TEST_TOKEN}")
}

fn no_cookie_anywhere(servers: &[&MockServer]) {
	for server in servers {
		for r in server.requests() {
			assert!(r.header("Cookie").is_none(), "cookie sent to {}{}", server.origin(), r.path);
			assert!(r.header("Authorization").is_none());
			assert!(r.header("x-api-key").is_none(), "API key sent to {}{}", server.origin(), r.path);
		}
	}
}

const MESH: &[u8] = b"version 4.01\nmesh payload";

#[test]
fn assets_is_shareable_across_threads() {
	fn check<T: Send + Sync + Clone + 'static>() {}
	check::<Assets>();
}

#[test]
fn cookie_goes_only_to_assetdelivery() {
	let real = MockServer::start(|r| if r.path == "/real/42" { Reply::bytes(MESH) } else { Reply::status(404) });
	let real_url = real.url("/real/42");
	let cdn = MockServer::start(move |r| if r.path == "/obj/42" { Reply::redirect(&real_url) } else { Reply::status(404) });
	let cdn_url = cdn.url("/obj/42");
	let ad = MockServer::start(move |r| {
		if r.path != "/v1/assetId/42" {
			return Reply::status(404);
		}
		if r.header("Cookie").is_some() {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::json(r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#)
		}
	});

	let a = assets(AssetConfig::new(temp_state("cookie")), endpoints(&ad, &cdn), true);
	let fetched = a.fetch("rbxassetid://42").unwrap();
	assert_eq!(fetched.bytes, MESH);
	assert_eq!(fetched.source, Source::Network);
	assert_eq!(fetched.kind(), "mesh v4.01");

	let ad_requests = ad.requests();
	assert_eq!(ad_requests.len(), 2, "public route, then the cookie route");
	assert_eq!(ad_requests[0].header("Cookie"), None);
	assert_eq!(ad_requests[1].header("Cookie"), Some(format!(".ROBLOSECURITY={TEST_COOKIE}").as_str()));
	assert_eq!(cdn.requests().len(), 1);
	assert_eq!(real.requests().len(), 1);
	no_cookie_anywhere(&[&cdn, &real]);
}

#[test]
fn studios_account_is_the_last_cookie_source() {
	use crate::studiocookie::{Lookup, Store};
	// a cookie from the environment or an env file wins, and Studio's store is then not read
	let configured = crate::account_cookie(|| Some((Secret::new(TEST_COOKIE).unwrap(), CookieSource::Env("ROBLOSECURITY"))), || panic!("Studio's store was read with a cookie configured"));
	assert_eq!(configured.map(|(_, source)| source), Some(CookieSource::Env("ROBLOSECURITY")));
	// switched off, not found or unreadable: no cookie
	for lookup in [Lookup::Off, Lookup::Unsupported, Lookup::NotFound, Lookup::Unreadable("Windows Credential Manager error 5".into())] {
		assert!(crate::account_cookie(|| None, || lookup).is_none());
	}
	// found: a cookie like the others, named by its store and never by its value
	let found = || crate::account_cookie(|| None, || Lookup::Found(Secret::new(TEST_COOKIE).unwrap(), Store::CredentialManager));
	let account = Account::find(None, found);
	assert_eq!(account.source.as_deref(), Some("studio (Windows Credential Manager)"));
	assert!(!format!("{:?}", account.credential).contains(TEST_COOKIE));
	// an account broker's credentials still come first
	let broker = Ok((Broker { base: "https://broker.example".into(), token: Secret::new(TEST_TOKEN).unwrap() }, BrokerSource::Env));
	let account = Account::find(Some(broker), || -> Option<(Secret, CookieSource)> { panic!("Studio's store was read with a broker's credentials") });
	assert_eq!(account.source.as_deref(), Some("account broker (env ROBLOX_BROKER_TOKEN)"));

	// and it goes where the cookie goes: assetdelivery alone, after the public route
	let real = MockServer::start(|_| Reply::bytes(MESH));
	let real_url = real.url("/real/42");
	let ad = MockServer::start(move |r| match r.header("Cookie") {
		Some(_) => Reply::json(&format!("{{\"location\":\"{real_url}\"}}")),
		None => Reply::json(r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#),
	});
	let a = Assets::with_endpoints(AssetConfig::new(temp_state("studio-cookie")), endpoints(&ad, &real), Some(Account::find(None, found)), Some(Key::find(None)));
	assert_eq!(a.cookie_source().as_deref(), Some("studio (Windows Credential Manager)"));
	assert_eq!(a.fetch("rbxassetid://42").unwrap().bytes, MESH);
	let asked = ad.requests();
	assert_eq!((asked[0].header("Cookie"), asked[1].header("Cookie")), (None, Some(format!(".ROBLOSECURITY={TEST_COOKIE}").as_str())));
	no_cookie_anywhere(&[&real]);
}

#[test]
fn cookie_never_crosses_redirects() {
	let real = MockServer::start(|_| Reply::bytes(MESH));
	let real_url = real.url("/real");
	let other = {
		let real_url = real_url.clone();
		MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{real_url}\"}}")))
	};
	let other_url = other.url("/json");
	let ad = MockServer::start(move |r| {
		let authed = r.header("Cookie").is_some();
		match r.path.as_str() {
			// Redirect to another host on the cookie route
			"/v1/assetId/7" if authed => Reply::redirect(&other_url),
			// Redirect to the same origin on the cookie route
			"/v1/assetId/8" if authed => Reply::redirect("/again/8"),
			"/again/8" => Reply::json(&format!("{{\"location\":\"{real_url}\"}}")),
			_ => Reply::json(r#"{"errors":[{"code":409,"message":"User is not authorized to access Asset."}]}"#),
		}
	});

	let a = assets(AssetConfig::new(temp_state("redirect")), endpoints(&ad, &other), true);
	assert_eq!(a.fetch("rbxassetid://7").unwrap().bytes, MESH);
	assert_eq!(a.fetch("rbxassetid://8").unwrap().bytes, MESH);
	no_cookie_anywhere(&[&other, &real]);
	let again: Vec<Recorded> = ad.requests().into_iter().filter(|r| r.path == "/again/8").collect();
	assert_eq!(again.len(), 1);
	assert_eq!(again[0].header("Cookie"), None, "redirected hop on the same origin");
}

#[test]
fn extra_headers_only_reach_assetdelivery() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(move |r| {
		if r.header("Cookie").is_some() {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::json(r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#)
		}
	});
	let config = AssetConfig {
		assetdelivery_headers: vec![
			("Roblox-Place-Id".into(), "123".into()),
			("Cookie".into(), "injected=1".into()),
			("Authorization".into(), "Bearer x".into()),
			("Bad Name".into(), "x".into()),
		],
		..AssetConfig::new(temp_state("headers"))
	};
	let a = assets(config, endpoints(&ad, &cdn), true);
	a.fetch("rbxassetid://11").unwrap();
	let requests = ad.requests();
	assert_eq!(requests.len(), 2);
	for r in &requests {
		assert_eq!(r.header("Roblox-Place-Id"), Some("123"));
		assert!(r.header("Authorization").is_none());
		assert!(r.header("Cookie").is_none_or(|c| c == format!(".ROBLOSECURITY={TEST_COOKIE}")), "only the real cookie");
	}
	assert!(cdn.requests().iter().all(|r| r.header("Roblox-Place-Id").is_none()));
	no_cookie_anywhere(&[&cdn]);
}

#[test]
fn public_assets_never_use_the_cookie() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")));
	let a = assets(AssetConfig::new(temp_state("public")), endpoints(&ad, &cdn), true);
	a.fetch("http://www.roblox.com/asset/?id=5").unwrap();
	assert_eq!(ad.requests().len(), 1);
	no_cookie_anywhere(&[&ad, &cdn]);
}

#[test]
fn unauthorized_errors_without_leaking() {
	let ad = MockServer::start(|_| Reply::json(r#"{"errors":[{"code":409,"message":"User is not authorized to access Asset."}]}"#));
	let a = assets(AssetConfig::new(temp_state("unauth")), endpoints(&ad, &ad), true);
	let error = a.fetch("rbxassetid://9").unwrap_err();
	assert!(matches!(error, AssetError::Http { status: 409, .. }), "{error:?}");
	for text in [format!("{error}"), format!("{error:?}")] {
		assert!(!text.contains(TEST_COOKIE));
		assert!(!text.contains("ROBLOSECURITY"));
	}

	// Without a cookie the public 401 is the answer
	let ad = MockServer::start(|_| Reply::json(r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#));
	let a = assets(AssetConfig::new(temp_state("nocookie")), endpoints(&ad, &ad), false);
	assert!(matches!(a.fetch("rbxassetid://9"), Err(AssetError::Http { status: 401, .. })));
	assert_eq!(ad.requests().len(), 1);

	// Not found
	let ad = MockServer::start(|_| Reply::json(r#"{"errors":[{"code":404,"message":"Request asset was not found"}]}"#));
	let a = assets(AssetConfig::new(temp_state("404")), endpoints(&ad, &ad), true);
	assert!(matches!(a.fetch("rbxassetid://10"), Err(AssetError::NotFound(_))));
}

#[test]
fn refusals_are_remembered_across_instances() {
	let ad = MockServer::start(|r| {
		if r.path == "/v1/assetId/404" {
			Reply::json(r#"{"errors":[{"code":404,"message":"Request asset was not found"}]}"#)
		} else {
			Reply::json(r#"{"errors":[{"code":403,"message":"Asset is not approved for the requester"}]}"#)
		}
	});
	let state = temp_state("refused");
	let first = assets(AssetConfig::new(&state), endpoints(&ad, &ad), true);
	assert!(matches!(first.fetch("rbxassetid://9"), Err(AssetError::Http { status: 403, .. })));
	assert!(matches!(first.fetch("rbxassetid://404"), Err(AssetError::NotFound(_))));
	let asked = ad.requests().len();

	// Another instance (another process in practice) answers from the disk without asking
	let second = assets(AssetConfig::new(&state), endpoints(&ad, &ad), true);
	assert!(matches!(second.fetch("rbxassetid://9"), Err(AssetError::Http { status: 403, .. })));
	assert!(matches!(second.fetch("rbxassetid://404"), Err(AssetError::NotFound(_))));
	assert_eq!(ad.requests().len(), asked);

	// Refresh asks again
	let refresh = assets(AssetConfig { refresh: true, ..AssetConfig::new(&state) }, endpoints(&ad, &ad), true);
	assert!(matches!(refresh.fetch("rbxassetid://9"), Err(AssetError::Http { status: 403, .. })));
	assert!(ad.requests().len() > asked);

	// Without a cookie a refusal is not final: nothing is remembered
	let state = temp_state("refused-nocookie");
	let anonymous = assets(AssetConfig::new(&state), endpoints(&ad, &ad), false);
	assert!(anonymous.fetch("rbxassetid://9").is_err());
	assert!(anonymous.inner.disk.read_refusal("rbxassetid://9", 0).is_none());
}

/// assetdelivery's error envelopes as measured 2026-10-10 (notes/assets.md "Refusals"), by asset id.
fn refusing_server() -> MockServer {
	MockServer::start(|r| {
		let id = r.path.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("");
		Reply::json(match id {
			"18" => r#"{"errors":[{"code":403,"message":"Asset has not been reviewed","customErrorCode":18}],"isArchived":false}"#,
			"19" => r#"{"errors":[{"code":403,"message":"Asset is not approved for the requester","customErrorCode":19}],"isArchived":false}"#,
			"22" => r#"{"errors":[{"code":403,"message":"Requested asset is archived","customErrorCode":22}],"isArchived":true}"#,
			"1" => r#"{"errors":[{"code":403,"message":"User is not authorized to access Asset.","customErrorCode":1}]}"#,
			"409" => r#"{"errors":[{"code":409,"message":"User is not authorized to access Asset."}]}"#,
			"401" => r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#,
			"77" => "{\"errors\":[{\"code\":403,\"message\":\"Something new\\u0007 \\u00e9\",\"customErrorCode\":77}]}",
			"500" => r#"{"errors":[{"code":500,"message":"AssetFormatBinary Error"}]}"#,
			_ => r#"{"errors":[{"code":404,"message":"Request asset was not found"}]}"#,
		})
	})
}

#[test]
fn refusals_say_why() {
	use crate::RefusalReason;

	let ad = refusing_server();
	let state = temp_state("reasons");
	let a = assets(AssetConfig::new(&state), endpoints(&ad, &ad), true);
	let cases = [
		("18", 403, Some(RefusalReason::Unreviewed), "HTTP 403, not yet reviewed by Roblox moderation"),
		("19", 403, Some(RefusalReason::Moderated), "HTTP 403, rejected by Roblox moderation"),
		("22", 403, Some(RefusalReason::Archived), "HTTP 403, archived by its creator"),
		("1", 403, Some(RefusalReason::NotAuthorized), "HTTP 403, not authorized for this account"),
		("409", 409, Some(RefusalReason::NotAuthorized), "HTTP 409, not authorized for this account"),
		("401", 401, Some(RefusalReason::AuthenticationRequired), "HTTP 401, authentication required"),
		("77", 403, Some(RefusalReason::Other("Something new".into())), "HTTP 403, \"Something new\""),
		("500", 500, None, "HTTP 500"),
	];
	for (id, status, reason, text) in &cases {
		let uri = format!("rbxassetid://{id}");
		let error = a.fetch(&uri).unwrap_err();
		let AssetError::Http { status: got, reason: got_reason, .. } = &error else {
			panic!("{uri}: {error:?}");
		};
		assert_eq!((got, got_reason), (status, reason), "{uri}");
		assert_eq!(error.to_string(), format!("{uri}: {text}"));
		assert!(!format!("{error}{error:?}").contains(TEST_COOKIE));
	}
	assert!([RefusalReason::Unreviewed, RefusalReason::Moderated, RefusalReason::Archived].iter().all(RefusalReason::universal));
	assert!(![RefusalReason::NotAuthorized, RefusalReason::AuthenticationRequired, RefusalReason::Other(String::new())].iter().any(RefusalReason::universal));
	// a refusal still tries the cookie (the asset's owner may be served), then is the answer
	assert_eq!(ad.requests().iter().filter(|r| r.path == "/v1/assetId/19").count(), 2);

	// another instance answers with the remembered reasons, without asking
	let asked = ad.requests().len();
	let second = assets(AssetConfig::new(&state), endpoints(&ad, &ad), true);
	for (id, _, _, text) in cases.iter().filter(|(_, status, ..)| *status != 500) {
		let uri = format!("http://www.roblox.com/asset/?id={id}");
		assert_eq!(second.fetch(&uri).unwrap_err().to_string(), format!("{uri}: {text}"));
	}
	assert_eq!(ad.requests().len(), asked);

	// a transcode of a moderated image is refused alike
	let error = a.fetch_transcode("rbxassetid://19", Transcode::Image, 0).unwrap_err();
	assert!(matches!(error, AssetError::Http { status: 403, reason: Some(RefusalReason::Moderated), .. }), "{error:?}");

	// a deleted asset is still not found
	assert!(matches!(a.fetch("rbxassetid://2"), Err(AssetError::NotFound(_))));
}

#[test]
fn concurrent_fetches_share_one_download() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH).delayed(300));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(move |r| {
		if r.path == "/v1/assetId/404" {
			Reply::json(r#"{"errors":[{"code":404,"message":"Request asset was not found"}]}"#).delayed(300)
		} else {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		}
	});
	let a = assets(AssetConfig::new(temp_state("dedup")), endpoints(&ad, &cdn), false);

	let spellings = ["rbxassetid://5", "http://www.roblox.com/asset/?id=5", "5", "rbxassetid://5/"];
	let results: Vec<_> = std::thread::scope(|scope| {
		let handles: Vec<_> = (0..16).map(|i| {
			let a = a.clone();
			scope.spawn(move || a.fetch(spellings[i % spellings.len()]))
		}).collect();
		handles.into_iter().map(|h| h.join().unwrap()).collect()
	});
	assert!(results.iter().all(|r| r.as_ref().is_ok_and(|f| f.bytes == MESH)));
	for (i, r) in results.iter().enumerate() {
		assert_eq!(r.as_ref().unwrap().uri, spellings[i % spellings.len()], "each caller sees its own spelling");
	}
	assert_eq!(ad.requests().len(), 1);
	assert_eq!(cdn.requests().len(), 1);

	// Errors are shared too
	let errors: Vec<_> = std::thread::scope(|scope| {
		let handles: Vec<_> = (0..8).map(|_| {
			let a = a.clone();
			scope.spawn(move || a.fetch("rbxassetid://404"))
		}).collect();
		handles.into_iter().map(|h| h.join().unwrap()).collect()
	});
	assert!(errors.iter().all(|r| matches!(r, Err(AssetError::NotFound(_)))));
	assert_eq!(ad.requests().iter().filter(|r| r.path == "/v1/assetId/404").count(), 1);
}

#[test]
fn disk_cache_is_shared_and_offline_works() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")));
	let state = temp_state("disk");

	let first = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	assert_eq!(first.fetch("rbxassetid://77").unwrap().source, Source::Network);
	let (bin, json) = first.inner.disk.paths("rbxassetid://77");
	assert!(bin.is_file() && json.is_file());
	let meta = first.inner.disk.read_meta("rbxassetid://77").unwrap();
	assert_eq!((meta.uri.as_str(), meta.size, meta.kind.as_str()), ("rbxassetid://77", MESH.len(), "mesh v4.01"));
	assert_eq!(meta.source_host, format!("127.0.0.1:{}", cdn.port));

	// Another instance (another process in practice) hits the disk cache
	let second = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	let fetched = second.fetch("http://www.roblox.com/asset/?id=77").unwrap();
	assert_eq!((fetched.source, fetched.bytes.as_slice()), (Source::DiskCache, MESH));
	assert_eq!(ad.requests().len(), 1);

	// Offline: cache only
	let offline = assets(AssetConfig { offline: true, ..AssetConfig::new(&state) }, endpoints(&ad, &cdn), true);
	assert_eq!(offline.fetch("rbxassetid://77").unwrap().source, Source::DiskCache);
	assert!(matches!(offline.fetch("rbxassetid://78"), Err(AssetError::Offline { .. })));
	assert_eq!(ad.requests().len(), 1);

	// Refresh: downloads again once, then memory
	let refresh = assets(AssetConfig { refresh: true, ..AssetConfig::new(&state) }, endpoints(&ad, &cdn), false);
	assert_eq!(refresh.fetch("rbxassetid://77").unwrap().source, Source::Network);
	assert_eq!(refresh.fetch("rbxassetid://77").unwrap().source, Source::Network);
	assert_eq!(ad.requests().len(), 2);
}

#[test]
fn the_content_store_serves_every_state_dir() {
	let cdn = MockServer::start(|r| match r.path.as_str() {
		"/cdn/77" => Reply::bytes(MESH),
		_ => Reply::bytes(b"version 4.01
other mesh"),
	});
	let cdn_url = cdn.url("/cdn/");
	let ad = MockServer::start(move |r| Reply::json(&format!("{{\"location\":\"{cdn_url}{}\"}}", r.path.rsplit('/').next().unwrap_or_default())));
	let (store, old, first, fresh) = (temp_state("store"), temp_state("store-old"), temp_state("store-first"), temp_state("store-fresh"));
	let with_store = |state: &std::path::Path| AssetConfig { store: Some(store.clone()), ..AssetConfig::new(state) };
	// a state dir that downloaded before the store was
	let before = assets(AssetConfig::new(&old), endpoints(&ad, &cdn), false);
	assert_eq!(before.fetch("rbxassetid://79").unwrap().source, Source::Network);
	crate::store::register(&store, &old);
	// a download goes to the store too (one file)
	let a = assets(with_store(&first), endpoints(&ad, &cdn), false);
	assert_eq!(a.fetch("rbxassetid://77").unwrap().source, Source::Network);
	let (store_bin, store_json) = crate::cache::DiskCache::new(&store, &[], None).paths("rbxassetid://77");
	assert!(store_bin.is_file() && store_json.is_file());
	assert_eq!(ad.requests().len(), 2);
	// a fresh state dir takes it from the store, and what a registered state dir has, offline
	let b = assets(AssetConfig { offline: true, ..with_store(&fresh) }, endpoints(&ad, &cdn), false);
	assert_eq!(b.fetch("rbxassetid://77").unwrap().bytes, MESH);
	assert_eq!(b.fetch("rbxassetid://79").unwrap().bytes, b"version 4.01
other mesh");
	assert_eq!(ad.requests().len(), 2);
	// which the store has now
	assert!(crate::cache::DiskCache::new(&store, &[], None).paths("rbxassetid://79").0.is_file());
	// a damaged file in the store is not taken (and goes)
	let other = temp_state("store-other");
	std::fs::write(&store_bin, b"version 4.01
cut").unwrap();
	let c = assets(AssetConfig { offline: true, ..with_store(&other) }, endpoints(&ad, &cdn), false);
	let _ = std::fs::remove_dir_all(first.join("assets"));
	assert!(matches!(c.fetch("rbxassetid://77"), Err(AssetError::Offline { .. })));
	assert!(!store_bin.exists());
	// without the store, nothing is shared
	let d = assets(AssetConfig { offline: true, ..AssetConfig::new(temp_state("store-none")) }, endpoints(&ad, &cdn), false);
	assert!(matches!(d.fetch("rbxassetid://79"), Err(AssetError::Offline { .. })));
}

#[test]
fn shared_state_dirs_lend_their_downloads() {
	// asset 77 changes after its first download (an unversioned id re-uploaded)
	let downloads = Arc::new(AtomicUsize::new(0));
	let cdn = {
		let downloads = downloads.clone();
		MockServer::start(move |r| match r.path.as_str() {
			"/cdn/77" if downloads.fetch_add(1, Ordering::SeqCst) > 0 => Reply::bytes(b"version 4.01\nnewer mesh"),
			"/cdn/77" => Reply::bytes(MESH),
			_ => Reply::bytes(b"version 4.01\nother mesh"),
		})
	};
	let cdn_url = cdn.url("/cdn/");
	let ad = MockServer::start(move |r| Reply::json(&format!("{{\"location\":\"{cdn_url}{}\"}}", r.path.rsplit('/').next().unwrap_or_default())));
	let (lender, borrower) = (temp_state("lender"), temp_state("borrower"));
	let first = assets(AssetConfig::new(&lender), endpoints(&ad, &cdn), false);
	assert_eq!(first.fetch("rbxassetid://77").unwrap().source, Source::Network);
	assert_eq!(ad.requests().len(), 1);

	// another state dir sharing the first one's takes its file instead of downloading it, metadata too
	let shared = || AssetConfig { shared: vec![lender.clone()], ..AssetConfig::new(&borrower) };
	let second = assets(shared(), endpoints(&ad, &cdn), false);
	assert!(second.inner.disk.contains("rbxassetid://77"));
	let fetched = second.fetch("rbxassetid://77").unwrap();
	assert_eq!((fetched.source, fetched.bytes.as_slice()), (Source::DiskCache, MESH));
	assert_eq!(ad.requests().len(), 1);
	let (bin, _) = second.inner.disk.paths("rbxassetid://77");
	assert_eq!(std::fs::read(&bin).unwrap(), MESH);
	assert_eq!(second.inner.disk.read_meta("rbxassetid://77").unwrap().uri, "rbxassetid://77");
	// its own name now: found without the lender, offline
	let alone = assets(AssetConfig { offline: true, ..AssetConfig::new(&borrower) }, endpoints(&ad, &cdn), false);
	assert_eq!(alone.fetch("rbxassetid://77").unwrap().source, Source::DiskCache);
	// what neither has is downloaded into the borrower's only
	assert_eq!(second.fetch("rbxassetid://78").unwrap().source, Source::Network);
	assert!(!first.inner.disk.paths("rbxassetid://78").0.exists());
	// a refresh replaces the borrower's name by a rename: the lender's file (one file with it until
	// then) keeps the old bytes
	let lent = first.inner.disk.paths("rbxassetid://77").0;
	let refresh = assets(AssetConfig { refresh: true, ..shared() }, endpoints(&ad, &cdn), false);
	assert_eq!(refresh.fetch("rbxassetid://77").unwrap().bytes, b"version 4.01\nnewer mesh");
	assert_eq!(std::fs::read(&bin).unwrap(), b"version 4.01\nnewer mesh");
	assert_eq!(std::fs::read(&lent).unwrap(), MESH);
	// warming counts the lender's files as cached
	let report = assets(shared(), endpoints(&ad, &cdn), false).warm(&[crate::Download::Asset("rbxassetid://77".into())], &|_| 0);
	assert_eq!((report.downloaded, report.cached), (0, 1));
	// a state dir listed as shared with itself changes nothing
	let itself = assets(AssetConfig { shared: vec![lender.clone()], ..AssetConfig::new(&lender) }, endpoints(&ad, &cdn), false);
	assert_eq!(itself.fetch("rbxassetid://77").unwrap().source, Source::DiskCache);
	for dir in [&lender, &borrower] {
		let _ = std::fs::remove_dir_all(dir);
	}
}

#[test]
fn rate_limits_are_retried() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let calls = Arc::new(AtomicUsize::new(0));
	let ad = {
		let calls = calls.clone();
		MockServer::start(move |_| {
			if calls.fetch_add(1, Ordering::SeqCst) == 0 {
				let mut reply = Reply::status(429);
				reply.headers.push(("Retry-After".into(), "0".into()));
				reply
			} else {
				Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
			}
		})
	};
	let a = assets(AssetConfig::new(temp_state("429")), endpoints(&ad, &cdn), false);
	assert_eq!(a.fetch("rbxassetid://3").unwrap().bytes, MESH);
	assert_eq!(ad.requests().len(), 2);

	// Persistent server errors give up with the status
	let ad = MockServer::start(|_| {
		let mut reply = Reply::status(503);
		reply.headers.push(("Retry-After".into(), "0".into()));
		reply
	});
	let a = assets(AssetConfig::new(temp_state("503")), endpoints(&ad, &ad), false);
	assert!(matches!(a.fetch("rbxassetid://3"), Err(AssetError::Http { status: 503, .. })));
	assert_eq!(ad.requests().len(), 5);
}

#[test]
fn thumbnails_and_gzip_bodies() {
	let png = b"\x89PNG\r\n\x1a\nfake png body".to_vec();
	let gz = {
		let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
		encoder.write_all(&png).unwrap();
		encoder.finish().unwrap()
	};
	let cdn = MockServer::start(move |_| Reply::bytes(&gz));
	let cdn_url = cdn.url("/thumb");
	let thumbs = MockServer::start(move |r| {
		if r.path == "/v1/assets?assetIds=1&size=150x150&format=Png&isCircular=false" {
			Reply::json(&format!("{{\"data\":[{{\"targetId\":1,\"state\":\"Completed\",\"imageUrl\":\"{cdn_url}\"}}]}}"))
		} else {
			Reply::status(400)
		}
	});
	let a = assets(AssetConfig::new(temp_state("thumb")), endpoints(&thumbs, &thumbs), true);
	let fetched = a.fetch("rbxthumb://type=Asset&id=1&w=150&h=150").unwrap();
	assert_eq!(fetched.bytes, png, "gzip body without Content-Encoding is decompressed");
	assert_eq!(fetched.kind(), "png");
	no_cookie_anywhere(&[&cdn]);
	// The thumbnails mock doubles as assetdelivery here, so check its requests directly
	assert!(thumbs.requests().iter().all(|r| r.header("Cookie").is_none()));
}

#[test]
fn studio_files_local_files_and_fetch_many() {
	let install = temp_state("install");
	let content = install.join("content");
	let platform = install.join("PlatformContent").join("pc");
	std::fs::create_dir_all(content.join("textures")).unwrap();
	std::fs::create_dir_all(platform.join("textures")).unwrap();
	std::fs::write(content.join("textures").join("face.png"), b"\x89PNG\r\n\x1a\nface").unwrap();
	std::fs::write(platform.join("textures").join("studs.dds"), b"DDS studs").unwrap();

	let ad = MockServer::start(|_| Reply::status(500));
	let config = AssetConfig { studio_content: Some(content.clone()), ..AssetConfig::new(temp_state("studio")) };
	let a = assets(config, endpoints(&ad, &ad), false);

	let face = a.fetch("rbxasset://textures/face.png").unwrap();
	assert_eq!((face.source, face.kind()), (Source::StudioInstall, "png"));
	assert_eq!(a.fetch("rbxasset://textures/studs.dds").unwrap().bytes, b"DDS studs");
	assert!(matches!(a.fetch("rbxasset://textures/none.png"), Err(AssetError::NotFound(_))));
	assert_eq!(a.studio_file("textures/studs.dds"), Some(platform.join("textures").join("studs.dds")));

	// Local files, re-read when they change
	let file = install.join("local.bin");
	std::fs::write(&file, b"one").unwrap();
	let path = file.to_string_lossy().into_owned();
	assert_eq!(a.fetch(&path).unwrap().bytes, b"one");
	assert_eq!(a.fetch(&path).unwrap().source, Source::LocalFile);
	std::fs::write(&file, b"second").unwrap();
	assert_eq!(a.fetch(&path).unwrap().bytes, b"second");
	assert!(matches!(a.fetch(&install.join("missing.bin").to_string_lossy()), Err(AssetError::NotFound(_))));

	// fetch_many keeps the order
	let uris = ["rbxasset://textures/face.png", "", path.as_str(), "rbxasset://textures/studs.dds", "nonsense"];
	let results = a.fetch_many(&uris);
	assert_eq!(results.len(), 5);
	assert!(results[0].as_ref().is_ok_and(|f| f.uri == uris[0]));
	assert!(matches!(results[1], Err(AssetError::InvalidUri(_))));
	assert!(results[2].as_ref().is_ok_and(|f| f.bytes == b"second"));
	assert!(results[3].as_ref().is_ok_and(|f| f.bytes == b"DDS studs"));
	assert!(matches!(results[4], Err(AssetError::InvalidUri(_))));
	assert!(ad.requests().is_empty());
}

const KTX2: &[u8] = b"\xabKTX 20\xbb\r\n\x1a\nfake transcode";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake png";

#[test]
fn transcodes_are_requested_and_cached_apart_from_the_image() {
	use crate::Transcode;
	use crate::transcode::query_escape;

	let cdn = MockServer::start(|r| match r.path.as_str() {
		"/png" => Reply::bytes(PNG),
		"/ktx2/0" | "/ktx2/1" => Reply::bytes(KTX2),
		_ => Reply::status(404),
	});
	let (png_url, ktx_url) = (cdn.url("/png"), cdn.url("/ktx2"));
	let ad = MockServer::start(move |r| {
		let Some((path, query)) = r.path.split_once('?') else {
			return Reply::json(&format!("{{\"location\":\"{png_url}\"}}"));
		};
		if path != "/v1/assetId/5" || !query.ends_with("&doNotFallbackToBaselineRepresentation=true") {
			return Reply::status(400);
		}
		// packs 0 and 1 of the image transcode exist, nothing else yet
		let pack = if query.contains(&query_escape(&Transcode::Image.priority_list(0))) {
			0
		} else if query.contains(&query_escape(&Transcode::Image.priority_list(1))) {
			1
		} else {
			return Reply::json(r#"{"errors":[{"code":406,"message":"Asset content representation is being generated","customErrorCode":21}]}"#);
		};
		Reply::json(&format!(r#"{{"location":"{ktx_url}/{pack}","contentRepresentationSpecifier":{{"format":"ktx2","majorVersion":"7rdo","fidelity":"x"}}}}"#))
	});
	let state = temp_state("transcode");
	let a = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	assert_eq!(a.fetch("rbxassetid://5").unwrap().bytes, PNG);
	let pack0 = a.fetch_transcode("rbxassetid://5", Transcode::Image, 0).unwrap();
	assert_eq!((pack0.bytes.as_slice(), pack0.source), (KTX2, Source::Network));
	assert_eq!(a.fetch_transcode("rbxassetid://5", Transcode::Image, 1).unwrap().bytes, KTX2);
	assert!(a.inner.disk.paths("rbxassetid://5#ktx2/7rdo//////AE=").0.is_file());
	assert!(a.inner.disk.paths("rbxassetid://5").0.is_file());

	// Another instance: both from the disk cache, still apart
	let again = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	assert_eq!(again.fetch("rbxassetid://5").unwrap().bytes, PNG);
	let cached = again.fetch_transcode("http://www.roblox.com/asset/?id=5", Transcode::Image, 0).unwrap();
	assert_eq!((cached.bytes.as_slice(), cached.source), (KTX2, Source::DiskCache));
	let requests = ad.requests().len();

	// Still being made: retried a few times, then 406
	let error = again.fetch_transcode("rbxassetid://5", Transcode::PackColor, 0).unwrap_err();
	assert!(matches!(error, AssetError::Http { status: 406, .. }), "{error:?}");
	assert_eq!(ad.requests().len(), requests + 5);

	// Remembered: another instance answers at once
	let requests = ad.requests().len();
	let third = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	assert!(matches!(third.fetch_transcode("rbxassetid://5", Transcode::PackColor, 0), Err(AssetError::Http { status: 406, .. })));
	assert_eq!(ad.requests().len(), requests);
	// Expired after being made for an hour: asked once more without waiting, then remembered as long again
	let key = Transcode::PackColor.cache_key(&crate::uri::ContentId::parse("rbxassetid://5").unwrap().canonical(), 0);
	let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
	third.inner.disk.write_refusal(&key, &crate::cache::Refusal { status: 406, until: now - 1, reason: None, since: Some(now - 3600) }).unwrap();
	let fourth = assets(AssetConfig::new(&state), endpoints(&ad, &cdn), false);
	assert!(matches!(fourth.fetch_transcode("rbxassetid://5", Transcode::PackColor, 0), Err(AssetError::Http { status: 406, .. })));
	assert_eq!(ad.requests().len(), requests + 1, "one request, no waits");
	let refusal = fourth.inner.disk.last_refusal(&key).unwrap();
	assert_eq!(refusal.since, Some(now - 3600));
	assert!(refusal.until >= now + 3600 - 5, "remembered for as long as it has been made: until {} at {now}", refusal.until);

	// Only asset ids have transcodes
	assert!(matches!(again.fetch_transcode("rbxasset://textures/face.png", Transcode::Image, 0), Err(AssetError::NoTranscode(_))));
	no_cookie_anywhere(&[&ad, &cdn]);
}

#[test]
fn transcodes_need_the_representation_named() {
	let cdn = MockServer::start(|_| Reply::bytes(PNG));
	let cdn_url = cdn.url("/png");
	// assetdelivery falling back to the uploaded image: no contentRepresentationSpecifier
	let ad = MockServer::start(move |r| {
		if r.header("Cookie").is_some() {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::json(r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#)
		}
	});
	let a = assets(AssetConfig::new(temp_state("transcode-named")), endpoints(&ad, &cdn), true);
	let error = a.fetch_transcode("rbxassetid://6", crate::Transcode::PackNormal, 2).unwrap_err();
	assert!(matches!(error, AssetError::NoTranscode(_)), "{error:?}");
	let requests = ad.requests();
	assert_eq!(requests.len(), 2, "public route, then the cookie route");
	assert_eq!(requests[1].header("Cookie"), Some(format!(".ROBLOSECURITY={TEST_COOKIE}").as_str()));
	assert!(cdn.requests().is_empty());
	no_cookie_anywhere(&[&cdn]);
}

// --- warm: the batch endpoint ---------------------------------------------------------------------

/// A mock assetdelivery batch endpoint: each request's location on `cdn` (`/cdn/<id>` or
/// `/cdn/<id>/<fidelity>` for a named representation, its specifier naming ktx2 when `transcodes`
/// holds the id), or an error code from `refuse`.
fn batch_server(cdn: String, transcodes: Vec<u64>, refuse: Vec<(u64, u16)>) -> MockServer {
	batch_server_at("/v1/assets/batch".into(), cdn, transcodes, refuse)
}

/// [`batch_server`] answering at `path`.
fn batch_server_at(path: String, cdn: String, transcodes: Vec<u64>, refuse: Vec<(u64, u16)>) -> MockServer {
	MockServer::start(move |r| {
		if r.method != "POST" || r.path != path {
			return Reply::status(404);
		}
		let requests: Vec<serde_json::Value> = serde_json::from_slice(&r.body).unwrap_or_default();
		let replies: Vec<serde_json::Value> = requests
			.iter()
			.map(|request| {
				let id = request["assetId"].as_u64().unwrap_or(0);
				let request_id = request["requestId"].clone();
				if let Some((_, code)) = refuse.iter().find(|(refused, _)| *refused == id) {
					return serde_json::json!({ "requestId": request_id, "errors": [{ "code": code, "message": "no" }] });
				}
				match request["contentRepresentationPriorityList"].as_str() {
					Some(list) if transcodes.contains(&id) => {
						let fidelity = list.len();
						serde_json::json!({ "requestId": request_id, "location": format!("{cdn}/cdn/{id}/{fidelity}"), "contentRepresentationSpecifier": { "format": "ktx2" } })
					}
					// no such representation: the upload's location, without a specifier
					_ => serde_json::json!({ "requestId": request_id, "location": format!("{cdn}/cdn/{id}") }),
				}
			})
			.collect();
		Reply::json(&serde_json::to_string(&replies).unwrap())
	})
}

#[test]
fn warm_downloads_through_the_batch_endpoint() {
	let cdn = MockServer::start(|r| if r.path.starts_with("/cdn/") { Reply::bytes(MESH) } else { Reply::status(404) });
	let ad = batch_server(cdn.origin(), Vec::new(), Vec::new());
	let a = assets(AssetConfig { max_concurrent_downloads: 8, ..AssetConfig::new(temp_state("warm")) }, endpoints(&ad, &cdn), true);
	let downloads: Vec<crate::Download> = (1..=20).map(|id| crate::Download::Asset(format!("rbxassetid://{id}"))).collect();
	let report = a.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed, report.cached), (20, 0, 0));
	assert!(report.batches <= 3, "20 files in a few batch requests, not one each: {}", report.batches);
	// the files are on disk: fetches need no network
	let before = ad.requests().len();
	for id in 1..=20 {
		assert_eq!(a.fetch(&format!("rbxassetid://{id}")).unwrap().source, Source::DiskCache);
	}
	assert_eq!(ad.requests().len(), before);
	assert!(ad.requests().iter().all(|r| r.header("Cookie") == Some(format!(".ROBLOSECURITY={TEST_COOKIE}").as_str())));
	no_cookie_anywhere(&[&cdn]);
	// again: everything cached, nothing asked
	let again = a.warm(&downloads, &|_| 0);
	assert_eq!((again.downloaded, again.cached, again.batches), (0, 20, 0));
}

#[test]
fn warm_follows_transcode_packs() {
	let cdn = MockServer::start(|r| if r.path.starts_with("/cdn/") { Reply::bytes(KTX2) } else { Reply::status(404) });
	let ad = batch_server(cdn.origin(), vec![7], Vec::new());
	let a = assets(AssetConfig::new(temp_state("warm-packs")), endpoints(&ad, &cdn), true);
	// pack 0 lists three packs
	let report = a.warm(&[crate::Download::Transcode("rbxassetid://7".into(), Transcode::Image)], &|bytes| if bytes == KTX2 { 3 } else { 0 });
	assert_eq!(report.downloaded, 3, "pack 0, then the two it lists");
	for pack in 0..3 {
		assert_eq!(a.fetch_transcode("rbxassetid://7", Transcode::Image, pack).unwrap().source, Source::DiskCache);
	}
	// a cached pack 0 still leads to its packs
	let again = a.warm(&[crate::Download::Transcode("rbxassetid://7".into(), Transcode::Image)], &|_| 3);
	assert_eq!((again.downloaded, again.cached), (0, 3));
}

#[test]
fn warm_takes_an_images_upload_without_a_transcode() {
	let cdn = MockServer::start(|r| if r.path == "/cdn/9" { Reply::bytes(b"\x89PNG\r\n\x1a\nimage") } else { Reply::status(404) });
	let ad = batch_server(cdn.origin(), Vec::new(), Vec::new());
	let a = assets(AssetConfig::new(temp_state("warm-upload")), endpoints(&ad, &cdn), true);
	let report = a.warm(&[crate::Download::Transcode("rbxassetid://9".into(), Transcode::Image)], &|_| 0);
	assert_eq!(report.downloaded, 1);
	assert_eq!(a.fetch("rbxassetid://9").unwrap().source, Source::DiskCache);
}

#[test]
fn warm_remembers_why_assets_were_refused() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let downloads: Vec<crate::Download> = ["19", "22"].iter().map(|id| crate::Download::Asset(format!("rbxassetid://{id}"))).collect();
	let batch = MockServer::start(|r| {
		let replies: Vec<serde_json::Value> = serde_json::from_slice::<Vec<serde_json::Value>>(&r.body)
			.unwrap_or_default()
			.iter()
			.map(|request| {
				let custom = if request["assetId"] == 19 { 19 } else { 22 };
				serde_json::json!({ "requestId": request["requestId"], "errors": [{ "code": 403, "message": "refused", "customErrorCode": custom }] })
			})
			.collect();
		Reply::json(&serde_json::to_string(&replies).unwrap())
	});
	let state = temp_state("warm-reasons-batch");
	let a = assets(AssetConfig::new(&state), endpoints(&batch, &cdn), true);
	let report = a.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed), (0, 2));
	// fetches answer from the remembered refusals, reasons included, without asking
	let asked = batch.requests().len();
	assert_eq!(a.fetch("rbxassetid://19").unwrap_err().to_string(), "rbxassetid://19: HTTP 403, rejected by Roblox moderation");
	assert_eq!(a.fetch("rbxassetid://22").unwrap_err().to_string(), "rbxassetid://22: HTTP 403, archived by its creator");
	assert_eq!(batch.requests().len(), asked);
	assert!(cdn.requests().is_empty());
}

#[test]
fn warm_remembers_refusals_and_leaves_failures_to_fetch() {
	let cdn = MockServer::start(|r| if r.path == "/cdn/1" { Reply::bytes(MESH) } else { Reply::status(404) });
	let ad = batch_server(cdn.origin(), Vec::new(), vec![(2, 404), (3, 500)]);
	let a = assets(AssetConfig::new(temp_state("warm-refused")), endpoints(&ad, &cdn), true);
	let downloads: Vec<crate::Download> = (1..=3).map(|id| crate::Download::Asset(format!("rbxassetid://{id}"))).collect();
	let report = a.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed), (1, 2));
	// the 404 is remembered: asked no more
	let again = a.warm(&downloads, &|_| 0);
	assert_eq!((again.downloaded, again.cached, again.failed), (0, 2, 1));
	assert!(matches!(a.fetch("rbxassetid://2"), Err(AssetError::NotFound(_))));
}

// --- an account broker in place of the cookie -------------------------------------------------------

const NEEDS_ACCOUNT: &str = r#"{"errors":[{"code":401,"message":"Authentication required to access Asset."}]}"#;

/// A broker's own refusal: `X-Broker-Error` and a JSON body.
fn broker_refusal(status: u16, code: &str, message: &str) -> Reply {
	let body = serde_json::json!({ "error": code, "message": message }).to_string();
	Reply { status, headers: vec![("X-Broker-Error".into(), code.into()), ("Content-Type".into(), "application/json".into())], body: body.into_bytes(), delay: Duration::ZERO }
}

#[test]
fn a_broker_carries_what_the_cookie_would() {
	let real = MockServer::start(|r| if r.path == "/real/42" { Reply::bytes(MESH) } else { Reply::status(404) });
	let real_url = real.url("/real/42");
	let cdn = MockServer::start(move |r| if r.path == "/obj/42" { Reply::redirect(&real_url) } else { Reply::status(404) });
	let cdn_url = cdn.url("/obj/42");
	let ad = {
		let cdn_url = cdn_url.clone();
		// assetdelivery itself only ever gives the public asset
		MockServer::start(move |r| if r.path == "/v1/assetId/5" { Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")) } else { Reply::json(NEEDS_ACCOUNT) })
	};
	let paths = [behind(&ad, "/v1/assetId/42"), behind(&ad, "/v1/assetId/42/version/3")];
	let broker = MockServer::start(move |r| {
		if r.method == "GET" && paths.contains(&r.path) && r.header("Authorization") == Some(bearer().as_str()) {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::status(404)
		}
	});

	let config = AssetConfig { assetdelivery_headers: vec![("Roblox-Place-Id".into(), "123".into())], ..AssetConfig::new(temp_state("broker")) };
	let a = via_broker(config, endpoints(&ad, &cdn), &broker);
	let fetched = a.fetch("rbxassetid://42").unwrap();
	assert_eq!((fetched.bytes.as_slice(), fetched.source), (MESH, Source::Network));
	assert_eq!(a.fetch("rbxassetid://42?version=3").unwrap().bytes, MESH);

	// the public route as ever, then the same request behind the broker with the token
	assert_eq!(ad.requests().iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["/v1/assetId/42", "/v1/assetId/42/version/3"]);
	let through = broker.requests();
	assert_eq!(through.len(), 2);
	for r in &through {
		assert_eq!(r.header("Authorization"), Some(bearer().as_str()));
		assert_eq!(r.header("Cookie"), None, "no cookie in broker mode");
		assert_eq!(r.header("X-CSRF-TOKEN"), None);
		assert_eq!(r.header("Roblox-Place-Id"), Some("123"));
		assert_eq!(r.header("Accept"), Some("application/json"));
	}
	// a public asset never goes near the broker
	assert_eq!(a.fetch("rbxassetid://5").unwrap().bytes, MESH);
	assert_eq!(broker.requests().len(), 2);
	no_cookie_anywhere(&[&ad, &cdn, &real]);

	// what is reported is the broker and where its credentials came from, not the token
	assert!(a.has_cookie());
	assert_eq!(a.cookie_source().as_deref(), Some("account broker (env ROBLOX_BROKER_TOKEN)"));
	let file = Account::find(Some(Ok((Broker { base: broker.origin(), token: Secret::new(TEST_TOKEN).unwrap() }, BrokerSource::File("creds.json".into())))), || None);
	assert_eq!(file.source.as_deref(), Some("account broker (file creds.json)"));
	// without a broker's credentials the cookie is what it was
	let cookie = Account::find(None, || Some((Secret::new(TEST_COOKIE).unwrap(), CookieSource::Env("ROBLOSECURITY"))));
	assert_eq!(cookie.source.as_deref(), Some("env ROBLOSECURITY"));
	assert!(matches!(cookie.credential, Some(crate::Credential::Cookie(_))));
}

#[test]
fn a_brokers_token_never_crosses_redirects() {
	let real = MockServer::start(|_| Reply::bytes(MESH));
	let real_url = real.url("/real");
	let other = {
		let real_url = real_url.clone();
		MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{real_url}\"}}")))
	};
	let other_url = other.url("/json");
	let ad = {
		let real_url = real_url.clone();
		MockServer::start(move |r| if r.path == "/again/8" { Reply::json(&format!("{{\"location\":\"{real_url}\"}}")) } else { Reply::json(NEEDS_ACCOUNT) })
	};
	let (to_other, relative, to_itself) = (behind(&ad, "/v1/assetId/7"), behind(&ad, "/v1/assetId/8"), behind(&ad, "/v1/assetId/9"));
	let broker = MockServer::start(move |r| {
		if r.path == to_other {
			// to another host
			Reply::redirect(&other_url)
		} else if r.path == relative {
			// a relative one is the forwarded request's: assetdelivery's origin
			Reply::redirect("/again/8")
		} else if r.path == to_itself {
			// back to the broker's own origin
			Reply::redirect(&format!("http://{}/elsewhere/9", r.header("Host").unwrap_or("")))
		} else if r.path == "/elsewhere/9" {
			Reply::json(&format!("{{\"location\":\"{real_url}\"}}"))
		} else {
			Reply::status(404)
		}
	});

	let a = via_broker(AssetConfig::new(temp_state("broker-redirect")), endpoints(&ad, &other), &broker);
	for id in [7, 8, 9] {
		assert_eq!(a.fetch(&format!("rbxassetid://{id}")).unwrap().bytes, MESH, "{id}");
	}
	no_cookie_anywhere(&[&other, &real, &ad]);
	let through = broker.requests();
	assert_eq!(through.iter().filter(|r| r.header("Authorization").is_some()).count(), 3, "the first hop of each");
	let second: Vec<&Recorded> = through.iter().filter(|r| r.path == "/elsewhere/9").collect();
	assert_eq!(second.len(), 1);
	assert_eq!(second[0].header("Authorization"), None, "redirected hop on the broker's own origin");
	assert_eq!(ad.requests().iter().filter(|r| r.path == "/again/8").count(), 1);
}

#[test]
fn warm_batches_go_through_the_broker() {
	let cdn = MockServer::start(|r| if r.path.starts_with("/cdn/") { Reply::bytes(MESH) } else { Reply::status(404) });
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let broker = batch_server_at(behind(&ad, "/v1/assets/batch"), cdn.origin(), Vec::new(), Vec::new());
	let a = via_broker(AssetConfig { max_concurrent_downloads: 8, ..AssetConfig::new(temp_state("broker-warm")) }, endpoints(&ad, &cdn), &broker);
	let downloads: Vec<crate::Download> = (1..=20).map(|id| crate::Download::Asset(format!("rbxassetid://{id}"))).collect();
	let report = a.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed, report.cached), (20, 0, 0));
	let through = broker.requests();
	assert_eq!(through.len(), report.batches);
	for r in &through {
		assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", behind(&ad, "/v1/assets/batch").as_str()));
		assert_eq!(r.header("Authorization"), Some(bearer().as_str()));
		assert_eq!(r.header("Content-Type"), Some("application/json"));
		assert_eq!(r.header("Cookie"), None, "no cookie in broker mode");
		assert_eq!(r.header("X-CSRF-TOKEN"), None);
	}
	assert!(ad.requests().is_empty(), "the batch goes to the broker alone");
	no_cookie_anywhere(&[&cdn]);

	// the broker answers Roblox's CSRF challenge itself: one passed on is no reason to send a token
	let challenged = MockServer::start(|_| Reply { status: 403, headers: vec![("x-csrf-token".into(), "abc".into())], body: Vec::new(), delay: Duration::ZERO });
	let a = via_broker(AssetConfig::new(temp_state("broker-csrf")), endpoints(&ad, &cdn), &challenged);
	let report = a.warm(&downloads[..3], &|_| 0);
	assert_eq!((report.downloaded, report.failed), (0, 3));
	assert_eq!(challenged.requests().len(), 1);
	assert_eq!(challenged.requests()[0].header("X-CSRF-TOKEN"), None);
}

#[test]
fn a_brokers_refusals_say_why() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let (signed_out, revoked, limited, roblox) = (behind(&ad, "/v1/assetId/1"), behind(&ad, "/v1/assetId/2"), behind(&ad, "/v1/assetId/3"), behind(&ad, "/v1/assetId/4"));
	let limits = AtomicUsize::new(0);
	let broker = MockServer::start(move |r| {
		if r.path == signed_out {
			broker_refusal(503, "account_signed_out", "the shared account is signed out")
		} else if r.path == revoked {
			broker_refusal(401, "token_invalid", "this token is unknown or revoked")
		} else if r.path == limited {
			if limits.fetch_add(1, Ordering::SeqCst) == 0 {
				let mut refusal = broker_refusal(429, "rate_limited", "too many requests");
				refusal.headers.push(("Retry-After".into(), "0".into()));
				refusal
			} else {
				Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
			}
		} else if r.path == roblox {
			// no X-Broker-Error: Roblox's own answer about the asset, passed through
			Reply::json(r#"{"errors":[{"code":403,"message":"User is not authorized to access Asset."}]}"#)
		} else {
			Reply::status(404)
		}
	});
	let state = temp_state("broker-refusals");
	let asked = |id: u32| broker.requests().iter().filter(|r| r.path == behind(&ad, &format!("/v1/assetId/{id}"))).count();

	let a = via_broker(AssetConfig::new(&state), endpoints(&ad, &cdn), &broker);
	// the account is down: said in the broker's words, at once
	let error = a.fetch("rbxassetid://1").unwrap_err();
	assert_eq!(error.to_string(), "rbxassetid://1: account broker: the shared account is signed out (HTTP 503)");
	assert_eq!(asked(1), 1, "not retried");
	// the token is refused: that is not Roblox refusing the asset
	let error = a.fetch("rbxassetid://2").unwrap_err();
	assert_eq!(error.to_string(), "rbxassetid://2: account broker: this token is unknown or revoked (HTTP 401)");
	assert!(!format!("{error} {error:?}").contains(TEST_TOKEN));
	// the broker's rate limit is waited out
	assert_eq!(a.fetch("rbxassetid://3").unwrap().bytes, MESH);
	assert_eq!(asked(3), 2);
	// Roblox's refusal through the broker is the asset's, as with the cookie
	assert!(matches!(a.fetch("rbxassetid://4"), Err(AssetError::Http { status: 403, .. })));

	// only Roblox's refusal is remembered: a new token, or the account back, is tried again
	let b = via_broker(AssetConfig::new(&state), endpoints(&ad, &cdn), &broker);
	assert!(b.fetch("rbxassetid://1").is_err());
	assert!(b.fetch("rbxassetid://2").is_err());
	assert!(b.fetch("rbxassetid://4").is_err());
	assert_eq!((asked(1), asked(2), asked(4)), (2, 2, 1));
}

#[test]
fn unusable_broker_credentials_are_said_not_skipped() {
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let found = Err(Unusable { source: BrokerSource::Env, why: "the broker URL must be https (plain http only for 127.0.0.1 or localhost)" });
	let account = Account::find(Some(found), || panic!("a cookie source was read with a broker's credentials"));
	let a = Assets::with_endpoints(AssetConfig::new(temp_state("broker-unusable")), endpoints(&ad, &ad), Some(account), Some(Key::find(None)));
	assert!(!a.has_cookie());
	assert_eq!(a.cookie_source(), None);
	let error = a.fetch("rbxassetid://42").unwrap_err().to_string();
	assert_eq!(error, "rbxassetid://42: account broker credentials (env ROBLOX_BROKER_TOKEN) not usable: the broker URL must be https (plain http only for 127.0.0.1 or localhost)");
	assert_eq!(ad.requests().len(), 1, "the public route alone");
	no_cookie_anywhere(&[&ad]);
}

// --- an Open Cloud API key ------------------------------------------------------------------------------

/// Assets with the fake API key and, when `cookie`, the fake cookie as the account's credential.
fn with_key(config: AssetConfig, endpoints: Endpoints, cookie: bool) -> Assets {
	let account = Account::find(None, || cookie.then(|| (Secret::new(TEST_COOKIE).unwrap(), CookieSource::Env("TEST"))));
	Assets::with_endpoints(config, endpoints, Some(account), Some(test_key()))
}

fn test_key() -> Key {
	Key::find(Some((Secret::plain(TEST_KEY).unwrap(), KeySource::Env)))
}

/// `endpoints` with Open Cloud at `open_cloud`.
fn with_open_cloud(endpoints: Endpoints, open_cloud: &MockServer) -> Endpoints {
	Endpoints { open_cloud: open_cloud.origin(), ..endpoints }
}

/// The key's route for an asset on the mock Open Cloud.
fn key_path(id: &str) -> String {
	format!("/asset-delivery-api/v1/assetId/{id}")
}

fn no_key_anywhere(servers: &[&MockServer]) {
	for server in servers {
		for r in server.requests() {
			assert!(r.header("x-api-key").is_none(), "API key sent to {}{}", server.origin(), r.path);
		}
	}
}

/// Every file under `dir`: neither its name nor its bytes hold the key.
fn key_nowhere_in(dir: &std::path::Path) {
	let mut stack = vec![dir.to_path_buf()];
	let mut files = 0;
	while let Some(dir) = stack.pop() {
		for entry in std::fs::read_dir(&dir).unwrap().flatten() {
			let path = entry.path();
			assert!(!path.to_string_lossy().contains(TEST_KEY), "{}", path.display());
			if path.is_dir() {
				stack.push(path);
			} else {
				files += 1;
				let bytes = std::fs::read(&path).unwrap();
				assert!(!bytes.windows(12).any(|run| TEST_KEY.as_bytes().windows(12).any(|part| part == run)), "the key in {}", path.display());
			}
		}
	}
	assert!(files > 0, "nothing was written to look at");
}

#[test]
fn an_api_key_goes_only_to_open_clouds_route() {
	let real = MockServer::start(|r| if r.path == "/real/42" { Reply::bytes(MESH) } else { Reply::status(404) });
	let real_url = real.url("/real/42");
	let cdn = MockServer::start(move |r| if r.path == "/obj/42" { Reply::redirect(&real_url) } else { Reply::status(404) });
	let cdn_url = cdn.url("/obj/42");
	let ad = {
		let cdn_url = cdn_url.clone();
		MockServer::start(move |r| if r.path == "/v1/assetId/5" { Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")) } else { Reply::json(NEEDS_ACCOUNT) })
	};
	let paths = [key_path("42"), format!("{}/version/3", key_path("42"))];
	let oc = MockServer::start(move |r| {
		if r.method == "GET" && paths.contains(&r.path) && r.header("x-api-key") == Some(TEST_KEY) {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::status(404)
		}
	});
	let config = AssetConfig { assetdelivery_headers: vec![("Roblox-Place-Id".into(), "123".into())], ..AssetConfig::new(temp_state("key")) };
	let a = with_key(config, with_open_cloud(endpoints(&ad, &cdn), &oc), false);
	let fetched = a.fetch("rbxassetid://42").unwrap();
	assert_eq!((fetched.bytes.as_slice(), fetched.source), (MESH, Source::Network));
	assert_eq!(a.fetch("rbxassetid://42?version=3").unwrap().bytes, MESH);

	// the public route first, then the same asset on the key's route with the key alone
	assert_eq!(ad.requests().iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["/v1/assetId/42", "/v1/assetId/42/version/3"]);
	let through = oc.requests();
	assert_eq!(through.len(), 2);
	for r in &through {
		assert_eq!(r.header("x-api-key"), Some(TEST_KEY));
		assert_eq!(r.header("Cookie"), None);
		assert_eq!(r.header("Authorization"), None);
		assert_eq!(r.header("Roblox-Place-Id"), None, "assetdelivery's extra headers stay with assetdelivery");
		assert_eq!(r.header("Accept"), Some("application/json"));
	}
	// a public asset never goes near the key's route
	assert_eq!(a.fetch("rbxassetid://5").unwrap().bytes, MESH);
	assert_eq!(oc.requests().len(), 2);
	no_cookie_anywhere(&[&ad, &cdn, &real]);

	// what is reported is where the key came from, never the key
	assert!(a.has_api_key());
	assert_eq!(a.api_key_source().as_deref(), Some("env ROBLOX_ASSET_DELIVERY_API_KEY"));
	assert!(!a.has_cookie());
	assert!(!format!("{:?}", a.api_key()).contains(TEST_KEY));
}

#[test]
fn the_keys_route_keeps_the_transcode_query() {
	use crate::transcode::query_escape;

	let cdn = MockServer::start(|r| if r.path == "/ktx2" { Reply::bytes(KTX2) } else { Reply::status(404) });
	let ktx_url = cdn.url("/ktx2");
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let oc = MockServer::start(move |r| {
		let Some((path, query)) = r.path.split_once('?') else {
			return Reply::status(400);
		};
		let named = query == format!("contentRepresentationPriorityList={}&doNotFallbackToBaselineRepresentation=true", query_escape(&Transcode::Image.priority_list(0)));
		if path == format!("{}/version/2", key_path("6")) && named && r.header("x-api-key") == Some(TEST_KEY) {
			Reply::json(&format!(r#"{{"location":"{ktx_url}","contentRepresentationSpecifier":{{"format":"ktx2","majorVersion":"7rdo"}}}}"#))
		} else {
			Reply::status(400)
		}
	});
	let a = with_key(AssetConfig::new(temp_state("key-transcode")), with_open_cloud(endpoints(&ad, &cdn), &oc), false);
	let pack = a.fetch_transcode("rbxassetid://6?version=2", Transcode::Image, 0).unwrap();
	assert_eq!((pack.bytes.as_slice(), pack.source), (KTX2, Source::Network));
	// the public route was asked with the same query first
	let public = ad.requests();
	assert_eq!(public.len(), 1);
	assert_eq!(public[0].path.split_once('?').map(|(_, query)| query), oc.requests()[0].path.split_once('?').map(|(_, query)| query));
	no_key_anywhere(&[&ad, &cdn]);
}

#[test]
fn an_api_key_never_crosses_redirects() {
	let real = MockServer::start(|_| Reply::bytes(MESH));
	let real_url = real.url("/real");
	let other = {
		let real_url = real_url.clone();
		MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{real_url}\"}}")))
	};
	let other_url = other.url("/json");
	let ad = {
		let real_url = real_url.clone();
		MockServer::start(move |r| if r.path == "/again/9" { Reply::json(&format!("{{\"location\":\"{real_url}\"}}")) } else { Reply::json(NEEDS_ACCOUNT) })
	};
	let ad_again = ad.url("/again/9");
	let oc = MockServer::start(move |r| {
		let keyed = r.header("x-api-key") == Some(TEST_KEY);
		match r.path.as_str() {
			// to another host
			path if path == key_path("7") && keyed => Reply::redirect(&other_url),
			// back to the key's own route on the same origin, for another asset
			path if path == key_path("8") && keyed => Reply::redirect(&key_path("80")),
			path if path == key_path("80") && !keyed => Reply::json(&format!("{{\"location\":\"{real_url}\"}}")),
			// to assetdelivery
			path if path == key_path("9") && keyed => Reply::redirect(&ad_again),
			_ => Reply::status(404),
		}
	});
	let a = with_key(AssetConfig::new(temp_state("key-redirect")), with_open_cloud(endpoints(&ad, &other), &oc), false);
	for id in [7, 8, 9] {
		assert_eq!(a.fetch(&format!("rbxassetid://{id}")).unwrap().bytes, MESH, "{id}");
	}
	no_key_anywhere(&[&other, &real, &ad]);
	let through = oc.requests();
	assert_eq!(through.iter().filter(|r| r.header("x-api-key").is_some()).count(), 3, "the first hop of each");
	let second: Vec<&Recorded> = through.iter().filter(|r| r.path == key_path("80")).collect();
	assert_eq!(second.len(), 1);
	assert_eq!(second[0].header("x-api-key"), None, "a redirected hop to the key's own route");
}

#[test]
fn the_key_comes_before_the_account() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = {
		let cdn_url = cdn_url.clone();
		MockServer::start(move |r| if r.header("Cookie").is_some() { Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")) } else { Reply::json(NEEDS_ACCOUNT) })
	};
	let oc = {
		let cdn_url = cdn_url.clone();
		MockServer::start(move |r| match r.path.rsplit('/').next().unwrap_or("") {
			"1" => Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")),
			"2" => Reply::json(r#"{"errors":[{"code":403,"message":"User is not authorized to access Asset."}]}"#),
			"3" => Reply::status(404),
			_ => {
				let mut reply = Reply::status(500);
				reply.headers.push(("Retry-After".into(), "0".into()));
				reply
			}
		})
	};
	let cookie_asked = |id: &str| ad.requests().iter().filter(|r| r.path == format!("/v1/assetId/{id}") && r.header("Cookie").is_some()).count();
	let a = with_key(AssetConfig::new(temp_state("key-order")), with_open_cloud(endpoints(&ad, &cdn), &oc), true);

	// the key has it: the cookie is never sent
	assert_eq!(a.fetch("rbxassetid://1").unwrap().bytes, MESH);
	assert_eq!(cookie_asked("1"), 0);
	// refused to the key, or not found to it, or its route down: then the cookie
	for id in ["2", "3", "4"] {
		assert_eq!(a.fetch(&format!("rbxassetid://{id}")).unwrap().bytes, MESH, "{id}");
		assert_eq!(cookie_asked(id), 1, "{id}");
		let public = ad.requests().into_iter().find(|r| r.path == format!("/v1/assetId/{id}") && r.header("Cookie").is_none()).unwrap();
		let keyed = oc.requests().into_iter().find(|r| r.path == key_path(id)).unwrap();
		let cookie = ad.requests().into_iter().find(|r| r.path == format!("/v1/assetId/{id}") && r.header("Cookie").is_some()).unwrap();
		assert!(public.at <= keyed.at && keyed.at <= cookie.at, "{id}: public, then the key, then the cookie");
	}
	no_key_anywhere(&[&ad, &cdn]);

	// a broker's token comes after the key too
	let broker = {
		let cdn_url = cdn_url.clone();
		MockServer::start(move |_| Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")))
	};
	let found = Ok((Broker { base: broker.origin(), token: Secret::new(TEST_TOKEN).unwrap() }, BrokerSource::Env));
	let account = Account::find(Some(found), || panic!("a cookie source was read with a broker's credentials"));
	let b = Assets::with_endpoints(AssetConfig::new(temp_state("key-broker")), with_open_cloud(endpoints(&ad, &cdn), &oc), Some(account), Some(test_key()));
	assert_eq!(b.fetch("rbxassetid://1").unwrap().bytes, MESH);
	assert!(broker.requests().is_empty());
	assert_eq!(b.fetch("rbxassetid://2").unwrap().bytes, MESH);
	assert_eq!(broker.requests().len(), 1);
	no_key_anywhere(&[&broker]);
}

#[test]
fn refused_to_the_key_alone() {
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let oc = MockServer::start(|r| match r.path.rsplit('/').next().unwrap_or("") {
		"2" => Reply::json(r#"{"errors":[{"code":403,"message":"User is not authorized to access Asset.","customErrorCode":19}]}"#),
		_ => Reply::status(404),
	});
	let state = temp_state("key-refused");
	let a = with_key(AssetConfig::new(&state), with_open_cloud(endpoints(&ad, &ad), &oc), false);
	// the key's refusal is the answer, and final: remembered
	let error = a.fetch("rbxassetid://2").unwrap_err();
	assert_eq!(error.to_string(), "rbxassetid://2: HTTP 403, rejected by Roblox moderation");
	// not found to the key, which the public route said needs authentication: that refusal
	let error = a.fetch("rbxassetid://3").unwrap_err();
	assert_eq!(error.to_string(), "rbxassetid://3: HTTP 401, authentication required");
	let asked = (ad.requests().len(), oc.requests().len());
	let again = with_key(AssetConfig::new(&state), with_open_cloud(endpoints(&ad, &ad), &oc), false);
	assert!(again.fetch("rbxassetid://2").is_err());
	assert!(again.fetch("rbxassetid://3").is_err());
	assert_eq!((ad.requests().len(), oc.requests().len()), asked, "both remembered");
}

#[test]
fn the_key_never_shows() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let oc = MockServer::start(move |r| {
		let echo = r.header("x-api-key").unwrap_or("").to_string();
		match r.path.rsplit('/').next().unwrap_or("") {
			"1" => Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")),
			// a server echoing the key, whole or cut, in its error
			"2" => Reply::json(&serde_json::json!({ "errors": [{ "code": 403, "message": format!("Invalid API key {echo}"), "customErrorCode": 77 }] }).to_string()),
			"3" => Reply::json(&serde_json::json!({ "errors": [{ "code": 401, "message": format!("key {}", &echo[..16]), "customErrorCode": 5 }] }).to_string()),
			// a plain one is kept
			_ => Reply::json(r#"{"errors":[{"code":403,"message":"Something new","customErrorCode":77}]}"#),
		}
	});
	let state = temp_state("key-hidden");
	let a = with_key(AssetConfig::new(&state), with_open_cloud(endpoints(&ad, &cdn), &oc), false);
	assert_eq!(a.fetch("rbxassetid://1").unwrap().bytes, MESH);
	let mut shown = Vec::new();
	for id in [2, 3, 4] {
		let error = a.fetch(&format!("rbxassetid://{id}")).unwrap_err();
		shown.push(format!("{error} {error:?}"));
	}
	assert_eq!(shown[0].split(" Http").next().unwrap(), "rbxassetid://2: HTTP 403, \"<message withheld>\"");
	assert_eq!(shown[1].split(" Http").next().unwrap(), "rbxassetid://3: HTTP 401, \"<message withheld>\"");
	assert_eq!(shown[2].split(" Http").next().unwrap(), "rbxassetid://4: HTTP 403, \"Something new\"");
	for text in &shown {
		assert!(!text.contains(&TEST_KEY[..12]), "{text}");
	}
	// a second instance answers from what was remembered: still without it
	let again = with_key(AssetConfig::new(&state), with_open_cloud(endpoints(&ad, &cdn), &oc), false);
	assert!(!again.fetch("rbxassetid://2").unwrap_err().to_string().contains(&TEST_KEY[..12]));
	// the cache, the refusal records and their names never hold it
	key_nowhere_in(&state);
	// debug output of the credentials
	assert!(!format!("{:?} {:?}", a.api_key(), test_key().credential).contains(TEST_KEY));
	no_key_anywhere(&[&ad, &cdn]);
}

#[test]
fn the_keys_rate_limit_is_waited_out() {
	let cdn = MockServer::start(|_| Reply::bytes(MESH));
	let cdn_url = cdn.url("/obj");
	let ad = MockServer::start(|_| Reply::json(NEEDS_ACCOUNT));
	let calls = Arc::new(AtomicUsize::new(0));
	let oc = {
		let calls = calls.clone();
		MockServer::start(move |_| match calls.fetch_add(1, Ordering::SeqCst) {
			// rate limited: try again at once
			0 => {
				let mut reply = Reply::status(429);
				reply.headers.push(("Retry-After".into(), "0".into()));
				reply
			}
			// nothing left of the window for a second
			1 => {
				let mut reply = Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"));
				reply.headers.extend([("x-ratelimit-limit".into(), "1000".into()), ("x-ratelimit-remaining".into(), "0".into()), ("x-ratelimit-reset".into(), "1".into())]);
				reply
			}
			_ => Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}")),
		})
	};
	let a = with_key(AssetConfig::new(temp_state("key-limit")), with_open_cloud(endpoints(&ad, &cdn), &oc), false);
	assert_eq!(a.fetch("rbxassetid://1").unwrap().bytes, MESH);
	assert_eq!(oc.requests().len(), 2, "the 429 was waited out, not the answer");
	assert_eq!(a.fetch("rbxassetid://2").unwrap().bytes, MESH);
	let requests = oc.requests();
	assert_eq!(requests.len(), 3);
	let waited = requests[2].at.duration_since(requests[1].at);
	assert!(waited >= Duration::from_millis(900), "the next request waited for the window: {waited:?}");
	// the public route is never held back by the key's limit
	assert_eq!(ad.requests().len(), 2);
}

#[test]
fn warm_asks_the_key_one_asset_at_a_time() {
	let cdn = MockServer::start(|r| if r.path.starts_with("/cdn/") { Reply::bytes(MESH) } else { Reply::status(404) });
	let cdn_origin = cdn.origin();
	// the batch: 1 is public, the others need more; with the cookie, 3 too
	let ad = MockServer::start(move |r| {
		if r.method != "POST" || r.path != "/v1/assets/batch" {
			return Reply::status(404);
		}
		let cookie = r.header("Cookie").is_some();
		let requests: Vec<serde_json::Value> = serde_json::from_slice(&r.body).unwrap_or_default();
		let replies: Vec<serde_json::Value> = requests
			.iter()
			.map(|request| {
				let id = request["assetId"].as_u64().unwrap_or(0);
				if id == 1 || (cookie && id == 3) {
					serde_json::json!({ "requestId": request["requestId"], "location": format!("{cdn_origin}/cdn/{id}") })
				} else {
					serde_json::json!({ "requestId": request["requestId"], "errors": [{ "code": 403, "message": "no" }] })
				}
			})
			.collect();
		Reply::json(&serde_json::to_string(&replies).unwrap())
	});
	let cdn_url = cdn.url("/cdn/2");
	// the key: 2 only
	let oc = MockServer::start(move |r| {
		if r.method == "GET" && r.path == key_path("2") && r.header("x-api-key") == Some(TEST_KEY) {
			Reply::json(&format!("{{\"location\":\"{cdn_url}\"}}"))
		} else {
			Reply::json(r#"{"errors":[{"code":403,"message":"User is not authorized to access Asset."}]}"#)
		}
	});
	let state = temp_state("key-warm");
	let a = with_key(AssetConfig { max_concurrent_downloads: 4, ..AssetConfig::new(&state) }, with_open_cloud(endpoints(&ad, &cdn), &oc), true);
	let downloads: Vec<crate::Download> = (1..=4).map(|id| crate::Download::Asset(format!("rbxassetid://{id}"))).collect();
	let report = a.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed), (3, 1));

	// a batch without the cookie, one request with the key per refused asset, a batch with the cookie for what the key was refused
	let batches = ad.requests();
	let ids = |r: &Recorded| serde_json::from_slice::<Vec<serde_json::Value>>(&r.body).unwrap().iter().map(|request| request["assetId"].as_u64().unwrap()).collect::<Vec<_>>();
	assert_eq!(batches.len(), 2);
	assert_eq!((batches[0].header("Cookie"), ids(&batches[0])), (None, vec![1, 2, 3, 4]));
	assert_eq!((batches[1].header("Cookie"), ids(&batches[1])), (Some(format!(".ROBLOSECURITY={TEST_COOKIE}").as_str()), vec![3, 4]));
	let mut keyed: Vec<String> = oc.requests().iter().map(|r| r.path.clone()).collect();
	keyed.sort();
	assert_eq!(keyed, [key_path("2"), key_path("3"), key_path("4")]);
	assert!(oc.requests().iter().all(|r| r.method == "GET" && r.header("x-api-key") == Some(TEST_KEY) && r.header("Cookie").is_none()));
	no_key_anywhere(&[&ad, &cdn]);
	// the files are on disk, and 4's refusal is final (every credential tried): nothing is asked again
	let asked = (ad.requests().len(), oc.requests().len());
	for id in 1..=3 {
		assert_eq!(a.fetch(&format!("rbxassetid://{id}")).unwrap().source, Source::DiskCache);
	}
	assert!(matches!(a.fetch("rbxassetid://4"), Err(AssetError::Http { status: 403, .. })));
	assert_eq!((ad.requests().len(), oc.requests().len()), asked);
	key_nowhere_in(&state);

	// without an account, what the key is refused is final as well; nothing goes in a second batch
	let ad_only = batch_server(cdn.origin(), Vec::new(), vec![(2, 403), (4, 403)]);
	let b = with_key(AssetConfig::new(temp_state("key-warm-alone")), with_open_cloud(endpoints(&ad_only, &cdn), &oc), false);
	let report = b.warm(&downloads, &|_| 0);
	assert_eq!((report.downloaded, report.failed), (3, 1));
	assert_eq!(ad_only.requests().len(), 1);
	assert!(b.inner.disk.read_refusal("rbxassetid://4", 0).is_some());
}

#[test]
fn child_processes_never_get_the_key() {
	let mut command = std::process::Command::new("child");
	command.env("ROBLOX_ASSET_DELIVERY_API_KEY", TEST_KEY).env("ROR_COOKIE_ENV", "cookie.env").env("ROR_STUDIO_COOKIE", "1");
	crate::hide_api_key(&mut command);
	let envs: Vec<(String, Option<String>)> = command.get_envs().map(|(name, value)| (name.to_string_lossy().into_owned(), value.map(|value| value.to_string_lossy().into_owned()))).collect();
	assert!(envs.contains(&("ROBLOX_ASSET_DELIVERY_API_KEY".into(), None)), "{envs:?}");
	// the env file's path, which names no secret, is kept as the cookie's always was
	assert!(envs.contains(&("ROR_COOKIE_ENV".into(), Some("cookie.env".into()))));
	// and the switch to Studio's account, which holds none: a child that downloads reads the account itself
	assert!(envs.contains(&("ROR_STUDIO_COOKIE".into(), Some("1".into()))));
}
