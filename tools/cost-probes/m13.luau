-- m13: beams in the transparent queue: which merge with their neighbours
local SS = game:GetService("ServerStorage")
local map = workspace:FindFirstChild("RorCapture")
if map then map.Parent = SS end
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local ok, err = pcall(function()
	local function count(build)
		P.reset()
		for _ = 1, 10 do RS.RenderStepped:Wait() end
		local base = Stats.SceneDrawcallCount
		build()
		for _ = 1, 40 do RS.RenderStepped:Wait() end
		return Stats.SceneDrawcallCount - base
	end
	P.look(Vector3.new(0, 30, 60), Vector3.new(0, 0, 0))
	local TEX = { "rbxasset://textures/particles/sparkles_main.dds", "rbxasset://textures/particles/smoke_main.dds" }
	local function beam(p, props)
		local h = P.part({ Size = Vector3.one, Position = p, Transparency = 1 })
		local a0 = Instance.new("Attachment") a0.Parent = h
		local a1 = Instance.new("Attachment") a1.Position = Vector3.new(1, 1, 0) a1.Parent = h
		local b = Instance.new("Beam") b.Attachment0 = a0 b.Attachment1 = a1
		for k, v in props or {} do b[k] = v end
		b.Parent = h
		return b
	end
	local function column(fn) for i = 0, 19 do fn(i, Vector3.new(0, 2, i * 2 - 20)) end end
	local function row(fn) for i = 0, 19 do fn(i, Vector3.new(i * 2 - 20, 2, 0)) end end
	local cases = {
		{ "20 beams, one column", function() column(function(i, p) beam(p) end) end },
		{ "20 beams, one row (one depth)", function() row(function(i, p) beam(p) end) end },
		{ "20 beams with a texture, column", function() column(function(i, p) beam(p, { Texture = TEX[1] }) end) end },
		{ "20 beams, two textures alternating", function() column(function(i, p) beam(p, { Texture = TEX[i % 2 + 1] }) end) end },
		{ "20 beams, two textures front/back halves", function() column(function(i, p) beam(p, { Texture = TEX[i < 10 and 1 or 2] }) end) end },
		{ "20 beams, widths differ", function() column(function(i, p) beam(p, { Width0 = 0.5 + i * 0.1, Width1 = 0.5 + i * 0.1 }) end) end },
		{ "20 beams, colours differ", function() column(function(i, p) beam(p, { Color = ColorSequence.new(Color3.fromHSV(i / 20, 1, 1)) }) end) end },
		{ "20 beams, segments alternate 10/4", function() column(function(i, p) beam(p, { Segments = i % 2 == 0 and 10 or 4 }) end) end },
		{ "20 beams, LightEmission alternates", function() column(function(i, p) beam(p, { LightEmission = i % 2 }) end) end },
		{ "20 beams, LightInfluence alternates", function() column(function(i, p) beam(p, { LightInfluence = i % 2 }) end) end },
		{ "20 beams, FaceCamera alternates", function() column(function(i, p) beam(p, { FaceCamera = i % 2 == 0 }) end) end },
		{ "20 beams, ZOffset alternates 0/1", function() column(function(i, p) beam(p, { ZOffset = i % 2 }) end) end },
		{ "20 beams, TextureSpeed alternates", function() column(function(i, p) beam(p, { Texture = TEX[1], TextureSpeed = i % 2 + 1 }) end) end },
		{ "20 beams, Transparency 1 everywhere", function() column(function(i, p) beam(p, { Transparency = NumberSequence.new(1) }) end) end },
		{ "20 beams, CurveSize0 alternates", function() column(function(i, p) beam(p, { CurveSize0 = i % 2 * 2 }) end) end },
		{ "10 beams then 10 transparent wood, halves", function() column(function(i, p) if i < 10 then beam(p) else P.part({ Size = Vector3.new(1.5, 1.5, 1.5), Position = p, Transparency = 0.5, Material = Enum.Material.Wood }) end end) end },
		{ "beams alternating with transparent wood", function() column(function(i, p) if i % 2 == 0 then beam(p) else P.part({ Size = Vector3.new(1.5, 1.5, 1.5), Position = p, Transparency = 0.5, Material = Enum.Material.Wood }) end end) end },
		{ "beams alternating with emitters", function() column(function(i, p) if i % 2 == 0 then beam(p) else local h = P.part({ Size = Vector3.one, Position = p, Transparency = 1 }) local e = Instance.new("ParticleEmitter") e.Rate = 20 e.Parent = h end end) end },
		{ "20 emitters, one column", function() column(function(i, p) local h = P.part({ Size = Vector3.one, Position = p, Transparency = 1 }) local e = Instance.new("ParticleEmitter") e.Rate = 20 e.Parent = h end) end },
		{ "20 trails? (no motion) beams on 2 attachments shared", function() local h = P.part({ Size = Vector3.one, Position = Vector3.new(0, 2, 0), Transparency = 1 }) local a0 = Instance.new("Attachment") a0.Parent = h local a1 = Instance.new("Attachment") a1.Position = Vector3.new(4, 4, 0) a1.Parent = h for i = 1, 20 do local b = Instance.new("Beam") b.Attachment0 = a0 b.Attachment1 = a1 b.Parent = h end end },
	}
	for _, c in cases do table.insert(P.out, c[1] .. ": " .. count(c[2])) end
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
return table.concat(P.out, string.char(10))
