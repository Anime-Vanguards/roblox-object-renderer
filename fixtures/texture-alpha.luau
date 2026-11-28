-- Texture alpha, vertex colours and EditableMesh casters (notes/render.md "Texture alpha in a client"): what a part's
-- colour map's alpha does to its colour and its shadow, on MeshParts and SpecialMeshes, from image files, EditableImages
-- and uploaded images, at Transparency 0 to 0.5; a FileMesh's vertex colours and alpha on SpecialMeshes and on textured
-- MeshParts; and whether MeshParts made from EditableMeshes cast shadows.
--
-- Textures come in `Context` (URIs; cases whose texture is missing are left out): Tan0, Tan64, Tan65, Tan128, Tan192,
-- Tan255 (8 x 8 images of (204, 153, 102) at that alpha), White (opaque white), Checker (64 x 64, 4 x 4 squares of tan
-- at alpha 255 and 0, the corner opaque), Smile (an image with alpha, as a file) and Hosted (the same image uploaded).
-- Meshes: Quad (the URI CreateMeshPartAsync takes for the install's quad mesh, 1024 units square in XY from its
-- origin, facing +Z; the engine emulator reads file:// paths), QuadId (its content id for SpecialMeshes, default the
-- install's), VertexColours / VertexAlpha (URIs of 4 x 4 stud quads facing +Z, centred, in four cells: 1 top-left, 2
-- top-right, 3 bottom-left, 4 bottom-right, with vertex colours (255, 128, 64), (64, 255, 128), (128, 64, 255), (153,
-- 153, 153), or white at alpha 255, 128, 64 and 0) and VertexColoursId / VertexAlphaId (their content ids).
--
-- "colour" (x 0): 4 x 4 stud quads facing +Z in rows of ten, 5 studs apart, before a green wall 3 studs behind, seen
--   from +Z under flat light (arm "flat"): MeshParts (`mp-`) and SpecialMeshes (`sm-`) of the quad mesh, EditableMesh
--   MeshParts (`em-`) under each texture (`L<alpha>` the tan files, `E<alpha>` EditableImages of tan, `HS` Hosted,
--   `LS` Smile) on red parts at Transparency 0, 0.01, 0.02 and 0.5 (`t<T>`), a few on white (`-w`); then the vertex
--   colour and alpha quads as SpecialMeshes (`vcs-`, `vas-`) and MeshParts (`vcm-`, `vam-`), untextured (`none`) and
--   under White (`W`) or L128; then the quad mesh's MeshParts (`sa-`) and EditableMesh quads (`esa-`) under
--   SurfaceAppearances in AlphaMode Transparency whose ColorMap is an EditableImage of one alpha, or (last, when
--   Context.HostedSurface holds a SurfaceAppearance whose ColorMap is the uploaded image) a copy of it.
-- "sun" (x -600): 4 x 4 cards 3 studs over a grey floor, facing up, under the sun (arm "sun"): the same kinds of
--   parts and textures, an opaque block and nothing for reference, and MeshParts made from EditableMeshes (a quad, a
--   closed box, a textured quad, one made from the mesh's data-model content and one given the mesh with ApplyMesh);
--   checkerboards and the vertex alpha quad (`va-`), also seen closer ("sun-row").
-- "lamps" (x -1200): shadowed PointLights 8 studs over 3 x 3 cards 4 studs up, the sun out (arm "lamps").
-- "all" (the default) builds every phase.
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic, Technology Future) with each view's
-- Lighting set from `Arms`, against ror's render of the same with that Lighting (--set Lighting.<name>=<value>):
-- ror render fixtures/texture-alpha.luau --context '{"Phase":"colour", ...}' --size 1644x1116 --no-service --camera ...
-- (--time 0.5 for "sun" and "all": the data-model copy yields; ROR_REE: the engine emulator's checkout;
-- ROR_ROBLOX_CONTENT: the content the client read). It returns { Views =
-- { { Tag, Eye, Target, Fov, Arm } }, Arms = { [name] = Lighting values }, Cases = { { name, kind, x, y, z } } }: where
-- to read each case (a quad's middle, a cell's middle, a shadow's middle and a lit point beside it, the middles of a
-- shadowed square and a hole, the floor under a lamp).
local ARMS = {
	flat = { Ambient = { 0.7, 0.7, 0.7 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 14.5, GeographicLatitude = 0, GlobalShadows = false, ShadowSoftness = 0.2 },
	sun = { Ambient = { 0.3, 0.3, 0.3 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 2, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 16.2, GeographicLatitude = 38, GlobalShadows = true, ShadowSoftness = 0.2 },
	lamps = { Ambient = { 0.3, 0.3, 0.3 }, OutdoorAmbient = { 0, 0, 0 }, Brightness = 0, EnvironmentDiffuseScale = 0, EnvironmentSpecularScale = 0, ClockTime = 16.2, GeographicLatitude = 38, GlobalShadows = true, ShadowSoftness = 0.2 },
}

return function(Context)
	Context = Context or {}
	local Phase = Context.Phase or "all"
	local AssetService = game:GetService("AssetService")
	local Root = Instance.new("Model")
	Root.Name = "TextureAlpha"
	local Views, Cases = {}, {}
	local function View(Tag, Eye, Target, Fov, Arm)
		table.insert(Views, { Tag = Tag, Eye = { Eye.X, Eye.Y, Eye.Z }, Target = { Target.X, Target.Y, Target.Z }, Fov = Fov, Arm = Arm })
	end
	local function Entry(Name, Kind, Point)
		table.insert(Cases, { name = Name, kind = Kind, x = Point.X, y = Point.Y, z = Point.Z })
	end

	local TAN = { 204, 153, 102 }
	local RED = Color3.fromRGB(220, 40, 40)
	local QUAD = Context.Quad or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local QUAD_ID = Context.QuadId or "rbxasset://avatar/compositing/CompositQuad.mesh"
	local TEXTURES = {
		L0 = Context.Tan0, L64 = Context.Tan64, L65 = Context.Tan65, L128 = Context.Tan128, L192 = Context.Tan192, L255 = Context.Tan255,
		W = Context.White, CK = Context.Checker, LS = Context.Smile, HS = Context.Hosted,
	}

	-- EditableImages of one tan texel (8 x 8)
	local Images = {}
	local function Editable(Alpha)
		if Images[Alpha] then
			return Images[Alpha]
		end
		local Pixels = buffer.create(8 * 8 * 4)
		for Index = 0, 63 do
			buffer.writeu8(Pixels, Index * 4, TAN[1])
			buffer.writeu8(Pixels, Index * 4 + 1, TAN[2])
			buffer.writeu8(Pixels, Index * 4 + 2, TAN[3])
			buffer.writeu8(Pixels, Index * 4 + 3, Alpha)
		end
		local Image = AssetService:CreateEditableImage({ Size = Vector2.new(8, 8) })
		Image:WritePixelsBuffer(Vector2.zero, Image.Size, Pixels)
		Images[Alpha] = Image
		return Image
	end
	-- `L128` -> a file's URI, `E128` -> an EditableImage; nil when the context lacks it
	local function Source(Key)
		if not Key then
			return nil
		end
		local Alpha = string.match(Key, "^E(%d+)$")
		if Alpha then
			return Editable(tonumber(Alpha))
		end
		return TEXTURES[Key]
	end
	local function Apply(Part, Key)
		local Texture = Source(Key)
		if typeof(Texture) == "string" then
			Part.TextureID = Texture
		elseif Texture then
			-- an EditableImage (an Object in a client)
			Part.TextureContent = Content.fromObject(Texture)
		end
	end

	-- an EditableMesh quad (4 x 4 studs, facing +Z, centred) or a closed 4 x 0.2 x 4 box, one mesh each, shared
	local Meshes = {}
	local function EditableQuad()
		if not Meshes.quad then
			local Mesh = AssetService:CreateEditableMesh()
			local Normal = Mesh:AddNormal(Vector3.new(0, 0, 1))
			local P = { Mesh:AddVertex(Vector3.new(-2, -2, 0)), Mesh:AddVertex(Vector3.new(2, -2, 0)), Mesh:AddVertex(Vector3.new(2, 2, 0)), Mesh:AddVertex(Vector3.new(-2, 2, 0)) }
			local U = { Mesh:AddUV(Vector2.new(0, 1)), Mesh:AddUV(Vector2.new(1, 1)), Mesh:AddUV(Vector2.new(1, 0)), Mesh:AddUV(Vector2.new(0, 0)) }
			for _, Face in { { 1, 2, 3 }, { 1, 3, 4 } } do
				local Id = Mesh:AddTriangle(P[Face[1]], P[Face[2]], P[Face[3]])
				Mesh:SetFaceNormals(Id, { Normal, Normal, Normal })
				Mesh:SetFaceUVs(Id, { U[Face[1]], U[Face[2]], U[Face[3]] })
			end
			Meshes.quad = Mesh
		end
		return Meshes.quad
	end
	local function EditableBox()
		if not Meshes.box then
			local Mesh = AssetService:CreateEditableMesh()
			local Corners = {}
			for Index = 0, 7 do
				Corners[Index] = Mesh:AddVertex(Vector3.new(if Index % 2 == 1 then 2 else -2, if (Index // 2) % 2 == 1 then 0.1 else -0.1, if Index // 4 == 1 then 2 else -2))
			end
			-- outward faces: -x, +x, -y, +y, -z, +z
			for _, Face in { { 0, 4, 6, 2 }, { 1, 3, 7, 5 }, { 0, 1, 5, 4 }, { 2, 6, 7, 3 }, { 0, 2, 3, 1 }, { 4, 5, 7, 6 } } do
				Mesh:AddTriangle(Corners[Face[1]], Corners[Face[2]], Corners[Face[3]])
				Mesh:AddTriangle(Corners[Face[1]], Corners[Face[3]], Corners[Face[4]])
			end
			Meshes.box = Mesh
		end
		return Meshes.box
	end

	-- a SurfaceAppearance in AlphaMode Transparency whose ColorMap is `Key`'s EditableImage (scripts cannot give one
	-- an uploaded map: for `HS` a copy of Context.HostedSurface, made beforehand)
	local function Surface(Part, Key)
		local Appearance = if Key == "HS" then Context.HostedSurface:Clone() else AssetService:CreateSurfaceAppearanceAsync({ ColorMap = Content.fromObject(Source(Key)) })
		Appearance.AlphaMode = Enum.AlphaMode.Transparency
		Appearance.Parent = Part
	end

	-- a part of one kind showing `Key`'s texture, facing +Z at `Center` (the caller turns it)
	--   mp: a MeshPart of the quad mesh; sm: a Part with the quad as a SpecialMesh FileMesh; em: an EditableMesh quad's
	--   MeshPart; vcs / vas, vcm / vam: the vertex colour or alpha quads as SpecialMeshes or MeshParts; sa / esa: the quad
	--   mesh's or the EditableMesh quad's MeshPart under a SurfaceAppearance (Surface)
	local function Make(Kind, Key, Placement)
		local Part
		if Kind == "mp" or Kind == "sa" then
			Part = AssetService:CreateMeshPartAsync(Content.fromUri(QUAD))
			Part.Size = Vector3.new(4, 4, 0.001)
			Part.CFrame = Placement * CFrame.new(-2, -2, 0)
			if Kind == "sa" then
				Surface(Part, Key)
			else
				Apply(Part, Key)
			end
		elseif Kind == "em" or Kind == "emb" or Kind == "esa" then
			Part = AssetService:CreateMeshPartAsync(Content.fromObject(if Kind == "emb" then EditableBox() else EditableQuad()))
			Part.CFrame = Placement
			if Kind == "esa" then
				Surface(Part, Key)
			else
				Apply(Part, Key)
			end
		elseif Kind == "vcm" or Kind == "vam" then
			Part = AssetService:CreateMeshPartAsync(Content.fromUri(if Kind == "vcm" then Context.VertexColours else Context.VertexAlpha))
			Part.CFrame = Placement
			Apply(Part, Key)
		else
			Part = Instance.new("Part")
			Part.Size = Vector3.new(4, 4, 0.2)
			Part.CFrame = Placement
			local Mesh = Instance.new("SpecialMesh")
			Mesh.MeshType = Enum.MeshType.FileMesh
			if Kind == "sm" then
				Mesh.MeshId = QUAD_ID
				Mesh.Scale = Vector3.new(4 / 1024, 4 / 1024, 4 / 1024)
				Mesh.Offset = Vector3.new(-2, -2, 0)
			else
				Mesh.MeshId = if Kind == "vcs" then Context.VertexColoursId else Context.VertexAlphaId
			end
			local Texture = Source(Key)
			if typeof(Texture) == "string" then
				Mesh.TextureId = Texture
			end
			Mesh.Parent = Part
		end
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		return Part
	end
	-- whether every texture a case names is there
	local function Has(Key)
		return Key == nil or string.match(Key, "^E%d+$") ~= nil or TEXTURES[Key] ~= nil
	end

	if Phase == "colour" or Phase == "all" then
		local Model = Instance.new("Model")
		Model.Name = "colour"
		Model.Parent = Root
		local List = {}
		local function Add(Name, Kind, Key, Transparency, Color)
			if Has(Key) then
				table.insert(List, { Name = Name, Kind = Kind, Key = Key, Transparency = Transparency, Color = Color or RED })
			end
		end
		local SEE = { { "t0", 0 }, { "t0.01", 0.01 }, { "t0.02", 0.02 }, { "t0.5", 0.5 } }
		for _, Key in { "L0", "L64", "L128", "L192", "L255", "E0", "E128", "HS", "LS" } do
			for _, See in SEE do
				Add(`mp-{Key}-{See[1]}`, "mp", Key, See[2])
			end
		end
		Add("mp-L128-t0-w", "mp", "L128", 0, Color3.new(1, 1, 1))
		Add("mp-HS-t0-w", "mp", "HS", 0, Color3.new(1, 1, 1))
		for _, Key in { "L0", "L128", "L255", "HS", "LS" } do
			for _, See in SEE do
				Add(`sm-{Key}-{See[1]}`, "sm", Key, See[2])
			end
		end
		for _, Key in { "L0", "L128", "HS" } do
			for _, See in { SEE[1], SEE[4] } do
				Add(`em-{Key}-{See[1]}`, "em", Key, See[2])
			end
		end
		-- vertex colours and alpha
		if Context.VertexColours and Context.VertexAlpha then
			for _, Case in {
				{ "vcs-none-t0-w", "vcs", nil, 0, Color3.new(1, 1, 1) }, { "vcs-none-t0", "vcs", nil, 0 }, { "vcs-none-t0.5-w", "vcs", nil, 0.5, Color3.new(1, 1, 1) },
				{ "vcs-W-t0", "vcs", "W", 0 }, { "vcs-L128-t0", "vcs", "L128", 0 }, { "vcs-W-t0.5", "vcs", "W", 0.5 },
				{ "vas-none-t0-w", "vas", nil, 0, Color3.new(1, 1, 1) }, { "vas-none-t0.5-w", "vas", nil, 0.5, Color3.new(1, 1, 1) },
				{ "vas-W-t0", "vas", "W", 0 }, { "vas-W-t0.5", "vas", "W", 0.5 },
				{ "vam-none-t0-w", "vam", nil, 0, Color3.new(1, 1, 1) }, { "vam-none-t0.02-w", "vam", nil, 0.02, Color3.new(1, 1, 1) }, { "vam-none-t0.5-w", "vam", nil, 0.5, Color3.new(1, 1, 1) },
				{ "vam-W-t0", "vam", "W", 0 }, { "vam-W-t0.02", "vam", "W", 0.02 }, { "vam-W-t0.5", "vam", "W", 0.5 }, { "vam-L128-t0.5", "vam", "L128", 0.5 },
				{ "vcm-none-t0-w", "vcm", nil, 0, Color3.new(1, 1, 1) }, { "vcm-W-t0", "vcm", "W", 0 }, { "vcm-L128-t0.5", "vcm", "L128", 0.5 },
			} do
				Add(Case[1], Case[2], Case[3], Case[4], Case[5])
			end
		end
		-- SurfaceAppearances in AlphaMode Transparency on the quad mesh: an EditableImage ColorMap of each alpha, and the
		-- uploaded image
		for _, Alpha in { 0, 32, 63, 64, 65, 96, 127, 128, 129, 160, 192, 224, 255 } do
			Add(`sa-E{Alpha}-t0`, "sa", `E{Alpha}`, 0)
		end
		for _, Alpha in { 0, 64, 65, 128, 192, 255 } do
			Add(`sa-E{Alpha}-t0.02`, "sa", `E{Alpha}`, 0.02)
			Add(`sa-E{Alpha}-t0.5`, "sa", `E{Alpha}`, 0.5)
		end
		-- and on EditableMesh quads
		for _, Alpha in { 0, 64, 128, 192 } do
			Add(`esa-E{Alpha}-t0`, "esa", `E{Alpha}`, 0)
		end
		Add("esa-E128-t0.5", "esa", "E128", 0.5)
		if Context.HostedSurface then
			Add("sa-HS-t0", "sa", "HS", 0)
			Add("sa-HS-t0.02", "sa", "HS", 0.02)
		end
		local COLUMNS = 10
		local Rows = math.ceil(#List / COLUMNS)
		for Index, Case in List do
			local Column = (Index - 1) % COLUMNS
			local Row = (Index - 1) // COLUMNS
			local Center = Vector3.new((Column - (COLUMNS - 1) / 2) * 5, 100 - Row * 5, 0)
			local Part = Make(Case.Kind, Case.Key, CFrame.new(Center))
			Part.Name = Case.Name
			Part.Color = Case.Color
			Part.Transparency = Case.Transparency
			Part.Parent = Model
			Entry(Case.Name, "middle", Center)
			if Case.Kind == "vcs" or Case.Kind == "vas" or Case.Kind == "vcm" or Case.Kind == "vam" then
				for Cell = 1, 4 do
					Entry(Case.Name, `cell{Cell}`, Center + Vector3.new(if Cell % 2 == 1 then -1 else 1, if Cell <= 2 then 1 else -1, 0))
				end
			end
		end
		local Wall = Instance.new("Part")
		Wall.Name = "Wall"
		Wall.Anchored = true
		Wall.Material = Enum.Material.SmoothPlastic
		Wall.Color = Color3.fromRGB(40, 150, 40)
		Wall.Size = Vector3.new(80, Rows * 5 + 20, 1)
		Wall.CFrame = CFrame.new(0, 100 - (Rows - 1) * 2.5, -3.5)
		Wall.Parent = Model
		-- six rows a view
		for First = 0, Rows - 1, 6 do
			local Last = math.min(First + 5, Rows - 1)
			local Y = 100 - (First + Last) * 2.5
			View(`colour-{First // 6 + 1}`, Vector3.new(0, Y, 100), Vector3.new(0, Y, 0), 20, "flat")
		end
	end

	-- cards facing up `Height` studs over the floor; sun shadows fall along -Lighting:GetSunDirection() at ClockTime 16.2
	-- and latitude 38 (fixed, as fixtures/leaf-shadows.luau)
	local FACE_UP = CFrame.Angles(-math.pi / 2, 0, 0)
	local Sun = Vector3.new(-0.862626, 0.439530, 0.250380)
	local function Floor(Model, Center)
		local Part = Instance.new("Part")
		Part.Name = "Floor"
		Part.Anchored = true
		Part.Material = Enum.Material.SmoothPlastic
		Part.Color = Color3.fromRGB(200, 200, 200)
		Part.Size = Vector3.new(200, 1, 200)
		Part.CFrame = CFrame.new(Center - Vector3.new(0, 0.5, 0))
		Part.Parent = Model
	end
	-- a card of one kind (as Make) at `Position`, facing up; "block" an opaque 4 x 0.2 x 4 block; "dmc" a MeshPart made
	-- from the EditableMesh quad's data-model content; "apply" a quad MeshPart given the EditableMesh quad's mesh
	local function Card(Model, Case, Position)
		local Placement = CFrame.new(Position) * FACE_UP
		local Part
		if Case.Kind == "block" then
			Part = Instance.new("Part")
			Part.Anchored = true
			Part.Material = Enum.Material.SmoothPlastic
			Part.Size = Vector3.new(4, 0.2, 4)
			Part.CFrame = CFrame.new(Position)
		elseif Case.Kind == "dmc" then
			local _, Copy = AssetService:CreateDataModelContentAsync(Content.fromObject(EditableQuad()))
			Part = AssetService:CreateMeshPartAsync(Copy)
			Part.Anchored = true
			Part.Material = Enum.Material.SmoothPlastic
			Part.CFrame = Placement
		elseif Case.Kind == "apply" then
			Part = Make("mp", nil, Placement)
			Part:ApplyMesh(AssetService:CreateMeshPartAsync(Content.fromObject(EditableQuad())))
			Part.CFrame = Placement
		elseif Case.Kind == "emb" then
			Part = Make("emb", Case.Key, CFrame.new(Position))
		else
			Part = Make(Case.Kind, Case.Key, Placement)
		end
		Part.Name = Case.Name
		Part.Color = Color3.fromRGB(60, 140, 60)
		Part.Transparency = Case.Transparency or 0
		Part.Parent = Model
		return Part
	end

	if Phase == "sun" or Phase == "all" then
		local Model = Instance.new("Model")
		Model.Name = "sun"
		Model.Parent = Root
		local Origin = Vector3.new(-600, 0, 0)
		Floor(Model, Origin)
		local List = { { Name = "block", Kind = "block" }, { Name = "none" } }
		local function Add(Name, Kind, Key, Transparency)
			if Has(Key) then
				table.insert(List, { Name = Name, Kind = Kind, Key = Key, Transparency = Transparency })
			end
		end
		for _, Kind in { "sm", "mp" } do
			Add(`{Kind}-quad`, Kind, nil, 0)
			for _, Key in { "L0", "L64", "L65", "L128", "L255", "HS" } do
				Add(`{Kind}-{Key}`, Kind, Key, 0)
			end
			for _, Pair in { { "L0", 0.3 }, { "L64", 0.3 }, { "L128", 0.3 }, { "L255", 0.3 }, { "L128", 0.5 }, { "L192", 0.5 }, { "HS", 0.5 } } do
				Add(`{Kind}-{Pair[1]}-t{Pair[2]}`, Kind, Pair[1], Pair[2])
			end
		end
		Add("em-quad", "em", nil, 0)
		Add("em-box", "emb", nil, 0)
		Add("em-L255", "em", "L255", 0)
		Add("em-dmc", "dmc", nil, 0)
		Add("em-apply", "apply", nil, 0)
		local COLUMNS = 7
		for Index, Case in List do
			local Column = (Index - 1) % COLUMNS
			local Row = (Index - 1) // COLUMNS
			local Position = Origin + Vector3.new(-49 + Column * 14, 3, -35 + Row * 10)
			if Case.Name ~= "none" then
				Card(Model, Case, Position)
			end
			local Middle = Position - Sun * (Position.Y / Sun.Y)
			Entry(Case.Name, "shadow", Middle)
			Entry(Case.Name, "lit", Middle + Vector3.new(0, 0, 4.5))
		end
		-- checkerboards in squares: the middles of a shadowed square and of a hole beside it
		local Row = math.ceil(#List / COLUMNS)
		for Index, Kind in { "sm", "mp" } do
			for Step, Transparency in { 0, 0.5 } do
				if TEXTURES.CK then
					local Name = `{Kind}-CK-t{Transparency}`
					local Position = Origin + Vector3.new(-49 + ((Index - 1) * 2 + Step - 1) * 14, 3, -35 + Row * 10)
					Card(Model, { Name = Name, Kind = Kind, Key = "CK", Transparency = Transparency }, Position)
					local Corner = Position + Vector3.new(-1.5, 0, -1.5)
					Entry(Name, "square", Corner - Sun * (3 / Sun.Y))
					Entry(Name, "hole", Corner + Vector3.new(1, 0, 0) - Sun * (3 / Sun.Y))
					Entry(Name, "lit", Position - Sun * (3 / Sun.Y) + Vector3.new(0, 0, 4.5))
				end
			end
		end
		-- the vertex alpha quad's MeshPart (cells at alpha 255, 128, 64 and 0), untextured, at Transparency 0 and 0.3: the
		-- middles of its cells' shadows
		if Context.VertexAlpha then
			for Step, Transparency in { 0, 0.3 } do
				local Name = `va-t{Transparency}`
				local Position = Origin + Vector3.new(-49 + (3 + Step) * 14, 3, -35 + Row * 10)
				Card(Model, { Name = Name, Kind = "vam", Transparency = Transparency }, Position)
				for Cell = 1, 4 do
					local Offset = Vector3.new(if Cell % 2 == 1 then -1 else 1, 0, if Cell <= 2 then -1 else 1)
					Entry(Name, `cell{Cell}`, Position + Offset - Sun * (3 / Sun.Y))
				end
				Entry(Name, "lit", Position - Sun * (3 / Sun.Y) + Vector3.new(0, 0, 4.5))
			end
		end
		View("sun", Origin + Vector3.new(0, 140, 60), Origin, 40, "sun")
		-- the last row's shadows closer
		local RowShadow = Origin + Vector3.new(-14, 0, -35 + Row * 10) - Sun * (3 / Sun.Y)
		View("sun-row", RowShadow + Vector3.new(0, 55, 12), RowShadow, 40, "sun")
	end

	if Phase == "lamps" or Phase == "all" then
		local Model = Instance.new("Model")
		Model.Name = "lamps"
		Model.Parent = Root
		local Origin = Vector3.new(-1200, 0, 0)
		Floor(Model, Origin)
		local List = {}
		local function Add(Name, Kind, Key, Transparency)
			if Has(Key) then
				table.insert(List, { Name = Name, Kind = Kind, Key = Key, Transparency = Transparency })
			end
		end
		Add("light-none", nil, nil, 0)
		Add("light-mp-quad", "mp", nil, 0)
		Add("light-sm-quad", "sm", nil, 0)
		Add("light-sm-L0", "sm", "L0", 0)
		Add("light-sm-L128", "sm", "L128", 0)
		Add("light-sm-CK", "sm", "CK", 0)
		Add("light-mp-L0", "mp", "L0", 0)
		Add("light-mp-L0-t0.3", "mp", "L0", 0.3)
		Add("light-mp-CK-t0.5", "mp", "CK", 0.5)
		Add("light-em-quad", "em", nil, 0)
		Add("light-em-dmc", "dmc", nil, 0)
		for Index, Case in List do
			local Column = (Index - 1) % 6
			local Row = (Index - 1) // 6
			local Position = Origin + Vector3.new(-40 + Column * 16, 4, -10 + Row * 20)
			local Card3 = nil
			if Case.Kind then
				Card3 = Card(Model, Case, Position)
				Card3.Size = Card3.Size * 0.75
				if Case.Kind == "sm" then
					local Mesh = Card3:FindFirstChildOfClass("SpecialMesh")
					Mesh.Scale = Vector3.new(3 / 1024, 3 / 1024, 3 / 1024)
					Mesh.Offset = Vector3.new(-1.5, -1.5, 0)
				elseif Case.Kind == "mp" then
					Card3.CFrame = CFrame.new(Position) * FACE_UP * CFrame.new(-1.5, -1.5, 0)
				end
			end
			local Carrier = Instance.new("Part")
			Carrier.Name = `{Case.Name}-lamp`
			Carrier.Anchored = true
			Carrier.Transparency = 1
			Carrier.CastShadow = false
			Carrier.Size = Vector3.new(0.2, 0.2, 0.2)
			Carrier.CFrame = CFrame.new(Position + Vector3.new(0, 8, 0))
			Carrier.Parent = Model
			local Light = Instance.new("PointLight")
			Light.Range = 16
			Light.Brightness = 1.5
			Light.Shadows = true
			Light.Parent = Carrier
			Entry(Case.Name, "light", Vector3.new(Position.X, 0, Position.Z))
			if Case.Key == "CK" then
				-- the corner square and the hole beside it (0.75-stud squares on the 3-stud card), at 1.5 times the
				-- card's offsets from the lamp
				Entry(Case.Name, "square", Vector3.new(Position.X - 1.6875, 0, Position.Z - 1.6875))
				Entry(Case.Name, "hole", Vector3.new(Position.X - 0.5625, 0, Position.Z - 1.6875))
			end
		end
		View("lamps", Origin + Vector3.new(0, 90, 50), Origin + Vector3.new(0, 0, 0), 40, "lamps")
	end

	Root.Parent = workspace
	return { Views = Views, Arms = ARMS, Cases = Cases }
end
