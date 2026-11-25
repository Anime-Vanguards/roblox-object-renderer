//! Talking to the render service: find the one running this build through
//! `<state dir>/services/<build>.json`, start it (once, under a lock, detached from the caller) when
//! it is missing, send jobs.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};

use super::{ServiceInfo, build_id, build_info_path, build_of, http, service_id, services_dir};
use crate::job::Job;

const START_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a client waits for a render's answer: a dormant service makes its GPU device and every
/// pipeline for the job first, and the first draws on a renderer prepare more (30-63 s, then 7 s, on a
/// software adapter given 4 CPUs).
pub(crate) const RENDER_TIMEOUT: Duration = Duration::from_secs(600);

fn read_info(path: &Path) -> Option<ServiceInfo> {
	let text = std::fs::read_to_string(path).ok()?;
	serde_json::from_str(&text).ok()
}

/// Whether the recorded service process is definitely gone (or its PID was reused by a much newer
/// process). Returns `false` when unsure (e.g. access denied), so live records are never pruned.
#[cfg(windows)]
fn process_dead(info: &ServiceInfo) -> bool {
	use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError};
	use windows_sys::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
	const STILL_ACTIVE: u32 = 259;
	if info.pid == 0 {
		return true;
	}
	unsafe {
		let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, info.pid);
		if handle.is_null() {
			return GetLastError() == ERROR_INVALID_PARAMETER;
		}
		let mut code = 0u32;
		let dead = if GetExitCodeProcess(handle, &mut code) != 0 && code != STILL_ACTIVE {
			true
		} else if info.started > 0 {
			let mut created = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
			let mut exit = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
			let mut kernel = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
			let mut user = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
			if GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) != 0 {
				let ticks = ((created.dwHighDateTime as u64) << 32) | (created.dwLowDateTime as u64);
				// Windows FILETIME epoch is 1601-01-01; Unix epoch offset is 11,644,473,600 s.
				let created_unix = (ticks / 10_000_000).saturating_sub(11_644_473_600);
				created_unix > info.started.saturating_add(60)
			} else {
				false
			}
		} else {
			false
		};
		CloseHandle(handle);
		dead
	}
}

#[cfg(not(windows))]
fn process_dead(_info: &ServiceInfo) -> bool {
	false
}

/// Removes `path` only if it still names the same service record (`pid`, `started`, `build`),
/// closing the race where a newly started service replaces the record between read and unlink.
fn prune_if_same(path: &Path, expected: &ServiceInfo) -> bool {
	let Some(current) = read_info(path) else {
		return false;
	};
	if current.pid == expected.pid && current.started == expected.started && current.build == expected.build {
		std::fs::remove_file(path).is_ok()
	} else {
		false
	}
}

/// Whether `build` currently has a live, ready (non-shutting-down) service recorded in `state_dir`.
pub(super) fn is_live_ready(state_dir: &Path, build: &str) -> bool {
	let record = build_info_path(state_dir, build);
	read_info(&record).is_some_and(|info| !process_dead(&info) && ready(&info, build))
}

/// Every recorded service of the state dir whose process is still alive (or cannot be proved dead);
/// records of exited processes are removed so `status` and `stop` never stall probing dead ports.
fn recorded(state_dir: &Path) -> Vec<ServiceInfo> {
	let Ok(entries) = std::fs::read_dir(services_dir(state_dir)) else {
		return Vec::new();
	};
	entries
		.flatten()
		.filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
		.filter_map(|entry| {
			let path = entry.path();
			let info = read_info(&path)?;
			if process_dead(&info) {
				if prune_if_same(&path, &info) {
					return None;
				}
				let fresh = read_info(&path)?;
				return (!process_dead(&fresh)).then_some(fresh);
			}
			Some(info)
		})
		.collect()
}

fn health(port: u16) -> Option<serde_json::Value> {
	let (status, body) = http::request(port, "GET", "/health", b"", Duration::from_secs(3)).ok()?;
	(status == 200).then(|| serde_json::from_slice(&body).ok()).flatten()
}

