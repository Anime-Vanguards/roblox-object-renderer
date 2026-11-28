//! Roblox content resolution and downloads.
//!
//! - `rbxasset://path` → the local Roblox Studio install, looked up in `PlatformContent/pc/`, then
//!   `content/`, then `ExtraContent/` (see `studio.rs` for detection and the evidence for the order)
//! - `rbxassetid://N`, `rbxassetid://N&version=V` (or `?version=V`),
//!   `http(s)://www.roblox.com/asset/?id=N` (and the other asset URL spellings), bare numeric ids →
//!   assetdelivery.roblox.com (public route first, then an Open Cloud API key's route, then the
//!   account cookie; a refusal carries assetdelivery's reason, [`RefusalReason`]), then the CDN
//!   location it returns (never with a credential)
//! - `rbxthumb://type=Asset&id=N&w=W&h=H` → thumbnails.roblox.com
//! - a local file path → the file
//!
//! [`Assets::fetch_transcode`] fetches the pack files of an image or TexturePack asset's KTX2
//! texture transcode instead (what Roblox clients draw; see [`Transcode`]), cached apart from the
//! uploaded image.
//!
//! Every network fetch is cached on disk (shared by every process using the same state dir, written
//! atomically, by other state dirs through hard links: [`AssetConfig::shared`], and by every state
//! dir of the user through the content store: [`AssetConfig::store`], [`store`]) and in memory;
//! concurrent fetches of one URI in a process download it once. Studio install and local files are
//! read in place (not copied to the disk cache) and memory-cached.
//!
//! The account cookie is read from the environment or env files (see [`AssetConfig`]), else, only
//! when env `ROR_STUDIO_COOKIE` is `1`, from where Roblox Studio keeps its signed-in account's (the
//! `studiocookie` module). It is only ever sent in the `Cookie` header of requests to
//! `https://assetdelivery.roblox.com`, never forwarded across redirects, and never printed, logged,
//! cached or written anywhere.
//!
//! With an account broker's credentials (the `broker` module: a service that holds the account's
//! session, used with a personal token) no cookie is read: the requests that would carry it go to the
//! broker with the token instead, which is sent nowhere else and treated like the cookie.
//!
//! An Open Cloud API key (the `apikey` module: env `ROBLOX_ASSET_DELIVERY_API_KEY`, or that line in
//! the cookie's env files) is tried before the account, the narrower credential first: only as
//! `x-api-key`, only to `https://apis.roblox.com/asset-delivery-api/v1/assetId/{id}[/version/{n}]`,
//! only after the public route refused, at the pace of its rate limit (the `ratelimit` module), and
//! kept out of every log, error, cache and child process like the cookie ([`hide_api_key`]).

mod apikey;
mod broker;
mod cache;
mod cookie;
mod http;
mod ratelimit;
mod sniff;
mod stamp;
pub mod store;
mod studio;
mod studiocookie;
mod transcode;
mod uri;
mod warm;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

pub use sniff::sniff_kind;
pub use stamp::{FileSnapshot, FileStamp};
pub use transcode::{Transcode, TranscodeTarget};
pub use uri::{ContentId, Thumbnail, ThumbnailKind};
pub use warm::{BATCH, Download, WarmReport};

use cache::{DiskCache, MemoryCache, Meta, Refusal};
use http::{Client, Credential, Endpoints, Reply};

#[derive(Clone, Debug)]
pub struct AssetConfig {
	/// Root of the state dir; downloads go to `<state_dir>/assets`.
	pub state_dir: PathBuf,
	/// The `content` directory (or its version directory) of the Roblox install whose built-in files ror
	/// reads; auto-detected when `None`: env `ROR_ROBLOX_CONTENT`, then the registry value
	/// `HKCU\Software\Roblox\RobloxStudio\ContentFolder`, then the newest Studio install
	/// (`%LOCALAPPDATA%\Roblox\Versions\*\RobloxStudioBeta.exe`), else the newest Roblox Player install
	/// (`RobloxPlayerBeta.exe`), which ships the same files (`studio.rs`).
	pub studio_content: Option<PathBuf>,
	/// No network access: only the Studio install, local files and the disk cache.
	pub offline: bool,
	/// Re-download network assets even when cached on disk (once per URI per [`Assets`]; repeats
	/// within the same `Assets` are served from memory).
	pub refresh: bool,
	/// Directory of the renderer checkout (its `.env` is a cookie source).
	///
	/// Cookie sources, first match wins: env `ROBLOX_ASSET_DELIVERY_COOKIE`, env `ROBLOSECURITY`,
	/// then the env files named by `ROR_COOKIE_ENV` and `RUR_COOKIE_ENV`, then `<repo_dir>/.env` (lines
	/// `ROBLOX_ASSET_DELIVERY_COOKIE=...` or `ROBLOSECURITY=...`). Read lazily, on the first request
	/// that needs authentication.
	///
	/// Last, only when env `ROR_STUDIO_COOKIE` is `1` and none of those has a cookie: the account signed
	/// in to Roblox Studio on this machine, its cookie read from where Studio keeps it (the Windows
	/// Credential Manager, else the registry key of older builds; on macOS Studio's cookie file).
	///
	/// An account broker's credentials come before all of them, and no cookie is read when they are
	/// found: env `ROBLOX_BROKER_TOKEN` with `ROBLOX_BROKER_URL`, then the JSON file
	/// (`{ "url": ..., "token": ... }`) named by env `ROBLOX_BROKER_CREDENTIALS`, else
	/// `~/.config/roblox-broker/credentials.json`.
	///
	/// An Open Cloud API key is looked for apart from both, and tried before them: env
	/// `ROBLOX_ASSET_DELIVERY_API_KEY`, then a line `ROBLOX_ASSET_DELIVERY_API_KEY=...` in the same env
	/// files as the cookie's.
	pub repo_dir: Option<PathBuf>,
	/// Byte budget of the in-memory cache.
	pub memory_budget: usize,
	/// Concurrent HTTP requests per [`Assets`] (and worker threads of [`Assets::fetch_many`]).
	pub max_concurrent_downloads: usize,
	/// Extra headers for assetdelivery API requests (e.g. `Roblox-Place-Id`). Never sent to the CDN
	/// or across redirects; `Cookie` and `Authorization` are ignored.
	pub assetdelivery_headers: Vec<(String, String)>,
	/// Other state dirs whose downloads this one shares: a file this state dir has not cached but
	/// one of them has is hard-linked in from there (one file on disk for both; copied where no link
	/// can be made) instead of downloaded again, offline too. Cached files are only ever replaced by
	/// a rename, never written into, so a refresh here leaves theirs as it was.
	pub shared: Vec<PathBuf>,
	/// The content store ([`store`], [`store::default_dir`]): a download this state dir lacks is taken
	/// from it (hard-linked in) or from another state dir that used it, and every download goes to it,
	/// so a fresh state dir downloads only what no state dir of the user has. `None` (the default): this
	/// state dir and `shared` alone.
	pub store: Option<PathBuf>,
	/// The GPU formats [`Assets::fetch_transcode`] and [`Assets::warm`] ask texture transcodes for:
	/// a desktop's BC1 / BC3 (the default) or a phone's ETC2. Each is cached under its own keys.
	pub transcode_target: TranscodeTarget,
}

