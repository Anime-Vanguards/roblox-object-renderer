-- a5: wall distance, size, material and building order (wall first or last)
-- run after prof.luau and occ.luau: python tools/occlusion_probes.py a5
local out = {}
local ok, err = pcall(function()
	OCC.hideMap()
	local function tris() for _ = 1, 30 do RS.RenderStepped:Wait() end return Stats.SceneTriangleCount end
	-- camera at z=60 looking -z (oc1's), 400 small targets around z=0
	local function targets() OCC.grid(400, 0.5, 0, 30) end
	local function case(label, dist, size, thick, mat, order)
		OCC.reset()
		cam.CFrame = CFrame.lookAt(Vector3.new(0, 50, 60), Vector3.new(0, 50, 0))
		local wall = { Size = Vector3.new(size[1], size[2], thick), CFrame = CFrame.new(0, 50, 60 - dist), Material = Enum.Material[mat] }
		local before
		if order == "after" then targets() before = tris() OCC.parts({ wall }) else OCC.parts({ wall }) targets() end
		local t = tris()
		table.insert(out, string.format("%-60s tris %5d%s", label, t, before and (" (before the wall " .. before .. ")") or ""))
	end
	for _, mat in { "Plastic", "Neon" } do
		for _, dist in { 20, 40, 55 } do
			case(mat .. " 200x100x2 at " .. dist .. " studs, together", dist, { 200, 100 }, 2, mat, "together")
		end
		case(mat .. " 200x100x2 at 20 studs, wall added after", 20, { 200, 100 }, 2, mat, "after")
		case(mat .. " 1000x1000x2 at 20 studs, together", 20, { 1000, 1000 }, 2, mat, "together")
		case(mat .. " 40x20x2 at 20 studs, together", 20, { 40, 20 }, 2, mat, "together")
	end
	case("Plastic 200x100x0.05 at 20, together", 20, { 200, 100 }, 0.05, "Plastic", "together")
	case("Plastic 200x100x0.05 at 40, together", 40, { 200, 100 }, 0.05, "Plastic", "together")
	case("Plastic 200x100x0.05 at 20, wall added after", 20, { 200, 100 }, 0.05, "Plastic", "after")
end)
OCC.reset()
OCC.showMap()
if not ok then table.insert(out, "ERROR " .. tostring(err)) end
return table.concat(out, "\n")
