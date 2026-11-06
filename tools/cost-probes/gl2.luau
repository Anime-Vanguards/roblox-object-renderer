P.preload()
local R = settings().Rendering
_G.ProbeQuality = _G.ProbeQuality or R.EditQualityLevel
local ok, err = pcall(function()
	for _, q in { 21, 18, 16, 14, 12, 10, 8, 4, 1 } do
		R.EditQualityLevel = q
		task.wait(0.3)
		P.look(Vector3.new(0, 80, 80), Vector3.zero)
		P.base()
		P.case("Q" .. q .. " 10 glass tr.5", function() P.blocks(10, 6, { Material = Enum.Material.Glass, Transparency = 0.5 }) end)
		P.case("Q" .. q .. " 10 blocks + 10 emitters", function() P.blocks(10, 6, {}) P.grid(10, 8, function(i, pos) local h = P.part({ Size = Vector3.one, Position = pos + Vector3.new(0, 6, 0), Transparency = 1 }) local e = Instance.new("ParticleEmitter") e.Rate = 0 e.Lifetime = NumberRange.new(100) e.Speed = NumberRange.new(0) e.Parent = h e:Emit(10) end) end)
		P.case("Q" .. q .. " 10 forcefield", function() P.blocks(10, 6, { Material = Enum.Material.ForceField }) end)
	end
end)
R.EditQualityLevel = _G.ProbeQuality
table.insert(P.out, "quality restored " .. tostring(R.EditQualityLevel) .. (ok and "" or (" ERROR " .. tostring(err))))
