-- ablate2: which occluders a loaded map view's rasterizing time goes to. Each class of opaque parts is
-- made non-occluding in turn (Transparency 0.02: still tested, never rasterized) and the change in
-- queryOcclusion is what that class costs to rasterize. Every change is undone.
-- run after prof.luau, with the map loaded by `ror cost <map> --cameras <one view> --check`
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local out = {}
local cam = workspace.CurrentCamera
local root = workspace:FindFirstChild("RorCapture")
local function measure()
	for _ = 1, 30 do RS.RenderStepped:Wait() end
	local r = PROF.capture(150, {})
	local renders = r.timers.Perform and r.timers.Perform.count or 1
	local t = r.timers.queryOcclusion
	return t and t.total / renders or 0, Stats.SceneDrawcallCount
end
local ok, err = pcall(function()
	__CAMERA__
	local eye = cam.CFrame.Position
	local opaque = {}
	for _, d in root:GetDescendants() do if d:IsA("BasePart") and d.Transparency == 0 then table.insert(opaque, d) end end
	local base, baseDraws = measure()
	table.insert(out, string.format("as is: queryOcclusion %.3f ms, %d draws (%d opaque parts)", base, baseDraws, #opaque))
	local function largest(p) return math.max(p.Size.X, p.Size.Y, p.Size.Z) end
	local classes = {
		{ "MeshParts", function(p) return p:IsA("MeshPart") end },
		{ "Parts (blocks, wedges, balls, cylinders)", function(p) return p:IsA("Part") or p:IsA("WedgePart") or p:IsA("CornerWedgePart") end },
		{ "unions", function(p) return p:IsA("UnionOperation") end },
		{ "small: largest side under 4 studs", function(p) return largest(p) < 4 end },
		{ "mid: 4-20 studs", function(p) local l = largest(p) return l >= 4 and l < 20 end },
		{ "large: 20 studs and more", function(p) return largest(p) >= 20 end },
		{ "near: within 100 studs", function(p) return (p.Position - eye).Magnitude < 100 end },
		{ "far: beyond 250 studs", function(p) return (p.Position - eye).Magnitude >= 250 end },
		{ "everything", function() return true end },
	}
	for _, class in classes do
		local set = {}
		for _, p in opaque do if class[2](p) then table.insert(set, p) p.Transparency = 0.02 end end
		local t, draws = measure()
		for _, p in set do p.Transparency = 0 end
		table.insert(out, string.format("  %-44s %5d parts: queryOcclusion %.3f ms (%+.3f, %3.0f%% of it), draws %d", class[1], #set, t, t - base, 100 * (base - t) / math.max(base, 1e-6), draws))
	end
	local t = measure()
	table.insert(out, string.format("restored: %.3f ms", t))
end)
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
