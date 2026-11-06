-- prof.luau: MicroProfiler timings through LibMP (require("@rbx/LibMP") in Studio MCP's execute_luau).
-- PROF.capture(frames) -> { frames, frameMs = {...}, timers = { [name] = { total, count, perFrame = {...} } } }
local LibMP = require("@rbx/LibMP")
local RS = game:GetService("RunService")
local PROF = {}

function PROF.capture(frames, opts)
	opts = opts or {}
	local C = LibMP.Control
	C:EnableProfiler(true)
	C:EnableCapture(true)
	C:SetFrameLimit(math.min(256, frames + 16))
	for _ = 1, frames + 12 do RS.RenderStepped:Wait() end
	C:CaptureToSlotSync(1)
	local s = LibMP.Session.OpenFromSlot(1)
	local tick = s:FetchGlobalDesc().TickToMsCpu
	local maxId = s:GetFrameIdMax() - 1
	local minId = math.max(s:GetFrameIdMin() + 1, maxId - frames + 1)
	local names, groups = {}, {}
	local function nameOf(id)
		local n = names[id]
		if n == nil then
			local d = s:FetchTimerDesc(id)
			n = d and d.TimerName or ("timer" .. id)
			names[id] = n
			if d then
				local g = s:FetchGroupDesc(d.GroupId)
				groups[n] = g and g.GroupName or "?"
			end
		end
		return n
	end
	local gpu = s:GetThreadIdGpu()
	local timers = {}
	local edges = {}
	local frameMs = {}
	for f = minId, maxId do
		local fd = s:FetchFrameDesc(f)
		table.insert(frameMs, (fd.TickEndCpu - fd.TickStartCpu) * tick)
	end
	local iter = s:CreateLogIterator()
	iter:Configure({ SkipGpuThreads = true, SkipEvents = true })
	local st = iter:GetState()
	iter:RewindTo(minId, maxId)
	while iter:Step() do
		if st:IsExit() and not st:ThreadStackIsUnderflowed() and not st:ThreadStackWasOverflowed() then
			local depth = st:ThreadStackDepth()
			local el = iter:GetCurrentThreadStackElement(depth)
			if el then
				local frame = st:FrameId()
				local name = nameOf(st:TimerId())
				if opts.tree and depth > 0 then
					local parent = iter:GetCurrentThreadStackElement(depth - 1)
					if parent then
						local key = nameOf(parent:EnterTimerId()) .. " > " .. name
						local e = edges[key]
						if not e then e = { total = 0, count = 0 } edges[key] = e end
						e.total += (st:Timestamp() - el:EnterTimestamp()) * tick
						e.count += 1
					end
				end
				local t = timers[name]
				if not t then
					t = { total = 0, count = 0, perFrame = {}, depthMin = depth, threads = {} }
					timers[name] = t
				end
				local ms = (st:Timestamp() - el:EnterTimestamp()) * tick
				t.total += ms
				t.count += 1
				t.perFrame[frame] = (t.perFrame[frame] or 0) + ms
				t.depthMin = math.min(t.depthMin, depth)
				t.threads[st:ThreadId()] = true
			end
		end
	end
	iter:Dispose()
	s:Dispose()
	local n = maxId - minId + 1
	for name, t in timers do
		local list = {}
		for f = minId, maxId do table.insert(list, t.perFrame[f] or 0) end
		table.sort(list)
		t.median = list[math.floor(#list / 2) + 1]
		t.p90 = list[math.floor(#list * 0.9) + 1]
		t.mean = t.total / n
		t.group = groups[name]
		local threads = 0
		for _ in t.threads do threads += 1 end
		t.threadCount = threads
		t.perFrame = nil
		t.threads = nil
	end
	table.sort(frameMs)
	return { frames = n, frameMedian = frameMs[math.floor(#frameMs / 2) + 1], timers = timers, edges = edges }
end

-- the timers matching a Lua pattern, by mean time per frame
function PROF.top(result, pattern, limit)
	local list = {}
	for name, t in result.timers do
		if not pattern or name:match(pattern) or (t.group and t.group:match(pattern)) then table.insert(list, { name = name, t = t }) end
	end
	table.sort(list, function(a, b) return a.t.mean > b.t.mean end)
	local out = {}
	for i = 1, math.min(limit or 30, #list) do
		local e = list[i]
		table.insert(out, string.format("%-44s %-10s mean %7.3f ms  median %7.3f  p90 %7.3f  n/frame %6.1f  depth %d  threads %d", e.name:sub(1, 44), (e.t.group or "?"):sub(1, 10), e.t.mean, e.t.median, e.t.p90, e.t.count / result.frames, e.t.depthMin, e.t.threadCount))
	end
	return table.concat(out, "\n")
end

-- the call tree under a scope: children by total time (per occurrence of the root)
function PROF.tree(result, root, maxDepth, minMs)
	local rootT = result.timers[root]
	if not rootT then return "no " .. root end
	local per = rootT.count
	local out = { string.format("%s: %.3f ms per occurrence (%d occurrences)", root, rootT.total / per, per) }
	local children = {}
	for key, e in result.edges do
		local parent, child = key:match("^(.-) > (.*)$")
		children[parent] = children[parent] or {}
		table.insert(children[parent], { name = child, e = e })
	end
	local seen = {}
	local function walk(name, depth)
		if depth > (maxDepth or 6) or seen[name] then return end
		seen[name] = true
		local list = children[name] or {}
		table.sort(list, function(a, b) return a.e.total > b.e.total end)
		for _, c in list do
			local ms = c.e.total / per
			if ms >= (minMs or 0.01) then
				table.insert(out, string.format("%s%-40s %7.3f ms  x%.1f", string.rep("  ", depth), c.name:sub(1, 40), ms, c.e.count / per))
				walk(c.name, depth + 1)
			end
		end
		seen[name] = nil
	end
	walk(root, 1)
	return table.concat(out, string.char(10))
end
