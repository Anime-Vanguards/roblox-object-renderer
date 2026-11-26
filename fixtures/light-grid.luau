-- The light grid and ambient occlusion in a Roblox client, phase by phase (Context.Phase), each built alone
-- round the origin. Every phase returns its Layout: the views (straight-down or oblique cameras) and the
-- arms (Lighting properties over Layout.Base, the LightingStyle, a phase rebuilt with extra Context) to
-- capture each view under.
--
--   hall      a closed hall of plain Parts 140 wide, 110 high and 200 deep, open at its -X end towards the
--             sun (46.7 degrees up): its floor and side wall behind the roof's shadow line under each
--             light term alone, all of them, OutdoorAmbient equal to Ambient, and the mouth closed
--   shapes    white floors under roofs of SpecialMesh types, unions (Context.MakeUnion, a client's
--             GeometryService), a MeshPart quad, turned and tilted slabs, and rows of upright sheets
--             (whole, turned, short, through part of a layer), straight down from under each; OutdoorAmbient
--             alone
--   walls     an 80 x 40 x 0.5-stud wall in the open (lg-specular's), 1 and 2 thick, 20 tall and on a voxel boundary,
--             head-on from 45 studs: how much of the sky its face's grid sees down its height; OutdoorAmbient alone
--   sky       a roofed and an open floor under the sky's specular light alone (EV +2) and its diffuse light
--             alone, the roofed view captured again from 0.01 studs lower
--   crease    a floor meeting a wall with a cube, a black bar and a low step on it, from 3 to 100 studs
--             through fields of view keeping its picture; Ambient alone without shadows (the occlusion's
--             reach against the render size: the same views at several viewport sizes)
--   contacts  a ball, a cube and a plate over a floor beside a wall, under Ambient, OutdoorAmbient and the
--             sky's light alone and under a day's Lighting, each under LightingStyle Realistic and Soft
--   roof      a 48 x 48 roof 16 studs up of a block (x -100) and of the install's quad mesh (x 100) in the
--             sun alone, and under OutdoorAmbient alone (the sun where the shadow maps are not drawn, by
--             graphics quality level)
--
-- Measured in a Roblox client (0.747) at 1600 x 900, the job writing each arm's Lighting
-- and, at identity 4, its LightingStyle. ror renders the same views with the arm's Lighting through
-- --lighting-from (a place with Layout.Base) and --set Lighting.<Property>=<value> (notes/render.md "Light
-- grid", "Ambient occlusion").
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Phase = Context and Context.Phase or "hall"
	local Root = Instance.new("Model")
	Root.Name = "LightGrid"

	local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local FACE_UP = CFrame.Angles(-math.pi / 2, 0, 0)
	local WHITE = Color3.new(1, 1, 1)
	local GREY = Color3.fromRGB(163, 162, 165)

	local function Box(Name, CF, Size, Options)
		Options = Options or {}
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Options.Material or Enum.Material.SmoothPlastic
		Part.Color = Options.Color or WHITE
		Part.Size = Size
		Part.CFrame = if typeof(CF) == "Vector3" then CFrame.new(CF) else CF
		Part.CastShadow = Options.CastShadow ~= false
		if Options.Shape then
			Part.Shape = Options.Shape
		end
		Part.Parent = Options.Parent or Root
		return Part
	end
	-- a box from `Min` to `Max`
	local function Span(Name, Min, Max, Options)
		return Box(Name, (Min + Max) * 0.5, Max - Min, Options)
	end
	local function Quad(Name, Position, Side)
		local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = WHITE
		Part.Size = Vector3.new(Side, Side, 0.001)
		Part.CFrame = CFrame.new(Position) * FACE_UP * CFrame.new(-Side / 2, -Side / 2, 0)
		Part.Parent = Root
		return Part
	end
	local function V(Vector)
		return { Vector.X, Vector.Y, Vector.Z }
	end
	-- straight down onto (x, z) from `Height`
	local function Down(Name, X, Z, Height, Fov)
		return { name = Name, eye = { X, Height, Z - 0.05 }, target = { X, 0, Z }, fov = Fov }
	end
	local function Look(Name, Eye, Target, Fov)
		return { name = Name, eye = V(Eye), target = V(Target), fov = Fov }
	end

	-- every light off; ClockTime 14.667 at the default latitude puts the sun 46.7 degrees up towards -X
	-- (and a little +Z)
	local Layout = {
		phase = Phase,
		base = {
			ClockTime = 14.667,
			GeographicLatitude = 41.733,
			Brightness = 0,
			Ambient = { 0, 0, 0 },
			OutdoorAmbient = { 0, 0, 0 },
			EnvironmentDiffuseScale = 0,
			EnvironmentSpecularScale = 0,
			ExposureCompensation = 0,
			GlobalShadows = true,
		},
		views = {},
		arms = {},
	}

	if Phase == "hall" then
		-- the hall: floor from x 0 to 200 (and 40 studs out of the mouth), side walls at z = +-70, the back
		-- wall at x 200, the roof's underside at 110; Context.Closed closes the mouth
		local Options = { Color = GREY }
		Span("Floor", Vector3.new(-40, -1, -72), Vector3.new(202, 0, 72), Options)
		Span("WallLeft", Vector3.new(0, 0, -72), Vector3.new(202, 110, -70), Options)
		Span("WallRight", Vector3.new(0, 0, 70), Vector3.new(202, 110, 72), Options)
		Span("WallBack", Vector3.new(200, 0, -70), Vector3.new(202, 110, 70), Options)
		Span("Roof", Vector3.new(0, 110, -72), Vector3.new(202, 112, 72), Options)
		if Context and Context.Closed then
			Span("Mouth", Vector3.new(-2, 0, -70), Vector3.new(0, 110, 70), Options)
		end
		for _, X in { 60, 100, 140, 180 } do
			table.insert(Layout.views, Down(`down{X}`, X, 0, 30, 70))
		end
		table.insert(Layout.views, Look("long", Vector3.new(10, 40, -50), Vector3.new(150, 0, 30), 70))
		table.insert(Layout.views, Look("wall", Vector3.new(130, 30, -40), Vector3.new(130, 30, 70), 70))
		local A, O = { 50, 50, 50 }, { 128, 128, 128 }
		Layout.arms = {
			{ name = "all", lighting = { Brightness = 3, Ambient = A, OutdoorAmbient = O } },
			{ name = "nosun", lighting = { Ambient = A, OutdoorAmbient = O } },
			{ name = "oa-is-a", lighting = { Brightness = 3, Ambient = A, OutdoorAmbient = A } },
			{ name = "closed", lighting = { Brightness = 3, Ambient = A, OutdoorAmbient = O }, context = { Closed = true } },
			{ name = "ambient", lighting = { Ambient = A } },
			{ name = "outdoor", lighting = { OutdoorAmbient = O } },
			{ name = "sun", lighting = { Brightness = 3 } },
		}
	elseif Phase == "shapes" then
		-- one case a floor, 100 studs apart along X (multiples of the grid's 4 studs), what stands over it
		-- centred 10 studs up unless said
		local Cases: { any } = {
			{ Name = "open" },
			{ Name = "block", Size = Vector3.new(24, 4, 24) },
			{ Name = "brick", Size = Vector3.new(24, 4, 24), Mesh = "Brick" },
			{ Name = "brick-half", Size = Vector3.new(24, 4, 24), Mesh = "Brick", Scale = Vector3.new(0.5, 1, 0.5) },
			{ Name = "wedge", Size = Vector3.new(24, 12, 24), Mesh = "Wedge", Height = 14 },
			{ Name = "cylinder", Size = Vector3.new(24, 4, 24), Mesh = "Cylinder" },
			{ Name = "head", Size = Vector3.new(24, 4, 24), Mesh = "Head" },
			{ Name = "torso", Size = Vector3.new(24, 4, 24), Mesh = "Torso" },
			{ Name = "union-box", Union = "box" },
			{ Name = "union-l", Union = "l" },
			{ Name = "union-ring", Union = "ring" },
			{ Name = "quad", Quad = true },
			{ Name = "slab-turn30", Size = Vector3.new(24, 1, 24), Turn = 30 },
			{ Name = "slab-tilt20", Size = Vector3.new(24, 1, 24), Tilt = 20 },
			-- upright sheets 0.5 thick in the middle of each voxel column, 24 long along Z, through the
			-- layer from 8 to 12 (Sheet: length, height, centre height, turn about Y)
			{ Name = "sheets", Sheet = { 24, 4, 10, 0 } },
			{ Name = "sheets-turn30", Sheet = { 24, 4, 10, 30 } },
			{ Name = "sheets-turn45", Sheet = { 24, 4, 10, 45 } },
			{ Name = "sheets-2long", Sheet = { 2, 4, 10, 0 } },
			{ Name = "sheets-3long", Sheet = { 3, 4, 10, 0 } },
			{ Name = "sheets-4long", Sheet = { 4, 4, 10, 0 } },
			{ Name = "sheets-half", Sheet = { 24, 2, 10, 0 } },
			{ Name = "sheets-across", Sheet = { 24, 4, 12, 0 } },
		}
		for Index, Case in Cases do
			local X = (Index - 1) * 100
			table.insert(Layout.views, Down(Case.Name, X, 0, 7.5, 90))
			-- Context.Only builds that case alone (a renderer whose grid spans less than the row)
			if Context and Context.Only and Context.Only ~= Case.Name then
				continue
			end
			Box(`Floor-{Case.Name}`, Vector3.new(X, -0.5, 0), Vector3.new(64, 1, 64))
			local At = Vector3.new(X, Case.Height or 10, 0)
			if Case.Mesh then
				local Part = Box(`Roof-{Case.Name}`, At, Case.Size)
				local Mesh = Instance.new("SpecialMesh")
				Mesh.MeshType = Enum.MeshType[Case.Mesh]
				Mesh.Scale = Case.Scale or Vector3.one
				Mesh.Parent = Part
			elseif Case.Size then
				local CF = CFrame.new(At) * CFrame.Angles(math.rad(Case.Tilt or 0), math.rad(Case.Turn or 0), 0)
				Box(`Roof-{Case.Name}`, CF, Case.Size)
			elseif Case.Quad then
				Quad(`Roof-{Case.Name}`, At, 24)
			elseif Case.Union then
				-- 24 x 4 x 24 as two halves (box), an L of three quarters, or a block with a 12-stud hole
				-- (MakeUnion(parts, subtract) unions the parts, or subtracts the rest from the first)
				local Made = Context and Context.MakeUnion
				if Made then
					local Parts
					if Case.Union == "box" then
						Parts = { Span("A", Vector3.new(-12, -2, -12), Vector3.new(0, 2, 12)), Span("B", Vector3.new(0, -2, -12), Vector3.new(12, 2, 12)) }
					elseif Case.Union == "l" then
						Parts = { Span("A", Vector3.new(-12, -2, -12), Vector3.new(12, 2, 0)), Span("B", Vector3.new(-12, -2, 0), Vector3.new(0, 2, 12)) }
					else
						Parts = { Span("A", Vector3.new(-12, -2, -12), Vector3.new(12, 2, 12)), Span("B", Vector3.new(-6, -4, -6), Vector3.new(6, 4, 6)) }
					end
					local Union = Made(Parts, Case.Union == "ring")
					for _, Part in Parts do
						Part:Destroy()
					end
					if Union then
						Union.Name = `Roof-{Case.Name}`
						Union.Anchored = true
						Union.CFrame = CFrame.new(At) * Union.CFrame
						Union.Parent = Root
					end
				end
			elseif Case.Sheet then
				local Length, Height, Middle, Turn = table.unpack(Case.Sheet)
				for I = -3, 2 do
					for K = -3, 2 do
						-- long sheets: one a column of voxels along X; short ones: one a voxel
						if Length < 24 or K == 0 then
							local Centre = Vector3.new(X + I * 4 + 2, Middle, if Length < 24 then K * 4 + 2 else 0)
							Box(`Sheet-{Case.Name}-{I}-{K}`, CFrame.new(Centre) * CFrame.Angles(0, math.rad(Turn), 0), Vector3.new(0.5, Height, Length))
						end
					end
				end
			end
		end
		Layout.arms = { { name = "outdoor", lighting = { OutdoorAmbient = { 178, 178, 178 } } } }
	elseif Phase == "walls" then
		-- lg-specular's wall in the open, 80 long, 40 tall and 0.5 thick, its face towards -Z, and the same wall 2 and 1
		-- studs thick, 20 tall, and on a voxel boundary; each seen head-on from 45 studs (OutdoorAmbient alone: how
		-- much of the sky its face's grid sees, top to bottom)
		local Walls: { any } = {
			{ Name = "w05", Thick = 0.5, Tall = 40, Z = 6.5 },
			{ Name = "w1", Thick = 1, Tall = 40, Z = 6.5 },
			{ Name = "w2", Thick = 2, Tall = 40, Z = 6.5 },
			{ Name = "w05-tall20", Thick = 0.5, Tall = 20, Z = 6.5 },
			{ Name = "w05-edge", Thick = 0.5, Tall = 40, Z = 8 },
		}
		for Index, Wall in Walls do
			local X = (Index - 1) * 200
			Box(`Wall-{Wall.Name}`, Vector3.new(X, 0, Wall.Z), Vector3.new(80, Wall.Tall, Wall.Thick))
			table.insert(Layout.views, Look(Wall.Name, Vector3.new(X, 0, -45), Vector3.new(X, 0, 0), 40))
		end
		Layout.arms = { { name = "outdoor", lighting = { OutdoorAmbient = { 178, 178, 178 } } } }
	elseif Phase == "sky" then
		Box("Floor-roof", Vector3.new(0, -0.5, 0), Vector3.new(64, 1, 64))
		Span("Roof-roof", Vector3.new(-24, 8, -24), Vector3.new(24, 12, 24))
		Box("Floor-open", Vector3.new(100, -0.5, 0), Vector3.new(64, 1, 64))
		Layout.views = { Down("roof", 0, 0, 7.5, 70), Down("open", 100, 0, 7.5, 70), Down("roof-again", 0, 0, 7.49, 70) }
		Layout.arms = {
			{ name = "specular", lighting = { EnvironmentSpecularScale = 1, ExposureCompensation = 2 } },
			{ name = "diffuse", lighting = { EnvironmentDiffuseScale = 1 } },
		}
	elseif Phase == "crease" then
		-- ror's ao-depth crease: a white floor (top at 0) meeting a white wall (face at z = 1), a white unit
		-- cube a stud out, a black bar 0.5 off the wall and the floor, a white step 0.25 high
		local Options = { CastShadow = false }
		Span("Floor", Vector3.new(-8, -1, -8), Vector3.new(8, 0, 2), Options)
		Span("Wall", Vector3.new(-8, 0, 1), Vector3.new(8, 8, 2), Options)
		Span("Cube", Vector3.new(-1.75, 0, -1), Vector3.new(-0.75, 1, 0), Options)
		Span("Bar", Vector3.new(0.25, 0.5, 0), Vector3.new(0.75, 3.5, 0.5), { CastShadow = false, Color = Color3.new(0, 0, 0) })
		Span("Step", Vector3.new(1.25, 0, -2), Vector3.new(2.75, 0.25, -0.5), Options)
		local Target = Vector3.new(0, 0.9, -0.5)
		local Away = Vector3.new(0.3, 0.5, -1).Unit
		local Half = 8 * math.tan(math.rad(20))
		for _, Distance in { 3, 8, 20, 50, 100 } do
			table.insert(Layout.views, Look(`d{Distance}`, Target + Away * Distance, Target, 2 * math.deg(math.atan(Half / Distance))))
		end
		Layout.arms = { { name = "ambient", lighting = { Ambient = { 178, 178, 178 }, GlobalShadows = false } } }
	elseif Phase == "contacts" then
		local Options = { Color = Color3.fromRGB(200, 200, 200) }
		Box("Floor", Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60), Options)
		Box("Wall", Vector3.new(0, 4, 10.5), Vector3.new(30, 8, 1), Options)
		Box("Ball", Vector3.new(-8, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball })
		Box("Cube", Vector3.new(0, 2, 0), Vector3.new(4, 4, 4))
		Box("Plate", Vector3.new(8, 3, 0), Vector3.new(6, 0.5, 6))
		Box("Pole", Vector3.new(0, 4, 6), Vector3.new(1, 8, 1))
		Layout.views = { Down("down", 0, 2, 30, 50), Look("side", Vector3.new(0, 10, -20), Vector3.new(0, 1, 3), 60) }
		local A, O = { 120, 120, 120 }, { 120, 120, 120 }
		for _, Style in { "Realistic", "Soft" } do
			table.insert(Layout.arms, { name = `ambient-{Style}`, style = Style, lighting = { Ambient = A } })
			table.insert(Layout.arms, { name = `outdoor-{Style}`, style = Style, lighting = { OutdoorAmbient = O } })
			table.insert(Layout.arms, { name = `sky-{Style}`, style = Style, lighting = { EnvironmentDiffuseScale = 1 } })
			table.insert(Layout.arms, {
				name = `day-{Style}`,
				style = Style,
				lighting = { Brightness = 2, Ambient = { 70, 70, 70 }, OutdoorAmbient = { 128, 128, 128 }, EnvironmentDiffuseScale = 1, EnvironmentSpecularScale = 1 },
			})
		end
	elseif Phase == "roof" then
		for _, Case in { { "block", -100 }, { "quad", 100 } } do
			local Name, X = Case[1], Case[2]
			Box(`Floor-{Name}`, Vector3.new(X, -0.5, 0), Vector3.new(120, 1, 120))
			if Name == "block" then
				Box(`Roof-{Name}`, Vector3.new(X, 16.5, 0), Vector3.new(48, 1, 48))
			else
				Quad(`Roof-{Name}`, Vector3.new(X, 16.5, 0), 48)
			end
			table.insert(Layout.views, Down(`{Name}-under`, X, 0, 15, 90))
			table.insert(Layout.views, Look(`{Name}-side`, Vector3.new(X + 60, 40, -60), Vector3.new(X + 10, 0, 0), 60))
		end
		Layout.arms = {
			{ name = "sun", lighting = { Brightness = 3 } },
			{ name = "outdoor", lighting = { OutdoorAmbient = { 128, 128, 128 } } },
		}
	end
	Root.Parent = workspace
	return Layout
end
