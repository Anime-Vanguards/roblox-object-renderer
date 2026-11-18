//! `ror viewport-worker`: ViewportFrames drawn for another program, such as roblox-ui-renderer, on a
//! renderer kept warm between requests (docs/viewport-worker.md). One JSON request a line on stdin, one
//! JSON reply a line on stdout, one at a time; the worker exits when stdin closes.
//!
//! A request `{"viewport": <snapshot>, "out": "<file.png>"}` draws a snapshot of a ViewportFrame: its
//! instances as rbx-dom property variants with local references (WorldModels, meshes, bones and
//! SurfaceAppearances kept), its camera and its fill. `{"command": "asset", "uri": ..., "out": ...}`
//! writes an asset's bytes. A host linking this library adds commands of its own through [`Commands`].
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::render::camera::CameraSpec;
use crate::render::gpu::Gpu;
use crate::render::{Background, Renderer, ViewRequest};
use crate::scene::environment::{Environment, ViewportLighting};
use crate::scene::load::Document;
use crate::scene::props;

/// The snapshot format this worker reads (a snapshot's `version`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest picture a snapshot may ask for, per axis.
pub const MAX_SIZE: u32 = 2048;

/// One instance of a snapshot: `parent` is another instance's `id`, or 0 for the snapshot's root.
#[derive(Deserialize)]
pub struct Instance {
	pub id: u32,
	pub parent: u32,
	pub class: String,
	pub name: String,
	pub properties: HashMap<String, Variant>,
	/// Ref properties, as the `id` of the instance they point at.
	#[serde(default)]
	pub references: HashMap<String, u32>,
}

/// What a ViewportFrame shows: the ViewportFrame first, then the instances under it, then any other
/// roots the picture reads (the place's MaterialService).
#[derive(Deserialize)]
pub struct Snapshot {
	pub version: u32,
	/// The picture's size in pixels: the frame's logical size.
	pub width: u32,
	pub height: u32,
	/// The frame's CurrentCamera.
	pub camera: CameraSpec,
	pub instances: Vec<Instance>,
	/// The frame's fill: Studio draws the world into a target cleared to it, so the picture holds it
	/// ([`Background::Frame`]); without it the picture is transparent.
	#[serde(default)]
	pub background: Option<Fill>,
}

/// A ViewportFrame's fill: BackgroundColor3 (0..1) and BackgroundTransparency.
#[derive(Deserialize)]
pub struct Fill {
	pub color: [f32; 3],
	#[serde(default)]
	pub transparency: f32,
}

/// The snapshot's instances as a document, and the ViewportFrame's ref in it.
pub fn document(snapshot: &Snapshot) -> Result<(Document, Ref)> {
	ensure!(snapshot.version == PROTOCOL_VERSION, "unsupported ViewportFrame snapshot version {} (this worker reads {PROTOCOL_VERSION})", snapshot.version);
	ensure!(!snapshot.instances.is_empty() && snapshot.instances.len() <= 100_000, "invalid ViewportFrame instance count");
	let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
	let mut refs = HashMap::new();
	for instance in &snapshot.instances {
		ensure!(instance.id != 0, "instance id 0 is the snapshot's root");
		ensure!(!refs.contains_key(&instance.id), "duplicate instance id {}", instance.id);
		let parent = if instance.parent == 0 { dom.root_ref() } else { *refs.get(&instance.parent).with_context(|| format!("instance {} comes before its parent {}", instance.id, instance.parent))? };
		let builder = InstanceBuilder::new(instance.class.as_str())
			.with_name(&instance.name)
			.with_properties(instance.properties.iter().map(|(name, value)| (name.as_str(), value.clone())));
		refs.insert(instance.id, dom.insert(parent, builder));
	}
	for instance in &snapshot.instances {
		let target = dom.get_by_ref_mut(refs[&instance.id]).unwrap();
		for (name, id) in &instance.references {
			target.properties.insert(name.as_str().into(), Variant::Ref(refs.get(id).copied().unwrap_or(Ref::none())));
		}
	}
	let frame = refs[&snapshot.instances[0].id];
	ensure!(dom.get_by_ref(frame).unwrap().class == "ViewportFrame", "a snapshot's first instance must be its ViewportFrame");
	Ok((Document::new(dom), frame))
}