/// Up and taking jobs.
fn ready(info: &ServiceInfo, build: &str) -> bool {
	info.build == build && health(info.port).is_some_and(|health| !health.get("shuttingDown").and_then(|value| value.as_bool()).unwrap_or(false))
}

/// The executable this process runs, as services record their starter's (see [`ServiceInfo::source`]).
fn source() -> Option<String> {
	let exe = std::env::current_exe().ok()?;
	Some(std::fs::canonicalize(&exe).unwrap_or(exe).display().to_string())
}

/// The running services of the state dir (one per build) with their health.
pub fn status(state_dir: &Path) -> Vec<(ServiceInfo, serde_json::Value)> {
	recorded(state_dir)
		.into_iter()
		.filter_map(|info| {
			let health = health(info.port)?;
			Some((info, health))
		})
		.collect()
}

/// Asks every service of the state dir to exit once its jobs finish; returns how many were asked.
pub fn stop(state_dir: &Path) -> Result<usize> {
	Ok(recorded(state_dir).iter().filter(|info| http::request(info.port, "POST", "/shutdown", b"{}", Duration::from_secs(3)).is_ok()).count())
}

/// The port of a service running this build, starting one if needed.
/// Online and `--offline` callers share one service; offline jobs pass `X-ROR-Offline: 1`.
pub fn ensure(state_dir: &Path, _offline: bool) -> Result<u16> {
	let build = service_id(&build_id(), false);
	let record = build_info_path(state_dir, &build);
	if let Some(info) = read_info(&record) {
		if process_dead(&info) {
			if !prune_if_same(&record, &info)
				&& let Some(fresh) = read_info(&record)
				&& !process_dead(&fresh)
				&& ready(&fresh, &build)
			{
				return Ok(fresh.port);
			}
		} else if ready(&info, &build) {
			return Ok(info.port);
		}
	}
	std::fs::create_dir_all(services_dir(state_dir))?;
	let lock = services_dir(state_dir).join(format!("{build}.lock"));
	let deadline = Instant::now() + START_TIMEOUT;
	loop {
		match OpenOptions::new().write(true).create_new(true).open(&lock) {
			Ok(_) => break,
			Err(_) => {
				// someone else is starting it: wait for theirs, or take over a stale lock
				if let Some(info) = read_info(&record)
					&& !process_dead(&info)
					&& ready(&info, &build)
				{
					return Ok(info.port);
				}
				let stale = std::fs::metadata(&lock).and_then(|meta| meta.modified()).map(|time| SystemTime::now().duration_since(time).unwrap_or_default() > START_TIMEOUT).unwrap_or(true);
				if stale {
					let _ = std::fs::remove_file(&lock);
					continue;
				}
				if Instant::now() > deadline {
					bail!("timed out waiting for another process to start the render service");
				}
				std::thread::sleep(Duration::from_millis(100));
			}
		}
	}
	// the previous holder may have started one just before releasing the lock
	if let Some(info) = read_info(&record)
		&& !process_dead(&info)
		&& ready(&info, &build)
	{
		let _ = std::fs::remove_file(&lock);
		return Ok(info.port);
	}
	let result = start(state_dir, false, &build);
	let _ = std::fs::remove_file(&lock);
	result
}

/// The service runs from a copy of the binary (`<state dir>/bin/ror-<build>.exe`), so the original
/// can be rebuilt while it runs; copies of other builds are removed when no longer running.
fn service_binary(state_dir: &Path, build: &str) -> Result<PathBuf> {
	service_binary_of(&std::env::current_exe().context("locating ror")?, state_dir, build)
}

/// [`service_binary`] of the executable `exe`.
fn service_binary_of(exe: &Path, state_dir: &Path, build: &str) -> Result<PathBuf> {
	let directory = state_dir.join("bin");
	std::fs::create_dir_all(&directory)?;
	let extension = exe.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
	// one copy of a binary, whichever kinds of service run it
	let name = format!("ror-{}{extension}", build_of(build));
	let target = directory.join(&name);
	if !target.exists() {
		let temporary = directory.join(format!("{name}.{}.tmp", std::process::id()));
		std::fs::copy(exe, &temporary).context("copying the service binary")?;
		if std::fs::rename(&temporary, &target).is_err() {
			let _ = std::fs::remove_file(&temporary);
		}
	}
	if let Ok(entries) = std::fs::read_dir(&directory) {
		for entry in entries.flatten() {
			if entry.file_name().to_string_lossy() != name {
				// fails while that build still runs; it goes on a later start
				let _ = std::fs::remove_file(entry.path());
			}
		}
	}
	Ok(target)
}

