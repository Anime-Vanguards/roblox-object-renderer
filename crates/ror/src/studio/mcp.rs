//! A minimal MCP client for a Studio bridge: an MCP server reached over TCP (JSON-RPC, one message a
//! line) that lists the open Studios (`list_roblox_studios`) and runs Luau in one of them by id
//! (`execute_luau`). Its address is `ROR_STUDIO_MCP` (`host:port`, or a port on 127.0.0.1); the bridge
//! itself is not part of this repository (docs/parity.md).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize)]
pub struct StudioEntry {
	pub id: String,
	#[serde(default)]
	pub name: Option<String>,
}

pub struct Mcp {
	reader: BufReader<TcpStream>,
	writer: TcpStream,
	next_id: u64,
}

/// The Studio bridge's address: `ROR_STUDIO_MCP`, `host:port` or a port on 127.0.0.1.
fn bridge_address() -> Result<String> {
	if let Some(value) = std::env::var("ROR_STUDIO_MCP").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty()) {
		return Ok(if value.parse::<u16>().is_ok() { format!("127.0.0.1:{value}") } else { value });
	}
	bail!("no Studio bridge: set ROR_STUDIO_MCP to the address of a Studio MCP bridge (host:port, or a port on 127.0.0.1; docs/parity.md)")
}

impl Mcp {
	pub fn connect() -> Result<Self> {
		let address = bridge_address()?;
		let stream = TcpStream::connect(address.as_str()).with_context(|| format!("connecting to the Studio bridge at {address} (is it running?)"))?;
		stream.set_read_timeout(Some(Duration::from_secs(300)))?;
		let writer = stream.try_clone()?;
		let mut mcp = Self { reader: BufReader::new(stream), writer, next_id: 1 };
		mcp.request(
			"initialize",
			json!({"protocolVersion": "2024-11-05", "clientInfo": {"name": "ror", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {}}),
		)?;
		mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
		Ok(mcp)
	}

	fn send(&mut self, message: &Value) -> Result<()> {
		let mut line = serde_json::to_string(message)?;
		line.push('\n');
		self.writer.write_all(line.as_bytes()).context("writing to the Studio bridge")?;
		Ok(())
	}

	fn request(&mut self, method: &str, params: Value) -> Result<Value> {
		let id = self.next_id;
		self.next_id += 1;
		self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
		let mut line = String::new();
		loop {
			line.clear();
			let read = self.reader.read_line(&mut line).context("reading from the Studio bridge")?;
			if read == 0 {
				bail!("the Studio bridge closed the connection");
			}
			let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
				continue;
			};
			if message.get("id").and_then(Value::as_u64) != Some(id) {
				continue;
			}
			if let Some(error) = message.get("error") {
				bail!("MCP {method}: {error}");
			}
			return Ok(message.get("result").cloned().unwrap_or(Value::Null));
		}
	}

