//! Roblox Archive (`.rbx`) containers.
//!
//! A `.rbx` file is a ZIP whose members are stored uncompressed: a `manifest.jsonl` at the root and
//! content-addressed files under `objects/`. The manifest's `package` record names what the archive
//! holds; for a place it points, through a `content` record, at an ordinary binary `.rbxl` among the
//! objects. The format is described at <https://create.roblox.com/docs/rbx> (version 1, published
//! ahead of its release) and in [docs/archives.md](../../../docs/archives.md).
//!
//! This crate reads the container without a DOM, so every tool that already parses `.rbxl` and
//! `.rbxm` can accept `.rbx` by passing its input through [`unwrap`] first:
//!
//! ```
//! # fn load(bytes: &[u8]) -> Result<(), ror_archive::Error> {
//! let scene = ror_archive::unwrap(bytes)?;
//! // `scene.bytes` is the input itself, or the standalone .rbxl/.rbxm an archive holds
//! # let _ = scene; Ok(())
//! # }
//! ```
//!
//! A scene inside an archive is not always standalone: a writer may move a property's bytes (a
//! script's `Source`, a shared string) into `objects/` and leave an empty placeholder in the scene,
//! marked by a property tag older parsers skip. [`Archive::flatten`] puts those values back, so the
//! result reads the same in a parser that knows nothing about archives. What cannot be written into
//! a standalone file (content embedded as an opaque object) is listed in the [`Report`].

mod manifest;
mod pack;
mod scene;
#[cfg(test)]
mod tests;
mod zip;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

pub use manifest::{Asset, Content, Manifest, Package};
pub use pack::Builder;
pub use scene::{EmbeddedContent, Report};

/// The ZIP local file header signature every archive starts with.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
/// An empty ZIP holds only its end-of-central-directory record.
const ZIP_EMPTY_MAGIC: &[u8] = b"PK\x05\x06";

/// Why an archive could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	/// The bytes are not a ZIP file at all.
	#[error("not a Roblox archive: the file does not start with a ZIP header")]
	NotArchive,
	/// The ZIP container breaks the archive rules (compressed or encrypted members, bad offsets, a
	/// member that fails its CRC-32).
	#[error("invalid .rbx container: {0}")]
	Container(String),
	/// `manifest.jsonl` is missing or malformed.
	#[error("invalid .rbx manifest: {0}")]
	Manifest(String),
	/// The archive is well-formed but uses something this reader does not know: a newer manifest
	/// version, another package type, a property tag it cannot restore.
	#[error("unsupported .rbx archive: {0}")]
	Unsupported(String),
	/// A member's bytes do not match the digest it is stored under.
	#[error("corrupt .rbx archive: object {digest} does not match its digest")]
	Digest {
		/// The digest the object is named by.
		digest: String,
	},
	/// The scene the archive holds is malformed.
	#[error("invalid scene in .rbx archive: {0}")]
	Scene(String),
	/// A configured limit was exceeded.
	#[error(".rbx archive exceeds a limit: {0}")]
	Limit(&'static str),
}

/// A specialized result for archive operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Bounds on what a reader accepts. Members are stored uncompressed, so an archive cannot expand
/// past its own size. The scene's chunks are compressed: `max_chunk_bytes` bounds one of them and
/// `max_scene_bytes` all of them together.
#[derive(Debug, Clone)]
pub struct Limits {
	/// The whole archive (default 4 GiB).
	pub max_archive_bytes: u64,
	/// `manifest.jsonl` (default 64 MiB).
	pub max_manifest_bytes: u64,
	/// One manifest record (default 1 MiB).
	pub max_record_bytes: usize,
	/// ZIP members, and content records (default 1,000,000).
	pub max_entries: usize,
	/// One chunk of the scene, decompressed: as it is read, and as it is written back with its values
	/// restored (default 1 GiB).
	pub max_chunk_bytes: usize,
	/// Every chunk of the scene, decompressed (default 8 GiB).
	pub max_scene_bytes: u64,
	/// Bytes written back into the scene from `objects/` (default 2 GiB). A string counts once for
	/// each value that holds it, a shared string once however many values share it.
	pub max_restored_bytes: u64,
}

