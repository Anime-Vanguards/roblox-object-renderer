-- A ScreenGui's ViewportFrame over the whole screen, as a portal arm is drawn: the local player's
-- PlayerGui holds a ScreenGui whose full-screen ViewportFrame (no background, its own light) shows a
-- WorldModel through a Camera that stands where the view's camera does, so its world lines up with
-- the Workspace's. A bar reaches out of a portal on a wall; a mask around the portal's opening (an
-- EditableMesh whose every vertex has alpha 0) hides what lies behind it in the frame's world (the bar's
-- back, a red ball beside the opening) and leaves the screen showing there. A green ball in front of the
-- mask shows whole, a label of a ScreenGui above it draws over the picture.
-- ror render fixtures/viewport-screen.luau --camera '{"mode":"lookAt","eye":[0,5,-16],"target":[0,5,0],"fov":50}'
-- (notes/render.md "ViewportFrames"; ROR_REE: the engine emulator's checkout)
local OPENING = Vector2.new(2, 1.5)
local OUTER = 40

local function Mask(): MeshPart
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
	-- a frame round the opening: above, below, left and right of it
	Quad(Vector2.new(-OUTER, OPENING.Y), Vector2.new(OUTER, OUTER))
	Quad(Vector2.new(-OUTER, -OUTER), Vector2.new(OUTER, -OPENING.Y))
	Quad(Vector2.new(-OUTER, -OPENING.Y), Vector2.new(-OPENING.X, OPENING.Y))
	Quad(Vector2.new(OPENING.X, -OPENING.Y), Vector2.new(OUTER, OPENING.Y))
	local Part = AssetService:CreateMeshPartAsync(Content.fromObject(Mesh))
	Part.Name = "Mask"
	Part.Anchored = true
	Part.DoubleSided = true
	Part.Color = Color3.new(0, 0, 0)
	Part.Material = Enum.Material.SmoothPlastic
	return Part
end

local function Piece(Name: string, Shape: Enum.PartType, Size: Vector3, At: CFrame, Color: Color3, Parent: Instance): Part
	local Part = Instance.new("Part")
	Part.Name = Name
	Part.Shape = Shape
	Part.Anchored = true
	Part.Size = Size
	Part.CFrame = At
	Part.Color = Color
	Part.Parent = Parent
	return Part
end

return function()
	Piece("Floor", Enum.PartType.Block, Vector3.new(40, 1, 30), CFrame.new(0, -0.5, 0), Color3.fromRGB(120, 124, 130), workspace)
	Piece("Wall", Enum.PartType.Block, Vector3.new(20, 12, 1), CFrame.new(0, 6, 0.5), Color3.fromRGB(46, 30, 70), workspace)
	-- the portal on the wall's face: the opening the mask leaves
	Piece("Portal", Enum.PartType.Block, Vector3.new(OPENING.X * 2, OPENING.Y * 2, 0.05), CFrame.new(0, 5, -0.03), Color3.fromRGB(150, 70, 230), workspace).Material = Enum.Material.Neon

	local Gui = Instance.new("ScreenGui")
	Gui.Name = "PortalArm"
	Gui.IgnoreGuiInset = true
	Gui.DisplayOrder = -5
	local Frame = Instance.new("ViewportFrame")
	Frame.Size = UDim2.fromScale(1, 1)
	Frame.BackgroundTransparency = 1
	Frame.Ambient = Color3.fromRGB(110, 100, 140)
	Frame.LightColor = Color3.new(1, 1, 1)
	Frame.LightDirection = Vector3.new(-1, -1, 1)
	Frame.Parent = Gui
	local World = Instance.new("WorldModel")
	World.Parent = Frame
	-- out of the portal towards the camera, its back half behind the wall's plane: cut at the opening's edge
	Piece("Bar", Enum.PartType.Block, Vector3.new(0.8, 0.8, 12), CFrame.new(0, 5, 0) * CFrame.Angles(0.15, 0.6, 0), Color3.fromRGB(240, 200, 60), World)
	Piece("Hidden", Enum.PartType.Ball, Vector3.new(2, 2, 2), CFrame.new(4, 5, 2), Color3.fromRGB(230, 40, 40), World)
	Piece("Front", Enum.PartType.Ball, Vector3.new(2, 2, 2), CFrame.new(-4, 3, -3), Color3.fromRGB(60, 200, 90), World)
	local Cover = Mask()
	Cover.CFrame = CFrame.new(0, 5, 0)
	Cover.Parent = World
	local Camera = Instance.new("Camera")
	Camera.FieldOfView = 50
	Camera.CFrame = CFrame.lookAt(Vector3.new(0, 5, -16), Vector3.new(0, 5, 0))
	Camera.Parent = Frame
	Frame.CurrentCamera = Camera

	local Labels = Instance.new("ScreenGui")
	Labels.Name = "Labels"
	Labels.IgnoreGuiInset = true
	local Label = Instance.new("TextLabel")
	Label.AnchorPoint = Vector2.new(0.5, 0.5)
	Label.Position = UDim2.fromScale(0.5, 0.5)
	Label.Size = UDim2.fromScale(0.1, 0.05)
	Label.BackgroundColor3 = Color3.fromRGB(250, 210, 40)
	Label.Text = "OVER"
	Label.TextScaled = true
	Label.Parent = Labels

	local PlayerGui = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
	Gui.Parent = PlayerGui
	Labels.Parent = PlayerGui
end
