//! Writing archives: wrapping a serialized place or model, with the option of moving its large
//! string values into objects the way a writer that saves incrementally does.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use crate::manifest::{BLAKE3_128, Digest, MANIFEST_NAME, OBJECTS_PREFIX, VERSION, valid_path};
use crate::scene::{
	self, Class, HEADER_LEN, Reader, Reference, TAG_EMBEDDED, TYPE_SHARED_STRING, TYPE_STRING, push_sized, read_inst,
	read_interleaved, write_chunk, write_interleaved, write_references,
};
use crate::{Asset, Error, Limits, Result, SceneFormat, zip};

/// The logical path a place's scene is given, as in the format's examples.
const PLACE_PATH: &str = "Place.rbxl";
/// The logical path a model's scene is given. The format does not name one yet.
const MODEL_PATH: &str = "Model.rbxm";

/// Properties that share the string and shared string columns without holding plain text or bytes:
/// names, tags, attributes, material colours, asset maps and the content ids of the older content
/// type (the list rbx-dom's reflection database gives). A reader that knows each property's type
/// takes references on text and bytes only, so [`Builder::externalize`] leaves these inline. Sorted.
const STRUCTURED: &[&[u8]] = &[
	b"ActivatedCursorIcon",
	b"AnimationId",
	b"Asset",
	b"AssetId",
	b"AttributesSerialize",
	b"BottomImage",
	b"CageMeshId",
	b"CameraButtonIcon",
	b"ColorMap",
	b"ContentMap",
	b"CursorIcon",
	b"FallbackImage",
	b"Graphic",
	b"HSRAssetId",
	b"HoverImage",
	b"Icon",
	b"Image",
	b"LinkedSource",
	b"MaterialColors",
	b"MeshId",
	b"MetalnessMap",
	b"MidImage",
	b"MoonTextureId",
	b"MouseIcon",
	b"Name",
	b"NormalMap",
	b"PackageId",
	b"PantsTemplate",
	b"PressedImage",
	b"PropertiesSerialize",
	b"ReferenceMeshId",
	b"RoughnessMap",
	b"ShirtTemplate",
	b"SkyboxBk",
	b"SkyboxDn",
	b"SkyboxFt",
	b"SkyboxLf",
	b"SkyboxRt",
	b"SkyboxUp",
	b"SolidMeshHolder",
	b"SoundId",
	b"SunTextureId",
	b"Tags",
	b"TemporaryCageMeshId",
	b"TemporaryReferenceId",
	b"Texture",
	b"TextureID",
	b"TextureId",
	b"TexturePack",
	b"TopImage",
	b"Video",
];

/// Which values [`Builder::build`] moves out of the scene.
#[derive(Clone, Copy)]
enum Externalize {
	/// Text and bytes of at least this many bytes.
	Plain(usize),
	/// Everything in a string or shared string column but names.
	All(usize),
}

impl Externalize {
	fn min_bytes(self) -> usize {
		match self {
			Self::Plain(min_bytes) | Self::All(min_bytes) => min_bytes,
		}
	}

	fn moves(self, property: &[u8]) -> bool {
		match self {
			Self::Plain(_) => STRUCTURED.binary_search(&property).is_err(),
			Self::All(_) => property != b"Name",
		}
	}
}

struct Record {
	path: Option<String>,
	bytes: Option<Vec<u8>>,
	asset: Option<Asset>,
	metadata: Option<Map<String, Value>>,
}

/// Builds an archive around a serialized place or model.
///
/// ```
/// # fn wrap(place: Vec<u8>) -> Result<Vec<u8>, ror_archive::Error> {
/// ror_archive::Builder::place(place).build()
/// # }
/// ```
pub struct Builder {
	kind: &'static str,
	scene_path: &'static str,
	scene: Vec<u8>,
	externalize: Option<Externalize>,
	records: Vec<Record>,
	zip64: bool,
}

impl Builder {
	/// An archive of a place: `scene` is the bytes of a `.rbxl` (or `.rbxlx`).
	pub fn place(scene: Vec<u8>) -> Self {
		Self { kind: "place", scene_path: PLACE_PATH, scene, externalize: None, records: Vec::new(), zip64: false }
	}

	/// An archive of a model: `scene` is the bytes of a `.rbxm` (or `.rbxmx`). The format gives model
	/// packages no layout yet; this writes the model as the content record `Model.rbxm` and names
	/// that path in the package's `model` field, which is where [`crate::Archive::root`] looks first.
	pub fn model(scene: Vec<u8>) -> Self {
		Self { kind: "model", scene_path: MODEL_PATH, scene, externalize: None, records: Vec::new(), zip64: false }
	}

