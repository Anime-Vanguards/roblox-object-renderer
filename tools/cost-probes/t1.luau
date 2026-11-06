P.preload()
P.look(Vector3.new(0, 30, 80), Vector3.new(0, 0, 0))
P.base()
local FACE = "rbxasset://textures/face.png"
local function row(n, step, fn)
	for i = 0, n - 1 do
		local p = P.part({ Size = Vector3.new(4, 4, 4), Position = Vector3.zero + step * (i - (n - 1) / 2) })
		fn(p, i)
	end
end
local function trDecal(p) p.Transparency = 0.5 local d = Instance.new("Decal") d.Texture = FACE d.Face = Enum.NormalId.Top d.Parent = p end
local function ff(p) p.Material = Enum.Material.ForceField end
for _, kind in { { "trDecal", trDecal }, { "FF", ff } } do
	for _, n in { 1, 2, 4, 8 } do
		P.case(kind[1] .. " row along X n=" .. n, function() row(n, Vector3.new(6, 0, 0), kind[2]) end)
		P.case(kind[1] .. " row along Z n=" .. n, function() row(n, Vector3.new(0, 0, 6), kind[2]) end)
	end
	P.case(kind[1] .. " along X n=8 jitter z .01", function() row(8, Vector3.new(6, 0, 0), function(p, i) kind[2](p) p.Position += Vector3.new(0, 0, i * 0.01) end) end)
	P.case(kind[1] .. " along X n=8 jitter y 1", function() row(8, Vector3.new(6, 0, 0), function(p, i) kind[2](p) p.Position += Vector3.new(0, i % 2, 0) end) end)
	P.case(kind[1] .. " along X n=8 size varied", function() row(8, Vector3.new(6, 0, 0), function(p, i) kind[2](p) p.Size = Vector3.new(4, 4 + i % 3, 4) end) end)
end
