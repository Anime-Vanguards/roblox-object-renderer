-- a6: the wall sizes and distances that fail to hide (a map of width x height at 10, 20 and 40 studs)
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a6
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	local function tris() for _ = 1, 25 do RS.RenderStepped:Wait() end return Stats.SceneTriangleCount end
	local function run(w, h, dist, thick)
		OCC.reset()
		cam.CFrame = CFrame.lookAt(Vector3.new(0, 50, 60), Vector3.new(0, 50, 0))
		OCC.parts({ { Size = Vector3.new(w, h, thick or 2), CFrame = CFrame.new(0, 50, 60 - dist) } })
		OCC.grid(100, 0.5, -20, 10)
		return tris()
	end
	local sizes = { 20, 40, 80, 120, 160, 200, 300, 400, 600, 1000, 2000 }
	for _, dist in { 10, 20, 40 } do
		table.insert(out, "distance " .. dist .. " (rows: width, columns: height " .. table.concat(sizes, "/") .. "): . hides, X shows")
		for _, w in sizes do
			local row = {}
			for _, h in sizes do
				local t = run(w, h, dist)
				table.insert(row, t < 200 and "." or "X")
			end
			table.insert(out, string.format("  w %5d  %s", w, table.concat(row, " ")))
		end
	end
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
