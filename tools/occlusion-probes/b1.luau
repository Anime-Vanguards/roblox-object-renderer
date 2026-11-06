-- b1: the per-object test against object count, screen size and occluder status (Transparency 0.02 never occludes)
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py b1
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	OCC.look()
	-- per object and per screen size: small cubes are never occluders; Transparency 0.02 never is
	for _, n in { 4000, 16000 } do
		for _, size in { 0.5, 2, 4 } do
			for _, tr in { 0, 0.02 } do
				OCC.reset() OCC.grid(n, size, -200, 400, { Transparency = tr })
				table.insert(out, OCC.line(string.format("%d cubes %.1f studs tr %.2f", n, size, tr), OCC.measure(150)))
			end
		end
	end
	-- the same 16000 small cubes stacked in depth behind one another (one screen spot) vs spread
	OCC.reset()
	local list = {}
	for i = 0, 15999 do table.insert(list, { Size = Vector3.one * 0.5, CFrame = CFrame.new((i % 40) * 0.6 - 12, 50 + (i // 40 % 40) * 0.6 - 12, -100 - (i // 1600) * 10) }) end
	OCC.parts(list)
	table.insert(out, OCC.line("16000 cubes 0.5 in 10 stacked layers (24x24 studs)", OCC.measure(150)))
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
