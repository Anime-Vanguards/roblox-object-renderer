-- Minified texture probes (notes/render.md "Particles", "Texture sampling"): which mip levels a Roblox
-- client blends for a texture at a given number of texels a pixel, how it builds the levels of an image
-- file, and what a coarser level keeps of the colour under transparent texels. Each group 1,000 studs from
-- the next along X, over a black Neon backdrop 30 studs behind it, seen from -Z at 60 studs with a field
-- of view of 40 (25.55 px a stud at 1116 px tall). Particles are still, unlit, white and camera-facing;
-- a quad spans twice its Size, so a `T`-texel image at `d` texels a pixel has Size T / (2 d 25.55).
--
-- Textures come in `Context` (URIs; groups whose textures are missing are left out):
--   AlphaLevels, GreyLevels, ColourLevels: 128-texel DDS files whose every level is one value (alpha
--   255 / 0 alternating, white / black alternating, a colour per level), so a pixel reads the levels and
--   the fraction the client blends. RampAlpha, RampGrey: 16 bands of alpha (grey) k * 17. Pattern<Name>:
--   256-texel images of period 2 (WK0 white beside black under alpha 0, WW0 white beside white under alpha
--   0, WK opaque white beside black, WG beside grey 64, A64 alpha 255 beside 64, CHK0 a checker of white
--   and black under alpha 0) and Stripes (white and black stripes 8 texels wide). LinesBlack, LinesWhite:
--   256-texel thin soft white lines, black or white under alpha 0.
--
-- "lod" (x 0): each of the three level textures at 2^(k/8) texels a pixel, k = 0 to 32, `<kind>-<k>`.
-- "aniso" (x 1000): ColourLevels and AlphaLevels squashed (Squash 1: 4 times as tall as wide; 3: 16
--   times) at 2^(k/4) texels a pixel along their height, k = 0 to 12, `<kind>-s<squash>-<k>`; and
--   AlphaLevels at Squash 1 turned 30 degrees, `alpha-r30-<k>`.
-- "chain" (x 2000): each pattern at 2, 2.83, 4, 8 and 16 texels a pixel, `<pattern>-<n>`.
-- "cal" (x 3000): RampAlpha and RampGrey at half a texel a pixel.
-- "lines" (x 4000): LinesBlack and LinesWhite at 1, 1.41, 2, 2.83, 4, 5.66, 8, 11.3 and 16 texels a pixel.
-- "exact" (x 5000): Stripes, LinesBlack, LinesWhite and CHK0 drawn 128, 64 and 32 px wide with their edges
--   on pixel edges (2, 4 and 8 texels a pixel), so each pixel centre is a texel centre of the level the
--   client samples there.
-- "surface" (x 6000): Decals (front faces of black SmoothPlastic panels towards the camera, lit by a
--   white Ambient the place gives) of WK0, LinesBlack, ColourLevels and AlphaLevels at 2, 4 and 8 texels a
--   pixel, and ColourLevels panels turned 60 and 75 degrees about Y at 2 and 4 texels a pixel along their
--   height (4 and 8, 7.7 and 15.5 across).
-- "noise256", "noise255" (x 7000, 8000): Noise<side>C (random RGB, opaque) and Noise<side>A (white, random
--   alpha) images of 256 and 255 texels drawn as wide as their levels 0 to 3 (256, 128, 64, 32 and 255, 127,
--   63, 31 px), `c-<level>` and `a-<level>`: each pixel reads one texel of that level.
return function(Context)
	local Root = Instance.new("Model")
	Root.Name = "MipProbes"
	local PPU = 558 / math.tan(math.rad(20)) / 60
	local Groups = {}

	local function Group(Name, Center)
		local Model = Instance.new("Model")
		Model.Name = Name
		Model.Parent = Root
		Groups[Name] = { center = { Center.X, Center.Y, Center.Z }, camera = { eye = { Center.X, Center.Y, -60 }, target = { Center.X, Center.Y, 0 }, fov = 40 } }
		local Backdrop = Instance.new("Part")
		Backdrop.Name = "Backdrop"
		Backdrop.Anchored = true
		Backdrop.CanCollide = false
		Backdrop.CastShadow = false
		Backdrop.Material = Enum.Material.Neon
		Backdrop.Color = Color3.new(0, 0, 0)
		Backdrop.Size = Vector3.new(300, 200, 1)
		Backdrop.CFrame = CFrame.new(Center + Vector3.new(0, 0, 30))
		Backdrop.Parent = Model
		return Model
	end
	local function Constant(Value)
		return NumberSequence.new({ NumberSequenceKeypoint.new(0, Value), NumberSequenceKeypoint.new(1, Value) })
	end
	local function Emitter(Model, Name, Position, Texture, Size, Set)
		local Part = Instance.new("Part")
		Part.Name = `{Name}-anchor`
		Part.Anchored = true
		Part.CanCollide = false
		Part.CastShadow = false
		Part.Transparency = 1
		Part.Size = Vector3.new(0.2, 0.2, 0.2)
		Part.CFrame = CFrame.new(Position)
		Part.Parent = Model
		local At = Instance.new("Attachment")
		At.Name = "At"
		At.Parent = Part
		local E = Instance.new("ParticleEmitter")
		E.Name = Name
		E.Rate = 0
		E.Lifetime = NumberRange.new(20, 20)
		E.Speed = NumberRange.new(0, 0)
		E.SpreadAngle = Vector2.new(0, 0)
		E.Rotation = NumberRange.new(0, 0)
		E.RotSpeed = NumberRange.new(0, 0)
		E.Size = Constant(Size)
		E.Transparency = Constant(0)
		E.Color = ColorSequence.new(Color3.new(1, 1, 1))
		E.LightEmission = 0
		E.LightInfluence = 0
		E.Brightness = 1
		E.Texture = Texture
		E.Drag = 0
		E.Acceleration = Vector3.new(0, 0, 0)
		E.EmissionDirection = Enum.NormalId.Top
		for Key, Value in Set or {} do
			E[Key] = Value
		end
		E.Parent = At
		return E
	end
	-- left to right in rows `Width` studs wide from the top, `Gap` studs apart; each item {name, width,
	-- height, place(position)}
	local function Shelf(Center, Items, Width, Gap)
		local Rows, Row, RowWidth = {}, {}, 0
		for _, Item in Items do
			if #Row > 0 and RowWidth + Gap + Item.width > Width then
				table.insert(Rows, Row)
				Row, RowWidth = {}, 0
			end
			RowWidth += (if #Row > 0 then Gap else 0) + Item.width
			table.insert(Row, Item)
		end
		if #Row > 0 then table.insert(Rows, Row) end
		local Total, Tall = 0, {}
		for Index, R in Rows do
			Tall[Index] = 0
			for _, Item in R do Tall[Index] = math.max(Tall[Index], Item.height) end
			Total += Tall[Index] + (if Index > 1 then Gap else 0)
		end
		local Y = Center.Y + Total / 2
		for Index, R in Rows do
			local Span = -Gap
			for _, Item in R do Span += Item.width + Gap end
			local X = Center.X + Span / 2
			for _, Item in R do
				-- seen from -Z, +X is on the left: rows run right to left in X to read left to right
				Item.place(Vector3.new(X - Item.width / 2, Y - Tall[Index] / 2, Center.Z))
				X -= Item.width + Gap
			end
			Y -= Tall[Index] + Gap
		end
	end
	local function SizeFor(Texels, Density)
		return Texels / (2 * Density * PPU)
	end

	if Context.AlphaLevels and Context.GreyLevels and Context.ColourLevels then
		local Center = Vector3.new(0, 0, 0)
		local Model = Group("lod", Center)
		local Items = {}
		for _, Kind in { { "alpha", Context.AlphaLevels }, { "grey", Context.GreyLevels }, { "colour", Context.ColourLevels } } do
			for K = 0, 32 do
				local Size = SizeFor(128, 2 ^ (K / 8))
				table.insert(Items, { width = 2 * Size, height = 2 * Size, place = function(P) Emitter(Model, `{Kind[1]}-{K}`, P, Kind[2], Size) end })
			end
		end
		Shelf(Center, Items, 1560 / PPU, 8 / PPU)

		local Model2 = Group("aniso", Center + Vector3.new(1000, 0, 0))
		local Items2 = {}
		for _, Kind in { { "colour", Context.ColourLevels }, { "alpha", Context.AlphaLevels } } do
			for _, Squash in { 1, 3 } do
				for K = 0, 12 do
					-- tall by 1 + Squash, narrow by as much
					local Size = SizeFor(128, 2 ^ (K / 4)) / (1 + Squash)
					table.insert(Items2, { width = 2 * Size / (1 + Squash), height = 2 * Size * (1 + Squash), place = function(P) Emitter(Model2, `{Kind[1]}-s{Squash}-{K}`, P, Kind[2], Size, { Squash = Constant(Squash) }) end })
				end
			end
		end
		for K = 0, 12 do
			local Size = SizeFor(128, 2 ^ (K / 4)) / 2
			local Reach = 2 * Size * 2
			table.insert(Items2, { width = Reach, height = Reach, place = function(P) Emitter(Model2, `alpha-r30-{K}`, P, Context.AlphaLevels, Size, { Squash = Constant(1), Rotation = NumberRange.new(30, 30) }) end })
		end
		Shelf(Center + Vector3.new(1000, 0, 0), Items2, 1560 / PPU, 8 / PPU)
	end

	local Patterns = {}
	for _, Name in { "WK0", "WW0", "WK", "WG", "A64", "CHK0" } do
		if Context[`Pattern{Name}`] then table.insert(Patterns, { string.lower(Name), Context[`Pattern{Name}`] }) end
	end
	if Context.Stripes then table.insert(Patterns, { "stripes", Context.Stripes }) end
	if #Patterns > 0 then
		local Center = Vector3.new(2000, 0, 0)
		local Model = Group("chain", Center)
		local Items = {}
		for _, Pattern in Patterns do
			for _, Density in { 2, 2.83, 4, 8, 16 } do
				local Size = SizeFor(256, Density)
				table.insert(Items, { width = 2 * Size, height = 2 * Size, place = function(P) Emitter(Model, `{Pattern[1]}-{math.round(Density * 100)}`, P, Pattern[2], Size) end })
			end
		end
		Shelf(Center, Items, 1560 / PPU, 8 / PPU)
	end

	if Context.RampAlpha and Context.RampGrey then
		local Center = Vector3.new(3000, 0, 0)
		local Model = Group("cal", Center)
		local Size = SizeFor(256, 0.5)
		Emitter(Model, "ramp-alpha", Center + Vector3.new(Size + 4 / PPU, 0, 0), Context.RampAlpha, Size)
		Emitter(Model, "ramp-grey", Center - Vector3.new(Size + 4 / PPU, 0, 0), Context.RampGrey, Size)
	end

	if Context.LinesBlack and Context.LinesWhite then
		local Center = Vector3.new(4000, 0, 0)
		local Model = Group("lines", Center)
		local Items = {}
		for _, Kind in { { "black", Context.LinesBlack }, { "white", Context.LinesWhite } } do
			for _, Density in { 1, 1.41, 2, 2.83, 4, 5.66, 8, 11.3, 16 } do
				local Size = SizeFor(256, Density)
				table.insert(Items, { width = 2 * Size, height = 2 * Size, place = function(P) Emitter(Model, `{Kind[1]}-{math.round(Density * 100)}`, P, Kind[2], Size) end })
			end
		end
		Shelf(Center, Items, 1560 / PPU, 8 / PPU)

		-- whole pixels from the screen's centre (a pixel corner at 1644 x 1116), edges on pixel edges
		local Exact = Vector3.new(5000, 0, 0)
		local Model2 = Group("exact", Exact)
		local Kinds = { { "black", Context.LinesBlack }, { "white", Context.LinesWhite } }
		if Context.Stripes then table.insert(Kinds, { "stripes", Context.Stripes }) end
		if Context.PatternCHK0 then table.insert(Kinds, { "chk0", Context.PatternCHK0 }) end
		for Column, Kind in Kinds do
			for Row, Width in { 128, 64, 32 } do
				local X = (Column - (#Kinds + 1) / 2) * 200
				local Y = (2 - Row) * 160
				Emitter(Model2, `{Kind[1]}-{Width}`, Exact + Vector3.new(-X / PPU, Y / PPU, 0), Kind[2], Width / (2 * PPU))
			end
		end
	end

	-- each pixel centre a texel centre of the level drawn: centres whole pixels (even widths) or half pixels
	-- (odd ones) from the screen's centre
	for Index, Side in { 256, 255 } do
		local Colour, Alpha = Context[`Noise{Side}C`], Context[`Noise{Side}A`]
		if Colour and Alpha then
			local Center = Vector3.new(6000 + Index * 1000, 0, 0)
			local Model = Group(`noise{Side}`, Center)
			for Row, Kind in { { "c", Colour }, { "a", Alpha } } do
				local Left = -700
				for Level = 0, 3 do
					local Width = math.floor(Side / 2 ^ Level)
					local X = Left + Width / 2
					local Y = (Row == 1 and -140 or 140) + (Width % 2) / 2
					Emitter(Model, `{Kind[1]}-{Level}`, Center + Vector3.new(-X / PPU, -Y / PPU, 0), Kind[2], Width / (2 * PPU))
					Left += Width + 16
				end
			end
		end
	end

	if Context.PatternWK0 and Context.LinesBlack and Context.ColourLevels and Context.AlphaLevels then
		local Center = Vector3.new(6000, 0, 0)
		local Model = Group("surface", Center)
		local function Panel(Name, Texture, Texels, Density, Turn)
			local Side = Texels / Density / PPU
			local Part = Instance.new("Part")
			Part.Name = Name
			Part.Anchored = true
			Part.CanCollide = false
			Part.CastShadow = false
			Part.Material = Enum.Material.SmoothPlastic
			Part.Color = Color3.new(0, 0, 0)
			Part.Size = Vector3.new(Side, Side, 0.2)
			local D = Instance.new("Decal")
			D.Name = "Decal"
			D.Face = Enum.NormalId.Front
			D.Texture = Texture
			D.Parent = Part
			return Part, Side, Turn or 0
		end
		local Items = {}
		for _, Kind in { { "wk0", Context.PatternWK0, 256 }, { "black", Context.LinesBlack, 256 }, { "colour", Context.ColourLevels, 128 }, { "alpha", Context.AlphaLevels, 128 } } do
			for _, Density in { 2, 4, 8 } do
				local Part, Side = Panel(`{Kind[1]}-{Density}`, Kind[2], Kind[3], Density)
				table.insert(Items, { width = Side, height = Side, place = function(P)
					Part.CFrame = CFrame.new(P)
					Part.Parent = Model
				end })
			end
		end
		for _, Turn in { 60, 75 } do
			for _, Density in { 2, 4 } do
				local Part, Side = Panel(`colour-t{Turn}-{Density}`, Context.ColourLevels, 128, Density)
				local Across = Side * math.cos(math.rad(Turn))
				table.insert(Items, { width = Across + 0.2, height = Side, place = function(P)
					Part.CFrame = CFrame.new(P) * CFrame.Angles(0, math.rad(Turn), 0)
					Part.Parent = Model
				end })
			end
		end
		Shelf(Center, Items, 1560 / PPU, 12 / PPU)
	end

	Root.Parent = workspace
	return { groups = Groups }
end
