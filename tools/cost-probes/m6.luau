-- m6: two blocks far apart: where copies stop sharing a draw
local SS = game:GetService("ServerStorage")
local map = workspace:FindFirstChild("RorCapture")
if map then map.Parent = SS end
local ok, err = pcall(function()
	local function at(points)
		return function()
			for _, p in points do P.part({ Size = Vector3.new(4, 4, 4), Position = p }) end
		end
	end
	local function look(center, dist)
		P.look(center + Vector3.new(0, dist, dist * 0.3), center)
		P.base()
	end
	look(Vector3.zero, 600)
	P.case("M6 2 blocks y=2, 400 apart in x", at({ Vector3.new(-200, 2, 0), Vector3.new(200, 2, 0) }))
	P.case("M6 2 blocks y=2 and y=300", at({ Vector3.new(0, 2, 0), Vector3.new(0, 300, 0) }))
	P.case("M6 2 blocks y=-300 and y=300", at({ Vector3.new(0, -300, 0), Vector3.new(0, 300, 0) }))
	look(Vector3.new(0, 1700, 0), 600)
	P.case("M6 2 blocks at y=1700, 10 apart", at({ Vector3.new(-5, 1700, 0), Vector3.new(5, 1700, 0) }))
	P.case("M6 2 blocks at y=1700, 400 apart", at({ Vector3.new(-200, 1700, 0), Vector3.new(200, 1700, 0) }))
	P.case("M6 2 blocks y=1650 and y=1750", at({ Vector3.new(0, 1650, 0), Vector3.new(0, 1750, 0) }))
	look(Vector3.new(0, 0, 0), 900)
	local grid = {}
	for x = -2, 2 do for z = -2, 2 do table.insert(grid, Vector3.new(x * 120, (x + z) * 60, z * 120)) end end
	P.case("M6 25 blocks on a slope (+-240 y)", at(grid))
	local flat = {}
	for x = -2, 2 do for z = -2, 2 do table.insert(flat, Vector3.new(x * 120, 2, z * 120)) end end
	P.case("M6 25 blocks flat", at(flat))
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
