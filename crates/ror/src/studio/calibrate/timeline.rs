//! Timeline runs: an effect played in the capture Studio and in ror from one playback
//! (scene::playback). Studio plays it frame by frame (RenderStepped) from a fresh copy of the model,
//! writing each frame's index into a row of marker tiles in the corner of the view, while ror
//! captures frames as fast as it can; the frame nearest the run's moment is kept, and ror draws the
//! playback at that frame's exact time with Studio's frame times (trails take a point a frame,
//! events wait for the next frame). Trails cannot be frozen as particles can (TimeScale 0), so this
//! is how a trail's age is known.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glam::{Affine3A, Vec3};
use rbx_dom_weak::types::Variant;
use serde::{Deserialize, Serialize};

use crate::render::camera::Camera;
use crate::scene::load::Document;
use crate::scene::playback::{Emit, Playback};
use crate::scene::props;
use crate::studio::compare::CaptureSession;

/// A run's timeline: the playback both sides play (its `time` the moment wanted), played in Studio
/// for `duration` seconds.
#[derive(Clone, Debug)]
pub struct Timeline {
	pub playback: Playback,
	pub duration: f32,
}

fn timelines() -> &'static Mutex<HashMap<(String, String), Timeline>> {
	static TIMELINES: OnceLock<Mutex<HashMap<(String, String), Timeline>>> = OnceLock::new();
	TIMELINES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The timeline of an experiment's run, if it has one.
pub fn get(experiment: &str, run: &str) -> Option<Timeline> {
	timelines().lock().ok()?.get(&(experiment.to_string(), run.to_string())).cloned()
}

pub fn register(experiment: &str, run: &str, timeline: Timeline) {
	if let Ok(mut timelines) = timelines().lock() {
		timelines.insert((experiment.to_string(), run.to_string()), timeline);
	}
}

/// The playback a run stored (its `playback.json`), as Studio played it. The runner plays emits by
/// attributes one way, "delayed" ([`crate::scene::playback::ByAttributes`]); runs stored before
/// 2026-10-10 name that timing "attributes", which names another mode since, and the harness writes
/// "delayed" itself, so an "attributes" here is that older name.
pub fn stored_playback(path: &std::path::Path) -> Result<Playback> {
	let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
	let mut playback: Playback = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
	for action in &mut playback.actions {
		if matches!(&action.emit, Some(Emit::Mode(mode)) if mode == "attributes") {
			action.emit = Some(Emit::Mode("delayed".into()));
		}
	}
	Ok(playback)
}

/// The frame index marker: tiles in the view's lower left corner, one meter of Neon gray each (no
/// bloom: emission 25.75 · 0.5⁴ = 1.6, under the threshold 3.88) over a black plate; the first is
/// always lit while the timeline runs, the next [`MARKER_BITS`] hold the frame index, lowest first.
const MARKER_BITS: usize = 11;
const MARKER_TILE: f32 = 12.0;
const MARKER_PITCH: f32 = 20.0;
const MARKER_MARGIN: f32 = 24.0;
const MARKER_DEPTH: f32 = 1.0;

pub(super) struct Marker {
	/// Each tile's centre in image pixels and world CFrame, and the tile's side in studs.
	pub(super) tiles: Vec<([f32; 2], Affine3A)>,
	tile_size: f32,
	plate: (Affine3A, Vec3),
	/// The image rectangle the marker covers (x0, y0, x1, y1), blanked in both images.
	pub(super) rect: [u32; 4],
}

impl Marker {
	pub(super) fn new(camera: &Camera, width: u32, height: u32) -> Self {
		let studs_per_pixel = 2.0 * (camera.fov.to_radians() * 0.5).tan() * MARKER_DEPTH / height as f32;
		let to_world = |x: f32, y: f32, depth: f32| -> Affine3A {
			let local = Vec3::new((x - width as f32 * 0.5) * studs_per_pixel * depth / MARKER_DEPTH, (height as f32 * 0.5 - y) * studs_per_pixel * depth / MARKER_DEPTH, -depth);
			camera.world * Affine3A::from_translation(local)
		};
		let y = height as f32 - MARKER_MARGIN;
		let tiles: Vec<([f32; 2], Affine3A)> = (0..=MARKER_BITS)
			.map(|index| {
				let x = MARKER_MARGIN + index as f32 * MARKER_PITCH;
				([x, y], to_world(x, y, MARKER_DEPTH))
			})
			.collect();
		let (x0, x1) = (MARKER_MARGIN - MARKER_PITCH * 0.5, MARKER_MARGIN + MARKER_BITS as f32 * MARKER_PITCH + MARKER_PITCH * 0.5);
		let (y0, y1) = (y - MARKER_PITCH * 0.5, y + MARKER_PITCH * 0.5);
		let depth = MARKER_DEPTH + 0.002;
		let plate = (to_world((x0 + x1) * 0.5, (y0 + y1) * 0.5, depth), Vec3::new((x1 - x0) * studs_per_pixel * depth / MARKER_DEPTH, (y1 - y0) * studs_per_pixel * depth / MARKER_DEPTH, 0.001));
		let rect = [(x0 - 4.0).max(0.0) as u32, (y0 - 4.0).max(0.0) as u32, ((x1 + 4.0) as u32).min(width), ((y1 + 4.0) as u32).min(height)];
		Self { tiles, tile_size: MARKER_TILE * studs_per_pixel, plate, rect }
	}

