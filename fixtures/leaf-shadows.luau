-- Leaf cards in the sun: flat 4 x 4 stud quads (the install's quad mesh bound with CreateMeshPartAsync,
-- facing up, 3 studs over a grey floor) each under a SurfaceAppearance whose ColorMap is an EditableImage
-- of one alpha everywhere, in every AlphaMode, alpha from 0 to 255 in AlphaMode Transparency, at part
-- Transparency 0 to 0.76 and with CastShadow off, beside an opaque block and an untextured quad; then
-- cards with holes (a checkerboard of alpha 0 and 255, a leaf pattern): one in view, one out of view
-- whose shadow falls in view, a checkerboard over an opaque card it shades (a crown shading itself),
-- and a shrub of tilted leaf cards. Context.Phase "lights": shadowed PointLights over eight more cards
-- are on (put the sun out with Lighting.Brightness 0); "sweep": the cut in alpha byte by byte, alone and
-- with Transparency, and a MeshPart's TextureContent at alpha 32; "forest": Context.Shrubs (20) shrubs a
-- side, for the cost.
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic) under ClockTime 16.2,
-- GeographicLatitude 38, ShadowSoftness 0.2, Brightness 2, Ambient 0.3 grey, OutdoorAmbient black and
-- EnvironmentDiffuseScale and EnvironmentSpecularScale 0, from (0, 140, 60) looking at the origin with
-- a field of view of 40, against ror's render of the same:
-- ror render fixtures/leaf-shadows.luau --lighting-from <a place with that Lighting> --size 1600x900 --   --camera '{"mode":"lookAt","eye":[0,140,60],"target":[0,0,0],"fov":40}' --no-service --   --context '{"Phase":"sun","Mesh":"file:///<Studio>/content/avatar/compositing/CompositQuad.mesh"}'
-- (docs/rendering.md "Shadows and Transparency"; ROR_REE: the engine emulator's checkout). It returns
-- each case's name and where to read it on the floor: its shadow's middle and a lit point beside it,
-- or the middles of a shadowed square and a hole, or the floor under a lamp.
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Phase = Context and Context.Phase or "sun"
	local Root = Instance.new("Model")
	Root.Name = "LeafShadows"

	local LEAF = { 60, 140, 60 }
	local Images = {}
	-- an image of one texel everywhere (8 x 8), or of a pattern (64 x 64) from Alpha(x, y)
	local function Image(Key, Side, Alpha)
		if Images[Key] then
			return Images[Key]
		end
		local Pixels = buffer.create(Side * Side * 4)
		for Y = 0, Side - 1 do
			for X = 0, Side - 1 do
				local Index = (Y * Side + X) * 4
				buffer.writeu8(Pixels, Index, LEAF[1])
				buffer.writeu8(Pixels, Index + 1, LEAF[2])
				buffer.writeu8(Pixels, Index + 2, LEAF[3])
				buffer.writeu8(Pixels, Index + 3, Alpha(X, Y))
			end
		end
		local Editable = AssetService:CreateEditableImage({ Size = Vector2.new(Side, Side) })
		Editable:WritePixelsBuffer(Vector2.zero, Editable.Size, Pixels)
		Images[Key] = Editable
		return Editable
	end
	local function Flat(Alpha)
		return Image(`flat{Alpha}`, 8, function()
			return Alpha
		end)
	end
	-- 4 x 4 squares, the corner (u, v near 0) opaque
	local function Checker()
		return Image("checker", 64, function(X, Y)
			return if (X // 16 + Y // 16) % 2 == 0 then 255 else 0
		end)
	end
	-- round leaves with soft edges: alpha 255 in their middle falling to 0 over 4 texels
	local LEAVES = { { 18, 20, 13 }, { 44, 16, 11 }, { 30, 44, 14 }, { 52, 46, 8 }, { 10, 52, 7 } }
	local function Leaves()
		return Image("leaves", 64, function(X, Y)
			local Most = 0
			for _, Leaf in LEAVES do
				local Distance = math.sqrt((X + 0.5 - Leaf[1]) ^ 2 + (Y + 0.5 - Leaf[2]) ^ 2)
				Most = math.max(Most, math.clamp((Leaf[3] - Distance) / 4, 0, 1))
			end
			return math.floor(Most * 255 + 0.5)
		end)
	end

	-- the install's quad mesh (1024 units square in XY from its origin, facing +Z, u along +X, v down +Y)
	-- turned to face up, u along +X and v along +Z, its middle moved onto the card's position (a client and
	-- ror keep a file mesh's origin as the part's). A client's MeshParts made from EditableMeshes cast no
	-- shadow at all, whatever their shape (a flat quad either way up or both, a closed 4 x 0.2 x 4 box),
	-- so the cards are file meshes; the engine emulator loads them from file:// paths, not rbxasset://
	-- (Context.Mesh).
	local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local FACE_UP = CFrame.Angles(-math.pi / 2, 0, 0)

	-- sun shadows fall along -Lighting:GetSunDirection() at ClockTime 16.2 and latitude 38: 1.963 studs
	-- along +X and 0.570 along -Z per stud of height (fixed here, so the scene is the same whatever the
	-- Lighting it is built under)
	local Sun = Vector3.new(-0.862626, 0.439530, 0.250380)
	local function Shadow(Point)
		return Point - Sun * (Point.Y / Sun.Y)
	end

	local Layout = {}
	local function Card(Case, Position, Size, Rotation)
		local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
		Part.Name = Case.Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Color3.fromRGB(LEAF[1], LEAF[2], LEAF[3])
		Part.Size = Vector3.new(Size, Size, 0.001)
		Part.CFrame = CFrame.new(Position) * (Rotation or CFrame.identity) * FACE_UP * CFrame.new(-Size / 2, -Size / 2, 0)
		Part.Transparency = Case.Transparency or 0
		Part.CastShadow = Case.CastShadow ~= false
		if Case.Map then
			local Surface = AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Case.Map) })
			Surface.AlphaMode = Case.Mode or Enum.AlphaMode.Transparency
			Surface.Parent = Part
		end
		Part.Parent = Root
		return Part
	end
	local function Entry(Name, Kind, Point)
		table.insert(Layout, { name = Name, kind = Kind, x = Point.X, y = Point.Y, z = Point.Z })
	end

	local Floor = Instance.new("Part")
	Floor.Name = "Floor"
	Floor.Anchored = true
	Floor.Material = Enum.Material.SmoothPlastic
	Floor.Color = Color3.fromRGB(200, 200, 200)
	Floor.Size = Vector3.new(200, 1, 200)
	Floor.CFrame = CFrame.new(0, -0.5, 0)
	Floor.Parent = Root

	local T = Enum.AlphaMode.Transparency

	-- Context.Phase "forest": Context.Shrubs (20) shrubs a side, 8 studs apart, of the shrub's 12 tilted
	-- leaf cards each (what the cut-out shadows cost)
	local TILTS = {
		{ 0, 4.0, 0, 0, 0 }, { 1.5, 3.2, 1, 25, 40 }, { -1.5, 3.5, -1, -20, 100 }, { 1, 4.8, -1.5, 35, 200 },
		{ -1, 5.2, 1.5, -30, 280 }, { 2, 2.6, -0.5, 50, 160 }, { -2, 2.8, 0.5, -45, 320 }, { 0.5, 5.5, 0.5, 15, 70 },
		{ -0.5, 3.0, -2, 60, 20 }, { 0, 2.5, 2, -55, 240 }, { 2.2, 4.2, 1.8, 20, 300 }, { -2.2, 4.4, -1.8, -25, 130 },
	}
	local function Shrub(Name, Base)
		for Index, Tilt in TILTS do
			local Rotation = CFrame.Angles(0, math.rad(Tilt[5]), 0) * CFrame.Angles(math.rad(Tilt[4]), 0, 0)
			Card({ Name = `{Name}-{Index}`, Map = Leaves() }, Base + Vector3.new(Tilt[1], Tilt[2], Tilt[3]), 3.5, Rotation)
		end
	end
	if Phase == "forest" then
		local Side = Context.Shrubs or 20
		for X = 1, Side do
			for Z = 1, Side do
				Shrub(`shrub-{X}-{Z}`, Vector3.new((X - (Side + 1) / 2) * 8, 0, (Z - (Side + 1) / 2) * 8))
			end
		end
		Root.Parent = workspace
		return Layout
	end
	local CASES: { any } = {
		{ Name = "block" },
		{ Name = "quad" },
		{ Name = "tr255", Alpha = 255 },
		{ Name = "tr0", Alpha = 0 },
		{ Name = "ov0", Alpha = 0, Mode = Enum.AlphaMode.Overlay },
		{ Name = "ov128", Alpha = 128, Mode = Enum.AlphaMode.Overlay },
		{ Name = "tm0", Alpha = 0, Mode = Enum.AlphaMode.TintMask },
	}
	for _, Alpha in { 1, 8, 32, 64, 96, 112, 120, 124, 126, 127, 128, 129, 130, 132, 136, 144, 160, 192, 200, 224, 250 } do
		table.insert(CASES, { Name = `tr{Alpha}`, Alpha = Alpha })
	end
	for _, Pair in { { 0.3, 255 }, { 0.5, 255 }, { 0.6, 255 }, { 0.7, 255 }, { 0.745, 255 }, { 0.76, 255 }, { 0.5, 192 }, { 0.3, 160 }, { 0.3, 128 }, { 0.5, 160 }, { 0.5, 140 } } do
		table.insert(CASES, { Name = `t{Pair[1]}-tr{Pair[2]}`, Alpha = Pair[2], Transparency = Pair[1] })
	end
	table.insert(CASES, { Name = "off-tr255", Alpha = 255, CastShadow = false })
	table.insert(CASES, { Name = "off-quad", CastShadow = false })
	table.insert(CASES, { Name = "none" })

	-- Context.Phase "sweep": the cut byte by byte either side of 0.252 of opacity, (1 - Transparency) x alpha
	if Phase == "sweep" then
		CASES = { { Name = "block" }, { Name = "none" } }
		for _, Row in { { 0, { 63, 64, 65, 66 } }, { 0.2, { 79, 80, 81, 82 } }, { 0.3, { 90, 91, 92, 93 } }, { 0.5, { 127, 128, 129, 130 } }, { 0.7, { 212, 213, 214, 215, 216, 217 } } } do
			for _, Alpha in Row[2] do
				table.insert(CASES, { Name = `t{Row[1]}-tr{Alpha}`, Alpha = Alpha, Transparency = Row[1] })
			end
		end
		table.insert(CASES, { Name = "t0.5-ov0", Alpha = 0, Mode = Enum.AlphaMode.Overlay, Transparency = 0.5 })
		table.insert(CASES, { Name = "t0.76-ov255", Alpha = 255, Mode = Enum.AlphaMode.Overlay, Transparency = 0.76 })
	end
	local COLUMNS = 7
	for Index, Case in CASES do
		local Column = (Index - 1) % COLUMNS
		local Row = (Index - 1) // COLUMNS
		local Position = Vector3.new(-49 + Column * 14, 3, -25 + Row * 10)
		if Case.Name == "block" then
			local Block = Instance.new("Part")
			Block.Name = "block"
			Block.Anchored = true
			Block.Material = Enum.Material.SmoothPlastic
			Block.Color = Color3.fromRGB(LEAF[1], LEAF[2], LEAF[3])
			Block.Size = Vector3.new(4, 0.2, 4)
			Block.CFrame = CFrame.new(Position)
			Block.Parent = Root
		elseif Case.Name ~= "none" then
			Case.Map = Case.Alpha and Flat(Case.Alpha)
			Card(Case, Position, 4)
		end
		local Middle = Shadow(Position)
		Entry(Case.Name, "shadow", Middle)
		Entry(Case.Name, "lit", Middle + Vector3.new(0, 0, 4.5))
	end

	if Phase == "sweep" then
		local Textured = Card({ Name = "texture32" }, Vector3.new(-35, 3, 25), 4)
		Textured.TextureContent = Content.fromObject(Flat(32))
		Entry("texture32", "shadow", Shadow(Vector3.new(-35, 3, 25)))
		Entry("texture32", "lit", Shadow(Vector3.new(-35, 3, 25)) + Vector3.new(0, 0, 4.5))
		Root.Parent = workspace
		return Layout
	end

	-- a checkerboard in view and one out of view whose shadow lands at (25, 0, 40): the middles of a
	-- shadowed square and of a hole next to it, 0.75 studs either side of the card's middle along u
	local function Squares(Name, Position)
		Card({ Name = Name, Map = Checker() }, Position, 6)
		Entry(Name, "square", Shadow(Position + Vector3.new(-2.25, 0, -2.25)))
		Entry(Name, "hole", Shadow(Position + Vector3.new(-0.75, 0, -2.25)))
	end
	Squares("checker-in", Vector3.new(38, 3, 40))
	Squares("checker-out", Vector3.new(25, 0, 40) + Sun * (40 / Sun.Y))

	-- a crown shading itself: a checkerboard 3 studs over an opaque card (AlphaMode Transparency at alpha
	-- 255) where its shadow falls
	local Upper = Vector3.new(0, 7, 40)
	local Lower = Upper - Sun * (3 / Sun.Y)
	Card({ Name = "crown-upper", Map = Checker() }, Upper, 6)
	Card({ Name = "crown-lower", Map = Flat(255) }, Lower, 6)
	do
		local Offset = Lower - Upper
		Entry("crown", "square", Upper + Vector3.new(-2.25, 0, -2.25) + Offset)
		Entry("crown", "hole", Upper + Vector3.new(-0.75, 0, -2.25) + Offset)
	end

	-- a shrub: leaf cards tilted every way between 2.5 and 5.5 studs up
	local ShrubAt = Vector3.new(-35, 0, 40)
	Shrub("shrub", ShrubAt)
	Entry("shrub", "shadow", Shadow(ShrubAt + Vector3.new(0, 4, 0)))
	Entry("shrub", "lit", Shadow(ShrubAt + Vector3.new(0, 4, 0)) + Vector3.new(0, 0, 7))

	-- shadowed PointLights 8 studs over 3 x 3 cards 4 studs up (the sun out): the floor under each, and
	-- under a 2 x 2 checkerboard the middles of a shadowed square and of a hole (1.5 times the card's
	-- offsets from the lamp)
	local LAMPS = {
		{ Name = "light-quad" }, { Name = "light-tr32", Alpha = 32 }, { Name = "light-tr64", Alpha = 64 }, { Name = "light-tr65", Alpha = 65 },
		{ Name = "light-tr77", Alpha = 77 }, { Name = "light-tr179", Alpha = 179 }, { Name = "light-checker" }, { Name = "light-none" },
	}
	for Index, Case in LAMPS do
		local Position = Vector3.new(-42 + (Index - 1) * 12, 4, -45)
		if Case.Name == "light-checker" then
			Case.Map = Image("checker2", 64, function(X, Y)
				return if (X // 32 + Y // 32) % 2 == 0 then 255 else 0
			end)
			Card(Case, Position, 3)
			Entry(Case.Name, "square", Vector3.new(Position.X - 1.125, 0, Position.Z - 1.125))
			Entry(Case.Name, "hole", Vector3.new(Position.X + 1.125, 0, Position.Z - 1.125))
		elseif Case.Name ~= "light-none" then
			Case.Map = Case.Alpha and Flat(Case.Alpha)
			Card(Case, Position, 3)
		end
		local Carrier = Instance.new("Part")
		Carrier.Name = `{Case.Name}-lamp`
		Carrier.Anchored = true
		Carrier.Transparency = 1
		Carrier.CastShadow = false
		Carrier.Size = Vector3.new(0.2, 0.2, 0.2)
		Carrier.CFrame = CFrame.new(Position + Vector3.new(0, 8, 0))
		Carrier.Parent = Root
		local Light = Instance.new("PointLight")
		Light.Range = 16
		Light.Brightness = 1.5
		Light.Shadows = true
		Light.Enabled = Phase == "lights"
		Light.Parent = Carrier
		Entry(Case.Name, "light", Vector3.new(Position.X, 0, Position.Z))
	end

	Root.Parent = workspace
	return Layout
end
