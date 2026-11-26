-- The sky specular and the sun's on large metal and dielectric slabs (notes/render.md "Specular fade", notes/materials.md).
-- "slabs": a 600 x 2 x 600 white slab of Foil, DiamondPlate, Metal, SmoothPlastic and Plastic, 2000 studs apart along
--   X at Z 0. The job that measured them saw them straight down from 12 to 200 studs (fov 30), and the Foil and Metal
--   slabs also 40 and 70 degrees up from four sides, setting the Lighting per arm (the sky specular alone, the sun
--   alone at ClockTime 8, the sun with EnvironmentSpecularScale 1, the sky's diffuse light, all of them;
--   notes/materials.md "Foil's lobe in the engine").
-- Returns each case's name, material and the middle of its top face.
return function(Context)
	local Phase = Context and Context.Phase or "all"
	local Root = Instance.new("Model")
	Root.Name = "SkySpecular"
	local Layout = {}

	local function Block(Name, Position, Size, Material, Color)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Material
		Part.Color = Color
		Part.Size = Size
		Part.CFrame = CFrame.new(Position)
		-- (a new Part has Studs on top and Inlet below)
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.CastShadow = false
		Part.Parent = Root
		return Part
	end

	if Phase == "all" or Phase == "slabs" then
		for Index, Name in { "Foil", "DiamondPlate", "Metal", "SmoothPlastic", "Plastic" } do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 0)
			Block(`Slab-{Name}`, Origin, Vector3.new(600, 2, 600), Enum.Material[Name], Color3.new(1, 1, 1))
			table.insert(Layout, { name = `Slab-{Name}`, phase = "slabs", material = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	elseif Phase ~= "all" then
		error(`sky-specular: no phase {Phase}`)
	end
	Root.Parent = workspace
	return Layout
end
