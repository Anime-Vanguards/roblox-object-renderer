//! The render service: one warm process per state dir that keeps the GPU device, pipelines,
//! meshes, textures and parsed models loaded while jobs come, and runs many callers' render jobs at
//! once. Quiet 10 s, it hands over to a dormant copy of itself, which holds the GPU's instance and
//! adapter but no device, and makes the device again for the next job ([`State::go_dormant`]).
//!
//! `ror serve` listens on 127.0.0.1 (a port the OS assigns) and records it in
//! `<state dir>/services/<build>.json` and, as the latest service, `<state dir>/service.json`;
//! [`client`] finds the service of its own build, starts it when missing, and sends jobs. Builds
//! sharing a state dir (several checkouts) keep a service each; a rebuild of the same checkout
//! replaces its predecessor's. Requests need an `X-ROR: 1` header.
//!
//! - `GET /health`: version, pid, uptime, job counters, cache sizes
//! - `POST /render`: a [`Job`](crate::job::Job) → its [`JobResult`](crate::job::JobResult)
//! - `POST /video`: a [`VideoJob`](crate::video::VideoJob) → its [`VideoResult`](crate::video::VideoResult)
//! - `POST /batch`: `{"jobs": [Job, ...]}` → `{"results": [JobResult or {"error": ...}, ...]}`,
//!   run concurrently
//! - `POST /shutdown`: exits once running jobs finish
//!
//! Jobs run side by side: each draw (a still's group of views, a clip's frame) starts once the GPU has
//! the memory its targets add free ([`crate::budget::GpuGate`]), and the hot path takes no lock another
//! render holds (the renderer is read-shared, [`State::awake`]).

pub mod client;
pub mod http;
mod warm;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::job::{self, Job, SceneCache};
use crate::output;
use crate::render::Renderer;
use crate::render::gpu::{Gpu, GpuAdapter};

/// What `service.json` records about a running service.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
	pub port: u16,
	pub pid: u32,
	/// The binary the service runs (see [`build_id`]); clients built differently replace it.
	pub build: String,
	pub started: u64,
	/// The executable the starting client ran (the service runs a copy): a newer build of the same
	/// executable replaces this service.
	#[serde(default)]
	pub source: Option<String>,
}

/// The latest service started for this state dir (for HTTP clients that just want a port).
pub fn info_path(state_dir: &Path) -> PathBuf {
	state_dir.join("service.json")
}

/// The services of every build, one record each.
pub fn services_dir(state_dir: &Path) -> PathBuf {
	state_dir.join("services")
}

pub fn build_info_path(state_dir: &Path, build: &str) -> PathBuf {
	services_dir(state_dir).join(format!("{build}.json"))
}

/// Identifies the running binary: version, size and modification time of the executable.
pub fn build_id() -> String {
	let exe = std::env::current_exe().ok();
	let meta = exe.as_ref().and_then(|exe| std::fs::metadata(exe).ok());
	let modified = meta.as_ref().and_then(|meta| meta.modified().ok()).and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|time| time.as_millis()).unwrap_or(0);
	format!("{}-{}-{}", env!("CARGO_PKG_VERSION"), meta.map(|meta| meta.len()).unwrap_or(0), modified)
}

/// What a service is: its binary, whether it may reach the network, and the settings its pictures and
/// counts depend on. A client only ever talks to a service of its own kind. These used to be whatever the
/// service that came up first had, so a command's `--offline` or `ROR_NO_AO=1` was dropped without a word
/// when another was running (the online service's asset checks billed every job: a solo particle frame
/// 524 ms against 73 offline, 2026-11-02), and the way round it was a line in the README.
pub fn service_id(build: &str, offline: bool) -> String {
	id_of(build, offline, std::env::vars())
}

/// The binary of a [`service_id`].
pub fn build_of(id: &str) -> &str {
	id.split('+').next().unwrap_or(id)
}

fn id_of(build: &str, offline: bool, environment: impl Iterator<Item = (String, String)>) -> String {
	let mut id = build.to_string();
	if offline {
		id.push_str("+offline");
	}
	let mut settings: Vec<(String, String)> = environment.filter(|(name, _)| changes_output(name)).collect();
	if !settings.is_empty() {
		settings.sort();
		let hash = settings.iter().flat_map(|(name, value)| name.bytes().chain([b'=']).chain(value.bytes()).chain([0])).fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100_0000_01b3));
		id.push_str(&format!("+{hash:016x}"));
	}
	id
}

/// The settings that change what a service draws or counts. Those for the command alone (where it finds
/// things, how long a service lasts, what it logs) leave a service shareable; a job's own go with the job
/// ([`crate::CALLER_SETTINGS`], `ROR_PARTICLE_SALT`).
fn changes_output(name: &str) -> bool {
	const NAMES: [&str; 12] = ["ROR_BACKEND", "ROR_CALIBRATE_SOURCE", "ROR_CLIP_GEOMETRY", "ROR_FRAME_REUSE", "ROR_GPU_ROOM_PERCENT", "ROR_MAIN_DEPTH", "ROR_ORGANIC_OFFSETS", "ROR_RESIDENCY", "ROR_SCENE_REUSE", "ROR_TRANSCODES", "ROR_VIDEO_ENCODER", "ROR_VIDEO_X264"];
	NAMES.contains(&name) || (name != "ROR_NO_SERVICE" && ["ROR_TUNE_", "ROR_NO_", "ROR_AO_", "ROR_COST_"].iter().any(|prefix| name.starts_with(prefix)))
}

