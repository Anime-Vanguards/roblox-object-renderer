-- The sun's shadow maps: what a cascade's filter does to patterns, which parts cast through
-- LocalTransparencyModifier, and how far each cascade reaches.
--
-- Context.Phase "patterns" (the default): cards 3 studs over a grey floor, 6 studs square, each cut into
-- a pattern either by its colour map's alpha (the install's quad mesh under a SurfaceAppearance in
-- AlphaMode Transparency, an EditableImage of alpha 0 and 255) or by building the pattern from thin Parts:
-- checkerboards of 0.75, 1.5 and 3-stud squares and 1.5-stud stripes across u and across v, beside a
-- whole card; then 4 x 0.2 x 4 plates whose LocalTransparencyModifier is set (a client's first-person
-- camera hides its character this way). Each group lies 40 studs from the next, so a camera straight down
-- over one sees only its own.
-- Context.Phase "posts": a 2,000-stud floor under a grid of posts 12 studs tall and Context.Width (1) wide
-- every Context.Spacing (25) studs, whose long shadows show by their blur which cascade draws them.
-- Context.Phase "plates": the floor under a grid of 6-stud plates 1.5 and 5 studs up, whose shadows show
-- from far off where the last cascade ends and how much each cascade leaks under a plate.
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic) under ClockTime 16.2,
-- GeographicLatitude 38, Brightness 2, Ambient 0.3 grey, OutdoorAmbient black and EnvironmentDiffuseScale
-- and EnvironmentSpecularScale 0 (ShadowSoftness 0.2, and 1 for the posts), against ror's render of the
-- same:
-- ror render fixtures/sun-shadows.luau --lighting-from <a place with that Lighting> --size 1600x900 --   --camera '{"mode":"lookAt","eye":[0,55,-0.05],"target":[0,0,0],"fov":40}' --no-service --   --context '{"Phase":"patterns","Mesh":"file:///<Studio>/content/avatar/compositing/CompositQuad.mesh"}'
-- (docs/rendering.md "Sun shadows"; ROR_REE: the engine emulator's checkout). It returns each case's name
-- and where to read it on the floor: the middles of its shadowed squares and holes, or a plate's shadow
-- and a lit point beside it, or a post's shadow and the floor beside that.
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Phase = Context and Context.Phase or "patterns"
	local Root = Instance.new("Model")
	Root.Name = "SunShadows"

	-- shadows fall along -Lighting:GetSunDirection() at ClockTime 16.2 and latitude 38 (fixed here, so the
	-- scene is the same whatever the Lighting it is built under)
	local Sun = Vector3.new(-0.862626, 0.439530, 0.250380)
	local function Shadow(Point)
		return Point - Sun * (Point.Y / Sun.Y)
	end

	local Layout = {}
	local function Entry(Name, Kind, Point, Extra)
		local Row = { name = Name, kind = Kind, x = Point.X, y = Point.Y, z = Point.Z }
		for Key, Value in Extra or {} do
			Row[Key] = Value
		end
		table.insert(Layout, Row)
	end
	local function Block(Name, Size, CF, Options)
		Options = Options or {}
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Options.Color or Color3.fromRGB(60, 140, 60)
		Part.Size = Size
		Part.CFrame = CF
		Part.Transparency = Options.Transparency or 0
		Part.CastShadow = Options.CastShadow ~= false
		Part.Parent = Root
		return Part
	end

	if Phase == "posts" or Phase == "plates" or Phase == "leaks" then
		-- four floor tiles (a Part is at most 2,048 studs a side), white so far shadows still read
		for _, Corner in { Vector3.new(-1, 0, -1), Vector3.new(1, 0, -1), Vector3.new(-1, 0, 1), Vector3.new(1, 0, 1) } do
			Block("Floor", Vector3.new(1000, 1, 1000), CFrame.new(Corner.X * 500, -0.5, Corner.Z * 500 + 900), { Color = Color3.fromRGB(200, 200, 200) })
		end
	end
	if Phase == "leaks" then
		-- rows of 6 x 0.4 x 6 plates every 20 studs along +Z, each row six plates 1, 2, 3, 4, 6 and 8 studs
		-- over the floor 30 studs apart, twice (from x = -190 and x = 40): what each cascade lets through
		-- under a plate Δ = height / sin(sun's elevation) in front of the floor
		local HEIGHTS = { 1, 2, 3, 4, 6, 8 }
		for Row = 0, 32 do
			for Copy, Start in { -190, 40 } do
				for Index, Height in HEIGHTS do
					Block(`leak{Copy},{Row},{Height}`, Vector3.new(6, 0.4, 6), CFrame.new(Start + (Index - 1) * 30, Height + 0.2, Row * 20), { Color = Color3.fromRGB(40, 40, 40) })
				end
			end
		end
		Root.Parent = workspace
		return { layout = Layout, leaks = { rows = 33, spacing = 20, starts = { -190, 40 }, step = 30, heights = HEIGHTS } }
	end
	if Phase == "plates" then
		-- 6 x 0.4 x 6 plates every Context.Spacing (25) studs, alternately 1.5 and 5 studs over the floor:
		-- a shadow's presence shows the last cascade's reach, its depth the leak each cascade lets under a
		-- plate. Plate (i, j) stands at (i, h, j) x Spacing - (0, 0, 40), i = -count..count, j = 0..2 count,
		-- h 1.5 where i + j is even and 5 where odd
		local Spacing = Context.Spacing or 25
		local Count = math.floor(900 / Spacing)
		for I = -Count, Count do
			for J = 0, 2 * Count do
				local Height = if (I + J) % 2 == 0 then 1.5 else 5
				Block(`plate{I},{J}`, Vector3.new(6, 0.4, 6), CFrame.new(I * Spacing, Height + 0.2, J * Spacing - 40), { Color = Color3.fromRGB(40, 40, 40) })
			end
		end
		Root.Parent = workspace
		return { layout = Layout, plates = { spacing = Spacing, count = Count, heights = { 1.5, 5 } } }
	end
	if Phase == "posts" then
		local Spacing = Context.Spacing or 25
		local Width = Context.Width or 1
		local Height = 12
		-- a post's shadow runs from its foot along the sun's way across the floor; its middle, and the floor
		-- 3 studs beside it (across the shadow, away from the next post's)
		local Along = Vector3.new(-Sun.X, 0, -Sun.Z).Unit
		local Across = Vector3.new(-Along.Z, 0, Along.X)
		local Count = math.floor(950 / Spacing)
		for I = -Count, Count do
			for J = 0, 2 * Count do
				local Foot = Vector3.new(I * Spacing, 0, J * Spacing - 40)
				Block(`post{I},{J}`, Vector3.new(Width, Height, Width), CFrame.new(Foot + Vector3.new(0, Height / 2, 0)), { Color = Color3.fromRGB(40, 40, 40) })
			end
		end
		-- (too many to list: post (i, j) stands at (i, 0, j) x Spacing - (0, 0, 40), i = -count..count, j =
		-- 0..2 count)
		local Middle = Shadow(Vector3.new(0, Height / 2, 0))
		Root.Parent = workspace
		return {
			layout = Layout,
			posts = { spacing = Spacing, width = Width, count = Count, height = Height, shadow = { Middle.X, Middle.Z }, across = { Across.X, Across.Z } },
		}
	end

	Block("Floor", Vector3.new(400, 1, 400), CFrame.new(0, -0.5, 0), { Color = Color3.fromRGB(200, 200, 200) })

	-- the install's quad mesh (1024 units square in XY from its origin, facing +Z, u along +X, v down +Y)
	-- turned to face up, u along +X and v along +Z, its middle moved onto the card's position; the engine
	-- emulator loads it from a file:// path (Context.Mesh)
	local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local FACE_UP = CFrame.Angles(-math.pi / 2, 0, 0)
	local SIDE = 6
	local HEIGHT = 3

	-- an image (64 x 64) of alpha 255 where Opaque(u, v) holds and 0 elsewhere, u and v in 0..1
	local function Image(Opaque)
		local Pixels = buffer.create(64 * 64 * 4)
		for Y = 0, 63 do
			for X = 0, 63 do
				local Index = (Y * 64 + X) * 4
				buffer.writeu8(Pixels, Index, 60)
				buffer.writeu8(Pixels, Index + 1, 140)
				buffer.writeu8(Pixels, Index + 2, 60)
				buffer.writeu8(Pixels, Index + 3, if Opaque((X + 0.5) / 64, (Y + 0.5) / 64) then 255 else 0)
			end
		end
		local Editable = AssetService:CreateEditableImage({ Size = Vector2.new(64, 64) })
		Editable:WritePixelsBuffer(Vector2.zero, Editable.Size, Pixels)
		return Editable
	end
	local function Card(Name, Position, Map)
		local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Color3.fromRGB(60, 140, 60)
		Part.Size = Vector3.new(SIDE, SIDE, 0.001)
		Part.CFrame = CFrame.new(Position) * FACE_UP * CFrame.new(-SIDE / 2, -SIDE / 2, 0)
		local Surface = AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Map) })
		Surface.AlphaMode = Enum.AlphaMode.Transparency
		Surface.Parent = Part
		Part.Parent = Root
	end

	-- patterns in cells of `Cells` x `Cells` across the card: checkerboards (the corner cell at u, v near 0
	-- opaque) and stripes (every other column of u, or row of v, opaque from the first)
	local PATTERNS = {
		{ Name = "checker0.75", Cells = 8, Opaque = function(I, J) return (I + J) % 2 == 0 end },
		{ Name = "checker1.5", Cells = 4, Opaque = function(I, J) return (I + J) % 2 == 0 end },
		{ Name = "checker3", Cells = 2, Opaque = function(I, J) return (I + J) % 2 == 0 end },
		{ Name = "stripes-u", Cells = 4, Opaque = function(I, _) return I % 2 == 0 end },
		{ Name = "stripes-v", Cells = 4, Opaque = function(_, J) return J % 2 == 0 end },
		{ Name = "whole", Cells = 1, Opaque = function() return true end },
	}
	local Groups = {}
	for Index, Pattern in PATTERNS do
		for Kind, Built in { "alpha", "parts" } do
			local Name = `{Built}-{Pattern.Name}`
			-- a 6 x 2 grid of groups, alpha cards in the first row and Parts in the second
			local Position = Vector3.new(-100 + (Index - 1) * 40, HEIGHT, -20 + (Kind - 1) * 40)
			local Cell = SIDE / Pattern.Cells
			local Corner = Position - Vector3.new(SIDE / 2, 0, SIDE / 2)
			if Built == "alpha" then
				Card(Name, Position, Image(function(U, V)
					return Pattern.Opaque(math.floor(U * Pattern.Cells), math.floor(V * Pattern.Cells))
				end))
			end
			for I = 0, Pattern.Cells - 1 do
				for J = 0, Pattern.Cells - 1 do
					local Middle = Corner + Vector3.new((I + 0.5) * Cell, 0, (J + 0.5) * Cell)
					local Opaque = Pattern.Opaque(I, J)
					if Built == "parts" and Opaque then
						Block(`{Name}-{I},{J}`, Vector3.new(Cell, 0.05, Cell), CFrame.new(Middle))
					end
					Entry(Name, if Opaque then "square" else "hole", Shadow(Middle), { group = Name })
				end
			end
			Entry(Name, "lit", Shadow(Position) + Vector3.new(0, 0, SIDE + 2), { group = Name })
			table.insert(Groups, { name = Name, x = Shadow(Position).X, z = Shadow(Position).Z })
		end
	end

	-- plates whose LocalTransparencyModifier is set: the shadow's cut is at 1 - (1 - Transparency)(1 -
	-- modifier) = 0.748, and the plate draws faded as by that transparency. The first row's is set before
	-- the plate is parented, the second's a frame after (a file keeps no modifier: an engine emulator's
	-- recording carries it as the Transparency drawn once it changes)
	local MODIFIED = {
		{ 0, 0.7479 }, { 0, 0.748 }, { 0.5, 0.4958 }, { 0.5, 0.496 }, { 0, 0.5 }, { 0.5, 0.5 }, { 0, 1 }, { 0, 0 },
	}
	local Late = {}
	for Row, When in { "early", "late" } do
		for Index, Case in MODIFIED do
			local Name = `ltm-{When}-t{Case[1]}-m{Case[2]}`
			local Position = Vector3.new(-100 + (Index - 1) * 20, HEIGHT, 70 + (Row - 1) * 20)
			local Plate = Block(Name, Vector3.new(4, 0.2, 4), CFrame.new(Position), { Color = Color3.fromRGB(230, 40, 40), Transparency = Case[1] })
			if When == "early" then
				Plate.LocalTransparencyModifier = Case[2]
			else
				Late[Plate] = Case[2]
			end
			Entry(Name, "shadow", Shadow(Position - Vector3.new(0, 0.1, 0)), { group = "ltm" })
			Entry(Name, "lit", Shadow(Position) + Vector3.new(0, 0, 4.5), { group = "ltm" })
			Entry(Name, "plate", Position + Vector3.new(0, 0.1, 0), { group = "ltm" })
		end
	end
	table.insert(Groups, { name = "ltm", x = -30, z = 80 })

	Root.Parent = workspace
	task.wait()
	for Plate, Modifier in Late do
		Plate.LocalTransparencyModifier = Modifier
	end
	task.wait()
	return { layout = Layout, groups = Groups }
end
