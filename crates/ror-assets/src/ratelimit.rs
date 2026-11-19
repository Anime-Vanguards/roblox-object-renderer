//! Pacing the requests made with an Open Cloud API key by what Roblox says of its rate limit
//! (documented as 1000 requests a minute per key owner): every request with the key in a process
//! waits its turn here.
//!
//! - A reply's `x-ratelimit-remaining` and `x-ratelimit-reset` (seconds) say what is left of the
//!   window and when it starts again; each request sent counts against what is left until the next
//!   reply says otherwise, and none goes while nothing is left before the reset.
//! - A 429 holds back every request until its `Retry-After`, else the window's reset, else for 1 s,
//!   doubling with each 429 in a row; the request is then sent again ([`crate::http`]'s retries).
//! - No wait is longer than a window (60 s), whatever a reply says.
//!
//! The state changes only through [`RateLimit::admit`] and [`RateLimit::observe`], which take the
//! time as an argument, so the rules are tested without waiting.

use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// The rate limit's window: the longest wait.
pub(crate) const WINDOW: Duration = Duration::from_secs(60);

/// What a reply said of the rate limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Headers {
	/// `x-ratelimit-remaining`: requests left in the window
	pub remaining: Option<u64>,
	/// `x-ratelimit-reset`: how long until the window starts again
	pub reset: Option<Duration>,
}

impl Headers {
	/// From a reply's headers (`header` answers one by its lowercase name). A value's leading number
	/// counts (`999`, `999, 1000;w=60`, `12.5`); anything else is not known.
	pub(crate) fn parse(header: impl Fn(&str) -> Option<String>) -> Headers {
		let leading = |value: String| -> Option<f64> {
			let value = value.trim().to_string();
			let end = value.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(value.len());
			value[..end].parse::<f64>().ok().filter(|number| number.is_finite() && *number >= 0.0)
		};
		Headers {
			remaining: header("x-ratelimit-remaining").and_then(leading).map(|number| number as u64),
			reset: header("x-ratelimit-reset").and_then(leading).map(|seconds| Duration::from_secs_f64(seconds.min(WINDOW.as_secs_f64()))),
		}
	}
}

#[derive(Debug, Default)]
struct State {
	/// No request before this (a 429)
	paused_until: Option<Instant>,
	/// Requests left in the window, as last said and counted down since
	remaining: Option<u64>,
	/// When the window starts again
	reset_at: Option<Instant>,
	/// 429s in a row
	refused: u32,
}

/// The shared pace of one key's requests.
#[derive(Debug, Default)]
pub(crate) struct RateLimit {
	state: Mutex<State>,
}

impl RateLimit {
	/// How long a request at `now` must wait first; `None`: it may go now, and is counted against what
	/// is left of the window.
	pub(crate) fn admit(&self, now: Instant) -> Option<Duration> {
		let mut state = self.state.lock();
		if let Some(until) = state.paused_until {
			if until > now {
				return Some(until - now);
			}
			state.paused_until = None;
		}
		if state.remaining == Some(0) {
			match state.reset_at {
				Some(at) if at > now => return Some(at - now),
				// a new window: what is left of it is not known until a reply says
				_ => (state.remaining, state.reset_at) = (None, None),
			}
		}
		if let Some(remaining) = state.remaining.as_mut() {
			*remaining -= 1;
		}
		None
	}

	/// Takes in what a reply with `status` received at `now` said ([`Headers`], `Retry-After`).
	pub(crate) fn observe(&self, now: Instant, status: u16, headers: &Headers, retry_after: Option<Duration>) {
		let mut state = self.state.lock();
		if let Some(remaining) = headers.remaining {
			state.remaining = Some(remaining);
			// nothing left and no reset said: try again in a second (a 429 then backs off further)
			let reset = headers.reset.or((remaining == 0).then_some(Duration::from_secs(1)));
			state.reset_at = reset.map(|reset| now + reset.min(WINDOW));
		}
		if status != 429 {
			state.refused = 0;
			return;
		}
		state.refused += 1;
		let doubling = Duration::from_secs(1 << (state.refused - 1).min(6));
		let wait = retry_after.or(headers.reset).unwrap_or(doubling).min(WINDOW);
		let until = now + wait;
		state.paused_until = Some(state.paused_until.map_or(until, |paused| paused.max(until)));
	}

