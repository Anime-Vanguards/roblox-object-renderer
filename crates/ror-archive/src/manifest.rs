//! `manifest.jsonl`: one JSON object per line. The first is the `header` (format version and digest
//! algorithm), the second the `package` (what the archive holds), the rest are `content` records
//! (embedded bytes, a reference to an asset, or both).

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::{Error, Limits, Result};

pub(crate) const MANIFEST_NAME: &str = "manifest.jsonl";
pub(crate) const OBJECTS_PREFIX: &str = "objects/";
/// The manifest version this reader knows.
pub(crate) const VERSION: u64 = 1;
/// The digest algorithm archives are written with.
pub(crate) const BLAKE3_128: &str = "blake3-128";
/// JSON nested deeper than this is refused.
const MAX_DEPTH: usize = 32;

/// The parsed manifest of an archive.
#[derive(Debug, Clone)]
pub struct Manifest {
	/// The manifest's format version (1).
	pub version: u64,
	/// The algorithm objects are named by, as the header spells it (`blake3-128`).
	pub digest_algorithm: String,
	/// What the archive holds.
	pub package: Package,
	/// The content records, in manifest order.
	pub content: Vec<Content>,
}

/// The `package` record.
#[derive(Debug, Clone)]
pub struct Package {
	/// The package's `type`: `place`, `model`, or a type this reader does not open.
	pub kind: String,
	/// The logical path of the serialized place, for a place package.
	pub place: Option<String>,
	/// Every field of the record as written, for metadata this reader does not interpret.
	pub fields: Map<String, Value>,
}

/// A `content` record: embedded bytes, a reference to an asset, or both.
#[derive(Debug, Clone)]
pub struct Content {
	/// The logical path within the package, when the content has one.
	pub path: Option<String>,
	/// The digest of the embedded bytes, stored at `objects/<digest>`.
	pub object_digest: Option<String>,
	/// The Roblox asset this content is, or stands for.
	pub asset: Option<Asset>,
	/// Free-form metadata.
	pub metadata: Option<Map<String, Value>>,
}

/// A reference to a Roblox asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
	/// The asset id.
	pub id: u64,
	/// The asset version, when the reference pins one.
	pub version: Option<u64>,
	/// A tag naming a version, when the reference carries one.
	pub tag: Option<String>,
}

/// A digest algorithm this crate computes. Roblox's page names none; `blake3-128` is the one the
/// draft readers and writers use, and the one a property's reference to an object is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Digest {
	/// The first 16 bytes of BLAKE3.
	Blake3_128,
}

impl Digest {
	pub(crate) fn named(algorithm: &str) -> Option<Self> {
		algorithm.eq_ignore_ascii_case(BLAKE3_128).then_some(Self::Blake3_128)
	}

	pub(crate) fn hex(self, bytes: &[u8]) -> String {
		let hash = blake3::hash(bytes).to_hex();
		match self {
			Self::Blake3_128 => hash[..32].to_owned(),
		}
	}
}