impl Default for Limits {
	fn default() -> Self {
		Self {
			max_archive_bytes: 4 << 30,
			max_manifest_bytes: 64 << 20,
			max_record_bytes: 1 << 20,
			max_entries: 1_000_000,
			max_chunk_bytes: 1 << 30,
			max_scene_bytes: 8 << 30,
			max_restored_bytes: 2 << 30,
		}
	}
}

/// How the scene an archive holds is serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneFormat {
	/// Roblox's binary format (`<roblox!`), as in `.rbxl` and `.rbxm`.
	Binary,
	/// Roblox's XML format (`<roblox `), as in `.rbxlx` and `.rbxmx`.
	Xml,
}

impl SceneFormat {
	/// The format of a serialized place or model, from its first bytes.
	pub fn sniff(bytes: &[u8]) -> Option<Self> {
		if bytes.starts_with(b"<roblox!") {
			return Some(Self::Binary);
		}
		let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
		let start = text.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(text.len());
		let head = &text[start..text.len().min(start + 512)];
		if head.starts_with(b"<roblox") {
			return Some(Self::Xml);
		}
		// an XML declaration first: `<?xml ...?>` then `<roblox`
		if head.starts_with(b"<?xml") {
			let text = String::from_utf8_lossy(head);
			if let Some(end) = text.find("?>") {
				return text[end + 2..].trim_start().starts_with("<roblox").then_some(Self::Xml);
			}
		}
		None
	}
}

/// The scene an archive's package points at.
#[derive(Debug, Clone)]
pub struct Root<'a> {
	/// The logical path of the scene's content record, when it has one.
	pub path: Option<String>,
	/// The digest the scene is stored under.
	pub digest: String,
	/// The scene as stored. It may hold placeholders for values kept in other objects; see
	/// [`Archive::flatten`].
	pub bytes: &'a [u8],
	/// How the scene is serialized.
	pub format: SceneFormat,
}

/// A place or model that any `.rbxl` / `.rbxm` parser reads on its own.
#[derive(Debug, Clone)]
pub struct Scene<'a> {
	/// The serialized place or model: borrowed when nothing had to be restored.
	pub bytes: Cow<'a, [u8]>,
	/// How it is serialized.
	pub format: SceneFormat,
	/// The archive's package type (`place`, `model`), or `None` when the input was not an archive.
	pub package: Option<String>,
	/// What was restored, and what could not be.
	pub report: Report,
}

/// Whether `bytes` is a ZIP file, the container of every Roblox archive. A ZIP that is not an
/// archive (no `manifest.jsonl`) fails in [`Archive::open`] with a message that says so.
pub fn is_archive(bytes: &[u8]) -> bool {
	bytes.starts_with(ZIP_MAGIC) || bytes.starts_with(ZIP_EMPTY_MAGIC)
}

/// The standalone place or model behind `bytes`: the bytes themselves when they are not an archive,
/// else the archive's scene with every externally stored value put back. The one call a loader of
/// `.rbxl` / `.rbxm` files needs to accept `.rbx` too.
pub fn unwrap(bytes: &[u8]) -> Result<Scene<'_>> {
	unwrap_with(bytes, &Limits::default())
}

/// [`unwrap`] with explicit limits.
pub fn unwrap_with<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Scene<'a>> {
	if !is_archive(bytes) {
		let format = SceneFormat::sniff(bytes).ok_or(Error::NotArchive)?;
		return Ok(Scene { bytes: Cow::Borrowed(bytes), format, package: None, report: Report::default() });
	}
	Archive::open_with(bytes, limits)?.flatten()
}

/// A validated archive over borrowed bytes. Members are stored uncompressed, so every object is a
/// slice of the input.
#[derive(Debug)]
pub struct Archive<'a> {
	bytes: &'a [u8],
	manifest: Manifest,
	objects: BTreeMap<String, Range<usize>>,
	/// Whether every object was checked against the digest it is named by.
	verified: bool,
	warnings: Vec<String>,
	limits: Limits,
}

