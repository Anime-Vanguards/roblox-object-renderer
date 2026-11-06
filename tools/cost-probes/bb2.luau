P.look(Vector3.new(0, 0, 0), Vector3.new(0, 0, -1))
local vp = workspace.CurrentCamera.ViewportSize
table.insert(P.out, "viewport " .. tostring(vp) .. " aspect " .. vp.X / vp.Y)
P.base()
local function bb(x, partSize, size)
	P.case(string.format("BB part %d size %s at x=%d depth 25", partSize, size, x), function()
		local p = P.part({ Size = Vector3.one * partSize, Position = Vector3.new(x, 0, -25), Transparency = 1 })
		local g = Instance.new("BillboardGui") g.Size = size == "px" and UDim2.fromOffset(80, 30) or UDim2.new(4, 0, 2, 0) g.Parent = p
		local f = Instance.new("Frame") f.Size = UDim2.fromScale(1, 1) f.Parent = g
	end, { frames = 10 })
end
for _, x in { 20, 25, 26, 27, 28, 30, 32, 35, 40, 50 } do bb(x, 1, "px") end
for _, x in { 26, 28, 30, 35, 40 } do bb(x, 10, "px") end
for _, x in { 26, 28, 30, 35 } do bb(x, 1, "studs") end
