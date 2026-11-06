P.look(Vector3.new(0, 5, 80), Vector3.new(0, 5, 0))
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
-- a wall covering x < 0 (left half of the view) at z = 40; group A: 20 small blocks of distinct
-- materials hidden behind it at x = -30; part B: a visible block at x = -30 + d (right of the wall
-- edge once d > 30)
local function scene(d, gx)
	return function()
		P.part({ Size = Vector3.new(150, 150, 4), Position = Vector3.new(-75, 5, 40) })
		for i = 0, 19 do P.part({ Size = Vector3.new(1, 1, 1), Position = Vector3.new(gx + (i % 5) * 1.2, 2 + (i // 5) * 1.2, 0), Material = M[i + 2] }) end
		P.part({ Size = Vector3.new(1, 1, 1), Position = Vector3.new(gx + d, 2, 0), Material = Enum.Material.Neon })
	end
end
table.insert(P.out, "group only (hidden): " .. count(function() P.part({ Size = Vector3.new(150, 150, 4), Position = Vector3.new(-75, 5, 40) }) for i = 0, 19 do P.part({ Size = Vector3.new(1, 1, 1), Position = Vector3.new(-30 + (i % 5) * 1.2, 2 + (i // 5) * 1.2, 0), Material = M[i + 2] }) end end))
local row = {}
for _, d in { 34, 36, 40, 48, 64, 96, 128, 192 } do table.insert(row, d .. ":" .. count(scene(d, -30))) end
table.insert(P.out, "group at -30, B at -30+d: " .. table.concat(row, " "))
row = {}
for _, gx in { -10, -20, -40, -60, -70 } do table.insert(row, gx .. ":" .. count(scene(12 - gx, gx))) end
table.insert(P.out, "group at gx, B at x=12: " .. table.concat(row, " "))