impl AssetConfig {
	/// Defaults with the given state dir: Studio auto-detected, online, 64 MiB memory cache,
	/// 16 concurrent downloads, no shared state dirs, no content store.
	pub fn new(state_dir: impl Into<PathBuf>) -> AssetConfig {
		AssetConfig {
			state_dir: state_dir.into(),
			studio_content: None,
			offline: false,
			refresh: false,
			repo_dir: None,
			// the disk cache reads back in milliseconds what the GPU already holds anyway
			memory_budget: 64 << 20,
			max_concurrent_downloads: 16,
			assetdelivery_headers: Vec::new(),
			shared: Vec::new(),
			store: None,
			transcode_target: TranscodeTarget::Desktop,
		}
	}
}

impl Default for AssetConfig {
	/// State dir `.cache` (relative to the working directory).
	fn default() -> AssetConfig {
		AssetConfig::new(".cache")
	}
}

/// How long a refused download is remembered, by HTTP status: an asset the API will not give (not
/// authorised, not found) a day; a transcode still being made after the waits (406) a few minutes at
/// first, so a fresh process draws the upload at once while Roblox makes it (a game's attack bundles'
/// first renders spent seconds per process asking again), and then for as long as it has been made,
/// up to a day ([`Assets::remember_refusal`]). Other failures are not remembered.
fn refusal_ttl(status: u16) -> Option<u64> {
	match status {
		401 | 403 | 404 | 409 => Some(24 * 3600),
		406 => Some(10 * 60),
		_ => None,
	}
}

/// The error a remembered refusal answers with.
fn refused(uri: &str, refusal: Refusal) -> AssetError {
	match refusal.status {
		404 => AssetError::NotFound(uri.to_string()),
		status => http(uri, status, refusal.reason),
	}
}

/// An [`AssetError::Http`] about `uri`.
fn http(uri: &str, status: u16, reason: Option<RefusalReason>) -> AssetError {
	AssetError::Http { uri: uri.to_string(), status, reason }
}

/// Why assetdelivery refused an asset, from its error's `customErrorCode`, else the status.
///
/// [`Unreviewed`](RefusalReason::Unreviewed), [`Moderated`](RefusalReason::Moderated) and
/// [`Archived`](RefusalReason::Archived) are refused to every account and every place: no cookie,
/// `Roblox-Place-Id` or endpoint gets them, Studio cannot load them either and draws nothing for
/// them (notes/assets.md "Refusals"). The others depend on the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
	/// `customErrorCode` 18, "Asset has not been reviewed"
	Unreviewed,
	/// `customErrorCode` 19, "Asset is not approved for the requester" (Studio logs it as
	/// `AssetDelivery403Moderated`)
	Moderated,
	/// `customErrorCode` 22, "Requested asset is archived"
	Archived,
	/// 401, "Authentication required to access Asset."
	AuthenticationRequired,
	/// 403 (`customErrorCode` 1 or none) or 409, "User is not authorized to access Asset."
	NotAuthorized,
	/// Another `customErrorCode`: assetdelivery's message (printable ASCII, at most 100 characters)
	Other(String),
}

impl RefusalReason {
	/// The reason of an assetdelivery error with refusal `status` (401, 403, 409); `None` for
	/// other statuses.
	fn of(error: Option<&ApiError>, status: u16) -> Option<RefusalReason> {
		if !matches!(status, 401 | 403 | 409) {
			return None;
		}
		let custom = error.and_then(|error| error.custom_code);
		Some(match custom {
			Some(18) => RefusalReason::Unreviewed,
			Some(19) => RefusalReason::Moderated,
			Some(22) => RefusalReason::Archived,
			Some(code) if code != 1 => {
				let message: String = error.and_then(|error| error.message.as_deref()).unwrap_or("").chars().filter(|c| (' '..='~').contains(c)).take(100).collect();
				RefusalReason::Other(if message.trim().is_empty() { format!("error {code}") } else { message.trim().to_string() })
			}
			_ if status == 401 => RefusalReason::AuthenticationRequired,
			_ => RefusalReason::NotAuthorized,
		})
	}

	/// Whether every account and place is refused alike (unreviewed, moderated, archived).
	pub fn universal(&self) -> bool {
		matches!(self, RefusalReason::Unreviewed | RefusalReason::Moderated | RefusalReason::Archived)
	}
}

impl std::fmt::Display for RefusalReason {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			RefusalReason::Unreviewed => f.write_str("not yet reviewed by Roblox moderation"),
			RefusalReason::Moderated => f.write_str("rejected by Roblox moderation"),
			RefusalReason::Archived => f.write_str("archived by its creator"),
			RefusalReason::AuthenticationRequired => f.write_str("authentication required"),
			RefusalReason::NotAuthorized => f.write_str("not authorized for this account"),
			RefusalReason::Other(message) => write!(f, "{message:?}"),
		}
	}
}

/// `, <reason>` after an HTTP status, when there is one.
fn reason_suffix(reason: &Option<RefusalReason>) -> String {
	reason.as_ref().map_or_else(String::new, |reason| format!(", {reason}"))
}

/// Where a fetched asset came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
	StudioInstall,
	LocalFile,
	DiskCache,
	Network,
}

#[derive(Debug)]
pub struct Fetched {
	/// The URI as requested (for repeats served from memory: the spelling of this request).
	pub uri: String,
	pub bytes: Vec<u8>,
	/// Where the bytes were loaded from (memory hits report the original source).
	pub source: Source,
}

impl Fetched {
	/// Sniffed content type, see [`sniff_kind`].
	pub fn kind(&self) -> &'static str {
		sniff_kind(&self.bytes)
	}
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum AssetError {
	#[error("invalid content URI {0:?}")]
	InvalidUri(String),
	#[error("{0} not found")]
	NotFound(String),
	#[error("{uri}: offline and not cached")]
	Offline { uri: String },
	/// An HTTP status, or an assetdelivery error code (401 authentication required, 403 / 409
	/// refused, ...), with why assetdelivery refused it (`, rejected by Roblox moderation`).
	#[error("{uri}: HTTP {status}{}", reason_suffix(.reason))]
	Http { uri: String, status: u16, reason: Option<RefusalReason> },
	#[error("{uri}: {detail}")]
	Network { uri: String, detail: String },
	#[error("{0}")]
	Io(String),
	/// The content has no texture transcode: not an asset id (Studio files, local files,
	/// thumbnails), or assetdelivery answered with something other than a KTX2 file.
	#[error("{0}: no texture transcode")]
	NoTranscode(String),
}

/// The result of one fetch.
pub type FetchResult = Result<Arc<Fetched>, AssetError>;

/// In-flight fetch shared by every caller asking for the same URI.
struct Slot {
	result: Mutex<Option<FetchResult>>,
	ready: Condvar,
}

/// Starts `command` without the Open Cloud API key in its environment (env
/// `ROBLOX_ASSET_DELIVERY_API_KEY`), as every child process ror starts but its own render service. The
/// env files it may be in are named by their paths, which children keep, as they keep the cookie's.
/// Children also keep `ROR_STUDIO_COOKIE`, which holds no credential: a child that downloads (the
/// render service, rur) then reads Studio's account itself, and the cookie never leaves the process
/// that read it.
pub fn hide_api_key(command: &mut std::process::Command) -> &mut std::process::Command {
	command.env_remove(apikey::KEY)
}

/// The Open Cloud API key, when one was found, and where it came from.
struct Key {
	credential: Option<Credential>,
	/// `env NAME`, `file <path>`: never a value
	source: Option<String>,
}

