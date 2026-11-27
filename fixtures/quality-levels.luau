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
--   shading     plates and balls of the specular materials under the sun, the sky and both, against the
--               sun from 30 studs and from 520 (past the high-quality shading below level 15)
--   kinds       white plates 1 to 8 studs wide, cards and shapes in the sun's glare: which a level lights by vertex
--   backs       balls, blocks, plates, a slab, a cylinder and a wedge 40 studs up, seen from the side away from the
--               sun: the light faces turned from the sun take, within the shadow maps and past them
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
	-- Normal (optional): the surface's normal there, for analyses by the angle to the sun
	local function Sample(Name, Point, Normal)
		table.insert(Samples, { Name = Name, Point = Point, Normal = Normal })
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
	elseif Station == "shading" then
		-- plates and balls of the specular materials under the sun alone, the sky alone and both, seen
		-- against the sun from close by (within every level's high-quality shading) and from 520 studs
		-- (past it below level 15), so a level's shading model reads off the same parts near and far
		Block("Floor", Vector3.new(200, 1, 200), Vector3.new(0, -0.5, 0), Color3.fromRGB(120, 120, 120))
		local Plates = { "Metal", "Foil", "DiamondPlate", "CorrodedMetal", "SmoothPlastic", "Plastic", "Brick", "Marble", "Slate" }
		for Index, Name in Plates do
			local Position = Vector3.new(0, 0.2, -24 + (Index - 1) * 6)
			Block(`Plate{Name}`, Vector3.new(5, 0.4, 5), Position, GREY, Enum.Material[Name])
			Sample(`plate-{Name}`, Position + Vector3.new(0, 0.2, 0))
		end
		local Balls = {
			{ "Metal", GREY, 0 }, { "MetalRed", Color3.fromRGB(200, 40, 40), 0 }, { "Foil", GREY, 0 },
			{ "SmoothPlastic", Color3.fromRGB(230, 230, 230), 0 }, { "Reflectance", Color3.fromRGB(230, 230, 230), 0.5 },
			{ "Plastic", Color3.fromRGB(230, 230, 230), 0 },
		}
		for Index, Spec in Balls do
			local Material = if Spec[1] == "MetalRed" then "Metal" elseif Spec[1] == "Reflectance" then "SmoothPlastic" else Spec[1]
			local Position = Vector3.new(-12, 2, -18 + (Index - 1) * 7)
			local Ball = Block(`Ball{Spec[1]}`, Vector3.new(4, 4, 4), Position, Spec[2], Enum.Material[Material])
			Ball.Shape = Enum.PartType.Ball
			Ball.Reflectance = Spec[3]
			-- the side towards the camera and the top
			Sample(`ball-{Spec[1]}-side`, Position + Vector3.new(1.7, 0.4, 0))
			Sample(`ball-{Spec[1]}-top`, Position + Vector3.new(0.4, 1.9, 0))
		end
		Sample("floor-near", Vector3.new(-12, 0, 0))
		Sample("floor-glare", Vector3.new(0, 0, 0))
		-- the camera looks along the sun's bearing (towards it), 26 degrees down: the sun's glare on the
		-- plates lies near the view's middle
		local Ahead = Vector3.new(Sun.X, 0, Sun.Z).Unit
		local Down = math.rad(26)
		local Back = (-Ahead * math.cos(Down) + Vector3.new(0, math.sin(Down), 0))
		local Near = Camera(Back * 30, Vector3.new(0, 0, 0), 70)
		local Far = Camera(Back * 520, Vector3.new(0, 0, 0), 7)
		-- the sky alone, its diffuse light and its specular (no sun, no flat ambient)
		local SKY = With(DAY, { Brightness = 0, Ambient = Color3.new(0, 0, 0), OutdoorAmbient = Color3.new(0, 0, 0) })
		Capture("shading-day-near", DAY, Near)
		Capture("shading-day-far", DAY, Far)
		Capture("shading-sun-near", SUN_ONLY, Near)
		Capture("shading-sun-far", SUN_ONLY, Far)
		Capture("shading-sky-near", SKY, Near)
		Capture("shading-sky-far", SKY, Far)
		-- across the sun (it comes from the left), from close by
		Capture("shading-day-side", DAY, Camera(Vector3.new(-2, 18, 34), Vector3.new(0, 0, -2), 70))
	elseif Station == "kinds" then
		-- white SmoothPlastic in the sun's glare, as the shading station sees it: plates 1 to 8 studs wide, a
		-- MeshPart card and a SpecialMesh card on the glare's line, and a block, ball, cylinder, wedge and a
		-- block with a SpecialMesh sphere behind them, to tell which kinds of part a level lights by vertex
		Block("Floor", Vector3.new(200, 1, 200), Vector3.new(0, -0.5, 0), Color3.fromRGB(120, 120, 120))
		local WHITE = Color3.fromRGB(230, 230, 230)
		for Index, Width in { 1, 2, 4, 8 } do
			local Position = Vector3.new(0, 0.1, -20 + (Index - 1) * 8)
			Block(`Plate{Width}`, Vector3.new(Width, 0.2, Width), Position, WHITE)
			Sample(`plate-{Width}`, Position + Vector3.new(0, 0.1, 0))
		end
		local ok = pcall(function()
			local Card = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
			Card.Name = "Card"
			Card.Anchored = true
			Card.Size = Vector3.new(4, 4, 0.001)
			Card.Color = WHITE
			Card.Material = Enum.Material.SmoothPlastic
			Card.CFrame = CFrame.new(0, 0.05, 12) * CFrame.Angles(-math.pi / 2, 0, 0) * CFrame.new(-2, -2, 0)
			Card.Parent = Root
		end)
		if ok then
			Sample("meshpart-card", Vector3.new(0, 0.05, 12))
		end
		local Holder = Block("SpecialCard", Vector3.new(4, 0.2, 4), Vector3.new(0, 0.1, 20), WHITE)
		local Special = Instance.new("SpecialMesh")
		Special.MeshType = Enum.MeshType.Brick
		Special.Parent = Holder
		Sample("specialmesh-brick", Vector3.new(0, 0.2, 20))
		local Shapes = { { "Block", Enum.PartType.Block }, { "Ball", Enum.PartType.Ball }, { "Cylinder", Enum.PartType.Cylinder }, { "Wedge", nil }, { "SphereMesh", nil } }
		for Index, Spec in Shapes do
			local Position = Vector3.new(-12, 2, -16 + (Index - 1) * 8)
			local Part
			if Spec[1] == "Wedge" then
				Part = Instance.new("WedgePart")
				Part.Anchored = true
				Part.Size = Vector3.new(4, 4, 4)
				-- its slope towards the camera (+X)
				Part.CFrame = CFrame.new(Position) * CFrame.Angles(0, -math.pi / 2, 0)
				Part.Color = WHITE
				Part.Material = Enum.Material.SmoothPlastic
				Part.TopSurface = Enum.SurfaceType.Smooth
				Part.BottomSurface = Enum.SurfaceType.Smooth
				Part.Parent = Root
			else
				Part = Block(Spec[1], Vector3.new(4, 4, 4), Position, WHITE)
				if Spec[2] then
					Part.Shape = Spec[2]
				end
				if Spec[1] == "SphereMesh" then
					local Mesh = Instance.new("SpecialMesh")
					Mesh.MeshType = Enum.MeshType.Sphere
					Mesh.Parent = Part
				end
			end
			Sample(`{Spec[1]}-top`, Position + Vector3.new(0.4, 1.9, 0))
			Sample(`{Spec[1]}-side`, Position + Vector3.new(1.9, 0.4, 0))
		end
		local Ahead = Vector3.new(Sun.X, 0, Sun.Z).Unit
		local Down = math.rad(26)
		local Back = (-Ahead * math.cos(Down) + Vector3.new(0, math.sin(Down), 0))
		local SKY = With(DAY, { Brightness = 0, Ambient = Color3.new(0, 0, 0), OutdoorAmbient = Color3.new(0, 0, 0) })
		Capture("kinds-sun", SUN_ONLY, Camera(Back * 30, Vector3.new(0, 0, 0), 70))
		Capture("kinds-sky", SKY, Camera(Back * 30, Vector3.new(0, 0, 0), 70))
		Capture("kinds-day", DAY, Camera(Back * 30, Vector3.new(0, 0, 0), 70))
		-- which of the day's terms take the light faces turned from the sun get under it alone (sun-* variants)
		if Context and Context.Terms then
			local View = Camera(Back * 30, Vector3.new(0, 0, 0), 70)
			Capture("kinds-sun-envd1", With(SUN_ONLY, { EnvironmentDiffuseScale = 1 }), View)
			Capture("kinds-sun-envd05", With(SUN_ONLY, { EnvironmentDiffuseScale = 0.5 }), View)
			Capture("kinds-sun-envd01", With(SUN_ONLY, { EnvironmentDiffuseScale = 0.1 }), View)
			Capture("kinds-sun-envs1", With(SUN_ONLY, { EnvironmentSpecularScale = 1 }), View)
			Capture("kinds-sun-amb", With(SUN_ONLY, { Ambient = Color3.fromRGB(50, 50, 50) }), View)
			Capture("kinds-sun-outdoor", With(SUN_ONLY, { OutdoorAmbient = Color3.fromRGB(128, 128, 128) }), View)
			Capture("kinds-day-envd0", With(DAY, { EnvironmentDiffuseScale = 0 }), View)
			Capture("kinds-sun-darksky", With(SUN_ONLY, { EnvironmentDiffuseScale = 1, Sky = "dark" }), View)
		end
	elseif Station == "backs" then
		-- faces turned away from the sun, in the open 40 studs up with no floor: white balls 2, 4 and 12 studs across,
		-- a red one and a Plastic one, blocks on and off the 4-stud voxel grid, a 12-stud block, plates 0.2 thick and a
		-- slab 2 thick turned so the side the camera sees takes the sun at n·l -0.25 to -0.95, a cylinder and a
		-- wedge, in a grid across the sun so no part stands in another's voxel shadow; and a ball, a block and a plate
		-- in a 24-stud block's shadow. Seen from between the side away from the sun and across it (both sides of the
		-- balls), from 70 studs (within every level's shadow maps from 12) and 700 (past them at every level)
		local WHITE = Color3.fromRGB(230, 230, 230)
		local Ahead = Vector3.new(Sun.X, 0, Sun.Z).Unit
		local Across = Vector3.new(Ahead.Z, 0, -Ahead.X)
		local Up = Sun:Cross(Across).Unit
		local Centre = Vector3.new(0, 40, 0)
		local Toward = (Across - Sun).Unit
		local function Slot(I, J)
			return Centre + Across * (20 * I) + Up * (20 * J)
		end
		local function Ball(Name, Diameter, Position, Colour, Material)
			local Part = Block(Name, Vector3.new(Diameter, Diameter, Diameter), Position, Colour or WHITE, Material)
			Part.Shape = Enum.PartType.Ball
			local R = Diameter / 2
			for Key, Normal in { view = Toward, deep = (Toward - Sun * 0.6).Unit, rim = (Toward - Sun * 1.6).Unit, lit = (Toward + Sun * 0.9).Unit } do
				Sample(`{Name}-{Key}`, Position + Normal * R, Normal)
			end
			return Part
		end
		local function Plate(Name, Size, Position, Cos)
			-- its top face's normal takes the sun at n·l = -Cos, turned towards the camera
			local Normal = (-Sun * Cos + Across * math.sqrt(1 - Cos * Cos)).Unit
			local Side = Normal:Cross(Up).Unit
			local Part = Block(Name, Size, CFrame.fromMatrix(Position, Side, Normal), WHITE)
			Sample(`{Name}-top`, Position + Normal * (Size.Y / 2), Normal)
			return Part
		end
		local function Snap(Point, Offset)
			local function S(V)
				return math.floor(V / 4 + 0.5) * 4 + Offset
			end
			return Vector3.new(S(Point.X), S(Point.Y), S(Point.Z))
		end
		local function Box(Name, Size, Position)
			local Part = Block(Name, Size, Position, WHITE)
			-- the faces the camera sees: +X and -Y turned from the sun, +Z towards it
			Sample(`{Name}-px`, Position + Vector3.new(Size.X / 2, 0, 0), Vector3.new(1, 0, 0))
			Sample(`{Name}-ny`, Position - Vector3.new(0, Size.Y / 2, 0), Vector3.new(0, -1, 0))
			Sample(`{Name}-pz`, Position + Vector3.new(0, 0, Size.Z / 2), Vector3.new(0, 0, 1))
			return Part
		end
		Ball("Ball4", 4, Slot(-2, 1))
		Ball("Ball2", 2, Slot(-1, 1))
		Ball("Ball12", 12, Slot(0, 1))
		Ball("BallRed", 4, Slot(1, 1), Color3.fromRGB(200, 40, 40))
		Ball("BallPlastic", 4, Slot(2, 1), WHITE, Enum.Material.Plastic)
		Box("Block4on", Vector3.new(4, 4, 4), Snap(Slot(-2, 0), 2))
		Box("Block4off", Vector3.new(4, 4, 4), Snap(Slot(-1, 0), 0))
		Box("Block12", Vector3.new(12, 12, 12), Snap(Slot(0, 0), 2))
		local Cylinder = Block("Cylinder", Vector3.new(4, 4, 4), Slot(1, 0), WHITE)
		Cylinder.Shape = Enum.PartType.Cylinder
		Sample("Cylinder-end", Slot(1, 0) + Vector3.new(2, 0, 0), Vector3.new(1, 0, 0))
		local Wedge = Instance.new("WedgePart")
		Wedge.Anchored = true
		Wedge.Size = Vector3.new(4, 4, 4)
		-- its slope faces +X and up, away from the sun
		Wedge.CFrame = CFrame.new(Slot(2, 0)) * CFrame.Angles(0, -math.pi / 2, 0)
		Wedge.Color = WHITE
		Wedge.Material = Enum.Material.SmoothPlastic
		Wedge.TopSurface = Enum.SurfaceType.Smooth
		Wedge.BottomSurface = Enum.SurfaceType.Smooth
		Wedge.Parent = Root
		Sample("Wedge-slope", Slot(2, 0), Vector3.new(1, 1, 0).Unit)
		for Index, Cos in { 0.25, 0.5, 0.75, 0.95 } do
			Plate(`Plate{math.floor(Cos * 100)}`, Vector3.new(10, 0.2, 10), Slot(Index - 3, -1), Cos)
		end
		Plate("Slab75", Vector3.new(10, 2, 10), Slot(2, -1), 0.75)
		-- in a 24-stud block's shadow: the block 28 studs towards the sun from the three, far across from the grid
		local Shade = Centre + Across * 110
		Block("Caster", Vector3.new(24, 24, 24), Shade + Sun * 28)
		Ball("ShadeBall", 4, Shade + Up * 6)
		Box("ShadeBlock", Vector3.new(4, 4, 4), Snap(Shade - Up * 2, 2))
		Plate("ShadePlate", Vector3.new(6, 0.2, 6), Shade - Across * 7 - Up * 4, 0.75)
		local NOGS = With(SUN_ONLY, { GlobalShadows = false })
		-- the focus at the view's target, as a look-at camera's here, near and far alike
		local function Camera(Eye, Target, Fov)
			return { Eye = Eye, Target = Target, Fov = Fov, Focus = "target" }
		end
		Capture("backs-sun", SUN_ONLY, Camera(Centre + Toward * 70, Centre, 70))
		Capture("backs-sun-far", SUN_ONLY, Camera(Centre + Toward * 700, Centre, 7))
		Capture("backs-nogs", NOGS, Camera(Centre + Toward * 70, Centre, 70))
		Capture("backs-shade", SUN_ONLY, Camera(Shade + Toward * 40, Shade, 50))
		Capture("backs-shade-far", SUN_ONLY, Camera(Shade + Toward * 700, Shade, 3))
		-- close by: the 4-stud ball and block, the 12-stud ball
		Capture("backs-ball4", SUN_ONLY, Camera(Slot(-2, 1) + Toward * 16, Slot(-2, 1), 30))
		Capture("backs-ball12", SUN_ONLY, Camera(Slot(0, 1) + Toward * 30, Slot(0, 1), 40))
		Capture("backs-day", DAY, Camera(Centre + Toward * 70, Centre, 70))
	elseif Station == "chunk" then
		-- 4-stud white balls in the middles of 64-stud cells of the world ((32, 32, 32) and its neighbours along X, Y and
		-- Z) and one across a cell's corner (64, 64, 64), seen from the side away from the sun: which focus positions
		-- leave a ball's far side the light it takes far from the focus
		local WHITE = Color3.fromRGB(230, 230, 230)
		local Toward = (Vector3.new(Sun.Z, 0, -Sun.X).Unit - Sun).Unit
		for _, Spec in { { "c", Vector3.new(32, 32, 32) }, { "x", Vector3.new(96, 32, 32) }, { "y", Vector3.new(32, 96, 32) }, { "z", Vector3.new(32, 32, 96) }, { "k", Vector3.new(64, 64, 64) } } do
			local Ball = Block(`Ball-{Spec[1]}`, Vector3.new(4, 4, 4), Spec[2], WHITE)
			Ball.Shape = Enum.PartType.Ball
			Sample(`Ball-{Spec[1]}-view`, Spec[2] + Toward * 2, Toward)
		end
		Capture("chunk-sun", SUN_ONLY, Camera(Vector3.new(64, 64, 64) + Toward * 160, Vector3.new(64, 64, 64), 40))
	elseif Station == "guiback" then
		-- a white frame on a SurfaceGui (LightInfluence 1) over half of a slab's face turned from the sun (n·l -0.75),
		-- 20 studs up with nothing below: the light a GUI and a part take from behind under EnvironmentDiffuseScale 0,
		-- 0.5 and 1 (a dark Sky, so the sky's own light is next to none)
		local Normal = (-Sun * 0.75 + Vector3.new(Sun.Z, 0, -Sun.X).Unit * math.sqrt(1 - 0.75 * 0.75)).Unit
		local Side = Normal:Cross(Vector3.yAxis).Unit
		local Middle = Vector3.new(0, 20, 0)
		local Slab = Block("Slab", Vector3.new(12, 0.4, 6), CFrame.fromMatrix(Middle, Side, Normal), Color3.fromRGB(230, 230, 230))
		local Gui = Instance.new("SurfaceGui")
		Gui.Face = Enum.NormalId.Top
		Gui.SizingMode = Enum.SurfaceGuiSizingMode.FixedSize
		Gui.CanvasSize = Vector2.new(200, 100)
		Gui.LightInfluence = 1
		Gui.Brightness = 1
		local Frame = Instance.new("Frame")
		Frame.BackgroundColor3 = Color3.fromRGB(230, 230, 230)
		Frame.BorderSizePixel = 0
		Frame.Size = UDim2.fromScale(0.5, 1)
		Frame.Parent = Gui
		Gui.Parent = Slab
		-- the canvas's left half (from the face's own top-left corner) and the bare right half
		local Top = Middle + Normal * 0.2
		Sample("gui-a", Top + Side * 3, Normal)
		Sample("gui-b", Top - Side * 3, Normal)
		local View = { Eye = Top + Normal * 30, Target = Top, Fov = 30, Focus = "target" }
		local DARKSUN = With(SUN_ONLY, { Sky = "dark" })
		Capture("guiback-e0", SUN_ONLY, View)
		Capture("guiback-e0-dark", DARKSUN, View)
		Capture("guiback-e05", With(DARKSUN, { EnvironmentDiffuseScale = 0.5 }), View)
		Capture("guiback-e1", With(DARKSUN, { EnvironmentDiffuseScale = 1 }), View)
	elseif Station == "cluster" then
		-- which parts share a ball's fill decision: a 4-stud white ball at (32, 32, z) in cells 256 studs apart along Z,
		-- each with one other part farther along X (none, a like ball, a Plastic block, a Neon block, a half transparent
		-- block, a MeshPart card, a ball across X = 128, a ball over Y = 64, a 40-stud bar from X 100 to 140, an
		-- unanchored ball); seen from the side away from the sun
		local WHITE = Color3.fromRGB(230, 230, 230)
		local Toward = (Vector3.new(Sun.Z, 0, -Sun.X).Unit - Sun).Unit
		local Others = { "none", "ball", "plastic", "neon", "glass", "mesh", "acrossx", "overy", "bar", "loose" }
		for Index, Kind in Others do
			local Z = 256 * (Index - 1) + 32
			local Probe = Vector3.new(32, 32, Z)
			local Ball = Block(`Probe-{Kind}`, Vector3.new(4, 4, 4), Probe, WHITE)
			Ball.Shape = Enum.PartType.Ball
			Sample(`Probe-{Kind}`, Probe + Toward * 2, Toward)
			local Far = Vector3.new(100, 32, Z + 68)
			if Kind == "ball" or Kind == "loose" then
				local Other = Block(`Other-{Kind}`, Vector3.new(4, 4, 4), Far, WHITE)
				Other.Shape = Enum.PartType.Ball
				Other.Anchored = Kind ~= "loose"
			elseif Kind == "plastic" then
				Block("Other-plastic", Vector3.new(4, 4, 4), Far, GREY, Enum.Material.Plastic)
			elseif Kind == "neon" then
				Block("Other-neon", Vector3.new(4, 4, 4), Far, WHITE, Enum.Material.Neon)
			elseif Kind == "glass" then
				Block("Other-glass", Vector3.new(4, 4, 4), Far, WHITE).Transparency = 0.5
			elseif Kind == "mesh" then
				pcall(function()
					local Card = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
					Card.Anchored = true
					Card.Size = Vector3.new(4, 4, 0.001)
					Card.CFrame = CFrame.new(Far) * CFrame.new(-2, -2, 0)
					Card.Parent = Root
				end)
			elseif Kind == "acrossx" then
				local Other = Block("Other-acrossx", Vector3.new(4, 4, 4), Vector3.new(140, 32, Z + 68), WHITE)
				Other.Shape = Enum.PartType.Ball
			elseif Kind == "overy" then
				local Other = Block("Other-overy", Vector3.new(4, 4, 4), Vector3.new(100, 70, Z + 68), WHITE)
				Other.Shape = Enum.PartType.Ball
			elseif Kind == "bar" then
				Block("Other-bar", Vector3.new(40, 4, 4), Vector3.new(120, 32, Z + 68), WHITE)
			end
			Capture(`cluster-{Kind}`, SUN_ONLY, Camera(Probe + Toward * 24, Probe, 30))
		end
	elseif Station == "bounce" then
		-- what lights faces turned from the sun near a floor where no shadow maps are drawn: four 140-stud floors 300
		-- studs apart (white, grey, black and a white MeshPart quad, which fills no voxel), each with upright walls
		-- facing away from the sun and across it, balls 2 to 32 studs up and plates facing down 3 to 16 studs up,
		-- in a row across the sun, seen from low on the side away from the sun
		local WHITE = Color3.fromRGB(230, 230, 230)
		local Ahead = Vector3.new(Sun.X, 0, Sun.Z).Unit
		local Across = Vector3.new(Ahead.Z, 0, -Ahead.X)
		local Floors = { { "white", WHITE }, { "grey", Color3.fromRGB(120, 120, 120) }, { "black", Color3.new(0, 0, 0) }, { "mesh", WHITE } }
		for Index, Spec in Floors do
			local Name = Spec[1]
			local Middle = Across * (300 * Index - 750)
			if Name == "mesh" then
				local ok = pcall(function()
					local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
					Part.Name = "FloorMesh"
					Part.Anchored = true
					Part.Material = Enum.Material.SmoothPlastic
					Part.Color = Spec[2]
					Part.Size = Vector3.new(140, 140, 0.001)
					Part.CFrame = CFrame.new(Middle) * CFrame.Angles(-math.pi / 2, 0, 0) * CFrame.new(-70, -70, 0)
					Part.Parent = Root
				end)
				if not ok then
					continue
				end
			else
				Block(`Floor-{Name}`, Vector3.new(140, 1, 140), Middle - Vector3.new(0, 0.5, 0), Spec[2])
			end
			local function At(Offset, Height)
				return Middle + Across * Offset + Vector3.new(0, Height, 0)
			end
			-- walls 8 x 8 x 0.2 standing on the floor: facing away from the sun (n·l -0.90) and across it (0)
			for _, Wall in { { "away", -32, -Ahead }, { "across", -20, Across } } do
				local Normal = Wall[3]
				local Position = At(Wall[2], 4)
				Block(`{Name}-wall-{Wall[1]}`, Vector3.new(8, 8, 0.2), CFrame.lookAt(Position, Position + Normal), WHITE)
				for _, H in { 1, 4, 7 } do
					Sample(`{Name}-wall-{Wall[1]}-{H}`, At(Wall[2], H) + Normal * 0.1, Normal)
				end
			end
			-- balls 4 across, their middles 2 to 32 studs over the floor
			for Step, H in { 2, 6, 12, 20, 32 } do
				local Position = At(-8 + (Step - 1) * 10, H)
				local Ball = Block(`{Name}-ball-{H}`, Vector3.new(4, 4, 4), Position, WHITE)
				Ball.Shape = Enum.PartType.Ball
				for Key, Normal in { side = -Ahead, down = (-Ahead - Vector3.new(0, 1, 0)).Unit, up = (-Ahead + Vector3.new(0, 1, 0)).Unit } do
					Sample(`{Name}-ball-{H}-{Key}`, Position + Normal * 2, Normal)
				end
			end
			-- plates 6 x 0.2 x 6 facing down, 3, 8 and 16 studs over the floor (n·l -0.44)
			for Step, H in { 3, 8, 16 } do
				Block(`{Name}-under-{H}`, Vector3.new(6, 0.2, 6), At(46 + (Step - 1) * 10, H), WHITE)
				Sample(`{Name}-under-{H}`, At(46 + (Step - 1) * 10, H - 0.1), Vector3.new(0, -1, 0))
			end
			-- the floor in front of the row
			Sample(`{Name}-floor`, At(14, 0) - Ahead * 10, Vector3.new(0, 1, 0))
			local Eye = Middle + Across * 16 - Ahead * 46 + Vector3.new(0, 2.5, 0)
			local Target = Middle + Across * 16 + Vector3.new(0, 9, 0)
			local View = { Eye = Eye, Target = Target, Fov = 70, Focus = "target" }
			Capture(`bounce-{Name}`, SUN_ONLY, View)
			if Name == "white" then
				local Far = { Eye = Target - Ahead * 700 - Vector3.new(0, 5, 0), Target = Target, Fov = 9, Focus = "target" }
				Capture("bounce-white-far", SUN_ONLY, Far)
				Capture("bounce-white-nogs", With(SUN_ONLY, { GlobalShadows = false }), View)
				Capture("bounce-white-day", DAY, View)
			end
		end
	else
		error("quality-levels: no station " .. tostring(Station))
	end

	Root.Parent = workspace
	for _, Step in After do
		Step()
	end
	return { Station = Station, Samples = Samples, Captures = Captures, Root = Root }
end