pub struct ServeOptions {
	pub state_dir: PathBuf,
	/// The build the service reports (the client's, when it runs from a copy of the binary).
	pub build: Option<String>,
	/// The executable of the client that started this copy (see [`ServiceInfo::source`]).
	pub source: Option<String>,
	/// Exit after this long without requests.
	pub idle: Duration,
	pub offline: bool,
	/// Start without a GPU device, made for the first job (the copy a quiet service hands over to)
	pub dormant: bool,
}

struct State {
	/// Itself, for the device-lost callback of each device it makes
	me: std::sync::Weak<State>,
	/// The GPU's instance and adapter, kept while dormant: the next device skips the Vulkan loader's
	/// and the driver's start
	adapter: GpuAdapter,
	adapter_info: wgpu::AdapterInfo,
	assets: ror_assets::Assets,
	/// The renderer and its device while awake, none while dormant ([`State::awake`]): read by every job
	/// at once, written only as a wake ends or the service goes dormant
	awake: RwLock<Option<Arc<Awake>>>,
	/// Held while a wake makes the device (one at a time; jobs coming meanwhile wait for that one)
	waking: Mutex<()>,
	/// Devices made (the first included), the last one's milliseconds, and when it was made
	wakes: AtomicU64,
	wake_ms: AtomicU64,
	last_wake: Mutex<Instant>,
	scenes: SceneCache,
	/// The draws admitted within the GPU's free memory
	gpu: crate::budget::GpuGate,
	started: Instant,
	last_activity: Mutex<Instant>,
	/// When the last job ended (requests that run no job, `/health`, leave it)
	last_job: Mutex<Instant>,
	running: AtomicU64,
	done: AtomicU64,
	failed: AtomicU64,
	shutting_down: AtomicBool,
	build: String,
	/// A warm-up burst runs (`POST /warm`, [`warm::burst`]): one at a time
	warming: AtomicBool,
	/// What a dormant copy of this service is started with ([`State::go_dormant`])
	state_dir: PathBuf,
	offline: bool,
	source: Option<String>,
	idle: Duration,
	/// A dormant copy took over: this one exits once its last request is a moment past
	handed_over: AtomicBool,
}

/// What lives on one device: dropped whole when the service goes dormant.
struct Awake {
	renderer: Renderer,
	warmer: std::sync::OnceLock<warm::Warmer>,
}

impl State {
	/// The renderer, its device made again on the kept adapter when the service is dormant (the first
	/// job's waits for it; jobs arriving meanwhile wait for the same one). Awake, a read that every job
	/// shares; a wake holds `waking`, never the renderer's lock, while it makes the device.
	fn awake(&self) -> Result<Arc<Awake>> {
		if let Some(awake) = self.awake.read().as_ref() {
			return Ok(awake.clone());
		}
		let _waking = self.waking.lock();
		if let Some(awake) = self.awake.read().as_ref() {
			return Ok(awake.clone());
		}
		let start = Instant::now();
		let gpu = Arc::new(Gpu::on_adapter(&self.adapter)?);
		let device_ms = start.elapsed().as_secs_f64() * 1000.0;
		let renderer = Renderer::new(gpu, self.assets.clone())?;
		let total_ms = start.elapsed().as_secs_f64() * 1000.0;
		eprintln!("ror: GPU device and renderer made in {total_ms:.0} ms (device {device_ms:.0}, renderer {:.0} ms)", total_ms - device_ms);
		// a lost device (a GPU driver reset) fails every later render: finish the running jobs and exit,
		// so the next request starts a fresh service
		let lost = self.me.clone();
		renderer.gpu.device.set_device_lost_callback(move |reason, message| {
			eprintln!("ror: the GPU device was lost ({reason:?}: {message}); exiting once the running jobs end");
			if let Some(state) = lost.upgrade() {
				state.shutting_down.store(true, Ordering::SeqCst);
			}
		});
		let made = Arc::new(Awake { renderer, warmer: std::sync::OnceLock::new() });
		*self.awake.write() = Some(made.clone());
		self.wakes.fetch_add(1, Ordering::SeqCst);
		self.wake_ms.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
		*self.last_wake.lock() = Instant::now();
		Ok(made)
	}

	/// The renderer if awake, without waking it.
	fn awake_now(&self) -> Option<Arc<Awake>> {
		self.awake.read().clone()
	}

	/// Awake, no job running, and neither a job nor a wake (a warm-up's) for `quiet`.
	fn quiet(&self, awake: &Option<Arc<Awake>>, quiet: Duration) -> bool {
		awake.is_some() && self.running.load(Ordering::SeqCst) == 0 && self.last_job.lock().elapsed() >= quiet && self.last_wake.lock().elapsed() >= quiet
	}

