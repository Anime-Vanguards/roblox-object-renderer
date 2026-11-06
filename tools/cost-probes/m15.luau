-- m15: a beam's culling box: how far its width and CurveSize reach (beams behind the camera)
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
		for _ = 1, 30 do RS.RenderStepped:Wait() end
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
	-- one beam 'behind' studs behind the camera, its ends 1 stud apart sideways
	local function threshold(behind, prop, values)
		local row = {}
		for _, v in values do
			local n = count(function()
				local props = { Width0 = 0, Width1 = 0 }
				if prop == "width" then props.Width0 = v props.Width1 = v else props.CurveSize0 = v props.CurveSize1 = -v end
				local _, a0, a1 = beam(Vector3.new(0, 10, 60 + behind), Vector3.new(1, 10, 60 + behind), props)
				a0.WorldAxis = Vector3.new(0, 0, 1) a1.WorldAxis = Vector3.new(0, 0, 1)
			end)
			table.insert(row, v .. ":" .. n)
		end
		table.insert(P.out, string.format("%d behind, %s: %s", behind, prop, table.concat(row, " ")))
	end
	threshold(10, "width", { 18, 19, 20, 21, 22, 23, 24, 26, 28 })
	threshold(10, "curve", { 12, 13, 14, 15, 16, 18, 20, 22, 24, 26, 28, 30, 34, 38 })
	threshold(20, "width", { 38, 39, 40, 41, 42, 44, 46 })
	threshold(20, "curve", { 24, 26, 28, 30, 32, 36, 40, 44, 48, 52, 56, 60, 70, 80 })
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
return table.concat(P.out, string.char(10))
