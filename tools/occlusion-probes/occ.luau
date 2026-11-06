-- occ.luau: synthetic scenes for occlusion-culling cost (after prof.luau). The loaded map moves to
-- ServerStorage while a case runs; everything is built in workspace.OccProbe.
local RS = game:GetService("RunService")
local SS = game:GetService("ServerStorage")
local Stats = game:GetService("Stats")
local OCC = {}
local cam = workspace.CurrentCamera
cam.CameraType = Enum.CameraType.Scriptable
cam.FieldOfView = 70

function OCC.hideMap()
	local map = workspace:FindFirstChild("RorCapture")
	if map then map.Parent = SS end
end
function OCC.showMap()
	local map = SS:FindFirstChild("RorCapture")
	if map then map.Parent = workspace end
end

function OCC.reset()
	local old = workspace:FindFirstChild("OccProbe")
	if old then old:Destroy() end
	local f = Instance.new("Folder")
	f.Name = "OccProbe"
	f.Parent = workspace
	OCC.folder = f
	return f
end

local template = Instance.new("Part")
template.Anchored = true
template.CastShadow = false
template.Size = Vector3.new(1, 1, 1)
template.TopSurface = Enum.SurfaceType.Smooth
template.BottomSurface = Enum.SurfaceType.Smooth

-- a part (Size, CFrame, extra props) into the probe folder, built in a holder and parented at once
function OCC.parts(list, parent)
	local holder = Instance.new("Model")
	for _, spec in list do
		local p = template:Clone()
		for k, v in spec do p[k] = v end
		p.Parent = holder
	end
	holder.Parent = parent or OCC.folder
	return holder
end

-- n cubes of `size` in a grid on the plane z = z0, spread over `width` studs
function OCC.grid(n, size, z0, width, props)
	local side = math.max(1, math.ceil(math.sqrt(n)))
	local step = width / side
	local list = {}
	for i = 0, n - 1 do
		local x, y = i % side, i // side
		local spec = { Size = Vector3.one * size, CFrame = CFrame.new((x - side / 2 + 0.5) * step, (y - side / 2 + 0.5) * step + 50, z0) }
		if props then for k, v in props do spec[k] = v end end
		table.insert(list, spec)
	end
	return OCC.parts(list)
end

-- the camera at the origin's +z side looking at -z
function OCC.look(eye, at)
	cam.CFrame = CFrame.lookAt(eye or Vector3.new(0, 50, 100), at or Vector3.new(0, 50, 0))
end

-- the occlusion and render cost of what is built now, per rendered frame
function OCC.measure(frames)
	for _ = 1, 20 do RS.RenderStepped:Wait() end
	local r = PROF.capture(frames or 200, {})
	local perform = r.timers.Perform
	local renders = perform and perform.count or 1
	local function per(name) local t = r.timers[name] return t and t.total / renders or 0 end
	local function count(name) local t = r.timers[name] return t and t.count / renders or 0 end
	return {
		renders = renders,
		occ = per("queryOcclusion"),
		cull = per("CullJob"),
		cullJobs = count("CullJob"),
		perform = per("Perform"),
		scene = per("Scene"),
		queue = per("updateRenderQueue"),
		wait = per("WaitOnRenderNodeQueryFence"),
		present = per("Present"),
		draws = Stats.SceneDrawcallCount,
		tris = Stats.SceneTriangleCount,
		prims = Stats.PrimitivesCount,
	}
end

function OCC.line(label, m)
	return string.format("%-50s occ %6.3f ms  cull %6.3f (%4.1f jobs)  wait %6.3f  perform %6.3f  scene %6.3f  queue %6.3f  draws %4d  tris %7d  [%d]", label, m.occ, m.cull, m.cullJobs, m.wait, m.perform, m.scene, m.queue, m.draws, m.tris, m.renders)
end
