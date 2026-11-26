# Rendering pipeline and rules

`ror` targets Roblox Studio at maximum graphics quality (level 21, `Future` lighting) using `wgpu` (Vulkan or Direct3D 12 on Windows, Vulkan on Linux, Metal on macOS).

Core modules: `crates/ror/src/scene/` (loading and extraction), `crates/ror/src/render/` (render passes and GPU caches), and `crates/ror/shaders/` (WGSL shaders).

## Scene pipeline

1. **Load (`scene::load`):** Parses `.rbxm`, `.rbxmx`, `.rbxl`, and `.rbxlx` files via `rbx-dom` (places render `Workspace`), the place or model inside a `.rbx` archive the same way ([archives.md](archives.md)), downloads asset IDs through `ror-assets`, or loads `ree` script recordings.
2. **Playback (`scene::playback`):** Applies timeline events, tweens, motion, and emissions at the target timestamp ([playback.md](playback.md)).
3. **Extract (`scene::extract`):** Converts instances into renderable `Scene` data—parts, transforms (`CFrame` / `GetPivot`), materials, composited character clothing, emitters, beams, trails, lights, Highlights, GUIs, and environment (`Lighting`, `Sky`, `Atmosphere`, `Clouds`, post effects).
4. **Resolve (`ror-assets`):** Resolves `rbxasset://` from the local Roblox Studio or Player installation and fetches/caches `rbxassetid://` and external assets.

## Supported scene features

**Rendered:** All primitive shapes (including Studs and surface types), `MeshPart`s, `SpecialMesh`es, unions, 2022 and pre-2022 materials with `MaterialVariant`s, `SurfaceAppearance`, `Decal`s, `Texture`s, `Neon`, `ForceField`, transparency, `Glass`, `ParticleEmitter`s, `Beam`s, `Trail`s, legacy effects (`Fire`, `Smoke`, `Sparkles`, `ForceField` shield, `Explosion`), skinned meshes, R6/R15 characters (`BodyColors`, `Shirt`, `Pants`, `ShirtGraphic`, `CharacterMesh`), `Highlight`s, `SurfaceGui`s / `BillboardGui`s / `ScreenGui`s (via `rur`), `ViewportFrame`s, local lights with shadows, sun shadows, voxel light grid, SSAO, skybox (sun, moon & stars), `Atmosphere`, fog, `Terrain.Clouds`, bloom, `ColorCorrection`, `ColorGrading`, `Blur`, `DepthOfField`, and `SunRays`.

**Not rendered:** `Workspace.Terrain` smooth terrain, water, and grass. Layered clothing renders as its raw authored mesh rather than fitted to the character.

## Color and tone-mapping pipeline

Shared rules in `shaders/common.wgsl`:

- **Decode:** Colors decode via square (`albedo = c²`, `c` in `0..1`), not sRGB.
- **Lighting:** Linear in `Lighting.Brightness`. Diffuse is Lambertian: `x = Brightness * albedo * (n · l) + specular`.
- **Exposure:** Multiplies radiance by `2^ExposureCompensation` prior to tone mapping.
- **Tone curve & encode:** Display output is `√T(x)` per channel, where `y = 1.0098 * x`:
  - For `x <= 2.1313`: `T(x) = y * (2.725891*y + 0.00329) / (y * (2.307040*y + 1.951868) + 1)`
  - For `x > 2.1313`: `T(x) = 1.00999 - 0.214791 * (1 + 0.32012 * (x - 2.1313))^-2.07968`
  - Reference check: a white surface facing a `Brightness = 1` sun has `x = 1` → `184/255`.
- **Clamping & blending:** Exposed radiance clamps at `7.84` per channel before blending and MSAA resolve. The scene render target stores `√x` so alpha blending operates in square-root space to match Studio.

## Lighting and shadows