	/// The frame index an image shows, `None` when the first tile is dark (no timeline running).
	pub(super) fn decode(&self, rgba: &[u8], width: u32) -> Option<u32> {
		let lit = |center: [f32; 2]| -> bool {
			let mut sum = 0u32;
			for dy in -2i32..=2 {
				for dx in -2i32..=2 {
					let (x, y) = (center[0] as i32 + dx, center[1] as i32 + dy);
					let index = ((y.max(0) as u32 * width + x.max(0) as u32) * 4) as usize;
					sum += rgba.get(index..index + 3).map(|pixel| pixel.iter().map(|&value| value as u32).max().unwrap_or(0)).unwrap_or(0);
				}
			}
			sum / 25 > 100
		};
		if !lit(self.tiles[0].0) {
			return None;
		}
		Some((1..=MARKER_BITS).filter(|&bit| lit(self.tiles[bit].0)).map(|bit| 1u32 << (bit - 1)).sum())
	}

}

/// The marker tiles' colour: 0.5 gray Neon (x = 1.6), as bright at any ExposureCompensation (the
/// frame index stays readable, under the bloom threshold, in dimmed runs).
fn marker_color(exposure: f32) -> String {
	if exposure == 0.0 {
		return "Color3.new(0.5, 0.5, 0.5)".into();
	}
	let gray = (0.5 * 2f32.powf(-exposure / 4.0)).min(1.0);
	format!("Color3.new({gray}, {gray}, {gray})")
}

/// Paints an image rectangle (x0, y0, x1, y1) black: the marker's, in both images before they are
/// compared.
pub fn blank(rgba: &mut [u8], width: u32, rect: [u32; 4]) {
	for y in rect[1]..rect[3] {
		for x in rect[0]..rect[2] {
			let index = ((y * width + x) * 4) as usize;
			if let Some(pixel) = rgba.get_mut(index..index + 3) {
				pixel.fill(0);
			}
		}
	}
}

fn luau_cframe(cframe: &Affine3A) -> String {
	let c = crate::scene::build::cframe(*cframe);
	let (p, o) = (c.position, c.orientation);
	format!("CFrame.new({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})", p.x, p.y, p.z, o.x.x, o.x.y, o.x.z, o.y.x, o.y.y, o.y.z, o.z.x, o.z.y, o.z.z)
}

/// A dotted path as a Luau array of names (from `workspace.RorCapture`).
fn luau_path(path: &str) -> Result<String> {
	let segments = crate::scene::load::split_path(path)?;
	Ok(format!("{{{}}}", segments.iter().map(|segment| format!("{segment:?}")).collect::<Vec<_>>().join(", ")))
}

/// A property value for Luau: the constructor of the type `existing` has, applied to the value's
/// numbers (a table `v`), so tweens lerp the numbers and build the value.
fn luau_builder(value: &serde_json::Value, existing: Option<&Variant>) -> Result<(String, String)> {
	let numbers: Vec<f64> = match value {
		serde_json::Value::Number(number) => vec![number.as_f64().unwrap_or_default()],
		serde_json::Value::Array(items) => items.iter().filter_map(|item| item.as_f64()).collect(),
		serde_json::Value::Bool(flag) => return Ok(("{}".into(), format!("function() return {flag} end"))),
		serde_json::Value::String(text) => return Ok(("{}".into(), format!("function() return {text:?} end"))),
		other => bail!("timeline: cannot set {other}"),
	};
	let table = format!("{{{}}}", numbers.iter().map(|value| value.to_string()).collect::<Vec<_>>().join(", "));
	let builder = match (numbers.len(), existing) {
		(1, Some(Variant::NumberSequence(_))) => "function(v) return NumberSequence.new(v[1]) end",
		(1, Some(Variant::NumberRange(_))) => "function(v) return NumberRange.new(v[1]) end",
		(1, _) => "function(v) return v[1] end",
		(3, Some(Variant::Color3(_) | Variant::Color3uint8(_))) => "function(v) return Color3.new(v[1], v[2], v[3]) end",
		(3, _) => "function(v) return Vector3.new(v[1], v[2], v[3]) end",
		(12, _) => "function(v) return CFrame.new(table.unpack(v)) end",
		(count, _) => bail!("timeline: a value of {count} numbers"),
	};
	Ok((table, builder.into()))
}

