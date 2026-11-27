-- Draw order and effect culling in a Roblox client (notes/render.md "Blend order", "Beams", "Trails", "Legacy
-- effects"). Each group stands 1,000 studs from the next along X. Effects are unlit (LightInfluence 0) white or
-- coloured squares (SquareParticle), solid beams and trails; parts are lit by a flat Ambient. Emitters have Rate 0:
-- a view emits the counts it names (`Emit`, paths under the group), lets them age (`Age`) and freezes them
-- (TimeScale 0). Movers named in `Move` slide from `From` to `To` (relative to where they stand) over `Duration`
-- seconds, set every frame; the view captures `After` seconds after they stop.
--
-- "legacy" (x 0, seen from -Z): Fires (Size 3, Heat 5) 2.5 studs behind and in front of 5 x 6 white SmoothPlastic
--   panes at Transparency 0.5, behind panes at 0.2 and 0.8, behind a Glass pane, 10 studs behind a pane, behind and
--   in front of a ParticleEmitter square and behind a beam (both gray, Transparency 0.5), a Smoke behind a pane, and
--   a Fire alone: where the client composites a Fire among what blends.
-- "zcut" (x 1000, a level camera looking along +Z): ParticleEmitter squares of Size 3 at ZOffset 30 whose depth
--   runs 26 to 32 in steps of 0.5 (columns): facing the camera (top row), flat (VelocityPerpendicular moving up,
--   bottom row) and tilted 45 degrees towards the camera (middle row); a gray plate at depth 15 (a SpecialMesh
--   brick: no occluder) covers each one's left half, so a square drawn at its ZOffset place shows over it and one
--   drawn at its own place under it.
-- "ties" (x 2000 on, 60 studs apart, each seen head-on from 20 studs and turned): two solid beams crossing on one
--   midpoint (red first in the file, then green first), two overlapping beams across the look on one plane, two
--   overlapping squares at one depth (equal Sizes and unequal), and two panes at Transparency 0.5 and 0.02 whose
--   order by depth along the look differs from their order by distance from the eye.
-- "trails" (x 3000 on): two solid trails on one pair of attachments (red first in the file, then green first),
--   left by a 12-stud slide, seen head-on, obliquely and along the slide; and trails along the view (head near,
--   head far) behind gray panes at Transparency 0.5 at five depths: which point a trail sorts by.
-- "cull" (x 4000 on, 100 studs apart): a beam and a square 36 studs out behind a 10 x 10 opaque block at 30 with
--   ZOffset 10 (in front of it once offset), the same behind a SpecialMesh brick (no occluder), squares that flew
--   from behind the block to in front of it, a beam behind the block bulging in front of it by its curve, and a
--   beam reaching from behind the block to in front of it: what the client's occlusion culling drops.
-- "minlength" (x 6000): trails sliding at 60 studs a second 60 studs off (MinLength 2, 4 and 0) and at 180 studs a
--   second 600 studs off (MinLength 6 and 0), Transparency 1 at the head and the tail and 0 mid-life, Lifetime 1,
--   captured mid-slide; each mover a small red block, its place on screen the moment captured.
--
-- Measured in a Roblox client (0.747, quality level 21, 1644 x 1116) against ror's render of the same place: the
-- job returns each view's camera; ror's side emits the same counts (`--emit <path>=<count>@0`), plays the same
-- motion (`--playback`) and draws them at the client's moment (`--time`).
return function(Context)
	local Root = Instance.new("Model")
	Root.Name = "P19Probes"
	local SQUARE = "rbxasset://textures/particles/SquareParticle.png"
	local WHITE, GRAY = Color3.new(1, 1, 1), Color3.new(0.5, 0.5, 0.5)
	local RED, GREEN = Color3.new(1, 0, 0), Color3.new(0, 1, 0)
	local Views = {}
	local Groups = {}

	local function V3(v)
		return { v.X, v.Y, v.Z }
	end
	local function Group(Name)
		local Model = Instance.new("Model")
		Model.Name = Name
		Model.Parent = Root
		Groups[Name] = Model
		return Model
	end
	local function View(Group, Tag, Eye, Target, Fov, Arm, Extra)
		local Entry = { Tag = Tag, Group = Group, Eye = V3(Eye), Target = V3(Target), Fov = Fov, Arm = Arm or "flat" }
		for Key, Value in Extra or {} do
			Entry[Key] = Value
		end
		table.insert(Views, Entry)
	end
	local function Block(Parent, Name, CF, Size, Color, Material, Transparency)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.CastShadow = false
		Part.Material = Material or Enum.Material.SmoothPlastic
		Part.Color = Color or Color3.new(0, 0, 0)
		Part.Size = Size
		Part.CFrame = CF
		Part.Transparency = Transparency or 0
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.Parent = Parent
		return Part
	end
	-- an opaque brick the client's occlusion culling does not rasterize (a SpecialMesh's part)
	local function Plate(Parent, Name, CF, Size, Color)
		local Part = Block(Parent, Name, CF, Size, Color)
		local Mesh = Instance.new("SpecialMesh")
		Mesh.MeshType = Enum.MeshType.Brick
		Mesh.Parent = Part
		return Part
	end
	local function Constant(Value)
		return NumberSequence.new({ NumberSequenceKeypoint.new(0, Value), NumberSequenceKeypoint.new(1, Value) })
	end
	local function Holder(Parent, Name, Position, Rotation)
		local Part = Block(Parent, Name, CFrame.new(Position), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
		local At = Instance.new("Attachment")
		At.Name = "At"
		At.CFrame = Rotation or CFrame.new()
		At.Parent = Part
		return At, Part
	end
	local function Emitter(Parent, Name, Set)
		local E = Instance.new("ParticleEmitter")
		E.Name = Name
		E.Rate = 0
		E.Lifetime = NumberRange.new(100, 100)
		E.Speed = NumberRange.new(0, 0)
		E.SpreadAngle = Vector2.new(0, 0)
		E.Rotation = NumberRange.new(0, 0)
		E.RotSpeed = NumberRange.new(0, 0)
		E.Size = Constant(1)
		E.Transparency = Constant(0)
		E.Color = ColorSequence.new(WHITE)
		E.LightEmission = 0
		E.LightInfluence = 0
		E.Brightness = 1
		E.Texture = SQUARE
		E.Drag = 0
		E.Acceleration = Vector3.new(0, 0, 0)
		E.EmissionDirection = Enum.NormalId.Top
		for Key, Value in Set or {} do
			E[Key] = Value
		end
		E.Parent = Parent
		return E
	end
	local function Beam(Parent, Name, A0, A1, Set)
		local B = Instance.new("Beam")
		B.Name = Name
		B.Attachment0 = A0
		B.Attachment1 = A1
		B.Color = ColorSequence.new(WHITE)
		B.Transparency = Constant(0)
		B.Width0 = 1
		B.Width1 = 1
		B.FaceCamera = true
		B.Segments = 10
		B.LightEmission = 0
		B.LightInfluence = 0
		B.Brightness = 1
		B.Texture = ""
		for Key, Value in Set or {} do
			B[Key] = Value
		end
		B.Parent = Parent
		return B
	end
	-- two attachments on one invisible holder part at `Position`, at local `P0` and `P1`
	local function Pair(Parent, Name, Position, P0, P1)
		local Part = Block(Parent, Name, CFrame.new(Position), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
		local A0 = Instance.new("Attachment")
		A0.Name = "A0"
		A0.CFrame = CFrame.new(P0)
		A0.Parent = Part
		local A1 = Instance.new("Attachment")
		A1.Name = "A1"
		A1.CFrame = CFrame.new(P1)
		A1.Parent = Part
		return A0, A1, Part
	end
	local function Trail(Parent, Name, A0, A1, Set)
		local T = Instance.new("Trail")
		T.Name = Name
		T.Attachment0 = A0
		T.Attachment1 = A1
		T.Color = ColorSequence.new(WHITE)
		T.Transparency = Constant(0)
		T.WidthScale = Constant(1)
		T.Lifetime = 30
		T.MinLength = 0
		T.FaceCamera = false
		T.LightEmission = 0
		T.LightInfluence = 0
		T.Brightness = 1
		T.Texture = ""
		for Key, Value in Set or {} do
			T[Key] = Value
		end
		T.Parent = Parent
		return T
	end

	-- legacy: a Fire among what blends
	do
		local O = Vector3.new(0, 0, 0)
		local G = Group("legacy")
		Block(G, "Backdrop", CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
		-- (name, what covers or backs it, its offset along Z from the cover (+: behind), the cover's Transparency, material)
		local Cases = {
			{ "bare", nil, 0 },
			{ "behind", "pane", 2.5, 0.5 },
			{ "front", "pane", -2.5, 0.5 },
			{ "behind02", "pane", 2.5, 0.2 },
			{ "behind08", "pane", 2.5, 0.8 },
			{ "behindfar", "pane", 10, 0.5 },
			{ "glass", "pane", 2.5, 0.5, Enum.Material.Glass },
			{ "sqbehind", "square", 2.5 },
			{ "sqfront", "square", -2.5 },
			{ "beam", "beam", 2.5 },
			{ "smoke", "pane", 2.5, 0.5, nil, "Smoke" },
			{ "bare2", nil, 0 },
		}
		for Index, Case in Cases do
			local Column = (Index - 1) % 6
			local Row = (Index - 1) // 6
			local At = O + Vector3.new((Column - 2.5) * 7, 4 - Row * 13, 0)
			local Name, Cover, Offset = Case[1], Case[2], Case[3]
			if Cover == "pane" then
				Block(G, `{Name}-pane`, CFrame.new(At), Vector3.new(5, 6, 0.2), WHITE, Case[5] or Enum.Material.SmoothPlastic, Case[4])
			elseif Cover == "square" then
				local Sq = Holder(G, `{Name}-sqholder`, At)
				Emitter(Sq, `{Name}-sq`, { Size = Constant(5), Color = ColorSequence.new(GRAY), Transparency = Constant(0.5) })
			elseif Cover == "beam" then
				local A0, A1 = Pair(G, `{Name}-beamholder`, At, Vector3.new(0, -3, 0), Vector3.new(0, 3, 0))
				Beam(G, `{Name}-beam`, A0, A1, { Width0 = 5, Width1 = 5, Color = ColorSequence.new(GRAY), Transparency = Constant(0.5) })
			end
			local Carrier = Block(G, `{Name}-carrier`, CFrame.new(At + Vector3.new(0, -2, Offset)), Vector3.new(1, 1, 1), WHITE, nil, 1)
			local Effect = Instance.new(Case[6] or "Fire")
			if Effect:IsA("Fire") then
				Effect.Size = 3
				Effect.Heat = 5
			else
				Effect.Size = 2
				Effect.Opacity = 0.5
				Effect.RiseVelocity = 1
			end
			Effect.Parent = Carrier
		end
		View("legacy", "legacy", O + Vector3.new(0, -2.5, -45), O + Vector3.new(0, -2.5, 0), 40, "flat", { Count = 8, Every = 4, Time = true })
	end

	-- legacy2: a Fire against particle squares and between panes (x 500, seen from -Z)
	do
		local O = Vector3.new(500, 0, 0)
		local G = Group("legacy2")
		Block(G, "Backdrop", CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
		local Emit = {}
		-- (name, its Fires' offsets along Z (+: behind the middle), what covers: "pane", "square", "two panes" or nothing)
		local Cases = {
			{ "bare", { 0 } },
			{ "behind", { 2.5 }, "pane" },
			{ "sqbehind", { 2.5 }, "square" },
			{ "sqfront", { -2.5 }, "square" },
			{ "between", { 2.5, -2.5 }, "pane" },
			{ "twopanes", { 0 }, "two panes" },
		}
		for Index, Case in Cases do
			local At = O + Vector3.new((Index - 3.5) * 7, 0, 0)
			local Name, Offsets, Cover = Case[1], Case[2], Case[3]
			if Cover == "pane" then
				Block(G, `{Name}-pane`, CFrame.new(At), Vector3.new(5, 6, 0.2), WHITE, nil, 0.5)
			elseif Cover == "two panes" then
				Block(G, `{Name}-panefront`, CFrame.new(At + Vector3.new(0, 0, -2.5)), Vector3.new(5, 6, 0.2), WHITE, nil, 0.5)
				Block(G, `{Name}-paneback`, CFrame.new(At + Vector3.new(0, 0, 2.5)), Vector3.new(5, 6, 0.2), WHITE, nil, 0.5)
			elseif Cover == "square" then
				local Sq = Holder(G, `{Name}-sqholder`, At)
				Emitter(Sq, `{Name}-sq`, { Size = Constant(3), Color = ColorSequence.new(GRAY), Transparency = Constant(0.5) })
				Emit[`{Name}-sq`] = 1
			end
			for Number, Offset in Offsets do
				local Shift = if #Offsets > 1 then (Number - 1.5) * 1.6 else 0
				local Carrier = Block(G, `{Name}-carrier{Number}`, CFrame.new(At + Vector3.new(Shift, -2, Offset)), Vector3.new(1, 1, 1), WHITE, nil, 1)
				local Effect = Instance.new("Fire")
				Effect.Size = 3
				Effect.Heat = 5
				Effect.Parent = Carrier
			end
		end
		View("legacy2", "legacy2", O + Vector3.new(0, -1, -45), O + Vector3.new(0, -1, 0), 40, "flat", { Count = 8, Every = 4, Time = true, Emit = Emit, Age = 0.3, Live = true })
	end

	-- zcut: ZOffset's near cut by depth and orientation
	do
		local O = Vector3.new(1000, 0, 0)
		local G = Group("zcut")
		Block(G, "Backdrop", CFrame.new(O + Vector3.new(0, 0, 200)), Vector3.new(600, 400, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Emit = {}
		-- (row name, height, orientation, the holder's turn so its Top points the way the square leaves)
		local Rows = {
			{ "face", 7, Enum.ParticleOrientation.FacingCamera, CFrame.new() },
			{ "tilt", -1.5, Enum.ParticleOrientation.VelocityPerpendicular, CFrame.Angles(-math.pi / 4, 0, 0) },
			{ "flat", -10, Enum.ParticleOrientation.VelocityPerpendicular, CFrame.new() },
		}
		for _, Row in Rows do
			for Column = 0, 12 do
				local Delta = -4 + Column * 0.5
				local Depth = 30 + Delta
				local X = (Column - 6) * 3.6
				local Name = `{Row[1]}{Column}`
				local At = Holder(G, `{Name}-holder`, O + Vector3.new(X, Row[2], Depth), Row[4])
				-- each column its own hue, so overlapping squares tell apart
				local Hue = Column / 13 * 6
				local function Channel(Offset)
					local K = (Offset + Hue) % 6
					return 1 - math.max(0, math.min(K, 4 - K, 1))
				end
				local Color = Color3.new(Channel(5), Channel(3), Channel(1))
				Emitter(At, Name, { Size = Constant(3), ZOffset = 30, Orientation = Row[3], Speed = NumberRange.new(0.01, 0.01), Color = ColorSequence.new(Color) })
				Emit[Name] = 1
				-- the plate over its left half, at depth 15: the square's box projected onto that depth
				local Low, High = Vector3.new(math.huge, math.huge, 0), Vector3.new(-math.huge, -math.huge, 0)
				for _, Corner in { Vector3.new(-1.5, -1.5, -1.5), Vector3.new(1.5, 1.5, 1.5), Vector3.new(-1.5, 1.5, 1.5), Vector3.new(1.5, -1.5, -1.5), Vector3.new(-1.5, -1.5, 1.5), Vector3.new(1.5, 1.5, -1.5), Vector3.new(-1.5, 1.5, -1.5), Vector3.new(1.5, -1.5, 1.5) } do
					local P = Vector3.new(X, Row[2], Depth) + Corner
					local Q = Vector3.new(P.X * 15 / P.Z, P.Y * 15 / P.Z, 0)
					Low = Vector3.new(math.min(Low.X, Q.X), math.min(Low.Y, Q.Y), 0)
					High = Vector3.new(math.max(High.X, Q.X), math.max(High.Y, Q.Y), 0)
				end
				local Mid = X * 15 / Depth
				local Left = Low.X - 0.2
				Plate(G, `{Name}-plate`, CFrame.new(O + Vector3.new((Left + Mid) / 2, (Low.Y + High.Y) / 2, 15)), Vector3.new(Mid - Left, High.Y - Low.Y + 0.4, 0.1), GRAY)
			end
		end
		View("zcut", "zcut", O, O + Vector3.new(0, 0, 10), 70, "flat", { Emit = Emit, Age = 0.3 })
		-- the same squares without ZOffset: where each one stands
		View("zcut", "zcut-z0", O, O + Vector3.new(0, 0, 10), 70, "flat", { Emit = Emit, Age = 0.3, Set = { ZOffset = 0 } })
	end

	-- zcut2: the same squares all at depth 30 (columns: ZOffset 0, 1, 5, 10, 20, 25, 27, 28, 29, 29.5, 30.5, 31, 32),
	-- facing (top), tilted (middle) and flat (bottom), no plates
	do
		local O = Vector3.new(1500, 0, 0)
		local G = Group("zcut2")
		Block(G, "Backdrop", CFrame.new(O + Vector3.new(0, 0, 200)), Vector3.new(600, 400, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Emit = {}
		local Offsets = { 0, 1, 5, 10, 20, 25, 27, 28, 29, 29.5, 30.5, 31, 32 }
		local Rows = {
			{ "face", 7, Enum.ParticleOrientation.FacingCamera, CFrame.new() },
			{ "tilt", -1.5, Enum.ParticleOrientation.VelocityPerpendicular, CFrame.Angles(-math.pi / 4, 0, 0) },
			{ "flat", -10, Enum.ParticleOrientation.VelocityPerpendicular, CFrame.new() },
		}
		for _, Row in Rows do
			for Column, Offset in Offsets do
				local Name = `z2{Row[1]}{Column}`
				local At = Holder(G, `{Name}-holder`, O + Vector3.new((Column - 7) * 3.6, Row[2], 30), Row[4])
				Emitter(At, Name, { Size = Constant(1.5), ZOffset = Offset, Orientation = Row[3], Speed = NumberRange.new(0.01, 0.01) })
				Emit[Name] = 1
			end
		end
		View("zcut2", "zcut2", O, O + Vector3.new(0, 0, 10), 70, "flat", { Emit = Emit, Age = 0.3 })
	end

	-- ties: draws at one depth
	do
		local G = Group("ties")
		local function Spot(Index)
			return Vector3.new(2000 + Index * 60, 0, 0)
		end
		local function Views4(Tag, O, Extra)
			View("ties", `{Tag}-head`, O + Vector3.new(0, 0, -20), O, 40, "flat", Extra)
			View("ties", `{Tag}-yawl`, O + Vector3.new(-10, 0, -17.32), O, 40, "flat", Extra)
			View("ties", `{Tag}-yawr`, O + Vector3.new(10, 0, -17.32), O, 40, "flat", Extra)
			View("ties", `{Tag}-up`, O + Vector3.new(0, -6.84, -18.79), O, 40, "flat", Extra)
		end
		-- crossing beams on one midpoint, red first in the file at the left, green first at the right
		do
			local O = Spot(0)
			Block(G, "x-backdrop", CFrame.new(O + Vector3.new(0, 0, 40)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
			for Side, First in { -1, 1 } do
				local At = O + Vector3.new(Side * 4, 0, 0)
				local Colors = if Side < 0 then { RED, GREEN } else { GREEN, RED }
				local A0, A1 = Pair(G, `x{Side}-a`, At, Vector3.new(-2.5, -2.5, 0), Vector3.new(2.5, 2.5, 0))
				local B0, B1 = Pair(G, `x{Side}-b`, At, Vector3.new(-2.5, 2.5, 0), Vector3.new(2.5, -2.5, 0))
				Beam(G, `x{Side}-first`, A0, A1, { Color = ColorSequence.new(Colors[1]), Width0 = 1.2, Width1 = 1.2 })
				Beam(G, `x{Side}-second`, B0, B1, { Color = ColorSequence.new(Colors[2]), Width0 = 1.2, Width1 = 1.2 })
			end
			Views4("x", O)
		end
		-- overlapping beams across the look on one plane, different midpoints
		do
			local O = Spot(1)
			Block(G, "p-backdrop", CFrame.new(O + Vector3.new(0, 0, 40)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
			for Side, First in { -1, 1 } do
				local At = O + Vector3.new(Side * 5, 0, 0)
				local Colors = if Side < 0 then { RED, GREEN } else { GREEN, RED }
				local A0, A1 = Pair(G, `p{Side}-a`, At, Vector3.new(-3, -0.4, 0), Vector3.new(1.5, -0.4, 0))
				local B0, B1 = Pair(G, `p{Side}-b`, At, Vector3.new(-1.5, 0.4, 0), Vector3.new(3, 0.4, 0))
				Beam(G, `p{Side}-first`, A0, A1, { Color = ColorSequence.new(Colors[1]), Width0 = 2, Width1 = 2, Segments = 1 })
				Beam(G, `p{Side}-second`, B0, B1, { Color = ColorSequence.new(Colors[2]), Width0 = 2, Width1 = 2, Segments = 1 })
			end
			Views4("p", O)
		end
		-- overlapping squares at one depth: equal Sizes (top), unequal (bottom)
		do
			local O = Spot(2)
			Block(G, "s-backdrop", CFrame.new(O + Vector3.new(0, 0, 40)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
			local Emit = {}
			for Side, First in { -1, 1 } do
				for Row, Sizes in { { 2, 2 }, { 2, 1.6 } } do
					local At = O + Vector3.new(Side * 4, Row == 1 and 2 or -2, 0)
					local Colors = if Side < 0 then { RED, GREEN } else { GREEN, RED }
					local H1 = Holder(G, `s{Side}{Row}-h1`, At + Vector3.new(-0.5, 0, 0))
					local H2 = Holder(G, `s{Side}{Row}-h2`, At + Vector3.new(0.5, 0, 0))
					Emitter(H1, `s{Side}{Row}-first`, { Size = Constant(Sizes[1]), Color = ColorSequence.new(Colors[1]) })
					Emitter(H2, `s{Side}{Row}-second`, { Size = Constant(Sizes[2]), Color = ColorSequence.new(Colors[2]) })
					Emit[`s{Side}{Row}-first`] = 1
					Emit[`s{Side}{Row}-second`] = 1
				end
			end
			Views4("s", O, { Emit = Emit, Age = 0.3 })
		end
		-- panes ordered by depth along the look against distance from the eye: a 6 x 3 pane at depth 20 near the
		-- view's axis and a 30-stud pane reaching outwards whose middle is nearer along the look (depth 19) but further
		-- from the eye, turned so its inner end (over the first pane) lies 1.4 studs behind it; Transparency 0.5 (left)
		-- and 0.02 (right), red first in the file (top) and green first (bottom). By depth along the look the long
		-- pane draws over the small one, by distance under it.
		do
			local O = Spot(3)
			Block(G, "d-backdrop", CFrame.new(O + Vector3.new(0, 0, 60)), Vector3.new(300, 200, 1), Color3.new(0, 0, 0))
			for Row = 1, 2 do
				for Column, T in { 0.5, 0.02 } do
					local Side = Column == 1 and -1 or 1
					local Base = O + Vector3.new(Side * 4, Row == 1 and 4.5 or -4.5, 20)
					local Colors = if Row == 1 then { RED, GREEN } else { GREEN, RED }
					local Small = Instance.new("Part")
					local Big = Instance.new("Part")
					for Index, Part in { Small, Big } do
						Part.Anchored = true
						Part.CastShadow = false
						Part.Material = Enum.Material.SmoothPlastic
						Part.Color = Colors[Index]
						Part.Transparency = T
					end
					Small.Name = `d{Row}{Column}-small`
					Small.Size = Vector3.new(6, 3, 0.1)
					Small.CFrame = CFrame.new(Base)
					Big.Name = `d{Row}{Column}-big`
					Big.Size = Vector3.new(30, 2, 0.1)
					Big.CFrame = CFrame.new(Base + Vector3.new(Side * 12, 0, -1)) * CFrame.Angles(0, Side * math.atan2(3, 15), 0)
					if Row == 1 then
						Small.Parent = G
						Big.Parent = G
					else
						Big.Parent = G
						Small.Parent = G
					end
				end
			end
			View("ties", "d-head", O, O + Vector3.new(0, 0, 20), 70, "flat")
		end
	end

	-- trails: ties on one pair of attachments, and the point a trail sorts by
	do
		local G = Group("trails")
		-- two trails on one pair (red first at the top mover, green first at the bottom one)
		do
			local O = Vector3.new(3000, 0, 0)
			Block(G, "pair-backdrop", CFrame.new(O + Vector3.new(0, 0, 40)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0))
			local Move = {}
			for Row, First in { 1, 2 } do
				local A0, A1, Part = Pair(G, `pair{Row}`, O + Vector3.new(-6, Row == 1 and 2 or -2, 0), Vector3.new(0, 0.6, 0), Vector3.new(0, -0.6, 0))
				local Colors = if Row == 1 then { RED, GREEN } else { GREEN, RED }
				Trail(G, `pair{Row}-first`, A0, A1, { Color = ColorSequence.new(Colors[1]) })
				Trail(G, `pair{Row}-second`, A0, A1, { Color = ColorSequence.new(Colors[2]) })
				table.insert(Move, { Mover = `pair{Row}`, By = { 12, 0, 0 } })
			end
			local Extra = { Move = Move, Duration = 1, After = 0.5 }
			View("trails", "pair-head", O + Vector3.new(0, 0, -20), O, 50, "flat", Extra)
			View("trails", "pair-yawl", O + Vector3.new(-12, 0, -16), O, 50, "flat", Extra)
			View("trails", "pair-yawr", O + Vector3.new(12, 0, -16), O, 50, "flat", Extra)
			View("trails", "pair-up", O + Vector3.new(0, -10, -17.3), O, 50, "flat", Extra)
			View("trails", "pair-along", O + Vector3.new(-22, 0.5, -3), O + Vector3.new(6, 0, 0), 50, "flat", Extra)
		end
		-- trails along the view behind panes: each copy (100 studs apart) seen from its origin along +Z; two trails run
		-- from depth 10 to 50 six studs either side of the view's axis, the left one's head near, the right one's far,
		-- each covered on screen by a gray pane at Transparency 0.5 at one depth (14, 22, 30, 38, 46: one copy each)
		for Copy, Depth in { 14, 22, 30, 38, 46 } do
			local O = Vector3.new(3500 + (Copy - 1) * 100, 0, 0)
			Block(G, `sort{Copy}-backdrop`, CFrame.new(O + Vector3.new(0, 0, 200)), Vector3.new(600, 400, 1), Color3.new(0, 0, 0))
			local Move = {}
			for Column, HeadNear in { true, false } do
				local X = Column == 1 and 6 or -6
				local Name = `sort{Copy}{Column}`
				local A0, A1 = Pair(G, Name, O + Vector3.new(X, 0, if HeadNear then 50 else 10), Vector3.new(0, 0.5, 0), Vector3.new(0, -0.5, 0))
				Trail(G, `{Name}-trail`, A0, A1, { FaceCamera = true })
				table.insert(Move, { Mover = Name, By = { 0, 0, if HeadNear then -40 else 40 } })
				-- as wide as the trail seen from the eye (x / 50 to x / 10 at depth `Depth`), as tall as its near end
				Block(G, `{Name}-pane`, CFrame.new(O + Vector3.new(X * Depth * 0.06, 0, Depth)), Vector3.new(math.abs(X) * Depth * 0.08 + 0.6, Depth * 0.1 + 0.6, 0.05), GRAY, nil, 0.5)
			end
			View("trails", `sort{Depth}`, O, O + Vector3.new(0, 0, 10), 70, "flat", { Move = Move, Duration = 1, After = 0.5 })
		end
	end

	-- cull: occlusion culling of effects brought in front of an occluder by ZOffset or motion
	do
		local G = Group("cull")
		local Index = 0
		local function Case(Tag, Build, Extra)
			local O = Vector3.new(4000 + Index * 100, 0, 0)
			Index += 1
			Block(G, `{Tag}-backdrop`, CFrame.new(O + Vector3.new(0, 0, 120)), Vector3.new(400, 300, 1), Color3.new(0, 0, 0))
			local More = Build(O) or {}
			for Key, Value in Extra or {} do
				More[Key] = Value
			end
			View("cull", `{Tag}-head`, O, O + Vector3.new(0, 0, 10), 50, "flat", More)
			View("cull", `{Tag}-yaw`, O + Vector3.new(-6, 0, 0), O + Vector3.new(0, 0, 30), 50, "flat", More)
		end
		local function Wall(Name, O, Brick)
			if Brick then
				return Plate(G, Name, CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(10, 10, 1), GRAY)
			end
			return Block(G, Name, CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(10, 10, 1), GRAY)
		end
		for _, Brick in { false, true } do
			local Suffix = Brick and "-brick" or ""
			Case(`beamz{Suffix}`, function(O)
				Wall(`beamz{Suffix}-wall`, O, Brick)
				local A0, A1 = Pair(G, `beamz{Suffix}-beam`, O + Vector3.new(0, 1, 36), Vector3.new(-3, 0, 0), Vector3.new(3, 0, 0))
				Beam(G, `beamz{Suffix}-b`, A0, A1, { Color = ColorSequence.new(RED), ZOffset = 10, Segments = 1 })
				return {}
			end)
			Case(`emitz{Suffix}`, function(O)
				Wall(`emitz{Suffix}-wall`, O, Brick)
				local At = Holder(G, `emitz{Suffix}-holder`, O + Vector3.new(0, -1.5, 36))
				Emitter(At, `emitz{Suffix}-e`, { Size = Constant(2), Color = ColorSequence.new(GREEN), ZOffset = 10 })
				return { Emit = { [`emitz{Suffix}-e`] = 1 }, Age = 0.3 }
			end)
		end
		-- squares that flew from 40 studs out (behind the block) to 25 (in front of it): from an Attachment and
		-- from a part's volume
		Case("fly", function(O)
			Wall("fly-wall", O, false)
			local At = Holder(G, "fly-holder", O + Vector3.new(-1.5, 0, 40), CFrame.Angles(-math.pi / 2, 0, 0))
			Emitter(At, "fly-e", { Size = Constant(1.5), Color = ColorSequence.new(GREEN), Speed = NumberRange.new(15, 15) })
			local Part = Block(G, "fly-part", CFrame.new(O + Vector3.new(1.5, 0, 40)), Vector3.new(0.5, 0.5, 0.5), nil, nil, 1)
			Emitter(Part, "fly-pe", { Size = Constant(1.5), Color = ColorSequence.new(RED), Speed = NumberRange.new(15, 15), EmissionDirection = Enum.NormalId.Front })
			return { Emit = { ["fly-e"] = 1, ["fly-pe"] = 1 }, Age = 1 }
		end)
		-- a beam behind the block whose curve bulges 15 studs towards the camera
		Case("curve", function(O)
			Wall("curve-wall", O, false)
			local A0, A1 = Pair(G, "curve-beam", O + Vector3.new(0, 0, 36), Vector3.new(-3, 0, 0), Vector3.new(3, 0, 0))
			A0.CFrame = A0.CFrame * CFrame.Angles(0, math.pi / 2, 0)
			A1.CFrame = A1.CFrame * CFrame.Angles(0, math.pi / 2, 0)
			Beam(G, "curve-b", A0, A1, { Color = ColorSequence.new(RED), CurveSize0 = 20, CurveSize1 = -20, Segments = 20 })
			return {}
		end)
		-- a beam from behind the block to in front of it
		Case("long", function(O)
			Wall("long-wall", O, false)
			local A0, A1 = Pair(G, "long-beam", O + Vector3.new(3, 0, 28), Vector3.new(0, 0, 8), Vector3.new(0, 0, -8))
			Beam(G, "long-b", A0, A1, { Color = ColorSequence.new(RED), FaceCamera = true })
			return {}
		end)
	end

	-- cull2 (x 5000 on, 100 studs apart): the ZOffset beam (y +1.5) and square (y -1.5) 36 studs out with ZOffset 10
	-- behind a 10 x 10 elliptic cylinder SpecialMesh wall at 30 (staff: cylinder SpecialMeshes never occlude), behind
	-- 1-stud blocks covering their middles only, half behind the block's edge, and an AlwaysOnTop BillboardGui (a red
	-- frame) behind the block
	do
		local G = Group("cull2")
		local Index = 0
		local function Case(Tag, Build, Extra)
			local O = Vector3.new(5000 + Index * 100, 0, 0)
			Index += 1
			Block(G, `{Tag}-backdrop`, CFrame.new(O + Vector3.new(0, 0, 120)), Vector3.new(400, 300, 1), Color3.new(0, 0, 0))
			local More = Build(O) or {}
			for Key, Value in Extra or {} do
				More[Key] = Value
			end
			View("cull2", `{Tag}-head`, O, O + Vector3.new(0, 0, 10), 50, "flat", More)
		end
		local function Effects(Tag, O, X)
			local A0, A1 = Pair(G, `{Tag}-beam`, O + Vector3.new(X, 1.5, 36), Vector3.new(-3, 0, 0), Vector3.new(3, 0, 0))
			Beam(G, `{Tag}-b`, A0, A1, { Color = ColorSequence.new(RED), ZOffset = 10, Segments = 1 })
			local At = Holder(G, `{Tag}-holder`, O + Vector3.new(X, -1.5, 36))
			Emitter(At, `{Tag}-e`, { Size = Constant(1.5), Color = ColorSequence.new(GREEN), ZOffset = 10 })
			return { Emit = { [`{Tag}-e`] = 1 }, Age = 0.3 }
		end
		Case("cylz", function(O)
			local Wall = Block(G, "cylz-wall", CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(10, 10, 1), GRAY)
			local Mesh = Instance.new("SpecialMesh")
			Mesh.MeshType = Enum.MeshType.Cylinder
			Mesh.Parent = Wall
			return Effects("cylz", O, 0)
		end)
		Case("smallz", function(O)
			Block(G, "smallz-block1", CFrame.new(O + Vector3.new(0, 1.5 * 30 / 36, 30)), Vector3.new(1, 1, 1), GRAY)
			Block(G, "smallz-block2", CFrame.new(O + Vector3.new(0, -1.5 * 30 / 36, 30)), Vector3.new(1, 1, 1), GRAY)
			return Effects("smallz", O, 0)
		end)
		Case("edgez", function(O)
			Block(G, "edgez-wall", CFrame.new(O + Vector3.new(-5, 0, 30)), Vector3.new(10, 10, 1), GRAY)
			return Effects("edgez", O, 0)
		end)
		Case("guiz", function(O)
			Block(G, "guiz-wall", CFrame.new(O + Vector3.new(0, 0, 30)), Vector3.new(10, 10, 1), GRAY)
			local Holder = Block(G, "guiz-holder", CFrame.new(O + Vector3.new(0, 0, 36)), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
			local Gui = Instance.new("BillboardGui")
			Gui.Name = "Gui"
			Gui.AlwaysOnTop = true
			Gui.Size = UDim2.new(4, 0, 2, 0)
			Gui.LightInfluence = 0
			Gui.Adornee = Holder
			local Frame = Instance.new("Frame")
			Frame.Size = UDim2.new(1, 0, 1, 0)
			Frame.BackgroundColor3 = RED
			Frame.BorderSizePixel = 0
			Frame.Parent = Gui
			Gui.Parent = Holder
			return {}
		end)
	end

	-- minlength: trails sliding with and without MinLength, near and far
	do
		local O = Vector3.new(6000, 0, 0)
		local G = Group("minlength")
		local Move = {}
		-- (name, distance, slide a second, MinLength, height as a share of the distance)
		Block(G, "ml-backdrop", CFrame.new(O + Vector3.new(0, 0, 700)), Vector3.new(2000, 1500, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Cases = {
			{ "n005", 60, 60, 0.05, 0.35 },
			{ "n2", 60, 60, 2, 0.25 },
			{ "n4", 60, 60, 4, 0.15 },
			{ "n0", 60, 60, 0, 0.05 },
			{ "f6", 600, 180, 6, -0.05 },
			{ "f0", 600, 180, 0, -0.15 },
		}
		local Faded = NumberSequence.new({ NumberSequenceKeypoint.new(0, 1), NumberSequenceKeypoint.new(0.5, 0), NumberSequenceKeypoint.new(1, 1) })
		for _, Case in Cases do
			local Name, Distance, Slide, Min, Height = Case[1], Case[2], Case[3], Case[4], Case[5]
			local Half = Distance * 0.012
			local Part = Block(G, `ml-{Name}`, CFrame.new(O + Vector3.new(Slide * 0.5, Height * Distance, Distance)), Vector3.new(1, 1, 1) * Distance * 0.004, RED, Enum.Material.Neon)
			local A0 = Instance.new("Attachment")
			A0.Name = "A0"
			A0.CFrame = CFrame.new(0, Half, 0)
			A0.Parent = Part
			local A1 = Instance.new("Attachment")
			A1.Name = "A1"
			A1.CFrame = CFrame.new(0, -Half, 0)
			A1.Parent = Part
			Trail(G, `ml-{Name}-trail`, A0, A1, { Transparency = Faded, Lifetime = 1, MinLength = Min })
			table.insert(Move, { Mover = `ml-{Name}`, By = { -Slide * 1.5, 0, 0 } })
		end
		View("minlength", "ml-a", O, O + Vector3.new(0, 0, 10), 70, "dark", { Move = Move, Duration = 1.5, CaptureAt = 0.95 })
		View("minlength", "ml-b", O, O + Vector3.new(0, 0, 10), 70, "dark", { Move = Move, Duration = 1.5, CaptureAt = 0.95 })
	end

	Root.Parent = workspace
	return { Views = Views }
end
