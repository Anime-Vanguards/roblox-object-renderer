-- m16: what a wall hides: transparent parts, decals on invisible, transparent and opaque parts
local SS = game:GetService("ServerStorage")
local map = workspace:FindFirstChild("RorCapture")
if map then map.Parent = SS end
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local ok, err = pcall(function()
	local function count(build)
		P.reset()
		for _ = 1, 10 do RS.RenderStepped:Wait() end
		local base = Stats.SceneDrawcallCount
		build()
		for _ = 1, 40 do RS.RenderStepped:Wait() end
		return Stats.SceneDrawcallCount - base
	end
	P.look(Vector3.new(0, 10, 60), Vector3.new(0, 10, 0))
	local FACE = "rbxasset://textures/face.png"
	local function wall() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 10, 30) }) end
	-- 20 things in a column along the view behind the wall, each its own material so none batch
	local M = Enum.Material:GetEnumItems()
	local function column(fn) for i = 0, 19 do fn(i, Vector3.new((i % 5) * 3 - 6, 8, -(i // 5) * 3)) end end
	local function decal(q) local d = Instance.new("Decal") d.Texture = FACE d.Face = Enum.NormalId.Back d.Parent = q end
	local cases = {
		{ "opaque blocks", function(i, p) P.part({ Size = Vector3.new(2, 2, 2), Position = p, Material = M[i + 2] }) end },
		{ "transparent blocks", function(i, p) P.part({ Size = Vector3.new(2, 2, 2), Position = p, Transparency = 0.5, Material = M[i + 2] }) end },
		{ "decals on invisible blocks", function(i, p) decal(P.part({ Size = Vector3.new(2, 2, 2), Position = p, Transparency = 1, Material = M[i + 2] })) end },
		{ "decals on transparent blocks", function(i, p) decal(P.part({ Size = Vector3.new(2, 2, 2), Position = p, Transparency = 0.5, Material = M[i + 2] })) end },
		{ "decals on opaque blocks", function(i, p) decal(P.part({ Size = Vector3.new(2, 2, 2), Position = p, Material = M[i + 2] })) end },
		{ "ForceField blocks", function(i, p) P.part({ Size = Vector3.new(2, 2, 2), Position = p, Material = Enum.Material.ForceField, Color = Color3.fromHSV(i / 20, 1, 1) }) end },
	}
	for _, c in cases do
		local open = count(function() column(c[2]) end)
		local walled = count(function() wall() column(c[2]) end)
		table.insert(P.out, string.format("20 %s: %d, behind the wall %d (the wall alone 1)", c[1], open, walled))
	end
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
return table.concat(P.out, string.char(10))
