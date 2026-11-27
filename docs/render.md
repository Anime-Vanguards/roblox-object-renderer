# `ror render`: inputs, cameras, and outputs

`ror render` draws one or more PNG views of a Roblox model, place, asset ID, live Studio export, or `.luau` script.

This page is the CLI reference. For rendering rules see [rendering.md](rendering.md), for asset caching see [assets.md](assets.md), and for effect timelines see [playback.md](playback.md).

`<state dir>` refers to the active [state directory](building.md#standalone-commands-and-state) (`<checkout>/.cache` in a source checkout).

```sh
ror render path/to/Model.rbxm                                         # <state dir>/out/Model/render.png
ror render Model.rbxm --views front,iso,turntable:8 --size 1024x1024 --out out/model
ror render 1234567890                                                 # Download and render an asset ID
ror render ReplicatedStorage.Models.Character --studio "My Place"     # Read-only export from open Studio
ror render Build.luau --views iso                                     # Execute script in ree and render Workspace
```

## Supported inputs

- **Local files:** `.rbxm`, `.rbxmx`, `.rbxl`, `.rbxlx` (places render `Workspace`), and `.rbx` archives of a place or a model ([archives.md](archives.md)). Use `--select <path>` to render only one instance inside a file.
- **Asset IDs:** Bare numeric IDs or `rbxassetid://` URIs.
- **Live Studio (`--studio <name>`):** A dotted instance path exported read-only via `SerializationService` along with the place's `MaterialVariant`s. Requires a local Studio MCP bridge ([parity.md](parity.md)).
- **Luau scripts (`.luau`):** Executed in `ree`; `ror` renders what the script parents into `Workspace` ([scripts.md](scripts.md)).

## Cameras and views

| Option | Description |
|---|---|
| `--views LIST` | Comma-separated presets: `front`, `back`, `left`, `right`, `top`, `bottom`, `iso`, `default`, `turntable:N`. |
| `--yaw`, `--pitch`, `--fov` | Single custom orbit camera framing the model bounds (if no visible parts exist, e.g., an invisible-part VFX model, frames visible effects at the rendered timestamp). |
| `--camera JSON` | Explicit camera object, e.g. `{"mode":"lookAt","eye":[0,5,-10],"target":[0,2,0],"fov":40}` or `{"mode":"orbit","yaw":25,"pitch":10,"fov":35}`. Passing a JSON array renders `camera1`, `camera2`, ... from a single scene load. |
| `--focus <instance>` | Frame a specific descendant (full dotted path or trailing name like `Head`) while keeping the rest of the scene visible around it. |
| `--frame minX,minY,minZ,maxX,maxY,maxZ` | Frame an explicit world bounding box (useful with the union of `stats.framed` across a timeline to keep framing locked). |
| `--camera-from <dir>` | Reuse matching named cameras from an earlier render or `ror cost` output (with a `ror cost` dir, omitting `--views` renders all gameplay views). |

Tip: Appending `#lod=N` to a mesh URI forces LOD level `N` from that mesh file's chain.

## Image options

- `--size WxH`: output dimensions in pixels (default `1536x1536`).
- `--supersample N`: render at `N`x resolution and downsample.
- `--msaa 1|4`: multisample antialiasing level.
- `--background sky|transparent|#rrggbb`: background mode.
- `--no-bloom`: disable bloom post-processing.

## Game LODs and culling (`--lod`)

By default, `ror` renders every part and every mesh at full detail (LOD 0). Pass `--lod` to preview what a live Roblox client actually draws:

- **`--lod roblox[:QUALITY]`:** Selects each `MeshPart`'s LOD level based on FOV-scaled camera distance (`d * tan(fov/2) / tan(35°)`: LOD 0 under 250 studs, then switch points at 500, 2,000, and 3,000) and `RenderFidelity` (`Performance` uses all chain levels; `Automatic` uses high-quality levels; `Precise` uses full detail; shadows always stay full detail). Also applies graphics-quality culling for tiny or distant parts (300 studs at quality 1–3, 700 at quality 10; quality defaults to 21). See [cost.md](cost.md#levels-of-detail).
- **`--lod levels`:** Switches mesh LOD levels without distance/size culling (for A/B comparisons against full detail).

`summary.json` records per-view LOD counts under `lod` (`levels`, per-mesh `meshes`, `culled` count, and `unchained` count for `Performance` meshes lacking a stored LOD chain, which `ror` renders at full detail). `ror video`, `ror compare`, and render jobs (`"lod": "roblox:21"`) accept `--lod` as well.

## Graphics quality levels (`--quality`)

By default every view draws what a Roblox client draws at its highest graphics quality level, 21. `--quality <1-21>` (`ror render` and `ror video`; `"quality": N` in a view of a render job or a service request) draws what a client draws at a lower level, under the scene's `LightingStyle` and `PrioritizeLightingQuality` (measured in the Roblox engine 0.747 with `fixtures/quality-levels.luau`; [rendering.md](rendering.md), "Graphics quality levels"):

- the sun's shadow maps only within the level's distance of the camera, the voxel light grid's softer sun past it (none of the maps below level 8);
- local lights per pixel, with their shadows, only within the level's distance of the camera (none below level 14, or 17 under `PrioritizeLightingQuality` false: every surface takes the lights' voxels, as under `LightingStyle` Soft), the voxels past it;
- the share of each `ParticleEmitter`'s `Rate` the level makes (`max(level, 2) / 21`; `Emit` bursts in full);
- per-pixel lighting, with the sun's glint, only for parts whose cluster is within the level's distance of the view's focus (none at levels 1 to 4, 9.85 studs at 5, 34 at 6, 73 at 7, 382 at 12, 775 at 16, 935 at 17, 1,200 from 19; under `PrioritizeLightingQuality` false none to 6, 2.1 studs at 7, 98 at 12, 490 at 16). A part's cluster is the parts whose middles share its 128 x 64 x 128-stud cell of the world, measured by the box round them all; the focus is what a look-at or orbit camera looks at, else the camera itself. Past it a part is lit by vertex: its sun glint taken at its corners (a large floor shows none), no metalness, no normal maps, and the sky's light the same from every side;
- faces turned away from the sun taking 0.41 times its light at their angle (times 1 - `EnvironmentDiffuseScale`, so none under a fully diffuse sky) on parts whose cluster lies farther from the focus than the level's distance: every part at levels 1 to 5, 5 studs at 6, 19.5 at 7, 66.5 at 9, 177 at 12, 391.5 at 16, 599.5 from 19 (under `PrioritizeLightingQuality` false every part to 9, 51 studs at 13, 207.5 at 16); nearer, none. Level 21 draws it too, past 599.5 studs;
- the high-quality shading only within the level's distance of the camera (from level 7: 55 studs, 142 at 9, 310 at 12, 547 at 15, 756 at 17, everywhere from 18), fading over a quarter of that distance each way: past it metals and `Foil` reflect no sky and are drawn as any surface, nothing shows the sky's specular, and normal maps lie flat;
- the built-in glow round bright surfaces in three strengths (levels 1-5, 6-11, 12 and up, the lower two weaker and weighing a source's luminance), and no `BloomEffect` below level 12;
- ambient occlusion from level 20; no anti-aliasing below level 17 (4 samples from 17, where a client takes 2); `SunRaysEffect` and `DepthOfFieldEffect` from level 16; `BlurEffect` at every level;
- without `--lod`: each `MeshPart` at its level of detail, as `--lod` draws it, and none of the parts the client's view distance and size cull at that level leave out (490 studs at level 1, 1,600 at 4 to 12, 10,000 at 17). `summary.json`'s `lod` says `"client": true`. With `--lod roblox:N` the cull is Studio's edit quality's instead.

Level 21 with `--quality 21` differs from the default only in the levels of detail and the cull.
This build's Terrain Clouds draw one look at every level.

## Lighting, materials, and map backdrops

- `--lighting-from <place>`: borrow `Lighting` (properties, `Sky`, `Atmosphere`, post effects) from another place file. Without it, `ror` uses Roblox default lighting.
- `--materials-from <place>`: borrow `MaterialService` (`Use2022Materials` and `MaterialVariant`s); a place without one lends the pre-2022 materials, as Roblox loads it. Without it a model draws the 2022 materials and a place its own.
- `--map <file>`: render another place's `Workspace` (or a model's top-level instances) around your input inside a root `Map` folder (`Map (setting)` if your input already has a `Map`), adopting that file's lighting and materials (a map place without a `MaterialService`: the pre-2022 materials) unless `--lighting-from` / `--materials-from` override them. Orbit framing, `stats.framed`, and authoring lints still target only your input (`Scene::framing_subject`).
- `--map-origin x,y,z[,yaw]`: offset and rotate the backdrop map so `(x, y, z)` aligns with your input's origin (e.g., placing an attack effect where a character stands). Both `render` and `video` support `--map` and `--map-origin`.

