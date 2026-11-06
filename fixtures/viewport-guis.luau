-- BillboardGuis and a SurfaceGui, each holding a ViewportFrame with coloured parts and its own camera:
-- a backdrop under the frame, a label over it, a UIGradient fading the picture from top to bottom, a bar
-- longer than the frame (clipped to it), ImageColor3 and ImageTransparency, and an AlwaysOnTop billboard
-- behind the slab.
-- ror render fixtures/viewport-guis.luau --camera '{"mode":"lookAt","eye":[0,5,-13],"target":[0,5,0],"fov":60}'
-- (notes/render.md "ViewportFrames"; ROR_REE: the engine emulator's checkout)
local function Frame(Parent: Instance, Name: string, Color: Color3, Options)
	local Backdrop = Instance.new("Frame")
	Backdrop.Name = "Backdrop"
	Backdrop.Size = UDim2.fromScale(1, 1)
	Backdrop.BackgroundColor3 = Color3.fromRGB(30, 34, 60)
	Backdrop.ZIndex = 1
	Backdrop.Parent = Parent

	local Viewport = Instance.new("ViewportFrame")
	Viewport.Name = Name
	Viewport.AnchorPoint = Vector2.new(0.5, 0.5)
	Viewport.Position = UDim2.fromScale(0.5, 0.55)
	Viewport.Size = UDim2.fromScale(0.9, 0.8)
	Viewport.BackgroundTransparency = 1
	Viewport.Ambient = Options.Ambient
	Viewport.LightColor = Options.LightColor
	Viewport.LightDirection = Vector3.new(-1, -1, -1)
	Viewport.ImageColor3 = Options.Tint or Color3.new(1, 1, 1)
	Viewport.ImageTransparency = Options.Transparency or 0
	Viewport.ZIndex = 2

	if Options.Fade then
		local Fade = Instance.new("UIGradient")
		Fade.Rotation = 90
		Fade.Transparency = NumberSequence.new({ NumberSequenceKeypoint.new(0, 0), NumberSequenceKeypoint.new(1, 1) })
		Fade.Parent = Viewport
	end

	local World = Instance.new("WorldModel")
	World.Parent = Viewport
	local Cube = Instance.new("Part")
	Cube.Name = "Cube"
	Cube.Size = Vector3.new(2, 2, 2)
	Cube.Color = Color
	Cube.CFrame = CFrame.new(-1.4, 0, 0) * CFrame.Angles(0.5, 0.7, 0)
	Cube.Parent = World
	local Ball = Instance.new("Part")
	Ball.Name = "Ball"
	Ball.Shape = Enum.PartType.Ball
	Ball.Size = Vector3.new(2, 2, 2)
	Ball.Color = Color3.fromRGB(60, 200, 90)
	Ball.CFrame = CFrame.new(1.6, 0, 0)
	Ball.Parent = World
	-- longer than the frame shows: cut at its sides
	local Bar = Instance.new("Part")
	Bar.Name = "Bar"
	Bar.Size = Vector3.new(30, 0.4, 0.4)
	Bar.Color = Color3.fromRGB(240, 240, 240)
	Bar.CFrame = CFrame.new(0, -1.6, 0)
	Bar.Parent = World

	local Camera = Instance.new("Camera")
	Camera.FieldOfView = 40
	Camera.CFrame = CFrame.lookAt(Vector3.new(0, 1.5, 8), Vector3.new(0, 0, 0))
	Camera.Parent = Viewport
	Viewport.CurrentCamera = Camera
	Viewport.Parent = Parent

	local Label = Instance.new("TextLabel")
	Label.Name = "Over"
	Label.AnchorPoint = Vector2.new(0.5, 0)
	Label.Position = UDim2.fromScale(0.5, 0.3)
	Label.Size = UDim2.fromScale(0.5, 0.14)
	Label.BackgroundColor3 = Color3.fromRGB(250, 210, 40)
	Label.Text = Options.Text
	Label.TextScaled = true
	Label.ZIndex = 3
	Label.Parent = Parent
end

local function Holder(Name: string, Position: Vector3): Part
	local Part = Instance.new("Part")
	Part.Name = Name
	Part.Anchored = true
	Part.Transparency = 1
	Part.Size = Vector3.new(1, 1, 1)
	Part.CFrame = CFrame.new(Position)
	Part.Parent = workspace
	return Part
end

return function()
	local Floor = Instance.new("Part")
	Floor.Name = "Floor"
	Floor.Anchored = true
	Floor.Size = Vector3.new(40, 1, 20)
	Floor.CFrame = CFrame.new(0, -0.5, 0)
	Floor.Color = Color3.fromRGB(120, 124, 130)
	Floor.Parent = workspace

	-- a billboard on an invisible part, as an emote hangs a portrait on the HumanoidRootPart:
	-- lit by the frame's light, its picture fading down its UIGradient
	local Lit = Instance.new("BillboardGui")
	Lit.Name = "Lit"
	Lit.Size = UDim2.fromScale(8, 6)
	Lit.LightInfluence = 0
	Lit.Adornee = Holder("LitHolder", Vector3.new(-5, 4, 0))
	Lit.Parent = Lit.Adornee
	Frame(Lit, "Portrait", Color3.fromRGB(220, 50, 50), { Ambient = Color3.fromRGB(90, 90, 90), LightColor = Color3.new(1, 1, 1), Fade = true, Text = "FADE" })

	-- a SurfaceGui on a slab's front face: flat ambient light, the picture tinted yellow
	local Slab = Instance.new("Part")
	Slab.Name = "Slab"
	Slab.Anchored = true
	Slab.Size = Vector3.new(8, 6, 0.5)
	Slab.CFrame = CFrame.new(5, 4, 0)
	Slab.Color = Color3.fromRGB(40, 40, 44)
	Slab.Parent = workspace
	local Surface = Instance.new("SurfaceGui")
	Surface.Name = "Surface"
	Surface.Face = Enum.NormalId.Front
	Surface.SizingMode = Enum.SurfaceGuiSizingMode.PixelsPerStud
	Surface.PixelsPerStud = 50
	Surface.LightInfluence = 0
	Surface.Parent = Slab
	Frame(Surface, "Picture", Color3.fromRGB(60, 110, 230), { Ambient = Color3.new(1, 1, 1), LightColor = Color3.new(0, 0, 0), Tint = Color3.fromRGB(255, 240, 90), Text = "TINT" })

	-- AlwaysOnTop, behind the slab: drawn over it, its picture 40 % transparent
	local Top = Instance.new("BillboardGui")
	Top.Name = "Top"
	Top.Size = UDim2.fromScale(4, 3)
	Top.AlwaysOnTop = true
	Top.Adornee = Holder("TopHolder", Vector3.new(7, 9, 4))
	Top.Parent = Top.Adornee
	Frame(Top, "Badge", Color3.fromRGB(230, 120, 40), { Ambient = Color3.fromRGB(120, 120, 120), LightColor = Color3.new(1, 1, 1), Transparency = 0.4, Text = "TOP" })
end
