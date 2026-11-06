-- a3: occluder status by material (Plastic, Neon, Glass), occluder size on screen, tested objects far and near
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a3
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	OCC.look()
	for _, mat in { "Plastic", "Neon", "Glass" } do
		OCC.reset() OCC.grid(4000, 4, -150, 400, { Material = Enum.Material[mat] })
		table.insert(out, OCC.line("4000 cubes 4 studs " .. mat, OCC.measure()))
	end
	-- occluder size on screen: 100 occluders tiling the view vs small
	OCC.reset() OCC.grid(100, 38, -150, 400)
	table.insert(out, OCC.line("100 occluders 38 studs (tiling the view)", OCC.measure()))
	OCC.reset() OCC.grid(100, 4, -150, 400)
	table.insert(out, OCC.line("100 occluders 4 studs", OCC.measure()))
	OCC.reset() OCC.grid(1000, 12, -150, 400)
	table.insert(out, OCC.line("1000 occluders 12 studs (tiling)", OCC.measure()))
	OCC.reset() OCC.grid(1000, 1, -150, 400)
	table.insert(out, OCC.line("1000 occluders 1 stud", OCC.measure()))
	-- occludees far away and tiny vs near
	OCC.reset() OCC.grid(16000, 0.5, -200, 400, { Material = Enum.Material.Neon })
	table.insert(out, OCC.line("16000 neon cubes 0.5 at 300 studs", OCC.measure()))
	OCC.reset() OCC.grid(16000, 5, -2900, 4000, { Material = Enum.Material.Neon })
	table.insert(out, OCC.line("16000 neon cubes 5 at 3000 studs (same view)", OCC.measure()))
	OCC.reset() OCC.grid(16000, 4, -200, 400, { Material = Enum.Material.Neon })
	table.insert(out, OCC.line("16000 neon cubes 4 studs at 300 (8x bigger)", OCC.measure()))
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