impl Key {
	fn find(found: Option<(cookie::Secret, apikey::KeySource)>) -> Key {
		match found {
			Some((secret, source)) => Key { credential: Some(Credential::ApiKey(secret)), source: Some(source.label()) },
			None => Key { credential: None, source: None },
		}
	}
}

/// Where a download asks for an asset's location, in order while it is refused.
#[derive(Clone, Copy)]
enum Route<'a> {
	/// assetdelivery without a credential
	Public,
	/// Open Cloud's asset delivery route with the API key
	Key(&'a Credential),
	/// assetdelivery with the account's credential (a broker's token, else the cookie)
	Account(&'a Credential),
}

/// What authenticates downloads that need the account, and where it came from.
struct Account {
	credential: Option<Credential>,
	/// `env NAME`, `file <path>`, `account broker (...)`: never a value
	source: Option<String>,
	/// A broker's credentials were found but cannot be used: why. No cookie stands in for them.
	problem: Option<String>,
}

impl Account {
	/// The broker's credentials when there are any (`cookie` is then never asked), else the cookie.
	fn find(broker: Option<broker::Found>, cookie: impl FnOnce() -> Option<(cookie::Secret, cookie::CookieSource)>) -> Account {
		match broker {
			Some(Ok((broker, source))) => Account { credential: Some(Credential::Broker(broker)), source: Some(format!("account broker ({})", source.label())), problem: None },
			Some(Err(unusable)) => Account { credential: None, source: None, problem: Some(format!("account broker credentials ({}) not usable: {}", unusable.source.label(), unusable.why)) },
			None => match cookie() {
				Some((secret, source)) => {
					let source = match source {
						cookie::CookieSource::Env(name) => format!("env {name}"),
						cookie::CookieSource::File(path) => format!("file {}", path.display()),
						cookie::CookieSource::Studio(store) => format!("studio ({})", store.label()),
					};
					Account { credential: Some(Credential::Cookie(secret)), source: Some(source), problem: None }
				}
				None => Account { credential: None, source: None, problem: None },
			},
		}
	}
}

/// The account's cookie: the environment's or an env file's (`configured`), else Roblox Studio's account
/// (`studio`, which [`studiocookie::load`] answers `Off` unless `ROR_STUDIO_COOKIE` is `1`), never read
/// when there is one.
fn account_cookie(configured: impl FnOnce() -> Option<(cookie::Secret, cookie::CookieSource)>, studio: impl FnOnce() -> studiocookie::Lookup) -> Option<(cookie::Secret, cookie::CookieSource)> {
	configured().or_else(|| studio().into_found().map(|(secret, store)| (secret, cookie::CookieSource::Studio(store))))
}

struct Inner {
	config: AssetConfig,
	client: Client,
	disk: DiskCache,
	studio: OnceLock<Option<studio::StudioInstall>>,
	account: OnceLock<Account>,
	key: OnceLock<Key>,
	memory: Mutex<MemoryCache>,
	inflight: Mutex<HashMap<String, Arc<Slot>>>,
	/// Network URIs already re-downloaded in refresh mode
	refreshed: Mutex<HashSet<String>>,
}

/// The shared asset resolver. Cheap to clone; safe to use from many threads at once.
#[derive(Clone)]
pub struct Assets {
	inner: Arc<Inner>,
	offline: bool,
}

impl Assets {
	pub fn new(config: AssetConfig) -> Self {
		Self::with_endpoints(config, Endpoints::production(), None, None)
	}

	fn with_endpoints(config: AssetConfig, endpoints: Endpoints, account: Option<Account>, key: Option<Key>) -> Self {
		let offline = config.offline;
		let client = Client::new(endpoints, config.max_concurrent_downloads, &config.assetdelivery_headers);
		let account_cell = OnceLock::new();
		if let Some(preset) = account {
			let _ = account_cell.set(preset);
		}
		let key_cell = OnceLock::new();
		if let Some(preset) = key {
			let _ = key_cell.set(preset);
		}
		Assets {
			inner: Arc::new(Inner {
				disk: DiskCache::new(&config.state_dir, &config.shared, config.store.as_deref()),
				memory: Mutex::new(MemoryCache::new(config.memory_budget)),
				client,
				studio: OnceLock::new(),
				account: account_cell,
				key: key_cell,
				inflight: Mutex::new(HashMap::new()),
				refreshed: Mutex::new(HashSet::new()),
				config,
			}),
			offline,
		}
	}

	/// A view of these assets sharing the same memory cache, disk cache, credentials and HTTP client,
	/// with network access allowed (`false`) or forbidden (`true`).
	pub fn with_offline(&self, offline: bool) -> Self {
		Self { inner: self.inner.clone(), offline }
	}

	/// Whether this handle is a per-request offline view of an online [`Assets`] instance (so
	/// offline misses should not poison failure caches shared with online callers).
	pub fn is_offline_view(&self) -> bool {
		self.offline && !self.inner.config.offline
	}

	pub fn config(&self) -> &AssetConfig {
		&self.inner.config
	}

	/// The detected Roblox install's `content` directory (Studio's, else the Player's).
	pub fn studio_content_dir(&self) -> Option<&Path> {
		self.studio().map(|s| s.content.as_path())
	}

	/// The file an `rbxasset://` path resolves to, without reading it.
	pub fn studio_file(&self, path: &str) -> Option<PathBuf> {
		match ContentId::parse(&format!("rbxasset://{path}")).ok()? {
			ContentId::StudioAsset(p) => self.studio()?.find(&p),
			_ => None,
		}
	}

	/// Bytes of content held by the in-memory cache.
	pub fn memory_cache_bytes(&self) -> usize {
		self.inner.memory.lock().used()
	}

	/// Lets go of the in-memory cache (a later fetch reads the disk cache); returns its bytes.
	pub fn release_memory(&self) -> usize {
		let mut memory = self.inner.memory.lock();
		let used = memory.used();
		*memory = MemoryCache::new(self.inner.config.memory_budget);
		used
	}

	/// Whether the account can be used: a cookie, or an account broker's credentials, is configured
	/// (reads the sources; never reveals a value).
	pub fn has_cookie(&self) -> bool {
		self.credential().is_some()
	}

	/// A description of where the account's credential comes from (`env ROBLOSECURITY`,
	/// `file <path>`, `studio (Windows Credential Manager)`; with a broker
	/// `account broker (env ROBLOX_BROKER_TOKEN)` or `account broker (file <path>)`), never a value itself.
	pub fn cookie_source(&self) -> Option<String> {
		self.account().source.clone()
	}

	/// Whether an Open Cloud API key is configured (reads the sources; never reveals a value).
	pub fn has_api_key(&self) -> bool {
		self.api_key().is_some()
	}

	/// Where the Open Cloud API key comes from (`env ROBLOX_ASSET_DELIVERY_API_KEY`, `file <path>`),
	/// never a value itself.
	pub fn api_key_source(&self) -> Option<String> {
		self.key().source.clone()
	}

	/// One line per credential source with its status (`not set`, `missing`, `no cookie line`,
	/// `invalid value`, `ok`, ...), in the order they are tried: the Open Cloud API key's, then the
	/// account broker's, then the cookie's, the last of them Roblox Studio's account
	/// (`studio account: off (set ROR_STUDIO_COOKIE=1)`, `not found`, `found (<store>)`,
	/// `unreadable: <why>`). Never contains a value.
	pub fn cookie_diagnostics(&self) -> Vec<String> {
		let repo_dir = self.inner.config.repo_dir.as_deref();
		let mut lines = apikey::diagnose(std::env::var_os(apikey::KEY), &cookie::env_files(repo_dir));
		lines.extend(broker::diagnose(&|name| std::env::var_os(name)));
		lines.extend(cookie::diagnose(repo_dir));
		lines.push(studiocookie::diagnose());
		lines
	}

	fn studio(&self) -> Option<&studio::StudioInstall> {
		self.inner.studio.get_or_init(|| studio::detect(self.inner.config.studio_content.as_deref())).as_ref()
	}

	fn account(&self) -> &Account {
		self.inner.account.get_or_init(|| Account::find(broker::load(), || account_cookie(|| cookie::load(self.inner.config.repo_dir.as_deref()), studiocookie::load)))
	}

	fn credential(&self) -> Option<&Credential> {
		self.account().credential.as_ref()
	}

	fn key(&self) -> &Key {
		self.inner.key.get_or_init(|| Key::find(apikey::load(self.inner.config.repo_dir.as_deref())))
	}

	fn api_key(&self) -> Option<&Credential> {
		self.key().credential.as_ref()
	}

	/// Whether a refusal is final: a credential was tried too (the API key or the account's).
	fn tried_credentials(&self) -> bool {
		self.api_key().is_some() || self.credential().is_some()
	}

	/// The route a refused download asks next: the API key's after the public route, then the account's.
	fn next_route(&self, after: Route<'_>) -> Option<Route<'_>> {
		let key = matches!(after, Route::Public).then(|| self.api_key()).flatten().map(Route::Key);
		key.or_else(|| (!matches!(after, Route::Account(_))).then(|| self.credential()).flatten().map(Route::Account))
	}

	/// assetdelivery's `GET /v1/assetId/{id}[/version/{v}][?query]`.
	fn assetdelivery_url(&self, id: u64, version: Option<u64>, query: Option<&str>) -> String {
		route_url(&self.inner.client.endpoints().assetdelivery, id, version, query)
	}

	/// Open Cloud's `GET /asset-delivery-api/v1/assetId/{id}[/version/{v}][?query]`, the API key's route.
	fn key_url(&self, id: u64, version: Option<u64>, query: Option<&str>) -> String {
		route_url(&format!("{}{}", self.inner.client.endpoints().open_cloud, apikey::PREFIX), id, version, query)
	}

	/// The answer to a refusal the account's credential could not be tried for (there is none, or the
	/// API key was the last route): the refusal, or why the account broker's credentials cannot be used.
	fn refused_without_credential(&self, uri: &str, status: u16, reason: Option<RefusalReason>) -> AssetError {
		match &self.account().problem {
			Some(problem) => AssetError::Network { uri: uri.to_string(), detail: problem.clone() },
			None => http(uri, status, reason),
		}
	}

	/// Fetches a content URI (blocking). Concurrent calls for the same URI share one download.
	pub fn fetch(&self, uri: &str) -> Result<Arc<Fetched>, AssetError> {
		let id = ContentId::parse(uri)?;
		let key = id.canonical();
		if let ContentId::File(path) = &id {
			return self.fetch_local(path, &key, uri).map(|(fetched, _)| fetched);
		}
		self.fetch_keyed(&key, uri, None, || self.load(&id, &key, uri))
	}

	/// Like [`Assets::fetch`], also returning the [`FileStamp`] the read bytes were proven to match
	/// when `uri` names a local file (`None` for non-file URIs, or when a local file moved while
	/// being read so the bytes were not cached).
	pub fn fetch_checked(&self, uri: &str) -> Result<(Arc<Fetched>, Option<FileStamp>), AssetError> {
		let id = ContentId::parse(uri)?;
		let key = id.canonical();
		if let ContentId::File(path) = &id {
			return self.fetch_local(path, &key, uri);
		}
		self.fetch_keyed(&key, uri, None, || self.load(&id, &key, uri)).map(|fetched| (fetched, None))
	}

	/// Fetches a local file against an already-taken [`FileSnapshot`], proving after the read that
	/// the returned bytes match `snapshot.stamp`. Returns `(fetched, true)` when proven (caching
	/// under `snapshot.stamp`), or `(fetched, false)` when the file moved while being read (never
	/// caching the moving bytes).
	pub fn fetch_local_with_snapshot(&self, uri: &str, path: &Path, snapshot: FileSnapshot) -> Result<(Arc<Fetched>, bool), AssetError> {
		let key = ContentId::File(path.to_path_buf()).canonical();
		if let Some(hit) = self.inner.memory.lock().get(&key, Some(snapshot.stamp)) {
			return Ok((respell(hit, uri), true));
		}
		run_before_local_read_hook(path);
		let fetched = self.load_local_file(path, uri)?;
		let verified = snapshot.verify_read(path, &fetched.bytes);
		let fetched = Arc::new(fetched);
		if verified {
			self.inner.memory.lock().insert(&key, fetched.clone(), Some(snapshot.stamp));
		}
		Ok((fetched, verified))
	}

	fn fetch_local(&self, path: &Path, key: &str, uri: &str) -> Result<(Arc<Fetched>, Option<FileStamp>), AssetError> {
		let snapshot = FileStamp::snapshot(path).map_err(|e| match e.kind() {
			std::io::ErrorKind::NotFound => AssetError::NotFound(uri.to_string()),
			_ => AssetError::Io(format!("{uri}: {e}")),
		})?;
		if let Some(hit) = self.inner.memory.lock().get(key, Some(snapshot.stamp)) {
			return Ok((respell(hit, uri), Some(snapshot.stamp)));
		}
		run_before_local_read_hook(path);
		let fetched = self.load_local_file(path, uri)?;
		let verified = snapshot.verify_read(path, &fetched.bytes);
		let fetched = Arc::new(fetched);
		if verified {
			self.inner.memory.lock().insert(key, fetched.clone(), Some(snapshot.stamp));
			Ok((fetched, Some(snapshot.stamp)))
		} else {
			Ok((fetched, None))
		}
	}

	fn load_local_file(&self, path: &Path, uri: &str) -> Result<Fetched, AssetError> {
		let bytes = std::fs::read(path).map_err(|e| match e.kind() {
			std::io::ErrorKind::NotFound => AssetError::NotFound(uri.to_string()),
			_ => AssetError::Io(format!("{uri}: {e}")),
		})?;
		Ok(Fetched { uri: uri.to_string(), bytes, source: Source::LocalFile })
	}

	/// Fetches one pack file of an asset's KTX2 texture transcode (blocking): what Roblox clients draw
	/// in place of the uploaded image. Pack 0 holds the smallest levels and lists the others in its
	/// `pack_N` keys (`ror_image::ktx2_pack_table`); `ror_image::join_packs` joins them. Each pack is
	/// cached under its own key, apart from the uploaded image. Errors with
	/// [`AssetError::NoTranscode`] for content that is not an asset id; a transcode that is still
	/// being made (assetdelivery's 406) is waited for (30 s in all), then reported as HTTP 406. The
	/// transcode is the one made for [`AssetConfig::transcode_target`].
	pub fn fetch_transcode(&self, uri: &str, transcode: Transcode, pack: u32) -> Result<Arc<Fetched>, AssetError> {
		let id = ContentId::parse(uri)?;
		let ContentId::Asset { id: asset, version } = id else {
			return Err(AssetError::NoTranscode(uri.to_string()));
		};
		if pack >= transcode::MAX_PACKS {
			return Err(AssetError::NoTranscode(format!("{uri} pack {pack}")));
		}
		let key = transcode.cache_key(&id.canonical(), pack, self.inner.config.transcode_target);
		self.fetch_keyed(&key, uri, None, || {
			self.load_disk_or_network(&key, uri, || {
				// one still being made when last asked (and waited for then) is asked once more, not waited for again
				let waits: &[u64] = if self.inner.disk.last_refusal(&key).is_some_and(|last| last.status == 406) { &[] } else { &TRANSCODE_WAITS_MS };
				self.download_transcode(asset, version, transcode, pack, uri, waits)
			})
		})
	}

	/// Remembers that the download of `key` was refused with `status` ([`refusal_ttl`]). A transcode
	/// still being made (406) is remembered for as long as it has been answered so, 10 minutes at
	/// least and a day at most: a map's textures whose transcodes Roblox had been "making" all
	/// day cost the first render of the map every 10 minutes 7.5 s of waits.
	fn remember_refusal(&self, key: &str, status: u16, reason: Option<RefusalReason>, now: u64) {
		let Some(ttl) = refusal_ttl(status) else { return };
		let (ttl, since) = if status == 406 {
			let since = self.inner.disk.last_refusal(key).filter(|last| last.status == 406).and_then(|last| last.since).unwrap_or(now);
			(now.saturating_sub(since).clamp(ttl, 24 * 3600), Some(since))
		} else {
			(ttl, None)
		};
		let _ = self.inner.disk.write_refusal(key, &Refusal { status, until: now + ttl, reason, since });
	}

	/// The memory cache, then one shared load per key (concurrent callers wait for the first).
	fn fetch_keyed(&self, key: &str, uri: &str, stamp: Option<FileStamp>, load: impl FnOnce() -> Result<Fetched, AssetError>) -> Result<Arc<Fetched>, AssetError> {
		let key = key.to_string();
		if let Some(hit) = self.inner.memory.lock().get(&key, stamp) {
			return Ok(respell(hit, uri));
		}
		if self.offline {
			// Offline lookups only touch local files and disk cache; bypass `inflight` so an offline
			// miss never fails a concurrent online caller (or waits on one) sharing the same `Assets`.
			let result = load().map(Arc::new);
			if let Ok(fetched) = &result {
				self.inner.memory.lock().insert(&key, fetched.clone(), stamp);
			}
			return result;
		}

		// Join or start the in-flight fetch for this key
		let (slot, leader) = {
			let mut inflight = self.inner.inflight.lock();
			match inflight.get(&key) {
				Some(slot) => (slot.clone(), false),
				None => {
					let slot = Arc::new(Slot { result: Mutex::new(None), ready: Condvar::new() });
					inflight.insert(key.clone(), slot.clone());
					(slot, true)
				}
			}
		};

		if !leader {
			let mut result = slot.result.lock();
			while result.is_none() {
				slot.ready.wait(&mut result);
			}
			return match result.as_ref() {
				Some(Ok(fetched)) => Ok(respell(fetched.clone(), uri)),
				Some(Err(e)) => Err(e.clone()),
				None => Err(AssetError::Io("in-flight fetch vanished".into())),
			};
		}

		// Leader: publish the result even if loading panics
		struct Publish<'a> {
			assets: &'a Assets,
			key: &'a str,
			slot: &'a Slot,
			done: bool,
		}
		impl Drop for Publish<'_> {
			fn drop(&mut self) {
				if !self.done {
					*self.slot.result.lock() = Some(Err(AssetError::Io(format!("{}: fetch panicked", self.key))));
					self.slot.ready.notify_all();
					self.assets.inner.inflight.lock().remove(self.key);
				}
			}
		}
		let mut publish = Publish { assets: self, key: &key, slot: &slot, done: false };

		let result = load().map(Arc::new);
		if let Ok(fetched) = &result {
			// Memory first, so a fetch arriving after the slot is gone hits the cache
			self.inner.memory.lock().insert(&key, fetched.clone(), stamp);
		}
		*slot.result.lock() = Some(result.clone());
		slot.ready.notify_all();
		self.inner.inflight.lock().remove(&key);
		publish.done = true;
		result
	}

	/// Fetches many URIs with bounded parallelism ([`AssetConfig::max_concurrent_downloads`]
	/// workers). Results are in input order.
	pub fn fetch_many<S: AsRef<str> + Sync>(&self, uris: &[S]) -> Vec<FetchResult> {
		let workers = self.inner.config.max_concurrent_downloads.clamp(1, 64).min(uris.len());
		if workers <= 1 {
			return uris.iter().map(|u| self.fetch(u.as_ref())).collect();
		}
		let next = std::sync::atomic::AtomicUsize::new(0);
		let results: Vec<Mutex<Option<FetchResult>>> = uris.iter().map(|_| Mutex::new(None)).collect();
		std::thread::scope(|scope| {
			for _ in 0..workers {
				scope.spawn(|| {
					loop {
						let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						if i >= uris.len() {
							break;
						}
						*results[i].lock() = Some(self.fetch(uris[i].as_ref()));
					}
				});
			}
		});
		results
			.into_iter()
			.enumerate()
			.map(|(i, r)| r.into_inner().unwrap_or_else(|| Err(AssetError::Io(format!("{}: not fetched", uris[i].as_ref())))))
			.collect()
	}

	fn load(&self, id: &ContentId, key: &str, uri: &str) -> Result<Fetched, AssetError> {
		match id {
			ContentId::StudioAsset(path) => {
				let file = self.studio().and_then(|s| s.find(path)).ok_or_else(|| AssetError::NotFound(uri.to_string()))?;
				let bytes = std::fs::read(&file).map_err(|e| AssetError::Io(format!("{uri}: {e}")))?;
				Ok(Fetched { uri: uri.to_string(), bytes, source: Source::StudioInstall })
			}
			ContentId::File(path) => {
				let bytes = std::fs::read(path).map_err(|e| match e.kind() {
					std::io::ErrorKind::NotFound => AssetError::NotFound(uri.to_string()),
					_ => AssetError::Io(format!("{uri}: {e}")),
				})?;
				Ok(Fetched { uri: uri.to_string(), bytes, source: Source::LocalFile })
			}
			ContentId::Asset { .. } | ContentId::Thumbnail(_) => self.load_network(id, key, uri),
		}
	}

	fn load_network(&self, id: &ContentId, key: &str, uri: &str) -> Result<Fetched, AssetError> {
		self.load_disk_or_network(key, uri, || match id {
			ContentId::Asset { id, version } => self.download_asset(*id, *version, uri),
			ContentId::Thumbnail(t) => self.download_thumbnail(t, uri),
			_ => Err(AssetError::InvalidUri(uri.to_string())),
		})
	}

	/// The disk cache under `key` (unless refreshing), else `download` (bytes, host), cached on disk.
	/// A refused download is remembered on disk too ([`refusal_ttl`]), so other processes sharing the
	/// state dir answer it at once instead of asking again.
	fn load_disk_or_network(&self, key: &str, uri: &str, download: impl FnOnce() -> Result<(Vec<u8>, String), AssetError>) -> Result<Fetched, AssetError> {
		let config = &self.inner.config;
		let use_disk = !config.refresh || self.inner.refreshed.lock().contains(key);
		if (use_disk || self.offline)
			&& let Some(bytes) = self.inner.disk.read(key)
		{
			return Ok(Fetched { uri: uri.to_string(), bytes, source: Source::DiskCache });
		}
		if self.offline {
			return Err(AssetError::Offline { uri: uri.to_string() });
		}
		let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
		if !config.refresh
			&& let Some(refusal) = self.inner.disk.read_refusal(key, now)
		{
			return Err(refused(uri, refusal));
		}

		let (bytes, host) = match download() {
			Ok(downloaded) => downloaded,
			Err(error) => {
				let refusal = match &error {
					AssetError::Http { status, reason, .. } => Some((*status, reason.clone())),
					AssetError::NotFound(_) => Some((404, None)),
					_ => None,
				};
				// an authorisation refusal is only final once a credential was tried too
				let final_refusal = |status: u16| !matches!(status, 401 | 403 | 409) || self.tried_credentials();
				if let Some((status, reason)) = refusal
					&& final_refusal(status)
				{
					self.remember_refusal(key, status, reason, now);
				}
				return Err(error);
			}
		};

		let meta = Meta {
			uri: key.to_string(),
			fetched_at: now,
			source_host: host,
			size: bytes.len(),
			kind: sniff_kind(&bytes).to_string(),
		};
		// A cache write failure does not fail the fetch
		let _ = self.inner.disk.write(key, &bytes, &meta);
		if config.refresh {
			self.inner.refreshed.lock().insert(key.to_string());
		}
		Ok(Fetched { uri: uri.to_string(), bytes, source: Source::Network })
	}

	/// assetdelivery `GET /v1/assetId/{id}[/version/{v}]` (public, then Open Cloud's route with the API
	/// key, then with the cookie or through the account broker, each on an authorisation error: [`Assets::find_location`]),
	/// then the CDN location without any. Returns (bytes, CDN host).
	fn download_asset(&self, id: u64, version: Option<u64>, uri: &str) -> Result<(Vec<u8>, String), AssetError> {
		let location = self.find_location(id, version, None, uri, &[])?;
		let reply = self.get(&location, None, "*/*", uri)?;
		if reply.status != 200 {
			return Err(http(uri, reply.status, None));
		}
		Ok((gunzip_if_needed(reply.body, uri)?, reply.host))
	}

	/// assetdelivery `GET /v1/assetId/{id}[/version/{v}]` naming the transcode's pack in
	/// `contentRepresentationPriorityList`, without falling back to the uploaded image (the routes of
	/// [`Assets::find_location`], the same query on each; while the transcode is being made, asked again
	/// after each of `waits`), then the CDN location without a credential. Returns (KTX2 bytes, CDN host).
	fn download_transcode(&self, id: u64, version: Option<u64>, transcode: Transcode, pack: u32, uri: &str, waits: &[u64]) -> Result<(Vec<u8>, String), AssetError> {
		let location = self.find_location(id, version, Some(&transcode_query(transcode, pack, self.inner.config.transcode_target)), uri, waits)?;
		let reply = self.get(&location, None, "*/*", uri)?;
		if reply.status != 200 {
			return Err(http(uri, reply.status, None));
		}
		let body = gunzip_if_needed(reply.body, uri)?;
		if sniff_kind(&body) != "ktx2" {
			return Err(AssetError::NoTranscode(uri.to_string()));
		}
		Ok((body, reply.host))
	}

	/// Where an asset's file is (with a transcode's `query`: only a reply naming its representation
	/// counts, waited for while it is being made, `waits`), asking each route while it is refused: the
	/// public route, then Open Cloud's with the API key, then assetdelivery's with the account's
	/// credential ([`Assets::next_route`]). The key's route failing otherwise (not found to it, rate
	/// limited, down) leaves the account's to try as well.
	fn find_location(&self, id: u64, version: Option<u64>, query: Option<&str>, uri: &str, waits: &[u64]) -> Result<String, AssetError> {
		let mut route = Route::Public;
		// the public route's refusal: the answer when the key's route has none and nothing follows it
		let mut refusal = (401, None);
		let mut waits = waits.iter();
		loop {
			let (url, credential) = match route {
				Route::Public => (self.assetdelivery_url(id, version, query), None),
				Route::Key(key) => (self.key_url(id, version, query), Some(key)),
				Route::Account(account) => (self.assetdelivery_url(id, version, query), Some(account)),
			};
			let located = match query {
				Some(_) => self.transcode_location(&url, credential, uri),
				None => self.asset_location(&url, credential, uri),
			};
			// not found to the key, which the public route said needs authentication: that refusal
			let after_key = |error: AssetError, refusal: &(u16, Option<RefusalReason>)| match error {
				AssetError::NotFound(_) => self.refused_without_credential(uri, refusal.0, refusal.1.clone()),
				error => error,
			};
			match located {
				Ok(Located::Url(location)) => return Ok(location),
				Ok(Located::Generating) => match waits.next() {
					Some(&ms) => std::thread::sleep(std::time::Duration::from_millis(ms)),
					None => return Err(http(uri, 406, None)),
				},
				Ok(Located::Unauthorized(status, reason)) => {
					if matches!(route, Route::Public) {
						refusal = (status, reason.clone());
					}
					match self.next_route(route) {
						Some(next) => route = next,
						None if !matches!(route, Route::Account(_)) => return Err(self.refused_without_credential(uri, status, reason)),
						None => return Err(http(uri, status, reason)),
					}
				}
				Err(error) if matches!(route, Route::Key(_)) && !matches!(error, AssetError::NoTranscode(_)) => match self.next_route(route) {
					Some(next) => route = next,
					None => return Err(after_key(error, &refusal)),
				},
				Err(error) => return Err(error),
			}
		}
	}

	/// A transcode's location: only a reply naming the requested representation counts (without one
	/// the location is the uploaded image's).
	fn transcode_location(&self, url: &str, credential: Option<&Credential>, uri: &str) -> Result<Located, AssetError> {
		let reply = self.get(url, credential, "application/json", uri)?;
		let parsed: Option<TranscodeLocation> = serde_json::from_slice(&reply.body).ok();
		if let Some(parsed) = parsed.as_ref()
			&& let Some(location) = parsed.location.as_ref().filter(|l| !l.is_empty())
		{
			let named = parsed.specifier.as_ref().is_some_and(|s| s.format.as_deref() == Some(transcode::FORMAT));
			return if named { Ok(Located::Url(location.clone())) } else { Err(AssetError::NoTranscode(uri.to_string())) };
		}
		let (status, reason) = error_status(parsed.as_ref().and_then(|p| p.errors.as_deref()), reply.status);
		let reason = withhold_echo(reason, credential);
		match status {
			401 | 403 | 409 => Ok(Located::Unauthorized(status, reason)),
			406 => Ok(Located::Generating),
			404 => Err(AssetError::NotFound(uri.to_string())),
			_ => Err(http(uri, status, None)),
		}
	}

	fn asset_location(&self, url: &str, credential: Option<&Credential>, uri: &str) -> Result<Located, AssetError> {
		let reply = self.get(url, credential, "application/json", uri)?;
		let parsed: Option<AssetLocation> = serde_json::from_slice(&reply.body).ok();
		if let Some(location) = parsed.as_ref().and_then(|p| p.location.clone()).filter(|l| !l.is_empty()) {
			return Ok(Located::Url(location));
		}
		// Errors arrive as HTTP 200 with {"errors":[{"code":403,"customErrorCode":19,...}]} or as plain statuses
		let (status, reason) = error_status(parsed.as_ref().and_then(|p| p.errors.as_deref()), reply.status);
		let reason = withhold_echo(reason, credential);
		match status {
			401 | 403 | 409 => Ok(Located::Unauthorized(status, reason)),
			404 => Err(AssetError::NotFound(uri.to_string())),
			_ => Err(http(uri, status, None)),
		}
	}

	/// thumbnails.roblox.com: JSON with `imageUrl` (retrying while `Pending`), then the image.
	fn download_thumbnail(&self, thumb: &Thumbnail, uri: &str) -> Result<(Vec<u8>, String), AssetError> {
		let endpoints = self.inner.client.endpoints().clone();
		let size = format!("{}x{}", thumb.width, thumb.height);
		let circular = if thumb.circular { "true" } else { "false" };

		let mut target = thumb.id;
		if thumb.kind == ThumbnailKind::GameThumbnail {
			// rbxthumb takes a place id; the API wants the universe
			let reply = self.get(&format!("{}/universes/v1/places/{}/universe", endpoints.universes, thumb.id), None, "application/json", uri)?;
			#[derive(Deserialize)]
			struct Universe {
				#[serde(rename = "universeId")]
				universe_id: Option<u64>,
			}
			target = serde_json::from_slice::<Universe>(&reply.body).ok().and_then(|u| u.universe_id).ok_or_else(|| AssetError::NotFound(uri.to_string()))?;
		}

		let (path, param) = match thumb.kind {
			ThumbnailKind::Asset | ThumbnailKind::FontFamily => ("v1/assets", "assetIds"),
			ThumbnailKind::Avatar => ("v1/users/avatar", "userIds"),
			ThumbnailKind::AvatarBust => ("v1/users/avatar-bust", "userIds"),
			ThumbnailKind::AvatarHeadShot => ("v1/users/avatar-headshot", "userIds"),
			ThumbnailKind::BadgeIcon => ("v1/badges/icons", "badgeIds"),
			ThumbnailKind::BundleThumbnail => ("v1/bundles/thumbnails", "bundleIds"),
			ThumbnailKind::GameIcon => ("v1/games/icons", "universeIds"),
			ThumbnailKind::GamePass => ("v1/game-passes", "gamePassIds"),
			ThumbnailKind::GameThumbnail => ("v1/games/multiget/thumbnails", "universeIds"),
			ThumbnailKind::GroupIcon => ("v1/groups/icons", "groupIds"),
			ThumbnailKind::Outfit => ("v1/users/outfits", "userOutfitIds"),
		};
		let mut url = format!("{}/{path}?{param}={target}&size={size}&format=Png&isCircular={circular}", endpoints.thumbnails);
		if thumb.kind == ThumbnailKind::GameThumbnail {
			url.push_str("&countPerUniverse=1");
		}

		for attempt in 0..10u32 {
			let reply = self.get(&url, None, "application/json", uri)?;
			if reply.status != 200 {
				return Err(http(uri, reply.status, None));
			}
			let parsed: ThumbnailReply = serde_json::from_slice(&reply.body).map_err(|_| AssetError::Network { uri: uri.to_string(), detail: "unexpected thumbnail API reply".into() })?;
			if let Some(error) = parsed.errors.as_ref().and_then(|e| e.first()) {
				let detail = error.message.clone().unwrap_or_else(|| "thumbnail API error".into());
				return Err(AssetError::Network { uri: uri.to_string(), detail });
			}
			let Some(entry) = parsed.data.into_iter().next().map(|d| d.thumbnails.and_then(|t| t.into_iter().next()).unwrap_or(d.entry)) else {
				return Err(AssetError::NotFound(uri.to_string()));
			};
			match (entry.state.as_deref(), entry.image_url) {
				// Blocked thumbnails point at Roblox's "unapproved" placeholder, as in game
				(Some("Completed") | Some("Blocked"), Some(image)) if !image.is_empty() => {
					let reply = self.get(&image, None, "image/png,image/*", uri)?;
					if reply.status != 200 {
						return Err(http(uri, reply.status, None));
					}
					return Ok((gunzip_if_needed(reply.body, uri)?, reply.host));
				}
				(Some("Pending"), _) => std::thread::sleep(std::time::Duration::from_millis(500 + 250 * attempt as u64)),
				(state, _) => {
					return Err(AssetError::Network { uri: uri.to_string(), detail: format!("thumbnail state {}", state.unwrap_or("unknown")) });
				}
			}
		}
		Err(AssetError::Network { uri: uri.to_string(), detail: "thumbnail still pending".into() })
	}

	/// GET with retries; failures become errors about `uri` (never the request URL or headers), and so
	/// does what an account broker refused itself, in its words: that is not Roblox's answer about
	/// the asset, so it is never read or remembered as one.
	fn get(&self, url: &str, credential: Option<&Credential>, accept: &str, uri: &str) -> Result<Reply, AssetError> {
		let reply = self.inner.client.get_retrying(url, credential, accept).map_err(|f| AssetError::Network { uri: uri.to_string(), detail: f.describe() })?;
		match &reply.broker_error {
			Some(said) => Err(AssetError::Network { uri: uri.to_string(), detail: format!("account broker: {said} (HTTP {})", reply.status) }),
			None => Ok(reply),
		}
	}
}

