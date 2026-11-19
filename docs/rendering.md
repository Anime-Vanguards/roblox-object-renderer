# Rendering pipeline and rules

`ror` targets Roblox Studio at maximum graphics quality (level 21, `Future` lighting) using `wgpu` (Vulkan or Direct3D 12 on Windows, Vulkan on Linux, Metal on macOS).

Core modules: `crates/ror/src/scene/` (loading and extraction), `crates/ror/src/render/` (render passes and GPU caches), and `crates/ror/shaders/` (WGSL shaders).

## Scene pipeline

1. **Load (`scene::load`):** Parses `.rbxm`, `.rbxmx`, `.rbxl`, and `.rbxlx` files via `rbx-dom` (places render `Workspace`), downloads asset IDs through `ror-assets`, or loads `ree` script recordings.
2. **Playback (`scene::playback`):** Applies timeline events, tweens, motion, and emissions at the target timestamp ([playback.md](playback.md)).
3. **Extract (`scene::extract`):** Converts instances into renderable `Scene` data—parts, transforms (`CFrame` / `GetPivot`), materials, composited character clothing, emitters, beams, trails, lights, Highlights, GUIs, and environment (`Lighting`, `Sky`, `Atmosphere`, `Clouds`, post effects).
4. **Resolve (`ror-assets`):** Resolves `rbxasset://` from the local Roblox Studio or Player installation and fetches/caches `rbxassetid://` and external assets.

## Supported scene features

**Rendered:** All primitive shapes (including Studs and surface types), `MeshPart`s, `SpecialMesh`es, unions, 2022 and pre-2022 materials with `MaterialVariant`s, `SurfaceAppearance`, `Decal`s, `Texture`s, `Neon`, `ForceField`, transparency, `Glass`, `ParticleEmitter`s, `Beam`s, `Trail`s, legacy effects (`Fire`, `Smoke`, `Sparkles`, `ForceField` shield, `Explosion`), skinned meshes, R6/R15 characters (`BodyColors`, `Shirt`, `Pants`, `ShirtGraphic`, `CharacterMesh`), `Highlight`s, `SurfaceGui`s / `BillboardGui`s / `ScreenGui`s (via `rur`), `ViewportFrame`s, local lights with shadows, sun shadows, voxel light grid, SSAO, skybox (sun & moon), `Atmosphere`, fog, `Terrain.Clouds`, bloom, `ColorCorrection`, `ColorGrading`, `Blur`, `DepthOfField`, and `SunRays`.

**Not rendered:** `Workspace.Terrain` smooth terrain, water, and grass; skybox stars. Layered clothing renders as its raw authored mesh rather than fitted to the character.

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
- **Sun & moon:** Positioned from `Lighting.ClockTime` and `GeographicLatitude` (`sun_direction`). Sun shadows use cascaded exponential variance shadow maps (EVSM) blurred by a separable screen-aligned Gaussian driven by `ShadowSoftness` (per Roblox's GDC 2020 talk; e.g., 0.31-stud edge width at `ShadowSoftness = 0.2`).
- **Local lights:** `PointLight`, `SpotLight`, and `SurfaceLight` support `Range`, `Brightness`, `Angle`, `Face`, and shadows. High light counts are accelerated via clustered shading (`render::light_clusters`), while particles, beams, and trails sample a voxel light field (`render::voxel_field`).
- **Ambient occlusion:** Screen-space AO computed from opaque depth and normals. Static-camera video clips reuse previous-frame AO and recompute only around moving geometry.

## Sky, atmosphere, and fog

- **Skybox (`roblox_sky_direction`):** `Ft` = `-Z`, `Bk` = `+Z`, `Lf` = `+X`, `Rt` = `-X`, with `Up` and `Dn` rotated 90°, further rotated by `SkyboxOrientation`. Displayed at full resolution via the tone curve of `1.935 * texel²`.
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

## Code entry points

- `render::Renderer::render_view` and `render/mod.rs` — Top-level view rendering and pass ordering.
- `render::pipelines` — WGSL shader compilation and pipeline creation.
- `render::prepared`, `render::residency`, `render::texture_files` — GPU geometry caches, mip streaming, and disk texture caches.
