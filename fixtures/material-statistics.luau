-- How the built-in materials' textures read with distance, light and part size (notes/materials.md).
-- Every case is a white part (or a row of them) far from the others, so one load holds every phase; Context.Phase
-- picks one ("all", the default, builds them all):
-- "tiles": a 600 x 2 x 600 slab of each material, 2000 studs apart along X at Z 0, seen straight down from 6 to 300
--   studs (fov 30) under flat ambient light: the colour map's contrast and tilings with view depth.
-- "relief": the same at Z 4000 for the materials whose normal maps show, seen likewise under a low sun (ClockTime 8):
--   the relief with view depth.
-- "parts": a 400-stud slab of each material at Z 8000 with a 12-stud and a 30-stud block on it (their tops 0.02 studs
--   over the slab's, at X -20 and +40 from its middle): small and large parts at one view depth in one frame.
-- "light": a 60 x 1 x 60 floor of each material, colour (90, 110, 120), under a PointLight (Brightness 1.5, Range 60,
--   no shadows) 13 studs over its middle, at Z 12000 (materials-generation's "light" phase): the light's mirror image.
-- "tone": a row of 10-stud SmoothPlastic and Neon tiles in warm and gray colours at Z 16000, seen straight down at
--   several ExposureCompensation values: how each channel rises as one saturates.
-- The job that captures it pins the camera and Lighting per case. Returns each case's name, phase, material and the
-- middle of its top face.
return function(Context)
	local Phase = Context and Context.Phase or "all"
	local Root = Instance.new("Model")
	Root.Name = "MaterialStatistics"
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

	local function Wants(Name)
		return Phase == "all" or Phase == Name
	end

	local WHITE = Color3.new(1, 1, 1)
	if Wants("tiles") then
		for Index, Name in { "Fabric", "Salt", "Cobblestone", "Granite", "Metal", "Pebble", "Slate", "Concrete", "CorrodedMetal" } do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 0)
			Block(`Tiles-{Name}`, Origin, Vector3.new(600, 2, 600), Enum.Material[Name], WHITE)
			table.insert(Layout, { name = `Tiles-{Name}`, phase = "tiles", material = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	end
	if Wants("relief") then
		for Index, Name in { "Foil", "Slate", "CorrodedMetal", "Ice", "Fabric", "DiamondPlate", "Plastic", "Concrete", "Wood", "Granite" } do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 4000)
			Block(`Relief-{Name}`, Origin, Vector3.new(600, 2, 600), Enum.Material[Name], WHITE)
			table.insert(Layout, { name = `Relief-{Name}`, phase = "relief", material = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	end
	if Wants("parts") then
		for Index, Name in { "Slate", "Granite", "Concrete", "CorrodedMetal", "Fabric", "Salt" } do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 8000)
			Block(`Parts-{Name}`, Origin, Vector3.new(400, 2, 400), Enum.Material[Name], WHITE)
			Block(`Parts-{Name}-12`, Origin + Vector3.new(-20, 0.02, 0), Vector3.new(12, 2, 12), Enum.Material[Name], WHITE)
			Block(`Parts-{Name}-30`, Origin + Vector3.new(40, 0.02, 0), Vector3.new(30, 2, 30), Enum.Material[Name], WHITE)
			table.insert(Layout, { name = `Parts-{Name}`, phase = "parts", material = Name, x = Origin.X, y = 1, z = Origin.Z })
		end
	end
	if Wants("light") then
		for Index, Name in { "Ice", "Metal", "Granite", "Marble" } do
			local Origin = Vector3.new((Index - 1) * 2000, 0, 12000)
			Block(`Light-{Name}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60), Enum.Material[Name], Color3.fromRGB(90, 110, 120))
			local Carrier = Instance.new("Part")
			Carrier.Name = `Lamp-{Name}`
			Carrier.Anchored = true
			Carrier.Transparency = 1
			Carrier.CastShadow = false
			Carrier.CanCollide = false
			Carrier.Size = Vector3.new(0.2, 0.2, 0.2)
			Carrier.CFrame = CFrame.new(Origin + Vector3.new(0, 13, 0))
			Carrier.Parent = Root
			local Light = Instance.new("PointLight")
			Light.Brightness = 1.5
			Light.Range = 60
			Light.Shadows = false
			Light.Parent = Carrier
			table.insert(Layout, { name = `Light-{Name}`, phase = "light", material = Name, x = Origin.X, y = 0, z = Origin.Z })
		end
	end
	if Wants("tone") then
		local TILES = {
			{ "SmoothPlastic", Color3.fromRGB(255, 110, 40) },
			{ "SmoothPlastic", Color3.fromRGB(200, 90, 50) },
			{ "SmoothPlastic", Color3.fromRGB(160, 160, 160) },
			{ "SmoothPlastic", Color3.fromRGB(255, 255, 255) },
			{ "Neon", Color3.fromRGB(255, 110, 40) },
			{ "Neon", Color3.fromRGB(120, 60, 40) },
		}
		local Origin = Vector3.new(0, 0, 16000)
		for Index, Tile in TILES do
			Block(`Tone-{Index}`, Origin + Vector3.new((Index - 3.5) * 12, 0, 0), Vector3.new(10, 2, 10), Enum.Material[Tile[1]], Tile[2])
		end
		Block("Tone-Floor", Origin + Vector3.new(0, -2, 0), Vector3.new(120, 2, 40), Enum.Material.SmoothPlastic, Color3.fromRGB(30, 30, 30))
		table.insert(Layout, { name = "Tone", phase = "tone", material = "SmoothPlastic", x = Origin.X, y = 1, z = Origin.Z })
	end
	if #Layout == 0 then
		error(`material-statistics: no phase {Phase}`)
	end
	Root.Parent = workspace
	return Layout
end
