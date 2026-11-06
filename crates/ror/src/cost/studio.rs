//! `ror cost --check`: the same views measured in the capture Studio by Roblox's own counters
//! (`Stats.SceneDrawcallCount` and `SceneAnalysisService:GetTriangleCompositionAsync`, which work in
//! Studio's edit mode as in play: measured both ways, notes/cost.md "Measuring"), beside the model's.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::Count;
use crate::render::camera::Camera;
use crate::studio::compare::CaptureSession;

/// What Studio counted from one camera.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measured {
	/// `Stats.SceneDrawcallCount` / `SceneTriangleCount`: every pass, shadows among them.
	pub scene: Count,
	pub shadows: Count,
	/// Per pass as SceneAnalysisService names them.
	pub passes: BTreeMap<String, Count>,
	/// The engine's own timings of the view (`--profile`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub profile: Option<Profile>,
}

/// CPU time of the render job's scopes from one camera, read from the MicroProfiler through Roblox's
/// LibMP (`require("@rbx/LibMP")` in Studio MCP's execute_luau): each scope's time summed over every
/// thread it ran on, per rendered frame (notes/occlusion.md "Measuring").
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
	/// Frames the render job drew in the window (`Perform`), the divisor of every scope's time.
	pub renders: u32,
	/// Milliseconds per rendered frame by scope name (`queryOcclusion`, `CullJob`, `Perform`, ...).
	pub scopes: BTreeMap<String, f64>,
	/// How many times per rendered frame each scope ran (`CullJob`: the culling jobs split across
	/// worker threads).
	pub calls: BTreeMap<String, f64>,
}

impl Measured {
	/// Every pass but Shadows (as the model's `total`).
	pub fn total(&self) -> Count {
		let mut total = Count::default();
		for (pass, count) in &self.passes {
			if pass != "Shadows" {
				total.add(count.draws, count.triangles);
			}
		}
		total
	}
}

/// Frames to let Studio build its clusters after the camera moves (the first frames after a change
/// count parts not yet in their batches: probes measured 20 frames after building).
const SETTLE_FRAMES: u32 = 30;

const MEASURE: &str = r#"local RunService = game:GetService("RunService")
local Stats = game:GetService("Stats")
local drawn = 0
for _ = 1, __FRAMES__ do
	RunService.RenderStepped:Wait()
	drawn = math.max(drawn, Stats.RenderCPUFrameTime)
end
-- a Studio whose window is covered stops drawing: its counters stay as they were
if drawn <= 0 then error("the capture Studio is not drawing (its window is covered or minimized): its counts would be stale") end
local passes = {}
local ok, composition = pcall(function() return game:GetService("SceneAnalysisService"):GetTriangleCompositionAsync() end)
if ok then
	for _, child in composition.Children or {} do
		passes[child.Name] = { draws = child.Sizes.Drawcalls, triangles = child.Sizes.Triangles }
	end
end
return game:GetService("HttpService"):JSONEncode({
	scene = { draws = Stats.SceneDrawcallCount, triangles = Stats.SceneTriangleCount },
	shadows = { draws = Stats.ShadowsDrawcallCount, triangles = Stats.ShadowsTriangleCount },
	passes = passes,
})"#;

/// Profiler frames captured per view: Studio draws about one frame in five of its scheduler's while
/// its window is in the background, so 150 hold some 30 rendered frames.
const PROFILE_FRAMES: u32 = 150;

/// The render scopes a profile keeps (notes/occlusion.md has what each measures).
const PROFILE: &str = r#"local ok, LibMP = pcall(function() return require("@rbx/LibMP") end)
if not ok then return "null" end
local RunService = game:GetService("RunService")
local wanted = {}
for _, name in { __SCOPES__ } do wanted[name] = true end
local C = LibMP.Control
C:EnableProfiler(true)
C:EnableCapture(true)
C:SetFrameLimit(math.min(256, __FRAMES__ + 16))
for _ = 1, __FRAMES__ + 12 do RunService.RenderStepped:Wait() end
C:CaptureToSlotSync(1)
local session = LibMP.Session.OpenFromSlot(1)
local tick = session:FetchGlobalDesc().TickToMsCpu
local last = session:GetFrameIdMax() - 1
local first = math.max(session:GetFrameIdMin() + 1, last - __FRAMES__ + 1)
local names = {}
local totals, calls = {}, {}
local iterator = session:CreateLogIterator()
iterator:Configure({ SkipGpuThreads = true, SkipEvents = true })
local state = iterator:GetState()
iterator:RewindTo(first, last)
while iterator:Step() do
	if state:IsExit() and not state:ThreadStackIsUnderflowed() and not state:ThreadStackWasOverflowed() then
		local id = state:TimerId()
		local name = names[id]
		if name == nil then
			local desc = session:FetchTimerDesc(id)
			name = desc and wanted[desc.TimerName] and desc.TimerName or false
			names[id] = name
		end
		if name then
			local element = iterator:GetCurrentThreadStackElement(state:ThreadStackDepth())
			if element then
				totals[name] = (totals[name] or 0) + (state:Timestamp() - element:EnterTimestamp()) * tick
				calls[name] = (calls[name] or 0) + 1
			end
		end
	end
