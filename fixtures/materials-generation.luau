-- Which material generation a place gets, and how the built-in materials look in each.
-- Context.Phase "light" (the default): one floor a case, 60 x 1 x 60, colour (90, 110, 120), each under one PointLight
-- (Brightness 1.5, Range 60, no shadows) 13 or 1.5 studs over its middle; the light's mirror image on the floor shows the
-- material's roughness and normal map. Cases lie 200 studs apart (no light reaches another case's floor), the 13-stud
-- row at Z 0 and the 1.5-stud row at Z 200; each is seen from 30 studs off its light along -X, 5 studs up, looking at
-- the floor's middle (fov 70), in the dark (ClockTime 0, Brightness, Ambient, OutdoorAmbient and the environment
-- scales 0).
-- Context.Phase "sun": white slabs 30 x 2 x 30 in rows of four, 40 studs apart, each row 400 studs from the next, seen
-- straight down from 200 studs (fov 45) under a low sun (ClockTime 8) with the sky's specular at several
-- EnvironmentSpecularScale values, under flat ambient light and under the sky alone: how much light each material
-- keeps, as `ror studio calibrate metal-diffuse` measures it in Studio.
-- Context.Phase "organic": a white 200-stud slab of each Organic material seen straight down from 236 studs (fov 20,
-- 1644 x 1116), as `organic-builtin`: the Organic pattern's noise frequency (tools/analysis/organic.py offsets --far).
-- Context.Phase "far": a white 1000-stud slab of each far-tiled material and two others, seen straight down from 30 to
-- 450 studs (fov 30): how a texture's contrast changes with distance.
-- Context.Phase "shapes": a block, a ball, a cylinder, a wedge and a corner wedge of each material, seen from 55 studs
-- off and 26 up (fov 50): how the texture wraps each shape. "organic", "far" and "shapes" are drawn under flat
-- ambient light (Brightness 0, Ambient 0.7, the environment scales 0).
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic, Technology Future) in places written
-- with and without a MaterialService (a place file without one loads the pre-2022 materials), the case's parts built
-- by the job; against ror's render of the same, from that place file:
-- ror render fixtures/materials-generation.luau --lighting-from <the place> --materials-from <the place>
--   --size 1600x900 --no-service --camera '{"mode":"lookAt","eye":[<x - 30>,5,<z>],"target":[<x>,0,<z>],"fov":70}'
-- (notes/materials.md "Which generation a file gets"; ROR_REE: the engine emulator's checkout). It returns each case's
-- name and where it is: a floor's middle and its light's height (light), a slab's middle (sun, organic, far), a row's
-- middle (shapes).
return function(Context)
	local Phase = Context and Context.Phase or "light"
	local Root = Instance.new("Model")
	Root.Name = "MaterialsGeneration"
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
		Part.Parent = Root
		return Part
	end

	if Phase == "light" then
		local MATERIALS = {
			"SmoothPlastic", "Plastic", "Metal", "Marble", "Glass", "Ice", "Foil", "DiamondPlate", "CorrodedMetal",
			"Slate", "Granite", "Concrete", "Wood", "WoodPlanks", "Brick", "Fabric", "Sand", "Pebble",
		}
		for Row, Height in { 13, 1.5 } do
			for Index, Name in MATERIALS do
				local Origin = Vector3.new((Index - 1) * 200, 0, (Row - 1) * 200)
				local Case = `{Name}-{Height}`
				Block(`Floor-{Case}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60), Enum.Material[Name], Color3.fromRGB(90, 110, 120))
				local Carrier = Instance.new("Part")
				Carrier.Name = `Lamp-{Case}`
				Carrier.Anchored = true
				Carrier.Transparency = 1
				Carrier.CastShadow = false
				Carrier.CanCollide = false
				Carrier.Size = Vector3.new(0.2, 0.2, 0.2)
				Carrier.CFrame = CFrame.new(Origin + Vector3.new(0, Height, 0))
				Carrier.Parent = Root
				local Light = Instance.new("PointLight")
				Light.Brightness = 1.5
				Light.Range = 60
				Light.Shadows = false
				Light.Parent = Carrier
				table.insert(Layout, { name = Case, material = Name, x = Origin.X, y = Origin.Y, z = Origin.Z, ly = Height })
			end
		end
	elseif Phase == "sun" then
		local ROWS = {
			{ "Metal", "Plastic", "Foil", "SmoothPlastic" },
			{ "DiamondPlate", "CorrodedMetal", "Ice", "Marble" },
			{ "Slate", "Concrete", "Wood", "Granite" },
		}
		for Row, Materials in ROWS do
			for Index, Name in Materials do
				local Origin = Vector3.new((Index - 1) * 40 - 60, 0, (Row - 1) * 400)
				Block(Name, Origin, Vector3.new(30, 2, 30), Enum.Material[Name], Color3.new(1, 1, 1))
				table.insert(Layout, { name = Name, row = Row, x = Origin.X, y = 1, z = Origin.Z })
			end
		end
	elseif Phase == "organic" then
		-- the Organic pattern's noise frequency: a white 200-stud slab of each Organic material, 400 studs apart,
		-- seen straight down from 236 studs (fov 20) under flat ambient light, as `ror studio calibrate
		-- organic-builtin` does
		local MATERIALS = { "Basalt", "Limestone", "Rock", "Sandstone", "LeafyGrass", "Snow", "Mud", "Ground", "Asphalt", "Glacier" }
		for Index, Name in MATERIALS do
			local Origin = Vector3.new((Index - 1) * 400, 0, 0)
			Block(Name, Origin, Vector3.new(200, 2, 200), Enum.Material[Name], Color3.new(1, 1, 1))
			table.insert(Layout, { name = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	elseif Phase == "far" then
		-- how a texture's contrast fades with distance: a white 1000-stud slab of each material, 2000 studs apart,
		-- seen straight down from several heights (fov 30) under flat ambient light
		local MATERIALS = { "Slate", "Concrete", "Granite", "CorrodedMetal", "Wood", "Marble" }
		for Index, Name in MATERIALS do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 0)
			Block(Name, Origin, Vector3.new(1000, 2, 1000), Enum.Material[Name], Color3.new(1, 1, 1))
			table.insert(Layout, { name = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	elseif Phase == "shapes" then
		-- how a material's texture wraps other shapes: a block, a ball, a cylinder, a wedge and a corner wedge of
		-- each material in a row 20 studs apart, the rows 100 studs apart, under flat ambient light
		local MATERIALS = { "Brick", "WoodPlanks", "Wood", "Slate", "Fabric", "Granite" }
		local SHAPES = {
			{ "Block", Enum.PartType.Block },
			{ "Ball", Enum.PartType.Ball },
			{ "Cylinder", Enum.PartType.Cylinder },
			{ "Wedge", Enum.PartType.Wedge },
			{ "CornerWedge", Enum.PartType.CornerWedge },
		}
		for Row, Name in MATERIALS do
			for Index, Shape in SHAPES do
				local Origin = Vector3.new((Index - 3) * 20, 6, (Row - 1) * 100)
				local Part = Block(`{Name}-{Shape[1]}`, Origin, Vector3.new(12, 12, 12), Enum.Material[Name], Color3.new(1, 1, 1))
				Part.Shape = Shape[2]
				Part.CFrame = CFrame.new(Origin) * CFrame.Angles(0, math.rad(30), 0)
			end
			table.insert(Layout, { name = Name, row = Row, x = 0, y = 6, z = (Row - 1) * 100 })
		end
	else
		error(`materials-generation: no phase {Phase}`)
	end
	Root.Parent = workspace
	return Layout
end