## Collision view (`--collision`)

`--collision [collide|query|touch|all]` renders physics collision shapes (`scene/collision.rs`) instead of visual geometry: convex hulls (distinct colors) or bounding boxes (`Box` fidelity in warm gray) for MeshParts/unions, primitive shapes for Parts (ignoring `SpecialMesh`), and invisible parts (`Transparency = 1`), with textures and effects omitted. Defaults to `CanCollide` parts.

> Note: Decoding saved Roblox collision geometry requires a decoder not included in this public repository; public builds reject `--collision`.

## Output files and pixel IDs

By default, `ror render` writes one PNG per view plus `summary.json` (cameras, part count, scene bounds, `stats.framed`, particle/beam/trail counts, `guis` drawn and skipped with reasons, missing assets with reasons, and timings). Pass `--json` to print the summary to stdout.

Additional machine-readable outputs:
- **`--ids`** (job JSON: `"ids": true`): writes `<view>.ids.png` (24-bit part index + 1 encoded in RGB low-byte-first, `0` = background), `parts.json` (path, class, material, transparency, position), and `visible.json` (visible pixel counts per part per view). Combine with `--no-png` to run fast visibility sweeps without encoding color PNGs.
- **`--alone <instance>`** (with `--ids`; job JSON: `"alone"`): computes how much of an instance is unoccluded in each view—returning `shown` (visible pixels in the full scene) and `whole` (visible pixels when rendered alone from the same camera) in a single command.
- **`"layers": true`** (library API `ViewRequest::layers`): reads back frontmost opaque part ID/depth plus GPU-counted transparent/decal overdraw fragments per pixel (`ViewImage::layers`), used by `ror cost` ([cost-loop.md](cost-loop.md)).