/// Draws a `{"viewport": <snapshot>, "out": <file>}` request into `out` (a PNG, straight alpha) and
/// returns the reply.
pub fn render(renderer: &Renderer, request: &Value) -> Result<Value> {
	let snapshot = Snapshot::deserialize(&request["viewport"]).context("reading the ViewportFrame snapshot")?;
	ensure!((1..=MAX_SIZE).contains(&snapshot.width) && (1..=MAX_SIZE).contains(&snapshot.height), "viewport dimensions must be 1..{MAX_SIZE}");
	let path = PathBuf::from(request["out"].as_str().context("missing output path")?);
	let (document, frame) = document(&snapshot)?;
	let frame = document.dom.get_by_ref(frame).unwrap();
	let mut scene = crate::scene::extract::extract_roots(&document, frame.children());
	scene.in_viewport_frame(ViewportLighting {
		ambient: props::color(frame, "Ambient", glam::Vec3::ONE),
		light_color: props::color(frame, "LightColor", glam::Vec3::ONE),
		light_direction: props::vec3(frame, "LightDirection", glam::Vec3::new(-1.0, -1.0, -1.0)),
		sky: Environment::from_lighting(&document.dom, frame).sky,
	});
	let background = match &snapshot.background {
		Some(fill) => Background::Frame { color: fill.color, alpha: 1.0 - fill.transparency.clamp(0.0, 1.0) },
		None => Background::Transparent,
	};
	let view = ViewRequest { width: snapshot.width, height: snapshot.height, camera: snapshot.camera, background, bloom: false, ..Default::default() };
	// the meshes and textures not on disk yet, many at a time (as `ror render` does); the render then reads the disk
	renderer.assets.warm(&crate::render::downloads(&scene), &crate::render::transcode_packs);
	let (images, stats) = renderer.render(&scene, std::slice::from_ref(&view))?;
	ensure!(stats.missing.is_empty(), "ViewportFrame assets unavailable: {}", stats.missing.join("; "));
	let image = &images[0];
	crate::output::write_png(&path, image.width, image.height, &image.rgba, true)?;
	Ok(json!({ "file": path, "warnings": scene.warnings, "background": snapshot.background.is_some() }))
}

/// What a program that links ror adds to its worker: commands of its own, and memory it gives back
/// when no request has come for a while.
pub trait Commands {
	/// The reply to `request`, or `None` when it is not one of these commands.
	fn handle(&mut self, _request: &Value) -> Option<Result<Value>> {
		None
	}
	/// Called once each time the worker goes quiet.
	fn release_idle(&mut self) {}
}

/// ror's own worker: viewports and assets only.
impl Commands for () {}

/// A worker's state between requests: the renderer starts with the first viewport.
pub struct Worker<'a> {
	assets: ror_assets::Assets,
	renderer: Option<Renderer>,
	commands: &'a mut dyn Commands,
}

impl<'a> Worker<'a> {
	pub fn new(assets: ror_assets::Assets, commands: &'a mut dyn Commands) -> Self {
		Self { assets, renderer: None, commands }
	}

	/// The reply to one request line; a failure is `{"error": ...}`.
	pub fn answer(&mut self, line: &str) -> Value {
		self.try_answer(line).unwrap_or_else(|error| json!({ "error": format!("{error:#}") }))
	}

	fn try_answer(&mut self, line: &str) -> Result<Value> {
		let request: Value = serde_json::from_str(line).context("a request is one JSON object a line")?;
		if let Some(reply) = self.commands.handle(&request) {
			return reply;
		}
		match request.get("command") {
			// an asset's bytes by content id, through ror's caches and downloads
			Some(Value::String(command)) if command == "asset" => {
				let uri = request["uri"].as_str().context("missing asset uri")?;
				let path = PathBuf::from(request["out"].as_str().context("missing output path")?);
				let fetched = self.assets.fetch(uri).with_context(|| format!("fetching {uri}"))?;
				crate::output::write_atomic(&path, &fetched.bytes)?;
				Ok(json!({ "file": path, "kind": fetched.kind() }))
			}
			Some(command) => bail!("this worker has no {command} command (docs/viewport-worker.md)"),
			None => {
				if self.renderer.is_none() {
					// small blocks: its pictures are a few hundred pixels, and the blocks stay committed while it waits
					let gpu = Gpu::with_memory_hints(wgpu::MemoryHints::MemoryUsage)?;
					self.renderer = Some(Renderer::new(Arc::new(gpu), self.assets.clone())?);
				}
				render(self.renderer.as_ref().unwrap(), &request)
			}
		}
	}