/// `<base>/v1/assetId/{id}[/version/{v}][?query]`.
fn route_url(base: &str, id: u64, version: Option<u64>, query: Option<&str>) -> String {
	let path = match version {
		Some(v) => format!("{base}/v1/assetId/{id}/version/{v}"),
		None => format!("{base}/v1/assetId/{id}"),
	};
	match query {
		Some(query) => format!("{path}?{query}"),
		None => path,
	}
}

/// The query naming a transcode's pack made for `target`, without falling back to the uploaded image.
fn transcode_query(transcode: Transcode, pack: u32, target: TranscodeTarget) -> String {
	format!("contentRepresentationPriorityList={}&doNotFallbackToBaselineRepresentation=true", transcode::query_escape(&transcode.priority_list(pack, target)))
}

/// A refusal's reason, but for a message from the API key's route that could echo the key
/// ([`apikey::may_echo`]): that is never kept, printed or remembered.
fn withhold_echo(reason: Option<RefusalReason>, credential: Option<&Credential>) -> Option<RefusalReason> {
	match (reason, credential.and_then(Credential::api_key)) {
		(Some(RefusalReason::Other(message)), Some(key)) if apikey::may_echo(&message, key.expose()) => Some(RefusalReason::Other("<message withheld>".into())),
		(reason, _) => reason,
	}
}