impl<'a> Archive<'a> {
	/// Reads and validates an archive with the default [`Limits`].
	pub fn open(bytes: &'a [u8]) -> Result<Self> {
		Self::open_with(bytes, &Limits::default())
	}

	/// Reads and validates an archive: the ZIP container, every member's CRC-32, the manifest, and
	/// every object's digest when the digest algorithm is the one this crate computes (`blake3-128`).
	/// Under any other algorithm the objects are not checked against their names, which the warnings
	/// say; [`Archive::flatten`] still checks each object a property refers to against the digest in
	/// the reference.
	pub fn open_with(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
		if !is_archive(bytes) {
			return Err(Error::NotArchive);
		}
		if bytes.len() as u64 > limits.max_archive_bytes {
			return Err(Error::Limit("archive bytes"));
		}
		let mut warnings = Vec::new();
		let mut manifest = None;
		let mut objects = BTreeMap::new();
		for entry in zip::read(bytes, limits)? {
			if entry.name == manifest::MANIFEST_NAME {
				if entry.data.len() as u64 > limits.max_manifest_bytes {
					return Err(Error::Limit("manifest bytes"));
				}
				manifest = Some(Manifest::parse(&bytes[entry.data.clone()], limits, &mut warnings)?);
			} else if let Some(digest) = entry.name.strip_prefix(manifest::OBJECTS_PREFIX).filter(|name| !name.is_empty()) {
				if !manifest::valid_digest(digest) {
					return Err(Error::Container(format!("object member has an invalid name: {}", entry.name)));
				}
				objects.insert(digest.to_owned(), entry.data.clone());
			} else if entry.name.ends_with('/') && entry.data.is_empty() {
				// a directory entry, as general ZIP tools write
			} else {
				warnings.push(format!("ignored a member that is neither the manifest nor an object: {}", entry.name));
			}
		}
		let Some(manifest) = manifest else {
			return Err(Error::Manifest(
				"the ZIP has no manifest.jsonl, so it is not a Roblox archive (.rbx)".into(),
			));
		};
		let verify = manifest::Digest::named(&manifest.digest_algorithm);
		if verify.is_none() {
			warnings.push(format!(
				"objects were not checked against their names: digest algorithm '{}' is not one this reader computes",
				manifest.digest_algorithm
			));
		}
		for (digest, range) in &objects {
			if let Some(verify) = verify
				&& verify.hex(&bytes[range.clone()]) != *digest
			{
				return Err(Error::Digest { digest: digest.clone() });
			}
		}
		let mut referenced = std::collections::BTreeSet::new();
		for record in &manifest.content {
			if let Some(digest) = &record.object_digest {
				if !objects.contains_key(digest) {
					return Err(Error::Manifest(format!("a content record names object {digest}, which the archive does not hold")));
				}
				referenced.insert(digest.as_str());
			}
		}
		let unlisted = objects.keys().filter(|digest| !referenced.contains(digest.as_str())).count();
		if unlisted > 0 {
			warnings.push(format!("{unlisted} object(s) have no content record"));
		}
		Ok(Self { bytes, manifest, objects, verified: verify.is_some(), warnings, limits: limits.clone() })
	}

	/// The parsed manifest.
	pub fn manifest(&self) -> &Manifest {
		&self.manifest
	}

	/// Things a strict reader would refuse that this one read past: members it ignored, digests it
	/// could not check, fields it does not know.
	pub fn warnings(&self) -> &[String] {
		&self.warnings
	}

