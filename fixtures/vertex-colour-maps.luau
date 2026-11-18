-- Vertex colours against colour maps: 4 x 4 stud quads (an EditableMesh bound with
-- CreateMeshPartAsync) of four flat-coloured cells, each beside the same quad with white vertex
-- colours, under every kind of colour map: none, EditableImages (white; (204, 153, 102) at alpha 255,
-- 128 and 0) as TextureContent, built-in images as TextureID (a white and a black one), a
-- SurfaceAppearance in each AlphaMode (with a ColorMap, and Overlay and Transparency with a NormalMap
-- alone), at two part Colors and Transparency 0 and 0.5; then three quads with one colour per corner,
-- untextured, under the white map and under the coloured one. A black backdrop behind.
--
-- Measured in a Roblox client (0.747) under flat light (Lighting.Ambient 0.7, OutdoorAmbient black,
-- Brightness 0, EnvironmentDiffuseScale and EnvironmentSpecularScale 0, GlobalShadows off) from
-- (0, 22.5, 125) looking down -Z with a field of view of 20, against ror's render of the same:
-- ror render fixtures/vertex-colour-maps.luau --lighting-from <a place with that Lighting> --size 2000x1250 \
--   --camera '{"mode":"lookAt","eye":[0,22.5,125],"target":[0,22.5,0],"fov":20}' --no-bloom
-- (docs/materials.md "MeshPart colour"; ROR_REE: the engine emulator's checkout). It returns each
-- quad's name and centre. A client allows a few EditableMeshes at once, so the quads share three.
return function(Context)
	local AssetService = game:GetService("AssetService")
	local Root = Instance.new("Model")
	Root.Name = "VertexColourMaps"

	local CELLS = {
		Color3.new(1, 0.5, 0.25),
		Color3.new(0.25, 1, 0.5),
		Color3.new(0.5, 0.25, 1),
		Color3.new(0.6, 0.6, 0.6),
	}
	local WHITE_CELLS = { Color3.new(1, 1, 1), Color3.new(1, 1, 1), Color3.new(1, 1, 1), Color3.new(1, 1, 1) }
	local CORNERS = { Color3.new(1, 0, 0), Color3.new(0, 1, 0), Color3.new(0, 0, 1), Color3.new(1, 1, 1) }

	-- an 8 x 8 image of one texel, one image per texel
	local Images = {}
	local function Image(R, G, B, A)
		local Key = `{R},{G},{B},{A}`
		if Images[Key] then
			return Images[Key]
		end
		local Side = 8
		local Pixels = buffer.create(Side * Side * 4)
		for Index = 0, Side * Side - 1 do
			buffer.writeu8(Pixels, Index * 4, R)
			buffer.writeu8(Pixels, Index * 4 + 1, G)
			buffer.writeu8(Pixels, Index * 4 + 2, B)
			buffer.writeu8(Pixels, Index * 4 + 3, A)
		end
		local Editable = AssetService:CreateEditableImage({ Size = Vector2.new(Side, Side) })
		Editable:WritePixelsBuffer(Vector2.zero, Editable.Size, Pixels)
		Images[Key] = Editable
		return Editable
	end

	-- a quad facing +Z: 2 x 2 cells of one colour each (1 top-left, 2 top-right, 3 bottom-left,
	-- 4 bottom-right as seen from +Z), or (Gradient) one colour per corner; one mesh per colour set
	local Meshes = {}
	local function Quad(Colors, Gradient)
		if Meshes[Colors] then
			return AssetService:CreateMeshPartAsync(Content.fromObject(Meshes[Colors]))
		end
		local Mesh = AssetService:CreateEditableMesh()
		Meshes[Colors] = Mesh
		local Normal = Mesh:AddNormal(Vector3.new(0, 0, 1))
		local function Face(A, B, C, Ca, Cb, Cc, Ua, Ub, Uc)
			local Id = Mesh:AddTriangle(A, B, C)
			Mesh:SetFaceNormals(Id, { Normal, Normal, Normal })
			Mesh:SetFaceColors(Id, { Ca, Cb, Cc })
			Mesh:SetFaceUVs(Id, { Ua, Ub, Uc })
		end
		if Gradient then
			local P = {
				Mesh:AddVertex(Vector3.new(-2, -2, 0)),
				Mesh:AddVertex(Vector3.new(2, -2, 0)),
				Mesh:AddVertex(Vector3.new(2, 2, 0)),
				Mesh:AddVertex(Vector3.new(-2, 2, 0)),
			}
			local UVS = { Vector2.new(0, 1), Vector2.new(1, 1), Vector2.new(1, 0), Vector2.new(0, 0) }
			local C, U = {}, {}
			for Index = 1, 4 do
				C[Index] = Mesh:AddColor(Colors[Index], 1)
				U[Index] = Mesh:AddUV(UVS[Index])
			end
			Face(P[1], P[2], P[3], C[1], C[2], C[3], U[1], U[2], U[3])
			Face(P[1], P[3], P[4], C[1], C[3], C[4], U[1], U[3], U[4])
		else
			for Cell = 1, 4 do
				local X0 = ((Cell - 1) % 2) * 2 - 2
				local Y0 = 2 - (math.floor((Cell - 1) / 2) + 1) * 2
				local P = {
					Mesh:AddVertex(Vector3.new(X0, Y0, 0)),
					Mesh:AddVertex(Vector3.new(X0 + 2, Y0, 0)),
					Mesh:AddVertex(Vector3.new(X0 + 2, Y0 + 2, 0)),
					Mesh:AddVertex(Vector3.new(X0, Y0 + 2, 0)),
				}
				local Color = Mesh:AddColor(Colors[Cell], 1)
				local U = {
					Mesh:AddUV(Vector2.new((X0 + 2) / 4, 1 - (Y0 + 2) / 4)),
					Mesh:AddUV(Vector2.new((X0 + 4) / 4, 1 - (Y0 + 2) / 4)),
					Mesh:AddUV(Vector2.new((X0 + 4) / 4, 1 - (Y0 + 4) / 4)),
					Mesh:AddUV(Vector2.new((X0 + 2) / 4, 1 - (Y0 + 4) / 4)),
				}
				Face(P[1], P[2], P[3], Color, Color, Color, U[1], U[2], U[3])
				Face(P[1], P[3], P[4], Color, Color, Color, U[1], U[3], U[4])
			end
		end
		return AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
	end

	local MAPS = {
		{ "none" },
		{ "white", 255, 255, 255, 255 },
		{ "tan", 204, 153, 102, 255 },
		{ "tanhalf", 204, 153, 102, 128 },
		{ "tanzero", 204, 153, 102, 0 },
		{ "idwhite", "rbxasset://textures/SurfacesDefault.png" },
		{ "idblack", "rbxasset://textures/blackBkg_square.png" },
	}
	local FLAT = { "flat", 128, 128, 255, 255 }
	local BLUE = Color3.fromRGB(102, 178, 255)
	local CREAM = Color3.new(0.8, 0.8, 0.5)
	local VERTEX = { { "vc", CELLS }, { "vw", WHITE_CELLS } }

	local Cases = {}
	-- TextureContent and TextureID, at each part Color and Transparency
	for _, Map in MAPS do
		for _, Part in { { "cw", Color3.new(1, 1, 1) }, { "cb", BLUE } } do
			for _, See in { { "t0", 0 }, { "t5", 0.5 } } do
				for _, Vertex in VERTEX do
					table.insert(Cases, { Name = `{Map[1]}-{Part[1]}-{See[1]}-{Vertex[1]}`, Map = Map, Color = Part[2], Transparency = See[2], Cells = Vertex[2] })
				end
			end
		end
	end
	for _, Map in { MAPS[3], MAPS[4] } do
		for _, Vertex in VERTEX do
			table.insert(Cases, { Name = `{Map[1]}-cream-{Vertex[1]}`, Map = Map, Color = CREAM, Transparency = 0, Cells = Vertex[2] })
		end
	end
	-- SurfaceAppearances: (name, map, appearance Color, part Color, AlphaMode, a NormalMap alone)
	local SURFACES = {
		{ "saov-white", MAPS[2], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Overlay },
		{ "satr-white", MAPS[2], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Transparency },
		{ "saov-tan", MAPS[3], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Overlay },
		{ "satr-tan", MAPS[3], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Transparency },
		{ "saov-tanhalf", MAPS[4], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Overlay },
		{ "satr-tanhalf", MAPS[4], Color3.new(1, 1, 1), Color3.new(1, 1, 1), Enum.AlphaMode.Transparency },
		{ "saov-tanhalf-tinted", MAPS[4], BLUE, CREAM, Enum.AlphaMode.Overlay },
		{ "satm-tanhalf", MAPS[4], BLUE, Color3.new(1, 1, 1), Enum.AlphaMode.TintMask },
		{ "satm-tan", MAPS[3], BLUE, Color3.new(1, 1, 1), Enum.AlphaMode.TintMask },
		{ "saov-nomap", FLAT, Color3.new(1, 1, 1), BLUE, Enum.AlphaMode.Overlay, true },
		{ "satr-nomap", FLAT, Color3.new(1, 1, 1), BLUE, Enum.AlphaMode.Transparency, true },
	}
	for _, Entry in SURFACES do
		for _, Vertex in VERTEX do
			table.insert(Cases, { Name = `{Entry[1]}-{Vertex[1]}`, Map = Entry[2], SurfaceColor = Entry[3], Color = Entry[4], Surface = Entry[5], NormalOnly = Entry[6], Transparency = 0, Cells = Vertex[2] })
		end
	end
	for _, Map in { MAPS[1], MAPS[2], MAPS[3] } do
		table.insert(Cases, { Name = `gradient-{Map[1]}`, Map = Map, Color = Color3.new(1, 1, 1), Transparency = 0, Cells = CORNERS, Gradient = true })
	end

	local COLUMNS = 12
	local Layout = {}
	for Index, Case in Cases do
		local Column = (Index - 1) % COLUMNS
		local Row = math.floor((Index - 1) / COLUMNS)
		local Center = Vector3.new((Column - (COLUMNS - 1) / 2) * 5, 40 - Row * 5, 0)
		local Part = Quad(Case.Cells, Case.Gradient)
		Part.Name = Case.Name
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Case.Color
		Part.Transparency = Case.Transparency
		Part.CFrame = CFrame.new(Center)
		local Map = Case.Map
		if Case.Surface then
			local Texture = Content.fromObject(Image(Map[2], Map[3], Map[4], Map[5]))
			local Surface = AssetService:CreateSurfaceAppearanceAsync(if Case.NormalOnly then { NormalMap = Texture } else { ColorMap = Texture })
			Surface.AlphaMode = Case.Surface
			Surface.Color = Case.SurfaceColor
			Surface.Parent = Part
		elseif typeof(Map[2]) == "string" then
			Part.TextureID = Map[2]
		elseif Map[2] then
			Part.TextureContent = Content.fromObject(Image(Map[2], Map[3], Map[4], Map[5]))
		end
		Part.Parent = Root
		table.insert(Layout, { Case.Name, Center.X, Center.Y })
	end

	local Backdrop = Instance.new("Part")
	Backdrop.Name = "Backdrop"
	Backdrop.Anchored = true
	Backdrop.Material = Enum.Material.SmoothPlastic
	Backdrop.Color = Color3.new(0, 0, 0)
	Backdrop.Size = Vector3.new(80, 60, 1)
	Backdrop.CFrame = CFrame.new(0, 22, -3)
	Backdrop.Parent = Root
	Root.Parent = workspace
	return Layout
end