/// The same entry under the caller's spelling of the URI.
fn respell(fetched: Arc<Fetched>, uri: &str) -> Arc<Fetched> {
	if fetched.uri == uri {
		fetched
	} else {
		Arc::new(Fetched { uri: uri.to_string(), bytes: fetched.bytes.clone(), source: fetched.source })
	}
}

/// CDN objects are stored gzip-compressed; ureq decodes `Content-Encoding: gzip`, this handles
/// bodies that arrive compressed without the header.
fn gunzip_if_needed(body: Vec<u8>, uri: &str) -> Result<Vec<u8>, AssetError> {
	if !body.starts_with(&[0x1f, 0x8b, 0x08]) {
		return Ok(body);
	}
	use std::io::Read;
	let mut out = Vec::with_capacity(body.len() * 3);
	flate2::read::MultiGzDecoder::new(&body[..])
		.take(1 << 30)
		.read_to_end(&mut out)
		.map_err(|e| AssetError::Network { uri: uri.to_string(), detail: format!("gzip: {e}") })?;
	Ok(out)
}

enum Located {
	Url(String),
	/// Refused, other credentials may help: the status / assetdelivery error code, and why
	Unauthorized(u16, Option<RefusalReason>),
	/// A transcode assetdelivery is still making (code 406, "Asset content representation is being
	/// generated")
	Generating,
}