	/// Moves the text and byte values of at least `min_bytes` bytes (script sources, terrain grids,
	/// collision and mesh data) out of a binary scene into objects of their own, leaving an empty
	/// placeholder and a reference behind. Equal values share one object. Names, tags, attributes and
	/// content ids stay inline: they share the same columns, and a reader that knows each property's
	/// type takes references on text and bytes only.
	pub fn externalize(mut self, min_bytes: usize) -> Self {
		self.externalize = Some(Externalize::Plain(min_bytes.max(1)));
		self
	}

	/// Like [`Builder::externalize`] for every string and shared string value but names, whatever the
	/// property holds. [`crate::Archive::flatten`] puts all of them back; a reader that types its
	/// properties may refuse the archive. For testing readers.
	pub fn externalize_all(mut self, min_bytes: usize) -> Self {
		self.externalize = Some(Externalize::All(min_bytes.max(1)));
		self
	}

	/// Adds embedded content: bytes under an optional logical path, as a script kept beside the place.
	pub fn content(mut self, path: Option<&str>, bytes: Vec<u8>) -> Self {
		self.records.push(Record { path: path.map(str::to_owned), bytes: Some(bytes), asset: None, metadata: None });
		self
	}

	/// Adds a reference to a Roblox asset, with the asset's bytes when they are embedded too.
	pub fn asset(mut self, path: Option<&str>, asset: Asset, bytes: Option<Vec<u8>>) -> Self {
		self.records.push(Record { path: path.map(str::to_owned), bytes, asset: Some(asset), metadata: None });
		self
	}