	/// Gives back what the worker keeps for the next requests: the renderer's leftovers, downloads held
	/// in memory, the host's and the allocator's free memory. The GPU device and the textures stay warm.
	pub fn release_idle(&mut self) {
		if let Some(renderer) = &self.renderer {
			renderer.release_idle();
		}
		self.commands.release_idle();
		self.assets.release_memory();
		crate::release_free_memory();
	}
}

/// After this long without a request the worker gives back what it keeps for the next ones
/// ([`Worker::release_idle`]). Idle, a worker that had drawn a storybook's viewports held 942 MB.
pub const QUIET_RELEASE: Duration = Duration::from_secs(30);

/// Answers requests from stdin on stdout until stdin closes.
pub fn serve(assets: ror_assets::Assets, commands: &mut dyn Commands) -> Result<()> {
	// implicit Vulkan layers (overlays, recorders, profilers) hook presenting, which this worker never does
	if ["VK_LOADER_LAYERS_DISABLE", "VK_LOADER_LAYERS_ENABLE", "VK_INSTANCE_LAYERS"].iter().all(|name| std::env::var_os(name).is_none()) {
		// SAFETY: before the GPU is opened or this worker's threads start; the thread pools are idle
		unsafe { std::env::set_var("VK_LOADER_LAYERS_DISABLE", "~implicit~") };
	}
	let mut worker = Worker::new(assets, commands);
	let mut output = std::io::stdout().lock();
	// requests arrive on a thread of their own, so a quiet stretch is seen; it ends when stdin closes
	let (sender, requests) = mpsc::channel();
	std::thread::spawn(move || {
		for line in std::io::stdin().lock().lines() {
			if sender.send(line).is_err() {
				break;
			}
		}
	});
	let mut released = true;
	loop {
		let line = match requests.recv_timeout(QUIET_RELEASE) {
			Ok(line) => line?,
			Err(mpsc::RecvTimeoutError::Timeout) => {
				if !released {
					released = true;
					worker.release_idle();
				}
				continue;
			}
			Err(mpsc::RecvTimeoutError::Disconnected) => break,
		};
		if line.trim().is_empty() {
			continue;
		}
		released = false;
		let answer = worker.answer(&line);
		writeln!(output, "{answer}")?;
		output.flush()?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn variant(value: Variant) -> Value {
		serde_json::to_value(value).unwrap()
	}

	/// A ViewportFrame holding one white block straight ahead of its camera, on a blue fill.
	fn snapshot(size: u32) -> Value {
		use rbx_dom_weak::types::{CFrame, Color3, Matrix3, Vector3};
		json!({
			"version": PROTOCOL_VERSION,
			"width": size,
			"height": size,
			"camera": { "mode": "lookAt", "eye": [0.0, 0.0, -10.0], "target": [0.0, 0.0, 0.0], "fov": 40.0 },
			"background": { "color": [0.0, 0.0, 1.0], "transparency": 0.0 },
			"instances": [
				{ "id": 1, "parent": 0, "class": "ViewportFrame", "name": "Frame", "properties": {
					"Ambient": variant(Variant::Color3(Color3::new(1.0, 1.0, 1.0))),
					"LightColor": variant(Variant::Color3(Color3::new(0.0, 0.0, 0.0))),
				} },
				{ "id": 2, "parent": 1, "class": "Part", "name": "Block", "properties": {
					"Size": variant(Variant::Vector3(Vector3::new(4.0, 4.0, 1.0))),
					"CFrame": variant(Variant::CFrame(CFrame::new(Vector3::new(0.0, 0.0, 0.0), Matrix3::identity()))),
					"Color": variant(Variant::Color3(Color3::new(1.0, 1.0, 1.0))),
					// SmoothPlastic: no material texture, so nothing to download
					"Material": variant(Variant::Enum(rbx_dom_weak::types::Enum::from_u32(272))),
				}, "references": { "Parent0": 9 } },
			],
		})
	}

	fn parsed(value: Value) -> Snapshot {
		Snapshot::deserialize(&value).unwrap()
	}

	#[test]
	fn a_snapshot_becomes_a_document_under_its_frame() {
		let (document, frame) = document(&parsed(snapshot(64))).unwrap();
		let frame = document.dom.get_by_ref(frame).unwrap();
		assert_eq!(frame.class, "ViewportFrame");
		let block = document.dom.get_by_ref(frame.children()[0]).unwrap();
		assert_eq!((block.class.as_str(), block.name.as_str()), ("Part", "Block"));
		// a reference to an instance the snapshot does not hold is nil
		assert_eq!(block.properties.get(&"Parent0".into()), Some(&Variant::Ref(Ref::none())));
	}

	#[test]
	fn a_snapshot_is_refused_when_its_shape_is_wrong() {
		let mut wrong_version = snapshot(64);
		wrong_version["version"] = json!(PROTOCOL_VERSION + 1);
		assert!(document(&parsed(wrong_version)).err().unwrap().to_string().contains("version"));
		let mut not_a_frame = snapshot(64);
		not_a_frame["instances"][0]["class"] = json!("Frame");
		assert!(document(&parsed(not_a_frame)).is_err());
		let mut duplicate = snapshot(64);
		duplicate["instances"][1]["id"] = json!(1);
		assert!(document(&parsed(duplicate)).err().unwrap().to_string().contains("duplicate"));
		let mut orphan = snapshot(64);
		orphan["instances"][1]["parent"] = json!(7);
		assert!(document(&parsed(orphan)).is_err());
	}

	#[test]
	fn unknown_commands_and_bad_lines_are_errors_not_exits() {
		let state = std::env::temp_dir().join(format!("ror-viewport-worker-commands-{}", std::process::id()));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let mut host = ();
		let mut worker = Worker::new(assets, &mut host);
		assert!(worker.answer("not json")["error"].is_string());
		let reply = worker.answer(r#"{"command":"catalog","av":"x"}"#);
		assert!(reply["error"].as_str().unwrap().contains("no \"catalog\" command"), "{reply}");
		let reply = worker.answer(&json!({ "viewport": snapshot(4096), "out": state.join("x.png") }).to_string());
		assert!(reply["error"].as_str().unwrap().contains("dimensions"), "{reply}");
		let _ = std::fs::remove_dir_all(&state);
	}

	/// A host's commands come first; the rest are ror's.
	#[test]
	fn a_hosts_commands_are_answered_before_ror_s() {
		struct Echo;
		impl Commands for Echo {
			fn handle(&mut self, request: &Value) -> Option<Result<Value>> {
				(request["command"] == "echo").then(|| Ok(json!({ "echo": request["text"] })))
			}
		}
		let state = std::env::temp_dir().join(format!("ror-viewport-worker-host-{}", std::process::id()));
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let mut echo = Echo;
		let mut worker = Worker::new(assets, &mut echo);
		assert_eq!(worker.answer(r#"{"command":"echo","text":"hi"}"#), json!({ "echo": "hi" }));
		assert!(worker.answer(r#"{"command":"other"}"#)["error"].is_string());
		let _ = std::fs::remove_dir_all(&state);
	}

	/// The picture is the world over the frame's fill, at the snapshot's size.
	#[test]
	fn a_viewport_request_draws_the_world_over_its_fill() {
		let state = std::env::temp_dir().join(format!("ror-viewport-worker-render-{}", std::process::id()));
		std::fs::create_dir_all(&state).unwrap();
		let assets = ror_assets::Assets::new(ror_assets::AssetConfig { offline: true, ..ror_assets::AssetConfig::new(&state) });
		let mut host = ();
		let mut worker = Worker::new(assets, &mut host);
		let out = state.join("frame.png");
		let reply = worker.answer(&json!({ "viewport": snapshot(64), "out": out }).to_string());
		assert_eq!(reply["background"], json!(true), "{reply}");
		let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&out).unwrap()));
		let mut reader = decoder.read_info().unwrap();
		let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
		let info = reader.next_frame(&mut rgba).unwrap();
		assert_eq!((info.width, info.height), (64, 64));
		let pixel = |x: usize, y: usize| &rgba[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
		assert_eq!(pixel(1, 1), [0, 0, 255, 255], "the fill at a corner");
		let middle = pixel(32, 32);
		assert!(middle[0] > 200 && middle[1] > 200 && middle[3] == 255, "the white block in the middle: {middle:?}");
		worker.release_idle();
		let _ = std::fs::remove_dir_all(&state);
	}
}
