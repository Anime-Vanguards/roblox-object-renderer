-- Reverts changes apply.luau made, by key (every key when none is named).
--
--   lune run revert.luau <applied.rbxl> <out.rbxl> changes.json [key ...]
--
-- Removed instances come back from ServerStorage.RorCostRemoved to their RorCostParent; set properties take
-- `before` from changes.json again on the instances whose RorCostKeys list the key; a cloned SurfaceAppearance goes
-- and the parked one returns.
local fs = require("@lune/fs")
local roblox = require("@lune/roblox")
local process = require("@lune/process")
local serde = require("@lune/serde")
local Content, Color3, Vector3, Enum = roblox.Content, roblox.Color3, roblox.Vector3, roblox.Enum

local args = process.args
assert(#args >= 3, "usage: lune run revert.luau <applied.rbxl> <out.rbxl> changes.json [key ...]")
local game = roblox.deserializePlace(fs.readFile(args[1]))
local list = serde.decode("json", fs.readFile(args[3]))
local wanted = nil
if #args > 3 then
	wanted = {}
	for i = 4, #args do
		wanted[args[i]] = true
	end
end

local byKey = {}
for _, change in list.changes do
	byKey[change.key] = change
end

local function value(kind: string, raw: any): any
	if kind == "content" then
		return Content.fromUri(raw)
	elseif kind == "color3" then
		return Color3.new(raw[1], raw[2], raw[3])
	elseif kind == "vector3" then
		return Vector3.new(raw[1], raw[2], raw[3])
	elseif string.sub(kind, 1, 5) == "enum:" then
		local enum = (Enum :: any)[string.sub(kind, 6)]
		return if type(raw) == "number" then enum:FromValue(raw) else enum[raw]
	end
	return raw
end
local NAMES = { Color3uint8 = "Color", MaterialVariantSerialized = "MaterialVariant" }

local reverted = 0
-- set properties and cloned appearances, on every instance that lists a key
for _, instance in game:GetDescendants() do
	local cloneKey = instance:GetAttribute("RorCostKey")
	if type(cloneKey) == "string" and instance.ClassName == "SurfaceAppearance" and instance.Parent and instance.Parent.Name ~= "RorCostRemoved" and (not wanted or wanted[cloneKey]) then
		instance:Destroy()
		continue
	end
	local keys = instance:GetAttribute("RorCostKeys")
	if type(keys) ~= "string" then
		continue
	end
	local left = {}
	for key in string.gmatch(keys, "[^,]+") do
		local change = byKey[key]
		if change and (not wanted or wanted[key]) then
			if change.op == "set" then
				local target: any = instance
				target[NAMES[change.property] or change.property] = value(change.valueType, change.before)
			end
			reverted += 1
		else
			table.insert(left, key)
		end
	end
	instance:SetAttribute("RorCostKeys", if #left > 0 then table.concat(left, ",") else nil)
end
-- removed instances and parked appearances
local removed = game:GetService("ServerStorage"):FindFirstChild("RorCostRemoved")
if removed then
	for _, instance in removed:GetChildren() do
		local key = instance:GetAttribute("RorCostKey")
		if type(key) == "string" and (not wanted or wanted[key]) then
			local back = instance:FindFirstChild("RorCostParent")
			if back and back.Value then
				instance.Parent = back.Value
				back:Destroy()
				instance:SetAttribute("RorCostKey", nil)
				reverted += 1
			end
		end
	end
	if #removed:GetChildren() == 0 then
		removed:Destroy()
	end
end

fs.writeFile(args[2], roblox.serializePlace(game))
print(string.format("reverted %d -> %s", reverted, args[2]))
