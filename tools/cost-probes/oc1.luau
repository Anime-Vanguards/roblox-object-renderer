local AssetService = game:GetService("AssetService")
P.look(Vector3.new(0, 5, 60), Vector3.new(0, 5, 0))
P.base()
local M = Enum.Material:GetEnumItems()
local function targets() P.blocks(20, 6, { Material = function(i) return M[i + 1] end }) end
-- a plane of k x k quads (2k^2 triangles) as a MeshPart, 200 x 100 studs, facing +Z
local function plane(k)
	local em = AssetService:CreateEditableMesh()
	local ids = {}
	for y = 0, k do for x = 0, k do ids[y * (k + 1) + x] = em:AddVertex(Vector3.new(x / k - 0.5, y / k - 0.5, 0)) end end
	for y = 0, k - 1 do for x = 0, k - 1 do
		local a, b, c, d = ids[y * (k + 1) + x], ids[y * (k + 1) + x + 1], ids[(y + 1) * (k + 1) + x], ids[(y + 1) * (k + 1) + x + 1]
		em:AddTriangle(a, b, d) em:AddTriangle(a, d, c)
	end end
	local ok, mp = pcall(function() return AssetService:CreateMeshPartAsync(Content.fromObject(em)) end)
	if not ok then table.insert(P.out, "mesh " .. k .. ": " .. tostring(mp)) return nil end
	return mp
end
for _, k in { 1, 4, 8, 16, 24, 32, 48, 64, 96 } do
	local mp = plane(k)
	if mp then
		P.case(string.format("OC plane %d tris + 20 mats behind", 2 * k * k), function(f)
			local w = mp:Clone() w.Anchored = true w.Size = Vector3.new(200, 100, 1) w.CFrame = CFrame.new(0, 5, 40) w.DoubleSided = true w.Parent = f
			targets()
		end, { frames = 30 })
	end
end
P.case("OC no wall, 20 mats", function() targets() end)
P.case("OC camera inside a big box", function() P.part({ Size = Vector3.new(20, 20, 20), Position = Vector3.new(0, 5, 60) }) targets() end, { frames = 30 })
P.case("OC wall tr 0.01", function() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 5, 40), Transparency = 0.01 }) targets() end, { frames = 30 })
P.case("OC wall thin 0.05", function() P.part({ Size = Vector3.new(200, 100, 0.05), Position = Vector3.new(0, 5, 40) }) targets() end, { frames = 30 })
P.case("OC wall neon", function() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 5, 40), Material = Enum.Material.Neon }) targets() end, { frames = 30 })
P.case("OC wall cylinder big", function() P.part({ Shape = Enum.PartType.Cylinder, Size = Vector3.new(2, 200, 200), Position = Vector3.new(0, 5, 40), CFrame = CFrame.new(0, 5, 40) * CFrame.Angles(0, math.rad(90), 0) }) targets() end, { frames = 30 })
P.case("OC wall ball big", function() P.part({ Shape = Enum.PartType.Ball, Size = Vector3.new(150, 150, 150), Position = Vector3.new(0, 5, -40) }) targets() end, { frames = 30 })
P.case("OC wall of 1-stud cubes 40x20 (a grid)", function() for x = -20, 19 do for y = -5, 14 do P.part({ Size = Vector3.new(1, 1, 1), Position = Vector3.new(x + 0.5, y + 0.5, 50) }) end end targets() end, { frames = 30 })
P.case("OC wall humanoid model", function() local m = Instance.new("Model") m.Parent = P.folder Instance.new("Humanoid").Parent = m P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 5, 40) }, m) targets() end, { frames = 30 })
