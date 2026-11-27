-- A Humanoid's name and health over its head as a Roblox client draws them (0.747, graphics quality 21,
-- 1644 x 1116 at GUI scale 1; notes/render.md "Names over heads"). Context.Case builds one case: R6-shaped rigs
-- (an anchored Torso, a Head over it and an invisible HumanoidRootPart round a Humanoid) ahead of a camera at
-- (0, 500, 0) looking down -Z, before a black wall, and returns what it made and its camera; without a Case it
-- returns the cases' names. The client's local character stood 2 studs behind the camera.
--
-- "w-d<N>": a rig named Alpha N studs ahead (the text's sizes and its fade near NameDisplayDistance).
-- "f-black-d<N>", "f-white-d10": a long DisplayName on black and before a white wall (the font and its edge).
-- "b-d<N>": a rig at half health N studs ahead (the bar's sizes); "c-<P>": the bar at P % (AlwaysOn).
-- "o-...": boards between the camera and the head or the name (which line hides it; transparent; CanQuery off).
--
-- ror render fixtures/name-tags.luau --context '{"Case":"w-d16"}' --size 1644x1116@1 --camera ...
local ORIGIN = Vector3.new(0, 500, 0)
local CAMERA = { eye = { 0, 500, 0 }, target = { 0, 500, -40 }, fov = 70 }

local function Part(Name: string, Size: Vector3, At: Vector3, Color: Color3, Parent: Instance): Part
	local Item = Instance.new("Part")
	Item.Name = Name
	Item.Anchored = true
	Item.Size = Size
	Item.CFrame = CFrame.new(At)
	Item.Color = Color
	Item.Material = Enum.Material.SmoothPlastic
	Item.TopSurface = Enum.SurfaceType.Smooth
	Item.BottomSurface = Enum.SurfaceType.Smooth
	Item.Parent = Parent
	return Item
end

-- an R6-shaped rig whose torso's centre is At
local function Rig(Name: string, At: Vector3, Props: any?): Model
	local Model = Instance.new("Model")
	Model.Name = Name
	Part("Torso", Vector3.new(2, 2, 1), At, Color3.fromRGB(40, 127, 71), Model)
	Part("Head", Vector3.new(2, 1, 1), At + Vector3.new(0, 1.5, 0), Color3.fromRGB(245, 205, 48), Model)
	local Root = Part("HumanoidRootPart", Vector3.new(2, 2, 1), At, Color3.fromRGB(163, 162, 165), Model)
	Root.Transparency = 1
	Model.PrimaryPart = Root
	local Humanoid = Instance.new("Humanoid")
	Humanoid.RequiresNeck = false
	local Health = nil
	for Key, Value in Props or {} do
		if Key == "Health" then
			Health = Value
		else
			(Humanoid :: any)[Key] = Value
		end
	end
	Humanoid.Parent = Model
	if Health then
		Humanoid.Health = Health
	end
	Model.Parent = workspace
	return Model
end

local function Ahead(Distance: number, X: number?, Y: number?): Vector3
	return ORIGIN + Vector3.new(X or 0, (Y or 0) - 1.5, -Distance)
end

local function Board(At: Vector3, Size: Vector3, Props: any?): Part
	local Item = Part("Board", Size, At, Color3.fromRGB(80, 80, 200), workspace)
	for Key, Value in Props or {} do
		(Item :: any)[Key] = Value
	end
	return Item
end

local GLYPHS = "Hamburgefonstiv AQgy 0123"
local Cases = {}
for _, Distance in { 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 34, 38, 42, 46, 50, 55, 60, 65, 70, 75, 80, 85, 88, 91, 94, 97 } do
	Cases["w-d" .. Distance] = function()
		return { Rig("Alpha", Ahead(Distance)) }
	end
end
for _, Distance in { 10, 20, 50 } do
	Cases["f-black-d" .. Distance] = function()
		return { Rig("Glyphs", Ahead(Distance), { DisplayName = GLYPHS }) }
	end
end
Cases["f-white-d10"] = function()
	return { Rig("Glyphs", Ahead(10), { DisplayName = GLYPHS }), Part("White", Vector3.new(40, 6, 1), Ahead(14, 0, 3), Color3.new(1, 1, 1), workspace) }
end
for _, Distance in { 10, 16, 24, 40, 60, 90 } do
	Cases["b-d" .. Distance] = function()
		return { Rig("Hurt", Ahead(Distance), { Health = 50 }) }
	end
end
for _, Percent in { 100, 90, 75, 60, 50, 40, 25, 10, 0 } do
	Cases["c-" .. Percent] = function()
		return { Rig("Hurt", Ahead(12), { HealthDisplayType = Enum.HumanoidHealthDisplayType.AlwaysOn, Health = Percent }) }
	end
end
Cases["o-head-only"] = function()
	return { Rig("HeadHidden", Ahead(30)), Board(Ahead(15, 0, 1.5), Vector3.new(4, 0.5, 0.2)) }
end
Cases["o-tag-only"] = function()
	return { Rig("TagHidden", Ahead(30)), Board(Ahead(15, 0, 2.35), Vector3.new(4, 0.5, 0.2)) }
end
Cases["o-transparent-board"] = function()
	return { Rig("Glassy", Ahead(30)), Board(Ahead(15, 0, 1.8), Vector3.new(8, 3, 0.2), { Transparency = 0.5 }) }
end
Cases["o-cancollide-off"] = function()
	return { Rig("Ghost", Ahead(30)), Board(Ahead(15, 0, 1.8), Vector3.new(8, 3, 0.2), { CanCollide = false, CanQuery = false }) }
end

return function(Context)
	local Case = Context and Context.Case
	if not Case then
		local Names = {}
		for Name in Cases do
			table.insert(Names, Name)
		end
		table.sort(Names)
		return { cases = Names, camera = CAMERA }
	end
	local Wall = Part("Wall", Vector3.new(2000, 2000, 1), Vector3.new(0, 500, -300), Color3.new(0, 0, 0), workspace)
	local Made = Cases[Case]()
	table.insert(Made, Wall)
	return { made = Made, camera = CAMERA }
end
