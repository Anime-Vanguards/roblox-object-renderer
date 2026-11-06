P.preload()
P.look(Vector3.new(0, 80, 80), Vector3.zero)
P.base()
for _, t in { 0, 0.01, 0.3, 0.5, 0.9 } do
	P.case("GL 10 glass tr" .. t, function() P.blocks(10, 6, { Material = Enum.Material.Glass, Transparency = t }) end)
	P.case("GL 10 glass tr" .. t .. " + 10 plastic", function() P.blocks(10, 6, { Material = Enum.Material.Glass, Transparency = t }) P.blocks(10, 7, {}) end)
end