	/// Goes dormant once quiet for `quiet` ([`State::quiet`]): hands over to a dormant copy of itself
	/// and exits, else drops its renderer itself ([`State::sleep`]). A process gives everything back
	/// when it ends; one that only drops its device kept 130-200 MB of commit (mimalloc's pages of
	/// threads gone, freed and never purged; 13 MB of it live), a fresh dormant copy holds ~30 MB.
	/// Returns what it did, for the log.
	fn go_dormant(&self, quiet: Duration) -> Option<String> {
		if !self.quiet(&self.awake.read(), quiet) || self.handed_over.load(Ordering::SeqCst) || self.shutting_down.load(Ordering::SeqCst) {
			return None;
		}
		let left = self.idle.saturating_sub(self.last_job.lock().elapsed());
		if !left.is_zero() && std::env::var("ROR_SERVICE_HANDOVER").map_or(true, |value| value != "0") {
			match client::start_dormant(&self.state_dir, self.offline, &self.build, self.source.clone(), left) {
				Ok(port) => {
					// its records name the copy: new clients go there, this one's last jobs finish here
					self.handed_over.store(true, Ordering::SeqCst);
					self.shutting_down.store(true, Ordering::SeqCst);
					return Some(format!("handed over to a dormant copy on port {port}"));
				}
				Err(error) => eprintln!("ror: starting a dormant copy: {error:#}; dropping the device here instead"),
			}
		}
		let before = crate::process_commit().unwrap_or(0) >> 20;
		self.sleep(quiet).then(|| format!("dormant, the GPU device and every cache on it let go of ({before} -> {} MB)", crate::process_commit().unwrap_or(0) >> 20))
	}

	/// Goes dormant in this process once quiet for `quiet` ([`State::quiet`]): drops the renderer, its
	/// device and every cache on them, the parsed scenes and the downloads held in memory. The process,
	/// the GPU's instance and adapter and the disk caches stay. Returns whether it went dormant now.
	fn sleep(&self, quiet: Duration) -> bool {
		// no wake making a device meanwhile, and no job taking the renderer: a fresh one is never dropped,
		// and a job counted running before it reads the renderer finds it here or wakes it again
		let waking = self.waking.lock();
		let mut awake = self.awake.write();
		if !self.quiet(&awake, quiet) {
			return false;
		}
		let Some(dropped) = awake.take() else {
			return false;
		};
		drop((awake, waking));
		// what renders keep outside the renderer goes too (the preparations' part keys); then the device,
		// once the GPU is done with it (Gpu's drop), unless a warm-up still holds it
		dropped.renderer.release_idle();
		drop(dropped);
		self.scenes.release_all();
		self.assets.release_memory();
		crate::release_free_memory();
		true
	}
}

/// After the last job ends: the allocator's free memory and the GPU's dropped resources go back to
/// the system ([`crate::release_free_memory`]).
const QUIET_COLLECT: Duration = Duration::from_secs(2);
/// After the last job ends: the service goes dormant ([`State::sleep`]), its renderer and GPU device
/// dropped with every cache on them; the next job makes them again on the kept adapter.
/// `ROR_QUIET_RELEASE` (seconds) sets it.
fn quiet_release() -> Duration {
	quiet("ROR_QUIET_RELEASE", 10.0)
}

/// [`quiet_release`] while a game runs ([`crate::gaming`]): the next job wakes a dormant copy in
/// 0.12-0.25 s, and the GPU memory a service holds between jobs (1.1-3.2 GB beside a game proxy,
/// 2026-11-02) is the game's.
const GAMING_QUIET_RELEASE: Duration = Duration::from_secs(2);

/// How often a busy service checks the GPU's headroom while a game runs ([`Renderer::fit_vram`]).
const GAMING_FIT_EVERY: Duration = Duration::from_secs(1);

fn quiet(name: &str, default: f64) -> Duration {
	Duration::from_secs_f64(std::env::var(name).ok().and_then(|value| value.parse::<f64>().ok()).unwrap_or(default))
}

/// How long any request keeps an otherwise idle service from exiting.
const REQUEST_GRACE: Duration = Duration::from_secs(10);

/// How long a service that handed over to its dormant copy waits after its last request before it
/// exits.
const HANDOVER_GRACE: Duration = Duration::from_secs(2);

/// The least time between two reliefs of a service short of memory while jobs run.
const RELIEVE_EVERY: Duration = Duration::from_secs(3);

/// How often the service asks rur's render service how it is ([`crate::render::rur::keep_alive`]).
const RUR_KEEP_ALIVE: Duration = Duration::from_secs(300);

fn unix_now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|time| time.as_secs()).unwrap_or(0)
}

/// The GPU a service of this state dir started on (`<state dir>/gpu.json`): its Vulkan adapter and the
/// Vulkan loader's driver manifests of its vendor. The next service is started with only those
/// drivers loaded (`VK_LOADER_DRIVERS_SELECT`, [`client`]): the loader otherwise starts every GPU's
/// driver to list its devices, an integrated GPU's for 1.4-2.1 s of a 2.3-3.1 s start beside a
/// discrete one (`examples/gpu_start.rs`, 2026-10-10; 0.5-1.1 s with the discrete GPU's alone).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RememberedGpu {
	pub adapter: String,
	pub drivers: String,
}

/// Its file in a state dir (and the content store's, which a state dir without one starts from:
/// output::read_state_file).
pub(crate) const REMEMBERED_GPU: &str = "gpu.json";

/// The GPU remembered for `state_dir` (else the content store's), if any.
pub(crate) fn remembered_gpu(state_dir: &Path) -> Option<RememberedGpu> {
	serde_json::from_slice(&output::read_state_file(state_dir, REMEMBERED_GPU)?).ok()
}

/// Remembers `gpu` for `state_dir` and the content store.
pub(crate) fn write_remembered_gpu(state_dir: &Path, gpu: &RememberedGpu) {
	if let Ok(text) = serde_json::to_string_pretty(gpu) {
		let _ = output::write_state_file(state_dir, REMEMBERED_GPU, text.as_bytes());
	}
}