/// The Luau that plays `timeline` in the capture Studio from a fresh copy of the uploaded model
/// (`model_key` names the upload) and returns at once: frames run on RenderStepped, their times in
/// `_G.__ror_timeline.times`.
pub(super) fn runner(timeline: &Timeline, document: &Document, model_key: u64, marker: &Marker, exposure: f32) -> Result<String> {
	let playback = &timeline.playback;
	let mut motions = Vec::new();
	for motion in &playback.motion {
		let keys: Vec<String> = motion
			.keys
			.iter()
			.map(|key| {
				let c = &key.cframe;
				let cframe = if c.len() == 3 { format!("Vector3.new({}, {}, {})", c[0], c[1], c[2]) } else { format!("CFrame.new({})", c.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")) };
				format!("{{at = {}, value = {cframe}, style = {:?}, direction = {:?}}}", key.at, key.easing.as_deref().unwrap_or("Linear"), key.direction.as_deref().unwrap_or("Out"))
			})
			.collect();
		motions.push(format!("{{path = {}, relative = {}, keys = {{{}}}}}", luau_path(&motion.target)?, motion.relative, keys.join(", ")));
	}
	let mut actions = Vec::new();
	for action in &playback.actions {
		let mut fields = vec![format!("at = {}", action.at), format!("path = {}", luau_path(&action.target)?)];
		match &action.emit {
			Some(Emit::Count(count)) => fields.push(format!("emit = {count}")),
			// the runner's Luau plays "delayed" under the name it had when the runs were captured, which
			// keeps the script, and so every capture's key, as it was
			Some(Emit::Mode(mode)) if mode == "delayed" => fields.push("emit = \"attributes\"".into()),
			Some(Emit::Mode(mode)) => bail!("timeline: the capture runner plays emits of a count or \"delayed\", not {mode:?}"),
			None => {}
		}
		if let Some(enabled) = action.enabled {
			fields.push(format!("enabled = {enabled}"));
		}
		if let Some(scale) = action.time_scale {
			fields.push(format!("timeScale = {scale}"));
		}
		if action.clear {
			fields.push("clear = true".into());
		}
		if let Some(class) = &action.class {
			fields.push(format!("class = {class:?}"));
		}
		if !action.set.is_empty() {
			let target = crate::scene::playback::resolve(document, &action.target)?.first().copied();
			let mut sets = Vec::new();
			for (name, value) in &action.set {
				let existing = target.and_then(|target| document.dom.get_by_ref(target)).and_then(|instance| props::get(instance, name).cloned());
				let (table, builder) = luau_builder(value, existing.as_ref())?;
				sets.push(format!("{{name = {name:?}, value = ({builder})({table})}}"));
			}
			fields.push(format!("set = {{{}}}", sets.join(", ")));
		}
		actions.push(format!("{{{}}}", fields.join(", ")));
	}
	let mut tweens = Vec::new();
	for tween in &playback.tweens {
		let target = crate::scene::playback::resolve(document, &tween.target)?.first().copied();
		let existing = target.and_then(|target| document.dom.get_by_ref(target)).and_then(|instance| props::get(instance, &tween.property).cloned());
		let mut builder = String::new();
		let keys: Vec<String> = tween
			.keys
			.iter()
			.map(|key| {
				let (table, make) = luau_builder(&key.value, existing.as_ref())?;
				builder = make;
				Ok(format!("{{at = {}, value = {table}, style = {:?}, direction = {:?}}}", key.at, key.easing.as_deref().unwrap_or("Linear"), key.direction.as_deref().unwrap_or("Out")))
			})
			.collect::<Result<_>>()?;
		tweens.push(format!("{{path = {}, property = {:?}, build = {builder}, keys = {{{}}}}}", luau_path(&tween.target)?, tween.property, keys.join(", ")));
	}
	let tiles: Vec<String> = marker.tiles.iter().map(|(_, cframe)| luau_cframe(cframe)).collect();
	// beams scroll from the first frame, not from the copy's arrival 0.8 s earlier (beam-scroll; empty
	// without beams, so other runs' scripts and capture keys stay as they were)
	let has_beams = document.dom.descendants().any(|instance| instance.class == "Beam");
	let beam_list = if has_beams { "\nlocal beams = {}\nfor _, instance in root:GetDescendants() do\n\tif instance:IsA(\"Beam\") then table.insert(beams, instance) end\nend" } else { "" };
	let beam_reset = if has_beams { "\n\t\tfor _, beam in beams do beam:SetTextureOffset(0) end" } else { "" };
	let script = format!(
		r#"local RS = game:GetService("RunService")
local TweenService = game:GetService("TweenService")
local KEY = "{model_key:016x}"
local DURATION = {duration}
local current = workspace:FindFirstChild("RorCapture")
if _G.__ror_pristine_key ~= KEY then
	if _G.__ror_pristine then _G.__ror_pristine:Destroy() end
	_G.__ror_pristine = current:Clone()
	_G.__ror_pristine_key = KEY
end
if _G.__ror_timeline and _G.__ror_timeline.connection then _G.__ror_timeline.connection:Disconnect() end
current:Destroy()
local root = _G.__ror_pristine:Clone()
root.Parent = workspace
-- emitters enabled in the file start with the timeline's first frame (as in a model cloned into
-- the world then: their Rate counts from that frame)
local fileEnabled = {{}}
for _, instance in root:GetDescendants() do
	if instance:IsA("ParticleEmitter") and instance.Enabled then
		instance.Enabled = false
		table.insert(fileEnabled, instance)
	end
end{beam_list}
local function resolve(path)
	local found = {{workspace}}
	local names = {{"RorCapture"}}
	for _, name in path do table.insert(names, name) end
	for _, name in names do
		local nextFound = {{}}
		for _, instance in found do
			for _, child in instance:GetChildren() do
				if child.Name == name then table.insert(nextFound, child) end
			end
		end
		found = nextFound
	end
	return found
end
local function everything(instance)
	local list = instance:GetDescendants()
	table.insert(list, instance)
	return list
end
local switchable = {{ParticleEmitter = true, Trail = true, Beam = true, PointLight = true, SpotLight = true, SurfaceLight = true, Highlight = true}}
-- markers: a guard tile, then the frame index's bits
local old = workspace:FindFirstChild("RorTimelineMarker")
if old then old:Destroy() end
local marker = Instance.new("Folder")
marker.Name = "RorTimelineMarker"
local plate = Instance.new("Part")
plate.Anchored = true
plate.CanCollide = false
plate.CastShadow = false
plate.Material = Enum.Material.SmoothPlastic
plate.Color = Color3.new(0, 0, 0)
plate.Size = Vector3.new({plate_x}, {plate_y}, 0.001)
plate.CFrame = {plate_cframe}
plate.Parent = marker
local tiles = {{}}
for _, cframe in {{{tiles}}} do
	local tile = Instance.new("Part")
	tile.Anchored = true
	tile.CanCollide = false
	tile.CastShadow = false
	tile.Material = Enum.Material.Neon
	tile.Color = {tile_color}
	tile.Size = Vector3.new({tile}, {tile}, 0.001)
	tile.CFrame = cframe
	tile.Transparency = 1
	tile.Parent = marker
	table.insert(tiles, tile)
end
marker.Parent = workspace
local motions = {{{motions}}}
local actions = {{{actions}}}
local tweens = {{{tweens}}}
local function pivot(target)
	if target:IsA("BasePart") then return target.CFrame end
	if target:IsA("Model") then return target:GetPivot() end
	if target:IsA("Attachment") then return target.WorldCFrame end
	return CFrame.new()
end
local function place(target, cframe)
	if target:IsA("BasePart") then target.CFrame = cframe
	elseif target:IsA("Model") then target:PivotTo(cframe)
	elseif target:IsA("Attachment") then target.WorldCFrame = cframe end
end
for _, motion in motions do
	motion.targets = resolve(motion.path)
	motion.rests = {{}}
	for index, target in motion.targets do motion.rests[index] = pivot(target) end
end
for _, action in actions do action.targets = resolve(action.path) end
for _, tween in tweens do tween.targets = resolve(tween.path) end
local function key(motion, rest, value)
	if typeof(value) == "Vector3" then
		if motion.relative then return rest * CFrame.new(value) end
		return rest.Rotation + value
	end
	if motion.relative then return rest * value end
	return value
end
local function moveAll(t)
	for _, motion in motions do
		local keys = motion.keys
		for index, target in motion.targets do
			local rest = motion.rests[index]
			local cframe
			if t <= keys[1].at then
				cframe = key(motion, rest, keys[1].value)
			elseif t >= keys[#keys].at then
				cframe = key(motion, rest, keys[#keys].value)
			else
				for k = 2, #keys do
					if t <= keys[k].at then
						local span = keys[k].at - keys[k - 1].at
						local alpha = if span > 1e-6 then TweenService:GetValue((t - keys[k - 1].at) / span, Enum.EasingStyle[keys[k].style], Enum.EasingDirection[keys[k].direction]) else 1
						cframe = key(motion, rest, keys[k - 1].value):Lerp(key(motion, rest, keys[k].value), alpha)
						break
					end
				end
			end
			place(target, cframe)
		end
	end
end
local scheduled = {{}}
local function fire(action, t)
	for _, target in action.targets do
		for _, instance in everything(target) do
			if action.class and instance.ClassName ~= action.class then continue end
			if action.enabled ~= nil and switchable[instance.ClassName] then instance.Enabled = action.enabled end
			if instance:IsA("ParticleEmitter") then
				if action.timeScale then instance.TimeScale = action.timeScale end
				if action.emit == "attributes" then
					local delay = math.max(0, instance:GetAttribute("EmitDelay") or 0)
					local count = instance:GetAttribute("EmitCount") or 5
					local duration = instance:GetAttribute("EmitDuration") or 0
					table.insert(scheduled, {{at = action.at + delay, run = function()
						instance:Emit(count)
						if duration > 0 then instance.Enabled = true end
					end}})
					if duration > 0 then
						table.insert(scheduled, {{at = action.at + delay + duration, run = function() instance.Enabled = false end}})
					end
				elseif action.emit then
					instance:Emit(action.emit)
				end
				if action.clear then instance:Clear() end
			elseif instance:IsA("Trail") and action.clear then
				instance:Clear()
			end
		end
		if action.set then
			for _, set in action.set do target[set.name] = set.value end
		end
	end
end
local function tweenAll(t)
	for _, tween in tweens do
		local keys = tween.keys
		local value = keys[#keys].value
		if t <= keys[1].at then
			value = keys[1].value
		else
			for k = 2, #keys do
				if t <= keys[k].at then
					local span = keys[k].at - keys[k - 1].at
					local alpha = if span > 1e-6 then TweenService:GetValue((t - keys[k - 1].at) / span, Enum.EasingStyle[keys[k].style], Enum.EasingDirection[keys[k].direction]) else 1
					value = {{}}
					for i = 1, #keys[k].value do value[i] = keys[k - 1].value[i] + (keys[k].value[i] - keys[k - 1].value[i]) * alpha end
					break
				end
			end
		end
		for _, target in tween.targets do target[tween.property] = tween.build(value) end
	end
end
moveAll(0)
tweenAll(0)
task.wait(0.8)
for _, instance in root:GetDescendants() do
	if instance:IsA("ParticleEmitter") or instance:IsA("Trail") then instance:Clear() end
end
local timeline = {{times = {{}}, done = false}}
_G.__ror_timeline = timeline
local fired = {{}}
local frame = 0
local t0
timeline.connection = RS.RenderStepped:Connect(function()
	local now = os.clock()
	if not t0 then t0 = now end
	local t = now - t0
	timeline.times[frame + 1] = t
	if frame == 0 then
		for _, instance in fileEnabled do instance.Enabled = true end{beam_reset}
	end
	moveAll(t)
	for index, action in actions do
		if not fired[index] and action.at <= t then
			fired[index] = true
			fire(action, t)
		end
	end
	for _, item in scheduled do
		if not item.done and item.at <= t then
			item.done = true
			item.run()
		end
	end
	tweenAll(t)
	tiles[1].Transparency = 0
	for bit = 1, {bits} do
		tiles[bit + 1].Transparency = if bit32.band(frame, bit32.lshift(1, bit - 1)) ~= 0 then 0 else 1
	end
	frame += 1
	if t >= DURATION then
		timeline.connection:Disconnect()
		timeline.connection = nil
		marker:Destroy()
		timeline.done = true
	end
end)
return "started""#,
		duration = timeline.duration,
		beam_list = beam_list,
		beam_reset = beam_reset,
		plate_x = marker.plate.1.x,
		plate_y = marker.plate.1.y,
		plate_cframe = luau_cframe(&marker.plate.0),
		tiles = tiles.join(", "),
		tile = marker.tile_size,
		tile_color = marker_color(exposure),
		motions = motions.join(", "),
		actions = actions.join(", "),
		tweens = tweens.join(", "),
		bits = MARKER_BITS,
	);
	let legacy = document.dom.descendants().any(|instance| matches!(instance.class.as_str(), "Fire" | "Smoke" | "Sparkles" | "Explosion"));
	if legacy { legacy_runner(script) } else { Ok(script) }
}

/// The runner for a model with Fire, Smoke, Sparkles or Explosions (the others' runner, and so their
/// capture keys, stay as they were): those enabled in the file start with the first frame, as
/// emitters do; `enabled` switches them and `timeScale` sets theirs; an Explosion goes off at the
/// first frame (it goes off when it enters the world, so the fresh copy is placed without them), or,
/// when an action emits at or above it, at each such action (a copy of it each time).
fn legacy_runner(script: String) -> Result<String> {
	let replacements = [
		(
			"local root = _G.__ror_pristine:Clone()\nroot.Parent = workspace\n",
			"local root = _G.__ror_pristine:Clone()
-- Explosions go off when they enter the world: placed at the first frame or when emitted
local explosions = {}
for _, instance in root:GetDescendants() do
	if instance:IsA(\"Explosion\") then
		local names = {}
		local node = instance
		while node and node ~= root do
			table.insert(names, 1, node.Name)
			node = node.Parent
		end
		table.insert(explosions, {instance = instance, parent = instance.Parent, names = names})
	end
end
for _, record in explosions do record.instance.Parent = nil end
root.Parent = workspace
",
		),
		("if instance:IsA(\"ParticleEmitter\") and instance.Enabled then", "if (instance:IsA(\"ParticleEmitter\") or instance:IsA(\"Fire\") or instance:IsA(\"Smoke\") or instance:IsA(\"Sparkles\")) and instance.Enabled then"),
		("Highlight = true}", "Highlight = true, Fire = true, Smoke = true, Sparkles = true}"),
		(
			"\t\t\tif instance:IsA(\"ParticleEmitter\") then\n\t\t\t\tif action.timeScale then instance.TimeScale = action.timeScale end",
			"\t\t\tif action.timeScale and (instance:IsA(\"Fire\") or instance:IsA(\"Smoke\") or instance:IsA(\"Sparkles\") or instance:IsA(\"Explosion\")) then instance.TimeScale = action.timeScale end
			if instance:IsA(\"ParticleEmitter\") then
				if action.timeScale then instance.TimeScale = action.timeScale end",
		),
		(
			"\t\tif action.set then\n\t\t\tfor _, set in action.set do target[set.name] = set.value end\n\t\tend\n\tend\nend",
			"\t\tif action.set then\n\t\t\tfor _, set in action.set do target[set.name] = set.value end\n\t\tend\n\tend
	if action.emit then
		for _, record in explosions do
			if covers(action.path, record.names) then
				local copy = record.instance:Clone()
				copy.Parent = record.parent
			end
		end
	end
end",
		),
		(
			"local scheduled = {}\n",
			"local scheduled = {}
-- whether an action's path is at or above an Explosion's
local function covers(path, names)
	if #path > #names then return false end
	for index, name in path do
		if names[index] ~= name then return false end
	end
	return true
end
for _, record in explosions do
	for _, action in actions do
		if action.emit and covers(action.path, record.names) then record.emitted = true end
	end
end
",
		),
		(
			"\t\tfor _, instance in fileEnabled do instance.Enabled = true end\n",
			"\t\tfor _, instance in fileEnabled do instance.Enabled = true end
		for _, record in explosions do
			if not record.emitted then record.instance.Parent = record.parent end
		end
",
		),
	];
	let mut script = script;
	for (anchor, replacement) in replacements {
		if !script.contains(anchor) {
			bail!("timeline: the runner has no {anchor:?} (legacy effects)");
		}
		script = script.replacen(anchor, replacement, 1);
	}
	Ok(script)
}

/// What a timeline capture recorded beside its image: the frame's time and every frame's time up to
/// it (seconds after the first).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Captured {
	pub time: f32,
	pub frames: Vec<f32>,
}

/// The rectangle a run's frame marker covers in its images (blank it in ror's too).
pub fn marker_rect(camera: &Camera, width: u32, height: u32) -> [u32; 4] {
	Marker::new(camera, width, height).rect
}

/// How long Studio has after the warm-up playback before the captured one.
const WARM_SETTLE_MS: u64 = 1000;

/// Plays `timeline` in the capture Studio and keeps the frame nearest its moment (or reads the
/// capture cache): the frame's pixels (the marker blanked) and its times.
#[cfg(windows)]
pub fn capture(session: &mut CaptureSession, camera: &Camera, timeline: &Timeline, document: &Document, model_key: u64, exposure: f32) -> Result<(Vec<u8>, Captured)> {
	let (width, height) = (session.width, session.height);
	let marker = Marker::new(camera, width, height);
	let script = runner(timeline, document, model_key, &marker, exposure)?;
	session.key_setup(&script);
	let cache = session.cache_path(camera);
	let sidecar = cache.with_extension("json");
	if session.reuse
		&& let (Ok((w, h, rgba)), Ok(text)) = (crate::output::decode_png(&cache), std::fs::read_to_string(&sidecar))
		&& (w, h) == (width, height)
	{
		session.reused += 1;
		return Ok((rgba, serde_json::from_str(&text).context("timeline capture sidecar")?));
	}
	let Some(studio) = session.studio.as_deref_mut() else {
		bail!("offline: no cached timeline capture {}", cache.display());
	};
	session_camera(studio, camera)?;
	let window = studio.window()?;
	let scale = window.dpi as f32 / 96.0;
	let (logical_width, logical_height) = studio.viewport()?;
	let viewport = crate::studio::capture::find_viewport(window.hwnd, (logical_width * scale).round() as i32, (logical_height * scale).round() as i32).context("the capture Studio's viewport window")?;
	// a texture's first frames in Studio show a low-resolution copy (particle-streaming: a fresh
	// texture's particles 2.7 times less sharp on its first run, 1.65 times after
	// ContentProvider:PreloadAsync, as sharp after a warm-up; particle-outer: an effect's thin crescents
	// all but gone): the timeline plays once uncaptured, then again for the capture
	// (`ROR_TIMELINE_COLD=1` skips the warm-up)
	if std::env::var_os("ROR_TIMELINE_COLD").is_none() {
		let result = studio.luau(&script).context("starting the timeline (warm-up)")?;
		if !result.contains("started") {
			bail!("the timeline did not start: {result}");
		}
		let warm = Instant::now();
		while warm.elapsed() < Duration::from_secs_f32(timeline.duration + 3.0) {
			std::thread::sleep(Duration::from_millis(200));
			if studio.luau("return if _G.__ror_timeline and _G.__ror_timeline.done then 'done' else 'running'")?.contains("done") {
				break;
			}
		}
		std::thread::sleep(Duration::from_millis(WARM_SETTLE_MS));
	}
	let result = studio.luau(&script).context("starting the timeline")?;
	if !result.contains("started") {
		bail!("the timeline did not start: {result}");
	}
	// frames as fast as they come, kept near the moment wanted: its frame index at the frame rate
	// seen so far (a nominal 59 a second until then; the capture Studio's RenderStepped has run at
	// up to 240 for a while)
	let start = Instant::now();
	let mut kept: Vec<(u32, Vec<u8>)> = Vec::new();
	let mut seen = 0;
	let mut first: Option<(u32, Instant)> = None;
	let mut rate = 59.0f32;
	while start.elapsed() < Duration::from_secs_f32(timeline.duration + 1.5) {
		let (w, h, rgba) = crate::studio::capture::capture(viewport.hwnd)?;
		if (w, h) != (width, height) {
			bail!("captured {w}x{h}, expected {width}x{height}");
		}
		let Some(index) = marker.decode(&rgba, width) else {
			if seen > 0 && start.elapsed() > Duration::from_secs_f32(timeline.duration * 0.5 + 0.5) {
				break;
			}
			continue;
		};
		seen += 1;
		match first {
			None => first = Some((index, Instant::now())),
			Some((first_index, at)) if index > first_index + 10 => rate = (index - first_index) as f32 / at.elapsed().as_secs_f32().max(1e-3),
			_ => {}
		}
		let wanted = (timeline.playback.time * rate).round() as i64;
		let reach = (24.0 * (rate / 59.0).max(1.0)).round() as i64;
		if (index as i64 - wanted).abs() <= reach && !kept.iter().any(|(kept, _)| *kept == index) {
			kept.push((index, rgba));
			// at most 48 frames in memory: the nearest to the moment
			if kept.len() > 48
				&& let Some(farthest) = (0..kept.len()).max_by_key(|&slot| (kept[slot].0 as i64 - wanted).abs())
			{
				kept.swap_remove(farthest);
			}
		}
	}
	let times_text = studio.luau("local timeline = _G.__ror_timeline local parts = {} for index, time in timeline.times do parts[index] = string.format('%.6f', time) end return (timeline.done and 'done ' or 'running ') .. table.concat(parts, ',')")?;
	let times: Vec<f32> = times_text.trim().split_once(' ').map(|(_, list)| list).unwrap_or_default().split(',').filter_map(|value| value.trim().parse().ok()).collect();
	let best = kept
		.into_iter()
		.filter(|(index, _)| (*index as usize) < times.len())
		.min_by(|a, b| (times[a.0 as usize] - timeline.playback.time).abs().total_cmp(&(times[b.0 as usize] - timeline.playback.time).abs()));
	let Some((index, mut rgba)) = best else {
		bail!("no frame of the timeline near {} s was captured ({seen} marked frames, {} frame times)", timeline.playback.time, times.len());
	};
	let captured = Captured { time: times[index as usize], frames: times[..=index as usize].to_vec() };
	blank(&mut rgba, width, marker.rect);
	crate::output::write_png(&cache, width, height, &rgba, true)?;
	crate::output::write_atomic(&sidecar, serde_json::to_string(&captured)?.as_bytes())?;
	Ok((rgba, captured))
}

/// Points Studio's camera (as `CaptureSession::set_camera`).
pub(super) fn session_camera(studio: &mut crate::studio::CaptureStudio, camera: &Camera) -> Result<()> {
	let c = camera.cframe_components();
	studio.luau(&format!(
		"{}
local camera = workspace.CurrentCamera
camera.CameraType = Enum.CameraType.Scriptable
camera.FieldOfView = {}
camera.CFrame = CFrame.new({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})
return 0",
		crate::studio::compare::HIDE_OVERLAYS,
		camera.fov,
		c[0],
		c[1],
		c[2],
		c[3],
		c[4],
		c[5],
		c[6],
		c[7],
		c[8],
		c[9],
		c[10],
		c[11]
	))?;
	Ok(())
}

/// The scene ror draws for a timeline run: the experiment's model at the captured frame's time,
/// with Studio's frame times.
pub fn scene(bytes: &[u8], timeline: &Timeline, captured: &Captured) -> Result<crate::scene::Scene> {
	let mut document = crate::scene::load::read_bytes(bytes)?;
	let mut playback = timeline.playback.clone();
	playback.time = captured.time;
	playback.frames = Some(captured.frames.clone());
	let resolved = playback.resolve(&document)?;
	resolved.apply_state(&mut document)?;
	let mut scene = crate::scene::extract::extract(&document);
	resolved.apply_history(&document, &mut scene);
	Ok(scene)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::scene::playback::Action;

	fn emitting(mode: &str) -> Timeline {
		let action = Action { target: "Effect".into(), emit: Some(Emit::Mode(mode.into())), ..Default::default() };
		Timeline { playback: Playback { time: 0.3, actions: vec![action], ..Default::default() }, duration: 0.5 }
	}

	/// The runner plays the calibration's timing, "delayed", under the name the captures were keyed
	/// with, so its script (each capture's key) stays as it was; it has no Luau for the game helpers'.
	#[test]
	fn the_runner_plays_emits_as_the_runs_were_captured() {
		use crate::scene::build::{ModelBuilder, PartSpec};
		let mut model = ModelBuilder::new("Effect");
		let part = model.part(&PartSpec::default());
		model.child(part, rbx_dom_weak::InstanceBuilder::new("ParticleEmitter"));
		let document = model.into_document();
		let camera = crate::render::camera::CameraSpec::default().place(None, 64, 64);
		let marker = Marker::new(&camera, 64, 64);
		let script = runner(&emitting("delayed"), &document, 1, &marker, 0.0).unwrap();
		assert!(script.contains("emit = \"attributes\"") && !script.contains("delayed"));
		for mode in ["attributes", "immediate", "cutscene"] {
			assert!(runner(&emitting(mode), &document, 1, &marker, 0.0).is_err(), "{mode}");
		}
	}

	/// A run stored before the game helpers' timings had names reads as it was played.
	#[test]
	fn stored_runs_play_as_they_were_captured() {
		let path = std::env::temp_dir().join(format!("ror-stored-playback-{}.json", std::process::id()));
		let mut stored = emitting("attributes").playback;
		stored.actions.push(Action { target: "Effect".into(), emit: Some(Emit::Count(4)), ..Default::default() });
		std::fs::write(&path, serde_json::to_string(&stored).unwrap()).unwrap();
		let read = stored_playback(&path).unwrap();
		std::fs::remove_file(&path).ok();
		assert_eq!(read.actions[0].emit, Some(Emit::Mode("delayed".into())));
		assert_eq!(read.actions[1].emit, Some(Emit::Count(4)));
	}
}