	/// Blocks until a request may go ([`RateLimit::admit`]).
	pub(crate) fn wait(&self) {
		while let Some(wait) = self.admit(Instant::now()) {
			std::thread::sleep(wait);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn headers(remaining: Option<u64>, reset: Option<u64>) -> Headers {
		Headers { remaining, reset: reset.map(Duration::from_secs) }
	}

	#[test]
	fn reads_the_leading_number() {
		let parse = |remaining: &str, reset: &str| {
			let (remaining, reset) = (remaining.to_string(), reset.to_string());
			Headers::parse(move |name| match name {
				"x-ratelimit-remaining" => Some(remaining.clone()),
				"x-ratelimit-reset" => Some(reset.clone()),
				_ => None,
			})
		};
		assert_eq!(parse("999", "37"), headers(Some(999), Some(37)));
		assert_eq!(parse(" 12, 1000;w=60", "2.5"), Headers { remaining: Some(12), reset: Some(Duration::from_millis(2500)) });
		// a reset beyond the window (or a timestamp) is a window at most
		assert_eq!(parse("0", "1763560000"), headers(Some(0), Some(60)));
		assert_eq!(parse("many", "-1"), Headers::default());
		assert_eq!(Headers::parse(|_| None), Headers::default());
	}

	#[test]
	fn nothing_left_waits_for_the_reset() {
		let limit = RateLimit::default();
		let t0 = Instant::now();
		// nothing said yet: every request goes
		assert_eq!(limit.admit(t0), None);
		assert_eq!(limit.admit(t0), None);
		// two left for 10 s: two go, the third waits for the reset
		limit.observe(t0, 200, &headers(Some(2), Some(10)), None);
		assert_eq!(limit.admit(t0), None);
		assert_eq!(limit.admit(t0 + Duration::from_secs(1)), None);
		assert_eq!(limit.admit(t0 + Duration::from_secs(4)), Some(Duration::from_secs(6)));
		// a new window: requests go until a reply says what is left
		assert_eq!(limit.admit(t0 + Duration::from_secs(10)), None);
		assert_eq!(limit.admit(t0 + Duration::from_secs(10)), None);
		// nothing left and no reset said: a second
		limit.observe(t0, 200, &headers(Some(0), None), None);
		assert_eq!(limit.admit(t0), Some(Duration::from_secs(1)));
		// a reply saying more is left lets requests go again
		limit.observe(t0, 200, &headers(Some(500), Some(30)), None);
		assert_eq!(limit.admit(t0), None);
	}

	#[test]
	fn a_429_holds_every_request_back() {
		let limit = RateLimit::default();
		let t0 = Instant::now();
		// Retry-After first
		limit.observe(t0, 429, &headers(Some(0), Some(20)), Some(Duration::from_secs(5)));
		assert_eq!(limit.admit(t0 + Duration::from_secs(1)), Some(Duration::from_secs(4)));
		// then the reset of the window (nothing left until then)
		assert_eq!(limit.admit(t0 + Duration::from_secs(5)), Some(Duration::from_secs(15)));
		assert_eq!(limit.admit(t0 + Duration::from_secs(20)), None);

		// with neither: 1 s, doubling with each 429 in a row, a window at most
		let limit = RateLimit::default();
		let mut now = t0;
		for (refusal, wait) in [1, 2, 4, 8, 16, 32, 60, 60].into_iter().enumerate() {
			limit.observe(now, 429, &Headers::default(), None);
			assert_eq!(limit.admit(now), Some(Duration::from_secs(wait)), "429 number {}", refusal + 1);
			now += Duration::from_secs(wait);
			assert_eq!(limit.admit(now), None);
		}
		// an answer ends the run
		limit.observe(now, 200, &Headers::default(), None);
		limit.observe(now, 429, &Headers::default(), None);
		assert_eq!(limit.admit(now), Some(Duration::from_secs(1)));
		// a later, shorter pause never shortens one already set
		limit.observe(now, 429, &Headers::default(), Some(Duration::ZERO));
		assert_eq!(limit.admit(now), Some(Duration::from_secs(1)));
		// a Retry-After beyond the window is a window
		let limit = RateLimit::default();
		limit.observe(t0, 429, &Headers::default(), Some(Duration::from_secs(3600)));
		assert_eq!(limit.admit(t0), Some(WINDOW));
	}
}