/// The driver manifests of a vendor's Vulkan driver on Windows (NVIDIA's nv-vk64.json, AMD's
/// amd-vulkan64.json, Intel's igvk64.json), as the loader's globs take them: a `*` only at the ends
/// ("nv-vk*.json" matched nothing, "*nv-vk*" NVIDIA's alone).
fn vendor_drivers(vendor: u32) -> Option<&'static str> {
	match vendor {
		0x10de => Some("*nv-vk*"),
		0x1002 => Some("*amd-vulkan*"),
		0x8086 => Some("*igvk*"),
		_ => None,
	}
}

/// A service started with the remembered GPU's drivers alone (`ROR_VULKAN_ADAPTER` names the adapter
/// expected) exits with 3 when it is not what they gave: the client starts it again with every driver.
/// One started with every driver remembers its GPU for the next.
fn remember_gpu(state_dir: &Path, adapter: &wgpu::AdapterInfo) {
	if let Ok(expected) = std::env::var("ROR_VULKAN_ADAPTER") {
		if adapter.backend != wgpu::Backend::Vulkan || adapter.name != expected {
			eprintln!("ror: the remembered GPU ({expected}) is not the one the Vulkan drivers it was started with gave ({}, {:?}): starting again with every driver", adapter.name, adapter.backend);
			std::process::exit(3);
		}
		return;
	}
	// a GPU whose driver alone did not give it stays started with every driver
	if remembered_gpu(state_dir).is_some_and(|known| known.adapter == adapter.name) {
		return;
	}
	if adapter.backend == wgpu::Backend::Vulkan
		&& adapter.device_type == wgpu::DeviceType::DiscreteGpu
		&& let Some(drivers) = vendor_drivers(adapter.vendor)
	{
		write_remembered_gpu(state_dir, &RememberedGpu { adapter: adapter.name.clone(), drivers: drivers.into() });
	}
}

