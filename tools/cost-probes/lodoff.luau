settings().Rendering.MeshPartDetailLevel = _G.ProbeDetail or Enum.MeshPartDetailLevel.Level00
settings().Rendering.EditQualityLevel = _G.ProbeQuality or settings().Rendering.EditQualityLevel
table.insert(P.out, "detail restored " .. tostring(settings().Rendering.MeshPartDetailLevel) .. ", quality " .. tostring(settings().Rendering.EditQualityLevel))