	/// The object stored under `digest`.
	pub fn object(&self, digest: &str) -> Option<&'a [u8]> {
		self.objects.get(digest).map(|range| &self.bytes[range.clone()])
	}

	/// Every object, by digest.
	pub fn objects(&self) -> impl Iterator<Item = (&str, &'a [u8])> + '_ {
		self.objects.iter().map(|(digest, range)| (digest.as_str(), &self.bytes[range.clone()]))
	}

	/// The embedded bytes of the content record with this logical path.
	pub fn content(&self, path: &str) -> Option<&'a [u8]> {
		let record = self.manifest.content.iter().find(|record| record.path.as_deref() == Some(path))?;
		self.object(record.object_digest.as_deref()?)
	}

	/// The scene the package points at, as stored.
	///
	/// A `place` package names it by logical path. The specification gives a `model` package no such
	/// field yet, so for one the scene is, in order: the record its `model` field names, the only
	/// record whose path ends in `.rbxm` or `.rbxmx`, or the only object that is a serialized model.
	pub fn root(&self) -> Result<Root<'a>> {
		let package = &self.manifest.package;
		let record = match package.kind.as_str() {
			"place" => {
				let path = package.place.as_deref().ok_or_else(|| {
					Error::Manifest("the place package has no 'place' path naming its serialized place".into())
				})?;
				self.record_at(path).ok_or_else(|| {
					Error::Manifest(format!("the package's place path '{path}' has no content record"))
				})?
			}
			"model" => self.model_record()?,
			other => {
				return Err(Error::Unsupported(format!(
					"package type '{other}': this reader opens place and model packages"
				)));
			}
		};
		let digest = record.object_digest.clone().ok_or_else(|| {
			Error::Unsupported(
				"the package's scene is an asset reference with no embedded bytes; download the asset instead".into(),
			)
		})?;
		let bytes = self.object(&digest).expect("content digests were checked against the objects");
		let format = SceneFormat::sniff(bytes).ok_or_else(|| {
			Error::Scene("the package's scene is neither a binary nor an XML Roblox file".into())
		})?;
		Ok(Root { path: record.path.clone(), digest, bytes, format })
	}

	fn record_at(&self, path: &str) -> Option<&Content> {
		self.manifest.content.iter().find(|record| record.path.as_deref() == Some(path))
	}

	fn model_record(&self) -> Result<&Content> {
		if let Some(path) = self.manifest.package.fields.get("model").and_then(|value| value.as_str()) {
			return self.record_at(path).ok_or_else(|| {
				Error::Manifest(format!("the package's model path '{path}' has no content record"))
			});
		}
		let embedded = || self.manifest.content.iter().filter(|record| record.object_digest.is_some());
		let by_path: Vec<_> = embedded()
			.filter(|record| {
				record.path.as_deref().is_some_and(|path| {
					let lower = path.to_ascii_lowercase();
					lower.ends_with(".rbxm") || lower.ends_with(".rbxmx")
				})
			})
			.collect();
		if let [record] = by_path[..] {
			return Ok(record);
		}
		let mut seen = std::collections::BTreeSet::new();
		let by_bytes: Vec<_> = embedded()
			.filter(|record| {
				let digest = record.object_digest.as_deref().expect("filtered to embedded records");
				seen.insert(digest) && self.object(digest).is_some_and(|bytes| SceneFormat::sniff(bytes).is_some())
			})
			.collect();
		match by_bytes[..] {
			[record] => Ok(record),
			[] => Err(Error::Unsupported("the model package holds no serialized model".into())),
			_ => Err(Error::Unsupported(format!(
				"the model package holds {} serialized models and does not say which is its root",
				by_bytes.len()
			))),
		}
	}

	/// The archive's place or model as a standalone file: the scene with every value that lives in
	/// `objects/` written back into it. Borrowed when the scene was standalone already.
	pub fn flatten(&self) -> Result<Scene<'a>> {
		let root = self.root()?;
		let (bytes, mut report) = match root.format {
			SceneFormat::Xml => (Cow::Borrowed(root.bytes), Report::default()),
			SceneFormat::Binary => scene::flatten(root.bytes, &root.digest, self)?,
		};
		report.warnings.splice(0..0, self.warnings.iter().cloned());
		Ok(Scene { bytes, format: root.format, package: Some(self.manifest.package.kind.clone()), report })
	}

	pub(crate) fn limits(&self) -> &Limits {
		&self.limits
	}

	/// Whether opening checked every object against the digest it is named by.
	pub(crate) fn verified(&self) -> bool {
		self.verified
	}
}
