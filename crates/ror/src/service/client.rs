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

fn read_info(path: &Path) -> Option<ServiceInfo> {
	let text = std::fs::read_to_string(path).ok()?;
	serde_json::from_str(&text).ok()
}

/// Every service recorded for the state dir, running or not.
fn recorded(state_dir: &Path) -> Vec<ServiceInfo> {
	let Ok(entries) = std::fs::read_dir(services_dir(state_dir)) else {
		return Vec::new();
	};
	entries.flatten().filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json")).filter_map(|entry| read_info(&entry.path())).collect()
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
pub fn ensure(state_dir: &Path, offline: bool) -> Result<u16> {
	let build = service_id(&build_id(), offline);
	let record = build_info_path(state_dir, &build);
	if let Some(info) = read_info(&record)
		&& ready(&info, &build)
	{
		return Ok(info.port);
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
		&& ready(&info, &build)
	{
		let _ = std::fs::remove_file(&lock);
		return Ok(info.port);
	}
	let result = start(state_dir, offline, &build);
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
			// earlier builds of the same executable are superseded: let them finish and exit
			for old in recorded(state_dir) {
				if build_of(&old.build) != build_of(build) && old.source.is_some() && old.source == source {
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
	job.out = job.out.as_deref().map(absolute);
	post(state_dir, "/render", &serde_json::to_vec(&job)?, offline, Duration::from_secs(600))
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
/// stderr as it comes, service::Reply), and this command's settings for its job ([`crate::CALLER_SETTINGS`]).
fn job_header() -> String {
	let mut header = "X-ROR: 1\r\nX-ROR-Notes: 1".to_string();
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
	let header = job_header();
	let mut attempt = 0;
	loop {
		attempt += 1;
		let port = ensure(state_dir, offline)?;
		match http::request_hearing(port, "POST", route, &header, body, timeout, &mut |note| eprintln!("ror: {note}")) {
			Ok((status, response)) => {
				let value: serde_json::Value = serde_json::from_slice(&response).map_err(|error| anyhow!("the render service's answer ({status}) is not JSON ({error}): {}", String::from_utf8_lossy(&response)))?;
				if status != 200 {
					match value.get("error").and_then(|error| error.as_str()).filter(|error| !error.trim().is_empty()) {
						Some(error) => bail!("{error}"),
						None => bail!("the render service answered {status} without saying why: {value} (its log: {})", log.display()),
					}
				}
				return Ok(value);
			}
			Err(http::Unanswered::NotSent(error)) if attempt < 2 => {
				eprintln!("ror: the job could not be sent to the render service on port {port} ({error:#}); sending it again");
			}
			Err(http::Unanswered::Closed(why)) if attempt < 2 => {
				eprintln!("ror: the render service on port {port} closed the connection without an answer ({why}): it exited or crashed (its log: {}); sending the job again", log.display());
			}
			Err(http::Unanswered::NotSent(error)) => bail!("could not send the job to the render service on port {port}: {error:#} (its log: {})", log.display()),
			Err(http::Unanswered::Closed(why)) => bail!("the render service on port {port} closed the connection without an answer ({why}), twice: it exited or crashed (its log: {})", log.display()),
			Err(http::Unanswered::TimedOut) => bail!(
				"the render service on port {port} gave no answer in {:.0} s; the job may still be running there (its log: {}). `ror service status` shows its jobs and memory",
				timeout.as_secs_f64(),
				log.display()
			),
		}
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
}