	/// Sets the metadata of the record added last.
	pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
		if let Some(record) = self.records.last_mut() {
			record.metadata = Some(metadata);
		}
		self
	}

	/// Writes ZIP64 records even when the archive is small enough not to need them.
	pub fn zip64(mut self, force: bool) -> Self {
		self.zip64 = force;
		self
	}

	/// The archive's bytes: the manifest first, then the objects in digest order, each stored
	/// uncompressed. The same input always makes the same bytes.
	pub fn build(self) -> Result<Vec<u8>> {
		let format = SceneFormat::sniff(&self.scene).ok_or_else(|| Error::Scene("the scene is neither a binary nor an XML Roblox file".into()))?;
		let mut objects: BTreeMap<String, Vec<u8>> = BTreeMap::new();
		let mut extracted = Vec::new();
		let scene = match (self.externalize, format) {
			(Some(rule), SceneFormat::Binary) => externalize(&self.scene, rule, &mut extracted)?,
			_ => self.scene,
		};
		let digest = Digest::Blake3_128;
		let scene_digest = digest.hex(&scene);
		let mut lines = vec![
			format!(r#"{{"kind":"header","version":{VERSION},"digestAlgorithm":"{BLAKE3_128}"}}"#),
			match self.kind {
				"place" => format!(r#"{{"kind":"package","type":"place","place":{}}}"#, json(self.scene_path)),
				kind => format!(r#"{{"kind":"package","type":"{kind}","model":{}}}"#, json(self.scene_path)),
			},
			format!(r#"{{"kind":"content","path":{},"objectDigest":"{scene_digest}"}}"#, json(self.scene_path)),
		];
		objects.insert(scene_digest.clone(), scene);
		let mut paths = vec![self.scene_path.to_owned()];
		for bytes in extracted {
			let name = digest.hex(&bytes);
			if name == scene_digest {
				return Err(Error::Scene("a value moved out of the scene has the scene's own digest".into()));
			}
			if objects.insert(name.clone(), bytes).is_none() {
				lines.push(format!(r#"{{"kind":"content","objectDigest":"{name}"}}"#));
			}
		}
		for record in self.records {
			let mut line = String::from(r#"{"kind":"content""#);
			if let Some(path) = &record.path {
				if !valid_path(path) || paths.contains(path) {
					return Err(Error::Manifest(format!("the logical path '{path}' is invalid or used twice")));
				}
				paths.push(path.clone());
				line.push_str(&format!(r#","path":{}"#, json(path)));
			}
			if let Some(asset) = &record.asset {
				line.push_str(&format!(r#","asset":{{"id":{}"#, asset.id));
				if let Some(version) = asset.version {
					line.push_str(&format!(r#","version":{version}"#));
				}
				if let Some(tag) = &asset.tag {
					line.push_str(&format!(r#","tag":{}"#, json(tag)));
				}
				line.push('}');
			}
			if let Some(bytes) = record.bytes {
				let name = digest.hex(&bytes);
				line.push_str(&format!(r#","objectDigest":"{name}""#));
				objects.insert(name, bytes);
			} else if record.asset.is_none() {
				return Err(Error::Manifest("a content record needs bytes or an asset".into()));
			}
			if let Some(metadata) = record.metadata {
				line.push_str(&format!(r#","metadata":{}"#, Value::Object(metadata)));
			}
			line.push('}');
			lines.push(line);
		}
		let mut manifest = lines.join("\n");
		manifest.push('\n');
		let mut members: Vec<(String, &[u8])> = vec![(MANIFEST_NAME.to_owned(), manifest.as_bytes())];
		members.extend(objects.iter().map(|(name, bytes)| (format!("{OBJECTS_PREFIX}{name}"), bytes.as_slice())));
		Ok(zip::write(&members, self.zip64))
	}
}

fn json(text: &str) -> String {
	Value::String(text.to_owned()).to_string()
}

/// The shared string table with the large entries taken out.
struct Table {
	/// Where each stored index goes: a kept entry's new index, or the reference that replaces it.
	moves: Vec<std::result::Result<u32, Reference>>,
	/// The index of the empty entry that stands in for a moved value.
	placeholder: u32,
	payload: Vec<u8>,
}

fn split_table(payload: &[u8], min_bytes: usize, inline: &HashSet<u32>, extracted: &mut Vec<Vec<u8>>) -> Result<Option<Table>> {
	let mut chunk = Reader::new(payload);
	if chunk.u32("the shared string table")? != 0 {
		return Err(Error::Unsupported("a shared string table of a newer version".into()));
	}
	let count = chunk.u32("the shared string table")? as usize;
	let mut kept = Vec::new();
	let mut kept_count = 0u32;
	let mut moves = Vec::with_capacity(count.min(payload.len() / 20));
	let mut placeholder = None;
	let mut moved = false;
	for index in 0..count {
		let hash = chunk.take(16, "a shared string's hash")?;
		let bytes = chunk.sized("a shared string")?;
		if bytes.len() >= min_bytes && !inline.contains(&(index as u32)) {
			let digest: [u8; 16] = blake3::hash(bytes).as_bytes()[..16].try_into().expect("sixteen bytes");
			moves.push(Err(Reference { digest, size: bytes.len() as u64, identity: None }));
			extracted.push(bytes.to_vec());
			moved = true;
			continue;
		}
		if bytes.is_empty() && placeholder.is_none() {
			placeholder = Some(kept_count);
		}
		moves.push(Ok(kept_count));
		kept.extend_from_slice(hash);
		push_sized(&mut kept, bytes);
		kept_count += 1;
	}
	if !moved {
		return Ok(None);
	}
	let placeholder = placeholder.unwrap_or_else(|| {
		kept.extend_from_slice(&blake3::hash(b"").as_bytes()[..16]);
		push_sized(&mut kept, b"");
		kept_count += 1;
		kept_count - 1
	});
	let mut payload = Vec::with_capacity(8 + kept.len());
	payload.extend_from_slice(&0u32.to_le_bytes());
	payload.extend_from_slice(&kept_count.to_le_bytes());
	payload.extend_from_slice(&kept);
	Ok(Some(Table { moves, placeholder, payload }))
}

/// A PROP chunk's class id, property name and type byte, with the reader left at the values. `None`
/// for a chunk that ends after the name.
fn prop_head<'a>(reader: &mut Reader<'a>) -> Result<Option<(u32, &'a [u8], u8)>> {
	let class_id = reader.u32("a PROP chunk")?;
	let property = reader.sized("a PROP chunk's property name")?;
	if reader.remaining() == 0 {
		return Ok(None);
	}
	let wire = reader.u8("a PROP chunk")?;
	if wire >= 0x80 {
		return Err(Error::Unsupported("the scene already stores values outside itself".into()));
	}
	Ok(Some((class_id, property, wire)))
}

fn count_of(classes: &BTreeMap<u32, Class>, class_id: u32) -> Result<usize> {
	classes
		.get(&class_id)
		.map(|class| class.count)
		.ok_or_else(|| Error::Scene(format!("a PROP chunk names class id {class_id}, which has no INST chunk")))
}

/// Rewrites a binary scene with its large string values replaced by references, and returns the
/// values taken out.
fn externalize(scene_bytes: &[u8], rule: Externalize, extracted: &mut Vec<Vec<u8>>) -> Result<Vec<u8>> {
	let max = Limits::default().max_chunk_bytes;
	let min_bytes = rule.min_bytes();
	let (list, end) = scene::chunks(scene_bytes)?;
	let mut scratch = Vec::new();
	// The shared string table comes before the properties that index it, so a first pass finds the
	// entries a property that stays inline points at. Those entries stay in the table.
	let mut classes: BTreeMap<u32, Class> = BTreeMap::new();
	let mut inline: HashSet<u32> = HashSet::new();
	let shared = list.iter().any(|chunk| &chunk.name == b"SSTR");
	for chunk in &list {
		match &chunk.name {
			b"INST" => {
				let (id, class) = read_inst(scene::data(scene_bytes, chunk, &mut scratch, max)?)?;
				classes.insert(id, class);
			}
			b"PROP" if shared => {
				let mut reader = Reader::new(scene::data(scene_bytes, chunk, &mut scratch, max)?);
				let Some((class_id, property, wire)) = prop_head(&mut reader)? else { continue };
				if wire == TYPE_SHARED_STRING && !rule.moves(property) {
					let count = count_of(&classes, class_id)?;
					inline.extend(read_interleaved(reader.take(count * 4, "a shared string property")?, count));
				}
			}
			_ => {}
		}
	}
	let mut out = Vec::with_capacity(scene_bytes.len());
	out.extend_from_slice(&scene_bytes[..HEADER_LEN]);
	let mut table: Option<Table> = None;
	for chunk in &list {
		let verbatim = &scene_bytes[chunk.whole.clone()];
		match &chunk.name {
			b"SSTR" => {
				table = split_table(scene::data(scene_bytes, chunk, &mut scratch, max)?, min_bytes, &inline, extracted)?;
				match &table {
					Some(table) => write_chunk(&mut out, b"SSTR", &table.payload)?,
					None => out.extend_from_slice(verbatim),
				}
			}
			b"PROP" => {
				let payload = scene::data(scene_bytes, chunk, &mut scratch, max)?;
				let mut reader = Reader::new(payload);
				let Some((class_id, property, wire)) = prop_head(&mut reader)? else {
					out.extend_from_slice(verbatim);
					continue;
				};
				// the type byte is the last one read
				let prefix_len = reader.at - 1;
				let count = count_of(&classes, class_id)?;
				let mut references: Vec<Option<Reference>> = vec![None; count];
				let mut values = Vec::new();
				match wire {
					TYPE_STRING if rule.moves(property) => {
						for reference in &mut references {
							let value = reader.sized("a string property")?;
							if value.len() < min_bytes {
								push_sized(&mut values, value);
								continue;
							}
							let digest: [u8; 16] = blake3::hash(value).as_bytes()[..16].try_into().expect("sixteen bytes");
							*reference = Some(Reference { digest, size: value.len() as u64, identity: None });
							extracted.push(value.to_vec());
							push_sized(&mut values, b"");
						}
						if references.iter().all(Option::is_none) {
							out.extend_from_slice(verbatim);
							continue;
						}
					}
					TYPE_SHARED_STRING if table.is_some() => {
						let table = table.as_ref().expect("checked");
						let raw = reader.take(count * 4, "a shared string property")?;
						let mut indices = read_interleaved(raw, count);
						for (index, reference) in indices.iter_mut().zip(&mut references) {
							match table.moves.get(*index as usize) {
								Some(Ok(kept)) => *index = *kept,
								Some(Err(moved)) => {
									*reference = Some(moved.clone());
									*index = table.placeholder;
								}
								None => return Err(Error::Scene("a shared string property indexes past its table".into())),
							}
						}
						write_interleaved(&mut values, &indices);
					}
					_ => {
						out.extend_from_slice(verbatim);
						continue;
					}
				}
				let mut rewritten = Vec::with_capacity(payload.len());
				rewritten.extend_from_slice(&payload[..prefix_len]);
				if references.iter().any(Option::is_some) {
					rewritten.push(TAG_EMBEDDED);
					write_references(&mut rewritten, &references);
				}
				rewritten.push(wire);
				rewritten.extend_from_slice(&values);
				write_chunk(&mut out, b"PROP", &rewritten)?;
			}
			_ => out.extend_from_slice(verbatim),
		}
	}
	out.extend_from_slice(&scene_bytes[end..]);
	Ok(out)
}
