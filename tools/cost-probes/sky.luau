P.reset()
local L = game:GetService("Lighting")
table.insert(P.out, "sun dir " .. tostring(L:GetSunDirection()) .. " moon " .. tostring(L:GetMoonDirection()))
local cam = workspace.CurrentCamera
for _, pitch in { -90, -60, -30, 0, 30, 60, 89 } do
	local row = {}
	for _, yaw in { 0, 45, 90, 135, 180, 225, 270, 315 } do
		cam.CFrame = CFrame.Angles(0, math.rad(yaw), 0) * CFrame.Angles(math.rad(pitch), 0, 0)
		local m = P.measure(4)
		table.insert(row, (m.comp:match("Sky=(%S+)") or "-"))
	end
	table.insert(P.out, string.format("pitch %3d: %s", pitch, table.concat(row, " ")))
end
for _, fov in { 20, 70, 120 } do
	cam.FieldOfView = fov
	cam.CFrame = CFrame.Angles(0, 0, 0)
	table.insert(P.out, "fov " .. fov .. " level: " .. (P.measure(4).comp:match("Sky=(%S+)") or "-"))
end
cam.FieldOfView = 70
local sun = L:GetSunDirection()
cam.CFrame = CFrame.lookAt(Vector3.zero, sun)
table.insert(P.out, "at sun: " .. (P.measure(4).comp:match("Sky=(%S+)") or "-"))
cam.CFrame = CFrame.lookAt(Vector3.zero, -sun)
table.insert(P.out, "away from sun: " .. (P.measure(4).comp:match("Sky=(%S+)") or "-"))
cam.CFrame = CFrame.lookAt(Vector3.zero, L:GetMoonDirection())
table.insert(P.out, "at moon: " .. (P.measure(4).comp:match("Sky=(%S+)") or "-"))
