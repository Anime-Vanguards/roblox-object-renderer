-- Graphics quality levels: one station a feature, built alone (Context.Station) so stations never see one
-- another, each with the camera, Lighting and captures it is measured under and the floor points read in
-- them. Used to sweep a Roblox client over its graphics quality levels 1 to 21, one level a session, and
-- to draw the same scenes here.
--
-- Stations:
--   quality     a 4 x 12 x 4 column in the sun, a shadowed PointLight (Range 24, Brightness 8) 7 studs up
--               behind a 1 x 5 x 8 blocker, a Neon bar; the camera 63 studs off
--   roof        a 48 x 48 roof 16 studs up, of a block (x -100) and of the install's quad mesh (x +100):
--               the sun alone, then sky light alone (OutdoorAmbient), so the floor under each roof shows how
--               the sun is stopped where the sun's shadow map is not drawn (the shadow falls 31 studs along
--               +X and 9 along -Z of the roof, the floor under the roof's own footprint is in the sun)
--   reach       3 x 14 x 3 columns 15 to 500 studs from the camera: how far sun shadows reach
--   lamps       one shadowed PointLight (Range 16) beside a blocker, repeated 15 to 230 studs from the camera
--   lightkinds  a SpotLight, a SurfaceLight, an unshadowed PointLight beside a 4-stud wall, a Range 4 light
--   bloom       Neon bars and a lit white plate in the dark, without and with a BloomEffect
--   ao          a box, a corner and a step under Ambient alone
--   far         pillars 280 to 20,000 studs out, each 2.5 % of its distance wide: the view distance
--   tiny        cubes 0.05 to 0.6 studs, 120 studs out: the size cull, at a field of view of 70 and of 8
--   fx          ParticleEmitters (300 at once by Emit, Rate 30, Rate 300) and a curved Beam, from above
--   post        a row of columns toward the low sun: no effect, then each post effect alone, Atmosphere, Clouds
--   materials   a plate of each Material, balls, Glass before stripes, a Decal, a Texture, a Highlight,
--               an EditableImage SurfaceAppearance; then four textured plates from close by
--   coverage    rails 6 studs up running 1,300 studs away from the camera: where each rail's shadow strip
--               ends, the sun's shadow map ends (three cameras)
--   lightline   22 shadowed PointLights 20 studs apart along a rail: where the rail's shadow ends, local
--               lights stop being drawn per pixel
--   hqline      strips of Metal, Foil, Brick and SmoothPlastic 800 studs long, running away from the camera
--   voxsun      blocks, slabs, a wall and a column on world multiples of 4 under the sun alone, from straight
--               above at 900 studs (past every shadow map) and at 150
--   voxlight    PointLights of Range 8, 16 and 32, a SpotLight and a SurfaceLight over a floor in the dark,
--               from straight above at 80 and at 800 studs
--   farfine1/2  upright pillars 490 to 19,000 studs out, finer than far: the view distance
--   atmo        an Atmosphere and a ColorGradingEffect (Retro) over columns 30 to 960 studs out
--
-- The scene is the same whatever the Lighting; Layout.Captures lists for each capture the Lighting it is
-- measured under (properties to set, effects to add, the Sky) and its camera. The sun's direction those
-- points are placed by is ClockTime 16.2 at GeographicLatitude 38 (as fixtures/leaf-shadows.luau). To
-- draw a station here, pass its camera with --camera, Context.Station (and Context.Mesh, the quad mesh's
-- file:// path) with --context, and a place with its Lighting with --lighting-from.
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Station = Context and Context.Station or "quality"
	local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local Root = Instance.new("Model")
	Root.Name = "QualityLevels-" .. Station

	-- towards the sun at ClockTime 16.2, latitude 38: shadows fall 1.963 studs along +X and 0.570 along
	-- -Z per stud of height
	local Sun = Vector3.new(-0.862626, 0.439530, 0.250380)
	local function Shadow(Point)
		return Point - Sun * (Point.Y / Sun.Y)
	end

	local GREY = Color3.fromRGB(163, 162, 165)
	local function Block(Name, Size, Position, Colour, Material, Parent)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Size = Size
		Part.CFrame = if typeof(Position) == "CFrame" then Position else CFrame.new(Position)
		Part.Color = Colour or GREY
		Part.Material = Material or Enum.Material.SmoothPlastic
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.Parent = Parent or Root
		return Part
	end
	local function Lamp(Name, Position, Class, Props)
		local Carrier = Block(Name, Vector3.new(0.2, 0.2, 0.2), Position)
		Carrier.Transparency = 1
		Carrier.CastShadow = false
		Carrier.CanCollide = false
		local Light = Instance.new(Class or "PointLight")
		for Key, Value in Props do
			Light[Key] = Value
		end
		Light.Parent = Carrier
		return Carrier, Light
	end

	local Samples = {}
	local function Sample(Name, Point)
		table.insert(Samples, { Name = Name, Point = Point })
	end
	local function Camera(Eye, Target, Fov)
		return { Eye = Eye, Target = Target, Fov = Fov or 70 }
	end

	local DAY = {
		ClockTime = 16.2, GeographicLatitude = 38, Brightness = 2, Ambient = Color3.fromRGB(50, 50, 50),
		OutdoorAmbient = Color3.fromRGB(128, 128, 128), EnvironmentDiffuseScale = 1, EnvironmentSpecularScale = 1,
		ShadowSoftness = 0.2, ExposureCompensation = 0, FogEnd = 100000, Sky = "default",
	}
	local function With(Base, Changes)
		local Copy = table.clone(Base)
		for Key, Value in Changes do
			Copy[Key] = Value
		end
		return Copy
	end
	local SUN_ONLY = With(DAY, { Ambient = Color3.new(0, 0, 0), OutdoorAmbient = Color3.new(0, 0, 0),
		EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0 })
	local SKY_ONLY = With(SUN_ONLY, { Brightness = 0, OutdoorAmbient = Color3.fromRGB(128, 128, 128) })
	local DARK = With(DAY, { Brightness = 0, Ambient = Color3.fromRGB(25, 25, 25), OutdoorAmbient = Color3.fromRGB(25, 25, 25),
		EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0 })
	local BLACK = With(DARK, { Ambient = Color3.new(0, 0, 0), OutdoorAmbient = Color3.new(0, 0, 0), Sky = "dark" })

	local Captures = {}
	local After = {}
	local function Capture(Tag, Lighting, View, Effects)
		table.insert(Captures, { Tag = Tag, Lighting = Lighting, Camera = View, Effects = Effects or {} })
	end

	if Station == "quality" then
		Block("Floor", Vector3.new(160, 1, 160), Vector3.new(0, -0.5, 0))
		Block("Column", Vector3.new(4, 12, 4), Vector3.new(0, 6, 0))
		Sample("sun-shadow", Shadow(Vector3.new(0, 8, 0)))
		Sample("sun-lit", Shadow(Vector3.new(0, 8, 0)) + Vector3.new(0, 0, 10))
		Lamp("Lamp", Vector3.new(-24, 7, 16), "PointLight", { Range = 24, Brightness = 8, Shadows = true })
		Block("Blocker", Vector3.new(1, 5, 8), Vector3.new(-20, 2.5, 16))
		Sample("light-shadow", Vector3.new(-15, 0, 16))
		Sample("light-open", Vector3.new(-33, 0, 16))
		for _, Distance in { 2, 4, 8, 12, 16, 20, 22, 24, 26 } do
			Sample(`light-rim-{Distance}`, Vector3.new(-24, 0, 16 - Distance))
		end
		Block("Neon", Vector3.new(1, 1, 10), Vector3.new(16, 0.5, 22), Color3.fromRGB(255, 220, 160), Enum.Material.Neon)
		for _, Distance in { 1, 2, 3.5, 6, 10 } do
			Sample(`neon-halo-{Distance}`, Vector3.new(16.5 + Distance, 0, 22))
		end
		Capture("quality", DAY, Camera(Vector3.new(-4, 30, 58), Vector3.new(-2, 0, 4), 70))
	elseif Station == "roof" then
		Block("Floor", Vector3.new(360, 1, 200), Vector3.new(10, -0.5, 0), Color3.fromRGB(200, 200, 200))
		for _, Roof in { { "block", -100 }, { "mesh", 100 } } do
			local X = Roof[2]
			if Roof[1] == "block" then
				Block("RoofBlock", Vector3.new(48, 1, 48), Vector3.new(X, 16.5, 0))
			else
				-- the install's quad (1024 units square in XY from its origin, facing +Z) turned to face up
				local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
				Part.Name = "RoofMesh"
				Part.Anchored = true
				Part.Material = Enum.Material.SmoothPlastic
				Part.Color = GREY
				Part.Size = Vector3.new(48, 48, 0.001)
				Part.CFrame = CFrame.new(X, 17, 0) * CFrame.Angles(-math.pi / 2, 0, 0) * CFrame.new(-24, -24, 0)
				Part.Parent = Root
			end
			-- along z = -4: x - 24 to x + 7.4 under the roof in the sun, x + 7.4 to x + 24 under the roof and in
			-- its shadow, x + 24 to x + 55.4 in its shadow in the open
			for Offset = -40, 70, 5 do
				Sample(`{Roof[1]}-{Offset}`, Vector3.new(X + Offset, 0, -4))
			end
			local View = Camera(Vector3.new(X + 8, 12, 72), Vector3.new(X + 8, 0, -4), 50)
			Capture(`roof-{Roof[1]}-sun`, SUN_ONLY, View)
			Capture(`roof-{Roof[1]}-sky`, SKY_ONLY, View)
		end
	elseif Station == "reach" then
		Block("Floor", Vector3.new(1200, 1, 1200), Vector3.new(0, -0.5, -560))
		for _, Distance in { 15, 25, 35, 50, 65, 80, 100, 120, 150, 180, 220, 270, 330, 400, 500 } do
			local X = -0.25 * Distance
			Block(`Column{Distance}`, Vector3.new(3, 14, 3), Vector3.new(X, 7, -Distance))
			local Middle = Shadow(Vector3.new(X, 9, -Distance))
			Sample(`shadow-{Distance}`, Middle)
			Sample(`lit-{Distance}`, Middle + Vector3.new(0, 0, 6))
		end
		Capture("reach", DAY, Camera(Vector3.new(0, 25, 20), Vector3.new(0, 0, -120), 50))
	elseif Station == "lamps" then
		Block("Floor", Vector3.new(200, 1, 400), Vector3.new(0, -0.5, -150))
		for Index, Distance in { 15, 30, 50, 75, 110, 160, 230 } do
			local X = if Index % 2 == 1 then -12 else 10
			Lamp(`Lamp{Distance}`, Vector3.new(X, 6, -Distance), "PointLight", { Range = 16, Brightness = 4, Shadows = true })
			Block(`Blocker{Distance}`, Vector3.new(1, 4, 6), Vector3.new(X + 3, 2, -Distance))
			Sample(`shadow-{Distance}`, Vector3.new(X + 5.5, 0, -Distance))
			Sample(`open-{Distance}`, Vector3.new(X - 5.5, 0, -Distance))
			for _, Radius in { 2, 5, 8, 11, 14 } do
				Sample(`rim-{Distance}-{Radius}`, Vector3.new(X, 0, -Distance + Radius))
			end
		end
		Capture("lamps", DARK, Camera(Vector3.new(0, 30, 25), Vector3.new(0, 0, -60), 60))
	elseif Station == "lightkinds" then
		Block("Floor", Vector3.new(160, 1, 120), Vector3.new(0, -0.5, 0))
		-- a SpotLight looking down
		local _, Spot = Lamp("Spot", Vector3.new(-24, 10, 0), "SpotLight",
			{ Range = 20, Brightness = 4, Angle = 40, Shadows = true, Face = Enum.NormalId.Bottom, Color = Color3.fromRGB(255, 200, 120) })
		Spot.Parent.Size = Vector3.new(1, 1, 1)
		for _, Radius in { 0, 3, 6, 9, 12 } do
			Sample(`spot-{Radius}`, Vector3.new(-24 + Radius, 0, 0))
		end
		Block("Pin", Vector3.new(1, 3, 1), Vector3.new(-22, 1.5, 4))
		Sample("spot-pin-shadow", Vector3.new(-21.4, 0, 5.2))
		-- a SurfaceLight on the -Z face of a panel standing on its edge
		local Panel = Block("Panel", Vector3.new(6, 6, 1), Vector3.new(0, 3, 10))
		local Surface = Instance.new("SurfaceLight")
		Surface.Face = Enum.NormalId.Front
		Surface.Range = 14
		Surface.Brightness = 4
		Surface.Angle = 60
		Surface.Color = Color3.fromRGB(150, 220, 255)
		Surface.Parent = Panel
		for _, Distance in { 1, 4, 8, 12 } do
			Sample(`surface-{Distance}`, Vector3.new(0, 0, 9.5 - Distance))
		end
		-- an unshadowed PointLight beside a wall one voxel thick: does its light reach past the wall
		Lamp("Open", Vector3.new(24, 3, 0), "PointLight", { Range = 12, Brightness = 4, Shadows = false })
		Block("Wall", Vector3.new(4, 8, 20), Vector3.new(30, 4, 0))
		Sample("open-near", Vector3.new(26, 0, 0))
		Sample("open-behind", Vector3.new(33, 0, 0))
		Sample("open-far", Vector3.new(18, 0, 0))
		-- a small light, Range 4
		Lamp("Small", Vector3.new(0, 1, -14), "PointLight", { Range = 4, Brightness = 8, Shadows = true, Color = Color3.fromRGB(255, 120, 120) })
		for _, Radius in { 0.5, 1.5, 3, 4.5 } do
			Sample(`small-{Radius}`, Vector3.new(Radius, 0, -14))
		end
		Capture("lightkinds", DARK, Camera(Vector3.new(0, 40, 40), Vector3.new(0, 0, -2), 60))
	elseif Station == "bloom" then
		Block("Floor", Vector3.new(120, 1, 120), Vector3.new(0, -0.5, 0), Color3.fromRGB(60, 60, 60))
		for _, Bar in { { "white", -12, Color3.fromRGB(255, 255, 255) }, { "red", 0, Color3.fromRGB(255, 40, 40) }, { "cyan", 12, Color3.fromRGB(40, 200, 255) } } do
			Block(`Neon-{Bar[1]}`, Vector3.new(1, 1, 12), Vector3.new(Bar[2], 0.5, 0), Bar[3], Enum.Material.Neon)
			Sample(`{Bar[1]}-bar`, Vector3.new(Bar[2], 1, 0))
			for _, Distance in { 0.8, 1.5, 3, 6 } do
				Sample(`{Bar[1]}-halo-{Distance}`, Vector3.new(Bar[2] + 0.5 + Distance, 0, 0))
			end
		end
		Block("Speck", Vector3.new(0.4, 0.4, 0.4), Vector3.new(24, 0.2, 0), Color3.fromRGB(255, 255, 255), Enum.Material.Neon)
		Sample("speck-halo-1", Vector3.new(25, 0, 0))
		Block("Plate", Vector3.new(6, 0.2, 6), Vector3.new(-26, 0.1, 0), Color3.fromRGB(255, 255, 255))
		Lamp("Hot", Vector3.new(-26, 3, 0), "PointLight", { Range = 8, Brightness = 40, Shadows = false })
		Sample("plate", Vector3.new(-26, 0.2, 0))
		Sample("plate-halo-2", Vector3.new(-26, 0, 5))
		Sample("floor-far", Vector3.new(0, 0, 25))
		local View = Camera(Vector3.new(0, 30, 30), Vector3.new(0, 0, 0), 50)
		Capture("bloom-none", DARK, View)
		Capture("bloom-default", DARK, View, { { Class = "BloomEffect", Intensity = 1, Size = 24, Threshold = 2 } })
		Capture("bloom-low", DARK, View, { { Class = "BloomEffect", Intensity = 1, Size = 24, Threshold = 0.8 } })
	elseif Station == "ao" then
		local AMBIENT = With(DARK, { Ambient = Color3.fromRGB(150, 150, 150), OutdoorAmbient = Color3.new(0, 0, 0) })
		Block("Floor", Vector3.new(100, 1, 100), Vector3.new(0, -0.5, 0), Color3.fromRGB(220, 220, 220))
		Block("Box", Vector3.new(8, 8, 8), Vector3.new(-12, 4, 0), Color3.fromRGB(220, 220, 220))
		for _, Distance in { 0.25, 0.5, 1, 2, 4, 8 } do
			Sample(`box-{Distance}`, Vector3.new(-12, 0, 4 + Distance))
		end
		Block("WallX", Vector3.new(16, 8, 1), Vector3.new(14, 4, -8), Color3.fromRGB(220, 220, 220))
		Block("WallZ", Vector3.new(1, 8, 16), Vector3.new(6.5, 4, -0.5), Color3.fromRGB(220, 220, 220))
		for _, Distance in { 0.25, 0.5, 1, 2, 4 } do
			Sample(`corner-{Distance}`, Vector3.new(7 + Distance, 0, -7.5 + Distance))
		end
		Block("Step", Vector3.new(8, 1, 8), Vector3.new(0, 0.5, 14), Color3.fromRGB(220, 220, 220))
		Sample("step-0.5", Vector3.new(4.5, 0, 14))
		Sample("floor-far", Vector3.new(30, 0, 20))
		Capture("ao", AMBIENT, Camera(Vector3.new(0, 20, 30), Vector3.new(0, 2, 0), 50))
	elseif Station == "far" then
		-- pillars floating at eye height (their nearest point is at the distance named), spread across the view
		local Distances = { 280, 320, 440, 460, 480, 540, 560, 580, 595, 610, 680, 720, 1100, 1160, 1550, 1650,
			2200, 2330, 3100, 3300, 4400, 4650, 6200, 6600, 8800, 9300, 12000, 20000 }
		for Index, Distance in Distances do
			local Angle = math.rad(-42 + 84 * (Index - 1) / (#Distances - 1))
			local Width = 0.025 * Distance
			local Centre = Distance + Width / 2
			local Position = Vector3.new(math.sin(Angle) * Centre, 50, -math.cos(Angle) * Centre)
			local Pillar = Block(`Pillar{Distance}`, Vector3.new(Width, 0.25 * Distance, Width),
				CFrame.lookAt(Position, Vector3.new(0, 50, 0)), Color3.fromRGB(30, 30, 30))
			Pillar.CastShadow = false
			Sample(`pillar-{Distance}`, Position)
		end
		Capture("far", DAY, Camera(Vector3.new(0, 50, 0), Vector3.new(0, 50, -1000), 70))
	elseif Station == "tiny" then
		local Sizes = { 0.05, 0.07, 0.09, 0.11, 0.13, 0.15, 0.18, 0.21, 0.24, 0.27, 0.3, 0.34, 0.38, 0.43, 0.48, 0.54, 0.6 }
		for Index, Size in Sizes do
			local Position = Vector3.new(-16 + 32 * (Index - 1) / (#Sizes - 1), 50, -120)
			local Cube = Block(`Cube{Size}`, Vector3.new(Size, Size, Size), Position, Color3.fromRGB(255, 255, 255))
			Cube.CastShadow = false
			Sample(`cube-{Size}`, Position)
		end
		local LIT = With(DAY, { Sky = "dark", Ambient = Color3.fromRGB(255, 255, 255), OutdoorAmbient = Color3.fromRGB(255, 255, 255) })
		Capture("tiny-70", LIT, Camera(Vector3.new(0, 50, 0), Vector3.new(0, 50, -120), 70))
		Capture("tiny-8", LIT, Camera(Vector3.new(0, 50, 0), Vector3.new(0, 50, -120), 8))
	elseif Station == "fx" then
		local function Emitter(Name, Position, Size, Props)
			local Plate = Block(Name, Vector3.new(Size, 0.2, Size), Position)
			Plate.Transparency = 1
			Plate.CastShadow = false
			local Emit = Instance.new("ParticleEmitter")
			Emit.Lifetime = NumberRange.new(20)
			Emit.Speed = NumberRange.new(0)
			Emit.Rotation = NumberRange.new(0)
			Emit.RotSpeed = NumberRange.new(0)
			Emit.LightEmission = 1
			Emit.LightInfluence = 0
			Emit.Color = ColorSequence.new(Color3.fromRGB(255, 255, 255))
			Emit.Transparency = NumberSequence.new(0)
			Emit.Texture = "rbxasset://textures/particles/sparkles_main.dds"
			for Key, Value in Props do
				Emit[Key] = Value
			end
			Emit.Parent = Plate
			return Emit
		end
		local Burst = Emitter("Burst", Vector3.new(-17, 0, 0), 22, { Rate = 0, Size = NumberSequence.new(0.5) })
		Emitter("Rate30", Vector3.new(17, 0, 0), 22, { Rate = 30, Size = NumberSequence.new(0.5) })
		Emitter("Rate300", Vector3.new(0, 0, 24), 20, { Rate = 300, Size = NumberSequence.new(0.3) })
		table.insert(After, function()
			Burst:Emit(300)
		end)
		local A = Instance.new("Attachment")
		A.Parent = Block("BeamStart", Vector3.new(0.2, 0.2, 0.2), Vector3.new(-24, 2, -22))
		local B = Instance.new("Attachment")
		B.Parent = Block("BeamEnd", Vector3.new(0.2, 0.2, 0.2), Vector3.new(24, 2, -22))
		for _, End in { A.Parent, B.Parent } do
			End.Transparency = 1
		end
		-- the curve bends along each attachment's axis: turned to +Z, the S lies flat, seen from above
		A.Axis = Vector3.new(0, 0, 1)
		B.Axis = Vector3.new(0, 0, 1)
		local Beam = Instance.new("Beam")
		Beam.Attachment0 = A
		Beam.Attachment1 = B
		Beam.CurveSize0 = 14
		Beam.CurveSize1 = -14
		Beam.Segments = 20
		Beam.Width0 = 1
		Beam.Width1 = 1
		Beam.FaceCamera = true
		Beam.LightEmission = 1
		Beam.LightInfluence = 0
		Beam.Color = ColorSequence.new(Color3.fromRGB(255, 200, 80))
		Beam.Parent = Root
		Sample("burst", Vector3.new(-17, 0, 0))
		Sample("rate30", Vector3.new(17, 0, 0))
		Sample("rate300", Vector3.new(0, 0, 24))
		Sample("beam-middle", Vector3.new(0, 2, -22))
		Capture("fx", BLACK, Camera(Vector3.new(0, 70, 0.01), Vector3.new(0, 0, 0), 60))
	elseif Station == "post" then
		Block("Floor", Vector3.new(800, 1, 800), Vector3.new(-200, -0.5, 0))
		-- the view looks along the sun's bearing, 12 degrees up
		local Ahead = Vector3.new(Sun.X, 0, Sun.Z).Unit
		local Side = Vector3.new(-Ahead.Z, 0, Ahead.X)
		for Index, Distance in { 10, 20, 40, 80, 160 } do
			local Offset = if Index % 2 == 1 then -1 else 1
			local Base = Ahead * Distance + Side * Offset * (2 + Distance * 0.15)
			Block(`Column{Distance}`, Vector3.new(2, 10, 2), Base + Vector3.new(0, 5, 0), Color3.fromRGB(230, 230, 230))
			Sample(`column-{Distance}`, Base + Vector3.new(0, 4, 0))
		end
		Sample("floor-near", Ahead * 6 + Vector3.new(0, 0, 0))
		local Eye = Vector3.new(0, 6, 0)
		local View = Camera(Eye, Eye + Ahead * 100 + Vector3.new(0, math.tan(math.rad(12)) * 100, 0), 70)
		Capture("post-none", DAY, View)
		Capture("post-bloom", DAY, View, { { Class = "BloomEffect", Intensity = 1, Size = 24, Threshold = 0.8 } })
		Capture("post-blur", DAY, View, { { Class = "BlurEffect", Size = 6 } })
		Capture("post-dof", DAY, View, { { Class = "DepthOfFieldEffect", FarIntensity = 0.75, FocusDistance = 15, InFocusRadius = 8, NearIntensity = 0.5 } })
		Capture("post-sunrays", DAY, View, { { Class = "SunRaysEffect", Intensity = 0.25, Spread = 1 } })
		Capture("post-cc", DAY, View, { { Class = "ColorCorrectionEffect", Brightness = 0.05, Contrast = 0.3, Saturation = -0.6, TintColor = Color3.fromRGB(255, 225, 190) } })
		Capture("post-grading", DAY, View, { { Class = "ColorGradingEffect" } })
		Capture("post-atmosphere", DAY, View, { { Class = "Atmosphere", Density = 0.35, Offset = 0.25, Color = Color3.fromRGB(199, 170, 107), Decay = Color3.fromRGB(92, 60, 13), Glare = 0.5, Haze = 1.5 } })
		Capture("post-clouds", DAY, View, { { Class = "Clouds", Cover = 0.6, Density = 0.7 } })
	elseif Station == "materials" then
		local Materials = { "Plastic", "SmoothPlastic", "Wood", "WoodPlanks", "Brick", "Concrete", "Cobblestone", "Slate",
			"Granite", "Marble", "Pebble", "Sand", "Grass", "Fabric", "Metal", "DiamondPlate", "CorrodedMetal", "Foil", "Ice", "Neon" }
		for Index, Name in Materials do
			local Column = (Index - 1) % 5
			local Row = (Index - 1) // 5
			local Position = Vector3.new(-14 + Column * 7, 0.5, -14 + Row * 7)
			Block(Name, Vector3.new(6, 1, 6), Position, GREY, Enum.Material[Name])
			Sample(`material-{Name}`, Position + Vector3.new(0, 0.5, 0))
		end
		for Index, Name in { "Metal", "Foil", "SmoothPlastic", "Glass" } do
			local Ball = Block(`Ball{Name}`, Vector3.new(4, 4, 4), Vector3.new(24, 2, -12 + (Index - 1) * 6), Color3.fromRGB(200, 200, 200), Enum.Material[Name])
			Ball.Shape = Enum.PartType.Ball
			Ball.Reflectance = if Name == "SmoothPlastic" then 0.5 else 0
			Sample(`ball-{Name}`, Ball.Position + Vector3.new(0, 1.2, 1.2))
		end
		-- stripes behind a Glass pane
		for Index = 0, 5 do
			Block(`Stripe{Index}`, Vector3.new(1, 6, 0.4), Vector3.new(-26 + Index * 2, 3, 6),
				if Index % 2 == 0 then Color3.fromRGB(220, 40, 40) else Color3.fromRGB(40, 40, 220))
		end
		local Pane = Block("Glass", Vector3.new(12, 6, 0.3), Vector3.new(-21, 3, 9), Color3.fromRGB(200, 230, 255), Enum.Material.Glass)
		Pane.Transparency = 0.5
		Sample("glass-stripe", Vector3.new(-26, 3, 9.2))
		local Decorated = Block("Decorated", Vector3.new(6, 6, 1), Vector3.new(-21, 3, -20), Color3.fromRGB(240, 240, 240))
		local Decal = Instance.new("Decal")
		Decal.Texture = "rbxasset://textures/face.png"
		Decal.Face = Enum.NormalId.Back
		Decal.Parent = Decorated
		local Textured = Block("Textured", Vector3.new(6, 6, 1), Vector3.new(-21, 3, -13), Color3.fromRGB(240, 240, 240))
		local Texture = Instance.new("Texture")
		Texture.Texture = "rbxasset://textures/grid4.png"
		Texture.StudsPerTileU = 2
		Texture.StudsPerTileV = 2
		Texture.Face = Enum.NormalId.Back
		Texture.Parent = Textured
		Sample("decal", Vector3.new(-21, 3, -19.4))
		Sample("texture", Vector3.new(-21, 3, -12.4))
		local Lit = Block("Highlighted", Vector3.new(4, 4, 4), Vector3.new(24, 2, 14), Color3.fromRGB(120, 200, 120))
		local Highlight = Instance.new("Highlight")
		Highlight.FillColor = Color3.fromRGB(255, 60, 200)
		Highlight.FillTransparency = 0.5
		Highlight.OutlineColor = Color3.fromRGB(255, 255, 0)
		Highlight.Adornee = Lit
		Highlight.Parent = Lit
		Sample("highlight", Lit.Position + Vector3.new(0, 2, 0))
		-- a SurfaceAppearance on a block-shaped mesh card: an EditableImage checker of two colours
		local ok = pcall(function()
			local Image = AssetService:CreateEditableImage({ Size = Vector2.new(64, 64) })
			local Pixels = buffer.create(64 * 64 * 4)
			for Y = 0, 63 do
				for X = 0, 63 do
					local Index = (Y * 64 + X) * 4
					local Light = (X // 8 + Y // 8) % 2 == 0
					buffer.writeu8(Pixels, Index, if Light then 240 else 40)
					buffer.writeu8(Pixels, Index + 1, if Light then 200 else 60)
					buffer.writeu8(Pixels, Index + 2, if Light then 60 else 160)
					buffer.writeu8(Pixels, Index + 3, 255)
				end
			end
			Image:WritePixelsBuffer(Vector2.zero, Image.Size, Pixels)
			local Card = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
			Card.Name = "Appearance"
			Card.Anchored = true
			Card.Size = Vector3.new(6, 6, 0.001)
			Card.CFrame = CFrame.new(24, 0.6, 20) * CFrame.Angles(-math.pi / 2, 0, 0) * CFrame.new(-3, -3, 0)
			local Surface = AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Image) })
			Surface.Parent = Card
			Card.Parent = Root
		end)
		if ok then
			Sample("appearance", Vector3.new(24, 0.6, 20))
		end
		Capture("materials", DAY, Camera(Vector3.new(0, 34, 40), Vector3.new(0, 0, -2), 55))
		-- close: four plates 4 x 4 from 9 studs
		for Index, Name in { "Brick", "Wood", "Grass", "Cobblestone" } do
			local Position = Vector3.new(60 + ((Index - 1) % 2) * 4.2, 0.25, ((Index - 1) // 2) * 4.2)
			Block(`Close{Name}`, Vector3.new(4, 0.5, 4), Position, GREY, Enum.Material[Name])
			Sample(`close-{Name}`, Position + Vector3.new(0, 0.25, 0))
		end
		Capture("materials-close", DAY, Camera(Vector3.new(62.1, 8, 6.5), Vector3.new(62.1, 0, 2.1), 50))
	elseif Station == "coverage" then
		-- rails 1.5 studs square, 6 studs up, running away from the camera: each casts a strip on the floor
		-- 13 studs to its +X (and 4 along -Z); where a strip ends, the sun's shadow map ends
		Block("Floor", Vector3.new(400, 1, 1400), Vector3.new(0, -0.5, -680))
		for Index = -3, 3 do
			local X = Index * 24
			Block(`Rail{Index}`, Vector3.new(1.5, 1.5, 1310), Vector3.new(X, 6.75, -645))
			for _, Z in { -20, -60, -100, -200, -400 } do
				Sample(`strip{Index}-{-Z}`, Shadow(Vector3.new(X, 6.75, Z)))
			end
		end
		Capture("coverage-a", DAY, Camera(Vector3.new(0, 30, 30), Vector3.new(0, 0, -150), 50))
		Capture("coverage-b", DAY, Camera(Vector3.new(0, 120, 60), Vector3.new(0, 0, -60), 60))
		Capture("coverage-c", DAY, Camera(Vector3.new(0, 12, 30), Vector3.new(0, 6, -500), 70))
	elseif Station == "lightline" then
		-- shadowed PointLights every 20 studs along a 2 x 2 rail: per pixel, the rail's shadow runs along the
		-- floor at x 4 to 10; through the voxels it does not
		Block("Floor", Vector3.new(120, 1, 900), Vector3.new(0, -0.5, -430))
		for Index = 0, 21 do
			local Z = -6 - Index * 20
			Lamp(`Lamp{Index}`, Vector3.new(0, 5, Z), "PointLight", { Range = 14, Brightness = 3, Shadows = true })
			Sample(`shadow-{-Z}`, Vector3.new(7, 0, Z))
			Sample(`open-{-Z}`, Vector3.new(-7, 0, Z))
		end
		Block("Rail", Vector3.new(2, 2, 460), Vector3.new(3, 2, -220))
		Capture("lightline-a", DARK, Camera(Vector3.new(-15, 30, 25), Vector3.new(0, 0, -100), 50))
		Capture("lightline-b", DARK, Camera(Vector3.new(-15, 70, 40), Vector3.new(0, 0, -40), 60))
	elseif Station == "hqline" then
		-- strips of Metal, Foil, Brick and SmoothPlastic 800 studs long, running away from the camera
		Block("Floor", Vector3.new(200, 1, 900), Vector3.new(0, -0.6, -430))
		for Index, Name in { "Metal", "Foil", "Brick", "SmoothPlastic" } do
			local X = -24 + (Index - 1) * 16
			Block(`Strip{Name}`, Vector3.new(14, 0.2, 800), Vector3.new(X, 0, -390), GREY, Enum.Material[Name])
			for _, Z in { -10, -40, -80, -160, -320 } do
				Sample(`{Name}-{-Z}`, Vector3.new(X, 0.1, Z))
			end
		end
		Capture("hqline", DAY, Camera(Vector3.new(0, 25, 20), Vector3.new(0, 0, -150), 50))
		Capture("hqline-b", DAY, Camera(Vector3.new(0, 90, 50), Vector3.new(0, 0, -60), 60))
	elseif Station == "voxsun" then
		-- casters on world multiples of 4 (the light grid's voxels) under the sun alone, seen from straight
		-- above: far up (900 studs, past every shadow map) and near (150 studs)
		Block("Floor", Vector3.new(280, 1, 180), Vector3.new(-10, -0.5, -8), Color3.fromRGB(200, 200, 200))
		local Casters = {
			{ "cube4", Vector3.new(4, 4, 4), Vector3.new(-86, 2, -38) },
			{ "cube8", Vector3.new(8, 8, 8), Vector3.new(-52, 4, -36) },
			{ "cube16", Vector3.new(16, 16, 16), Vector3.new(-8, 8, -32) },
			{ "slab8", Vector3.new(16, 1, 16), Vector3.new(48, 8.5, -32) },
			{ "slab24", Vector3.new(16, 1, 16), Vector3.new(-80, 24.5, 24) },
			{ "wall", Vector3.new(1, 12, 24), Vector3.new(-3.5, 6, 20) },
			{ "column", Vector3.new(4, 12, 4), Vector3.new(50, 6, 18) },
		}
		for _, Caster in Casters do
			Block(Caster[1], Caster[2], Caster[3])
			Sample(Caster[1], Caster[3] - Vector3.new(0, Caster[3].Y, 0))
		end
		local Far = Camera(Vector3.new(-10, 900, -7.99), Vector3.new(-10, 0, -8), 9)
		local Near = Camera(Vector3.new(-10, 150, -7.99), Vector3.new(-10, 0, -8), 50)
		Capture("voxsun-far", SUN_ONLY, Far)
		Capture("voxsun-near", SUN_ONLY, Near)
		Capture("voxsun-high", With(SUN_ONLY, { ClockTime = 13 }), Far)
		Capture("voxsun-sky", SKY_ONLY, Far)
	elseif Station == "voxlight" then
		-- lights over a white floor in the dark, from straight above: near (80 studs) and far (800 studs,
		-- past where any level lights per pixel)
		Block("Floor", Vector3.new(200, 1, 140), Vector3.new(0, -0.5, 0), Color3.fromRGB(230, 230, 230))
		Lamp("Point8", Vector3.new(-54, 2, -30), "PointLight", { Range = 8, Brightness = 2 })
		Lamp("Point16", Vector3.new(-14, 6, -30), "PointLight", { Range = 16, Brightness = 2 })
		Lamp("Point16off", Vector3.new(29, 6, -29), "PointLight", { Range = 16, Brightness = 2 })
		Lamp("Point32", Vector3.new(66, 10, 2), "PointLight", { Range = 32, Brightness = 1 })
		local _, Spot = Lamp("Spot", Vector3.new(-50, 10, 26), "SpotLight", { Range = 20, Brightness = 2, Angle = 45, Face = Enum.NormalId.Bottom })
		Spot.Parent.Size = Vector3.new(1, 1, 1)
		local Panel = Block("Panel", Vector3.new(8, 6, 1), Vector3.new(-10, 3, 34))
		local Surface = Instance.new("SurfaceLight")
		Surface.Face = Enum.NormalId.Front
		Surface.Range = 16
		Surface.Brightness = 2
		Surface.Angle = 90
		Surface.Parent = Panel
		for _, Name in { "Point8", "Point16", "Point16off", "Point32", "Spot" } do
			local Carrier = Root:FindFirstChild(Name)
			Sample(Name, Carrier.Position - Vector3.new(0, Carrier.Position.Y, 0))
		end
		Capture("voxlight-near", BLACK, Camera(Vector3.new(0, 80, 0.01), Vector3.new(0, 0, 0), 70))
		Capture("voxlight-far", BLACK, Camera(Vector3.new(0, 800, 0.01), Vector3.new(0, 0, 0), 8))
	elseif Station == "farfine1" or Station == "farfine2" then
		-- upright pillars, not turned (their boxes are the world boxes), at eye height, each 2 % of its
		-- distance wide; the distance named is from the eye to the nearest point of the box
		local Lists = {
			farfine1 = { 490, 510, 530, 760, 850, 950, 1050, 1200, 1300, 1400, 1500, 1570, 1610, 2230, 2270, 2310, 2400,
				2600, 2800, 3000, 3400, 3700, 4000, 4300, 4800, 5400, 6000, 9600, 10400, 11200, 13000, 15000, 17000, 19000 },
			farfine2 = { 500, 520, 545, 800, 900, 1000, 1100, 1250, 1350, 1450, 1540, 1590, 1630, 2250, 2290, 2330, 2500,
				2700, 2900, 3150, 3550, 3850, 4150, 4500, 5100, 5700, 6400, 9000, 10000, 10800, 11600, 14000, 16000, 18000 },
		}
		local Distances = Lists[Station]
		for Index, Distance in Distances do
			local Angle = math.rad(-44 + 88 * (Index - 1) / (#Distances - 1))
			local Width = 0.02 * Distance
			local Direction = Vector3.new(math.sin(Angle), 0, -math.cos(Angle))
			-- move the box along its bearing until its nearest point to the eye is Distance off
			local Half = Width / 2
			local Centre = Direction * Distance
			for _ = 1, 30 do
				local Nearest = Vector3.new(math.clamp(0, Centre.X - Half, Centre.X + Half), 0, math.clamp(0, Centre.Z - Half, Centre.Z + Half))
				Centre += Direction * (Distance - Nearest.Magnitude)
			end
			local Pillar = Block(`Pillar{Distance}`, Vector3.new(Width, 0.25 * Distance, Width), Centre + Vector3.new(0, 50, 0), Color3.fromRGB(30, 30, 30))
			Pillar.CastShadow = false
			Sample(`pillar-{Distance}`, Centre + Vector3.new(0, 50, 0))
		end
		Capture(Station, DAY, Camera(Vector3.new(0, 50, 0), Vector3.new(0, 50, -1000), 70))
	elseif Station == "atmo" then
		-- an Atmosphere and a graded tone curve, given longer to settle
		Block("Floor", Vector3.new(2000, 1, 2000), Vector3.new(0, -0.5, -900))
		for Index, Distance in { 30, 60, 120, 240, 480, 960 } do
			local Side = (Index % 2 * 2 - 1) * 0.3 * Distance
			Block(`Column{Distance}`, Vector3.new(0.06 * Distance, 0.3 * Distance, 0.06 * Distance),
				Vector3.new(Side, 0.15 * Distance, -Distance), Color3.fromRGB(230, 230, 230))
			Sample(`column-{Distance}`, Vector3.new(Side, 0.1 * Distance, -Distance))
		end
		local View = Camera(Vector3.new(0, 10, 20), Vector3.new(0, 30, -500), 70)
		-- a client draws an Atmosphere only under a Sky instance (the default sky without one shows none)
		local Slow = With(DAY, { Settle = 4, Sky = "instance" })
		Capture("atmo-none", Slow, View)
		Capture("atmo-on", Slow, View, { { Class = "Atmosphere", Density = 0.35, Offset = 0.25, Color = Color3.fromRGB(199, 170, 107), Decay = Color3.fromRGB(92, 60, 13), Glare = 0.5, Haze = 1.5 } })
		Capture("atmo-retro", Slow, View, { { Class = "ColorGradingEffect", TonemapperPreset = Enum.TonemapperPreset.Retro } })
	else
		error("quality-levels: no station " .. tostring(Station))
	end

	Root.Parent = workspace
	for _, Step in After do
		Step()
	end
	return { Station = Station, Samples = Samples, Captures = Captures, Root = Root }
end
