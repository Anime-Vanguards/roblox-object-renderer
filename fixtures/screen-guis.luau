-- ScreenGuis and ViewportFrames as a Roblox client draws them (0.747 and 0.748, graphics quality 21, 1644 x 1116 at
-- GUI scale 1; notes/render.md "ScreenGuis" and "ViewportFrames"). Context.Case builds one case into the local
-- player's PlayerGui in front of a black wall (Lighting black: the wall reads 0) and returns what it made (to
-- destroy) and its camera; without a Case it returns the cases' names, in Context.Phase's group:
--
-- "screen": ScreenInsets (None, DeviceSafeInsets, CoreUISafeInsets, TopbarSafeInsets, IgnoreGuiInset), frames outside
-- an inset canvas, two ScreenGuis of one DisplayOrder parented in either order, and a fine pattern of one-pixel lines.
-- "viewports": ViewportFrames holding Neon and Glass, a Sky, coloured light and ambient, what the world
-- draws around parts (fog, Atmosphere, bloom, local lights, Highlights, GUIs, particles, beams), a UICorner, a
-- wide and a tall frame (the field of view's axis), a UIGradient, Rotation, a vertex-alpha hole through a
-- framed one's background, a CanvasGroup, studs on balls and cylinders, and ladders of Neon, Glass and
-- Metal.
--
-- ror render fixtures/screen-guis.luau --context '{"Case":"inset-None"}' --size 1644x1116@1 --camera ...
-- (ROR_REE, ROR_RUR; Lighting from a place with black light)
local Players = game:GetService("Players")
local Lighting = game:GetService("Lighting")

local EYE = Vector3.new(0, 500, 0)
local CAMERA = { eye = { 0, 500, 0 }, target = { 0, 500, -40 }, fov = 70 }

local function Part(Name: string, Size: Vector3, At: CFrame, Color: Color3, Parent: Instance, Options: any?): Part
	local Opt = Options or {}
	local Item = Instance.new("Part")
	Item.Name = Name
	Item.Anchored = true
	Item.Shape = Opt.Shape or Enum.PartType.Block
	Item.Size = Size
	Item.CFrame = At
	Item.Color = Color
	Item.Material = Opt.Material or Enum.Material.SmoothPlastic
	Item.Transparency = Opt.Transparency or 0
	if not Opt.Studs then
		Item.TopSurface = Enum.SurfaceType.Smooth
		Item.BottomSurface = Enum.SurfaceType.Smooth
	else
		Item.TopSurface = Enum.SurfaceType.Studs
		Item.BottomSurface = Enum.SurfaceType.Inlet
	end
	Item.Parent = Parent
	return Item
end

local function Frame(Parent: Instance, Name: string, Color: Color3, Position: UDim2, Size: UDim2, Anchor: Vector2?, Transparency: number?): Frame
	local Item = Instance.new("Frame")
	Item.Name = Name
	Item.BackgroundColor3 = Color
	Item.BackgroundTransparency = Transparency or 0
	Item.BorderSizePixel = 0
	Item.Position = Position
	Item.Size = Size
	Item.AnchorPoint = Anchor or Vector2.zero
	Item.Parent = Parent
	return Item
end

local function Screen(Name: string, Props: any?): ScreenGui
	local Gui = Instance.new("ScreenGui")
	Gui.Name = Name
	Gui.ResetOnSpawn = false
	for Key, Value in Props or {} do
		(Gui :: any)[Key] = Value
	end
	return Gui
end

-- a ViewportFrame at (x, y) of w x h GUI pixels with its own camera looking at the origin from Eye
local function Viewport(Parent: Instance, Name: string, Rect: { number }, Eye: Vector3, Fov: number?, Props: any?): (ViewportFrame, Instance)
	local Item = Instance.new("ViewportFrame")
	Item.Name = Name
	Item.Position = UDim2.fromOffset(Rect[1], Rect[2])
	Item.Size = UDim2.fromOffset(Rect[3], Rect[4])
	Item.BackgroundColor3 = Color3.new(0, 0, 0)
	Item.BorderSizePixel = 0
	Item.Ambient = Color3.new(0, 0, 0)
	Item.LightColor = Color3.new(1, 1, 1)
	Item.LightDirection = Vector3.new(-1, -1, -1)
	for Key, Value in Props or {} do
		(Item :: any)[Key] = Value
	end
	local Camera = Instance.new("Camera")
	Camera.FieldOfView = Fov or 40
	Camera.CFrame = CFrame.lookAt(Eye, Vector3.zero)
	Camera.Parent = Item
	Item.CurrentCamera = Camera
	Item.Parent = Parent
	local World = Instance.new("WorldModel")
	World.Parent = Item
	return Item, World
end

local function Insets(Mode: string?, Ignore: boolean?)
	local Gui = Screen("Insets", if Mode then { ScreenInsets = (Enum.ScreenInsets :: any)[Mode] } else { IgnoreGuiInset = Ignore })
	Frame(Gui, "Extent", Color3.new(0.5, 0.5, 0.5), UDim2.new(), UDim2.fromScale(1, 1), nil, 0.5)
	Frame(Gui, "TopLeft", Color3.new(1, 1, 1), UDim2.new(), UDim2.fromOffset(60, 60))
	Frame(Gui, "BottomRight", Color3.new(1, 0, 0), UDim2.fromScale(1, 1), UDim2.fromOffset(60, 60), Vector2.one)
	return { Gui }
end

-- two ScreenGuis of one DisplayOrder, overlapping
local function Tie(Order: string)
	local A, B = Screen("TieA", { DisplayOrder = 5 }), Screen("TieB", { DisplayOrder = 5 })
	Frame(A, "Red", Color3.new(1, 0, 0), UDim2.fromOffset(200, 200), UDim2.fromOffset(300, 200))
	Frame(B, "Green", Color3.new(0, 1, 0), UDim2.fromOffset(350, 280), UDim2.fromOffset(300, 200))
	return { A, B }, Order
end

-- a mesh of quads whose every vertex has alpha 0 round an opening `Opening` studs across (half sizes), out to
-- `Outer`, facing +Z: in a ViewportFrame's world it hides what lies behind it and is a hole through the picture
local function Mask(Opening: Vector2, Outer: number): MeshPart
	local AssetService = game:GetService("AssetService")
	local Mesh = AssetService:CreateEditableMesh({})
	local Clear = Mesh:AddColor(Color3.new(0, 0, 0), 0)
	local function Quad(A: Vector2, B: Vector2)
		local Corners = {
			Mesh:AddVertex(Vector3.new(A.X, A.Y, 0)),
			Mesh:AddVertex(Vector3.new(B.X, A.Y, 0)),
			Mesh:AddVertex(Vector3.new(B.X, B.Y, 0)),
			Mesh:AddVertex(Vector3.new(A.X, B.Y, 0)),
		}
		for _, Face in { Mesh:AddTriangle(Corners[1], Corners[2], Corners[3]), Mesh:AddTriangle(Corners[1], Corners[3], Corners[4]) } do
			Mesh:SetFaceColors(Face, { Clear, Clear, Clear })
		end
	end
	Quad(Vector2.new(-Outer, Opening.Y), Vector2.new(Outer, Outer))
	Quad(Vector2.new(-Outer, -Outer), Vector2.new(Outer, -Opening.Y))
	Quad(Vector2.new(-Outer, -Opening.Y), Vector2.new(-Opening.X, Opening.Y))
	Quad(Vector2.new(Opening.X, -Opening.Y), Vector2.new(Outer, Opening.Y))
	local Item = AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
	Item.Name = "Mask"
	Item.Anchored = true
	Item.DoubleSided = true
	Item.Color = Color3.new(0, 0, 0)
	Item.Material = Enum.Material.SmoothPlastic
	return Item
end

local Cases = {}
Cases.screen = {
	["inset-None"] = function()
		return Insets("None")
	end,
	["inset-DeviceSafeInsets"] = function()
		return Insets("DeviceSafeInsets")
	end,
	["inset-CoreUISafeInsets"] = function()
		return Insets("CoreUISafeInsets")
	end,
	["inset-TopbarSafeInsets"] = function()
		return Insets("TopbarSafeInsets")
	end,
	["inset-ignore"] = function()
		return Insets(nil, true)
	end,
	-- frames outside a TopbarSafeInsets canvas: left of it (over Roblox's buttons), below it, through it, past
	-- its right edge and above it; a client draws them all, the screen's edges alone clipping
	["topbar-overflow"] = function()
		local Gui = Screen("Overflow", { ScreenInsets = Enum.ScreenInsets.TopbarSafeInsets })
		Frame(Gui, "Left", Color3.new(1, 0, 0), UDim2.fromOffset(-150, 10), UDim2.fromOffset(100, 30))
		Frame(Gui, "Below", Color3.new(0, 1, 0), UDim2.fromOffset(100, 70), UDim2.fromOffset(100, 60))
		Frame(Gui, "Tall", Color3.new(0, 0, 1), UDim2.fromOffset(300, 0), UDim2.fromOffset(50, 300))
		Frame(Gui, "Right", Color3.new(1, 1, 0), UDim2.new(1, 20, 0, 10), UDim2.fromOffset(100, 30), Vector2.new(1, 0))
		Frame(Gui, "Past", Color3.new(1, 0, 1), UDim2.new(1, -40, 0, 10), UDim2.fromOffset(100, 30))
		Frame(Gui, "Above", Color3.new(0, 1, 1), UDim2.fromOffset(500, -20), UDim2.fromOffset(60, 40))
		return { Gui }
	end,
	-- frames outside a CoreUISafeInsets canvas: above it (into the top bar's band), left of it, past its bottom
	["inset-overflow"] = function()
		local Gui = Screen("Overflow", { ScreenInsets = Enum.ScreenInsets.CoreUISafeInsets })
		Frame(Gui, "Up", Color3.new(1, 0, 0), UDim2.fromOffset(300, -40), UDim2.fromOffset(100, 80))
		Frame(Gui, "Left", Color3.new(0, 1, 0), UDim2.fromOffset(-50, 100), UDim2.fromOffset(100, 40))
		Frame(Gui, "Under", Color3.new(0, 0, 1), UDim2.new(0, 500, 1, -20), UDim2.fromOffset(100, 80))
		return { Gui }
	end,
	["tie-a-then-b"] = function()
		return Tie("ab")
	end,
	["tie-b-then-a"] = function()
		return Tie("ba")
	end,
	["fine"] = function()
		local Gui = Screen("Fine", { IgnoreGuiInset = true })
		for X = 0, 240, 6 do
			Frame(Gui, "V" .. X, Color3.new(1, 1, 1), UDim2.fromOffset(100 + X, 100), UDim2.fromOffset(1, 240))
		end
		for Y = 0, 240, 6 do
			Frame(Gui, "H" .. Y, Color3.new(1, 1, 1), UDim2.fromOffset(400, 100 + Y), UDim2.fromOffset(240, 1))
		end
		Frame(Gui, "Right", Color3.new(0, 1, 0), UDim2.new(1, -10, 0.5, 0), UDim2.fromOffset(10, 10))
		Frame(Gui, "Bottom", Color3.new(0, 0, 1), UDim2.new(0.5, 0, 1, -10), UDim2.fromOffset(10, 10))
		local Text = Instance.new("TextLabel")
		Text.BackgroundTransparency = 1
		Text.Text = "Fine text 0123"
		Text.TextColor3 = Color3.new(1, 1, 1)
		Text.Font = Enum.Font.SourceSans
		Text.TextSize = 24
		Text.Position = UDim2.fromOffset(700, 100)
		Text.Size = UDim2.fromOffset(300, 40)
		Text.Parent = Gui
		return { Gui }
	end,
}

local EYE3 = Vector3.new(7, 5, 9)
local BALL = { Shape = Enum.PartType.Ball }
Cases.viewports = {
	-- Neon: colours and greys at the frame's white light and no ambient
	["vp-neon"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Colors = { Color3.new(1, 1, 1), Color3.new(1, 0, 0), Color3.new(1, 0.5, 0), Color3.new(0.2, 0.4, 1), Color3.new(0.5, 0.5, 0.5), Color3.new(0.25, 0.25, 0.25) }
		for Index, Color in Colors do
			local _, World = Viewport(Gui, "Neon" .. Index, { 40 + ((Index - 1) % 3) * 520, 60 + (Index > 3 and 520 or 0), 480, 480 }, Vector3.new(0, 0, 8), 40)
			Part("Neon", Vector3.new(4, 4, 1), CFrame.new(), Color, World, { Material = Enum.Material.Neon })
		end
		return { Gui }
	end,
	-- Glass: at Transparency 0 and 0.5 before a red block, beside SmoothPlastic
	["vp-glass"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local _, World = Viewport(Gui, "Glass", { 40, 60, 1000, 600 }, Vector3.new(0, 2, 12), 50, { Ambient = Color3.fromRGB(80, 80, 80) })
		Part("Back", Vector3.new(12, 3, 1), CFrame.new(0, 0, -3), Color3.new(1, 0, 0), World)
		Part("Glass0", Vector3.new(3, 3, 1), CFrame.new(-4, 0, 0), Color3.fromRGB(150, 200, 255), World, { Material = Enum.Material.Glass })
		Part("Glass5", Vector3.new(3, 3, 1), CFrame.new(0, 0, 0), Color3.fromRGB(150, 200, 255), World, { Material = Enum.Material.Glass, Transparency = 0.5 })
		Part("Plastic5", Vector3.new(3, 3, 1), CFrame.new(4, 0, 0), Color3.fromRGB(150, 200, 255), World, { Transparency = 0.5 })
		return { Gui }
	end,
	-- a Sky child: its images as the frame's sky light and reflections
	["vp-sky"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Item, World = Viewport(Gui, "Sky", { 40, 60, 1000, 600 }, Vector3.new(0, 1, 12), 50)
		local Sky = Instance.new("Sky")
		local Dark, White = "rbxasset://textures/AvatarEditorImages/DarkPixel.png", "rbxasset://textures/ui/InGameMenu/WhiteSquare.png"
		Sky.SkyboxBk, Sky.SkyboxDn, Sky.SkyboxFt, Sky.SkyboxLf, Sky.SkyboxRt, Sky.SkyboxUp = Dark, Dark, Dark, Dark, Dark, White
		Sky.CelestialBodiesShown = false
		Sky.Parent = Item
		Part("Plastic", Vector3.new(3, 3, 3), CFrame.new(-4, 0, 0), Color3.new(1, 1, 1), World, BALL)
		Part("Metal", Vector3.new(3, 3, 3), CFrame.new(0, 0, 0), Color3.new(1, 1, 1), World, { Shape = Enum.PartType.Ball, Material = Enum.Material.Metal })
		Part("Block", Vector3.new(3, 3, 3), CFrame.new(4, 0, 0), Color3.new(1, 1, 1), World)
		return { Gui }
	end,
	-- light and ambient other than white: a match's cards (Ambient 210, LightColor 175 gray), and colours
	["vp-light"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Lights = {
			{ Color3.fromRGB(210, 210, 210), Color3.fromRGB(175, 175, 175) },
			{ Color3.fromRGB(0, 0, 0), Color3.fromRGB(255, 60, 60) },
			{ Color3.fromRGB(40, 60, 200), Color3.fromRGB(0, 0, 0) },
			{ Color3.fromRGB(100, 100, 100), Color3.fromRGB(128, 255, 128) },
		}
		for Index, Pair in Lights do
			local _, World = Viewport(Gui, "Light" .. Index, { 40 + ((Index - 1) % 2) * 800, 60 + (Index > 2 and 520 or 0), 760, 480 }, EYE3, 40, { Ambient = Pair[1], LightColor = Pair[2] })
			Part("Ball", Vector3.new(3, 3, 3), CFrame.new(-2.2, 0, 0), Color3.new(1, 1, 1), World, BALL)
			Part("Block", Vector3.new(3, 3, 3), CFrame.new(2.2, 0, 0), Color3.fromRGB(200, 150, 100), World)
		end
		return { Gui }
	end,
	-- what the world draws around its parts, given to the frame's world: fog and an Atmosphere and bloom in
	-- Lighting, a PointLight, a Highlight, a SurfaceGui, particles and a beam
	["vp-world"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		Lighting.FogStart = 0
		Lighting.FogEnd = 30
		Lighting.FogColor = Color3.new(1, 0, 0)
		local Atmosphere = Instance.new("Atmosphere")
		Atmosphere.Density = 0.6
		Atmosphere.Color = Color3.new(0, 1, 0)
		Atmosphere.Parent = Lighting
		local Bloom = Instance.new("BloomEffect")
		Bloom.Intensity = 1
		Bloom.Threshold = 0.5
		Bloom.Size = 40
		Bloom.Parent = Lighting
		local _, World = Viewport(Gui, "World", { 40, 60, 1200, 700 }, Vector3.new(0, 3, 24), 50, { Ambient = Color3.fromRGB(60, 60, 60) })
		Part("Floor", Vector3.new(40, 1, 60), CFrame.new(0, -2, -10), Color3.fromRGB(180, 180, 180), World)
		Part("FarBlock", Vector3.new(4, 4, 4), CFrame.new(-8, 0, -30), Color3.new(1, 1, 1), World)
		Part("NeonBar", Vector3.new(4, 0.5, 0.5), CFrame.new(6, 2, 0), Color3.new(1, 1, 1), World, { Material = Enum.Material.Neon })
		local Lit = Part("Lit", Vector3.new(3, 3, 3), CFrame.new(0, 0, 0), Color3.new(1, 1, 1), World)
		local Light = Instance.new("PointLight")
		Light.Range = 12
		Light.Brightness = 5
		Light.Color = Color3.new(0, 0, 1)
		Light.Parent = Lit
		local Marked = Part("Marked", Vector3.new(3, 3, 3), CFrame.new(-6, 0, 2), Color3.new(1, 1, 1), World)
		local Highlight = Instance.new("Highlight")
		Highlight.FillColor = Color3.new(1, 0, 1)
		Highlight.Parent = Marked
		local Board = Part("Board", Vector3.new(4, 3, 0.2), CFrame.new(6, -0.5, 3), Color3.fromRGB(90, 90, 90), World)
		local Surface = Instance.new("SurfaceGui")
		Surface.Face = Enum.NormalId.Back
		Surface.Parent = Board
		Frame(Surface, "Yellow", Color3.new(1, 1, 0), UDim2.new(), UDim2.fromScale(1, 1))
		local Emitter = Instance.new("ParticleEmitter")
		Emitter.Rate = 200
		Emitter.Speed = NumberRange.new(2)
		Emitter.Color = ColorSequence.new(Color3.new(0, 1, 1))
		Emitter.Parent = Marked
		local A0, A1 = Instance.new("Attachment"), Instance.new("Attachment")
		A0.Position, A1.Position = Vector3.new(0, 3, 0), Vector3.new(0, 3, 0)
		A0.Parent = Lit
		A1.Parent = Marked
		local Beam = Instance.new("Beam")
		Beam.Attachment0, Beam.Attachment1 = A0, A1
		Beam.Width0, Beam.Width1 = 1, 1
		Beam.Color = ColorSequence.new(Color3.new(1, 0.5, 0))
		Beam.LightEmission = 1
		Beam.Parent = Lit
		return { Gui, Atmosphere, Bloom }
	end,
	-- a UICorner, a UIGradient, and the field of view's axis in a wide and a tall frame
	["vp-shape"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Round, World1 = Viewport(Gui, "Round", { 40, 60, 400, 400 }, EYE3, 40, { Ambient = Color3.fromRGB(120, 120, 120) })
		local Corner = Instance.new("UICorner")
		Corner.CornerRadius = UDim.new(0, 80)
		Corner.Parent = Round
		Part("Big", Vector3.new(8, 8, 8), CFrame.new(), Color3.new(1, 1, 1), World1)
		local Faded, World2 = Viewport(Gui, "Faded", { 480, 60, 400, 400 }, EYE3, 40, { Ambient = Color3.fromRGB(120, 120, 120) })
		local Gradient = Instance.new("UIGradient")
		Gradient.Color = ColorSequence.new(Color3.new(1, 0, 0), Color3.new(1, 1, 1))
		Gradient.Transparency = NumberSequence.new(0, 1)
		Gradient.Parent = Faded
		Part("Big", Vector3.new(8, 8, 8), CFrame.new(), Color3.new(1, 1, 1), World2)
		for Index, Rect in { { 920, 60, 680, 200 }, { 920, 300, 200, 760 } } do
			local _, World = Viewport(Gui, "Axis" .. Index, Rect, Vector3.new(0, 0, 20), 30, { Ambient = Color3.fromRGB(120, 120, 120) })
			for X = -6, 6, 2 do
				for Y = -6, 6, 2 do
					Part("Dot", Vector3.new(0.6, 0.6, 0.6), CFrame.new(X, Y, 0), Color3.new(1, 1, 1), World, BALL)
				end
			end
		end
		return { Gui }
	end,
	-- VPF-1: Rotation of a frame with a background, and a CanvasGroup holding a frame over a ViewportFrame
	["vp-rotation"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		for Index, Angle in { 0, 20 } do
			local Item, World = Viewport(Gui, "Turned" .. Index, { 60 + (Index - 1) * 560, 100, 440, 320 }, EYE3, 40, { Ambient = Color3.fromRGB(120, 120, 120), BackgroundColor3 = Color3.fromRGB(40, 60, 120), Rotation = Angle })
			Part("Bar", Vector3.new(10, 1, 1), CFrame.new(), Color3.new(1, 0.8, 0.2), World)
			Part("Ball", Vector3.new(3, 3, 3), CFrame.new(0, 2, 0), Color3.new(1, 1, 1), World, BALL)
			local _ = Item
		end
		local Group = Instance.new("CanvasGroup")
		Group.Position = UDim2.fromOffset(1180, 100)
		Group.Size = UDim2.fromOffset(400, 400)
		Group.BackgroundTransparency = 1
		Group.Parent = Gui
		local _, World = Viewport(Group, "Grouped", { 0, 0, 300, 300 }, EYE3, 40, { Ambient = Color3.fromRGB(120, 120, 120), BackgroundColor3 = Color3.fromRGB(120, 40, 40), ZIndex = 2 })
		Part("Big", Vector3.new(6, 6, 6), CFrame.new(), Color3.new(1, 1, 1), World)
		Frame(Group, "Over", Color3.new(0, 1, 0), UDim2.fromOffset(150, 150), UDim2.fromOffset(200, 200)).ZIndex = 1
		return { Gui }
	end,
	-- VPF-1b: a vertex-alpha mask with an opening in front of a red block, in frames whose background is opaque,
	-- half transparent and transparent, over a green frame: what the mask's hole shows
	["vp-hole"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		Frame(Gui, "Under", Color3.new(0, 1, 0), UDim2.fromOffset(40, 160), UDim2.fromOffset(1560, 400))
		for Index, Transparency in { 0, 0.5, 1 } do
			local _, World = Viewport(Gui, "Hole" .. Index, { 60 + (Index - 1) * 520, 200, 480, 320 }, Vector3.new(0, 0, 12), 40, { Ambient = Color3.fromRGB(120, 120, 120), BackgroundColor3 = Color3.fromRGB(40, 60, 120), BackgroundTransparency = Transparency })
			Part("Back", Vector3.new(10, 6, 1), CFrame.new(0, 0, -2), Color3.new(1, 0, 0), World)
			local Cover = Mask(Vector2.new(1.5, 1), 3)
			Cover.CFrame = CFrame.new()
			Cover.Parent = World
			Part("Ball", Vector3.new(1.5, 1.5, 1.5), CFrame.new(-3.5, 1.5, 1), Color3.new(1, 1, 1), World, BALL)
		end
		return { Gui }
	end,
	-- VPF-2b: Neon greys and colours in a frame, lit by white light (top) and by none (bottom)
	["vp-neon-ladder"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Greys = { 0.2, 0.3, 0.35, 0.4, 0.45, 0.5, 0.55, 0.6, 0.65, 0.7, 0.8, 0.9 }
		local Colors = { Color3.new(1, 0.5, 0), Color3.new(0.5, 1, 0.25), Color3.new(0.25, 0.5, 1), Color3.new(1, 0.75, 0.5), Color3.new(0.6, 0.3, 0.15), Color3.new(0.75, 0.75, 0.75) }
		for Index, Light in { Color3.new(1, 1, 1), Color3.new(0, 0, 0) } do
			local _, World = Viewport(Gui, "Ladder" .. Index, { 40, 60 + (Index - 1) * 400, 1560, 360 }, Vector3.new(0, 0, 40), 12, { LightColor = Light })
			for Column, Grey in Greys do
				Part("Grey", Vector3.new(1.6, 1.6, 0.2), CFrame.new((Column - 6.5) * 2, 1.2, 0), Color3.new(Grey, Grey, Grey), World, { Material = Enum.Material.Neon })
			end
			for Column, Color in Colors do
				Part("Color", Vector3.new(1.6, 1.6, 0.2), CFrame.new((Column - 6.5) * 2, -1.2, 0), Color, World, { Material = Enum.Material.Neon })
			end
		end
		return { Gui }
	end,
	-- VPF-2b: Glass and SmoothPlastic at Transparency 0, 0.25, 0.5 and 0.75 before a red, a white and a black block
	["vp-glass-ladder"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		for Index, Back in { Color3.new(1, 0, 0), Color3.new(1, 1, 1), Color3.new(0, 0, 0) } do
			local _, World = Viewport(Gui, "Glass" .. Index, { 40 + (Index - 1) * 520, 60, 500, 500 }, Vector3.new(0, 0, 20), 30, { Ambient = Color3.fromRGB(80, 80, 80) })
			Part("Back", Vector3.new(12, 8, 1), CFrame.new(0, 0, -3), Back, World)
			for Column, Transparency in { 0, 0.25, 0.5, 0.75 } do
				for Row, Material in { Enum.Material.Glass, Enum.Material.SmoothPlastic } do
					Part("Tile", Vector3.new(1.8, 1.8, 0.4), CFrame.new((Column - 2.5) * 2.4, Row == 1 and 1.4 or -1.4, 0), Color3.fromRGB(150, 200, 255), World, { Material = Material, Transparency = Transparency })
				end
			end
		end
		return { Gui }
	end,
	-- VPF-2b: Metal, Plastic, SmoothPlastic and DiamondPlate balls (top) and blocks (bottom) under a Sky child white
	-- above (left) and without a Sky (right)
	["vp-metal"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Dark, White = "rbxasset://textures/AvatarEditorImages/DarkPixel.png", "rbxasset://textures/ui/InGameMenu/WhiteSquare.png"
		for Index = 1, 2 do
			local Item, World = Viewport(Gui, "Metal" .. Index, { 40 + (Index - 1) * 780, 60, 760, 600 }, Vector3.new(0, 1, 14), 45)
			if Index == 1 then
				local Sky = Instance.new("Sky")
				Sky.SkyboxBk, Sky.SkyboxDn, Sky.SkyboxFt, Sky.SkyboxLf, Sky.SkyboxRt, Sky.SkyboxUp = Dark, Dark, Dark, Dark, Dark, White
				Sky.CelestialBodiesShown = false
				Sky.Parent = Item
			end
			for Column, Material in { Enum.Material.Plastic, Enum.Material.Metal, Enum.Material.SmoothPlastic, Enum.Material.DiamondPlate } do
				Part("Ball", Vector3.new(2.4, 2.4, 2.4), CFrame.new((Column - 2.5) * 3, 1.8, 0), Color3.new(1, 1, 1), World, { Shape = Enum.PartType.Ball, Material = Material })
				Part("Block", Vector3.new(2.4, 2.4, 2.4), CFrame.new((Column - 2.5) * 3, -1.4, 0), Color3.new(1, 1, 1), World, { Material = Material })
			end
		end
		return { Gui }
	end,
	-- VPF-2b: white and pale blue Glass at Transparency 0.1 to 0.9 before a black and a white block
	["vp-glass-fine"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		for Index, Back in { Color3.new(0, 0, 0), Color3.new(1, 1, 1) } do
			local _, World = Viewport(Gui, "Glass" .. Index, { 40, 60 + (Index - 1) * 500, 1560, 460 }, Vector3.new(0, 0, 30), 14, { Ambient = Color3.fromRGB(80, 80, 80) })
			Part("Back", Vector3.new(30, 8, 1), CFrame.new(0, 0, -3), Back, World)
			for Column = 1, 9 do
				for Row, Color in { Color3.new(1, 1, 1), Color3.fromRGB(150, 200, 255) } do
					Part("Tile", Vector3.new(1.8, 1.8, 0.4), CFrame.new((Column - 5) * 2.4, Row == 1 and 1.2 or -1.2, 0), Color, World, { Material = Enum.Material.Glass, Transparency = Column / 10 })
				end
			end
		end
		return { Gui }
	end,
	-- VPF-2b: Metal, DiamondPlate and Plastic blocks and a Metal ball under a Sky child white or dark all round, or
	-- none, with the frame's light on (top) and off (bottom), Ambient grey
	["vp-metal-sky"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Dark, White = "rbxasset://textures/AvatarEditorImages/DarkPixel.png", "rbxasset://textures/ui/InGameMenu/WhiteSquare.png"
		for Row, Light in { Color3.new(1, 1, 1), Color3.new(0, 0, 0) } do
			for Column, Image in { White, Dark, false } do
				local Item, World = Viewport(Gui, "Metal" .. Row .. Column, { 40 + (Column - 1) * 520, 60 + (Row - 1) * 500, 500, 460 }, Vector3.new(0, 0, 16), 30, { LightColor = Light, Ambient = Color3.fromRGB(100, 100, 100) })
				if Image then
					local Sky = Instance.new("Sky")
					Sky.SkyboxBk, Sky.SkyboxDn, Sky.SkyboxFt, Sky.SkyboxLf, Sky.SkyboxRt, Sky.SkyboxUp = Image, Image, Image, Image, Image, Image
					Sky.CelestialBodiesShown = false
					Sky.Parent = Item
				end
				for Index, Material in { Enum.Material.Metal, Enum.Material.DiamondPlate, Enum.Material.Plastic } do
					Part("Block", Vector3.new(2.4, 2.4, 2.4), CFrame.new((Index - 2) * 3, 1.6, 0), Color3.new(1, 1, 1), World, { Material = Material })
				end
				Part("Ball", Vector3.new(2.4, 2.4, 2.4), CFrame.new(-3, -1.6, 0), Color3.new(1, 1, 1), World, { Shape = Enum.PartType.Ball, Material = Enum.Material.Metal })
				Part("Grey", Vector3.new(2.4, 2.4, 2.4), CFrame.new(0, -1.6, 0), Color3.new(0.5, 0.5, 0.5), World, { Material = Enum.Material.Metal })
			end
		end
		return { Gui }
	end,
	-- VPF-3: studs on blocks, balls and cylinders in a frame (rur's viewport-models), and the same in the world
	["vp-studs"] = function()
		local Gui = Screen("Vp", { IgnoreGuiInset = true })
		local Root = Instance.new("Model")
		Root.Name = "Studs"
		for Index = 1, 2 do
			local _, World = Viewport(Gui, "Studs" .. Index, { 40 + (Index - 1) * 540, 60, 500, 580 }, Vector3.new(6, 4, 9) * (Index == 1 and 1 or 0.6), 35, { Ambient = Color3.fromRGB(100, 100, 100), BackgroundColor3 = Color3.fromRGB(30, 36, 50) })
			for PartIndex, Shape in { Enum.PartType.Block, Enum.PartType.Ball, Enum.PartType.Cylinder } do
				local Colors = { Color3.fromRGB(245, 90, 70), Color3.fromRGB(80, 220, 130), Color3.fromRGB(90, 145, 250) }
				Part("Piece", Vector3.new(2, 2, 2), CFrame.new((PartIndex - 2) * 1.8, 0, 0), Colors[PartIndex], World, { Shape = Shape, Studs = true, Material = Enum.Material.Plastic })
			end
		end
		return { Gui }
	end,
}

return function(Context)
	local Phase = Context and Context.Phase or "screen"
	local Case = Context and Context.Case
	if not Case then
		local Names = {}
		for Name in Cases[Phase] do
			table.insert(Names, Name)
		end
		table.sort(Names)
		return { cases = Names, camera = CAMERA }
	end
	-- the black wall behind every case
	local Wall = Part("Wall", Vector3.new(400, 400, 1), CFrame.new(EYE + Vector3.new(0, 0, -40)), Color3.new(0, 0, 0), workspace)
	local Group = Cases.screen[Case] or Cases.viewports[Case]
	local Made, Order = Group()
	local PlayerGui = Players.LocalPlayer:WaitForChild("PlayerGui")
	if Order == "ba" then
		Made[2].Parent = PlayerGui
		Made[1].Parent = PlayerGui
	else
		for _, Item in Made do
			if Item:IsA("ScreenGui") then
				Item.Parent = PlayerGui
			end
		end
	end
	table.insert(Made, Wall)
	return { made = Made, camera = CAMERA }
end