pub fn serve(options: ServeOptions) -> Result<()> {
	let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding the service port")?;
	// the ffmpeg clips encode with, readied while the device is made (a dormant copy's service before it
	// did; MP4s encode in the process)
	if !options.dormant {
		crate::video::warm_ffmpeg();
	}
	let port = listener.local_addr()?.port();
	let starting = Instant::now();
	let adapter = GpuAdapter::find()?;
	let adapter_info = adapter.adapter.get_info();
	let adapter_ms = starting.elapsed().as_secs_f64() * 1000.0;
	remember_gpu(&options.state_dir, &adapter_info);
	let assets = ror_assets::Assets::new(output::asset_config(options.state_dir.clone(), options.offline));
	// a process holds the folder it runs in (Windows will not delete it): never the caller's, a worktree say, for the service's lifetime
	// (in its plain form: every process it starts inherits it, see output::work_from)
	if let Err(error) = output::work_from(&options.state_dir) {
		eprintln!("ror serve: working from {}: {error}", options.state_dir.display());
	}
	let state = Arc::new_cyclic(|me| State {
		me: me.clone(),
		adapter,
		adapter_info,
		assets,
		awake: RwLock::new(None),
		waking: Mutex::new(()),
		wakes: AtomicU64::new(0),
		wake_ms: AtomicU64::new(0),
		last_wake: Mutex::new(Instant::now()),
		scenes: SceneCache::default(),
		gpu: crate::budget::GpuGate::default(),
		started: Instant::now(),
		last_activity: Mutex::new(Instant::now()),
		last_job: Mutex::new(Instant::now()),
		running: AtomicU64::new(0),
		done: AtomicU64::new(0),
		failed: AtomicU64::new(0),
		shutting_down: AtomicBool::new(false),
		build: options.build.clone().unwrap_or_else(|| service_id(&build_id(), options.offline)),
		warming: AtomicBool::new(false),
		state_dir: options.state_dir.clone(),
		offline: options.offline,
		source: options.source.clone(),
		idle: options.idle,
		handed_over: AtomicBool::new(false),
	});
	// the device the client's first job draws on (a dormant copy makes it for its first job), on a thread
	// of its own: memory a thread allocated and another freed goes back only when the allocating thread
	// runs the allocator again or ends, and this one waits for connections from here on
	if !options.dormant {
		std::thread::scope(|scope| scope.spawn(|| state.awake().map(drop)).join()).map_err(|_| anyhow::anyhow!("making the GPU device panicked"))??;
	}
	let info = ServiceInfo { port, pid: std::process::id(), build: state.build.clone(), started: unix_now(), source: options.source.clone() };
	if let Some(source) = options.source.clone() {
		successor(state.clone(), source, options.state_dir.clone());
	}
	let info_files = [build_info_path(&options.state_dir, &info.build), info_path(&options.state_dir)];
	std::fs::create_dir_all(services_dir(&options.state_dir))?;
	for file in &info_files {
		output::write_atomic(file, serde_json::to_string_pretty(&info)?.as_bytes())?;
	}
	let device = if options.dormant { "dormant until a job".to_string() } else { format!("device and renderer {} ms", state.wake_ms.load(Ordering::SeqCst)) };
	eprintln!("ror: service on 127.0.0.1:{port} (pid {}, {} on {:?}; adapter {adapter_ms:.0} ms, {device})", info.pid, state.adapter_info.name, state.adapter_info.backend);

	{
		let state = state.clone();
		let idle = options.idle;
		std::thread::spawn(move || {
			let mut kept_rur = Instant::now();
			// the job end whose leftovers were collected
			let mut collected = None;
			let quiet_release_set = quiet_release();
			let (mut relieved, mut fitted) = (None, None::<Instant>);
			loop {
				std::thread::sleep(Duration::from_millis(250));
				let last_job = *state.last_job.lock();
				let since_job = last_job.elapsed();
				// beside a game, the GPU memory a quiet service holds goes sooner (GAMING_QUIET_RELEASE)
				let gaming = crate::gaming::active();
				let quiet_release = if gaming { quiet_release_set.min(GAMING_QUIET_RELEASE) } else { quiet_release_set };
				if gaming
					&& state.running.load(Ordering::SeqCst) > 0
					&& fitted.is_none_or(|at| at.elapsed() >= GAMING_FIT_EVERY)
					&& let Some(awake) = state.awake_now()
				{
					// the draws' headroom beside a game, while jobs run (what renders keep for later goes first)
					fitted = Some(Instant::now());
					awake.renderer.fit_vram();
				}
				if state.running.load(Ordering::SeqCst) == 0 {
					if let Some(done) = state.go_dormant(quiet_release) {
						eprintln!("ror: {:.0} s without jobs: {done}", quiet_release.as_secs_f64());
					} else if since_job > QUIET_COLLECT
						&& collected != Some(last_job)
						&& let Some(awake) = state.awake_now()
					{
						collected = Some(last_job);
						// the targets kept past their budget while the jobs drew (reuse::IN_USE_FOR) go
						awake.renderer.trim();
						let _ = awake.renderer.gpu.poll(wgpu::PollType::Poll);
						crate::release_free_memory();
					}
				}
				// the machine short of memory while jobs run (theirs or others'): what the caches keep goes,
				// whatever the quiet steps above would keep (a busy service never goes quiet)
				if state.running.load(Ordering::SeqCst) > 0
					&& relieved.is_none_or(|at: Instant| at.elapsed() > RELIEVE_EVERY)
					&& let Some(free) = crate::available_memory()
				{
					let floor = crate::budget::floor();
					if free < floor
						&& let Some(awake) = state.awake_now()
					{
						relieved = Some(Instant::now());
						let freed = job::relieve(&awake.renderer, &state.scenes, floor - free);
						eprintln!("ror: {} MB free, under the floor of {} MB: let go of {} MB of caches while jobs run", free >> 20, floor >> 20, freed >> 20);
					}
				}
				// rur's render service is kept up while this one has jobs, never by a quiet or dormant one: a
				// keep-alive is a request, and rur-native held 409 MB for as long as ror lived
				if kept_rur.elapsed() > RUR_KEEP_ALIVE {
					kept_rur = Instant::now();
					if state.running.load(Ordering::SeqCst) > 0 || since_job < quiet_release {
						crate::render::rur::keep_alive();
					}
				}
				// idle means no jobs (a status poller must not keep it up); a recent request still holds it for the job it precedes
				let quiet = state.last_activity.lock().elapsed();
				let running = state.running.load(Ordering::SeqCst);
				// handed over: a client that asked this one's health a moment before still sends its job here
				let leaving = state.shutting_down.load(Ordering::SeqCst) && (!state.handed_over.load(Ordering::SeqCst) || quiet > HANDOVER_GRACE);
				if running == 0 && (leaving || (since_job > idle && quiet > REQUEST_GRACE)) {
					// leave the records to a newer service that already replaced them
					for file in &info_files {
						if let Ok(text) = std::fs::read_to_string(file)
							&& let Ok(current) = serde_json::from_str::<ServiceInfo>(&text)
							&& current.pid == std::process::id()
						{
							let _ = std::fs::remove_file(file);
						}
					}
					std::process::exit(0);
				}
			}
		});
	}

	for stream in listener.incoming() {
		let Ok(stream) = stream else {
			continue;
		};
		let state = state.clone();
		std::thread::spawn(move || {
			let _ = stream.set_nodelay(true);
			if let Err(error) = handle(&state, &stream) {
				let body = serde_json::json!({"error": format!("{error:#}")}).to_string();
				let _ = http::write_response(&stream, 500, "application/json", body.as_bytes());
			}
		});
	}
	Ok(())
}

/// Watches the executable that started this service: once a rebuild has replaced it (another build,
/// unchanged for two looks two seconds apart), starts the new build's service
/// (client::start_successor), which has this one exit once it is ready (earlier builds of one
/// executable are superseded). The next call of the new build then finds its service up and its executable
/// already opened once (Windows Defender scans a new executable at its first start, ~0.25 s, and the
/// service's copy of it at its own): the first call after a rebuild 0.9-1.1 s, instead of a call's
/// usual 0.02-0.2 s. Once per service; `ROR_SERVICE_SUCCESSOR=0` turns it off.
fn successor(state: Arc<State>, source: String, state_dir: PathBuf) {
	if std::env::var("ROR_SERVICE_SUCCESSOR").is_ok_and(|value| value == "0") {
		return;
	}
	let _ = std::thread::Builder::new().name("ror-successor".into()).spawn(move || {
		let stamp = |path: &str| -> Option<(u64, u128)> {
			let meta = std::fs::metadata(path).ok()?;
			let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis();
			Some((meta.len(), modified))
		};
		let mut seen = None;
		loop {
			std::thread::sleep(Duration::from_secs(2));
			if state.shutting_down.load(Ordering::SeqCst) {
				return;
			}
			let Some(now) = stamp(&source) else {
				seen = None;
				continue;
			};
			if format!("{}-{}-{}", env!("CARGO_PKG_VERSION"), now.0, now.1) == build_of(&state.build) {
				seen = None;
				continue;
			}
			// a build being written: looked at again
			if seen != Some(now) {
				seen = Some(now);
				continue;
			}
			let build = service_id(&format!("{}-{}-{}", env!("CARGO_PKG_VERSION"), now.0, now.1), state.offline);
			match client::start_successor(&state_dir, Path::new(&source), &build, state.offline) {
				Ok(port) => eprintln!("ror: {source} was rebuilt: its service is on port {port}"),
				Err(error) => eprintln!("ror: {source} was rebuilt; starting its service: {error:#}"),
			}
			return;
		}
	});
}