- **Ambient:** Flat across faces: `x = 2 * max(Ambient², OutdoorAmbient²) * albedo`. `OutdoorAmbient` is attenuated by sky visibility from the light grid; both ambient terms are darkened by SSAO.
- **Voxel light grid:** 4-stud world-aligned voxels. Each voxel propagates sky transmission downward as the mean of the 8 horizontal neighbors one layer above (`light * transmission`), stored as `v = 1 - (1 - L)⁴`. Surfaces sample `v` trilinearly 6 studs along their geometric normal. Blocks occlude voxels by horizontal coverage times `1 - Transparency⁴`; balls, cylinders, and wedges scale by volume fraction times `0.79`, `0.93`, and `1.0`; `MeshPart`s do not contribute to the light grid. `v` multiplies `OutdoorAmbient` only (not `Ambient`, local lights, or sun).
- **Environment lighting (`render::environment`):** Diffuse sky light (`EnvironmentDiffuseScale`) samples a 6-direction ambient cube weighted by `normal²`. Specular sky reflections (`EnvironmentSpecularScale`) sample prefiltered mip levels of the skybox.
- **Sun & moon:** Positioned from `Lighting.ClockTime` and `GeographicLatitude` (`sun_direction`). Sun shadows use cascaded exponential variance shadow maps (EVSM) blurred by a separable screen-aligned Gaussian driven by `ShadowSoftness` (per Roblox's GDC 2020 talk; e.g., 0.31-stud edge width at `ShadowSoftness = 0.2`). As in the Roblox engine (0.747, quality level 21, `fixtures/sun-shadows.luau`), a surface further than 587.5 studs from the camera takes no sun shadow, whatever the field of view and the window's shape, and no cascade lets light under a part more than about 7 studs in front of a surface along the sun's rays.
- **Local lights:** `PointLight`, `SpotLight`, and `SurfaceLight` support `Range`, `Brightness`, `Angle`, `Face`, and shadows. High light counts are accelerated via clustered shading (`render::light_clusters`), while particles, beams, and trails sample a voxel light field (`render::voxel_field`).
- **Lighting style:** Under `Lighting.LightingStyle` Soft every surface takes its local light from the lights' 4-stud voxels, as particles, and surfaces 600 studs or more from the camera, do under Realistic: no shadow maps (a light's `Shadows` changes nothing in `ror`), no `n · l` and no specular, one blurred pool reaching a little past `Range`, and light passing walls and plates. The sun, the sky, materials and the voxel light grid are drawn as under Realistic. Measured in the Roblox engine (0.747, quality level 21) with `fixtures/lighting-style.luau`, each view drawn under both styles: where nothing stands near a light, 89-98 % of the lit pixels within 2 of 255 and 99.8-100 % within 8. Not modelled: the engine dims a shadowed light's voxels round parts that fill its grid (a block or a wall beside the light keeps 0.7-0.9 of the light on the floor near it and less behind it, `MeshPart`s too; a light without `Shadows` is not dimmed), and Soft draws a little more ambient occlusion round contacts.
- **Shadows and Transparency:** A part with `CastShadow` casts a full shadow below `Transparency` 0.748 and none from 0.748 up, from the sun and from shadowed local lights alike, whatever its `Material` (`ForceField`, `Glass` and `Neon` included) and whether it is a `Part` or a `MeshPart`. There is no partial shadow in between, and a part at `Transparency` 1 casts nothing. Measured in the Roblox engine (0.747, quality level 21, `LightingStyle` Realistic; under Soft local lights cast no shadow maps, see Lighting style): plates 0.00005 apart cast up to 0.74795, and the cut does not move with the caster's size (1 to 12 studs) or its distance from the camera (50 to 220 studs). A `SurfaceAppearance` in `AlphaMode` Transparency casts them texel by texel, where its `ColorMap`'s alpha times `1 - Transparency` is over the same cut (0.252, that is 1 - 0.748): leaf cards, foliage and hair shadow in the shape their alpha cuts out, from the sun and from shadowed local lights, onto the ground and onto one another, also when they are out of view. At `Transparency` 0 alpha 65 of 255 casts the full shadow and 64 none, at 0.5 alpha 129 and 128, at 0.7 alpha 215 and 214; no texel casts part of a shadow. Patterns cut by alpha shadow as the same pattern built from parts does: the alpha is read two texture levels finer than the shadow map's texels. In `AlphaMode` Overlay and TintMask the alpha cuts nothing. Measured in the Roblox engine (0.747, quality level 21, `LightingStyle` Realistic) with `fixtures/leaf-shadows.luau`. A `SpecialMesh` texture's alpha leaves its sun shadow out in `ror`; that case is not measured.
- **`LocalTransparencyModifier`:** Model files do not keep it. A `.luau` script run through the engine emulator records the transparency a part is drawn at, the modifier included, once it changes, and `ror` draws and cuts the shadow by it as the engine does (the cut at 1 - (1 - `Transparency`)(1 - modifier) = 0.748); a modifier set before the part first appears is not recorded yet.
- **Shadow-only parts (`RorShadowOnly`):** A hint for this renderer alone, with no counterpart in the Roblox engine, which ignores the attribute and treats the part by its properties (so below `Transparency` 0.748 it draws and shadows, and from there up it does neither). A `BasePart` whose attribute `RorShadowOnly` is the boolean `true` (`part:SetAttribute("RorShadowOnly", true)`; a number, a string or `false` does nothing) is left out of the picture and, with `CastShadow` on, casts the sun's and local lights' shadows as it would at `Transparency` 0, at any `Transparency` up to and including 1. Use it for casters a cutaway view removes from sight (a wall or roof that must still shade the room). Nothing of the part is drawn: its surface, `Decal`s, `Texture`s and `SurfaceAppearance`, its place in a `Highlight` and in the id and layer outputs. Everything else follows its properties: single-sided meshes shadow from the faces turned towards the light unless `DoubleSided` or `ForceField`, the voxel light grid takes it by its shape and `Transparency` like any part (an opaque block also shuts out sky light, one at `Transparency` 1 none), lights and effects parented to it still work, and it counts towards the scene's bounds and automatic framing. With `CastShadow` off it does nothing at all. `ror cost` estimates the engine's work and has no rule for the attribute: leave marked parts out of a scene you cost.
- **A view without the sun's shadow maps (`RorNoSunShadowMaps`):** A hint for this renderer alone, with no counterpart in the Roblox engine. A `Lighting` whose attribute `RorNoSunShadowMaps` is the boolean `true` (`Lighting:SetAttribute("RorNoSunShadowMaps", true)`, also on the `Lighting` that `--lighting-from` reads) is drawn without the sun's shadow maps, as a client draws at a graphics quality level under 12 (measured in Studio 0.742 at edit quality 1 to 21: a column's sun shadow is absent at levels 1 to 10, soft at 12 and 14, full from 16). No `MeshPart` shades the sun then. What still stops it is what fills the voxel light grid: the sun reaches a surface as far as the grid sees the sky over it, the same visibility `OutdoorAmbient` takes, so a roof of blocks still keeps a room out of the sun. That rule is an approximation, not a measurement: how the engine shades the sun at those levels has not been captured. Everything else is drawn as at the top level; in particular local lights stay per pixel with their own shadows, where the engine lights through its voxel grid below level 16. Without the attribute nothing changes.
- **Ambient occlusion:** Screen-space AO computed from opaque depth and normals. Static-camera video clips reuse previous-frame AO and recompute only around moving geometry.

## Sky, atmosphere, and fog

- **Skybox (`roblox_sky_direction`):** `Ft` = `-Z`, `Bk` = `+Z`, `Lf` = `+X`, `Rt` = `-X`, with `Up` and `Dn` rotated 90°, further rotated by `SkyboxOrientation`. Displayed at full resolution via the tone curve of `1.935 * texel²`.
- **Stars (`sky.wgsl` `stars`):** With `CelestialBodiesShown`, `StarCount` stars (at most 5,000; 3,000 without a `Sky`) strewn over the whole sky, 0.6 of them above the horizon, each a square one pixel across at any field of view in one of four colours, its light (a fitted distribution) spread over the pixels by the 4x multisampling points it covers. They dim towards the horizon, by the time of day (`render::environment::star_light`: full by night, a quarter to a half around sunrise and sunset, none from 7:15 to 16:45), slightly with `EnvironmentDiffuseScale`, and in steps under an `Atmosphere`'s haze. Roblox strews them anew whenever `StarCount` changes and redraws some of their brightness every ~0.7 s; ror's field is fixed, so only their statistics match.
- **Atmosphere (`AtmosphereSky`):** Renders sky haze/glare and blends distant geometry into atmospheric scattering.
- **Fog:** Given normalized distance `u` from `FogStart` to `FogEnd` (`0..1`), surfaces blend by `u * (1 + 0.325 * (1 - u))` toward `1.935 * FogColor²`.

## Clouds

Terrain `Clouds` ray-march through the cloud volumes shipped in your Roblox installation (`content/sky/clouds.dds` and `cloudDetail3D.dds`), thresholded by `Cover`, lit by sun, sky, and ambient light, and also dim direct sunlight near the camera (`render::clouds::sun_cover`).

Public builds render clouds using a generic volumetric shader (`shaders/clouds.wgsl`, `render/clouds/generic.rs`): a single-scattering spherical-shell march with Beer-Lambert extinction, two-lobe Henyey-Greenstein sun scattering with a short self-shadow march, and a multiple-scattering approximation. `Cover` thresholds and `Color` exponent are fitted to Studio; other constants are tuned by eye, so public clouds are not pixel-calibrated to Studio. Override tuning parameters via `ROR_TUNE_CLOUDS="key=value,..."` (matching fields of `Tuning`). `ror compare` excludes clouds on both sides because Studio clouds continuously evolve.

## Post effects and bloom

- **Bloom (`render::post`, `shaders/bloom.wgsl`):** Reproduces both Studio's always-on baseline bloom and `BloomEffect` using a half-resolution grid, sources clamped at `7.84`, and a halo kernel fitted to Studio.
- **Other post effects (`shaders/post.wgsl`):** `ColorCorrection`, `ColorGrading` (`TonemapperPreset`), `Blur`, `DepthOfField`, and `SunRays` attached under `Lighting` or `Camera`.

## GUIs

`SurfaceGui`, `BillboardGui`, and `ScreenGui` subtrees are dispatched to [rur](https://github.com/Anime-Vanguards/roblox-ui-renderer), which returns rendered images that `ror` composites onto part faces, billboard planes, or the final screen (`render::rur`, `render::guis`). `ViewportFrame` 3D worlds are rendered directly by `ror`—either through the frame's own camera (using its `Ambient`, `LightColor`, and `LightDirection`, without sky light, shadows, or tone mapping) or through the main camera for portal/window setups.

## Drawing a scene again

`Renderer::render` keeps what does not depend on the camera between calls, so a host drawing the same `Scene` again and again from a moving camera (an editor's viewport) pays for it once:

- **Geometry:** the parts' draws, instance buffer and light grid are kept by the scene's content (every part's properties, material variants, Highlight membership), not by the `Scene` value, so a scene edited in place or built anew is matched exactly: an unchanged scene reuses them, a changed one is prepared again from its unchanged parts' own preparations. `RenderStats::geometry_reused` says whether a render reused them. A scene some of whose assets could not be loaded is kept until the first failure is due to be retried (30 seconds after it), and its renders keep reporting them in `RenderStats::missing`.
- **Local light shadows:** the maps of the shadowed local lights a view reaches are kept while the camera reaches the same ones.
- **Every frame:** the sun's shadow cascades, ambient occlusion, the main pass and the post effects follow the camera.

A live scene (`Scene::live`) is prepared every frame: its skinned meshes are written over in place.

## Code entry points

- `render::Renderer::render_view` and `render/mod.rs` — Top-level view rendering and pass ordering.
- `render::pipelines` — WGSL shader compilation and pipeline creation.
- `render::prepared`, `render::residency`, `render::texture_files` — GPU geometry caches, mip streaming, and disk texture caches.
