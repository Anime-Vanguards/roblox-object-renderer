-- m7: two blocks at pairs of positions: where copies stop sharing a draw
local SS = game:GetService("ServerStorage")
local map = workspace:FindFirstChild("RorCapture")
if map then map.Parent = SS end
local RS = game:GetService("RunService")
local Stats = game:GetService("Stats")
local ok, err = pcall(function()
	local function pair(a, b)
		local f = P.reset()
		P.look((a + b) / 2 + Vector3.new(0, 30, 900), (a + b) / 2)
		for _ = 1, 10 do RS.RenderStepped:Wait() end
		local base = Stats.SceneDrawcallCount
		P.part({ Size = Vector3.new(4, 4, 4), Position = a }) P.part({ Size = Vector3.new(4, 4, 4), Position = b })
		for _ = 1, 25 do RS.RenderStepped:Wait() end
		return Stats.SceneDrawcallCount - base
	end
	local function row(label, pairs)
		local cells = {}
		for _, p in pairs do table.insert(cells, string.format("%s:%d", p[3], pair(p[1], p[2]))) end
		table.insert(P.out, label .. " " .. table.concat(cells, " "))
	end
	local function vy(a, b) return { Vector3.new(0, a, 0), Vector3.new(0, b, 0), a .. "/" .. b } end
	local function hx(a, b) return { Vector3.new(a, 2, 0), Vector3.new(b, 2, 0), a .. "/" .. b } end
	local function hz(a, b) return { Vector3.new(0, 2, a), Vector3.new(0, 2, b), a .. "/" .. b } end
	row("y pairs", { vy(0, 10), vy(0, 30), vy(0, 60), vy(0, 100), vy(0, 127), vy(0, 129), vy(100, 120), vy(126, 130), vy(-2, 2), vy(250, 262), vy(254, 258), vy(510, 514) })
	row("x pairs", { hx(-2, 2), hx(0, 500), hx(0, 1000), hx(-500, 500), hx(0, 2000), hx(1020, 1030), hx(2040, 2060) })
	row("z pairs", { hz(-2, 2), hz(0, 500), hz(0, 1000), hz(0, 2000) })
end)
if map then map.Parent = workspace end
if not ok then table.insert(P.out, "ERROR " .. tostring(err)) end
