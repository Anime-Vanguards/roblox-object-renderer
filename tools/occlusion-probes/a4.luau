-- a4: which walls hide what is behind them (materials, thickness, distance)
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a4
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	OCC.look()
	local function hides(label, wallSpec, cubes)
		OCC.reset()
		OCC.grid(cubes or 400, 0.5, -200, 100)
		local base = (function() for _ = 1, 30 do RS.RenderStepped:Wait() end return Stats.SceneDrawcallCount, Stats.SceneTriangleCount end)()
		local _, baseTris = Stats.SceneDrawcallCount, Stats.SceneTriangleCount
		OCC.parts({ wallSpec })
		for _ = 1, 30 do RS.RenderStepped:Wait() end
		table.insert(out, string.format("%-52s tris without the wall %6d, with it %6d", label, baseTris, Stats.SceneTriangleCount))
	end
	local function w(size, thick, z, props)
		local s = { Size = Vector3.new(size, size, thick), CFrame = CFrame.new(0, 50, z) }
		for k, v in props or {} do s[k] = v end
		return s
	end
	hides("Plastic wall 1000x1000x2 at 200 studs", w(1000, 2, -100))
	hides("Neon wall 1000x1000x2", w(1000, 2, -100, { Material = Enum.Material.Neon }))
	hides("Glass wall 1000x1000x2", w(1000, 2, -100, { Material = Enum.Material.Glass }))
	hides("Plastic wall 1000x1000x0.1", w(1000, 0.1, -100))
	hides("Plastic wall 1000x1000x0.01", w(1000, 0.01, -100))
	hides("Plastic wall 300x300x0.1 at 200 studs", w(300, 0.1, -100))
	hides("Plastic wall 150x150x0.1 at 50 studs", w(150, 0.1, 50))
	hides("Plastic wall 150x150x0.2 at 50 studs", w(150, 0.2, 50))
	hides("Plastic wall 150x150x0.5 at 50 studs", w(150, 0.5, 50))
	hides("Plastic wall 150x150x2 at 50 studs", w(150, 2, 50))
	hides("Plastic wall 150x75x0.2 at 58 studs (oc-like)", { Size = Vector3.new(150, 75, 0.2), CFrame = CFrame.new(0, 50, 42) })
	hides("Plastic wall 150x75x4 at 58 studs (oc-like)", { Size = Vector3.new(150, 75, 4), CFrame = CFrame.new(0, 50, 42) })
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
