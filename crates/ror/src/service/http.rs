//! Minimal HTTP/1.1 for the local render service: one request per connection, Content-Length
//! bodies only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

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

const MAX_BODY: usize = 64 << 20;

pub fn read_request(stream: &TcpStream) -> Result<Request> {
	stream.set_read_timeout(Some(Duration::from_secs(30)))?;
	let mut reader = BufReader::new(stream);
	let mut line = String::new();
	reader.read_line(&mut line).context("request line")?;
	let mut parts = line.split_whitespace();
	let method = parts.next().context("method")?.to_string();
	let path = parts.next().context("path")?.to_string();
	let mut headers = Vec::new();
	loop {
		line.clear();
		reader.read_line(&mut line).context("header")?;
		let trimmed = line.trim_end();
		if trimmed.is_empty() {
			break;
		}
		if let Some((key, value)) = trimmed.split_once(':') {
			headers.push((key.trim().to_string(), value.trim().to_string()));
		}
		if headers.len() > 100 {
			bail!("too many headers");
		}
	}
	let length: usize = headers.iter().find(|(key, _)| key.eq_ignore_ascii_case("content-length")).and_then(|(_, value)| value.parse().ok()).unwrap_or(0);
	if length > MAX_BODY {
		bail!("request body too large");
	}
	let mut body = vec![0; length];
	reader.read_exact(&mut body).context("body")?;
	Ok(Request { method, path, headers, body })
}

pub fn write_response(mut stream: &TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
	let reason = match status {
		200 => "OK",
		400 => "Bad Request",
		403 => "Forbidden",
		404 => "Not Found",
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
	let sent = (|| -> Result<TcpStream> {
		let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500)).with_context(|| format!("connecting to 127.0.0.1:{port}"))?;
		stream.set_read_timeout(Some(timeout))?;
		stream.set_nodelay(true)?;
		let head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{header}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
		stream.write_all(head.as_bytes())?;
		stream.write_all(body)?;
		stream.flush()?;
		Ok(stream)
	})();
	let stream = sent.map_err(Unanswered::NotSent)?;
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
