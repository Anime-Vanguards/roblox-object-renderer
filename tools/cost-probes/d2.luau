P.preload()
P.look(Vector3.new(0, 200, 200), Vector3.zero)
P.base()
local FACE, SPAWN = "rbxasset://textures/face.png", "rbxasset://textures/SpawnLocation.png"
local function decals(n, face, tex, cls)
	P.blocks(n, 6, {})
	for i, p in P.folder:GetChildren() do local d = Instance.new(cls or "Decal") d.Texture = tex or FACE d.Face = face or Enum.NormalId.Top d.Parent = p end
end
for _, n in { 50, 100, 120, 128, 129, 200, 256, 257, 400, 1000 } do
	P.case("D2 " .. n .. " decals top", function() decals(n) end, { frames = 30 })
end
P.case("D2 100 decals front face", function() decals(100, Enum.NormalId.Front) end, { frames = 30 })
P.case("D2 100 decals + 100 Textures same tex", function()
	P.blocks(100, 6, {})
	for i, p in P.folder:GetChildren() do local d = Instance.new(i % 2 == 0 and "Decal" or "Texture") d.Texture = FACE d.Face = Enum.NormalId.Top d.Parent = p end
end, { frames = 30 })
P.case("D2 1 block 6 decals 6 faces", function()
	local p = P.part({ Size = Vector3.new(6, 6, 6), Position = Vector3.new(0, 4, 0) })
	for _, f in Enum.NormalId:GetEnumItems() do local d = Instance.new("Decal") d.Texture = FACE d.Face = f d.Parent = p end
end, { frames = 30 })
P.case("D2 1 block 3 decals top same tex", function()
	local p = P.part({ Size = Vector3.new(6, 6, 6), Position = Vector3.new(0, 4, 0) })
	for i = 1, 3 do local d = Instance.new("Decal") d.Texture = FACE d.Face = Enum.NormalId.Top d.Parent = p end
end, { frames = 30 })
P.case("D2 100 decals ZIndex varied", function()
	P.blocks(100, 6, {})
	for i, p in P.folder:GetChildren() do local d = Instance.new("Decal") d.Texture = FACE d.ZIndex = i % 3 d.Face = Enum.NormalId.Top d.Parent = p end
end, { frames = 30 })
