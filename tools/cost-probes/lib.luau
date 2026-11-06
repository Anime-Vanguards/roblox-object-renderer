-- draw-call probe helpers: build in workspace.Probe, read Stats + SceneAnalysis after a few frames
local Stats = game:GetService("Stats")
local SAS = game:GetService("SceneAnalysisService")
local RS = game:GetService("RunService")
local AssetService = game:GetService("AssetService")
local HttpService = game:GetService("HttpService")
local cam = workspace.CurrentCamera
cam.CameraType = Enum.CameraType.Scriptable
local P = {}
P.out = {}
function P.reset()
	local old = workspace:FindFirstChild("Probe")
	if old then old:Destroy() end
	local f = Instance.new("Folder")
	f.Name = "Probe"
	f.Parent = workspace
	P.folder = f
	return f
end
function P.look(eye, at)
	cam.CFrame = CFrame.lookAt(eye, at)
end
local function flatten(node, prefix, acc)
	local name = prefix == "" and node.Name or (prefix .. "/" .. node.Name)
	if prefix ~= "" then acc[name] = string.format("%d/%d", node.Sizes.Drawcalls, node.Sizes.Triangles) end
	for _, c in node.Children or {} do flatten(c, prefix == "" and "." or name, acc) end
	return acc
end
function P.measure(frames)
	for i = 1, frames or 6 do RS.RenderStepped:Wait() end
	local ok, comp = pcall(function() return SAS:GetTriangleCompositionAsync() end)
	local parts = {}
	if ok then
		local flat = flatten(comp, "", {})
		local keys = {}
		for k in flat do table.insert(keys, k) end
		table.sort(keys)
		for _, k in keys do table.insert(parts, k:gsub("^%./", "") .. "=" .. flat[k]) end
	end
	return { scene = Stats.SceneDrawcallCount, tris = Stats.SceneTriangleCount, sd = Stats.ShadowsDrawcallCount, st = Stats.ShadowsTriangleCount, ui3d = Stats.UI3DDrawcallCount, comp = table.concat(parts, " ") }
end
function P.log(label, m)
	table.insert(P.out, string.format("%-34s scene %4d/%-7d sh %4d/%-7d ui3d %d | %s", label, m.scene, m.tris, m.sd, m.st, m.ui3d, m.comp))
end
function P.step(label, frames)
	P.log(label, P.measure(frames))
end
P.templates = {}
function P.mesh(id)
	local t = P.templates[id]
	if not t then
		t = AssetService:CreateMeshPartAsync(Content.fromAssetId(id))
		P.templates[id] = t
	end
	local m = t:Clone()
	m.Anchored = true
	return m
end
function P.part(props, parent)
	local p = Instance.new(props.ClassName or "Part")
	for k, v in props do if k ~= "ClassName" then p[k] = v end end
	p.Anchored = true
	p.Parent = parent or P.folder
	return p
end
function P.grid(n, spacing, fn)
	local side = math.ceil(math.sqrt(n))
	for i = 0, n - 1 do
		local x, z = i % side, i // side
		fn(i, Vector3.new((x - side / 2) * spacing, 2, (z - side / 2) * spacing))
	end
end
-- cases: each in a fresh folder, measured as the change from the empty scene
_G.ProbeTemplates = _G.ProbeTemplates or {}
P.templates = _G.ProbeTemplates
-- the uploaded meshes the cases build with: { small = <id>, sa = <id>, lod = <id>, heavy = <id>, skinned = <id> }
-- (none are published with ror: fill in meshes of your own to run the cases that use them)
P.MESH = {}
function P.preload()
	for _, id in P.MESH do pcall(P.mesh, id) end
end
local function parse(comp)
	local t = {}
	for k, v in comp:gmatch("(%S+)=(%S+)") do t[k] = v end
	return t
end
function P.base(frames)
	P.reset()
	P.empty = P.measure(frames or 10)
end
function P.case(label, build, opts)
	opts = opts or {}
	local f = P.reset()
	if opts.eye then P.look(opts.eye, opts.at or Vector3.zero) end
	build(f)
	local m = P.measure(opts.frames or 20)
	local e, mc = parse(P.empty.comp), parse(m.comp)
	local diffs = {}
	local keys = {}
	for k in mc do keys[k] = true end
	for k in e do keys[k] = true end
	local sorted = {}
	for k in keys do table.insert(sorted, k) end
	table.sort(sorted)
	for _, k in sorted do
		local a, b = e[k] or "0/0", mc[k] or "0/0"
		if a ~= b then
			local ad, at = a:match("(%d+)/(%d+)")
			local bd, bt = b:match("(%d+)/(%d+)")
			table.insert(diffs, string.format("%s %+d/%+d", k, tonumber(bd) - tonumber(ad), tonumber(bt) - tonumber(at)))
		end
	end
	table.insert(P.out, string.format("%-44s draws %+5d tris %+8d sh %d/%d ui3d %+d | %s", label, m.scene - P.empty.scene, m.tris - P.empty.tris, m.sd, m.st, m.ui3d - P.empty.ui3d, table.concat(diffs, ", ")))
	return m
end
function P.blocks(n, spacing, props, parent)
	P.grid(n, spacing, function(i, pos)
		local pr = table.clone(props or {})
		pr.Size = pr.Size or Vector3.new(4, 4, 4)
		pr.Position = pos
		if type(pr.Color) == "function" then pr.Color = pr.Color(i) end
		if type(pr.Material) == "function" then pr.Material = pr.Material(i) end
		if type(pr.Size) == "function" then pr.Size = pr.Size(i) end
		if type(pr.Transparency) == "function" then pr.Transparency = pr.Transparency(i) end
		if type(pr.Shape) == "function" then pr.Shape = pr.Shape(i) end
		P.part(pr, parent)
	end)
end
function P.meshes(n, spacing, id, fn, parent)
	P.grid(n, spacing, function(i, pos)
		local m = P.mesh(id)
		m.Position = pos
		if fn then fn(m, i) end
		m.Parent = parent or P.folder
	end)
end
