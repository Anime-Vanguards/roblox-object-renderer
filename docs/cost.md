# Scene rendering cost (`ror cost`)

`ror cost` estimates how many draw calls and triangles Roblox's renderer submits for a scene from given camera views, simulates Roblox's CPU occlusion culling, and identifies the biggest optimization opportunities.

Implementation lives in `crates/ror/src/cost/` (`mod.rs`, `cull.rs`, `meshes.rs`, `studio.rs`) and `crates/ror/src/lod.rs`. All batching, LOD, and occlusion rules were measured directly against Roblox Studio's internal counters using the probe scripts in `tools/cost-probes/` and `tools/occlusion-probes/` (`tools/cost_probes.py`, `tools/occlusion_probes.py`).

For the full optimization workflow (phone frame-time attribution, testing edits in memory, visual diffing, and exporting Lune patch scripts), see [cost-loop.md](cost-loop.md).

## Common commands

```sh
ror cost Map.rbxm --sweep 5                      # 5 densest spots, 4 directions each at eye height
ror cost Map.rbxm --paths 4                      # Gameplay views along 4 path points (near & far, 4 directions)
ror cost Place.rbxl --cameras cameras.json       # Custom cameras: {"name", "eye", "target", "fov"} or {"name", "cframe", "fov"}
ror cost Map.rbxm --grid 60                      # Top-down heatmap sampling every 60 studs
ror cost After.rbxm --camera-from out/before --compare out/before   # Before/after comparison on identical cameras
ror cost Map.rbxm --paths 4 --check --profile    # Compare against Studio counters + MicroProfiler timings
```

## Key flags