/// Waits between requests for a transcode that is being made: 30 s in all, asked again after 0.5 s
/// and then less and less often. Of 148 transcodes of real textures assetdelivery made after a first
/// request, 94 % were there within 30 s (median 9.8 s; the 7.5 s waited before caught 36 %;
/// notes/assets.md "Transcodes being made"). A Roblox client asks two or three times within a second,
/// then draws nothing until it asks again minutes later.
#[cfg(not(test))]
pub(crate) const TRANSCODE_WAITS_MS: [u64; 14] = [500, 500, 1000, 1000, 1000, 2000, 2000, 2000, 3000, 3000, 3000, 3000, 4000, 4000];
#[cfg(test)]
pub(crate) const TRANSCODE_WAITS_MS: [u64; 4] = [10, 10, 10, 10];

#[derive(Deserialize)]
struct AssetLocation {
	#[serde(alias = "Location")]
	location: Option<String>,
	#[serde(alias = "Errors")]
	errors: Option<Vec<ApiError>>,
}

#[derive(Deserialize)]
struct TranscodeLocation {
	#[serde(alias = "Location")]
	location: Option<String>,
	#[serde(alias = "Errors")]
	errors: Option<Vec<ApiError>>,
	#[serde(rename = "contentRepresentationSpecifier", alias = "ContentRepresentationSpecifier")]
	specifier: Option<RepresentationSpecifier>,
}

