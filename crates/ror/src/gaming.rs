//! A game played on the same machine, as a status file says: the file `ROR_GAMING_STATUS` names, which
//! a monitor writes every second with its mode and the game it found, that game's memory read from the
//! process table and the GPU's counters (no handle on the game). While it says `gaming`, ror sizes its
//! work beside the game instead of taking what is free: the machine's memory floor counts the game's
//! working set as taken ([`crate::budget::floor`]), and draws keep the GPU's memory a headroom short of
//! full whatever else draws, so the game's own textures stay resident ([`crate::budget::GpuGate`],
//! [`crate::render::Renderer::fit_vram`]). Without the file nothing changes.
//!
//! The file is JSON: `{"mode": "gaming", "updatedUnix": <seconds>, "stopped": false, "game": {"image":
//! "game.exe", "workingSetMB": 9000, "privateMB": 11000, "gpuMB": 7500}}` (`game` may be null).

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// A game running, as the status file last said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gaming {
	/// The game's image (`game.exe`), if the monitor found one (gaming mode may also follow a
	/// fullscreen app)
	pub image: Option<String>,
	/// Its working set, private bytes and dedicated GPU memory
	pub working_set: Option<u64>,
	pub private: Option<u64>,
	pub gpu: Option<u64>,
}

/// How often the status file is read again: the monitor writes it each second.
const REREAD: Duration = Duration::from_millis(500);
/// A status older than this is a monitor that stopped or hangs: no game is assumed.
const STALE_SECONDS: f64 = 10.0;

struct Cached {
	read: Option<Instant>,
	value: Option<Gaming>,
}

static CACHE: Mutex<Cached> = parking_lot::const_mutex(Cached { read: None, value: None });

/// The status file: `ROR_GAMING_STATUS`, none by default.
fn status_path() -> Option<PathBuf> {
	std::env::var_os("ROR_GAMING_STATUS").filter(|path| !path.is_empty()).map(PathBuf::from)
}

/// The game running now, if the status file says one is (read at most every half second).
pub fn now() -> Option<Gaming> {
	let mut cache = CACHE.lock();
	if cache.read.is_some_and(|read| read.elapsed() < REREAD) {
		return cache.value.clone();
	}
	cache.read = Some(Instant::now());
	let unix = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |time| time.as_secs_f64());
	cache.value = status_path().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| parse(&bytes, unix));
	cache.value.clone()
}

/// Whether a game runs now ([`now`]).
pub fn active() -> bool {
	now().is_some()
}

/// The game in a status file's bytes, read at `unix` seconds: `None` unless the monitor is in gaming
/// mode, running and fresh.
pub fn parse(bytes: &[u8], unix: f64) -> Option<Gaming> {
	let status: serde_json::Value = serde_json::from_slice(bytes).ok()?;
	if status["mode"].as_str() != Some("gaming") || status["stopped"].as_bool() == Some(true) {
		return None;
	}
	let updated = status["updatedUnix"].as_f64()?;
	if (unix - updated).abs() > STALE_SECONDS {
		return None;
	}
	let game = &status["game"];
	let megabytes = |key: &str| game[key].as_u64().map(|mb| mb << 20);
	Some(Gaming { image: game["image"].as_str().map(str::to_string), working_set: megabytes("workingSetMB"), private: megabytes("privateMB"), gpu: megabytes("gpuMB") })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_fresh_gaming_status_gives_the_game_and_its_memory() {
		let status = br#"{"version":1,"updatedUnix":1000.5,"mode":"gaming","game":{"pid":7,"image":"game.exe","path":"C:\\g\\game.exe","workingSetMB":9000,"privateMB":11000,"gpuMB":7500}}"#;
		let gaming = parse(status, 1002.0).expect("gaming");
		assert_eq!(gaming.image.as_deref(), Some("game.exe"));
		assert_eq!(gaming.working_set, Some(9000 << 20));
		assert_eq!(gaming.private, Some(11000 << 20));
		assert_eq!(gaming.gpu, Some(7500 << 20));
		// gaming mode for a fullscreen app: no game, no memory figures
		let fullscreen = parse(br#"{"updatedUnix":1000,"mode":"gaming","game":null}"#, 1000.0).expect("gaming");
		assert_eq!(fullscreen, Gaming::default());
	}

	#[test]
	fn other_modes_a_stopped_monitor_and_a_stale_status_are_no_game() {
		assert_eq!(parse(br#"{"updatedUnix":1000,"mode":"active","game":null}"#, 1000.0), None);
		assert_eq!(parse(br#"{"updatedUnix":1000,"mode":"gaming","stopped":true,"holdGpu":false}"#, 1000.0), None);
		assert_eq!(parse(br#"{"updatedUnix":1000,"mode":"gaming","game":null}"#, 1030.0), None);
		assert_eq!(parse(b"not json", 1000.0), None);
	}
}
