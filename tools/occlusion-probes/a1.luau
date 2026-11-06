-- a1: the culling jobs' time against objects in view (tiny cubes, no occluders) and behind the camera
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a1
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	OCC.look()
	OCC.reset()
	table.insert(out, OCC.line("empty", OCC.measure()))
	for _, n in { 1000, 4000, 16000, 50000 } do
		OCC.reset()
		OCC.grid(n, 0.5, -200, 400)
		table.insert(out, OCC.line(n .. " cubes in view, no occluder", OCC.measure()))
	end
	OCC.reset()
	OCC.grid(16000, 0.5, 300, 400)
	table.insert(out, OCC.line("16000 cubes behind the camera", OCC.measure()))
	OCC.reset()
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