#[derive(Deserialize)]
struct RepresentationSpecifier {
	format: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
	code: Option<i64>,
	message: Option<String>,
	/// assetdelivery's finer code (18 unreviewed, 19 moderated, 21 being generated, 22 archived)
	#[serde(rename = "customErrorCode", alias = "CustomErrorCode")]
	custom_code: Option<i64>,
}

/// The status of an assetdelivery reply without a location (its first error's code, else the HTTP
/// status; 502 for a 200 without either) and, for a refusal, why.
fn error_status(errors: Option<&[ApiError]>, http_status: u16) -> (u16, Option<RefusalReason>) {
	let error = errors.and_then(|errors| errors.first());
	let code = error.and_then(|e| e.code).filter(|c| (100..600).contains(c)).map(|c| c as u16);
	let status = code.unwrap_or(if http_status == 200 { 502 } else { http_status });
	(status, RefusalReason::of(error, status))
}

#[derive(Deserialize)]
struct ThumbnailReply {
	#[serde(default)]
	data: Vec<ThumbnailData>,
	errors: Option<Vec<ApiError>>,
}

#[derive(Deserialize)]
struct ThumbnailData {
	#[serde(flatten)]
	entry: ThumbnailEntry,
	/// `games/multiget/thumbnails` nests the entries
	thumbnails: Option<Vec<ThumbnailEntry>>,
}

