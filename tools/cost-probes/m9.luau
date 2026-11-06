-- m9: a row of copies partly hidden: occlusion per instance
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
	P.look(Vector3.new(0, 5, 60), Vector3.new(0, 5, 0))
	-- 20 identical wood blocks in a row at z = 0, x = -38..38 (every 4 studs); walls are plastic
	local function row() for i = 0, 19 do P.part({ Size = Vector3.new(2, 2, 2), Position = Vector3.new(i * 4 - 38, 5, 0), Material = Enum.Material.Wood }) end end
	local function wall(pieces) for _, x in pieces do P.part({ Size = Vector3.new(3, 20, 1), Position = Vector3.new(x * 0.6, 5, 30) }) end end
	table.insert(P.out, "row alone: " .. count(row))
	-- posts in front hiding every other block (each post hides one block)
	local every = {} for i = 0, 19, 2 do table.insert(every, i * 4 - 38) end
	table.insert(P.out, "row + posts hiding every other block: " .. count(function() row() wall(every) end))
	local half = {} for i = 0, 9 do table.insert(half, i * 4 - 38) end
	table.insert(P.out, "row + posts hiding the left half: " .. count(function() row() wall(half) end))
	local three = {} for _, i in { 3, 8, 14 } do table.insert(three, i * 4 - 38) end
	table.insert(P.out, "row + 3 posts (3 gaps in the run): " .. count(function() row() wall(three) end))
	-- the same row, every other block out of the frustum instead (behind the camera)
	table.insert(P.out, "row, every other block behind the camera: " .. count(function() for i = 0, 19 do P.part({ Size = Vector3.new(2, 2, 2), Position = Vector3.new(i * 4 - 38, 5, i % 2 == 0 and 0 or 120), Material = Enum.Material.Wood }) end end))
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
