-- Local lights' shadows under LightingStyle Realistic, measured in a Roblox client (0.747, quality level
-- 21) in the dark (Brightness, Ambient, OutdoorAmbient and the environment scales 0). Each phase returns
-- its cases: name, the floor's middle (x, y, z), the light (lx, ly, lz) and the camera to see it from
-- (eye, target, fov; straight down from `height` without them) at `exposure`.
--
-- Context.Phase "carriers": shadowed SurfaceLights (Brightness 2, Range 16) facing down from the underside of a
-- 4 x 1 x 4 carrier 8 studs up: invisible, opaque at Angle 30, 60, 90 and 180 (the shadow's apex 7.46,
-- 3.46, 2 and 2 studs behind the face, outside the carrier), opaque with the light's Shadows off or the
-- carrier's CastShadow off, and a 4-stud cube (the apex inside it); then an invisible carrier with an
-- opaque plate between the face and the apex (Angle 60, 90, 180) and one past the apex.
-- Context.Phase "backs": boards before an 8-stud SurfaceLight face (Range 30, facing +Z, no floor), seen from
-- behind (their side turned away from the face) at -2 EV, with Shadows and without: 4 x 4 boards 0.2 thick
-- 1.5 and 3 studs out at Angle 0, 1 and 2 thick at Angle 0, 0.2 thick at Angle 30, 90 and 180.
-- Context.Phase "graze": shadowed and unshadowed PointLights (Brightness 1) 3 studs over white floors of
-- several sizes, Range 120 and 60, seen from above: how much a light's own shadow darkens the floor it
-- grazes (notes/render.md "Local lights", the grazing darkening).
--
-- ror render fixtures/local-lights.luau --lighting-from <a dark place> --context '{"Phase":"graze"}' ...
return function(Context)
	local Phase = Context and Context.Phase or "carriers"
	local Root = Instance.new("Model")
	Root.Name = "LocalLights"

	local function Block(Name, Position, Size, Options)
		Options = Options or {}
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Color3.new(1, 1, 1)
		Part.Size = Size
		Part.CFrame = CFrame.new(Position)
		Part.Transparency = Options.Transparency or 0
		Part.CastShadow = Options.CastShadow ~= false
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.Parent = Root
		return Part
	end
	-- a light on a carrier of `Size` at `Position` (`Kind`, `Face` an Enum.NormalId, `Angle`; `Opaque`: a
	-- white carrier that casts shadows, else an invisible one that casts nothing)
	local function Fixture(Name, Position, Size, Options)
		local Carrier = Block(`{Name}-lamp`, Position, Size, if Options.Opaque then { CastShadow = Options.CastShadow } else { Transparency = 1, CastShadow = false })
		local Light = Instance.new(Options.Kind or "PointLight")
		Light.Range = Options.Range or 16
		Light.Brightness = Options.Brightness or 2
		Light.Color = Color3.new(1, 1, 1)
		Light.Shadows = Options.Shadows == true
		if Options.Kind and Options.Kind ~= "PointLight" then
			Light.Face = Options.Face or Enum.NormalId.Bottom
			Light.Angle = Options.Angle or 90
		end
		Light.Parent = Carrier
		return Light
	end

	local Layout = {}
	if Phase == "carriers" then
		local CASES: { any } = {
			{ Name = "c-open-a90", Angle = 90 },
			{ Name = "c-opaque-a90", Angle = 90, Opaque = true },
			{ Name = "c-opaque-a60", Angle = 60, Opaque = true },
			{ Name = "c-opaque-a30", Angle = 30, Opaque = true },
			{ Name = "c-opaque-a180", Angle = 180, Opaque = true },
			{ Name = "c-opaque-off-a90", Angle = 90, Opaque = true, Shadows = false },
			{ Name = "c-opaque-cast-off-a90", Angle = 90, Opaque = true, CastShadow = false },
			{ Name = "c-cube-a90", Angle = 90, Opaque = true, Cube = true },
			-- a 6 x 0.2 x 6 plate `Plate` studs over the face (8 up), the carrier invisible
			{ Name = "c-plate-a60", Angle = 60, Plate = 1 },
			{ Name = "c-plate-a90", Angle = 90, Plate = 1 },
			{ Name = "c-plate-a180", Angle = 180, Plate = 1 },
			{ Name = "c-plate-past-a90", Angle = 90, Plate = 3 },
		}
		for Index, Case in CASES do
			local Origin = Vector3.new((Index - 1) * 100, 0, 0)
			Block(`Floor-{Case.Name}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60))
			local Size = if Case.Cube then Vector3.new(4, 4, 4) else Vector3.new(4, 1, 4)
			Fixture(Case.Name, Origin + Vector3.new(0, 8 + Size.Y / 2, 0), Size, { Kind = "SurfaceLight", Angle = Case.Angle, Opaque = Case.Opaque, CastShadow = Case.CastShadow, Shadows = Case.Shadows ~= false })
			if Case.Plate then
				Block(`Plate-{Case.Name}`, Origin + Vector3.new(0, 8 + Case.Plate, 0), Vector3.new(6, 0.2, 6))
			end
			table.insert(Layout, { name = Case.Name, x = Origin.X, y = Origin.Y, z = Origin.Z, lx = Origin.X, ly = 8, lz = Origin.Z, eye = { Origin.X, 18, Origin.Z - 30 }, target = { Origin.X, 0, Origin.Z + 3 }, fov = 50 })
		end
	elseif Phase == "backs" then
		local CASES: { any } = {
			{ Name = "b-d1.5", Distance = 1.5, Angle = 0 },
			{ Name = "b-d3", Distance = 3, Angle = 0 },
			{ Name = "b-t1", Distance = 1.5, Angle = 0, Thick = 1 },
			{ Name = "b-t2", Distance = 1.5, Angle = 0, Thick = 2 },
			{ Name = "b-a30", Distance = 1.5, Angle = 30 },
			{ Name = "b-a90", Distance = 1.5, Angle = 90 },
			{ Name = "b-a180", Distance = 1.5, Angle = 180 },
		}
		local Index = 0
		for _, Case in CASES do
			for _, Shadows in { false, true } do
				local Name = Case.Name .. (if Shadows then "-s" else "")
				local Origin = Vector3.new(Index * 100, 20, 0)
				Index += 1
				Fixture(Name, Origin + Vector3.new(0, 0, -0.5), Vector3.new(8, 8, 1), { Kind = "SurfaceLight", Face = Enum.NormalId.Back, Angle = Case.Angle, Range = 30, Shadows = Shadows })
				-- its near side where the 0.2-stud board's is (light-surface-back: that board's centre `Distance` out)
				local Thick = Case.Thick or 0.2
				local Board = Origin + Vector3.new(0, 0, Case.Distance + (Thick - 0.2) / 2)
				Block(`Board-{Name}`, Board, Vector3.new(4, 4, Thick))
				local Back = Board + Vector3.new(0, 0, Thick / 2)
				table.insert(Layout, { name = Name, x = Back.X, y = Back.Y, z = Back.Z, lx = Origin.X, ly = Origin.Y, lz = Origin.Z, eye = { Back.X + 3, Back.Y + 2, Back.Z + 30 }, target = { Back.X, Back.Y, Back.Z }, fov = 20, exposure = -2 })
			end
		end
	elseif Phase == "graze" then
		local CASES: { any } = {
			{ Range = 120, Half = 30 },
			{ Range = 120, Half = 40 },
			{ Range = 120, Half = 50 },
			{ Range = 120, Half = 60 },
			{ Range = 120, Half = 72 },
			{ Range = 120, Half = 90 },
			{ Range = 60, Half = 30 },
			{ Range = 60, Half = 50 },
		}
		local Index = 0
		for _, Case in CASES do
			for _, Shadows in { false, true } do
				local Name = `g-r{Case.Range}-h{Case.Half}` .. (if Shadows then "-s" else "")
				local Origin = Vector3.new(Index * 300, 0, 3000)
				Index += 1
				Block(`Floor-{Name}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(Case.Half * 2, 1, Case.Half * 2))
				local At = Origin + Vector3.new(0, 3, 0)
				Fixture(Name, At, Vector3.new(1, 1, 1), { Range = Case.Range, Brightness = 1, Shadows = Shadows })
				table.insert(Layout, { name = Name, x = Origin.X, y = Origin.Y, z = Origin.Z, lx = At.X, ly = At.Y, lz = At.Z, height = math.max(Case.Half, 40) * 1.45 + 6, fov = 70, half = Case.Half })
			end
		end
	end
	Root.Parent = workspace
	return Layout
end
