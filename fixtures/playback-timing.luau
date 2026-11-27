-- Playback timing probes (notes/render.md "Playback"): where a burst leaves from when its emitter moves
-- between Emit calls in one frame, how Rate emitters count frames from the one they are switched on in,
-- and which frame of a OneShot flipbook a short-lived particle shows. Each group 1,000 studs from the
-- next along X, each over a black Neon backdrop; particles are white SquareParticle squares, unlit, unless
-- a group says otherwise. A probe job (run in a Roblox client) moves, enables, emits and freezes them at
-- chosen frames and logs every frame's time; ror's side plays the same events at those frame times.
--
-- "emitmove" (x 0, seen from -Z): four 0.2-stud parts (Transparency 1), each with an Attachment `At` and
--   an emitter of 1-stud squares that never move or die: `free` on an attachment, `locked-at` and
--   `locked-part` LockedToPart on attachments, `spawn` on the part itself (a 0.4-stud box). A job moves
--   each attachment (the `spawn` part) to three corners in one frame, emitting one particle after each
--   move, then moves `locked-at`'s attachment and `locked-part`'s part (turned a quarter about Z).
-- "rate" (x 1000, seen from -Z): Rate emitters of 0.3-stud squares leaving up at 30 studs a second (half
--   a stud a frame at 60 frames a second), Rates 1, 2, 3, 4, 5, 6, 7, 10, 12, 13, 15, 16, 20, 30 and 60:
--   `rate-on-<n>` switched off in the file (a job switches them on), `rate-in-<n>` switched on in a Model
--   `rate-in` a job keeps out of the world and parents in (0.7 studs to the right of its pair);
--   `rate-ref` (Rate 0) for one emitted particle whose height is its age.
-- "tween" (x 4000, seen from -Z): white Neon cubes a job moves with TweenService along +X (rows), and a row
--   of eight `bit-<n>` tiles under them a job lights with the frame's index, so a captured frame says which
--   frame it is.
-- "texels" (x 5000, seen from -Z at 60 studs): one still particle each of `Context.BlackTexture` (thin
--   white lines whose transparent texels are black) and `Context.WhiteTexture` (the same alpha, white
--   everywhere), Sizes 10, 5, 2.5, 1.25 and 0.625 (a 256-texel image at 1, 2, 4, 8 and 16 texels a
--   pixel), `black-<n>` in the top row and `white-<n>` under it.
-- "sheet" (x 2000, seen from -Z): OneShot Grid4x4 flipbooks of `Context.SheetTexture` (a sheet whose
--   frames tell themselves apart), Size 1.2, Lifetimes 0.03 to 0.15 (`sheet-<ms>`), and
--   `sheet-ref` (a square leaving up at 20 studs a second).
-- "sheet2" (x 3000, seen from -Z): the same for unblended OneShot sheets, FlipbookStartRandom, Loop and
--   PingPong at 30 frames a second, and 2 x 2 and 8 x 8 OneShot sheets (`Context.Sheet2Texture`,
--   `Context.Sheet8Texture`); `sheet2-ref` as `sheet-ref`.
return function(Context)
	local Root = Instance.new("Model")
	Root.Name = "TimingProbes"
	local SQUARE = "rbxasset://textures/particles/SquareParticle.png"
	local Groups = {}

	local function Group(Name, Center, Camera)
		local Model = Instance.new("Model")
		Model.Name = Name
		Model.Parent = Root
		Groups[Name] = { center = { Center.X, Center.Y, Center.Z }, camera = Camera }
		return Model
	end
	local function Block(Parent, Name, CF, Size, Color, Material, Transparency)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.CanCollide = false
		Part.CastShadow = false
		Part.Material = Material or Enum.Material.SmoothPlastic
		Part.Color = Color or Color3.new(0, 0, 0)
		Part.Size = Size
		Part.CFrame = CF
		Part.Transparency = Transparency or 0
		Part.Parent = Parent
		return Part
	end
	local function Constant(Value)
		return NumberSequence.new({ NumberSequenceKeypoint.new(0, Value), NumberSequenceKeypoint.new(1, Value) })
	end
	local function Emitter(Parent, Name, Set)
		local E = Instance.new("ParticleEmitter")
		E.Name = Name
		E.Rate = 0
		E.Lifetime = NumberRange.new(30, 30)
		E.Speed = NumberRange.new(0, 0)
		E.SpreadAngle = Vector2.new(0, 0)
		E.Rotation = NumberRange.new(0, 0)
		E.RotSpeed = NumberRange.new(0, 0)
		E.Size = Constant(1)
		E.Transparency = Constant(0)
		E.Color = ColorSequence.new(Color3.new(1, 1, 1))
		E.LightEmission = 0
		E.LightInfluence = 0
		E.Brightness = 1
		E.Texture = SQUARE
		E.Drag = 0
		E.Acceleration = Vector3.new(0, 0, 0)
		E.EmissionDirection = Enum.NormalId.Top
		E.Shape = Enum.ParticleEmitterShape.Box
		for Key, Value in Set do
			E[Key] = Value
		end
		E.Parent = Parent
		return E
	end
	local function Anchor(Parent, Name, Position)
		local Part = Block(Parent, Name, CFrame.new(Position), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
		local At = Instance.new("Attachment")
		At.Name = "At"
		At.Parent = Part
		return At
	end

	-- emitmove: bursts from an emitter moved between Emit calls in one frame
	do
		local Center = Vector3.new(0, 0, 0)
		local Model = Group("emitmove", Center, { eye = { 0, 0, -50 }, target = { 0, 0, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		Emitter(Anchor(Model, "free-part", Center + Vector3.new(-9, 5, 0)), "free", {})
		Emitter(Anchor(Model, "locked-at-part", Center + Vector3.new(-9, -6, 0)), "locked-at", { LockedToPart = true })
		Emitter(Anchor(Model, "locked-part-part", Center + Vector3.new(9, -6, 0)), "locked-part", { LockedToPart = true })
		local Spawn = Block(Model, "spawn-part", CFrame.new(Center + Vector3.new(9, 5, 0)), Vector3.new(0.4, 0.4, 0.4), nil, nil, 1)
		Emitter(Spawn, "spawn", {})
	end

	-- rate: Rate emitters switched on or parented in at a frame
	do
		local Center = Vector3.new(1000, 0, 0)
		local Model = Group("rate", Center, { eye = { 1000, 6, -34 }, target = { 1000, 6, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(300, 200, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Column = { Size = Constant(0.3), Speed = NumberRange.new(30, 30), Lifetime = NumberRange.new(5, 5) }
		local Arriving = Instance.new("Model")
		Arriving.Name = "rate-in"
		Arriving.Parent = Model
		local Rates = { 1, 2, 3, 4, 5, 6, 7, 10, 12, 13, 15, 16, 20, 30, 60 }
		for Index, Rate in Rates do
			local X = (Index - 8) * 1.6
			local On = Emitter(Anchor(Model, `rate-on-{Rate}-anchor`, Center + Vector3.new(X - 0.35, 0, 0)), `rate-on-{Rate}`, Column)
			On.Rate = Rate
			On.Enabled = false
			local In = Emitter(Anchor(Arriving, `rate-in-{Rate}-anchor`, Center + Vector3.new(X + 0.35, 0, 0)), `rate-in-{Rate}`, Column)
			In.Rate = Rate
		end
		Emitter(Anchor(Model, "rate-ref-anchor", Center + Vector3.new(-14.4, 0, 0)), "rate-ref", { Size = Constant(0.3), Speed = NumberRange.new(30, 30), Lifetime = NumberRange.new(5, 5) })
	end

	-- tween: TweenService moves against the frame drawn
	do
		local Center = Vector3.new(4000, 0, 0)
		local Model = Group("tween", Center, { eye = { 4006, 0, -30 }, target = { 4006, 0, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		for Row = 1, 4 do
			Block(Model, `cube-{Row}`, CFrame.new(Center + Vector3.new(0, 6 - Row * 2.5, 0)), Vector3.new(0.8, 0.8, 0.8), Color3.new(1, 1, 1), Enum.Material.Neon)
		end
		for Bit = 0, 7 do
			Block(Model, `bit-{Bit}`, CFrame.new(Center + Vector3.new(Bit * 1.5, -7, 0)), Vector3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.Material.Neon, 1)
		end
	end

	-- sheet: which frame of a OneShot flipbook short-lived particles show
	do
		local Center = Vector3.new(2000, 0, 0)
		local Model = Group("sheet", Center, { eye = { 2000, 0, -40 }, target = { 2000, 0, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		for Index, Life in { 0.03, 0.04, 0.05, 0.06, 0.075, 0.1, 0.15 } do
			local Name = `sheet-{math.round(Life * 1000)}`
			Emitter(Anchor(Model, `{Name}-anchor`, Center + Vector3.new((Index - 4) * 3.5, 2, 0)), Name, {
				Size = Constant(1.2),
				Lifetime = NumberRange.new(Life, Life),
				Texture = Context.SheetTexture or SQUARE,
				FlipbookLayout = Enum.ParticleFlipbookLayout.Grid4x4,
				FlipbookMode = Enum.ParticleFlipbookMode.OneShot,
			})
		end
		Emitter(Anchor(Model, "sheet-ref-anchor", Center + Vector3.new(-12, -6, 0)), "sheet-ref", { Size = Constant(0.4), Speed = NumberRange.new(20, 20), Lifetime = NumberRange.new(5, 5) })
	end

	-- sheet2: the other flipbook modes and grids, the same way
	do
		local Center = Vector3.new(3000, 0, 0)
		local Model = Group("sheet2", Center, { eye = { 3000, 0, -40 }, target = { 3000, 0, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Grid4, Grid2, Grid8 = Context.SheetTexture or SQUARE, Context.Sheet2Texture or SQUARE, Context.Sheet8Texture or SQUARE
		local Cases = {
			{ "unblended-50", 0.05, Grid4, "Grid4x4", "OneShot", { FlipbookBlendFrames = false } },
			{ "unblended-100", 0.1, Grid4, "Grid4x4", "OneShot", { FlipbookBlendFrames = false } },
			{ "unblended-150", 0.15, Grid4, "Grid4x4", "OneShot", { FlipbookBlendFrames = false } },
			{ "random-100", 0.1, Grid4, "Grid4x4", "OneShot", { FlipbookStartRandom = true } },
			{ "random-150", 0.15, Grid4, "Grid4x4", "OneShot", { FlipbookStartRandom = true } },
			{ "loop-30", 5, Grid4, "Grid4x4", "Loop", { FlipbookFramerate = NumberRange.new(30, 30) } },
			{ "pingpong-30", 5, Grid4, "Grid4x4", "PingPong", { FlipbookFramerate = NumberRange.new(30, 30) } },
			{ "grid2-100", 0.1, Grid2, "Grid2x2", "OneShot", {} },
			{ "grid2-150", 0.15, Grid2, "Grid2x2", "OneShot", {} },
			{ "grid8-100", 0.1, Grid8, "Grid8x8", "OneShot", {} },
			{ "grid8-150", 0.15, Grid8, "Grid8x8", "OneShot", {} },
		}
		for Index, Case in Cases do
			local Row, Column = (Index - 1) // 6, (Index - 1) % 6
			local Set = {
				Size = Constant(1.2),
				Lifetime = NumberRange.new(Case[2], Case[2]),
				Texture = Case[3],
				FlipbookLayout = Enum.ParticleFlipbookLayout[Case[4]],
				FlipbookMode = Enum.ParticleFlipbookMode[Case[5]],
			}
			for Key, Value in Case[6] do
				Set[Key] = Value
			end
			Emitter(Anchor(Model, `{Case[1]}-anchor`, Center + Vector3.new((Column - 2.5) * 3.5, 3 - Row * 5, 0)), Case[1], Set)
		end
		Emitter(Anchor(Model, "sheet2-ref-anchor", Center + Vector3.new(-12, -9, 0)), "sheet2-ref", { Size = Constant(0.4), Speed = NumberRange.new(20, 20), Lifetime = NumberRange.new(5, 5) })
	end

	-- texels: what the colour under a texture's transparent texels does, magnified and minified
	do
		local Center = Vector3.new(5000, 0, 0)
		local Model = Group("texels", Center, { eye = { 5000, 0, -60 }, target = { 5000, 0, 0 }, fov = 40 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(300, 200, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local X = { -20, -8, 0, 5, 8.5 }
		for Index, Size in { 10, 5, 2.5, 1.25, 0.625 } do
			for Row, Kind in { "black", "white" } do
				local Texture = (Kind == "black" and Context.BlackTexture or Context.WhiteTexture) or SQUARE
				Emitter(Anchor(Model, `{Kind}-{Index}-anchor`, Center + Vector3.new(X[Index], 7 - (Row - 1) * 12, 0)), `{Kind}-{Index}`, { Size = Constant(Size), Texture = Texture })
			end
		end
	end

	Root.Parent = workspace
	return { groups = Groups }
end