fn start(state_dir: &Path, offline: bool, build: &str) -> Result<u16> {
	let exe = service_binary(state_dir, build)?;
	start_from(state_dir, offline, build, exe, source(), None)
}

/// The dormant copy a service hands over to as it goes quiet (service::State::go_dormant): this
/// executable with its build and source, started `--dormant` (the GPU's adapter found, no device) with
/// `idle` left before it exits. Its port once it answers and its records name it.
pub(super) fn start_dormant(state_dir: &Path, offline: bool, build: &str, source: Option<String>, idle: Duration) -> Result<u16> {
	start_from(state_dir, offline, build, std::env::current_exe().context("locating ror")?, source, Some(idle))
}

/// The service of a rebuilt executable `source` (its build `build`), started by the service of the
/// build before it (service::successor): from its own copy, so that `source` is open only while copied
/// (a rebuild then removes it as usual), and recorded as started from `source`, so that the one before
/// it exits once it is up and a later rebuild is noticed. Its executable is started once beforehand,
/// for the virus scanner to see it before the next call of it waits on that.
pub fn start_successor(state_dir: &Path, source: &Path, build: &str, offline: bool) -> Result<u16> {
	let mut scan = std::process::Command::new(source);
	ror_assets::hide_api_key(&mut scan);
	scan.arg("--version").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
	hide_window(&mut scan);
	scan.status().context("starting the rebuilt executable")?;
	let exe = service_binary_of(source, state_dir, build)?;
	start_from(state_dir, offline, build, exe, Some(std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf()).display().to_string()), None)
}

