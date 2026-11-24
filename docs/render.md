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

## Lighting, materials, and map backdrops

- `--lighting-from <place>`: borrow `Lighting` (properties, `Sky`, `Atmosphere`, post effects) from another place file. Without it, `ror` uses Roblox default lighting.
- `--materials-from <place>`: borrow `MaterialService` (`Use2022Materials` and `MaterialVariant`s). Defaults to 2022 materials.
- `--map <file>`: render another place's `Workspace` (or a model's top-level instances) around your input inside a root `Map` folder (`Map (setting)` if your input already has a `Map`), adopting that file's lighting and materials unless `--lighting-from` / `--materials-from` override them. Orbit framing, `stats.framed`, and authoring lints still target only your input (`Scene::framing_subject`).
- `--map-origin x,y,z[,yaw]`: offset and rotate the backdrop map so `(x, y, z)` aligns with your input's origin (e.g., placing an attack effect where a character stands). Both `render` and `video` support `--map` and `--map-origin`.

## Collision view (`--collision`)

`--collision [collide|query|touch|all]` renders physics collision shapes (`scene/collision.rs`) instead of visual geometry: convex hulls (distinct colors) or bounding boxes (`Box` fidelity in warm gray) for MeshParts/unions, primitive shapes for Parts (ignoring `SpecialMesh`), and invisible parts (`Transparency = 1`), with textures and effects omitted. Defaults to `CanCollide` parts.

> Note: Decoding saved Roblox collision geometry requires a decoder not included in this public repository; public builds reject `--collision`.

## Output files and pixel IDs

By default, `ror render` writes one PNG per view plus `summary.json` (cameras, part count, scene bounds, `stats.framed`, particle/beam/trail counts, missing assets with reasons, and timings). Pass `--json` to print the summary to stdout.

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

- **UI rendering via `rur`:** `SurfaceGui`s, `BillboardGui`s, and `ScreenGui`s are rendered by [roblox-ui-renderer](https://github.com/Anime-Vanguards/roblox-ui-renderer). `ror` locates `rur` via `ROR_RUR` or `../roblox-ui-renderer`, and Node via `ROR_NODE` or `PATH`. If unavailable, GUIs are skipped with a warning. Disable entirely with `ROR_NO_GUIS=1`.
- **Caching & service:** GUI renders are cached by content hash in memory and `<state dir>/guis` (per `rur` build). `ror` communicates directly with `rur`'s render service once started (`ROR_RUR_DIRECT=0` forces Node CLI calls), and video jobs prefetch animated GUI frames asynchronously ahead of GPU submission.
- **Scale:** Per-view `guiScale` controls device pixels per GUI pixel (defaults to `image_height / 744`, matching Studio's 1.5x at 1116 px height).
- **`ViewportFrame`s:** Clipped and rounded by `UICorner`. Portal-style `SurfaceGui` ViewportFrames whose camera looks through the face render from the main camera's perspective (with mesh vertex alpha punching through the background). Standard ViewportFrames (in a `SurfaceGui`, `BillboardGui` or `ScreenGui`) render through their own camera (its `CurrentCamera`, else `CameraCFrame` / `CameraFieldOfView`) using their `Ambient`, `LightColor`, `LightDirection`, `ImageColor3`, `ImageTransparency`, and `UIGradient`, at the frame's proportions (at most 2048 pixels on the longer side; a `ScreenGui`'s at the frame's size in GUI pixels, stretched over it, as Studio draws it, a `SurfaceGui`'s or `BillboardGui`'s as many pixels tall as the frame shows); a `MeshPart`'s vertex alpha is the picture's alpha, so a mesh at alpha 0 hides what lies behind it in the frame's world and lets what is under the picture show there (the frame's background, or with none what is under the frame).
- **`ScreenGui`s:** Only `ScreenGui`s inside a `PlayerGui` (such as from a `ree` recording) are composited over the final post-processed frame in `DisplayOrder` (respecting top-bar inset unless ignored), their ViewportFrames' worlds included (a full-screen frame whose camera follows the main camera draws its world over the scene). `ScreenGui`s in `StarterGui` or `Workspace` do not render.

## Not rendered

- **Terrain:** Smooth terrain, water, and grass under `Workspace.Terrain` are skipped (only `Terrain.Clouds` render).
- **Layered clothing:** `WrapLayer` and `WrapTarget` meshes render as their authored mesh rather than deformed onto the character body.
