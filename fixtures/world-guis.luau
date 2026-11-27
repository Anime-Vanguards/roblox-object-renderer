-- SurfaceGuis and BillboardGuis in the world, measured in a Roblox client (0.747, graphics quality 21): how they
-- are lit, sampled and sized (notes/render.md "GUIs"). Each phase builds its stations and returns its captures:
-- tag, the camera (eye, target, fov), the Lighting arms to take it under (`envs`, named in the measuring kit) and
-- probes (name and world point) at the GUIs' frames and the bare parts beside them.
--
-- Context.Phase "light": white and gray frames (LightInfluence 1 and 0.5) on slabs facing the sun (Context.Sun,
-- the client's Lighting:GetSunDirection()): opaque, without CastShadow, invisible, half transparent, a 4-stud
-- block and a 0.05 plate; a wall of frames and a plate facing up with a roof over half of it; billboards in the
-- open, under a roof, over a block and before a wall.
-- Context.Phase "backlit": frames on slabs 0.05 to 6 studs deep turned 120 and 150 degrees from the sun.
-- Context.Phase "sky": frames and a billboard facing each of the six axes, lit by the sky alone.
-- Context.Phase "lights": frames on boards beside a PointLight (with and without Shadows), a SpotLight, a
-- SurfaceLight and a light behind the board, and billboards beside, before and over a PointLight, in the dark.
-- Context.Phase "sampling": a 32 x 24 canvas magnified, and a 300 x 300 canvas of fine lines and text seen from
-- 0.5 to 16 times smaller than its image (1.5 device pixels a canvas pixel) and at angles.
-- Context.Phase "billboards": BillboardGuis with DistanceLowerLimit, DistanceUpperLimit and DistanceStep seen
-- from several depths, billboards on Models (with and without a PrimaryPart, a moved WorldPivot), and two
-- unit-shaped rigs carrying an overhead icon row (stud sizes, LightInfluence 0, TextScaled label with UIStroke
-- and UIGradient) and a health bar with a name, under daylight.
--
-- ror render fixtures/world-guis.luau --context '{"Phase":"light","Sun":[x,y,z]}' --camera ... (ROR_REE, ROR_RUR)
return function(Context)
	local Phase = Context and Context.Phase or "light"
	local SunList = Context and Context.Sun or { 0.5, 0.7, -0.5 }
	local Sun = Vector3.new(SunList[1], SunList[2], SunList[3]).Unit
	local Root = Instance.new("Model")
	Root.Name = "WorldGuis"

	local function Block(Name: string, CF: CFrame, Size: Vector3, Options: any?): BasePart
		local Opt = Options or {}
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Opt.Color or Color3.new(1, 1, 1)
		Part.Size = Size
		Part.CFrame = CF
		Part.Transparency = Opt.Transparency or 0
		Part.CastShadow = Opt.CastShadow ~= false
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.Parent = Opt.Parent or Root
		return Part
	end
	local function Frame(Parent: Instance, Name: string, Position: UDim2, Size: UDim2, Color: Color3): Frame
		local Item = Instance.new("Frame")
		Item.Name = Name
		Item.Position = Position
		Item.Size = Size
		Item.BackgroundColor3 = Color
		Item.BorderSizePixel = 0
		Item.Parent = Parent
		return Item
	end
	local function Surface(Part: BasePart, Name: string, Canvas: Vector2, LightInfluence: number, Face: Enum.NormalId?): SurfaceGui
		local Gui = Instance.new("SurfaceGui")
		Gui.Name = Name
		Gui.Face = Face or Enum.NormalId.Front
		Gui.SizingMode = Enum.SurfaceGuiSizingMode.FixedSize
		Gui.CanvasSize = Canvas
		Gui.LightInfluence = LightInfluence
		Gui.Brightness = 1
		Gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
		Gui.Parent = Part
		return Gui
	end
	local function Billboard(Parent: Instance, Name: string, Size: UDim2, LightInfluence: number): BillboardGui
		local Gui = Instance.new("BillboardGui")
		Gui.Name = Name
		Gui.Size = Size
		Gui.LightInfluence = LightInfluence
		Gui.Brightness = 1
		Gui.ClipsDescendants = false
		Gui.ZIndexBehavior = Enum.ZIndexBehavior.Sibling
		Gui.Parent = Parent
		return Gui
	end
	-- a point on a part's Front face at canvas fractions (u, v): the canvas runs along -X and down -Y
	local function OnFront(Part: BasePart, U: number, V: number): Vector3
		return Part.CFrame:PointToWorldSpace(Vector3.new((0.5 - U) * Part.Size.X, (0.5 - V) * Part.Size.Y, -Part.Size.Z / 2))
	end
	local function P(Name: string, Point: Vector3)
		return { name = Name, p = { Point.X, Point.Y, Point.Z } }
	end
	local function V(Point: Vector3)
		return { Point.X, Point.Y, Point.Z }
	end
	-- an invisible anchor that casts nothing
	local function Anchor(Name: string, Position: Vector3): BasePart
		return Block(Name, CFrame.new(Position), Vector3.new(0.2, 0.2, 0.2), { Transparency = 1, CastShadow = false })
	end
	-- a slab facing `Normal` with frames on its Front face: the left third bare, then white and gray frames at
	-- LightInfluence 1 (canvas 0.35-0.65) and 0.5 (0.7-0.95); returns its probes
	local function Slab(Name: string, Center: Vector3, Normal: Vector3, Size: Vector3, Options: any?): { any }
		local Part = Block(Name, CFrame.lookAt(Center, Center + Normal), Size, Options)
		local Full = Surface(Part, "LI1", Vector2.new(600, 100), 1)
		Frame(Full, "White", UDim2.fromScale(0.35, 0.05), UDim2.fromScale(0.3, 0.42), Color3.new(1, 1, 1))
		Frame(Full, "Gray", UDim2.fromScale(0.35, 0.53), UDim2.fromScale(0.3, 0.42), Color3.fromRGB(128, 128, 128))
		local Half = Surface(Part, "LI05", Vector2.new(600, 100), 0.5)
		Frame(Half, "White", UDim2.fromScale(0.7, 0.05), UDim2.fromScale(0.25, 0.42), Color3.new(1, 1, 1))
		Frame(Half, "Gray", UDim2.fromScale(0.7, 0.53), UDim2.fromScale(0.25, 0.42), Color3.fromRGB(128, 128, 128))
		return {
			P(Name .. ":bare", OnFront(Part, 0.15, 0.5)),
			P(Name .. ":w1", OnFront(Part, 0.5, 0.26)),
			P(Name .. ":g1", OnFront(Part, 0.5, 0.74)),
			P(Name .. ":w05", OnFront(Part, 0.825, 0.26)),
			P(Name .. ":g05", OnFront(Part, 0.825, 0.74)),
		}
	end
	-- a billboard (white over gray, `Studs` square) on `Parent` at LightInfluence `LI`; returns its probes
	local function Board(Parent: Instance, Name: string, Studs: number, LI: number, Offset: Vector3?, At: Vector3): { any }
		local Gui = Billboard(Parent, Name, UDim2.fromScale(Studs, Studs), LI)
		Gui.StudsOffsetWorldSpace = Offset or Vector3.zero
		Frame(Gui, "White", UDim2.fromScale(0, 0), UDim2.fromScale(1, 0.5), Color3.new(1, 1, 1))
		Frame(Gui, "Gray", UDim2.fromScale(0, 0.5), UDim2.fromScale(1, 0.5), Color3.fromRGB(128, 128, 128))
		local Centre = At + (Offset or Vector3.zero)
		return { P(Name .. ":w", Centre + Vector3.new(0, Studs / 4, 0)), P(Name .. ":g", Centre - Vector3.new(0, Studs / 4, 0)) }
	end
	-- a camera `Distance` studs out along `Direction` from `Target`
	local function Look(Target: Vector3, Direction: Vector3, Distance: number, Fov: number)
		local Eye = Target + Direction.Unit * Distance
		if math.abs(Direction.Unit.Y) > 0.99 then
			Eye += Vector3.new(0, 0, -0.001 * Distance)
		end
		return V(Eye), V(Target), Fov
	end

	local Captures = {}
	local function Capture(Tag: string, Envs: { string }, Eye, Target, Fov: number, Probes: { any })
		table.insert(Captures, { tag = Tag, envs = Envs, eye = Eye, target = Target, fov = Fov, probes = Probes })
	end

	if Phase == "light" then
		local SUN_ENVS = { "sun-gs", "sun" }
		-- p: across the sun, horizontal; the camera looks at a slab from between the sun and p
		local Across = Vector3.yAxis:Cross(Sun).Unit
		local View = (Sun + Across * 0.35).Unit
		local SLABS: { any } = {
			{ Name = "slab", Size = Vector3.new(12, 2, 0.5) },
			{ Name = "slab-nocast", Size = Vector3.new(12, 2, 0.5), CastShadow = false },
			{ Name = "slab-invisible", Size = Vector3.new(12, 2, 0.5), Transparency = 1 },
			{ Name = "slab-glass", Size = Vector3.new(12, 2, 0.5), Transparency = 0.5 },
			{ Name = "block", Size = Vector3.new(12, 2, 4) },
			{ Name = "plate", Size = Vector3.new(12, 2, 0.05) },
			-- the same slab moved half a voxel along the sun and up
			{ Name = "slab-shift", Size = Vector3.new(12, 2, 0.5), Shift = Vector3.new(1.3, 2.1, 0.7) },
		}
		for Index, Case in SLABS do
			local Center = Vector3.new((Index - 1) * 60, 20, 0) + (Case.Shift or Vector3.zero)
			local Probes = Slab(Case.Name, Center, Sun, Case.Size, { CastShadow = Case.CastShadow, Transparency = Case.Transparency })
			local Eye, Target, Fov = Look(Center, View, 30, 30)
			Capture("L-" .. Case.Name, SUN_ENVS, Eye, Target, Fov, Probes)
		end
		-- slabs turned from the sun (n·l 0.87, 0.5, 0, -0.5)
		for Index, Angle in { 30, 60, 90, 120 } do
			local Normal = (Sun * math.cos(math.rad(Angle)) + Across * math.sin(math.rad(Angle))).Unit
			local Center = Vector3.new((Index - 1) * 60, 20, 120)
			local Probes = Slab(`turn{Angle}`, Center, Normal, Vector3.new(12, 2, 0.5))
			local Eye, Target, Fov = Look(Center, (Normal + Sun * 0.3 + Across * 0.3).Unit, 30, 30)
			Capture(`L-turn{Angle}`, SUN_ENVS, Eye, Target, Fov, Probes)
		end
		-- a ladder of grays (0.5 to 1 in steps of 0.05) at LightInfluence 1 on a slab facing the sun
		do
			local Center = Vector3.new(0, 20, 180)
			local Part = Block("ladder", CFrame.lookAt(Center, Center + Sun), Vector3.new(22, 2, 0.5))
			local Gui = Surface(Part, "Ladder", Vector2.new(1100, 100), 1)
			local Probes = { P("ladder:bare", OnFront(Part, 0.03, 0.5)) }
			for Step = 0, 10 do
				local Value = 0.5 + Step * 0.05
				Frame(Gui, `G{Step}`, UDim2.fromScale((Step + 1) / 12 + 0.005, 0.1), UDim2.fromScale(1 / 12 - 0.01, 0.8), Color3.new(Value, Value, Value))
				table.insert(Probes, P(`ladder:{math.round(Value * 100)}`, OnFront(Part, (Step + 1.5) / 12, 0.5)))
			end
			local Eye, Target, Fov = Look(Center, View, 45, 30)
			Capture("L-ladder", { "sun-gs", "sun", "ambient", "sky-default" }, Eye, Target, Fov, Probes)
		end
		-- slabs facing across the sun, one under a 24-stud roof 3 studs over it, one in the open: does a GUI read the
		-- light grid's OutdoorAmbient and sky as the part does?
		for Index, Roofed in { false, true } do
			local Center = Vector3.new(240 + (Index - 1) * 60, 20, 120)
			local Probes = Slab(if Roofed then "roofed" else "unroofed", Center, Across, Vector3.new(12, 2, 0.5))
			if Roofed then
				Block("grid-roof", CFrame.new(Center + Vector3.new(0, 3.5, 0)), Vector3.new(24, 1, 24))
			end
			local Eye, Target, Fov = Look(Center, (Across + Sun * 0.1 - Vector3.yAxis * 0.1).Unit, 25, 30)
			Capture(if Roofed then "L-roofed" else "L-unroofed", { "outdoor-gs", "sky-default-gs", "outdoor" }, Eye, Target, Fov, Probes)
		end
		-- a wall of frames facing the sun: does the GUI's sun vary across a face?
		do
			local Center = Vector3.new(0, 20, 240)
			local Wall = Block("wall", CFrame.lookAt(Center, Center + Sun), Vector3.new(24, 12, 1))
			local Gui = Surface(Wall, "Wall", Vector2.new(480, 240), 1)
			local Probes = {}
			for Row = 0, 2 do
				for Column = 0, 5 do
					Frame(Gui, `F{Row}{Column}`, UDim2.fromScale(Column / 6 + 0.01, Row / 3 + 0.02), UDim2.fromScale(1 / 6 - 0.02, 1 / 3 - 0.04), Color3.new(1, 1, 1))
					table.insert(Probes, P(`wall:f{Row}{Column}`, OnFront(Wall, (Column + 0.5) / 6, (Row + 0.5) / 3)))
				end
			end
			local Eye, Target, Fov = Look(Center, View, 50, 35)
			Capture("L-wall", SUN_ENVS, Eye, Target, Fov, Probes)
		end
		-- a plate facing up, frames over its +X half's top, a roof over its -Z half 6 studs above
		do
			local Center = Vector3.new(120, 0, 240)
			local Plate = Block("plate-up", CFrame.new(Center), Vector3.new(24, 0.5, 24))
			local Gui = Surface(Plate, "Top", Vector2.new(480, 480), 1, Enum.NormalId.Top)
			-- Top: canvas right along -Z, down along +X (notes/render.md "GUIs")
			Frame(Gui, "White", UDim2.fromScale(0.02, 0.52), UDim2.fromScale(0.96, 0.46), Color3.new(1, 1, 1))
			Block("roof", CFrame.new(Center + Vector3.new(0, 6, -6)), Vector3.new(24, 1, 12))
			local Probes = {}
			for Step = 0, 11 do
				local Z = -11 + Step * 2
				table.insert(Probes, P(`plate-up:gui{Z}`, Center + Vector3.new(6, 0.25, Z)))
				table.insert(Probes, P(`plate-up:bare{Z}`, Center + Vector3.new(-6, 0.25, Z)))
			end
			local Eye, Target, Fov = Look(Center, Vector3.new(0.35, 1, 0.5), 45, 40)
			Capture("L-plate-up", SUN_ENVS, Eye, Target, Fov, Probes)
			-- the same from 700 studs, past the sun's shadow maps
			local FarEye, FarTarget, FarFov = Look(Center, Vector3.new(0.35, 1, 0.5), 700, 2.6)
			Capture("L-plate-far", { "sun-gs" }, FarEye, FarTarget, FarFov, Probes)
		end
		-- billboards: in the open, under a roof, 3 studs over a 2-stud block (a unit's bar), 1 stud before a wall
		do
			local Origin = Vector3.new(240, 20, 240)
			local Probes = {}
			for _, Item in Board(Anchor("bb-open", Origin), "open", 2, 1, nil, Origin) do
				table.insert(Probes, Item)
			end
			local Covered = Origin + Vector3.new(0, 0, 20)
			Block("bb-roof", CFrame.new(Covered + Sun * 8), Vector3.new(10, 1, 10))
			for _, Item in Board(Anchor("bb-covered", Covered), "covered", 2, 1, nil, Covered) do
				table.insert(Probes, Item)
			end
			local Unit = Origin + Vector3.new(0, 0, 40)
			local Body = Block("bb-body", CFrame.new(Unit), Vector3.new(2, 2, 2))
			for _, Item in Board(Body, "unit", 2, 1, Vector3.new(0, 3, 0), Unit) do
				table.insert(Probes, Item)
			end
			table.insert(Probes, P("unit:body", Unit + View * 1))
			local Before = Origin + Vector3.new(0, 0, 60)
			Block("bb-wall", CFrame.lookAt(Before - View * 1.25, Before - View * 1.25 + View), Vector3.new(8, 8, 0.5))
			for _, Item in Board(Anchor("bb-before", Before), "before", 2, 1, nil, Before) do
				table.insert(Probes, Item)
			end
			-- (the covered one seen across the sun, under its roof; "behind": the open one from the side away from the sun)
			for Index, Name in { "open", "covered", "unit", "before", "behind" } do
				local Target = Origin + Vector3.new(0, 0, (math.min(Index, 5) - 1) * 20)
				local Direction = View
				if Name == "covered" then
					Direction = Across
				elseif Name == "behind" then
					Target = Origin
					Direction = (Across - Sun * 0.8).Unit
				end
				local Eye, _, Fov = Look(Target, Direction, 25, 30)
				local Mine = {}
				local Prefix = if Name == "behind" then "open" else Name
				for _, Item in Probes do
					if string.sub(Item.name, 1, #Prefix + 1) == Prefix .. ":" then
						table.insert(Mine, Item)
					end
				end
				Capture("L-bb-" .. Name, SUN_ENVS, Eye, V(Target), Fov, Mine)
			end
		end
	elseif Phase == "backlit" then
		-- frames on slabs of 0.05 to 6 studs turned 120 and 150 degrees from the sun: lit from behind through
		-- the part that holds them
		local SUN_ENVS = { "sun-gs", "sun" }
		local Across = Vector3.yAxis:Cross(Sun).Unit
		for Row, Angle in { 120, 150 } do
			local Normal = (Sun * math.cos(math.rad(Angle)) + Across * math.sin(math.rad(Angle))).Unit
			for Index, Depth in { 0.05, 0.5, 2, 6 } do
				local Center = Vector3.new((Index - 1) * 60, 20, 300 + Row * 60)
				-- (capture tags take no dots: the depth in hundredths)
				local Name = `back{Angle}-{math.round(Depth * 100)}`
				local Probes = Slab(Name, Center, Normal, Vector3.new(12, 2, Depth))
				local Eye, Target, Fov = Look(Center, (Normal + Across * 0.3).Unit, 30, 30)
				Capture(`L-{Name}`, SUN_ENVS, Eye, Target, Fov, Probes)
			end
		end
	elseif Phase == "sky" then
		local SKY_ENVS = { "sky-default", "sky-default-gs", "sky-up", "sky-ft", "sky-blue" }
		local AXES = { { "px", Vector3.xAxis }, { "nx", -Vector3.xAxis }, { "py", Vector3.yAxis }, { "ny", -Vector3.yAxis }, { "pz", Vector3.zAxis }, { "nz", -Vector3.zAxis } }
		for Index, Axis in AXES do
			local Center = Vector3.new(2000 + (Index - 1) * 40, 30, 0)
			local Probes = Slab("sky-" .. Axis[1], Center, Axis[2], Vector3.new(12, 2, 0.5))
			local Side = if math.abs(Axis[2].Y) > 0.5 then Vector3.xAxis else Vector3.yAxis
			local Eye, Target, Fov = Look(Center, Axis[2] + Side * 0.3, 30, 30)
			Capture("S-" .. Axis[1], SKY_ENVS, Eye, Target, Fov, Probes)
		end
		local At = Vector3.new(2000, 30, 60)
		local Probes = Board(Anchor("sky-bb", At), "skybb", 3, 1, nil, At)
		for Index, Direction in { Vector3.new(0, 0, -1), Vector3.new(1, 0, 0), Vector3.new(0, 1, 0.3), Vector3.new(0, -1, 0.3) } do
			local Eye, Target, Fov = Look(At, Direction, 20, 30)
			Capture(`S-bb{Index}`, SKY_ENVS, Eye, Target, Fov, Probes)
		end
	elseif Phase == "lights" then
		local DARK_ENVS = { "dark-gs", "dark" }
		-- a 24 x 6 board facing -Z with frames (LightInfluence 1) over its top half, a light 3 studs before its centre
		local function Lit(Name: string, Origin: Vector3, Kind: string, Options: any?)
			local Opt = Options or {}
			local Part = Block(Name, CFrame.new(Origin), Vector3.new(24, 6, 0.5))
			local Gui = Surface(Part, "Lit", Vector2.new(960, 240), Opt.LI or 1)
			Frame(Gui, "White", UDim2.fromScale(0, 0.04), UDim2.fromScale(1, 0.42), Color3.new(1, 1, 1))
			local LightAt = Origin + (Opt.Offset or Vector3.new(0, 0, -3))
			local Carrier = Anchor(Name .. "-lamp", LightAt)
			local Light = Instance.new(Kind)
			Light.Brightness = 2
			Light.Range = 16
			Light.Color = Color3.new(1, 1, 1)
			Light.Shadows = Opt.Shadows == true
			if Kind ~= "PointLight" then
				Light.Face = Opt.Face or Enum.NormalId.Back
				Light.Angle = Opt.Angle or 90
			end
			Light.Parent = Carrier
			local Probes = {}
			for Step = 0, 12 do
				local X = -12 + Step * 2
				table.insert(Probes, P(`{Name}:gui{X}`, Origin + Vector3.new(X, 1.5, -0.25)))
				table.insert(Probes, P(`{Name}:bare{X}`, Origin + Vector3.new(X, -1.5, -0.25)))
			end
			local Eye, Target, Fov = Look(Origin, Opt.View or Vector3.new(0.15, 0.1, -1), 40, 40)
			Capture("D-" .. Name, DARK_ENVS, Eye, Target, Fov, Probes)
		end
		Lit("point", Vector3.new(4000, 20, 0), "PointLight")
		Lit("point-s", Vector3.new(4000, 20, 100), "PointLight", { Shadows = true })
		Lit("point-li05", Vector3.new(4000, 20, 200), "PointLight", { LI = 0.5 })
		Lit("point-far", Vector3.new(4000, 20, 300), "PointLight", { Offset = Vector3.new(0, 0, -8) })
		Lit("point-behind", Vector3.new(4000, 20, 400), "PointLight", { Offset = Vector3.new(0, 0, 3) })
		Lit("spot", Vector3.new(4000, 20, 500), "SpotLight", { Angle = 60, Face = Enum.NormalId.Back })
		Lit("surface", Vector3.new(4000, 20, 600), "SurfaceLight", { Angle = 90, Face = Enum.NormalId.Back })
		-- a light 4 studs out with a 4 x 8 board 2 studs out over the right of the frames (its shadow 4 to 12 studs
		-- right), shadowed or not, seen from the left
		local FromLeft = Vector3.new(-0.6, 0.1, -1)
		Lit("blocked-s", Vector3.new(4000, 20, 700), "PointLight", { Shadows = true, Offset = Vector3.new(0, 0, -4), View = FromLeft })
		Block("blocker-s", CFrame.new(4000 + 4, 20, 698), Vector3.new(4, 8, 0.4))
		Lit("blocked", Vector3.new(4000, 20, 800), "PointLight", { Offset = Vector3.new(0, 0, -4), View = FromLeft })
		Block("blocker", CFrame.new(4000 + 4, 20, 798), Vector3.new(4, 8, 0.4))
		-- billboards with a PointLight 3 studs beside, before (towards the camera) and over them
		for Index, Case in { { "beside", Vector3.new(3, 0, 0) }, { "before", Vector3.new(0, 0, -3) }, { "over", Vector3.new(0, 3, 0) } } do
			local At = Vector3.new(4100, 20, (Index - 1) * 100)
			local Probes = Board(Anchor("bb-" .. Case[1], At), Case[1], 2, 1, nil, At)
			local Carrier = Anchor("bb-" .. Case[1] .. "-lamp", At + Case[2])
			local Light = Instance.new("PointLight")
			Light.Brightness = 2
			Light.Range = 16
			Light.Parent = Carrier
			local Eye, Target, Fov = Look(At, Vector3.new(0, 0, -1), 20, 30)
			Capture("D-bb-" .. Case[1], DARK_ENVS, Eye, Target, Fov, Probes)
		end
	elseif Phase == "sampling" then
		local FLAT = { "unlit" }
		-- a 32 x 24 canvas on an 8 x 6 face seen magnified (gui-surface-basics magnified)
		do
			local Center = Vector3.new(0, 20, 6000)
			local Part = Block("magnified", CFrame.new(Center), Vector3.new(8, 6, 1), { Color = Color3.fromRGB(100, 100, 100) })
			local Gui = Surface(Part, "Small", Vector2.new(32, 24), 0)
			Frame(Gui, "TL", UDim2.fromScale(0, 0), UDim2.fromScale(0.25, 0.25), Color3.new(1, 0, 0))
			Frame(Gui, "TR", UDim2.fromScale(0.75, 0), UDim2.fromScale(0.25, 0.25), Color3.new(0, 1, 0))
			Frame(Gui, "BL", UDim2.fromScale(0, 0.75), UDim2.fromScale(0.25, 0.25), Color3.new(0, 0, 1))
			Frame(Gui, "BR", UDim2.fromScale(0.75, 0.75), UDim2.fromScale(0.25, 0.25), Color3.new(1, 1, 1))
			for Index = 0, 7 do
				Frame(Gui, `Dot{Index}`, UDim2.fromOffset(10 + 2 * Index, 9 + Index % 2), UDim2.fromOffset(1, 1), Color3.new(1, 1, 1))
			end
			local Label = Instance.new("TextLabel")
			Label.BackgroundTransparency = 1
			Label.Position = UDim2.fromOffset(9, 13)
			Label.Size = UDim2.fromOffset(14, 9)
			Label.Text = "Ag"
			Label.TextSize = 8
			Label.TextColor3 = Color3.new(1, 1, 1)
			Label.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json")
			Label.Parent = Gui
			local Eye, Target, Fov = Look(Center, Vector3.new(0, 0, -1), 7.5, 60)
			Capture("P-magnified", FLAT, Eye, Target, Fov, { P("magnified:tl", OnFront(Part, 0.125, 0.125)) })
		end
		-- a 300 x 300 canvas on a 6 x 6 face: black lines on white (1 px every 3, 2 px every 4), text of five sizes,
		-- a gradient and an image
		do
			local Center = Vector3.new(100, 20, 6000)
			local Part = Block("fine", CFrame.new(Center), Vector3.new(6, 6, 0.5), { Color = Color3.fromRGB(100, 100, 100) })
			local Gui = Surface(Part, "Fine", Vector2.new(300, 300), 0)
			Frame(Gui, "Paper", UDim2.fromScale(0, 0), UDim2.fromScale(1, 1), Color3.new(1, 1, 1))
			for Index = 0, 32 do
				Frame(Gui, `Thin{Index}`, UDim2.fromOffset(4 + 3 * Index, 4), UDim2.fromOffset(1, 92), Color3.new(0, 0, 0))
			end
			for Index = 0, 23 do
				Frame(Gui, `Thick{Index}`, UDim2.fromOffset(104 + 4 * Index, 4), UDim2.fromOffset(2, 92), Color3.new(0, 0, 0))
			end
			for Index, Size in { 8, 12, 18, 24, 36 } do
				local Label = Instance.new("TextLabel")
				Label.BackgroundTransparency = 1
				Label.Position = UDim2.fromOffset(4, 100 + (Index - 1) * 38)
				Label.Size = UDim2.fromOffset(196, 36)
				Label.Text = "Hello, world 0123"
				Label.TextSize = Size
				Label.TextXAlignment = Enum.TextXAlignment.Left
				Label.TextColor3 = Color3.new(0, 0, 0)
				Label.FontFace = Font.new("rbxasset://fonts/families/BuilderSans.json")
				Label.Parent = Gui
			end
			local Ramp = Frame(Gui, "Ramp", UDim2.fromOffset(204, 4), UDim2.fromOffset(92, 92), Color3.new(1, 1, 1))
			local Gradient = Instance.new("UIGradient")
			Gradient.Color = ColorSequence.new(Color3.new(1, 0, 0), Color3.new(0, 0, 1))
			Gradient.Parent = Ramp
			local Image = Instance.new("ImageLabel")
			Image.BackgroundTransparency = 1
			Image.Position = UDim2.fromOffset(204, 104)
			Image.Size = UDim2.fromOffset(92, 92)
			Image.Image = "rbxasset://textures/ui/GuiImagePlaceholder.png"
			Image.Parent = Gui
			-- k times smaller than the GUI's image (1.5 device pixels a canvas pixel at 1116 pixels tall, fov 30)
			local Focal = 558 / math.tan(math.rad(15))
			for _, K in { 0.5, 1, 1.5, 2, 2.8, 4, 5.6, 8, 11, 16 } do
				local Distance = Focal * 6 / 300 / (1.5 / K)
				local Eye, Target, Fov = Look(Center, Vector3.new(0, 0, -1), Distance, 30)
				Capture(`P-fine-k{string.gsub(tostring(K), "%.", "_")}`, FLAT, Eye, Target, Fov, { P("fine:paper", OnFront(Part, 0.9, 0.9)) })
			end
			for _, Angle in { 60, 75 } do
				local Direction = Vector3.new(math.sin(math.rad(Angle)), 0, -math.cos(math.rad(Angle)))
				local Eye, Target, Fov = Look(Center, Direction, Focal * 6 / 300, 30)
				Capture(`P-fine-a{Angle}`, FLAT, Eye, Target, Fov, { P("fine:paper", OnFront(Part, 0.9, 0.9)) })
			end
		end
	elseif Phase == "billboards" then
		-- DistanceLowerLimit, DistanceUpperLimit and DistanceStep: 3 x 1 stud (or 120 x 40 pixel) red billboards
		-- with a white middle, 5 studs apart in a row facing -Z
		local LIMITS: { any } = {
			{ "plain", {} },
			{ "lower30", { DistanceLowerLimit = 30 } },
			{ "upper30", { DistanceUpperLimit = 30 } },
			{ "step16", { DistanceStep = 16 } },
			{ "px-lower30", { DistanceLowerLimit = 30 }, true },
			{ "px-upper30", { DistanceUpperLimit = 30 }, true },
		}
		local Row = Vector3.new(0, 20, 9000)
		local Probes = {}
		for Index, Case in LIMITS do
			local At = Row + Vector3.new((Index - 3.5) * 5, 0, 0)
			local Gui = Billboard(Anchor("lim-" .. Case[1], At), "Limit-" .. Case[1], if Case[3] then UDim2.fromOffset(120, 40) else UDim2.fromScale(3, 1), 0)
			for Key, Value in Case[2] do
				Gui[Key] = Value
			end
			Frame(Gui, "Red", UDim2.fromScale(0, 0), UDim2.fromScale(1, 1), Color3.new(1, 0, 0))
			Frame(Gui, "White", UDim2.fromScale(0.25, 0.25), UDim2.fromScale(0.5, 0.5), Color3.new(1, 1, 1))
			table.insert(Probes, P("lim:" .. Case[1], At))
		end
		for _, Depth in { 12, 20, 30, 45, 60, 90 } do
			local Eye, Target, Fov = Look(Row, Vector3.new(0, 0, -1), Depth, 50)
			Capture(`B-limits-d{Depth}`, { "flat" }, Eye, Target, Fov, Probes)
		end
		-- billboards on Models: a 4-stud cube at the origin and a 1-stud cube at (8, 6, 0); no PrimaryPart, the small
		-- cube as PrimaryPart, the pivot moved to (-5, -5, 0)
		local Models = Vector3.new(100, 20, 9000)
		local ModelProbes = {}
		for Index, Kind in { "box", "primary", "pivot" } do
			local Origin = Models + Vector3.new((Index - 2) * 30, 0, 0)
			local Holder = Instance.new("Model")
			Holder.Name = "adornee-" .. Kind
			local Big = Block(Kind .. "-big", CFrame.new(Origin), Vector3.new(4, 4, 4), { Parent = Holder, Color = Color3.fromRGB(90, 90, 90) })
			local Small = Block(Kind .. "-small", CFrame.new(Origin + Vector3.new(8, 6, 0)), Vector3.new(1, 1, 1), { Parent = Holder, Color = Color3.fromRGB(90, 90, 90) })
			Holder.Parent = Root
			if Kind == "primary" then
				Holder.PrimaryPart = Small
			elseif Kind == "pivot" then
				Holder.WorldPivot = CFrame.new(Origin + Vector3.new(-5, -5, 0))
			end
			local Gui = Billboard(Holder, "OnModel", UDim2.fromScale(1, 1), 0)
			Gui.Adornee = Holder
			Gui.AlwaysOnTop = true
			Frame(Gui, "Green", UDim2.fromScale(0, 0), UDim2.fromScale(1, 1), Color3.new(0, 1, 0))
			table.insert(ModelProbes, P("model:" .. Kind, Origin))
			local _ = Big
		end
		local Eye, Target, Fov = Look(Models, Vector3.new(0, 0, -1), 90, 50)
		Capture("B-models", { "flat" }, Eye, Target, Fov, ModelProbes)
		-- two unit-shaped rigs on a grass floor, an icon row over each (LightInfluence 0) and a health bar under it
		-- with a name (LightInfluence 0 on the first rig, 1 on the second)
		local Field = Vector3.new(300, 0, 9000)
		Block("grass", CFrame.new(Field - Vector3.new(0, 0.5, 0)), Vector3.new(60, 1, 60), { Color = Color3.fromRGB(75, 151, 75) })
		local UnitProbes = {}
		for Index, LI in { 0, 1 } do
			local Feet = Field + Vector3.new((Index - 1.5) * 8, 0, 0)
			local Rig = Instance.new("Model")
			Rig.Name = `Unit{Index}`
			local RootPart = Block("HumanoidRootPart", CFrame.new(Feet + Vector3.new(0, 3, 0)), Vector3.new(2, 2, 1), { Parent = Rig, Transparency = 1, CastShadow = false })
			Block("Torso", CFrame.new(Feet + Vector3.new(0, 3, 0)), Vector3.new(2, 2, 1), { Parent = Rig, Color = Color3.fromRGB(196, 40, 28) })
			Block("Head", CFrame.new(Feet + Vector3.new(0, 4.6, 0)), Vector3.new(1.2, 1.2, 1.2), { Parent = Rig, Color = Color3.fromRGB(245, 205, 48) })
			Block("LeftLeg", CFrame.new(Feet + Vector3.new(-0.5, 1, 0)), Vector3.new(1, 2, 1), { Parent = Rig, Color = Color3.fromRGB(13, 105, 172) })
			Block("RightLeg", CFrame.new(Feet + Vector3.new(0.5, 1, 0)), Vector3.new(1, 2, 1), { Parent = Rig, Color = Color3.fromRGB(13, 105, 172) })
			Rig.PrimaryPart = RootPart
			Rig.Parent = Root
			-- the icon row: an image over its name, sized in studs
			local Icons = Billboard(RootPart, "OverheadIcons", UDim2.fromScale(1.6, 1.5), 0)
			Icons.StudsOffsetWorldSpace = Vector3.new(0, 3.75, 0)
			local Cell = Instance.new("Frame")
			Cell.BackgroundTransparency = 1
			Cell.Size = UDim2.fromScale(1, 1)
			Cell.Parent = Icons
			local Image = Instance.new("ImageLabel")
			Image.BackgroundTransparency = 1
			Image.Image = "rbxasset://textures/ui/GuiImagePlaceholder.png"
			Image.ScaleType = Enum.ScaleType.Fit
			Image.AnchorPoint = Vector2.new(0.5, 1)
			Image.Position = UDim2.fromScale(0.5, 1)
			Image.Size = UDim2.fromScale(1, 0.8)
			Image.Parent = Cell
			local Label = Instance.new("TextLabel")
			Label.BackgroundTransparency = 1
			Label.AnchorPoint = Vector2.new(0.5, 0)
			Label.Position = UDim2.fromScale(0.5, 0)
			Label.Size = UDim2.fromScale(1, 0.2)
			Label.FontFace = Font.fromEnum(Enum.Font.FredokaOne)
			Label.Text = "Upgrade"
			Label.TextColor3 = Color3.new(1, 1, 1)
			Label.TextScaled = true
			Label.Parent = Cell
			local Gradient = Instance.new("UIGradient")
			Gradient.Color = ColorSequence.new(Color3.fromRGB(255, 230, 90), Color3.fromRGB(255, 120, 40))
			Gradient.Rotation = 90
			Gradient.Parent = Label
			local Stroke = Instance.new("UIStroke")
			Stroke.Color = Color3.new(0, 0, 0)
			Stroke.Thickness = 2
			Stroke.Parent = Label
			-- the health bar: a dark track, a green fill at 70 % and the unit's name above it
			local Bar = Billboard(RootPart, "Healthbar", UDim2.new(3, 0, 0.75, 0), LI)
			Bar.StudsOffset = Vector3.new(0, 2.4, 0)
			local Name = Instance.new("TextLabel")
			Name.BackgroundTransparency = 1
			Name.Size = UDim2.fromScale(1, 0.5)
			Name.FontFace = Font.fromEnum(Enum.Font.FredokaOne)
			Name.Text = "Hero"
			Name.TextColor3 = Color3.new(1, 1, 1)
			Name.TextScaled = true
			Name.Parent = Bar
			local NameStroke = Instance.new("UIStroke")
			NameStroke.Thickness = 1.5
			NameStroke.Parent = Name
			local Track = Frame(Bar, "Track", UDim2.fromScale(0.05, 0.6), UDim2.fromScale(0.9, 0.35), Color3.fromRGB(30, 30, 30))
			local TrackStroke = Instance.new("UIStroke")
			TrackStroke.Thickness = 2
			TrackStroke.Parent = Track
			Frame(Track, "Fill", UDim2.fromScale(0, 0), UDim2.fromScale(0.7, 1), Color3.fromRGB(80, 220, 90))
			local BarCentre = Feet + Vector3.new(0, 3 + 2.4, 0)
			table.insert(UnitProbes, P(`unit{Index}:fill`, BarCentre + Vector3.new(-0.6, -0.1, 0)))
			table.insert(UnitProbes, P(`unit{Index}:track`, BarCentre + Vector3.new(1.05, -0.1, 0)))
			table.insert(UnitProbes, P(`unit{Index}:torso`, Feet + Vector3.new(0, 3, -0.5)))
		end
		local Centre = Field + Vector3.new(0, 4, 0)
		local Cameras = {
			{ "B-units-near", Vector3.new(0, 0.35, -1), 14, 50 },
			{ "B-units-av", Vector3.new(0, 0.8, -1), 40, 70 },
			{ "B-units-far", Vector3.new(0.3, 0.9, -1), 80, 70 },
		}
		for _, Item in Cameras do
			local UnitEye, UnitTarget, UnitFov = Look(Centre, Item[2], Item[3], Item[4])
			Capture(Item[1], { "day" }, UnitEye, UnitTarget, UnitFov, UnitProbes)
		end
	end
	Root.Parent = workspace
	return Captures
end
