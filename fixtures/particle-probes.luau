-- Particle spawn, motion and blending probes, each group 1,000 studs from the next along X so a camera
-- on one sees only its own (notes/render.md "Particles"). Every emitter has Rate 0: a probe clears its
-- group's emitters, emits each one's `P5Count` attribute at once, lets them age and freezes them
-- (TimeScale 0). Particles are white SquareParticle squares unless a group says otherwise.
--
-- "inward" (x 0, seen from -Z): part emitters 4 studs across (Transparency 1) of each Shape (rows: Box,
--   Sphere, Cylinder, Disc) and ShapeInOut (columns: Outward, Inward, InAndOut), EmissionDirection Top,
--   Speed 4, 300 faint additive particles each; and Attachment emitters Outward and Inward (bottom row).
--   `ref` (an Attachment, Speed 4 up, alone) measures the age the client froze them at.
-- "soft" (x 1000, seen from above at 50 degrees): a gray SmoothPlastic floor (top at y 0) under an Ambient
--   of 1, and opaque black particles of Size 0.5 at heights 0.003 to 1 stud over it, one each: FacingCamera,
--   VelocityPerpendicular moving up (flat on the floor), VelocityPerpendicular moving along X (standing
--   across the floor) and VelocityParallel moving along X, each at Speed 0.07 (frozen within a frame).
-- "wind" (x 2000, seen from -Z): Workspace.GlobalWind as the place sets it; Attachment emitters 20
--   particles each at one point, Speed 0 unless named, with WindAffectsDrag and Drag 0 to 5, one without
--   WindAffectsDrag, one leaving upwards at 5 studs a second, one pulled up by Acceleration; `ref` again.
-- "fog" (x 3000, seen from -Z): opaque black, opaque white (Brightness 1) and additive white
--   (Transparency 0.5) squares 40 to 160 studs from the camera, each the same size on screen, over a black
--   Neon panel 2 studs behind (top row) or over nothing but a black Neon wall 400 studs off (bottom row).
-- "partial" (x 4000 on, 40 studs apart along X, each seen alone from -Z): 12 x 2 x 2 Cylinders along X
--   (EmissionDirection Top) at ShapePartial 1.1 to 5 and -0.25 to -2, Volume and Surface, 2000 faint
--   additive particles each where they spawn.
-- "smoke" (x 5000, seen from -Z): black Smoke (Opacity 1, no rise) 90 studs from the camera over a black
--   Neon panel and over a black Neon wall 400 studs off, and opaque black ParticleEmitter squares beside
--   them.
--
-- Measured in a Roblox client (0.747, quality level 21, 1644 x 1116) against ror's render of the same
-- place: the job returns the camera; ror's side emits the same counts (`--emit <path>=<count>@0`) and
-- draws them at the client's age (`--time`).
return function(Context)
	local Root = Instance.new("Model")
	Root.Name = "P5Probes"
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
	-- an emitter with the probes' defaults, `Set` on top
	local function Emitter(Parent, Name, Count, Set)
		local E = Instance.new("ParticleEmitter")
		E.Name = Name
		E.Rate = 0
		E.Lifetime = NumberRange.new(20, 20)
		E.Speed = NumberRange.new(0, 0)
		E.SpreadAngle = Vector2.new(0, 0)
		E.Rotation = NumberRange.new(0, 0)
		E.RotSpeed = NumberRange.new(0, 0)
		E.Size = Constant(0.1)
		E.Transparency = Constant(0)
		E.Color = ColorSequence.new(Color3.new(1, 1, 1))
		E.LightEmission = 0
		E.LightInfluence = 0
		E.Brightness = 1
		E.Texture = SQUARE
		E.Drag = 0
		E.Acceleration = Vector3.new(0, 0, 0)
		E.EmissionDirection = Enum.NormalId.Top
		for Key, Value in Set do
			E[Key] = Value
		end
		E:SetAttribute("P5Count", Count)
		E.Parent = Parent
		return E
	end
	local function Anchor(Parent, Name, CF)
		local Part = Block(Parent, Name, CFrame.new(CF.Position), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
		local At = Instance.new("Attachment")
		At.Name = "At"
		At.CFrame = CF - CF.Position
		At.Parent = Part
		return At
	end

	-- inward
	do
		local Center = Vector3.new(0, 0, 0)
		local Model = Group("inward", Center, { eye = { 0, -2, -46 }, target = { 0, -2, 0 }, fov = 50 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Faint = { Size = Constant(0.06), LightEmission = 1, Transparency = Constant(0.5), Speed = NumberRange.new(4, 4) }
		local Shapes = { "Box", "Sphere", "Cylinder", "Disc" }
		local Modes = { "Outward", "Inward", "InAndOut" }
		for Row, Shape in Shapes do
			for Column, Mode in Modes do
				local Position = Center + Vector3.new((2 - Column) * 12, (2.5 - Row) * 9, 0)
				local Part = Block(Model, `{Shape}-{Mode}-part`, CFrame.new(Position), Vector3.new(4, 4, 4), nil, nil, 1)
				local Set = table.clone(Faint)
				Set.Shape = Enum.ParticleEmitterShape[Shape]
				Set.ShapeInOut = Enum.ParticleEmitterShapeInOut[Mode]
				Emitter(Part, `{Shape}-{Mode}`, 300, Set)
			end
		end
		for Column, Mode in { "Outward", "Inward" } do
			local At = Anchor(Model, `point-{Mode}-anchor`, CFrame.new(Center + Vector3.new((2 - Column) * 12, -18, 0)))
			local Set = table.clone(Faint)
			Set.ShapeInOut = Enum.ParticleEmitterShapeInOut[Mode]
			Set.Size = Constant(0.15)
			Emitter(At, `point-{Mode}`, 20, Set)
		end
		local Ref = Anchor(Model, "ref-anchor", CFrame.new(Center + Vector3.new(-12, -18, 0)))
		Emitter(Ref, "ref", 20, { Size = Constant(0.15), LightEmission = 1, Transparency = Constant(0.5), Speed = NumberRange.new(4, 4) })
	end

	-- soft: flat particles near a floor
	do
		local Center = Vector3.new(1000, 0, 0)
		local Model = Group("soft", Center, { eye = { 1000, 13, -11 }, target = { 1000, 0, 0 }, fov = 50 })
		Block(Model, "Floor", CFrame.new(Center + Vector3.new(0, -2, 0)), Vector3.new(60, 4, 40), Color3.fromRGB(128, 128, 128))
		local Heights = { 0.003, 0.01, 0.03, 0.1, 0.3, 1 }
		local Kinds = {
			{ "facing", Enum.ParticleOrientation.FacingCamera, Enum.NormalId.Top },
			{ "perp-up", Enum.ParticleOrientation.VelocityPerpendicular, Enum.NormalId.Top },
			{ "perp-x", Enum.ParticleOrientation.VelocityPerpendicular, Enum.NormalId.Right },
			{ "parallel-x", Enum.ParticleOrientation.VelocityParallel, Enum.NormalId.Right },
		}
		for Row, Kind in Kinds do
			for Column, Height in Heights do
				local Position = Center + Vector3.new((Column - 3.5) * 2.5, Height, (Row - 2.5) * 3)
				local At = Anchor(Model, `{Kind[1]}-h{Height}-anchor`, CFrame.new(Position))
				Emitter(At, `{Kind[1]}-h{Height}`, 1, {
					Size = Constant(0.5),
					Color = ColorSequence.new(Color3.new(0, 0, 0)),
					Speed = NumberRange.new(0.07, 0.07),
					Orientation = Kind[2],
					EmissionDirection = Kind[3],
				})
			end
		end
	end

	-- wind: GlobalWind and WindAffectsDrag
	do
		local Center = Vector3.new(2000, 0, 0)
		local Model = Group("wind", Center, { eye = { 2006, 0, -48 }, target = { 2006, 0, 0 }, fov = 50 })
		Block(Model, "Backdrop", CFrame.new(Center + Vector3.new(0, 0, 30)), Vector3.new(200, 120, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Cases = {
			{ "wad-d0", { WindAffectsDrag = true, Drag = 0 } },
			{ "wad-d0.5", { WindAffectsDrag = true, Drag = 0.5 } },
			{ "wad-d1", { WindAffectsDrag = true, Drag = 1 } },
			{ "wad-d2", { WindAffectsDrag = true, Drag = 2 } },
			{ "wad-d5", { WindAffectsDrag = true, Drag = 5 } },
			{ "nowad-d1", { WindAffectsDrag = false, Drag = 1 } },
			{ "wad-d1-up", { WindAffectsDrag = true, Drag = 1, Speed = NumberRange.new(5, 5) } },
			{ "wad-d1-accel", { WindAffectsDrag = true, Drag = 1, Acceleration = Vector3.new(0, 4, 0) } },
		}
		for Index, Case in Cases do
			local At = Anchor(Model, `{Case[1]}-anchor`, CFrame.new(Center + Vector3.new(-12, 15 - Index * 3.6, 0)))
			local Set = { Size = Constant(0.25), LightEmission = 1, Transparency = Constant(0.3) }
			for Key, Value in Case[2] do
				Set[Key] = Value
			end
			Emitter(At, Case[1], 20, Set)
		end
		local Ref = Anchor(Model, "ref-anchor", CFrame.new(Center + Vector3.new(24, -16, 0)))
		Emitter(Ref, "ref", 20, { Size = Constant(0.25), LightEmission = 1, Transparency = Constant(0.3), Speed = NumberRange.new(4, 4) })
		-- LockedToPart on an attachment whose up is the world's -X: does the wind follow its frame?
		local Locked = Anchor(Model, "locked-anchor", CFrame.new(Center + Vector3.new(-12, -17.4, 0)) * CFrame.Angles(0, 0, math.pi / 2))
		Emitter(Locked, "wad-d2-locked", 20, { Size = Constant(0.25), LightEmission = 1, Transparency = Constant(0.3), WindAffectsDrag = true, Drag = 2, LockedToPart = true })
	end

	-- fog: particles at depth
	do
		local Center = Vector3.new(3000, 0, 0)
		local Model = Group("fog", Center, { eye = { 3000, 0, -40 }, target = { 3000, 0, 0 }, fov = 40 })
		Block(Model, "FarWall", CFrame.new(Center + Vector3.new(0, 0, 360)), Vector3.new(800, 600, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Looks = {
			{ "black", { Color = ColorSequence.new(Color3.new(0, 0, 0)) } },
			{ "white", {} },
			{ "add", { LightEmission = 1, Transparency = Constant(0.5) } },
		}
		for Step, Distance in { 40, 80, 120, 160 } do
			local Scale = Distance / 40
			for Index, Look in Looks do
				for Row, Backed in { true, false } do
					local Position = Center + Vector3.new(((Step - 2.5) * 4.2 + (Index - 2) * 1.3) * Scale, (Row - 1.5) * -3 * Scale, Distance - 40)
					local At = Anchor(Model, `{Look[1]}-d{Distance}-{if Backed then "panel" else "open"}-anchor`, CFrame.new(Position))
					local Set = { Size = Constant(0.5 * Scale) }
					for Key, Value in Look[2] do
						Set[Key] = Value
					end
					Emitter(At, `{Look[1]}-d{Distance}-{if Backed then "panel" else "open"}`, 1, Set)
					if Backed then
						Block(Model, `panel-{Look[1]}-d{Distance}`, CFrame.new(Position + Vector3.new(0, 0, 2)), Vector3.new(1.2, 1.2, 0.2) * Scale, Color3.new(0, 0, 0), Enum.Material.Neon)
					end
				end
			end
		end
	end

	-- partial: Cylinders past ShapePartial 1 and below 0
	do
		local Model = Group("partial", Vector3.new(4000, 0, 0), nil)
		Block(Model, "Backdrop", CFrame.new(4300, 0, 60), Vector3.new(800, 200, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		local Cases = { { 1.1, "Volume" }, { 1.25, "Volume" }, { 1.5, "Volume" }, { 2, "Volume" }, { 3, "Volume" }, { 5, "Volume" }, { -0.25, "Volume" }, { -0.5, "Volume" }, { -1, "Volume" }, { -2, "Volume" }, { 1.5, "Surface" }, { 2, "Surface" }, { 3, "Surface" }, { -0.5, "Surface" } }
		for Index, Case in Cases do
			local Part = Block(Model, `p{Case[1]}-{Case[2]}-part`, CFrame.new(4000 + (Index - 1) * 40, 0, 0), Vector3.new(12, 2, 2), nil, nil, 1)
			Emitter(Part, `p{Case[1]}-{Case[2]}`, 2000, {
				Size = Constant(0.05),
				LightEmission = 1,
				Transparency = Constant(0.8),
				Shape = Enum.ParticleEmitterShape.Cylinder,
				ShapePartial = Case[1],
				ShapeStyle = Enum.ParticleEmitterShapeStyle[Case[2]],
			})
		end
	end

	-- smoke: legacy Smoke in fog
	do
		local Center = Vector3.new(5000, 0, 0)
		local Model = Group("smoke", Center, { eye = { 5000, 0, -90 }, target = { 5000, 0, 0 }, fov = 20 })
		Block(Model, "FarWall", CFrame.new(Center + Vector3.new(0, 0, 310)), Vector3.new(600, 400, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
		Block(Model, "Panel", CFrame.new(Center + Vector3.new(-6, 0, 4)), Vector3.new(10, 14, 0.2), Color3.new(0, 0, 0), Enum.Material.Neon)
		for Index, Backed in { true, false } do
			local X = if Backed then -6 else 6
			local Holder = Block(Model, `smoke-{if Backed then "panel" else "open"}`, CFrame.new(Center + Vector3.new(X, -3, 0)), Vector3.new(0.2, 0.2, 0.2), nil, nil, 1)
			local Smoke = Instance.new("Smoke")
			Smoke.Color = Color3.new(0, 0, 0)
			Smoke.Opacity = 1
			Smoke.RiseVelocity = 0
			Smoke.Size = 3
			Smoke.Parent = Holder
			local At = Anchor(Model, `black-{if Backed then "panel" else "open"}-anchor`, CFrame.new(Center + Vector3.new(X, 4, 0)))
			Emitter(At, `black-{if Backed then "panel" else "open"}`, 1, { Size = Constant(1), Color = ColorSequence.new(Color3.new(0, 0, 0)) })
		end
	end

	Root.Parent = workspace
	return { groups = Groups }
end