/// Digests name ZIP members, so they stay lowercase hex whatever the algorithm.
pub(crate) fn valid_digest(digest: &str) -> bool {
	(16..=128).contains(&digest.len())
		&& digest.len().is_multiple_of(2)
		&& digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Logical paths are relative, use `/`, and have no empty, `.` or `..` component. The format's
/// 128-character limit is on ZIP member names, which these are not.
pub(crate) fn valid_path(path: &str) -> bool {
	!path.is_empty()
		&& !path.contains(['\\', '\0'])
		&& path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

impl Manifest {
	pub(crate) fn parse(bytes: &[u8], limits: &Limits, warnings: &mut Vec<String>) -> Result<Self> {
		let text = std::str::from_utf8(bytes).map_err(|_| Error::Manifest("it is not UTF-8".into()))?;
		if text.starts_with('\u{feff}') {
			return Err(Error::Manifest("it starts with a byte-order mark".into()));
		}
		let mut header = None;
		let mut package = None;
		let mut content: Vec<Content> = Vec::new();
		let mut paths = BTreeSet::new();
		let mut unknown = BTreeSet::new();
		for (index, line) in text.split('\n').enumerate() {
			let line = line.strip_suffix('\r').unwrap_or(line);
			if line.trim().is_empty() {
				continue;
			}
			let number = index + 1;
			let fail = |message: String| Error::Manifest(format!("line {number}: {message}"));
			if line.len() > limits.max_record_bytes {
				return Err(Error::Limit("manifest record bytes"));
			}
			let record = parse_record(line).map_err(|error| fail(error.to_string()))?;
			let kind = record.get("kind").and_then(Value::as_str).ok_or_else(|| fail("the record has no 'kind'".into()))?;
			match kind {
				"header" => {
					if header.is_some() || package.is_some() || !content.is_empty() {
						return Err(fail("the header must be the first record and appear once".into()));
					}
					let version = record
						.get("version")
						.and_then(Value::as_u64)
						.ok_or_else(|| fail("the header has no integer 'version'".into()))?;
					if version != VERSION {
						return Err(Error::Unsupported(format!(
							"manifest version {version}: this reader knows version {VERSION}"
						)));
					}
					let algorithm = record
						.get("digestAlgorithm")
						.and_then(Value::as_str)
						.filter(|name| !name.is_empty())
						.ok_or_else(|| fail("the header has no 'digestAlgorithm'".into()))?;
					note_unknown(&record, &["kind", "version", "digestAlgorithm"], "header", &mut unknown);
					header = Some((version, algorithm.to_owned()));
				}
				"package" => {
					if header.is_none() {
						return Err(fail("the first record must be the header".into()));
					}
					if package.is_some() || !content.is_empty() {
						return Err(fail("the package must follow the header and appear once".into()));
					}
					let kind = record
						.get("type")
						.and_then(Value::as_str)
						.filter(|name| !name.is_empty())
						.ok_or_else(|| fail("the package has no 'type'".into()))?
						.to_owned();
					let place = match record.get("place") {
						None | Some(Value::Null) => None,
						Some(Value::String(path)) if valid_path(path) => Some(path.clone()),
						Some(_) => return Err(fail("the package's 'place' is not a valid logical path".into())),
					};
					package = Some(Package { kind, place, fields: record });
				}
				"content" => {
					if package.is_none() {
						return Err(fail("a header and a package must come before content records".into()));
					}
					if content.len() >= limits.max_entries {
						return Err(Error::Limit("content records"));
					}
					note_unknown(&record, &["kind", "path", "objectDigest", "asset", "metadata"], "content", &mut unknown);
					let record = parse_content(&record).map_err(fail)?;
					if let Some(path) = &record.path
						&& !paths.insert(path.clone())
					{
						return Err(fail(format!("the logical path '{path}' is used twice")));
					}
					content.push(record);
				}
				other => {
					return Err(Error::Unsupported(format!(
						"manifest line {number} is a '{other}' record: version {VERSION} has header, package and content records"
					)));
				}
			}
		}
		let Some((version, digest_algorithm)) = header else {
			return Err(Error::Manifest("it has no header record".into()));
		};
		let Some(package) = package else {
			return Err(Error::Manifest("it has no package record".into()));
		};
		if !unknown.is_empty() {
			let fields: Vec<_> = unknown.into_iter().collect();
			warnings.push(format!("ignored manifest fields this reader does not know: {}", fields.join(", ")));
		}
		Ok(Self { version, digest_algorithm, package, content })
	}
}

fn note_unknown(record: &Map<String, Value>, known: &[&str], kind: &str, unknown: &mut BTreeSet<String>) {
	for field in record.keys().filter(|field| !known.contains(&field.as_str())) {
		unknown.insert(format!("{kind}.{field}"));
	}
}

fn parse_content(record: &Map<String, Value>) -> std::result::Result<Content, String> {
	let path = match record.get("path") {
		None | Some(Value::Null) => None,
		Some(Value::String(path)) if valid_path(path) => Some(path.clone()),
		Some(_) => return Err("the content record's 'path' is not a valid logical path".into()),
	};
	let object_digest = match record.get("objectDigest") {
		None | Some(Value::Null) => None,
		Some(Value::String(digest)) if valid_digest(digest) => Some(digest.clone()),
		Some(_) => return Err("the content record's 'objectDigest' is not a lowercase hex digest".into()),
	};
	let asset = match record.get("asset") {
		None | Some(Value::Null) => None,
		Some(Value::Object(asset)) => {
			let id = asset.get("id").and_then(Value::as_u64).ok_or("the content record's asset has no integer 'id'")?;
			let version = match asset.get("version") {
				None | Some(Value::Null) => None,
				Some(value) => Some(value.as_u64().ok_or("the content record's asset 'version' is not an integer")?),
			};
			let tag = match asset.get("tag") {
				None | Some(Value::Null) => None,
				Some(Value::String(tag)) => Some(tag.clone()),
				Some(_) => return Err("the content record's asset 'tag' is not a string".into()),
			};
			Some(Asset { id, version, tag })
		}
		Some(_) => return Err("the content record's 'asset' is not an object".into()),
	};
	let metadata = match record.get("metadata") {
		None | Some(Value::Null) => None,
		Some(Value::Object(metadata)) => Some(metadata.clone()),
		Some(_) => return Err("the content record's 'metadata' is not an object".into()),
	};
	if object_digest.is_none() && asset.is_none() {
		return Err("the content record has neither an 'objectDigest' nor an 'asset'".into());
	}
	Ok(Content { path, object_digest, asset, metadata })
}

/// One record: a JSON object with no key repeated at any depth. serde_json keeps the last of two
/// equal keys, which would let two readers disagree about what a record says.
fn parse_record(line: &str) -> serde_json::Result<Map<String, Value>> {
	let mut decoder = serde_json::Deserializer::from_str(line);
	Unique(0).deserialize(&mut decoder)?;
	decoder.end()?;
	match serde_json::from_str(line)? {
		Value::Object(record) => Ok(record),
		_ => Err(de::Error::custom("the record is not a JSON object")),
	}
}

struct Unique(usize);

impl<'de> DeserializeSeed<'de> for Unique {
	type Value = ();

	fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> std::result::Result<(), D::Error> {
		decoder.deserialize_any(self)
	}
}

impl<'de> Visitor<'de> for Unique {
	type Value = ();

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("JSON whose objects repeat no key")
	}

	fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
		Ok(())
	}

	fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> std::result::Result<(), A::Error> {
		if self.0 >= MAX_DEPTH {
			return Err(de::Error::custom("the record is nested too deeply"));
		}
		while items.next_element_seed(Unique(self.0 + 1))?.is_some() {}
		Ok(())
	}

	fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> std::result::Result<(), A::Error> {
		if self.0 >= MAX_DEPTH {
			return Err(de::Error::custom("the record is nested too deeply"));
		}
		let mut seen = BTreeSet::new();
		while let Some(key) = fields.next_key::<String>()? {
			if !seen.insert(key.clone()) {
				return Err(de::Error::custom(format!("the key '{key}' is repeated")));
			}
			fields.next_value_seed(Unique(self.0 + 1))?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse(text: &str) -> Result<(Manifest, Vec<String>)> {
		let mut warnings = Vec::new();
		Manifest::parse(text.as_bytes(), &Limits::default(), &mut warnings).map(|manifest| (manifest, warnings))
	}

	const HEADER: &str = r#"{"kind":"header","version":1,"digestAlgorithm":"blake3-128"}"#;
	const DIGEST: &str = "00112233445566778899aabbccddeeff";

	#[test]
	fn the_specifications_example_parses() {
		let text = format!(
			"{HEADER}\n{}\n{}\n{}\n{}\n{}\n",
			r#"{"kind":"package","type":"place","place":"Place.rbxl"}"#,
			format_args!(r#"{{"kind":"content","path":"Place.rbxl","objectDigest":"{DIGEST}"}}"#),
			format_args!(r#"{{"kind":"content","path":"Scripts/Main.luau","objectDigest":"{DIGEST}"}}"#),
			r#"{"kind":"content","path":"Models/Tree","asset":{"id":1818,"version":3}}"#,
			format_args!(r#"{{"kind":"content","asset":{{"id":2020,"version":1}},"objectDigest":"{DIGEST}"}}"#),
		);
		let (manifest, warnings) = parse(&text).unwrap();
		assert_eq!(manifest.version, 1);
		assert_eq!(manifest.digest_algorithm, "blake3-128");
		assert_eq!(manifest.package.kind, "place");
		assert_eq!(manifest.package.place.as_deref(), Some("Place.rbxl"));
		assert_eq!(manifest.content.len(), 4);
		assert_eq!(manifest.content[2].asset, Some(Asset { id: 1818, version: Some(3), tag: None }));
		assert_eq!(manifest.content[2].object_digest, None);
		assert_eq!(manifest.content[3].path, None);
		assert!(warnings.is_empty(), "{warnings:?}");
	}

	#[test]
	fn a_model_package_needs_no_place() {
		let text = format!("{HEADER}\r\n{}\r\n", r#"{"kind": "package", "type":"model"}"#);
		let (manifest, _) = parse(&text).unwrap();
		assert_eq!(manifest.package.kind, "model");
		assert_eq!(manifest.package.place, None);
		assert!(manifest.content.is_empty());
	}

	#[test]
	fn records_out_of_order_are_refused() {
		let package = r#"{"kind":"package","type":"model"}"#;
		let content = format!(r#"{{"kind":"content","objectDigest":"{DIGEST}"}}"#);
		for text in [
			format!("{package}\n{HEADER}\n"),
			format!("{HEADER}\n{HEADER}\n{package}\n"),
			format!("{HEADER}\n{content}\n{package}\n"),
			format!("{HEADER}\n{package}\n{package}\n"),
			format!("{HEADER}\n"),
			String::new(),
		] {
			assert!(matches!(parse(&text), Err(Error::Manifest(_))), "{text}");
		}
	}

	#[test]
	fn a_newer_version_and_an_unknown_record_are_unsupported() {
		let package = r#"{"kind":"package","type":"model"}"#;
		let newer = r#"{"kind":"header","version":2,"digestAlgorithm":"blake3-128"}"#;
		assert!(matches!(parse(&format!("{newer}\n{package}\n")), Err(Error::Unsupported(_))));
		let other = r#"{"kind":"signature","value":"x"}"#;
		assert!(matches!(parse(&format!("{HEADER}\n{package}\n{other}\n")), Err(Error::Unsupported(_))));
	}

	#[test]
	fn a_content_record_needs_bytes_or_an_asset_and_a_sound_path() {
		let package = r#"{"kind":"package","type":"model"}"#;
		for record in [
			r#"{"kind":"content","path":"A"}"#.to_owned(),
			format!(r#"{{"kind":"content","path":"../A","objectDigest":"{DIGEST}"}}"#),
			format!(r#"{{"kind":"content","path":"A//B","objectDigest":"{DIGEST}"}}"#),
			format!(r#"{{"kind":"content","path":"A\\B","objectDigest":"{DIGEST}"}}"#),
			r#"{"kind":"content","objectDigest":"ABCDEF0011223344"}"#.to_owned(),
			r#"{"kind":"content","asset":{"id":"12"}}"#.to_owned(),
			format!(r#"{{"kind":"content","objectDigest":"{DIGEST}","metadata":[]}}"#),
		] {
			let text = format!("{HEADER}\n{package}\n{record}\n");
			assert!(matches!(parse(&text), Err(Error::Manifest(_))), "{record}");
		}
		let twice = format!(r#"{{"kind":"content","path":"A","objectDigest":"{DIGEST}"}}"#);
		assert!(matches!(parse(&format!("{HEADER}\n{package}\n{twice}\n{twice}\n")), Err(Error::Manifest(_))));
	}

	#[test]
	fn a_repeated_key_is_refused_and_an_unknown_field_is_noted() {
		let package = r#"{"kind":"package","type":"model","type":"place"}"#;
		assert!(matches!(parse(&format!("{HEADER}\n{package}\n")), Err(Error::Manifest(_))));
		let header = r#"{"kind":"header","version":1,"digestAlgorithm":"blake3-128","generator":"x"}"#;
		let package = r#"{"kind":"package","type":"model","title":"kept as package metadata"}"#;
		let (manifest, warnings) = parse(&format!("{header}\n{package}\n")).unwrap();
		assert_eq!(manifest.package.fields["title"], "kept as package metadata");
		assert_eq!(warnings.len(), 1);
		assert!(warnings[0].contains("header.generator"), "{warnings:?}");
	}

	#[test]
	fn digests_are_named_by_algorithm() {
		assert_eq!(Digest::named("blake3-128"), Some(Digest::Blake3_128));
		assert_eq!(Digest::named("BLAKE3-128"), Some(Digest::Blake3_128));
		// names nobody has defined are not guessed at: what a property's 16-byte reference would mean
		// under them is unknown
		for unknown in ["blake3", "blake3-256", "sha256", ""] {
			assert_eq!(Digest::named(unknown), None, "{unknown}");
		}
		// BLAKE3's published test vector for the empty input
		assert_eq!(Digest::Blake3_128.hex(b""), "af1349b9f5f9a1a6a0404dea36dcc949");
	}

	#[test]
	fn a_logical_path_may_be_longer_than_a_member_name() {
		// the 128-character limit is on ZIP member names; objects are named by digest, not by path
		let package = r#"{"kind":"package","type":"model"}"#;
		let long = format!("Scripts/{}/Main.luau", "Deeply/Nested".repeat(20));
		assert!(long.len() > 128);
		let record = format!(r#"{{"kind":"content","path":"{long}","objectDigest":"{DIGEST}"}}"#);
		let (manifest, _) = parse(&format!("{HEADER}\n{package}\n{record}\n")).unwrap();
		assert_eq!(manifest.content[0].path.as_deref(), Some(long.as_str()));
	}
}