#[derive(Deserialize)]
struct ThumbnailEntry {
	state: Option<String>,
	#[serde(rename = "imageUrl")]
	image_url: Option<String>,
}

#[cfg(test)]
type LocalReadHook = Box<dyn FnMut() + Send>;

#[cfg(test)]
static BEFORE_LOCAL_READ: OnceLock<Mutex<HashMap<PathBuf, LocalReadHook>>> = OnceLock::new();

#[cfg(test)]
fn run_before_local_read_hook(path: &Path) {
	let Some(hooks) = BEFORE_LOCAL_READ.get() else {
		return;
	};
	let hook = hooks.lock().remove(path);
	if let Some(mut f) = hook {
		f();
	}
}

#[cfg(not(test))]
#[inline]
fn run_before_local_read_hook(_path: &Path) {}

#[cfg(test)]
mod local_race_tests {
	use super::*;

	/// Deterministic A -> B -> A race test on `Assets::fetch` (no sleeps):
	/// file is A when stamped, rewritten to B before `std::fs::read`, then rewritten back to A.
	/// `Assets::fetch` must NOT keep B under A's stamp in `MemoryCache`, so the next fetch of A
	/// returns A.
	#[test]
	fn fetch_does_not_cache_moving_local_file_on_a_to_b_to_a() {
		let dir = std::env::temp_dir().join(format!("ror-assets-race-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		let state = dir.join("state");
		let path = dir.join("asset.bin");
		let bytes_a = b"asset-version-alpha";
		let bytes_b = b"asset-version-bravo";

		let assets = Assets::new(AssetConfig { offline: true, ..AssetConfig::new(&state) });
		let uri = path.to_string_lossy().to_string();

		// Fresh (content-stamped) A -> B -> A:
		std::fs::write(&path, bytes_a).unwrap();
		let hook_path = path.clone();
		BEFORE_LOCAL_READ.get_or_init(Default::default).lock().insert(
			path.clone(),
			Box::new(move || {
				std::fs::write(&hook_path, bytes_b).unwrap();
			}),
		);
		let (during, proven) = assets.fetch_checked(&uri).unwrap();
		assert_eq!(during.bytes.as_slice(), bytes_b, "hook must have swapped file to B before read");
		assert!(proven.is_none(), "moving read must report unproven stamp");
		// Now restore A on disk; MemoryCache must NOT serve B under A's stamp!
		std::fs::write(&path, bytes_a).unwrap();
		let (after, proven_after) = assets.fetch_checked(&uri).unwrap();
		assert_eq!(after.bytes.as_slice(), bytes_a, "subsequent fetch after returning to A must read A, not cached B");
		assert!(proven_after.is_some(), "settled read of A must be proven");

		// Also test with an explicit OS-stamped (aged) snapshot via `fetch_local_with_snapshot`:
		let os_a = FileStamp::os_stamp(&path).unwrap();
		if os_a.trusted {
			let snap_a = FileStamp::snapshot(&path).unwrap();
			// Clear memory cache by using a fresh Assets instance.
			let assets2 = Assets::new(AssetConfig { offline: true, ..AssetConfig::new(&state) });
			let hook_path2 = path.clone();
			BEFORE_LOCAL_READ.get_or_init(Default::default).lock().insert(
				path.clone(),
				Box::new(move || {
					std::fs::write(&hook_path2, bytes_b).unwrap();
				}),
			);
			let (during2, ok2) = assets2.fetch_local_with_snapshot(&uri, &path, snap_a).unwrap();
			assert_eq!(during2.bytes.as_slice(), bytes_b);
			assert!(!ok2, "fetch_local_with_snapshot must reject moving read");
			std::fs::write(&path, bytes_a).unwrap();
			let after2 = assets2.fetch(&uri).unwrap();
			assert_eq!(after2.bytes.as_slice(), bytes_a, "second Assets must not have cached B under A");
		}
		let _ = std::fs::remove_dir_all(&dir);
	}
}
