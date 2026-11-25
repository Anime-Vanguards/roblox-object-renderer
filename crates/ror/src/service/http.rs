//! Minimal HTTP/1.1 for the local render service: one request per connection, Content-Length
//! bodies only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

pub struct Request {
	pub method: String,
	pub path: String,
	pub headers: Vec<(String, String)>,
	pub body: Vec<u8>,
}

impl Request {
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
	}
}

/// Maximum accepted HTTP request body size (512 MiB). Playbacks passed by path (`playbackFile`)
/// keep normal requests to a few kilobytes; over-limit bodies get a clear HTTP 413 refusal.
pub const MAX_BODY: usize = 512 << 20;

/// Chunk size when streaming a request body from a local client.
const BODY_CHUNK: usize = 1 << 20;

/// How reading an HTTP request can fail.
#[derive(Debug)]
pub enum ReadError {
	/// `Content-Length` exceeded the configured body limit. `unread` is the number of body bytes
	/// not yet read from the socket (after what `BufReader` already pulled in while parsing headers).
	TooLarge { length: usize, limit: usize, unread: usize },
	/// Malformed HTTP, timeout, or I/O failure.
	Other(anyhow::Error),
}

impl From<anyhow::Error> for ReadError {
	fn from(error: anyhow::Error) -> Self {
		Self::Other(error)
	}
}

impl From<std::io::Error> for ReadError {
	fn from(error: std::io::Error) -> Self {
		Self::Other(error.into())
	}
}

pub fn read_request(stream: &TcpStream) -> Result<Request> {
	read_request_with_limit(stream, MAX_BODY).map_err(|error| match error {
		ReadError::TooLarge { length, limit, .. } => anyhow!("request body is {length} bytes, over the {limit}-byte limit"),
		ReadError::Other(error) => error,
	})
}

pub fn read_request_with_limit(stream: &TcpStream, limit: usize) -> std::result::Result<Request, ReadError> {
	stream.set_read_timeout(Some(Duration::from_secs(30)))?;
	let mut reader = BufReader::new(stream);
	let mut line = String::new();
	if reader.read_line(&mut line).context("request line")? == 0 {
		return Err(anyhow!("empty request").into());
	}
	let mut parts = line.split_whitespace();
	let method = parts.next().context("method")?.to_string();
	let path = parts.next().context("path")?.to_string();
	let mut headers = Vec::new();
	loop {
		line.clear();
		if reader.read_line(&mut line).context("header")? == 0 {
			return Err(anyhow!("incomplete headers").into());
		}
		let trimmed = line.trim_end();
		if trimmed.is_empty() {
			break;
		}
		if let Some((key, value)) = trimmed.split_once(':') {
			headers.push((key.trim().to_string(), value.trim().to_string()));
		}
		if headers.len() > 100 {
			return Err(anyhow!("too many headers").into());
		}
	}
	let length: usize = match headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("content-length")) {
		Some((_, value)) => value.parse().with_context(|| format!("invalid Content-Length {value:?}"))?,
		None => 0,
	};
	if length > limit {
		let unread = length.saturating_sub(reader.buffer().len());
		return Err(ReadError::TooLarge { length, limit, unread });
	}
	// Stream the body in chunks so large payloads never pre-allocate gigabytes before bytes arrive.
	let mut body = Vec::with_capacity(length.min(BODY_CHUNK));
	let mut remaining = length;
	let mut chunk = vec![0u8; remaining.min(BODY_CHUNK)];
	while remaining > 0 {
		let want = remaining.min(chunk.len());
		let read = reader.read(&mut chunk[..want]).context("body")?;
		if read == 0 {
			return Err(anyhow!("unexpected EOF in request body ({} of {length} bytes read)", length - remaining).into());
		}
		body.extend_from_slice(&chunk[..read]);
		remaining -= read;
	}
	Ok(Request { method, path, headers, body })
}

pub fn write_response(mut stream: &TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
	let reason = match status {
		200 => "OK",
		400 => "Bad Request",
		403 => "Forbidden",
		404 => "Not Found",
		413 => "Payload Too Large",
		500 => "Internal Server Error",
		503 => "Service Unavailable",
		_ => "Status",
	};
	let head = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
	stream.write_all(head.as_bytes())?;
	stream.write_all(body)?;
	stream.flush()?;
	Ok(())
}

/// Writes an HTTP response and drains the client's unread bytes before closing so Windows does not
/// send a TCP RST (`os error 10053`) that drops the error body while a synchronous client is still
/// writing a large request body. When `expected_unread` is `Some(n)`, stops draining as soon as all
/// `n` remaining body bytes arrive (or the client closes), bounded by a 10 s safety cap.
pub fn write_response_and_close(stream: &TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
	write_response_and_drain(stream, status, content_type, body, None)
}