#[cfg(windows)]
fn hide_window(command: &mut std::process::Command) {
	use std::os::windows::process::CommandExt;
	command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_window(_: &mut std::process::Command) {}

/// Starts `exe` as the service; `dormant_idle`: as a dormant copy with that long left before it exits.
fn start_from(state_dir: &Path, offline: bool, build: &str, exe: PathBuf, source: Option<String>, dormant_idle: Option<Duration>) -> Result<u16> {
	let log = OpenOptions::new().create(true).append(true).open(state_dir.join("serve.log")).context("opening serve.log")?;
	// the service is ror itself and downloads for its callers: it keeps the API key, as it keeps the cookie
	let mut command = std::process::Command::new(&exe);
	command.arg("serve").arg("--state-dir").arg(state_dir).arg("--build").arg(build);
	if let Some(source) = &source {
		command.arg("--source").arg(source);
	}
	if offline {
		command.arg("--offline");
	}
	// the settings a command gives its own job reach the service with each job (job_header), never as
	// the service's own: a service started by a command at ROR_MEMORY_FLOOR_GB=200 held every later
	// command's job to that floor
	for name in crate::CALLER_SETTINGS {
		command.env_remove(name);
	}
	// how long it waits for the next job before exiting (`ror serve --idle`)
	if let Some(idle) = dormant_idle {
		command.arg("--dormant").arg("--idle").arg(idle.as_secs().max(1).to_string());
	} else if let Some(idle) = std::env::var("ROR_SERVICE_IDLE").ok().filter(|value| value.trim().parse::<u64>().is_ok()) {
		command.arg("--idle").arg(idle.trim());
	}
	// the Vulkan driver of the GPU the last service started on, alone (super::RememberedGpu), unless
	// the caller chose drivers or a backend itself
	let gpu = super::remembered_gpu(state_dir);
	let filtered = if ["VK_LOADER_DRIVERS_SELECT", "ROR_BACKEND"].iter().all(|name| std::env::var_os(name).is_none())
		&& std::env::var("ROR_VULKAN_FILTER").map_or(true, |value| value != "0")
		&& let Some(gpu) = gpu.as_ref().filter(|gpu| !gpu.drivers.is_empty())
	{
		command.env("VK_LOADER_DRIVERS_SELECT", &gpu.drivers).env("ROR_VULKAN_ADAPTER", &gpu.adapter);
		true
	} else {
		false
	};
	// started in its state dir: a process holds the folder it runs in, and the caller's may be a worktree about to be removed
	// (plain, not verbatim: what it starts before it moves there inherits it, see output::work_from)
	command.current_dir(crate::output::plain_path(state_dir));
	command.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
	let mut child = spawn_detached(&mut command).context("starting the render service")?;
	let pid = child.id();
	let deadline = Instant::now() + START_TIMEOUT;
	let record = build_info_path(state_dir, build);
	while Instant::now() < deadline {
		// the remembered GPU was not what its drivers gave (the service said so in serve.log): every
		// driver from now on, for this GPU
		if filtered && child.try_wait().ok().flatten().is_some() {
			if let Some(gpu) = &gpu {
				super::write_remembered_gpu(state_dir, &super::RememberedGpu { adapter: gpu.adapter.clone(), drivers: String::new() });
			}
			return start(state_dir, offline, build);
		}
		if let Some(info) = read_info(&record)
			&& info.pid == pid
			&& health(info.port).is_some()
		{
			// earlier builds (and legacy `+offline` splits) of the same executable are superseded: let them finish and exit
			for old in recorded(state_dir) {
				let legacy_offline = old.build != build && old.build.split('+').any(|part| part == "offline");
				if (build_of(&old.build) != build_of(build) || legacy_offline) && old.source.is_some() && old.source == source {
					let _ = http::request(old.port, "POST", "/shutdown", b"{}", Duration::from_secs(3));
				}
			}
			return Ok(info.port);
		}
		// the device takes ~0.2 s and the pipelines ~0.03 s: looked for often, so the first job does
		// not wait out a long sleep
		std::thread::sleep(Duration::from_millis(10));
	}
	bail!("the render service did not come up (see {})", state_dir.join("serve.log").display())
}

#[cfg(windows)]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
	use std::os::windows::process::CommandExt;
	const DETACHED_PROCESS: u32 = 0x0000_0008;
	const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
	use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
	use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
	// The child would otherwise inherit this process's stdout / stderr: a caller reading them
	// through a pipe (an agent's shell) would never see them close while the service lives.
	let handles: Vec<_> = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].iter().map(|&which| unsafe { GetStdHandle(which) }).filter(|handle| !handle.is_null()).collect();
	for &handle in &handles {
		unsafe {
			SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
		}
	}
	// Outlive the caller's job object (agent shells kill their process trees) when allowed.
	command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
	let child = match command.spawn() {
		Ok(child) => Ok(child),
		Err(_) => {
			command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
			command.spawn()
		}
	};
	for &handle in &handles {
		unsafe {
			SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
		}
	}
	child
}

#[cfg(not(windows))]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
	command.spawn()
}