fn handle(state: &State, stream: &TcpStream) -> Result<()> {
	let request = http::read_request(stream)?;
	*state.last_activity.lock() = Instant::now();
	let local_host = request.header("host").is_some_and(|host| host.starts_with("127.0.0.1:") || host.starts_with("localhost:"));
	if request.header("x-ror") != Some("1") || !local_host {
		return http::write_response(stream, 403, "application/json", br#"{"error":"requests need the X-ROR: 1 header and a local Host"}"#);
	}
	match (request.method.as_str(), request.path.as_str()) {
		("GET", "/health") => {
			// never waits for a wake, nor wakes a dormant service
			let awake = state.awake_now();
			let (awake, caches, free) = match awake {
				Some(awake) => ("awake", awake.renderer.cache_stats(), awake.renderer.gpu.free_memory()),
				None if state.waking.is_locked() => ("waking", serde_json::Value::Null, None),
				None => ("dormant", serde_json::Value::Null, None),
			};
			let body = serde_json::json!({
				"ok": true,
				"build": state.build,
				"offline": state.offline,
				"pid": std::process::id(),
				"uptimeSeconds": state.started.elapsed().as_secs_f64(),
				"adapter": state.adapter_info.name,
				// dormant: no GPU device, nothing cached; the next job makes them again ("wakes", "wakeMs")
				"state": awake,
				"wakes": state.wakes.load(Ordering::SeqCst),
				"wakeMs": state.wake_ms.load(Ordering::SeqCst),
				"jobs": {
					"running": state.running.load(Ordering::SeqCst),
					"done": state.done.load(Ordering::SeqCst),
					"failed": state.failed.load(Ordering::SeqCst),
				},
				// loads within the machine's free memory (crate::budget)
				"loads": {
					"inFlight": crate::budget::in_flight().0,
					"reservedBytes": crate::budget::in_flight().1,
					"floorBytes": crate::budget::floor(),
					"availableBytes": crate::available_memory(),
				},
				// jobs admitted within it, and queued (crate::budget::admit)
				"admission": crate::budget::jobs(),
				// draws admitted within the GPU's free memory (crate::budget::GpuGate), and what it has free
				"draws": state.gpu.status(),
				"gpuFreeBytes": free.map(|free| free.free),
				"gpuTotalBytes": free.map(|free| free.total),
				"commitBytes": crate::process_commit(),
				"caches": caches,
				"shuttingDown": state.shutting_down.load(Ordering::SeqCst),
			});
			http::write_response(stream, 200, "application/json", body.to_string().as_bytes())
		}
		("POST", "/render") => {
			let job: Job = serde_json::from_slice(&request.body).context("job JSON")?;
			let reply = Reply::new(stream, &request)?;
			let (status, body) = crate::for_caller(reply.clone(), || run_job(state, &job));
			reply.answer(status, body.to_string().as_bytes())
		}
		("POST", "/video") => {
			let job: crate::video::VideoJob = serde_json::from_slice(&request.body).context("video job JSON")?;
			let reply = Reply::new(stream, &request)?;
			// a sheet-first clip answers once its sheet is written, and finishes on this thread
			let mut answered: Option<std::path::PathBuf> = None;
			let mut early = |result: &crate::video::VideoResult| {
				if let Ok(body) = serde_json::to_vec(result)
					&& reply.answer(200, &body).is_ok()
				{
					let _ = stream.shutdown(std::net::Shutdown::Both);
				}
				answered = result.pending.clone();
			};
			let (status, body) = crate::for_caller(reply.clone(), || {
				counted(state, |renderer| crate::video::run_with(renderer, &state.scenes, &job, Some(&state.gpu), Some(&mut early)).and_then(|result| Ok(serde_json::to_value(result)?)))
			});
			match answered {
				Some(pending) => {
					if status != 200 {
						let error = body.get("error").and_then(|error| error.as_str()).unwrap_or("the clip failed");
						eprintln!("ror: a sheet-first clip failed after its sheet ({}): {error}", pending.display());
						crate::video::fail_pending(&pending, error);
					}
					Ok(())
				}
				None => reply.answer(status, body.to_string().as_bytes()),
			}
		}
		("POST", "/batch") => {
			#[derive(Deserialize)]
			struct Batch {
				jobs: Vec<Job>,
			}
			let batch: Batch = serde_json::from_slice(&request.body).context("batch JSON")?;
			let results: Vec<serde_json::Value> = std::thread::scope(|scope| {
				let handles: Vec<_> = batch.jobs.iter().map(|job| scope.spawn(move || run_job(state, job).1)).collect();
				handles.into_iter().map(|handle| handle.join().unwrap_or_else(|_| serde_json::json!({"error": "job panicked"}))).collect()
			});
			http::write_response(stream, 200, "application/json", serde_json::json!({ "results": results }).to_string().as_bytes())
		}
		("POST", "/warm") => {
			// one burst at a time, and none while a job draws (it is at working clocks then)
			let warming = state.running.load(Ordering::SeqCst) == 0 && !state.shutting_down.load(Ordering::SeqCst) && !state.warming.swap(true, Ordering::SeqCst);
			http::write_response(stream, 200, "application/json", serde_json::json!({ "ok": true, "warming": warming }).to_string().as_bytes())?;
			let _ = stream.shutdown(std::net::Shutdown::Both);
			if warming {
				// a dormant service makes its device now, while the caller exports or records
				match state.awake() {
					Ok(awake) => {
						let (batches, took) = warm::burst(&awake.renderer.gpu, &awake.warmer, || state.running.load(Ordering::SeqCst) > 0 || state.shutting_down.load(Ordering::SeqCst));
						if std::env::var_os("ROR_WARM_LOG").is_some() {
							let times: Vec<String> = batches.iter().map(|batch| format!("{:.1}", batch.as_secs_f64() * 1000.0)).collect();
							eprintln!("ror: warm-up {:.0} ms, {} batches (ms): {}", took.as_secs_f64() * 1000.0, batches.len(), times.join(" "));
						}
					}
					Err(error) => eprintln!("ror: making the GPU device for a warm-up: {error:#}"),
				}
				state.warming.store(false, Ordering::SeqCst);
			}
			Ok(())
		}
		("POST", "/shutdown") => {
			state.shutting_down.store(true, Ordering::SeqCst);
			http::write_response(stream, 200, "application/json", br#"{"ok":true}"#)
		}
		_ => http::write_response(stream, 404, "application/json", br#"{"error":"not found"}"#),
	}
}

/// A job request's answer and what its job says before it ([`crate::say`]). A client that asks for notes
/// (`X-ROR-Notes: 1`) hears each as it is said, in an answer of [`http::NOTES`] begun with the first
/// (a job that says nothing is answered as before); the client's `X-ROR-Setting: NAME=value` lines are
/// its job's settings ([`crate::CALLER_SETTINGS`]: a command's ROR_MEMORY_FLOOR_GB reaches the service
/// it sends its job to). A job waiting for memory 5-29 minutes said so only to serve.log (2026-10-23).
struct Reply {
	stream: TcpStream,
	notes: bool,
	settings: Vec<(String, String)>,
	/// The notes' answer has begun: its status goes in its last line
	begun: std::cell::Cell<bool>,
}

impl Reply {
	fn new(stream: &TcpStream, request: &http::Request) -> Result<std::rc::Rc<Self>> {
		let settings = request
			.headers
			.iter()
			.filter(|(key, _)| key.eq_ignore_ascii_case("x-ror-setting"))
			.filter_map(|(_, value)| value.split_once('='))
			.filter(|(name, _)| crate::CALLER_SETTINGS.contains(&name.trim()))
			.map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
			.collect();
		Ok(std::rc::Rc::new(Self { stream: stream.try_clone()?, notes: request.header("x-ror-notes") == Some("1"), settings, begun: std::cell::Cell::new(false) }))
	}

	fn answer(&self, status: u16, body: &[u8]) -> Result<()> {
		if self.begun.get() { http::end_notes(&self.stream, status, body) } else { http::write_response(&self.stream, status, "application/json", body) }
	}
}

impl crate::Caller for Reply {
	fn hear(&self, text: &str) {
		if !self.notes {
			return;
		}
		if !self.begun.replace(true) {
			let _ = http::begin_notes(&self.stream);
		}
		let _ = http::write_note(&self.stream, text);
	}

	fn gone(&self) -> bool {
		http::closed(&self.stream)
	}

	fn setting(&self, name: &str) -> Option<String> {
		self.settings.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
	}
}

fn run_job(state: &State, job: &Job) -> (u16, serde_json::Value) {
	counted(state, |renderer| Ok(serde_json::to_value(job::run_gated(renderer, &state.scenes, job, Some(&state.gpu))?)?))
}

/// Runs one job on the renderer (made again if the service is dormant), counting it running, done or
/// failed; a panic fails it alone.
fn counted(state: &State, run: impl FnOnce(&Renderer) -> Result<serde_json::Value>) -> (u16, serde_json::Value) {
	// counted running before it takes the renderer: the service does not go dormant under it
	state.running.fetch_add(1, Ordering::SeqCst);
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let awake = state.awake()?;
		// within the GPU memory the system gives the service before the job adds to it
		awake.renderer.fit_vram();
		run(&awake.renderer)
	}));
	*state.last_job.lock() = Instant::now();
	state.running.fetch_sub(1, Ordering::SeqCst);
	*state.last_activity.lock() = Instant::now();
	match result {
		Ok(Ok(result)) => {
			state.done.fetch_add(1, Ordering::SeqCst);
			(200, result)
		}
		Ok(Err(error)) => {
			state.failed.fetch_add(1, Ordering::SeqCst);
			(400, serde_json::json!({"error": format!("{error:#}")}))
		}
		Err(_) => {
			state.failed.fetch_add(1, Ordering::SeqCst);
			(500, serde_json::json!({"error": "the render panicked"}))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// One request answered on a thread of its own as the service answers a job: the job says `notes`
	/// and reads its caller's ROR_MEMORY_WAIT (the thread's result), then the answer goes.
	fn serve_once(notes: &'static [&'static str]) -> (u16, std::thread::JoinHandle<Option<String>>) {
		let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let server = std::thread::spawn(move || {
			let (stream, _) = listener.accept().unwrap();
			let request = http::read_request(&stream).unwrap();
			let reply = Reply::new(&stream, &request).unwrap();
			let wait = crate::for_caller(reply.clone(), || {
				for note in notes {
					crate::say(note);
				}
				reply.settings.iter().find(|(name, _)| name == "ROR_MEMORY_WAIT").map(|(_, value)| value.clone())
			});
			reply.answer(200, br#"{"ok":true}"#).unwrap();
			wait
		});
		(port, server)
	}

	/// The answer (status and body) or how it went unanswered.
	type Answer = std::result::Result<(u16, Vec<u8>), http::Unanswered>;

	/// A job request; its answer and the notes heard before it.
	fn send(port: u16, header: &str, timeout: Duration) -> (Answer, Vec<String>) {
		let mut heard = Vec::new();
		let result = http::request_hearing(port, "POST", "/render", header, b"{}", timeout, &mut |note| heard.push(note.to_string()));
		(result, heard)
	}

	#[test]
	fn a_service_id_holds_what_the_service_answers_for() {
		let none = || std::iter::empty();
		let (online, offline) = (id_of("0.1.0-1-2", false, none()), id_of("0.1.0-1-2", true, none()));
		assert_eq!(online, "0.1.0-1-2", "a plain service keeps the id older clients know");
		assert_ne!(online, offline);
		assert_eq!(build_of(&offline), "0.1.0-1-2");
		let set = |name: &str, value: &str| vec![(name.to_string(), value.to_string())].into_iter();
		let tuned = id_of("0.1.0-1-2", true, set("ROR_NO_AO", "1"));
		assert_ne!(tuned, offline);
		assert_ne!(tuned, id_of("0.1.0-1-2", true, set("ROR_NO_AO", "2")));
		assert_eq!(build_of(&tuned), "0.1.0-1-2");
		// the command's own business leaves a service shareable
		assert_eq!(id_of("0.1.0-1-2", false, set("ROR_STATE_DIR", "x")), online);
		assert_eq!(id_of("0.1.0-1-2", false, set("ROR_NO_SERVICE", "1")), online);
	}

	#[test]
	fn what_a_job_says_reaches_its_client_ahead_of_the_answer() {
		// a job waiting for memory said so only to serve.log, while its caller saw nothing for minutes
		let (port, server) = serve_once(&["render of a waits for memory: ...", "render of a starts after 3.0 s in the queue"]);
		let (answer, heard) = send(port, "X-ROR: 1\r\nX-ROR-Notes: 1\r\nX-ROR-Setting: ROR_MEMORY_WAIT=5\r\nX-ROR-Setting: PATH=x", Duration::from_secs(10));
		assert_eq!(answer.unwrap(), (200, br#"{"ok":true}"#.to_vec()));
		assert_eq!(heard, ["render of a waits for memory: ...", "render of a starts after 3.0 s in the queue"]);
		// the client's settings are its job's (only those a caller may give)
		assert_eq!(server.join().unwrap().as_deref(), Some("5"));
		// a client that asks for no notes, or a job that says nothing, gets the plain answer
		for (notes, header) in [(&["unheard"][..], "X-ROR: 1"), (&[][..], "X-ROR: 1\r\nX-ROR-Notes: 1")] {
			let (port, server) = serve_once(notes);
			let (answer, heard) = send(port, header, Duration::from_secs(10));
			assert_eq!(answer.unwrap(), (200, br#"{"ok":true}"#.to_vec()));
			assert!(heard.is_empty());
			assert_eq!(server.join().unwrap(), None);
		}
	}

	#[test]
	fn an_unanswered_request_says_how() {
		// sent, then the service went away (a crash) or gave no answer in time
		let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let server = std::thread::spawn(move || {
			let (stream, _) = listener.accept().unwrap();
			http::read_request(&stream).unwrap();
			drop(stream);
			let (stream, _) = listener.accept().unwrap();
			http::read_request(&stream).unwrap();
			std::thread::sleep(Duration::from_millis(1500));
		});
		assert!(matches!(send(port, "X-ROR: 1", Duration::from_secs(10)).0, Err(http::Unanswered::Closed(_))));
		assert!(matches!(send(port, "X-ROR: 1", Duration::from_millis(300)).0, Err(http::Unanswered::TimedOut)));
		server.join().unwrap();
		// nothing listening: not sent, which may be sent again
		assert!(matches!(send(port, "X-ROR: 1", Duration::from_secs(1)).0, Err(http::Unanswered::NotSent(_))));
	}

	#[test]
	fn a_service_sees_its_client_go() {
		let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let client = std::thread::spawn(move || {
			let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
			std::io::Write::write_all(&mut stream, b"POST /render HTTP/1.1\r\nHost: 127.0.0.1\r\nX-ROR: 1\r\nContent-Length: 2\r\n\r\n{}").unwrap();
			std::thread::sleep(Duration::from_millis(300));
		});
		let (stream, _) = listener.accept().unwrap();
		http::read_request(&stream).unwrap();
		assert!(!http::closed(&stream), "the client is there");
		client.join().unwrap();
		let deadline = Instant::now() + Duration::from_secs(5);
		while !http::closed(&stream) {
			assert!(Instant::now() < deadline, "the client's end was never seen");
			std::thread::sleep(Duration::from_millis(20));
		}
	}
}