pub fn write_response_and_drain(stream: &TcpStream, status: u16, content_type: &str, body: &[u8], expected_unread: Option<usize>) -> Result<()> {
	let written = write_response(stream, status, content_type, body);
	let _ = stream.shutdown(Shutdown::Write);
	let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
	let deadline = Instant::now() + if expected_unread.is_some() { Duration::from_secs(10) } else { Duration::from_millis(250) };
	let mut sink = vec![0u8; 64 * 1024];
	let mut reader = stream;
	let mut remaining = expected_unread;
	while Instant::now() < deadline {
		if remaining == Some(0) {
			break;
		}
		let want = remaining.map_or(sink.len(), |left| left.min(sink.len()));
		match reader.read(&mut sink[..want]) {
			Ok(0) => break,
			Ok(n) => {
				if let Some(left) = &mut remaining {
					*left = left.saturating_sub(n);
				}
			}
			Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
			Err(_) => break,
		}
	}
	written
}

/// The type of an answer that carries notes ahead of it: a JSON line `{"note": "..."}` for each note, as
/// the job says it, then `{"status": <HTTP status>, "answer": <the answer>}` (service::Reply).
pub const NOTES: &str = "application/x-ror-notes";

/// Starts an answer that carries notes ([`NOTES`]); its status is the last line's.
pub fn begin_notes(mut stream: &TcpStream) -> Result<()> {
	stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {NOTES}\r\nConnection: close\r\n\r\n").as_bytes())?;
	stream.flush()?;
	Ok(())
}

/// A note of an answer begun with [`begin_notes`].
pub fn write_note(mut stream: &TcpStream, text: &str) -> Result<()> {
	stream.write_all(format!("{}\n", serde_json::json!({ "note": text })).as_bytes())?;
	stream.flush()?;
	Ok(())
}

/// The last line of an answer begun with [`begin_notes`]: its status and `body` (JSON).
pub fn end_notes(mut stream: &TcpStream, status: u16, body: &[u8]) -> Result<()> {
	let mut line = format!("{{\"status\":{status},\"answer\":").into_bytes();
	line.extend_from_slice(body);
	line.extend_from_slice(b"}\n");
	stream.write_all(&line)?;
	stream.flush()?;
	Ok(())
}

/// Whether the other end has closed the connection (a request's client gone): a peek finds its end, or
/// an error, where an open one has nothing to read.
pub fn closed(stream: &TcpStream) -> bool {
	if stream.set_nonblocking(true).is_err() {
		return false;
	}
	let mut byte = [0u8; 1];
	let closed = match stream.peek(&mut byte) {
		Ok(read) => read == 0,
		Err(error) => error.kind() != std::io::ErrorKind::WouldBlock,
	};
	let _ = stream.set_nonblocking(false);
	closed
}

/// How a request to the local service went without an answer.
#[derive(Debug)]
pub enum Unanswered {
	/// Not sent: no connection, or it broke while the request went out (the service may have exited).
	NotSent(anyhow::Error),
	/// Sent, and no answer within the timeout: the job may still run there.
	TimedOut,
	/// Sent, and the connection closed or broke before the answer: the service exited or crashed.
	Closed(String),
}

/// A request to the local service whose answer may carry notes ([`NOTES`]): each is given to `hear` as
/// it comes, and the answer's status and body returned. `header` is the request's own header lines
/// (`X-ROR: 1` and what goes with it).
pub fn request_hearing(port: u16, method: &str, path: &str, header: &str, body: &[u8], timeout: Duration, hear: &mut dyn FnMut(&str)) -> std::result::Result<(u16, Vec<u8>), Unanswered> {
	let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500))
		.with_context(|| format!("connecting to 127.0.0.1:{port}"))
		.map_err(Unanswered::NotSent)?;
	let setup = (|| -> Result<()> {
		stream.set_read_timeout(Some(timeout))?;
		stream.set_nodelay(true)?;
		Ok(())
	})();
	if let Err(error) = setup {
		return Err(Unanswered::NotSent(error));
	}
	let head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{header}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
	let write_err = (|| -> std::io::Result<()> {
		stream.write_all(head.as_bytes())?;
		stream.write_all(body)?;
		stream.flush()?;
		Ok(())
	})()
	.err();
	if let Some(error) = write_err {
		// If the server rejected the request early and closed after writing an HTTP error response,
		// try reading that response before reporting a broken write.
		let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
		if let Ok(answer) = read_response_hearing(stream, hear) {
			return Ok(answer);
		}
		return Err(Unanswered::NotSent(error.into()));
	}
	read_response_hearing(stream, hear)
}

