-- Applies a change list written by `ror cost --edits ... --export` to a saved copy of the place (Lune 0.10.5 or later:
-- 0.10.2 cannot read current places).
--
--   lune run apply.luau <in.rbxl> <out.rbxl> changes.json [--skip keys.txt] [--only remove,set,surfaceAppearance]
--
-- remove: the instance moves to ServerStorage.RorCostRemoved, an ObjectValue RorCostParent under it and its key in the
-- attribute RorCostKey. set: the property takes `after`. surfaceAppearance: the target's SurfaceAppearance moves to
-- RorCostRemoved and the source's is cloned under the target (the clone carries RorCostKey). Every changed instance
-- lists its keys in the attribute RorCostKeys; revert.luau puts `before` back from changes.json. merge entries are left
-- for a merge tool. Every id resolves against the file as read, before anything moves.
local fs = require("@lune/fs")
local roblox = require("@lune/roblox")
local process = require("@lune/process")
local serde = require("@lune/serde")
local Instance, Content, Color3, Vector3, Enum = roblox.Instance, roblox.Content, roblox.Color3, roblox.Vector3, roblox.Enum

local args = process.args
assert(#args >= 3, "usage: lune run apply.luau <in.rbxl> <out.rbxl> changes.json [--skip keys.txt] [--only remove,set,surfaceAppearance]")
local skip, only = {}, nil
local i = 4
while i <= #args do
	if args[i] == "--skip" then
		for line in string.gmatch(fs.readFile(args[i + 1]), "[^\r\n]+") do
			skip[line] = true
		end
		i += 1
	elseif args[i] == "--only" then
		only = {}
		for op in string.gmatch(args[i + 1], "[^,]+") do
			only[op] = true
		end
		i += 1
	end
	i += 1
end

local game = roblox.deserializePlace(fs.readFile(args[1]))
local list = serde.decode("json", fs.readFile(args[3]))

-- an id's steps: each name and its occurrence among same-named siblings, from the DataModel down
local function resolve(steps): Instance?
	local node: Instance = game
	for _, step in steps do
		local name, occurrence = step[1], step[2]
		local n, found = 0, nil
		for _, child in node:GetChildren() do
			if child.Name == name then
				n += 1
				if n == occurrence then
					found = child
					break
				end
			end
		end
		if not found then
			return nil
		end
		node = found
	end
	return node
end

local resolved = {}
for _, change in list.changes do
	if change.steps and #change.steps > 0 then
		resolved[change.key] = resolve(change.steps) or false
	end
	if change.fromSteps then
		resolved[change.key .. "#from"] = resolve(change.fromSteps) or false
	end
end

local storage = game:GetService("ServerStorage")
local removed = storage:FindFirstChild("RorCostRemoved") or Instance.new("Folder")
removed.Name = "RorCostRemoved"
removed.Parent = storage

local function addKey(instance: Instance, key: string)
	local keys = instance:GetAttribute("RorCostKeys")
	if type(keys) ~= "string" or keys == "" then
		instance:SetAttribute("RorCostKeys", key)
	elseif not string.find("," .. keys .. ",", "," .. key .. ",", 1, true) then
		instance:SetAttribute("RorCostKeys", keys .. "," .. key)
	end
end

local function park(instance: Instance, key: string)
	local back = Instance.new("ObjectValue")
	back.Name = "RorCostParent"
	back.Value = instance.Parent
	instance:SetAttribute("RorCostKey", key)
	instance.Parent = removed
	back.Parent = instance
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

-- Lune names a few properties as Studio does
local NAMES = { Color3uint8 = "Color", MaterialVariantSerialized = "MaterialVariant" }

local applied, missing, skipped = {}, {}, {}
for _, change in list.changes do
	local op = change.op
	if skip[change.key] or (only and not only[op]) then
		table.insert(skipped, change.key)
		continue
	end
	if op == "merge" then
		table.insert(skipped, change.key .. " (a merge: build its mesh with a merge tool)")
		continue
	end
	local target = resolved[change.key]
	if not target or target.ClassName ~= change.class then
		table.insert(missing, change.key)
		continue
	end
	if op == "remove" then
		park(target, change.key)
	elseif op == "set" then
		local property = NAMES[change.property] or change.property
		local subject: any = target
		local ok, err = pcall(function()
			subject[property] = value(change.valueType, change.after)
		end)
		if not ok then
			table.insert(missing, change.key .. " (" .. tostring(err) .. ")")
			continue
		end
		addKey(target, change.key)
	elseif op == "surfaceAppearance" then
		local source = resolved[change.key .. "#from"]
		for _, child in target:GetChildren() do
			if child.ClassName == "SurfaceAppearance" then
				park(child, change.key)
			end
		end
		local appearance = source and source:FindFirstChildWhichIsA("SurfaceAppearance")
		if appearance then
			local copy = appearance:Clone()
			copy:SetAttribute("RorCostKey", change.key)
			copy.Parent = target
		end
		addKey(target, change.key)
	end
	table.insert(applied, change.key)
end

fs.writeFile(args[2], roblox.serializePlace(game))
print(string.format("applied %d, skipped %d, missing %d -> %s", #applied, #skipped, #missing, args[2]))
for _, key in missing do
	print("missing: " .. key)
end