	fn call_tool(&mut self, name: &str, arguments: Value) -> Result<(String, bool)> {
		let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
		let text = result
			.get("content")
			.and_then(Value::as_array)
			.map(|items| items.iter().filter_map(|item| item.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(""))
			.unwrap_or_default();
		let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
		Ok((text, is_error))
	}

	pub fn list_studios(&mut self) -> Result<Vec<StudioEntry>> {
		let (text, _) = self.call_tool("list_roblox_studios", json!({}))?;
		let value: Value = serde_json::from_str(&text).with_context(|| format!("list_roblox_studios: {text}"))?;
		Ok(serde_json::from_value(value.get("studios").cloned().unwrap_or(Value::Array(Vec::new())))?)
	}

	/// The Studio whose name contains `query` (or whose id is `query`).
	pub fn find_studio(&mut self, query: &str) -> Result<StudioEntry> {
		let studios = self.list_studios()?;
		studios
			.iter()
			.find(|studio| studio.id == query)
			.or_else(|| studios.iter().find(|studio| studio.name.as_deref().is_some_and(|name| name.contains(query))))
			.cloned()
			.ok_or_else(|| {
				let names: Vec<_> = studios.iter().map(|studio| studio.name.clone().unwrap_or_else(|| "(opening)".into())).collect();
				anyhow!("no Studio matches {query:?} (open: {})", names.join(", "))
			})
	}

	/// Runs Luau in the Edit datamodel and returns its result text.
	pub fn run_luau(&mut self, studio: &str, code: &str) -> Result<String> {
		let (text, is_error) = self.call_tool("execute_luau", json!({"studio_id": studio, "datamodel_type": "Edit", "code": code}))?;
		if is_error {
			bail!("Studio: {text}");
		}
		Ok(text)
	}

	/// Runs a chunk whose result is a (possibly large or binary) string: the result is base64
	/// encoded in Studio and paged out of `_G`. Where the Luau tool has no `_G` (a Studio's
	/// Assistant tool: "attempt to index nil with '__ROR_RESULT'"), [`Mcp::run_luau_pages`] instead.
	pub fn run_luau_bytes(&mut self, studio: &str, code: &str) -> Result<Vec<u8>> {
		let wrapped = format!("local __ror_result = (function()\n{code}\nend)()\nif type(__ror_result) ~= \"string\" then __ror_result = tostring(__ror_result) end\n{BASE64_ENCODE}\n_G.__ROR_RESULT = __ror_base64(__ror_result)\nreturn #_G.__ROR_RESULT");
		let length: usize = match self.run_luau(studio, &wrapped) {
			Ok(text) => text.trim().parse().context("paged result length")?,
			Err(error) if format!("{error:#}").contains("attempt to index nil with '__ROR_RESULT'") => return self.run_luau_pages(studio, code),
			Err(error) => return Err(error),
		};
		let mut encoded = String::with_capacity(length);
		let chunk = 90_000;
		let mut start = 1;
		while start <= length {
			encoded.push_str(self.run_luau(studio, &format!("return string.sub(_G.__ROR_RESULT, {start}, {})", start + chunk - 1))?.trim());
			start += chunk;
		}
		self.run_luau(studio, "_G.__ROR_RESULT = nil return 0")?;
		if encoded.len() != length {
			bail!("paged {} of {length} base64 characters", encoded.len());
		}
		base64_decode(&encoded)
	}

	/// [`Mcp::run_luau_bytes`] keeping nothing in Studio between calls: every call runs the chunk
	/// again and returns one page of its result in base64, with the result's length and a hash of
	/// bytes sampled across it, which must agree from page to page (the result must be repeatable,
	/// as a serialization of unchanged instances is).
	pub fn run_luau_pages(&mut self, studio: &str, code: &str) -> Result<Vec<u8>> {
		const PAGE: usize = 67_500;
		let mut bytes = Vec::new();
		let mut first: Option<(usize, String)> = None;
		loop {
			let start = bytes.len();
			let wrapped = format!(
				"local __ror_result = (function()\n{code}\nend)()\nif type(__ror_result) ~= \"string\" then __ror_result = tostring(__ror_result) end\n{BASE64_ENCODE}
local length = #__ror_result
local hash = length
local step = math.max(1, length // 4096)
for i = 1, length, step do
	hash = bit32.bxor(bit32.lrotate(hash, 5), string.byte(__ror_result, i))
end
return length .. \":\" .. hash .. \":\" .. __ror_base64(string.sub(__ror_result, {}, {}))",
				start + 1,
				start + PAGE
			);
			let text = self.run_luau(studio, &wrapped)?;
			let mut fields = text.trim().splitn(3, ':');
			let length: usize = fields.next().unwrap_or_default().parse().context("paged result length")?;
			let hash = fields.next().unwrap_or_default().to_string();
			let page = base64_decode(fields.next().unwrap_or_default())?;
			match &first {
				None => first = Some((length, hash)),
				Some(expected) if *expected != (length, hash.clone()) => bail!("the result changed between pages ({expected:?} then ({length}, {hash}))"),
				Some(_) => {}
			}
			if page.is_empty() && bytes.len() < length {
				bail!("paged {} of {length} bytes", bytes.len());
			}
			bytes.extend_from_slice(&page);
			if bytes.len() >= length {
				bytes.truncate(length);
				return Ok(bytes);
			}
		}
	}

	/// Stores `bytes` in Studio as a buffer in `_G[key]` (base64 in chunks, decoded in Studio).
	pub fn upload(&mut self, studio: &str, key: &str, bytes: &[u8]) -> Result<()> {
		let encoded = base64_encode(bytes);
		self.run_luau(studio, &format!("_G.__ror_upload = {{}} return 0"))?;
		for chunk in encoded.as_bytes().chunks(200_000) {
			let chunk = std::str::from_utf8(chunk).expect("base64 is ASCII");
			self.run_luau(studio, &format!("table.insert(_G.__ror_upload, \"{chunk}\") return 0"))?;
		}
		self.run_luau(studio, &format!("{BASE64_DECODE}\n_G[{key:?}] = __ror_unbase64(table.concat(_G.__ror_upload))\n_G.__ror_upload = nil\nreturn buffer.len(_G[{key:?}])"))?;
		Ok(())
	}
}

const BASE64_ENCODE: &str = r#"local function __ror_base64(s)
	local alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
	local bytes = buffer.fromstring(s)
	local length = buffer.len(bytes)
	local out = buffer.create(math.ceil(length / 3) * 4)
	local write = 0
	for i = 0, length - 1, 3 do
		local a = buffer.readu8(bytes, i)
		local b = if i + 1 < length then buffer.readu8(bytes, i + 1) else 0
		local c = if i + 2 < length then buffer.readu8(bytes, i + 2) else 0
		local n = a * 65536 + b * 256 + c
		buffer.writeu8(out, write, string.byte(alphabet, bit32.rshift(n, 18) + 1))
		buffer.writeu8(out, write + 1, string.byte(alphabet, bit32.band(bit32.rshift(n, 12), 63) + 1))
		buffer.writeu8(out, write + 2, if i + 1 < length then string.byte(alphabet, bit32.band(bit32.rshift(n, 6), 63) + 1) else 61)
		buffer.writeu8(out, write + 3, if i + 2 < length then string.byte(alphabet, bit32.band(n, 63) + 1) else 61)
		write += 4
	end
	return buffer.tostring(out)
end"#;

const BASE64_DECODE: &str = r#"local function __ror_unbase64(s)
	local lookup = {}
	local alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
	for i = 1, 64 do
		lookup[string.byte(alphabet, i)] = i - 1
	end
	local input = buffer.fromstring(s)
	local length = buffer.len(input)
	local padding = 0
	if length > 0 and buffer.readu8(input, length - 1) == 61 then padding += 1 end
	if length > 1 and buffer.readu8(input, length - 2) == 61 then padding += 1 end
	local out = buffer.create(length // 4 * 3 - padding)
	local write = 0
	local total = buffer.len(out)
	for i = 0, length - 1, 4 do
		local a = lookup[buffer.readu8(input, i)] or 0
		local b = lookup[buffer.readu8(input, i + 1)] or 0
		local c = lookup[buffer.readu8(input, i + 2)] or 0
		local d = lookup[buffer.readu8(input, i + 3)] or 0
		local n = a * 262144 + b * 4096 + c * 64 + d
		if write < total then buffer.writeu8(out, write, bit32.rshift(n, 16)) end
		if write + 1 < total then buffer.writeu8(out, write + 1, bit32.band(bit32.rshift(n, 8), 255)) end
		if write + 2 < total then buffer.writeu8(out, write + 2, bit32.band(n, 255)) end
		write += 3
	end
	return out
end"#;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
	let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
	for chunk in bytes.chunks(3) {
		let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
		out.push(ALPHABET[(n >> 18) as usize & 63] as char);
		out.push(ALPHABET[(n >> 12) as usize & 63] as char);
		out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
		out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
	}
	out
}

pub fn base64_decode(text: &str) -> Result<Vec<u8>> {
	let mut lookup = [255u8; 256];
	for (index, &byte) in ALPHABET.iter().enumerate() {
		lookup[byte as usize] = index as u8;
	}
	let bytes: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
	let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
	for chunk in bytes.chunks(4) {
		let mut n = 0u32;
		let mut valid = 0;
		for (index, &byte) in chunk.iter().enumerate() {
			if byte == b'=' {
				break;
			}
			let value = lookup[byte as usize];
			if value == 255 {
				bail!("invalid base64");
			}
			n |= (value as u32) << (18 - 6 * index);
			valid += 1;
		}
		if valid >= 2 {
			out.push((n >> 16) as u8);
		}
		if valid >= 3 {
			out.push((n >> 8) as u8);
		}
		if valid >= 4 {
			out.push(n as u8);
		}
	}
	Ok(out)
}

#[cfg(test)]
mod tests {
	#[test]
	fn base64_roundtrip() {
		for length in 0..20 {
			let bytes: Vec<u8> = (0..length).map(|value| (value * 37 + 11) as u8).collect();
			assert_eq!(super::base64_decode(&super::base64_encode(&bytes)).unwrap(), bytes);
		}
	}
}
