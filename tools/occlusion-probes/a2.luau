-- a2: walls hiding everything or nothing, and growing numbers of occluder cubes
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a2
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	OCC.look()
	local function wall(z, size, thick)
		OCC.parts({ { Size = Vector3.new(size, size, thick or 2), CFrame = CFrame.new(0, 50, z) } })
	end
	OCC.reset() OCC.grid(16000, 0.5, -200, 400) wall(-100, 1000)
	table.insert(out, OCC.line("16000 cubes behind a wall hiding all", OCC.measure()))
	OCC.reset() OCC.grid(16000, 0.5, -200, 400) wall(-300, 1000)
	table.insert(out, OCC.line("16000 cubes in front of a wall", OCC.measure()))
	OCC.reset() OCC.grid(16000, 0.5, -200, 400) wall(-100, 1000, 0.1)
	table.insert(out, OCC.line("16000 cubes behind a 0.1-thick wall", OCC.measure()))
	for _, k in { 10, 100, 1000, 4000 } do
		OCC.reset() OCC.grid(k, 4, -150, 400)
		table.insert(out, OCC.line(k .. " occluder cubes (4 studs)", OCC.measure()))
	end
	for _, k in { 1000, 4000 } do
		OCC.reset() OCC.grid(k, 4, -150, 400, { Size = Vector3.new(4, 4, 0.1) })
		table.insert(out, OCC.line(k .. " thin tiles (0.1, not occluders)", OCC.measure()))
	end
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
