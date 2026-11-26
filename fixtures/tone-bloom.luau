-- The tone curve and the built-in bloom, each group on a black Neon backdrop (black Neon: nothing lights
-- it) 1,000 studs from the next, so a camera looking along +Z from 60 studs through a 40 degree field of
-- view sees only its own group (25.5 px a stud on a 1116 px tall view):
--
-- "tone" (x 0): 256 flush 2-stud SmoothPlastic tiles of grays 0 to 255, row by row from the top left.
--   Under a flat Ambient of 1 alone (no sun, sky light or fog) a gray c shows x = 2 c^2 2^EV.
-- "neon" (x 6000): the same 256 tiles in Neon (x = 25.75 c^4 2^EV, unlit).
-- "fog" (x 1000): black Neon panels 60 to 2,000 studs from the camera, side by side, each 0.14 of its
--   distance wide: under the default fog (FogEnd 100000) they show the fog alone.
-- "threshold" (x 2000): 28 3-stud SmoothPlastic squares of grays 228 to 255, 8 studs apart (four rows of
--   seven, the brightest at the bottom right): under the Ambient of 1 at EV +1 they step from x 3.20 to
--   4.00, across the bloom's threshold.
-- "kernel" (x 3000): white Neon squares of 4, 8, 12 px (top row) and 24, 48, 96 px (bottom row) on the
--   1116 px tall view, 13 studs apart: capped sources whose halos show the bloom's kernel.
-- "veil" (x 4000): one white Neon square of 12 studs (306 px) in the middle of the backdrop.
-- "black" (x 5000): the backdrop alone.
--
-- Measured in a Roblox client (0.747, quality level 21, LightingStyle Realistic, 1644 x 1116 and other
-- sizes, lossless frames; notes/render.md "Colour pipeline and tone curve" and "Bloom") against ror's render
-- of the same place: Lighting with Brightness, OutdoorAmbient and the environment scales 0, GlobalShadows
-- off, and per view its Ambient, ExposureCompensation, FogEnd and a BloomEffect of Intensity 0 (no halo)
-- or a ColorGradingEffect (Retro). It returns each group's camera target and the middle and size of every
-- tile or square.
return function(Context)
	local Root = Instance.new("Model")
	Root.Name = "ToneBloom"
	local Groups = {}

	local function Block(Name, Position, Size, Color, Material)
		local Part = Instance.new("Part")
		Part.Name = Name
		Part.Anchored = true
		Part.CastShadow = false
		Part.Material = Material
		Part.Color = Color
		Part.Size = Size
		Part.CFrame = CFrame.new(Position)
		Part.Parent = Root
		return Part
	end
	local function Backdrop(Center)
		Block("Backdrop", Center + Vector3.new(0, 0, 6), Vector3.new(160, 100, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
	end
	local function Group(Name, Center)
		local Entry = { name = Name, target = { Center.X, Center.Y, Center.Z }, items = {} }
		Groups[Name] = Entry
		return Entry
	end
	local function Item(Entry, Name, Position, Side, Extra)
		local Row = { name = Name, center = { Position.X, Position.Y, Position.Z - 0.1 }, side = Side }
		for Key, Value in Extra or {} do
			Row[Key] = Value
		end
		table.insert(Entry.items, Row)
	end

	-- tone: flush tiles, so no edge but the group's own lies between two
	do
		local Center = Vector3.new(0, 0, 0)
		local Entry = Group("tone", Center)
		Backdrop(Center)
		for Gray = 0, 255 do
			local Row, Column = Gray // 16, Gray % 16
			local Position = Center + Vector3.new((Column - 7.5) * 2, (7.5 - Row) * 2, 0)
			Block(`gray{Gray}`, Position, Vector3.new(2, 2, 0.2), Color3.fromRGB(Gray, Gray, Gray), Enum.Material.SmoothPlastic)
			Item(Entry, `gray{Gray}`, Position, 2, { gray = Gray })
		end
	end

	-- neon: the same tiles in Neon, unlit (x = 25.75 c^4 2^EV whatever the light)
	do
		local Center = Vector3.new(6000, 0, 0)
		local Entry = Group("neon", Center)
		Backdrop(Center)
		for Gray = 0, 255 do
			local Row, Column = Gray // 16, Gray % 16
			local Position = Center + Vector3.new((Column - 7.5) * 2, (7.5 - Row) * 2, 0)
			Block(`neon{Gray}`, Position, Vector3.new(2, 2, 0.2), Color3.fromRGB(Gray, Gray, Gray), Enum.Material.Neon)
			Item(Entry, `neon{Gray}`, Position, 2, { gray = Gray })
		end
	end

	-- fog: panels facing the camera at (1000, 0, -60), each 8 degrees of the view apart
	do
		local Center = Vector3.new(1000, 0, 0)
		local Entry = Group("fog", Center)
		local Eye = Center - Vector3.new(0, 0, 60)
		for Index, Distance in { 60, 125, 250, 500, 1000, 2000 } do
			local Angle = math.rad((Index - 3.5) * 8)
			local Direction = Vector3.new(math.sin(Angle), 0, math.cos(Angle))
			local Position = Eye + Direction * Distance
			local Part = Block(`fog{Distance}`, Position, Vector3.new(0.14 * Distance, 0.5 * Distance, 1), Color3.new(0, 0, 0), Enum.Material.Neon)
			Part.CFrame = CFrame.lookAt(Position, Eye)
			Item(Entry, `fog{Distance}`, Position, 0.14 * Distance, { distance = Distance })
		end
	end

	-- threshold: squares far enough apart that no halo reaches the next (8 studs, 204 px)
	do
		local Center = Vector3.new(2000, 0, 0)
		local Entry = Group("threshold", Center)
		Backdrop(Center)
		for Index = 0, 27 do
			local Gray = 228 + Index
			local Row, Column = Index // 7, Index % 7
			local Position = Center + Vector3.new((Column - 3) * 8, (1.5 - Row) * 8, 0)
			Block(`square{Gray}`, Position, Vector3.new(3, 3, 0.2), Color3.fromRGB(Gray, Gray, Gray), Enum.Material.SmoothPlastic)
			Item(Entry, `square{Gray}`, Position, 3, { gray = Gray })
		end
	end

	-- kernel: white Neon squares (x 25.75, capped) of sizes in px on the 1116 px view
	do
		local Center = Vector3.new(3000, 0, 0)
		local Entry = Group("kernel", Center)
		Backdrop(Center)
		local PerStud = 1116 / (2 * 60 * math.tan(math.rad(20)))
		for Index, Pixels in { 4, 8, 12, 24, 48, 96 } do
			local Row, Column = (Index - 1) // 3, (Index - 1) % 3
			local Position = Center + Vector3.new((Column - 1) * 13, (0.5 - Row) * 12, 0)
			local Side = Pixels / PerStud
			Block(`kernel{Pixels}`, Position, Vector3.new(Side, Side, 0.2), Color3.new(1, 1, 1), Enum.Material.Neon)
			Item(Entry, `kernel{Pixels}`, Position, Side, { pixels = Pixels })
		end
	end

	-- veil: one large capped source, and the backdrop alone
	do
		local Center = Vector3.new(4000, 0, 0)
		local Entry = Group("veil", Center)
		Backdrop(Center)
		Block("veil", Center, Vector3.new(12, 12, 0.2), Color3.new(1, 1, 1), Enum.Material.Neon)
		Item(Entry, "veil", Center, 12)
		local Black = Vector3.new(5000, 0, 0)
		Group("black", Black)
		Backdrop(Black)
	end

	Root.Parent = workspace
	return { groups = Groups, eyeDistance = 60, fov = 40 }
end