## Authoring lints

Both `render` and `video` report `weightless` in `summary.json` (and stderr): active `ParticleEmitter`s with `Speed.Max > 8` studs/s, `Drag <= 0`, and `Acceleration` below `0.5` studs/s² on every axis.

```json
{ "count": 3, "flying": 21, "share": 0.143, "emitters": [{ "path": "...", "copies": 2, "speed": 40, "particles": 120 }], "line": "..." }
```

Dormant emitters and legacy effects are excluded; properties reflect playback sets/tweens at the rendered moment. Hand-authored VFX rarely leave fast particles completely weightless (~4.7% across a 57k-emitter game sample). `ror video` adds additional VFX lints ([video.md](video.md#what-the-summary-measures)).

## Inspecting and diffing renders

For text-based inspection and visual regression checks:
- **`ror inspect <out>`:** Lists parts sorted by image coverage percentage (class, material, transparency). Add `--at X,Y` to identify the part at a specific pixel. Requires a render created with `--ids`.
- **`ror diff <before> <after>`:** Compares matching views across two render directories, reporting changed pixel fraction, bounding region, parts that gained/lost pixels (when both used `--ids`), and writing `<view>.diff.png` (changed pixels highlighted in orange over grayscale).

Both commands accept `--json`.

## GUIs (`SurfaceGui`, `BillboardGui`, `ScreenGui`, `ViewportFrame`)

- **UI rendering via `rur`:** `SurfaceGui`s, `BillboardGui`s, and `ScreenGui`s are rendered by [roblox-ui-renderer](https://github.com/Anime-Vanguards/roblox-ui-renderer). `ror` locates `rur` in order: `ROR_RUR`, `RUR_HOME` (each a checkout directory or its `cli/rur.mjs`), `../roblox-ui-renderer` beside a `ror` checkout, and `rur` / `rur.mjs` on `PATH` (if `ROR_RUR` is explicitly set to a path that is not a valid `rur` checkout, `ror` warns loudly and does not silently fall through to another install); Node is located via `ROR_NODE` or `PATH`. `summary.json` records every GUI under `guis.drawn` (paths drawn in at least one view) and `guis.skipped` (`{path, reason}` for disabled, out-of-range, back-facing, missing-adornee, `ROR_NO_GUIS`, missing/invalid `rur`, or failed GUIs). Disable entirely with `ROR_NO_GUIS=1`.
- **Caching & service:** GUI renders are cached by content hash in memory and `<state dir>/guis` (per `rur` build). `ror` communicates directly with `rur`'s render service once started (`ROR_RUR_DIRECT=0` forces Node CLI calls), and video jobs prefetch animated GUI frames asynchronously ahead of GPU submission.
- **Scale:** Per-view `guiScale` controls device pixels per GUI pixel (defaults to `image_height / 744`, matching Studio's 1.5x at 1116 px height); on the command line `--size WIDTHxHEIGHT@SCALE` sets it (`--size 1920x1080@1` draws GUIs as a Roblox client at DPI scale 1 does).
- **Lighting and sampling in the world:** A `SurfaceGui`'s or `BillboardGui`'s `LightInfluence` mixes its colour at `Brightness` with the light a white surface facing it takes, as a Roblox client lights it: the sun through its shadow (0.41 of it from behind, times 1 - `EnvironmentDiffuseScale`, through the light grid's voxel sun under `GlobalShadows`, so a thicker holder keeps less of it), local lights with their shadows, `Ambient`, and `OutdoorAmbient` and the sky's light as far as the light grid lets them through (the sky's light the same whichever way the GUI faces). Its content is read as the client draws it, at the four sample positions of 4x MSAA, and a `SurfaceGui` the view magnifies is drawn at a higher scale so that its frames keep sharp edges.
- **`ViewportFrame`s:** Clipped and rounded by `UICorner`, turned with the frame's `Rotation`. Portal-style `SurfaceGui` ViewportFrames whose camera looks through the face render from the main camera's perspective (with mesh vertex alpha punching through the background). Standard ViewportFrames (in a `SurfaceGui`, `BillboardGui` or `ScreenGui`) render through their own camera (its `CurrentCamera`, else `CameraCFrame` / `CameraFieldOfView`) using their `Ambient`, `LightColor`, `LightDirection`, a `Sky` child (half its diffuse light; a Roblox client at its top graphics quality also reflects it on metals, which ror does not draw yet), `ImageColor3`, `ImageTransparency`, and `UIGradient` (its colours mixed in squared values, as a Roblox client mixes them), at the frame's proportions (at most 2048 pixels on the longer side; a `ScreenGui`'s at the frame's size in GUI pixels, stretched over it, as Studio draws it, a `SurfaceGui`'s or `BillboardGui`'s as many pixels tall as the frame shows); a `MeshPart`'s vertex alpha is the picture's alpha, so a mesh at alpha 0 hides what lies behind it in the frame's world and lets what is under the picture show there (the frame's background, or with none what is under the frame).
- **`ScreenGui`s:** `ScreenGui`s inside a `PlayerGui` (such as from a `ree` recording) and, for a place, those in its `StarterGui` (a client copies them into its `PlayerGui` once its character spawns; not with `CharacterAutoLoads` off or `--select`) are composited over the final post-processed frame in `DisplayOrder` (a tie: the later child on top), their ViewportFrames' worlds included (a full-screen frame whose camera follows the main camera draws its world over the scene), at the view's own pixels up to 4K. In a recording of play `ScreenInsets` lays them out as a client does: `CoreUISafeInsets` (the default) below the 58-pixel top bar, `TopbarSafeInsets` in the top bar's free band right of Roblox's menu and chat buttons (from 164 GUI pixels), `None` and `DeviceSafeInsets` over the whole view; none is clipped to its area. Roblox's own top bar is not drawn. `ScreenGui`s in `Workspace` do not render.
- **Names over heads:** A `Humanoid` whose model has a `Head` shows its `DisplayName` (else the model's name) and, when its `HealthDisplayType` shows one, a health bar over the head, as a Roblox client draws them: SourceSans at 24, 18 or 12 pixels by distance, over the finished image, fading out near `NameDisplayDistance`, hidden while a part stands between the camera and the head; `DisplayDistanceType` `None` shows none. The camera stands in for the client's local character (a `Viewer` humanoid shows within 100 studs).

## Not rendered

- **Terrain:** Smooth terrain, water, and grass under `Workspace.Terrain` are skipped (only `Terrain.Clouds` render).
- **Layered clothing:** `WrapLayer` and `WrapTarget` meshes render as their authored mesh rather than deformed onto the character body.
