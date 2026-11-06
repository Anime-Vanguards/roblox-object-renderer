-- ablate.luau (after prof.luau): what a loaded map view's queryOcclusion time is made of. The camera is
-- where the last --check view left it (or __CAMERA__). Every change is undone.
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local out = {}
local cam = workspace.CurrentCamera
local root = workspace:FindFirstChild("RorCapture")
local function measure(label)
	for _ = 1, 30 do RS.RenderStepped:Wait() end
	local r = PROF.capture(150, {})
	local renders = r.timers.Perform and r.timers.Perform.count or 1
	local function per(name) local t = r.timers[name] return t and t.total / renders or 0 end
	table.insert(out, string.format("%-62s queryOcclusion %6.3f ms  CullJob %6.3f  render thread %6.3f  draws %4d  tris %7d", label, per("queryOcclusion"), per("CullJob"), per("Perform"), Stats.SceneDrawcallCount, Stats.SceneTriangleCount))
	return per("queryOcclusion")
end
-- which parts lie (by their box) in the camera's view
local function inView(part)
	local cf, size = part.CFrame, part.Size
	local half = size / 2
	local any = false
	for i = 0, 7 do
		local corner = cf * Vector3.new(i % 2 == 0 and -half.X or half.X, (i // 2) % 2 == 0 and -half.Y or half.Y, (i // 4) == 0 and -half.Z or half.Z)
		local _, on = cam:WorldToViewportPoint(corner)
		if on then any = true break end
	end
	if not any then
		local _, on = cam:WorldToViewportPoint(cf.Position)
		any = on
	end
	return any
end
local ok, err = pcall(function()
	__CAMERA__
	local parts = {}
	for _, d in root:GetDescendants() do if d:IsA("BasePart") then table.insert(parts, d) end end
	local base = measure("as is (" .. #parts .. " parts)")
	-- 1. nothing occludes: every opaque part at Transparency 0.02 (still tested, never rasterized)
	local changed = {}
	for _, p in parts do if p.Transparency == 0 then table.insert(changed, p) p.Transparency = 0.02 end end
	local noOcc = measure("no occluders (" .. #changed .. " opaque parts made 0.02)")
	for _, p in changed do p.Transparency = 0 end
	-- 2. the parts outside the view removed
	local outside, inside = {}, {}
	for _, p in parts do if inView(p) then table.insert(inside, p) else table.insert(outside, p) end end
	local parents = {}
	for i, p in outside do parents[i] = p.Parent p.Parent = nil end
	local noOutside = measure("parts outside the view removed (" .. #outside .. " of " .. #parts .. ")")
	-- 3. and half of those inside (every other one) too
	local half = {}
	for i, p in inside do if i % 2 == 0 then table.insert(half, p) end end
	local halfParents = {}
	for i, p in half do halfParents[i] = p.Parent p.Parent = nil end
	local halfIn = measure("and half the parts in view removed (" .. #half .. ")")
	for i, p in half do p.Parent = halfParents[i] end
	for i, p in outside do p.Parent = parents[i] end
	measure("restored")
	table.insert(out, string.format("raster share %.0f%% (as is %.3f, no occluders %.3f); outside the view %.3f ms; half the view's parts %.3f ms", 100 * (base - noOcc) / math.max(base, 1e-6), base, noOcc, base - noOutside, noOutside - halfIn))
end)
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
