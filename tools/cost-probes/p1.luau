P.preload()
P.look(Vector3.new(0, 60, 80), Vector3.zero)
P.base()
local FACE, SPAWN = "rbxasset://textures/face.png", "rbxasset://textures/SpawnLocation.png"
local function holder(pos)
	return P.part({ Size = Vector3.new(1, 1, 1), Position = pos, Transparency = 1, CanCollide = false })
end
local function emitters(n, props)
	P.grid(n, 8, function(i, pos)
		local e = Instance.new("ParticleEmitter")
		e.Rate = 0
		e.Lifetime = NumberRange.new(100)
		e.Speed = NumberRange.new(0)
		for k, v in props or {} do if k ~= "__count" then e[k] = type(v) == "function" and v(i) or v end end
		e.Parent = holder(pos)
		e:Emit(props and props.__count or 10)
	end)
end
P.case("P invisible holder parts only x10", function() P.grid(10, 8, function(i, pos) holder(pos) end) end)
P.case("P 1 emitter 10 particles", function() emitters(1) end)
P.case("P 10 emitters same", function() emitters(10) end)
P.case("P 10 emitters 2 textures", function() emitters(10, { Texture = function(i) return i % 2 == 0 and FACE or SPAWN end }) end)
P.case("P 10 emitters LightEmission 1", function() emitters(10, { LightEmission = 1 }) end)
P.case("P 10 emitters 1 particle each", function() emitters(10, { __count = 1 }) end)
P.case("P 1 emitter 0 particles", function()
	local e = Instance.new("ParticleEmitter") e.Rate = 0 e.Parent = holder(Vector3.zero)
end)
P.case("P 1 emitter 200 particles", function() emitters(1, { __count = 200 }) end)
P.case("P 10 emitters Orientation velocityParallel", function() emitters(10, { Orientation = Enum.ParticleOrientation.VelocityParallel }) end)
P.case("P 10 emitters FlipbookLayout", function() emitters(10, { FlipbookLayout = Enum.ParticleFlipbookLayout.Grid4x4 }) end)
P.case("P 10 emitters LightInfluence 0", function() emitters(10, { LightInfluence = 0 }) end)
P.case("P 10 emitters ZOffset", function() emitters(10, { ZOffset = 2 }) end)
P.case("P 10 emitters LockedToPart", function() emitters(10, { LockedToPart = true }) end)
local function beams(n, props)
	P.grid(n, 8, function(i, pos)
		local h = holder(pos)
		local a0 = Instance.new("Attachment") a0.Position = Vector3.new(-3, 0, 0) a0.Parent = h
		local a1 = Instance.new("Attachment") a1.Position = Vector3.new(3, 3, 0) a1.Parent = h
		local b = Instance.new("Beam") b.Attachment0 = a0 b.Attachment1 = a1
		for k, v in props or {} do b[k] = type(v) == "function" and v(i) or v end
		b.Parent = h
	end)
end
P.case("B 1 beam", function() beams(1) end)
P.case("B 10 beams same", function() beams(10) end)
P.case("B 10 beams segments 1", function() beams(10, { Segments = 1 }) end)
P.case("B 10 beams curved", function() beams(10, { CurveSize0 = 2, CurveSize1 = -2 }) end)
P.case("B 10 beams 2 textures", function() beams(10, { Texture = function(i) return i % 2 == 0 and FACE or SPAWN end }) end)
P.case("B 10 beams FaceCamera", function() beams(10, { FaceCamera = true }) end)
P.case("B 10 beams disabled", function() beams(10, { Enabled = false }) end)
P.case("B 10 beams transparency 1", function() beams(10, { Transparency = NumberSequence.new(1) }) end)
P.case("B 10 beams width 0", function() beams(10, { Width0 = 0, Width1 = 0 }) end)
