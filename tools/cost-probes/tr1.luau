P.preload()
P.look(Vector3.new(0, 30, 60), Vector3.zero)
P.base()
local RS = game:GetService("RunService")
local function trails(n, props)
	local movers = {}
	P.grid(n, 8, function(i, pos)
		local h = P.part({ Size = Vector3.one, Position = pos, Transparency = 1 })
		local a0 = Instance.new("Attachment") a0.Position = Vector3.new(0, 1, 0) a0.Parent = h
		local a1 = Instance.new("Attachment") a1.Position = Vector3.new(0, -1, 0) a1.Parent = h
		local t = Instance.new("Trail") t.Attachment0 = a0 t.Attachment1 = a1 t.Lifetime = 5
		for k, v in props or {} do t[k] = v end
		t.Parent = h
		table.insert(movers, h)
	end)
	local base = {}
	for i, h in movers do base[i] = h.Position end
	for f = 1, 30 do
		for i, h in movers do h.Position = base[i] + Vector3.new(math.sin(f / 5) * 3, 0, 0) end
		RS.RenderStepped:Wait()
	end
end
P.case("TR 1 trail moving", function() trails(1) end, { frames = 2 })
P.case("TR 10 trails moving", function() trails(10) end, { frames = 2 })
P.case("TR 10 trails moving 2 textures", function() trails(10) for i, t in P.folder:GetDescendants() do if t:IsA("Trail") and i % 2 == 0 then t.Texture = "rbxasset://textures/face.png" end end end, { frames = 2 })
P.case("TR 10 trails still (never moved)", function() P.grid(10, 8, function(i, pos) local h = P.part({ Size = Vector3.one, Position = pos, Transparency = 1 }) local a0 = Instance.new("Attachment") a0.Parent = h local a1 = Instance.new("Attachment") a1.Position = Vector3.new(0, 2, 0) a1.Parent = h local t = Instance.new("Trail") t.Attachment0 = a0 t.Attachment1 = a1 t.Parent = h end) end)