fn absolute(path: &Path) -> PathBuf {
	std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A file the service opens, made absolute when it is one here (the service runs in its state dir);
/// an asset id or URI is kept as it is.
fn file_here(value: &str) -> String {
	if Path::new(value).exists() { absolute(Path::new(value)).display().to_string() } else { value.to_string() }
}

/// The folder a job's relative local paths are read from: the one it names, else this working
/// directory (the service, in its state dir, cannot tell it).
fn relative_here(named: Option<PathBuf>) -> Option<PathBuf> {
	named.map(|folder| absolute(&folder)).or_else(|| std::env::current_dir().ok())
}

/// The local render service refused or could not handle the request; the caller should fall back to
/// rendering in-process.
#[derive(Debug)]
pub struct ServiceRefused(pub String);

impl std::fmt::Display for ServiceRefused {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for ServiceRefused {}

/// Whether `error` came from the render service refusing or failing to handle the request (as
/// opposed to a bad input/job error returned by the job itself, or a timeout where the job may
/// still be running in the service).
pub fn is_refused(error: &anyhow::Error) -> bool {
	error.downcast_ref::<ServiceRefused>().is_some()
}

fn refused(message: impl Into<String>) -> anyhow::Error {
	anyhow::Error::new(ServiceRefused(message.into()))
}

/// Sends a job to the service (starting it if needed) and returns its result JSON.
pub fn render(state_dir: &Path, job: &Job, offline: bool) -> Result<serde_json::Value> {
	let mut job = job.clone();
	job.relative_to = relative_here(job.relative_to);
	job.input = file_here(&job.input);
	job.materials_from = job.materials_from.as_deref().map(file_here);
	job.lighting_from = job.lighting_from.as_deref().map(file_here);
	if let Some(map) = &mut job.map {
		map.file = file_here(&map.file);
	}
	job.playback_file = job.playback_file.as_deref().map(absolute);
	job.out = job.out.as_deref().map(absolute);
	post(state_dir, "/render", &serde_json::to_vec(&job)?, offline, RENDER_TIMEOUT)
}

/// Starts this build's service if it is not running and asks it to bring the GPU up to its working
/// clocks for a job about to come ([`super::warm`]); returns its port and whether a burst began.
pub fn warm(state_dir: &Path, offline: bool) -> Result<(u16, bool)> {
	let port = ensure(state_dir, offline)?;
	let (_, response) = http::request(port, "POST", "/warm", b"{}", Duration::from_secs(3))?;
	let value: serde_json::Value = serde_json::from_slice(&response).map_err(|error| anyhow!("service reply: {error}"))?;
	Ok((port, value.get("warming").and_then(|warming| warming.as_bool()).unwrap_or(false)))
}

/// Sends a video job to the service (starting it if needed) and returns its result JSON.
pub fn video(state_dir: &Path, job: &crate::video::VideoJob, offline: bool) -> Result<serde_json::Value> {
	let mut job = job.clone();
	job.relative_to = relative_here(job.relative_to);
	job.input = file_here(&job.input);
	job.materials_from = job.materials_from.as_deref().map(file_here);
	job.lighting_from = job.lighting_from.as_deref().map(file_here);
	if let Some(map) = &mut job.map {
		map.file = file_here(&map.file);
	}
	job.playback_file = job.playback_file.as_deref().map(absolute);
	job.ffmpeg = job.ffmpeg.map(|ffmpeg| if ffmpeg.exists() { absolute(&ffmpeg) } else { ffmpeg });
	job.out = job.out.as_deref().map(absolute);
	post(state_dir, "/video", &serde_json::to_vec(&job)?, offline, Duration::from_secs(3600))
}

/// The request's header lines: ror's, notes wanted (what the job says while it waits is printed on
/// stderr as it comes, service::Reply), per-request offline mode (`X-ROR-Offline: 1`), and this
/// command's settings for its job ([`crate::CALLER_SETTINGS`]).
fn job_header(offline: bool) -> String {
	let mut header = "X-ROR: 1\r\nX-ROR-Notes: 1".to_string();
	if offline {
		header.push_str("\r\nX-ROR-Offline: 1");
	}
	for name in crate::CALLER_SETTINGS {
		if let Ok(value) = std::env::var(name) {
			header.push_str(&format!("\r\nX-ROR-Setting: {name}={}", value.trim()));
		}
	}
	header
}

/// Sends a job; every way it fails says why. The service may have exited between the health check and
/// the request, or crashed on it: a request not sent, or one whose connection closed unanswered, is sent
/// again once. One sent and unanswered within `timeout` is not: its job may still run there (a job sent
/// twice was drawn twice, once for nobody).
fn post(state_dir: &Path, route: &str, body: &[u8], offline: bool, timeout: Duration) -> Result<serde_json::Value> {
	let log = state_dir.join("serve.log");
	let header = job_header(offline);
	let mut attempt = 0;
	loop {
		attempt += 1;
		let port = ensure(state_dir, offline).map_err(|error| refused(format!("{error:#}")))?;
		match post_once(port, route, &header, body, timeout, &log) {
			PostAttempt::Ok(value) => return Ok(value),
			PostAttempt::Retry(message) if attempt < 2 => eprintln!("ror: {message}; sending the job again"),
			PostAttempt::Retry(message) => return Err(refused(message)),
			PostAttempt::Refused(message) => return Err(refused(message)),
			PostAttempt::JobError(error) => return Err(error),
		}
	}
}

enum PostAttempt {
	Ok(serde_json::Value),
	Retry(String),
	Refused(String),
	JobError(anyhow::Error),
}

fn post_once(port: u16, route: &str, header: &str, body: &[u8], timeout: Duration, log: &Path) -> PostAttempt {
	match http::request_hearing(port, "POST", route, header, body, timeout, &mut |note| eprintln!("ror: {note}")) {
		Ok((status, response)) => {
			let value: serde_json::Value = match serde_json::from_slice(&response) {
				Ok(value) => value,
				Err(error) => return PostAttempt::Refused(format!("the render service's answer ({status}) is not JSON ({error}): {}", String::from_utf8_lossy(&response))),
			};
			if status != 200 {
				let message = match value.get("error").and_then(|error| error.as_str()).filter(|error| !error.trim().is_empty()) {
					Some(error) => error.to_string(),
					None => format!("the render service answered {status} without saying why: {value} (its log: {})", log.display()),
				};
				// HTTP 400 comes from the job itself failing validation or asset lookup; any other
				// non-200 status (403, 404, 413, 500, 503, ...) means the service refused or broke.
				return if status == 400 { PostAttempt::JobError(anyhow!("{message}")) } else { PostAttempt::Refused(format!("HTTP {status}: {message}")) };
			}
			PostAttempt::Ok(value)
		}
		Err(http::Unanswered::NotSent(error)) => PostAttempt::Retry(format!("could not send the job to the render service on port {port}: {error:#} (its log: {})", log.display())),
		Err(http::Unanswered::Closed(why)) => PostAttempt::Retry(format!("the render service on port {port} closed the connection without an answer ({why}): it exited or crashed (its log: {})", log.display())),
		Err(http::Unanswered::TimedOut) => PostAttempt::JobError(anyhow!(
			"the render service on port {port} gave no answer in {:.0} s; the job may still be running there (its log: {}). `ror service status` shows its jobs and memory",
			timeout.as_secs_f64(),
			log.display()
		)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A job sent to the service reads its relative local paths from the folder it names, made
	/// absolute, else from this working directory, never the service's state dir.
	#[test]
	fn a_job_names_where_its_relative_paths_are_read_from() {
		let here = std::env::current_dir().unwrap();
		assert_eq!(relative_here(None), Some(here.clone()));
		assert_eq!(relative_here(Some(PathBuf::from("project"))), Some(here.join("project")));
		let named = std::env::temp_dir().join("named");
		assert_eq!(relative_here(Some(named.clone())), Some(named));
	}

	#[test]
	fn job_header_carries_offline_only_when_requested() {
		assert!(!job_header(false).contains("X-ROR-Offline"));
		assert!(job_header(true).contains("X-ROR-Offline: 1"));
	}

	#[cfg(windows)]
	#[test]
	fn stale_dead_pid_records_are_pruned_and_live_records_stay() {
		let state = std::env::temp_dir().join(format!("ror-prune-records-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		let dir = services_dir(&state);
		std::fs::create_dir_all(&dir).unwrap();
		let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
		let write = |name: &str, value: serde_json::Value| {
			std::fs::write(dir.join(format!("{name}.json")), value.to_string()).unwrap();
		};
		// Dead PID (including an old-format record without `source` and a legacy `+offline` record).
		write("old-no-source", serde_json::json!({ "port": 1, "pid": 999996, "build": "0.1.0-1-1", "started": now }));
		write("dead-offline", serde_json::json!({ "port": 2, "pid": 999992, "build": "0.1.0-1-2+offline", "started": now, "source": "C:/old/ror.exe" }));
		// PID reuse: current live PID, but the record claims it started long before this process was created.
		write("reused-pid", serde_json::json!({ "port": 3, "pid": std::process::id(), "build": "0.1.0-1-3", "started": 1 }));
		// Live record: current process PID with current timestamp must stay.
		write("live", serde_json::json!({ "port": 4, "pid": std::process::id(), "build": "0.1.0-1-4", "started": now }));

		let kept = recorded(&state);
		assert_eq!(kept.len(), 1, "only the live record should remain: {kept:?}");
		assert_eq!(kept[0].build, "0.1.0-1-4");
		assert!(!dir.join("old-no-source.json").exists());
		assert!(!dir.join("dead-offline.json").exists());
		assert!(!dir.join("reused-pid.json").exists());
		assert!(dir.join("live.json").exists());
		let _ = std::fs::remove_dir_all(&state);
	}

	#[test]
	fn prune_if_same_preserves_record_replaced_by_newer_service() {
		let state = std::env::temp_dir().join(format!("ror-prune-race-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&state);
		let dir = services_dir(&state);
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("0.1.0-1-1.json");
		let stale = ServiceInfo { port: 10001, pid: 999996, build: "0.1.0-1-1".into(), started: 100, source: None };
		std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
		// A freshly started service replaces the record at the same path before the stale reader unlinks.
		let fresh = ServiceInfo { port: 10002, pid: std::process::id(), build: "0.1.0-1-1".into(), started: 200, source: None };
		std::fs::write(&path, serde_json::to_string(&fresh).unwrap()).unwrap();
		assert!(!prune_if_same(&path, &stale), "must not delete a record replaced by a newer service");
		assert_eq!(read_info(&path).map(|info| (info.pid, info.port)), Some((fresh.pid, fresh.port)));
		// Same PID with a newer `started` timestamp is also a different service instance.
		let newer_start = ServiceInfo { started: 201, ..fresh.clone() };
		std::fs::write(&path, serde_json::to_string(&newer_start).unwrap()).unwrap();
		assert!(!prune_if_same(&path, &fresh), "must not delete a record with a newer started timestamp");
		assert!(path.exists());
		// When the record still matches the stale snapshot, pruning removes it.
		let unchanged = dir.join("0.1.0-1-2.json");
		std::fs::write(&unchanged, serde_json::to_string(&stale).unwrap()).unwrap();
		assert!(prune_if_same(&unchanged, &stale), "matching stale record should be pruned");
		assert!(!unchanged.exists());
		let _ = std::fs::remove_dir_all(&state);
	}

	/// Service refusals (403, 413, 500, 503, non-JSON) classify as `Refused` so the CLI falls back
	/// to in-process rendering, while HTTP 400 job errors stay `JobError`.
	#[test]
	fn service_refusal_is_distinguished_from_job_error() {
		let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let server = std::thread::spawn(move || {
			let replies: [(u16, &[u8]); 4] = [
				(500, br#"{"error":"request body rejected"}"#),
				(413, br#"{"error":"payload too large"}"#),
				(400, br#"{"error":"no such file"}"#),
				(503, b"not json"),
			];
			for (status, body) in replies {
				let (stream, _) = listener.accept().unwrap();
				let _ = http::read_request(&stream);
				let _ = http::write_response_and_close(&stream, status, "application/json", body);
			}
		});
		let log = PathBuf::from("serve.log");
		let header = "X-ROR: 1";
		assert!(matches!(post_once(port, "/render", header, b"{}", Duration::from_secs(5), &log), PostAttempt::Refused(msg) if msg.contains("HTTP 500") && msg.contains("request body rejected")));
		assert!(matches!(post_once(port, "/render", header, b"{}", Duration::from_secs(5), &log), PostAttempt::Refused(msg) if msg.contains("HTTP 413")));
		assert!(matches!(post_once(port, "/render", header, b"{}", Duration::from_secs(5), &log), PostAttempt::JobError(err) if !is_refused(&err) && err.to_string().contains("no such file")));
		assert!(matches!(post_once(port, "/render", header, b"{}", Duration::from_secs(5), &log), PostAttempt::Refused(msg) if msg.contains("not JSON")));
		server.join().unwrap();
	}
}