fn read_response_hearing(stream: TcpStream, hear: &mut dyn FnMut(&str)) -> std::result::Result<(u16, Vec<u8>), Unanswered> {
	let broke = |error: std::io::Error| match error.kind() {
		std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => Unanswered::TimedOut,
		_ => Unanswered::Closed(error.to_string()),
	};
	let mut reader = BufReader::new(stream);
	let mut line = String::new();
	if reader.read_line(&mut line).map_err(broke)? == 0 {
		return Err(Unanswered::Closed("no answer".into()));
	}
	let status: u16 = line.split_whitespace().nth(1).and_then(|code| code.parse().ok()).ok_or_else(|| Unanswered::Closed(format!("a broken answer: {line:?}")))?;
	let (mut length, mut content_type) = (None, None);
	loop {
		line.clear();
		if reader.read_line(&mut line).map_err(broke)? == 0 {
			return Err(Unanswered::Closed("the answer ended in its head".into()));
		}
		let trimmed = line.trim_end();
		if trimmed.is_empty() {
			break;
		}
		if let Some((key, value)) = trimmed.split_once(':') {
			if key.trim().eq_ignore_ascii_case("content-length") {
				length = value.trim().parse::<usize>().ok();
			} else if key.trim().eq_ignore_ascii_case("content-type") {
				content_type = Some(value.trim().to_ascii_lowercase());
			}
		}
	}
	if content_type.as_deref() == Some(NOTES) {
		loop {
			line.clear();
			if reader.read_line(&mut line).map_err(broke)? == 0 {
				return Err(Unanswered::Closed("the answer ended after its notes, before its last line".into()));
			}
			let value: serde_json::Value = serde_json::from_str(line.trim()).map_err(|error| Unanswered::Closed(format!("a broken note line ({error}): {:?}", line.trim())))?;
			if let Some(note) = value.get("note").and_then(|note| note.as_str()) {
				hear(note);
			} else if let Some(status) = value.get("status").and_then(|status| status.as_u64()) {
				let answer = serde_json::to_vec(value.get("answer").unwrap_or(&serde_json::Value::Null)).map_err(|error| Unanswered::Closed(error.to_string()))?;
				return Ok((status as u16, answer));
			}
		}
	}
	let mut body = Vec::new();
	match length {
		Some(length) => {
			body.resize(length, 0);
			reader.read_exact(&mut body).map_err(broke)?;
		}
		None => {
			reader.read_to_end(&mut body).map_err(broke)?;
		}
	}
	Ok((status, body))
}

/// A request to the local service; returns the status and body.
pub fn request(port: u16, method: &str, path: &str, body: &[u8], timeout: Duration) -> Result<(u16, Vec<u8>)> {
	request_as(port, method, path, "X-ROR: 1", body, timeout)
}

/// A request to a local service that wants `header` (`X-ROR: 1` for ror's, `X-RUR: 1` for rur's).
pub fn request_as(port: u16, method: &str, path: &str, header: &str, body: &[u8], timeout: Duration) -> Result<(u16, Vec<u8>)> {
	request_typed(port, method, path, header, body, timeout).map(|(status, _, body)| (status, body))
}

/// [`request_as`], with the answer's Content-Type.
pub fn request_typed(port: u16, method: &str, path: &str, header: &str, body: &[u8], timeout: Duration) -> Result<(u16, Option<String>, Vec<u8>)> {
	let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500)).with_context(|| format!("connecting to 127.0.0.1:{port}"))?;
	stream.set_read_timeout(Some(timeout))?;
	stream.set_nodelay(true)?;
	let head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{header}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
	stream.write_all(head.as_bytes())?;
	stream.write_all(body)?;
	stream.flush()?;
	let mut reader = BufReader::new(stream);
	let mut line = String::new();
	reader.read_line(&mut line).context("status line")?;
	let status: u16 = line.split_whitespace().nth(1).and_then(|code| code.parse().ok()).ok_or_else(|| anyhow!("bad status line {line:?}"))?;
	let (mut length, mut content_type) = (None, None);
	loop {
		line.clear();
		reader.read_line(&mut line)?;
		let trimmed = line.trim_end();
		if trimmed.is_empty() {
			break;
		}
		if let Some((key, value)) = trimmed.split_once(':') {
			if key.trim().eq_ignore_ascii_case("content-length") {
				length = value.trim().parse::<usize>().ok();
			} else if key.trim().eq_ignore_ascii_case("content-type") {
				content_type = Some(value.trim().to_ascii_lowercase());
			}
		}
	}
	let mut body = Vec::new();
	match length {
		Some(length) => {
			body.resize(length, 0);
			reader.read_exact(&mut body)?;
		}
		None => {
			reader.read_to_end(&mut body)?;
		}
	}
	Ok((status, content_type, body))
}