| Flag | Description |
|---|---|
| `--sweep N` | Sample the `N` densest locations in the scene, looking in 4 directions from eye height. |
| `--paths N` | Generate near and far gameplay cameras along `N` points of a map's enemy paths (parts under outermost folders named `Path(s)`, `Node(s)`, or `Waypoint(s)`). |
| `--cameras FILE` | Load named cameras from a JSON array (`eye`/`target`/`fov` or `cframe`/`fov`). |
| `--grid STUDS` | Sample far gameplay cameras every `STUDS` studs (20 works well for a match map) across the path bounding box + 40 studs padding, and paint top-down heatmaps (`grid.draws.png`, `grid.triangles.png`, `grid.occlusion.png`, `grid.json`). Optional `--region X0,Z0,X1,Z1[,Y]`. Grid views are not rendered as individual stills. |
| `--camera-from DIR` | Reuse the exact camera set from a previous run. |
| `--compare DIR` | Print per-view draw, triangle, and occlusion deltas against a previous run. |
| `--no-occlusion` | Count frustum culling only (upper bound on what Roblox can draw). |
| `--no-images` / `--no-pixels` | `--no-images` skips writing per-view PNGs; `--no-pixels` skips pixel attribution rendering. |
| `--top N` | Number of top batches, occluders, missed occluders, and tested groups included in `summary.json` (default: 24; `cost.json` always contains all). |
| `--items` | Write `items.json`: every drawable item (path, part index, mesh, pass, triangles per LOD, bounds) and per-view draw/LOD/occlusion status. |
| `--technology TECH` | Override `Lighting.Technology` (affects transparent Glass and character costs). |
| `--quality N` / `--lod roblox:N` | Set Roblox Studio edit graphics quality level (1–21, default 21), affecting LODs and distance/size culling. With `--check`, forces capture Studio to count with distance-based LODs instead of full detail. |
| `--client-quality N` / `--lod client:N` | Set the Roblox client 10-bar graphics slider (`SavedQualityLevel` 1–10), mapped via Roblox's settings-script formula (`floor(21 * bar / 10)` with bar 1 held at 1, not directly measured in Studio or client) to engine levels `[1, 4, 6, 8, 10, 12, 14, 16, 18, 21]` (bar 1 = 300 studs, bar 2 = engine 4 / 450 studs). Feature switches (`sunShadows >= 12`, `perPixelLights >= 16`, `bloom >= 6`) still come from Studio edit measurements (open for client bars 3–5 where client sweeps saw shadow GPU work). `analysis.json` records `quality.scale`, `quality.clientLevel`, `quality.mappingBasis`, and `quality.viewDistance`. |
| `--full-detail` | Keep all meshes at LOD 0 (matching Studio's default `MeshPartDetailLevel`), while keeping distance/size culling active. |
| `--check` | Load the scene into capture Studio (`ror studio open`, pass `--technology` to match) and print Studio's real counters beside `ror`'s estimates (`cost-check.json`). Requires a Studio MCP bridge ([parity.md](parity.md)). |
| `--profile` | With `--check`, pull render-job CPU timings (`queryOcclusion`, render-thread wait, render-thread work) from Studio's MicroProfiler via LibMP. |
| `--preset phone\|desktop`, `--edits`, `--look`, `--export`, `--explain`, `--budget`, `--heat` | Cost-loop flags for preset frame attribution (phone calibrated ms by default and on `--preset phone`; `--preset desktop` uses estimated reference-PC CPU ms + uncalibrated GPU refused as `null` with `gpuUnits`), what-if edits, and Lune export; see [cost-loop.md](cost-loop.md). Note: commit `871a83e` (2026-11-24 21:15) temporarily applied desktop CPU-only pricing (`gpuMs: 0`) to runs without `--preset`; the follow-up fix restored phone-frame pricing for runs without `--preset` and reports uncalibrated desktop GPU fields as `null`. |

## Output files

Each run prints a summary covering per-view pass breakdowns (`Opaque`, `Transparent`, `Particles`, `UI`, `Sky`, `Cloud`, `GenericPostProcess`, plus character `Shadows` listed separately outside totals per Roblox guidance), culling stats, draw-call categories, heaviest batches, and ranked optimization gains (weighted at 1 draw call = 20,000 triangles).

Files written to `--out`:
- `cost.json`: full per-view passes and batches with instance paths (compact JSON when a grid exceeds 64 views).
- `summary.json`: summary report with the top `--top` batches, largest occluders, missed occluders, and tested groups. (Also available via render jobs with `"cost": {...}`.)
- `<view>.png`: rendered view.
- `<view>.draws.png`: false-color image where every draw call gets a distinct color.
- `<view>.heat.png`: instancing efficiency heatmap (red = 1 copy/draw, yellow = 10 copies/draw, blue = 100+ copies/draw, gray = culled).
- `analysis.txt`, `analysis.json`, `tree.json`: per-instance attribution (`prices`, `fidelity`, `unmodeled`) and ranked findings ([cost-loop.md](cost-loop.md)).

## How Roblox counts draws and triangles

Roblox exposes renderer statistics directly to scripts in both edit and play mode:
- `Stats.SceneDrawcallCount` and `Stats.SceneTriangleCount` (all passes from the previous frame)
- `Stats.ShadowsDrawcallCount`
- Shift+F2 render stats
- `SceneAnalysisService:GetTriangleCompositionAsync()` (per pass: `Opaque`, `Transparent`, `Particles`, `UI`, `Sky`, `Cloud`, `GenericPostProcess`, `Shadows`)

`ror cost --check` reads these counters directly from capture Studio. Following Roblox's performance guidance, `ror cost` weights 1 draw call at 20,000 triangles and excludes `Shadows` from draw/triangle totals.

## Batching rules

Roblox batches static world geometry so that all copies sharing the same geometry and appearance submit as a single draw call, regardless of count or position.

**Merged into one draw call:**
- Parts differing only in `Color`, `Size`, rotation, `Reflectance`, `DoubleSided`, `Anchored`, `CastShadow`, surface types, or non-zero transparency value (tested up to 6,000 meshes in a single draw call with no batch cap).
- Copies sharing identical `SurfaceAppearance` maps across separate `SurfaceAppearance` instances (when `SurfaceAppearance` is present, `Material` and `TextureID` no longer split batches; a different normal map does).

**Split into separate draw calls:**
- Different geometry (primitive shape, `MeshId`, `SpecialMesh` mesh, or distinct union; identical copies of one union do batch).
- Different appearance (`Material`, `MaterialVariant`, `MeshPart.TextureID`, or `SurfaceAppearance` maps).
- Opaque (`Transparency == 0`) vs. transparent (`Transparency > 0`).
- Different LOD levels.
- Distance bands from the camera: `< 600` studs, `600–1,200` studs, and `> 1,200` studs.
- `SpecialMesh` (`FileMesh`) vs. `MeshPart` referencing the same mesh asset.
- Duplicate uploads of the exact same mesh under different asset IDs (`ror cost` detects these via full-detail geometry shape under `duplicateMeshes`).

**Drawn individually (never batched):**
- `ForceField` parts, transparent parts with decals, `ParticleEmitter`s, `Beam`s (even when invisible), `Trail`s, and `Humanoid` character clusters and their transparent parts.
- Decals batch per image and face.
- Each `SelectionBox` submits 12 draw calls.

**Primitive triangle counts:** Block `12`, Wedge / CornerWedge `10`, Cylinder `96`, Ball `432`, SpecialMesh Sphere `432`, Head `846`. MeshParts use the triangle count of their active LOD level.

## Levels of detail

Measured in Studio 0.742 with `Workspace.MeshStreamingAndImprovedLods = Disabled` and `MeshPartDetailLevel = DistanceBased` (`crate::lod`):

- **Switch distances:** Computed from straight-line distance `d` from the camera to the part's bounding box (`0` if inside), scaled by field of view: `d * tan(fov / 2) / tan(35°)`.
  - `< 250`: LOD 0 (full detail)
  - `< 500`, `< 2,000`, `< 3,000`: subsequent LOD levels in the mesh file
  - `>= 3,000`: LOD 4 (fifth level)
  - No hysteresis; switches occur one frame late.
- **RenderFidelity behavior:**
  - `Performance`: uses all LOD levels in the mesh file. If a `Performance` mesh has no stored LOD chain, Roblox generates levels at runtime; `ror` cannot generate them, draws full detail, and reports them as unchained.
  - `Automatic`: uses only the file's high-quality LOD levels.
  - `Precise` (and all `SpecialMesh`es): always full detail.
  - Shadows always render at full detail.
- **Quality culling:** Graphics quality culls parts that are too small on screen or beyond the quality view distance (300 studs at quality 1–3, 700 studs at quality 10).
- **LOD recommendations:** `ror cost` flags meshes where switching to `Performance` saves more triangles than the extra draw calls introduced when copies straddle a LOD switch distance (weighted at 20,000 triangles per draw). Typically far scenery (e.g., mountains) benefits while nearby buildings do not.

## Occlusion

Roblox runs a software occlusion rasterizer on CPU worker threads. It rasterizes front-facing triangles of qualifying occluders into a depth buffer, then tests each object's 2D screen-space bounding box per instance. A batched draw call is eliminated only when every copy in the batch is hidden. Because Roblox adapts occlusion workload to host CPU timing, individual views can vary by tens of percent while multi-view averages stay consistent.

**Counts as an occluder:**
- Opaque parts (`Transparency == 0`, any material, any thickness): Blocks, Wedges, Cylinders, and Balls.
- Opaque `MeshPart`s with at most ~2,048 distinct vertex positions, provided they are large enough on screen for their vertex density (~0.67 positions per pixel of screen bounding box at 256 px width). Only camera-facing triangles are rasterized.

**Does NOT occlude:**
- Unions, Trusses, `SpecialMesh`es, `Humanoid` models, skinned meshes, `SurfaceAppearance` with `AlphaMode = Transparency`, any part with `Transparency > 0`, any part containing the camera, and parts smaller than ~2 pixels on a 256-pixel-wide view.

`ror cost` runs an equivalent software rasterizer (`cost::cull`), estimates `queryOcclusion` CPU time (fitted to Studio's MicroProfiler; median view within 19%), and reports:
- Top occluders (what hides the most geometry).
- Most expensive mesh occluders (their triangles drive most occlusion CPU cost) vs. how much they actually hide.
- Mesh occluders that hide nothing.
- Missed occluders (large solid parts that cannot occlude, ranked by how much a proxy block inside them would hide).

## Accuracy

Across 472 views of 10 game maps, `ror cost` matched Studio's total draw calls within **0.3%** overall, with a median per-view error within **4%**.

Known caveats:
- Individual views can diverge due to Roblox's adaptive CPU occlusion budget; use `--no-occlusion` for the frustum-only ceiling.
- Decals and Textures on meshes are counted by assigning each full-detail triangle to its single dominant box face (matching the renderer's `box_face` projection rule) and skipping faces with zero assigned triangles on loaded meshes. Set `ROR_COST_FACING_MIN` to override with a cosine threshold.
