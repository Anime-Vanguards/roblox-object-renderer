-- Post effects (notes/render.md "Post effects", "Fog"): each group on its own, 20,000 studs from the next,
-- so a camera on one sees nothing of another. Context.Group builds one group alone ("blur", "rays", "fog",
-- "dof", "halo"); without it, all of them. Every part is anchored, casts no shadow and is lit by a flat
-- Ambient alone (Lighting Brightness, OutdoorAmbient and the environment scales 0): white SmoothPlastic
-- shows x = 2 c^2 2^EV.
--
-- "blur" (x 0): over a black Neon backdrop, seen from 60 studs down +Z through a 40 degree field of view
--   (25.55 px a stud on a 1116 px tall view): a row of white vertical lines 1 to 8 px wide (two of each
--   width at other pixel phases), a row of white squares 1 to 32 px, and 6-stud squares of grays 255, 200,
--   150, 100 and 50. BlurEffect Sizes 0 to 64 on them show how much of a feature's light the blur keeps
--   and in which space it blurs.
-- "rays" (x 20000, y 4000): nothing but the sky (a black Sky without its sun and moon); the camera looks
--   near the sun so it lands at chosen places on and off the view: SunRaysEffect's light alone (the
--   sky shows about 24 of 255 at EV 0, subtracted).
-- "fog" (x 40000): looking down +Z at the dark sky: black Neon panels 30 to 1,920 studs from the camera
--   along the top (each 0.14 of its distance wide: the fog alone), and along the bottom black legacy Smoke
--   and black ParticleEmitter particles 90 and 300 studs away with nothing behind them but the sky (each
--   a blob of many stacked particles: opaque until the fog nearly hides one, so they show whether fog
--   lends its colour, not how a single particle fades).
-- "dof" (x 60000): seen from the group's origin down +Z through 50 degrees, checkered tiles (2 x 2 white and
--   black squares, 0.08 of their depth wide, so they look alike) at 8 to 400 studs before a black Neon
--   backdrop 1,500 studs out: DepthOfFieldEffect's blur by depth.
-- "halo" (x 80000): the same camera; three lanes, each a white and a black in-focus tile 20 studs out
--   before a gray slab 60, 200 or 400 studs out: the halo an in-focus outline leaves on a blurred
--   background.
--
-- Measured in a Roblox client (0.747, quality level 21, 1644 x 1116, lossless frames) against ror's render
-- of the same place from the camera the client returned. Returns each group's items: name, centre and
-- size (studs), and what they are.
return function(Context)
	local Only = Context and Context.Group
	local Root = Instance.new("Model")
	Root.Name = "PostEffects"
	local Groups = {}

	local function Block(Name, Position, Size, Color, Material, Transparency)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.CastShadow = false
		Part.Material = Material or Enum.Material.SmoothPlastic
		Part.Color = Color
		Part.Size = Size
		Part.Transparency = Transparency or 0
		Part.CFrame = CFrame.new(Position)
		Part.Parent = Root
		return Part
	end
	local function Group(Name, Center)
		if Only and Only ~= Name then
			return nil
		end
		local Entry = { name = Name, center = { Center.X, Center.Y, Center.Z }, items = {} }
		Groups[Name] = Entry
		return Entry
	end
	local function Item(Entry, Name, Position, Size, Extra)
		local Row = { name = Name, center = { Position.X, Position.Y, Position.Z }, size = { Size.X, Size.Y, Size.Z } }
		for Key, Value in Extra or {} do
			Row[Key] = Value
		end
		table.insert(Entry.items, Row)
	end
	local Black = Color3.new(0, 0, 0)
	local White = Color3.new(1, 1, 1)

	-- blur: features of known width in pixels at 25.55 px a stud; the tiles' faces at z = -0.1
	do
		local Center = Vector3.new(0, 0, 0)
		local Entry = Group("blur", Center)
		if Entry then
			local Pixel = 1 / 25.55
			Block("Backdrop", Center + Vector3.new(0, 0, 6), Vector3.new(200, 120, 1), Black, Enum.Material.Neon)
			local Widths = { 1, 2, 3, 4, 6, 8 }
			for Index = 0, 11 do
				local Width = Widths[Index % 6 + 1]
				-- the second of each width an eighth of a pixel further along per line: other phases
				local Position = Center + Vector3.new((Index - 5.5) * 4 + Index * Pixel / 8, 11, 0)
				local Size = Vector3.new(Width * Pixel, 6, 0.2)
				Block(`line{Index}`, Position, Size, White)
				Item(Entry, `line{Index}`, Position - Vector3.new(0, 0, 0.1), Size, { kind = "line", px = Width })
			end
			local Dots = { 1, 2, 3, 4, 6, 8, 12, 16, 24, 32 }
			for Index, Side in Dots do
				local Position = Center + Vector3.new((Index - 5.5) * 5, 3, 0)
				local Size = Vector3.new(Side * Pixel, Side * Pixel, 0.2)
				Block(`dot{Side}`, Position, Size, White)
				Item(Entry, `dot{Side}`, Position - Vector3.new(0, 0, 0.1), Size, { kind = "dot", px = Side })
			end
			for Index, Gray in { 255, 200, 150, 100, 50 } do
				local Position = Center + Vector3.new((Index - 3) * 9, -8, 0)
				local Size = Vector3.new(6, 6, 0.2)
				Block(`square{Gray}`, Position, Size, Color3.fromRGB(Gray, Gray, Gray))
				Item(Entry, `square{Gray}`, Position - Vector3.new(0, 0, 0.1), Size, { kind = "square", gray = Gray })
			end
		end
	end

	-- rays: the sky alone (an anchor far below keeps the model from being empty)
	do
		local Center = Vector3.new(20000, 4000, 0)
		local Entry = Group("rays", Center)
		if Entry then
			Block("Anchor", Center - Vector3.new(0, 3000, 0), Vector3.new(0.2, 0.2, 0.2), Black, Enum.Material.Neon, 1)
		end
	end

	-- fog: panels along the top, Smoke and particles along the bottom, nothing behind them but the sky
	do
		local Center = Vector3.new(40000, 0, 0)
		local Entry = Group("fog", Center)
		if Entry then
			local Eye = Center - Vector3.new(0, 0, 60)
			local Distances = { 30, 60, 120, 240, 480, 960, 1920 }
			for Index, Distance in Distances do
				-- 0.14 rad wide, side by side 8.5 degrees apart, 7 degrees above the view's middle
				local Angle = math.rad((Index - 4) * 8.5)
				local Direction = Vector3.new(math.sin(Angle), math.tan(math.rad(7)), math.cos(Angle))
				local Position = Eye + Direction * Distance
				local Size = Vector3.new(0.14 * Distance, 0.08 * Distance, 0.01 * Distance)
				local Panel = Block(`panel{Distance}`, Position, Size, Black, Enum.Material.Neon)
				Panel.CFrame = CFrame.lookAt(Position, Position + Vector3.new(Direction.X, 0, Direction.Z))
				Item(Entry, `panel{Distance}`, Position, Size, { kind = "panel", distance = Distance })
			end
			-- (name, distance, angle from the middle in degrees): legacy Smoke and ParticleEmitter blobs
			for Index, Case in { { "smoke", 90, -18 }, { "smoke", 300, -6 }, { "particles", 90, 6 }, { "particles", 300, 18 } } do
				local Kind, Distance, Degrees = Case[1], Case[2], Case[3]
				local Angle = math.rad(Degrees)
				local Direction = Vector3.new(math.sin(Angle), -math.tan(math.rad(9)), math.cos(Angle))
				local Position = Eye + Direction * Distance
				local Holder = Block(`{Kind}{Distance}`, Position, Vector3.new(0.2, 0.2, 0.2), Black, Enum.Material.SmoothPlastic, 1)
				-- about 0.06 of the distance across on screen
				local Scale = Distance / 90
				if Kind == "smoke" then
					local Smoke = Instance.new("Smoke")
					Smoke.Color = Black
					Smoke.Opacity = 1
					Smoke.Size = 3 * Scale
					Smoke.RiseVelocity = 0.5 * Scale
					Smoke.Parent = Holder
				else
					local Emitter = Instance.new("ParticleEmitter")
					Emitter.Color = ColorSequence.new(Black)
					Emitter.LightEmission = 0
					Emitter.LightInfluence = 0
					Emitter.Transparency = NumberSequence.new(0)
					Emitter.Size = NumberSequence.new(1.5 * Scale)
					Emitter.Speed = NumberRange.new(0)
					Emitter.Rate = 40
					Emitter.Lifetime = NumberRange.new(5)
					Emitter.SpreadAngle = Vector2.new(0, 0)
					Emitter.Texture = "rbxasset://textures/whiteCircle.png"
					Emitter.Parent = Holder
				end
				Item(Entry, `{Kind}{Distance}`, Position, Vector3.new(4, 4, 4) * Scale, { kind = Kind, distance = Distance })
			end
		end
	end

	-- dof: checkered tiles by depth, on a 4 x 3 grid of directions
	local function Checker(Name, Position, Side, Facing)
		local Half = Side / 2
		local Right = Facing:Cross(Vector3.new(0, 1, 0)).Unit
		local Up = Right:Cross(Facing).Unit
		for Row = 0, 1 do
			for Column = 0, 1 do
				local Offset = Right * ((Column - 0.5) * Half) + Up * ((0.5 - Row) * Half)
				local Part = Block(`{Name}_{Row}{Column}`, Position + Offset, Vector3.new(Half, Half, Side * 0.02), if (Row + Column) % 2 == 0 then White else Black)
				Part.CFrame = CFrame.lookAt(Position + Offset, Position + Offset - Facing)
			end
		end
	end
	do
		local Center = Vector3.new(60000, 0, 0)
		local Entry = Group("dof", Center)
		if Entry then
			Block("Backdrop", Center + Vector3.new(0, 0, 1500), Vector3.new(4000, 4000, 1), Black, Enum.Material.Neon)
			local Depths = { 8, 12, 16, 20, 30, 50, 75, 100, 150, 200, 300, 400 }
			local Tan = math.tan(math.rad(25))
			for Index, Depth in Depths do
				local Column, Row = (Index - 1) % 4, (Index - 1) // 4
				local Tangent = Vector3.new((Column - 1.5) * 0.3 * Tan * 1.473, (1 - Row) * 0.6 * Tan, 1)
				local Position = Center + Tangent * Depth
				Checker(`tile{Depth}`, Position, 0.08 * Depth, Tangent.Unit)
				Item(Entry, `tile{Depth}`, Position, Vector3.new(0.08, 0.08, 0) * Depth, { kind = "tile", depth = Depth })
			end
		end
	end

	-- halo: in-focus tiles 20 studs out before gray slabs 60, 200 and 400 out
	do
		local Center = Vector3.new(80000, 0, 0)
		local Entry = Group("halo", Center)
		if Entry then
			local Tan = math.tan(math.rad(25))
			for Lane, Depth in { 60, 200, 400 } do
				local X = (Lane - 2) * 0.3 * Tan * 1.473
				-- the slab: a third of the view wide, the view's height, gray 128 (x 0.5)
				local Slab = Center + Vector3.new(X, 0, 1) * Depth
				Block(`slab{Depth}`, Slab, Vector3.new(0.29 * Tan * 1.473 * Depth, 1.8 * Tan * Depth, 0.01 * Depth), Color3.fromRGB(128, 128, 128))
				Item(Entry, `slab{Depth}`, Slab, Vector3.new(0.29 * Tan * 1.473, 1.8 * Tan, 0) * Depth, { kind = "slab", depth = Depth })
				for Index, Color in { White, Black } do
					local Position = Center + Vector3.new(X, (Index == 1 and 0.25 or -0.25) * Tan, 1) * 20
					local Size = Vector3.new(0.1 * Tan * 20, 0.2 * Tan * 20, 0.05)
					Block(`tile{Depth}_{Index}`, Position, Size, Color)
					Item(Entry, `tile{Depth}_{Index}`, Position - Vector3.new(0, 0, 0.025), Size, { kind = "tile", depth = 20, before = Depth, white = Index == 1 })
				end
			end
		end
	end

	Root.Parent = workspace
	return { groups = Groups, root = Root }
end
