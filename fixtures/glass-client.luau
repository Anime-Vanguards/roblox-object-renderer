-- Glass in a Roblox client, beyond what the capture Studio's stored glass experiments show (notes/materials.md "Glass").
-- Context.Phase "alpha": 4-stud EditableMesh quads facing +Z whose four vertices have alpha 0, 0.5 or 1 (columns),
-- as Glass at Transparency 0, 0.3 and 0.5, SmoothPlastic at 0 and 0.5 and gray Neon at 0 (rows), once 8 studs before a
-- gray wall and once before the open sky (AlphaSky, 300 studs along +Z): what a MeshPart's vertex alpha does.
-- "reflectance": 2.4-stud cubes at Reflectance 0, 0.1, 0.25, 0.5, 0.75 and 1 (columns), Glass and SmoothPlastic, white,
-- black and lavender (207, 159, 250), at Transparency 0, and lavender Glass at 0.1 and 0.3 (rows), before a dark wall.
-- "light": 60 x 1 x 60 floors of Glass and SmoothPlastic, (90, 110, 120) and white, each under one PointLight
-- (Brightness 1.5, Range 60, no shadows) 13 or 1.5 studs over its middle (as fixtures/materials-generation.luau's
-- "light"), and 30 x 2 x 30 slabs of each lit by the sun or the sky alone (LightSlabs).
-- "sides": Glass EditableMeshes with and without DoubleSided, a quad seen from both sides and a closed sphere, and a
-- Fire, Sparkles and Smoke behind (and a Fire before) panes at Transparency 0.5, before black and white stripes.
-- "sides2": Glass quads of the built-in quad mesh with and without DoubleSided, facing the camera and turned away.
-- "past": Glass past Transparency 1 under disabled Highlights: a flat disc at 65, a Block, a Cylinder and a Ball at 3, a
-- 45-stud cube at 5.
-- "fresnel": 6-stud Balls at Transparency 0 to 0.9 (columns), black Glass and white Glass before a black wall and white
-- Glass before gray Neon panels (rows), seen from 100 studs under flat light at EnvironmentSpecularScale 0, 0.5 and 1
-- and the default lighting: what Glass reflects and passes by angle.
-- "all" (the default) builds every phase, each in its own place, far from the others.
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic, Technology Future, a 2022
-- MaterialService) with each view's Lighting set from `Arms` before its capture; against ror's render of the same:
-- ror render fixtures/glass-client.luau --context '{"Phase":"alpha"}' --size 1644x1116 --no-service
--   --camera '{"mode":"lookAt","eye":[0,52.5,60],"target":[0,52.5,0],"fov":40}' --set Lighting.Ambient=0.7,0.7,0.7 ...
-- (ROR_REE: the engine emulator's checkout). It returns { Views = { { Tag, Eye, Target, Fov, Arm } }, Arms = { [name] =
-- Lighting values } }.
local ARMS = {
	flat = { Ambient = { 0.7, 0.7, 0.7 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 14.5, GlobalShadows = false },
	default = { Ambient = { 0.2745098, 0.2745098, 0.2745098 }, OutdoorAmbient = { 0.2745098, 0.2745098, 0.2745098 }, Brightness = 2, EnvironmentDiffuseScale = 1, EnvironmentSpecularScale = 1, ClockTime = 14.5, GlobalShadows = true },
	sky = { Ambient = { 0, 0, 0 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 1, EnvironmentSpecularScale = 1, ClockTime = 14.5, GlobalShadows = false },
	sun = { Ambient = { 0, 0, 0 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 2, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 14.5, GlobalShadows = false },
	dark = { Ambient = { 0, 0, 0 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 0, GlobalShadows = false },
	flatspec = { Ambient = { 0.7, 0.7, 0.7 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 1, ClockTime = 14.5, GlobalShadows = false },
	flathalf = { Ambient = { 0.7, 0.7, 0.7 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0.5, ClockTime = 14.5, GlobalShadows = false },
}

return function(Context)
	local Phase = Context and Context.Phase or "all"
	local AssetService = game:GetService("AssetService")
	local Root = Instance.new("Model")
	Root.Name = "GlassClient"
	local Views = {}
	local function View(Tag, Eye, Target, Fov, Arm)
		table.insert(Views, { Tag = Tag, Eye = { Eye.X, Eye.Y, Eye.Z }, Target = { Target.X, Target.Y, Target.Z }, Fov = Fov, Arm = Arm })
	end
	local function Block(Name, CF, Size, Material, Color, Transparency)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.CastShadow = false
		Part.Size = Size
		Part.CFrame = CF
		Part.Material = Material
		Part.Color = Color
		Part.Transparency = Transparency or 0
		Part.TopSurface = Enum.SurfaceType.Smooth
		Part.BottomSurface = Enum.SurfaceType.Smooth
		Part.Parent = Root
		return Part
	end
	local function Stripes(Name, Center, Width, Height, Count)
		for Index = 0, Count - 1 do
			local Shade = Index % 2 == 0 and Color3.new(1, 1, 1) or Color3.new(0, 0, 0)
			Block(`{Name}{Index}`, CFrame.new(Center + Vector3.new((Index - (Count - 1) / 2) * Width, 0, 0)), Vector3.new(Width, Height, 0.5), Enum.Material.SmoothPlastic, Shade)
		end
	end
	-- a 4-stud quad facing +Z, its four vertices white at one alpha (a client allows a few EditableMeshes at once,
	-- so the quads of one alpha share one)
	local Meshes = {}
	local function Quad(Alpha)
		if Meshes[Alpha] then
			return AssetService:CreateMeshPartAsync(Content.fromObject(Meshes[Alpha]))
		end
		local Mesh = AssetService:CreateEditableMesh()
		Meshes[Alpha] = Mesh
		local Normal = Mesh:AddNormal(Vector3.new(0, 0, 1))
		local Color = Mesh:AddColor(Color3.new(1, 1, 1), Alpha)
		local P = {
			Mesh:AddVertex(Vector3.new(-2, -2, 0)),
			Mesh:AddVertex(Vector3.new(2, -2, 0)),
			Mesh:AddVertex(Vector3.new(2, 2, 0)),
			Mesh:AddVertex(Vector3.new(-2, 2, 0)),
		}
		for _, Face in { Mesh:AddTriangle(P[1], P[2], P[3]), Mesh:AddTriangle(P[1], P[3], P[4]) } do
			Mesh:SetFaceNormals(Face, { Normal, Normal, Normal })
			Mesh:SetFaceColors(Face, { Color, Color, Color })
		end
		return AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
	end
	-- a closed sphere of radius 2, its triangles facing out
	local function Sphere()
		local Mesh = AssetService:CreateEditableMesh()
		local Rings, Segments = 12, 24
		local Grid = {}
		for Ring = 0, Rings do
			local Theta = math.pi * Ring / Rings
			Grid[Ring] = {}
			for Segment = 0, Segments - 1 do
				local Phi = 2 * math.pi * Segment / Segments
				local Direction = Vector3.new(math.sin(Theta) * math.cos(Phi), math.cos(Theta), math.sin(Theta) * math.sin(Phi))
				local Vertex = Mesh:AddVertex(Direction * 2)
				Grid[Ring][Segment] = { Vertex, Mesh:AddNormal(Direction) }
			end
		end
		local function Face(A, B, C)
			local Id = Mesh:AddTriangle(A[1], B[1], C[1])
			Mesh:SetFaceNormals(Id, { A[2], B[2], C[2] })
		end
		for Ring = 0, Rings - 1 do
			for Segment = 0, Segments - 1 do
				local Next = (Segment + 1) % Segments
				local A, B, C, D = Grid[Ring][Segment], Grid[Ring][Next], Grid[Ring + 1][Next], Grid[Ring + 1][Segment]
				if Ring > 0 then
					Face(A, B, C)
				end
				if Ring < Rings - 1 then
					Face(A, C, D)
				end
			end
		end
		return AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
	end
	local function Place(Part, Name, CF, Size, Material, Color, Transparency)
		Part.Name = Name
		Part.Anchored = true
		Part.CastShadow = false
		Part.Size = Size
		Part.CFrame = CF
		Part.Material = Material
		Part.Color = Color
		Part.Transparency = Transparency
		Part.Parent = Root
		return Part
	end

	if Phase == "all" or Phase == "alpha" then
		local ALPHAS = { 0, 0.5, 1 }
		local ROWS = {
			{ "glass-t0", Enum.Material.Glass, Color3.new(1, 1, 1), 0 },
			{ "glass-t0.3", Enum.Material.Glass, Color3.new(1, 1, 1), 0.3 },
			{ "glass-t0.5", Enum.Material.Glass, Color3.new(1, 1, 1), 0.5 },
			{ "plastic-t0", Enum.Material.SmoothPlastic, Color3.new(1, 1, 1), 0 },
			{ "plastic-t0.5", Enum.Material.SmoothPlastic, Color3.new(1, 1, 1), 0.5 },
			{ "neon-t0", Enum.Material.Neon, Color3.new(0.5, 0.5, 0.5), 0 },
		}
		for _, Copy in { { "AlphaWall", Vector3.new(0, 50, 0), true }, { "AlphaSky", Vector3.new(0, 50, 300), false } } do
			local Origin = Copy[2]
			for Row, Spec in ROWS do
				for Column, Alpha in ALPHAS do
					local At = Origin + Vector3.new((Column - 2) * 5, (3.5 - Row) * 5, 0)
					Place(Quad(Alpha), `{Copy[1]}-{Spec[1]}-a{Alpha}`, CFrame.new(At), Vector3.new(4, 4, 0.001), Spec[2], Spec[3], Spec[4])
				end
			end
			if Copy[3] then
				Block(`{Copy[1]}-wall`, CFrame.new(Origin + Vector3.new(0, 0, -8)), Vector3.new(60, 60, 1), Enum.Material.SmoothPlastic, Color3.fromRGB(100, 100, 100))
			end
			View(Copy[1], Origin + Vector3.new(0, 0, 60), Origin, 40, "flat")
			View(`{Copy[1]}-default`, Origin + Vector3.new(0, 0, 60), Origin, 40, "default")
		end
	end

	if Phase == "all" or Phase == "reflectance" then
		local Origin = Vector3.new(1000, 50, 0)
		local REFLECTANCES = { 0, 0.1, 0.25, 0.5, 0.75, 1 }
		local LAVENDER = Color3.fromRGB(207, 159, 250)
		local ROWS = {
			{ "glass-white", Enum.Material.Glass, Color3.new(1, 1, 1), 0 },
			{ "glass-black", Enum.Material.Glass, Color3.new(0, 0, 0), 0 },
			{ "glass-lavender", Enum.Material.Glass, LAVENDER, 0 },
			{ "plastic-white", Enum.Material.SmoothPlastic, Color3.new(1, 1, 1), 0 },
			{ "plastic-black", Enum.Material.SmoothPlastic, Color3.new(0, 0, 0), 0 },
			{ "plastic-lavender", Enum.Material.SmoothPlastic, LAVENDER, 0 },
			{ "glass-lavender-t0.1", Enum.Material.Glass, LAVENDER, 0.1 },
			{ "glass-lavender-t0.3", Enum.Material.Glass, LAVENDER, 0.3 },
		}
		for Row, Spec in ROWS do
			for Column, Reflectance in REFLECTANCES do
				local At = Origin + Vector3.new((Column - 3.5) * 3.2, (4.5 - Row) * 3.2, 0)
				local Part = Block(`Refl-{Spec[1]}-r{Reflectance}`, CFrame.new(At), Vector3.new(2.4, 2.4, 2.4), Spec[2], Spec[3], Spec[4])
				Part.Reflectance = Reflectance
			end
		end
		Block("Refl-wall", CFrame.new(Origin + Vector3.new(0, 0, -8)), Vector3.new(60, 60, 1), Enum.Material.SmoothPlastic, Color3.fromRGB(40, 40, 40))
		for _, Arm in { "flat", "sky", "sun", "default" } do
			View(`Refl-{Arm}`, Origin + Vector3.new(0, 0, 60), Origin, 35, Arm)
		end
	end

	if Phase == "all" or Phase == "light" then
		local FLOORS = {
			{ "glass", Enum.Material.Glass, Color3.fromRGB(90, 110, 120) },
			{ "plastic", Enum.Material.SmoothPlastic, Color3.fromRGB(90, 110, 120) },
			{ "glass-white", Enum.Material.Glass, Color3.new(1, 1, 1) },
			{ "plastic-white", Enum.Material.SmoothPlastic, Color3.new(1, 1, 1) },
		}
		for Index, Spec in FLOORS do
			for Row, Height in { 13, 1.5 } do
				local Origin = Vector3.new(2000 + (Index - 1) * 200, 0, (Row - 1) * 200)
				Block(`Light-{Spec[1]}-h{Height}`, CFrame.new(Origin), Vector3.new(60, 1, 60), Spec[2], Spec[3], 0)
				local Holder = Block(`Light-{Spec[1]}-h{Height}-holder`, CFrame.new(Origin + Vector3.new(0, 0.5 + Height, 0)), Vector3.new(0.2, 0.2, 0.2), Enum.Material.SmoothPlastic, Color3.new(0, 0, 0), 1)
				local Light = Instance.new("PointLight")
				Light.Brightness = 1.5
				Light.Range = 60
				Light.Shadows = false
				Light.Parent = Holder
				View(`Light-{Spec[1]}-h{Height}`, Origin + Vector3.new(-30, 5, 0), Origin, 70, "dark")
			end
		end
		-- slabs side by side, seen from above and to the side
		local Origin = Vector3.new(2000, 0, 600)
		for Index, Spec in FLOORS do
			Block(`LightSlab-{Spec[1]}`, CFrame.new(Origin + Vector3.new((Index - 2.5) * 36, 0, 0)), Vector3.new(30, 2, 30), Spec[2], Spec[3], 0)
		end
		for _, Arm in { "sun", "sky", "flat" } do
			View(`LightSlabs-{Arm}`, Origin + Vector3.new(0, 90, 90), Origin, 60, Arm)
		end
	end

	if Phase == "all" or Phase == "sides" then
		local Origin = Vector3.new(3000, 50, 0)
		Stripes("Sides-stripe", Origin + Vector3.new(0, 0, -10), 1.5, 30, 30)
		local WHITE = Color3.new(1, 1, 1)
		-- quads (front faces +Z, the camera at +Z): turned half round they show their back
		for Column, Spec in { { "quad-t0.5", 0.5, false }, { "quad-t0.5-ds", 0.5, true }, { "quad-t0-ds", 0, true }, { "quad-t0.8-ds", 0.8, true } } do
			for Row, Turn in { 0, math.pi } do
				local At = Origin + Vector3.new((Column - 2.5) * 6, 8 - Row * 6, 0)
				local Part = Place(Quad(1), `Sides-{Spec[1]}-{Row == 1 and "front" or "back"}`, CFrame.new(At) * CFrame.Angles(0, Turn, 0), Vector3.new(4, 4, 0.001), Enum.Material.Glass, WHITE, Spec[2])
				Part.DoubleSided = Spec[3]
			end
		end
		-- closed spheres
		for Column, Spec in { { "sphere-t0.5", 0.5, false }, { "sphere-t0.5-ds", 0.5, true } } do
			local Part = Place(Sphere(), `Sides-{Spec[1]}`, CFrame.new(Origin + Vector3.new((Column - 1.5) * 6, -4, 0)), Vector3.new(4, 4, 4), Enum.Material.Glass, WHITE, Spec[2])
			Part.DoubleSided = Spec[3]
		end
		-- legacy effects behind and before panes
		for Column, Spec in { { "fire-behind", "Fire", -2.5, Enum.Material.Glass }, { "fire-front", "Fire", 2.5, Enum.Material.Glass }, { "fire-plastic", "Fire", -2.5, Enum.Material.SmoothPlastic }, { "sparkles-behind", "Sparkles", -2.5, Enum.Material.Glass }, { "smoke-behind", "Smoke", -2.5, Enum.Material.Glass } } do
			local At = Origin + Vector3.new((Column - 3) * 7, -11, 0)
			Block(`Sides-{Spec[1]}-pane`, CFrame.new(At), Vector3.new(6, 6, 0.2), Spec[4], WHITE, 0.5)
			local Carrier = Block(`Sides-{Spec[1]}-carrier`, CFrame.new(At + Vector3.new(0, -1.5, Spec[3])), Vector3.new(1, 1, 1), Enum.Material.SmoothPlastic, WHITE, 1)
			Instance.new(Spec[2]).Parent = Carrier
		end
		View("Sides", Origin + Vector3.new(0, -1.5, 45), Origin + Vector3.new(0, -1.5, 0), 45, "flat")
	end

	if Phase == "all" or Phase == "offaxis" then
		-- facing 3-stud panes across the view (OffaxisX: along X at Transparency 0.6, 0.7 and 0.8 in rows 6 studs
		-- apart, before vertical stripes; OffaxisY: along Y in columns, before horizontal stripes) and 4-stud Balls
		-- along X at 0.7 (OffaxisBalls), all 4 studs before gray Neon stripes 1 stud wide and 2 apart on black,
		-- seen head-on from 30 studs (fov 40)
		local function Panel(Name, Center, Vertical)
			Block(`{Name}-wall`, CFrame.new(Center + Vector3.new(0, 0, -4.5)), Vector3.new(80, 60, 0.5), Enum.Material.SmoothPlastic, Color3.new(0, 0, 0))
			for Stripe = -20, 20 do
				local Offset = Stripe * 2
				local At = Vertical and Vector3.new(Offset, 0, -4) or Vector3.new(0, Offset, -4)
				local Size = Vertical and Vector3.new(1, 60, 0.2) or Vector3.new(80, 1, 0.2)
				Block(`{Name}-stripe{Stripe}`, CFrame.new(Center + At), Size, Enum.Material.Neon, Color3.fromRGB(140, 140, 140))
			end
		end
		local WHITE = Color3.new(1, 1, 1)
		local OriginX = Vector3.new(4000, 50, 0)
		Panel("OffaxisX", OriginX, true)
		for Row, T in { 0.6, 0.7, 0.8 } do
			for Index = -3, 3 do
				Block(`OffaxisX-t{T}-x{Index * 5}`, CFrame.new(OriginX + Vector3.new(Index * 5, (2 - Row) * 6, 0)), Vector3.new(3, 3, 0.2), Enum.Material.Glass, WHITE, T)
			end
		end
		View("OffaxisX", OriginX + Vector3.new(0, 0, 30), OriginX, 40, "default")
		local OriginY = Vector3.new(4000, 50, 300)
		Panel("OffaxisY", OriginY, false)
		for Column, T in { 0.6, 0.7, 0.8 } do
			for Index = -2, 2 do
				Block(`OffaxisY-t{T}-y{Index * 4.5}`, CFrame.new(OriginY + Vector3.new((Column - 2) * 7, Index * 4.5, 0)), Vector3.new(3, 3, 0.2), Enum.Material.Glass, WHITE, T)
			end
		end
		View("OffaxisY", OriginY + Vector3.new(0, 0, 30), OriginY, 40, "default")
		local OriginB = Vector3.new(4000, 50, 600)
		Panel("OffaxisBalls", OriginB, true)
		for Index = -3, 3 do
			local Ball = Block(`OffaxisBalls-x{Index * 5}`, CFrame.new(OriginB + Vector3.new(Index * 5, 0, 0)), Vector3.new(4, 4, 4), Enum.Material.Glass, WHITE, 0.7)
			Ball.Shape = Enum.PartType.Ball
		end
		View("OffaxisBalls", OriginB + Vector3.new(0, 0, 30), OriginB, 40, "default")
	end

	if Phase == "all" or Phase == "fresnel" then
		local Origin = Vector3.new(5000, 50, 0)
		local WHITE, BLACK = Color3.new(1, 1, 1), Color3.new(0, 0, 0)
		for Column = 0, 9 do
			local T = Column / 10
			for Row, Spec in { { "black", BLACK, false }, { "white", WHITE, false }, { "panel", WHITE, true } } do
				local At = Origin + Vector3.new((Column - 4.5) * 7, (2 - Row) * 8, 0)
				local Ball = Block(`Fresnel-{Spec[1]}-t{T}`, CFrame.new(At), Vector3.new(6, 6, 6), Enum.Material.Glass, Spec[2], T)
				Ball.Shape = Enum.PartType.Ball
				if Spec[3] then
					Block(`Fresnel-{Spec[1]}-t{T}-panel`, CFrame.new(At + Vector3.new(0, 0, -6)), Vector3.new(7, 7.5, 0.2), Enum.Material.Neon, Color3.fromRGB(140, 140, 140))
				end
			end
		end
		Block("Fresnel-wall", CFrame.new(Origin + Vector3.new(0, 0, -6.5)), Vector3.new(120, 60, 0.5), Enum.Material.SmoothPlastic, BLACK)
		for _, Arm in { "flat", "flathalf", "flatspec", "default" } do
			View(`Fresnel-{Arm}`, Origin + Vector3.new(0, 0, 100), Origin, 30, Arm)
		end
	end

	if Phase == "all" or Phase == "sides2" then
		-- the same with a file mesh (the built-in quad, Context.Mesh for the engine emulator), DoubleSided set
		-- before it is parented: Glass at Transparency 0, 0.5 and 0.8, with and without DoubleSided (columns),
		-- facing the camera and turned half round (rows)
		local QUAD = Context and Context.Mesh or "rbxasset://avatar/compositing/CompositQuad.mesh"
		local Origin = Vector3.new(6000, 50, 0)
		Stripes("Sides2-stripe", Origin + Vector3.new(0, 0, -10), 1.5, 30, 30)
		local Column = 0
		for _, Spec in { { "t0", 0, false }, { "t0-ds", 0, true }, { "t0.5", 0.5, false }, { "t0.5-ds", 0.5, true }, { "t0.8-ds", 0.8, true } } do
			Column += 1
			for Row, Turn in { 0, math.pi } do
				local Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
				Part.DoubleSided = Spec[3]
				local At = Origin + Vector3.new((Column - 3) * 6, 5 - (Row - 1) * 8, 0)
				Place(Part, `Sides2-{Spec[1]}-r{Row}`, CFrame.new(At) * CFrame.Angles(0, Turn, 0), Vector3.new(4, 4, 0.001), Enum.Material.Glass, Color3.fromRGB(120, 200, 140), Spec[2])
			end
		end
		View("Sides2", Origin + Vector3.new(0, 1, 40), Origin + Vector3.new(0, 1, 0), 45, "default")
	end

	if Phase == "all" or Phase == "past" then
		-- Glass past Transparency 1, each under a disabled Highlight (which keeps it drawn), before stripes of six
		-- colours 1.5 studs wide 8 studs behind: a flat disc 7.8 studs across at 65 (a Cylinder 0.2 thick, its face
		-- to the camera), a Block, a Cylinder and a Ball 6 studs across at 3, and a 45-stud cube at 5
		local Origin = Vector3.new(7000, 50, 0)
		local COLOURS = { { 230, 60, 60 }, { 240, 200, 50 }, { 60, 190, 80 }, { 50, 170, 220 }, { 70, 70, 210 }, { 200, 80, 200 } }
		local function Backdrop(Name, Center, Width, Count)
			for Index = 0, Count - 1 do
				local C = COLOURS[Index % #COLOURS + 1]
				Block(`{Name}{Index}`, CFrame.new(Center + Vector3.new((Index - (Count - 1) / 2) * Width, 0, 0)), Vector3.new(Width, Width * Count * 0.7, 0.5), Enum.Material.SmoothPlastic, Color3.fromRGB(C[1], C[2], C[3]))
			end
		end
		local function Kept(Part)
			local Highlight = Instance.new("Highlight")
			Highlight.Enabled = false
			Highlight.Parent = Part
			return Part
		end
		local WHITE = Color3.new(1, 1, 1)
		Backdrop("Past-stripe", Origin + Vector3.new(0, 0, -8), 1.5, 40)
		Kept(Block("Past-disc", CFrame.new(Origin + Vector3.new(-12, 0, 0)) * CFrame.Angles(0, math.pi / 2, 0), Vector3.new(0.2, 7.8, 7.8), Enum.Material.Glass, WHITE, 65)).Shape = Enum.PartType.Cylinder
		Kept(Block("Past-block", CFrame.new(Origin + Vector3.new(-3, 0, 0)), Vector3.new(6, 6, 6), Enum.Material.Glass, WHITE, 3))
		Kept(Block("Past-cylinder", CFrame.new(Origin + Vector3.new(5, 0, 0)), Vector3.new(6, 6, 6), Enum.Material.Glass, WHITE, 3)).Shape = Enum.PartType.Cylinder
		Kept(Block("Past-ball", CFrame.new(Origin + Vector3.new(13, 0, 0)), Vector3.new(6, 6, 6), Enum.Material.Glass, WHITE, 3)).Shape = Enum.PartType.Ball
		View("Past", Origin + Vector3.new(0, 0, 40), Origin, 50, "flat")
		local Cube = Vector3.new(7000, 50, 600)
		Backdrop("PastCube-stripe", Cube + Vector3.new(0, 0, -40), 6, 40)
		Kept(Block("PastCube", CFrame.new(Cube), Vector3.new(45, 45, 45), Enum.Material.Glass, WHITE, 5))
		View("PastCube", Cube + Vector3.new(0, 0, 120), Cube, 50, "flat")
	end

	if Phase ~= "all" and #Views == 0 then
		error(`glass-client: no phase {Phase}`)
	end
	Root.Parent = workspace
	return { Views = Views, Arms = ARMS }
end
