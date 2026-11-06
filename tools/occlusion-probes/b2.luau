-- b2: the per-object test (tiny cubes) and the per-occluder cost (opaque against Transparency 0.02 copies)
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py b2
local out = {}
local ok, err = pcall(function()
	local probe = workspace:FindFirstChild("Probe") if probe then probe:Destroy() end
	OCC.hideMap()
	OCC.look()
	-- 1. per tested object: tiny cubes, never occluders
	for _, n in { 0, 8000, 32000, 64000 } do
		OCC.reset() if n > 0 then OCC.grid(n, 0.4, -200, 400) end
		table.insert(out, OCC.line(n .. " tiny cubes", OCC.measure(150)))
	end
	-- 2. occluders: k cubes of side s at 250 studs, opaque vs Transparency 0.02 (same tests, no occluders)
	for _, case in { { 250, 20 }, { 1000, 10 }, { 4000, 5 }, { 1000, 5 }, { 1000, 2.5 }, { 250, 5 } } do
		local k, side = case[1], case[2]
		for _, tr in { 0, 0.02 } do
			OCC.reset() OCC.grid(k, side, -150, 400, { Transparency = tr })
			table.insert(out, OCC.line(string.format("%d occluder cubes side %.1f tr %.2f", k, side, tr), OCC.measure(150)))
		end
	end
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
