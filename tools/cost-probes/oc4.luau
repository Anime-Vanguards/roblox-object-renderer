P.look(Vector3.new(0, 5, 60), Vector3.new(0, 5, 0))
P.base()
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local M = Enum.Material:GetEnumItems()
local function count(build)
	local f = P.reset()
	build(f)
	for _ = 1, 40 do RS.RenderStepped:Wait() end
	return Stats.SceneDrawcallCount - P.empty.scene
end
local targets = function() P.blocks(20, 6, { Material = function(i) return M[i + 1] end }) end
local row = {}
for _, t in { 0.25, 0.3, 0.35, 0.4, 0.45, 0.5, 0.6, 0.7, 0.8, 0.9, 1, 1.2, 1.5, 2, 3, 4 } do
	table.insert(row, t .. ":" .. count(function() P.part({ Size = Vector3.new(200, 100, t), Position = Vector3.new(0, 5, 40) }) targets() end))
end
table.insert(P.out, "thickness  " .. table.concat(row, " "))
row = {}
for _, z in { 38, 39, 39.5, 40, 40.5, 41, 42, 45, 50 } do
	table.insert(row, z .. ":" .. count(function() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 5, z) }) targets() end))
end
table.insert(P.out, "wall z     " .. table.concat(row, " "))
row = {}
for _, w in { 60, 80, 100, 150, 200, 300, 500 } do
	table.insert(row, w .. ":" .. count(function() P.part({ Size = Vector3.new(w, w / 2, 2), Position = Vector3.new(0, 5, 40) }) targets() end))
end
table.insert(P.out, "wall width " .. table.concat(row, " "))
row = {}
for _, h in { 10, 20, 30, 50, 100 } do
	table.insert(row, h .. ":" .. count(function() P.part({ Size = Vector3.new(200, h, 2), Position = Vector3.new(0, 5, 40) }) targets() end))
end
table.insert(P.out, "wall height " .. table.concat(row, " "))
