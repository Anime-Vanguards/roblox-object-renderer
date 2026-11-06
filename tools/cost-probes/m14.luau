-- m14: beams against occlusion and their culling box
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
	P.look(Vector3.new(0, 10, 60), Vector3.new(0, 10, 0))
	local function beam(p, q, props)
		local h = P.part({ Size = Vector3.one, Position = p, Transparency = 1 })
		local a0 = Instance.new("Attachment") a0.Parent = h
		local a1 = Instance.new("Attachment") a1.WorldPosition = q a1.Parent = h
		local b = Instance.new("Beam") b.Attachment0 = a0 b.Attachment1 = a1
		for k, v in props or {} do b[k] = v end
		b.Parent = h
		return b, a0, a1
	end
	local function wall() P.part({ Size = Vector3.new(200, 100, 2), Position = Vector3.new(0, 10, 30) }) end
	local function emitters() for i = 1, 10 do local h = P.part({ Size = Vector3.one, Position = Vector3.new(i * 4 - 20, 10, 0), Transparency = 1 }) local e = Instance.new("ParticleEmitter") e.Rate = 20 e.Parent = h end end
	local function beams(props) for i = 1, 10 do beam(Vector3.new(i * 4 - 20, 5, 0), Vector3.new(i * 4 - 20, 15, 0), props) end end
	table.insert(P.out, "wall alone: " .. count(wall))
	table.insert(P.out, "10 beams: " .. count(function() beams() end))
	table.insert(P.out, "10 beams behind the wall: " .. count(function() wall() beams() end))
	table.insert(P.out, "10 FaceCamera beams behind the wall: " .. count(function() wall() beams({ FaceCamera = true }) end))
	table.insert(P.out, "10 emitters: " .. count(emitters))
	table.insert(P.out, "10 emitters behind the wall: " .. count(function() wall() emitters() end))
	-- culling box: beams 10 studs behind the camera (z = 70), ends side by side, curve toward the view
	for _, curve in { 0, 10, 20, 40, 80 } do
		table.insert(P.out, "10 beams 10 behind the camera, CurveSize " .. curve .. " (axis toward the view): " .. count(function()
			for i = 1, 10 do
				local b, a0, a1 = beam(Vector3.new(i * 2 - 10, 10, 70), Vector3.new(i * 2 - 10 + 1, 10, 70), { CurveSize0 = curve, CurveSize1 = -curve })
				a0.WorldAxis = Vector3.new(0, 0, -1) a1.WorldAxis = Vector3.new(0, 0, -1)
			end
		end))
	end
	for _, curve in { 10, 40, 80 } do
		table.insert(P.out, "10 beams 10 behind the camera, CurveSize " .. curve .. " (axis away from the view): " .. count(function()
			for i = 1, 10 do
				local b, a0, a1 = beam(Vector3.new(i * 2 - 10, 10, 70), Vector3.new(i * 2 - 10 + 1, 10, 70), { CurveSize0 = curve, CurveSize1 = -curve })
				a0.WorldAxis = Vector3.new(0, 0, 1) a1.WorldAxis = Vector3.new(0, 0, 1)
			end
		end))
	end
	for _, width in { 1, 10, 20, 40, 80 } do
		table.insert(P.out, "10 beams 10 behind the camera, width " .. width .. ": " .. count(function()
			for i = 1, 10 do beam(Vector3.new(i * 2 - 10, 10, 70), Vector3.new(i * 2 - 10, 11, 70), { Width0 = width, Width1 = width }) end
		end))
		table.insert(P.out, "  FaceCamera: " .. count(function()
			for i = 1, 10 do beam(Vector3.new(i * 2 - 10, 10, 70), Vector3.new(i * 2 - 10, 11, 70), { Width0 = width, Width1 = width, FaceCamera = true }) end
		end))
	end
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
return table.concat(P.out, string.char(10))
