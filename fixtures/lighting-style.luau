-- What Lighting.LightingStyle changes. Context.Phase "lights" (the default): one local light a case, each
-- over its own white SmoothPlastic floor 100 studs from the next (so no light reaches another case's
-- floor), with or without Shadows, past plates of several transparencies and sizes, a MeshPart quad, a
-- wall, a ball, a SpotLight and a SurfaceLight. Context.Phase "voxels": a dim shadowed PointLight at a
-- voxel's centre and one occluder a case filling a voxel round it. Context.Phase "day": one floor in the
-- sun with plates, a pole, balls of several materials, a roof and shadowed PointLights, for whatever else
-- the style changes.
--
-- Measured in a Roblox client (0.747, quality level 21) in the dark (Brightness, Ambient, OutdoorAmbient
-- and the environment scales 0) and under a lit Lighting, every case seen straight down from 40 studs
-- under each style in turn. A client takes Realistic whatever the place file says, and LightingStyle is
-- written only with capability PluginOrOpenCloud, so the job that switches it runs at identity 4. Against
-- ror's render of the same, from a place file with the style:
-- ror render fixtures/lighting-style.luau --lighting-from <the place> --size 1600x900 --no-service --   --camera '{"mode":"lookAt","eye":[<x>,40,-0.05],"target":[<x>,0,0],"fov":40}'   --context '{"Phase":"lights","Mesh":"file:///<Studio>/content/avatar/compositing/CompositQuad.mesh"}'
-- (docs/rendering.md "Lighting style"; ROR_REE: the engine emulator's checkout). It returns each case's
-- name, its floor's middle and its light's position.
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Phase = Context and Context.Phase or "lights"
	local Root = Instance.new("Model")
	Root.Name = "LightingStyle"

	-- the install's quad mesh (1024 units square in XY from its origin, facing +Z) turned to face up, its
	-- middle on the part's position; the engine emulator loads it from a file:// path (Context.Mesh)
	local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local FACE_UP = CFrame.Angles(-math.pi / 2, 0, 0)

	local function Block(Name, Position, Size, Options)
		Options = Options or {}
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Options.Material or Enum.Material.SmoothPlastic
		Part.Color = Options.Color or Color3.new(1, 1, 1)
		Part.Size = Size
		Part.CFrame = CFrame.new(Position)
		Part.Transparency = Options.Transparency or 0
		Part.CastShadow = Options.CastShadow ~= false
		if Options.Shape then
			Part.Shape = Options.Shape
		end
		Part.Parent = Root
		return Part
	end
	local function Quad(Name, Position, Side)
		local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
		Part.Name = Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Color3.new(1, 1, 1)
		Part.Size = Vector3.new(Side, Side, 0.001)
		Part.CFrame = CFrame.new(Position) * FACE_UP * CFrame.new(-Side / 2, -Side / 2, 0)
		Part.Parent = Root
		return Part
	end
	-- a light on an invisible carrier that casts nothing (`Kind` PointLight, SpotLight or SurfaceLight; a
	-- SurfaceLight's carrier is 4 x 1 x 4 with its Bottom face at `Position`)
	local function Lamp(Name, Position, Options)
		local Kind = Options.Kind or "PointLight"
		local Carrier = Instance.new("Part")
		Carrier.Name = `{Name}-lamp`
		Carrier.Anchored = true
		Carrier.Transparency = 1
		Carrier.CastShadow = false
		Carrier.Size = if Kind == "SurfaceLight" then Vector3.new(4, 1, 4) else Vector3.new(0.2, 0.2, 0.2)
		Carrier.CFrame = CFrame.new(Position + (if Kind == "SurfaceLight" then Vector3.new(0, 0.5, 0) else Vector3.zero))
		Carrier.Parent = Root
		local Light = Instance.new(Kind)
		Light.Range = Options.Range or 16
		Light.Brightness = Options.Brightness or 2
		Light.Color = Options.Color or Color3.new(1, 1, 1)
		Light.Shadows = Options.Shadows == true
		if Kind ~= "PointLight" then
			Light.Face = Enum.NormalId.Bottom
			Light.Angle = Options.Angle or 120
		end
		Light.Parent = Carrier
		return Light
	end

	local Layout = {}
	if Phase == "day" then
		-- a grey floor 120 studs across; plates 3 studs up in the sun, a pole, balls, a roof of Parts with a
		-- shadowed PointLight under it (the light grid's terms and a local light beside the sun)
		Block("Floor", Vector3.new(0, -0.5, 0), Vector3.new(120, 1, 120), { Color = Color3.fromRGB(200, 200, 200) })
		Block("plate", Vector3.new(-30, 3, -30), Vector3.new(6, 0.5, 6))
		Block("plate-t0.5", Vector3.new(-15, 3, -30), Vector3.new(6, 0.5, 6), { Transparency = 0.5 })
		Block("pole", Vector3.new(0, 6, -30), Vector3.new(1, 12, 1))
		Quad("quad", Vector3.new(15, 3, -30), 6)
		Block("ball-plastic", Vector3.new(-30, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball })
		Block("ball-metal", Vector3.new(-22, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball, Material = Enum.Material.Metal })
		Block("ball-glass", Vector3.new(-14, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball, Material = Enum.Material.Glass, Transparency = 0.3 })
		Block("ball-neon", Vector3.new(-6, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball, Material = Enum.Material.Neon, Color = Color3.fromRGB(255, 120, 40) })
		Block("roof", Vector3.new(24, 10.5, 24), Vector3.new(24, 1, 24))
		Block("under-plate", Vector3.new(28, 3, 24), Vector3.new(3, 0.4, 3))
		Lamp("under", Vector3.new(24, 6, 24), { Range = 16, Brightness = 2, Shadows = true, Color = Color3.fromRGB(255, 200, 120) })
		Lamp("open", Vector3.new(-24, 6, 30), { Range = 16, Brightness = 2, Shadows = true, Color = Color3.fromRGB(120, 180, 255) })
		Block("open-plate", Vector3.new(-20, 3, 30), Vector3.new(3, 0.4, 3))
		table.insert(Layout, { name = "day", x = 0, y = 0, z = 0 })
		Root.Parent = workspace
		return Layout
	end

	-- Context.Phase "voxels": a dim shadowed PointLight (Brightness 0.5, Range 16) at the centre of the
	-- 4-stud voxel (0..4, 8..12, 0..4) of its floor, and one occluder a case filling a voxel of the grid
	-- (`At`: the voxel's centre) or part of it, beside, under or over the light
	if Phase == "voxels" then
		local VOXEL_CASES: { any } = {
			{ Name = "v-none" },
			{ Name = "v-under", At = Vector3.new(2, 6, 2) },
			{ Name = "v-side", At = Vector3.new(10, 6, 2) },
			{ Name = "v-far", At = Vector3.new(18, 6, 2) },
			{ Name = "v-level", At = Vector3.new(10, 10, 2) },
			{ Name = "v-above", At = Vector3.new(2, 14, 2) },
			{ Name = "v-low", At = Vector3.new(10, 2, 2) },
			{ Name = "v-side-t0.5", At = Vector3.new(10, 6, 2), Transparency = 0.5 },
			{ Name = "v-side-t0.75", At = Vector3.new(10, 6, 2), Transparency = 0.75 },
			{ Name = "v-side-t0.9", At = Vector3.new(10, 6, 2), Transparency = 0.9 },
			{ Name = "v-side-off", At = Vector3.new(10, 6, 2), LightShadows = false },
			{ Name = "v-side-plate", At = Vector3.new(10, 6, 2), Size = Vector3.new(4, 0.4, 4) },
			{ Name = "v-side-cube1", At = Vector3.new(10, 6, 2), Size = Vector3.new(1, 1, 1) },
			{ Name = "v-side-ball", At = Vector3.new(10, 6, 2), Shape = Enum.PartType.Ball },
			{ Name = "v-side-quad", At = Vector3.new(10, 6, 2), Quad = true },
			{ Name = "v-side-noshadow", At = Vector3.new(10, 6, 2), CastShadow = false },
			{ Name = "v-wall", At = Vector3.new(10, 8, 0), Size = Vector3.new(4, 16, 40) },
		}
		for Index, Case in VOXEL_CASES do
			local Origin = Vector3.new((Index - 1) * 100, 0, 0)
			Block(`Floor-{Case.Name}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60))
			if Case.Quad then
				Quad(`Occluder-{Case.Name}`, Origin + Case.At, 4)
			elseif Case.At then
				Block(`Occluder-{Case.Name}`, Origin + Case.At, Case.Size or Vector3.new(4, 4, 4), { Transparency = Case.Transparency, Shape = Case.Shape, CastShadow = Case.CastShadow })
			end
			local At = Origin + Vector3.new(2, 10, 2)
			Lamp(Case.Name, At, { Brightness = 0.5, Shadows = Case.LightShadows ~= false })
			table.insert(Layout, { name = Case.Name, x = Origin.X, y = Origin.Y, z = Origin.Z, lx = At.X, ly = At.Y, lz = At.Z })
		end
		Root.Parent = workspace
		return Layout
	end

	-- one case a floor, 100 studs apart along X: (name, what stands over the floor, the light)
	local OFF_AXIS = Vector3.new(5, 4, 0)
	local CASES: { any } = {
		{ Name = "open-on", Shadows = true },
		{ Name = "open-off" },
		{ Name = "plate-on", Shadows = true, Plate = { 6, 0 } },
		{ Name = "plate-off", Plate = { 6, 0 } },
		{ Name = "plate-t0.25", Shadows = true, Plate = { 6, 0.25 } },
		{ Name = "plate-t0.5", Shadows = true, Plate = { 6, 0.5 } },
		{ Name = "plate-t0.7", Shadows = true, Plate = { 6, 0.7 } },
		{ Name = "plate-t0.75", Shadows = true, Plate = { 6, 0.75 } },
		{ Name = "plate-t0.9", Shadows = true, Plate = { 6, 0.9 } },
		{ Name = "plate1-on", Shadows = true, Plate = { 1, 0 } },
		{ Name = "plate2-on", Shadows = true, Plate = { 2, 0 } },
		{ Name = "quad-on", Shadows = true, Quad = 6 },
		{ Name = "wall-on", Shadows = true, Wall = true, Light = Vector3.new(0, 4, 0) },
		{ Name = "wall-off", Wall = true, Light = Vector3.new(0, 4, 0) },
		{ Name = "spot-on", Shadows = true, Plate = { 6, 0 }, Kind = "SpotLight" },
		{ Name = "surface-on", Shadows = true, Plate = { 6, 0 }, Kind = "SurfaceLight" },
		{ Name = "graze-on", Shadows = true, Light = Vector3.new(0, 0.5, 0) },
		{ Name = "graze-off", Light = Vector3.new(0, 0.5, 0) },
		{ Name = "ball-on", Shadows = true, Ball = true, Light = Vector3.new(0, 6, 0) },
		{ Name = "ball-off", Ball = true, Light = Vector3.new(0, 6, 0) },
		{ Name = "high-on", Shadows = true, Plate = { 6, 0 }, Light = Vector3.new(0, 16, 0), Range = 32, PlateAt = Vector3.new(5, 10, 0) },
		{ Name = "color-on", Shadows = true, Plate = { 6, 0 }, Color = Color3.fromRGB(255, 120, 40) },
	}
	for Index, Case in CASES do
		local Origin = Vector3.new((Index - 1) * 100, 0, 0)
		Block(`Floor-{Case.Name}`, Origin + Vector3.new(0, -0.5, 0), Vector3.new(60, 1, 60))
		if Case.Plate then
			Block(`Plate-{Case.Name}`, Origin + (Case.PlateAt or OFF_AXIS), Vector3.new(Case.Plate[1], 0.4, Case.Plate[1]), { Transparency = Case.Plate[2] })
		end
		if Case.Quad then
			Quad(`Quad-{Case.Name}`, Origin + OFF_AXIS, Case.Quad)
		end
		if Case.Wall then
			-- a wall 1 stud thick and 12 tall, 4 studs from the light along +X
			Block(`Wall-{Case.Name}`, Origin + Vector3.new(4.5, 6, 0), Vector3.new(1, 12, 24))
		end
		if Case.Ball then
			Block(`Ball-{Case.Name}`, Origin + Vector3.new(5, 2, 0), Vector3.new(4, 4, 4), { Shape = Enum.PartType.Ball })
		end
		local At = Origin + (Case.Light or Vector3.new(0, 8, 0))
		Lamp(Case.Name, At, { Kind = Case.Kind, Shadows = Case.Shadows, Range = Case.Range, Color = Case.Color })
		table.insert(Layout, { name = Case.Name, x = Origin.X, y = Origin.Y, z = Origin.Z, lx = At.X, ly = At.Y, lz = At.Z })
	end
	Root.Parent = workspace
	return Layout
end
