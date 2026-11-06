P.look(Vector3.new(0, 5, 60), Vector3.new(0, 5, 0))
P.base()
local M = Enum.Material:GetEnumItems()
local function targets() P.blocks(20, 6, { Material = function(i) return M[i + 2] end }) end
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local function series(label, build)
	local f = P.reset()
	build(f)
	local row = {}
	for i = 1, 12 do
		for _ = 1, 15 do RS.RenderStepped:Wait() end
		table.insert(row, Stats.SceneDrawcallCount - P.empty.scene)
	end
	table.insert(P.out, string.format("%-34s %s", label, table.concat(row, " ")))
end
for _, t in { 0.05, 0.3, 0.5, 2 } do series("thickness " .. t, function() P.part({ Size = Vector3.new(200, 100, t), Position = Vector3.new(0, 5, 40) }) targets() end) end
for _, mat in { "Plastic", "Wood", "Foil", "Fabric", "Neon", "SmoothPlastic", "Concrete" } do
	series("material " .. mat, function() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 5, 40), Material = Enum.Material[mat] }) targets() end)
end
series("no wall", function() targets() end)
