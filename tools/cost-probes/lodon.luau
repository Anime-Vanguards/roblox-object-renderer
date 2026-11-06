-- distance-based mesh LOD for this run only; the Studio settings (detail level, edit quality) go back to their values at the end (lodoff.luau)
_G.ProbeDetail = _G.ProbeDetail or settings().Rendering.MeshPartDetailLevel
_G.ProbeQuality = _G.ProbeQuality or settings().Rendering.EditQualityLevel
settings().Rendering.MeshPartDetailLevel = Enum.MeshPartDetailLevel.DistanceBased