end
iterator:Dispose()
session:Dispose()
local renders = calls.Perform or 0
local scopes, perCall = {}, {}
for name, total in totals do
	scopes[name] = total / math.max(renders, 1)
	perCall[name] = calls[name] / math.max(renders, 1)
end
return game:GetService("HttpService"):JSONEncode({ renders = renders, scopes = scopes, calls = perCall })"#;

/// Scopes kept by a profile: the render thread's frame, the culling jobs on worker threads, and the
/// render thread's wait for them.
pub const PROFILE_SCOPES: [&str; 26] = [
	"Perform",
	"Prepare",
	"Scene",
	"UpdateView",
	"RenderView",
	"updateRenderQueue",
	"sortObjects",
	"updateParticles",
	"CullJob",
	"VisibleQuery",
	"queryOcclusion",
	"queryFrustumOrdered",
	"WaitOnRenderNodeQueryFence",
	"Present",
	"waitOnGpu",
	"SceneUpdater::UpdatePrepare",
	"SceneUpdater::computeLightingPerform",
	"ShadowMapSystem::updatePerform",
	"DefaultPass",
	"Id_Opaque",
	"Id_Transparent",
	"DepthPrePass",
	"Pass3dAdorn",
	"updateEffectBoundings",
	"SceneUpdater::updateDynamicParts",
	"fillGuiVertices",
];

/// Seconds to wait for the scene's emitters to fill (ror counts their steady state; Studio's start
/// emitting when the model is loaded): their longest Lifetime, at most 10.
const LIFETIMES: &str = r#"local longest = 0
for _, emitter in workspace.RorCapture:GetDescendants() do
	if emitter:IsA("ParticleEmitter") and emitter.Enabled and emitter.Rate > 0 then longest = math.max(longest, emitter.Lifetime.Max) end
end
return math.min(longest, 10)"#;

/// Measures each camera in a capture session that holds the scene (its model, materials and
/// lighting already set), once its emitters have run their longest lifetime.
pub fn measure(session: &mut CaptureSession, cameras: &[(String, Camera)], profile: bool) -> Result<Vec<(String, Measured)>> {
	if let Some(studio) = session.studio.as_deref_mut() {
		let seconds: f64 = studio.luau(LIFETIMES)?.trim().parse().unwrap_or(0.0);
		std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
	}
	let mut measured = Vec::new();
	for (name, camera) in cameras {
		session.set_camera(camera)?;
		let studio = session.studio.as_deref_mut().context("--check needs the capture Studio (ror studio open)")?;
		let text = studio.luau(&MEASURE.replace("__FRAMES__", &SETTLE_FRAMES.to_string())).with_context(|| format!("measuring view {name}"))?;
		let mut value: Measured = serde_json::from_str(text.trim()).with_context(|| format!("view {name}: {text}"))?;
		if profile {
			let scopes: Vec<String> = PROFILE_SCOPES.iter().map(|scope| format!("{scope:?}")).collect();
			let code = PROFILE.replace("__FRAMES__", &PROFILE_FRAMES.to_string()).replace("__SCOPES__", &scopes.join(", "));
			let text = studio.luau(&code).with_context(|| format!("profiling view {name}"))?;
			value.profile = serde_json::from_str(text.trim()).with_context(|| format!("view {name} profile: {text}"))?;
		}
		measured.push((name.clone(), value));
	}
	Ok(measured)
}

/// Loads a model into the capture place's `Workspace.RorCapture` for a cost check, unless the same
/// bytes are there already, and waits (in short calls: a whole map's assets take minutes, longer
/// than one MCP call may) until Studio has fetched what it draws.
pub fn load(studio: &mut crate::studio::CaptureStudio, model: &[u8]) -> Result<()> {
	let key = format!("{:016x}", super::meshes::fnv(model));
	let loaded = studio.luau(&format!("local folder = workspace:FindFirstChild(\"RorCapture\") return tostring(folder and folder:GetAttribute(\"RorCostModel\") == \"{key}\")"))?;
	if loaded.trim() != "true" {
		let id = studio.id.clone();
		studio.mcp.upload(&id, "__ror_model", model)?;
		studio
			.luau(&format!(
				"{}
local old = workspace:FindFirstChild(\"RorCapture\") if old then old:Destroy() end
local folder = Instance.new(\"Folder\")
folder.Name = \"RorCapture\"
for _, instance in game:GetService(\"SerializationService\"):DeserializeInstancesAsync(_G.__ror_model) do instance.Parent = folder end
_G.__ror_model = nil
folder:SetAttribute(\"RorCostModel\", \"{key}\")
folder.Parent = workspace
return #folder:GetDescendants()",
				crate::studio::compare::HIDE_OVERLAYS
			))
			.context("loading the model into the capture Studio")?;
	}
	let start = std::time::Instant::now();
	loop {
		let queue: u64 = studio.luau("task.wait(2) return game:GetService(\"ContentProvider\").RequestQueueSize")?.trim().parse().unwrap_or(0);
		if queue == 0 {
			break;
		}
		if start.elapsed().as_secs() > 900 {
			eprintln!("ror: the capture Studio still fetches {queue} assets after 15 minutes; measuring anyway");
			break;
		}
	}
	// meshes and textures reach the image a few frames after they arrive
	std::thread::sleep(std::time::Duration::from_secs(3));
	Ok(())
}
