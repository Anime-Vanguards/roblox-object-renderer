# The cost optimization loop

`ror cost` breaks down every view's rendering cost down to individual Roblox instances, estimates the impact in phone frame milliseconds, suggests ranked edits, tests those edits in memory with visual diffs, and exports accepted changes as Lune scripts for your `.rbxl` place.

All JSON outputs include a schema version and use deterministic sorting and rounding so identical inputs produce byte-identical files and clean diffs.

Implementation: `crates/ror/src/cost/` (`agent.rs`, `attribute.rs`, `pixels.rs`, `findings.rs`, `edits.rs`, `prices.rs`, `ids.rs`).

## End-to-end workflow

```sh
# 1. Measure all views for a phone target
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map
# Writes: out/map/analysis.txt (start here), analysis.json, tree.json

# 2. Inspect a specific expensive instance across every view
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map \
  --explain "Workspace.Map.Backdrop"

# 3. Test candidate edits in memory and check visual impact
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map \
  --edits trials.json --look
# Writes: out/map/whatif.txt, whatif.json, look/<trial>/<view>.crop1.png ...

# 4. Export accepted edits and apply them to a copy of the place
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map \
  --edits keep.json --export out/map/kit
lune run out/map/kit/edits/apply.luau Map.rbxl Map-cut.rbxl out/map/kit/edits/changes.json

# 5. Verify the patched place against a budget
ror cost Map-cut.rbxl --cameras cameras.json --preset phone --no-images --out out/map-cut \
  --budget phone --fail-over-budget
```

Runs execute through the warm render service, rendering each view at 1/3 phone resolution for pixel attribution. Evaluating 16 views of a large map takes a few seconds, each what-if trial takes roughly the same time, and `--look` adds two full-resolution renders per view.

## Choosing cameras

Always evaluate a representative set of player viewpoints rather than a single angle; the report tracks both the mean across views and the worst view.

| Option | Behavior |
|---|---|
| `--cameras FILE` | JSON array of named cameras: `[{"name", "eye": [x,y,z], "target": [x,y,z], "fov"}]` (or `"cframe"`). Best for walkable places where you know player vantage points. |
| `--paths N` | Gameplay cameras along `N` points of a map's enemy paths (near and far, 4 directions). |
| `--sweep N` | Eye-height cameras at the `N` densest spots (4 directions). |
| `--camera-from DIR` | Reuse the exact cameras from an earlier run. |

Note: `--grid STUDS` only generates top-down maps ([cost.md](cost.md)) and does not run the attribution loop.

## Target presets

- `--preset phone`: 2532 x 1170 px (844 x 390 pt at 3x) at Studio edit graphics quality **2** (300-stud culling horizon; pass `--client-quality 2` for the Roblox client 10-bar slider bar 2, which maps to engine quality 4 / 450 studs with shadows and bloom still off).
- `--preset desktop`: 1920 x 1080 px at graphics quality **21**.
- Without `--preset`, `ror` uses `--size` and `--quality` (or `--client-quality <1-10>`) and prices the frame with the calibrated phone model (the same default as before `871a83e`, so baselines and GPU-led finding rankings stay unchanged unless `--preset desktop` is asked for). `analysis.json` records `quality.scale` (`"edit"` or `"client"`), `quality.clientLevel` (when client-scale), and `quality.viewDistance`.

What Roblox renders at each graphics quality level under `Future` lighting (measured in Studio):

| Quality | Sun shadows | Local lights | Bloom |
|---|---|---|---|
| 1–5 | None | Voxel light grid (soft, no shadows, no per-pixel cost) | None |
| 6–8 | None | Voxel light grid | Faint |
| 10 | Short reach only | Voxel light grid | Faint |
| 12–14 | Soft | Voxel light grid | Full |
| 16–21 | Full | Per-pixel with shadows | Full |

The cost model enables sun shadows at quality 12+, per-pixel local lights at 16+, and bloom at 6+. It also applies quality-based view distance culling (300 studs at quality 1–3) and small-part culling ([cost.md](cost.md)), and caps phone particle emission at an estimated 100 particles/s per emitter.

Only `--preset desktop` applies desktop pricing: it estimates CPU prices by scaling the phone fit by one reference-PC main-thread ratio (`1 / 2.224`, `fidelity.cpuCalibrated: false`, `fidelity.cpuBasis: "estimated"`), refuses desktop GPU milliseconds (`gpuMs: null`, `prices` `gpuUnit`/`gpuToFrame` `value: null`, `fidelity.gpuCalibrated: false`), reports shader and overdraw work in `gpuUnits`, and ranks findings by combined CPU + `gpuUnits` share so GPU findings are not hidden behind smaller CPU savings. `analysis.json` includes `fidelity` and `unmodeled` sections on every preset so callers know which costs are calibrated versus unmodeled and what measurement settles each gap.

## Cost attribution

Every unit of work in each view is billed back to the instance responsible (`attribute.rs`):

| Cost category | How it is measured | Billed to |
|---|---|---|
| Draw calls | Roblox batching rules; batch cost is split evenly across copies in the batch | Each copy |
| Triangles | Triangles drawn at each copy's active LOD | The copy |
| Parts | Every part in the place, visible or culled (culling and render queues touch all parts) | The part |
| Opaque pixels | Frontmost opaque pixels owned by the part (from an ID+depth pass), weighted by shader class (`plain`, `textured`, `Neon`, `Glass`) | The part |
| Blended pixels | Transparent part/decal fragments in front of opaque depth (GPU-counted), plus rasterized particle, beam, and trail fragments | The part or effect |
| Local light pixels | Opaque pixels inside a light's range and cone (`lightPixels`) | The light (surfaces report `litPixels` for reference without double-billing) |
| Shadow triangles | When sun shadows are active, full-detail triangles of casters within 400 studs | The caster |
| Environment & post FX | Sky pixels and each enabled full-screen effect pass (`Sky`, `Clouds`, `Atmosphere`, post effects) | The `Lighting` child (or `(shared passes)` if lent via `--lighting-from`) |
| Legacy effects | Particle draws and fill from `Fire`, `Smoke`, `Sparkles`, and `Explosion` | The effect instance |

`tree.json` rolls costs up through parent models and folders so you can drill down from the top of the hierarchy. Raw counts (draws, triangles, blended pixels) are kept alongside milliseconds so findings can be cross-checked with `ror cost --check`.

## Pricing model

All unit prices are recorded in `analysis.json` under `prices`:

| Metric | Price | Basis |
|---|---|---|
| 1 draw call | 15.8 us phone frame | Fitted to live phone measurements adding/removing parts, draws, and triangles in a large place |
| 1 part in place | 0.26 us | Same phone fit |
| 1,000 triangles drawn | 2.2 us | Same phone fit (phones are rarely triangle-bound) |
| 1 phone GPU ms | 0.195 ms frame time | Measured fraction of GPU time that blocks the frame |
| 1 full viewport of opaque Plastic | 0.42 phone GPU ms | Anchored to GPU time dropped when hiding the map in the reference place |
| Per-pixel shader weights | Textured `1.4`, Neon `0.8`, Glass `1.2`, Sky `0.3`, Blended `1.6` (`2.0` for alpha `SurfaceAppearance`), Particle `1.3`, Local light `0.25`, Sun shadow `0.2` | Estimated relative shader cost (not measured per shader on device) |
| Shadow map redraws | 25% of caster triangles per frame | Estimated (Roblox caches shadow maps and redraws incrementally as the camera moves) |

Because GPU shader weights are estimates, treat small differences (under ~0.05 ms per view) as noise.

## Output files and schemas

### `analysis.json` (`ror-cost-analysis/1`)

Top-level fields: `preset`, `quality` (active features), `prices`, `pixelsCounted`, `views` (per view: `name`, Roblox `draws`/`triangles` excluding Shadows, `work`, `overBudget`), `mean`, `worst`, `topInstances`, and `findings`.

Every `work` object (in `analysis.json` and `tree.json`) contains: `draws`, `triangles`, `pixels`, `blended` (at preset resolution), `lightPixels`, `litPixels`, `shadowTriangles`, `parts`, `cpuMs`, `gpuMs`, and `ms` (`cpuMs + gpuMs share`).

### `tree.json` (`ror-cost-tree/1`)

Contains `nodes` sorted by `mean.ms`—every instance with non-zero cost in any view plus all its ancestors. Each node holds `id`, `uid`, `class`, `name`, `parent`, `mean` (self + descendants), `worstView`, `worst`, `views`, and `leaves`. Unowned full-screen passes (particle pass, built-in bloom) appear under `(shared passes)`.

### Instance IDs

Instance IDs are dotted paths from the file root. Duplicate sibling names are 1-indexed (e.g., `Workspace.Map.Rock[3]`, `Workspace.Map["Rock.001"][2]`). IDs are deterministic across reads of the same file and stay fixed during a trial (removing an instance never renumbers siblings). `uid` holds Roblox's `UniqueId` when present.

### Findings

Each finding includes: `id`, `rank`, `kind`, `title`, `targets` (sample) and `targetCount`, `why`, `saving` (`meanMs`, `worstMs`, `worstView`, draws/triangles/blended saved, per-view `views`), `edit`, `look` (`none` = identical pixels, `unseen` = invisible in all given cameras, `changes` = visually different, check with `--look`), `basis` (`attribution` or `batches`), and `note`.

| Kind | What it detects | Suggested edit |
|---|---|---|
| `unseen` | Parts never visible in any camera at quality 21 (no opaque pixel, no blended fragment), grouped to the highest fully unseen model | `hide` |
| `seeThrough` | Transparent parts and alpha `SurfaceAppearance` meshes ranked by blended pixel overdraw | `hide` |
| `appearance` | Same geometry using multiple appearances (splitting batches) | Unify to most common appearance |
| `duplicateMesh` | Identical mesh uploaded under multiple asset IDs | Point all to most common `MeshId` |
| `levelOfDetail` | Meshes where `RenderFidelity = Performance` saves more triangles than the extra LOD batches cost | Set `RenderFidelity = Performance` |
| `merge` | Multiple meshes sharing one appearance under a single model | `merge` into one draw |
| `emitterFill` | Particle emitters ranked by blended pixel fill | Halve `Rate` (under quality cap) |
| `invisibleEffect` | Invisible beams/emitters still submitting draws | `Enabled = false` |
| `lightRange` | Per-pixel local lights ranked by lit pixels (at quality 16+) | `Enabled = false` |
| `lightReachesNothing` | Local lights that hit no visible pixel in any view | `Enabled = false` |
| `castShadow` | Small shadow casters (< 1 stud across) when sun shadows are active | `CastShadow = false` |

> [!NOTE]
> `unseen` checks visibility at quality 21 without preset distance culling so you don't delete parts that are hidden on mobile but visible on desktop. Still, make sure your camera set covers all playable areas—and test with `--edits`, because hiding an occluder can reveal geometry behind it and increase draw calls.

## Testing edits (`--edits` and `--look`)

Pass a JSON file (`ror-edits/1`) containing one or more trials:

```json
{
  "schema": "ror-edits/1",
  "trials": [
    { "name": "fog off", "edits": [{ "op": "hide", "targets": ["mesh:1234567890"] }] },
    { "name": "lamps", "edits": [{ "op": "set", "targets": ["under:Workspace.Map.Lamps"], "classes": ["PointLight"], "properties": { "Shadows": false, "Range": 12 } }] },
    { "name": "rocks", "edits": [{ "op": "appearance", "targets": ["Workspace.Map.Rock[2]"], "from": "Workspace.Map.Rock[1]" }] }
  ]
}
```

(A file with a top-level `"edits"` array instead of `"trials"` runs as a single trial named `"edits"`.)

**Supported operations:**
- `remove`: deletes target instances and their descendants.
- `hide`: sets `Transparency = 1` and `CastShadow = false` on parts (and `Transparency = 1` on their decals) while keeping collision, joints, scripts, effects, and lights intact.
- `set`: sets properties by name (`Transparency`, `CastShadow`, `Material`, `MaterialVariant`, `Color`, `RenderFidelity`, `MeshId`, `TextureID`, `Enabled`, `Range`, `Brightness`, `Shadows`, `Rate`, etc.). Values accept numbers, booleans, strings, enum names, `[r, g, b]` (0–1), or `"#rrggbb"`. Setting `MeshId` or `TextureId` on a primitive `Part` forwards to its child `SpecialMesh`. Multiple sets to the same property collapse into one change.
- `appearance`: copies `Material`, `MaterialVariant`, `MeshPart.TextureID`, and `SurfaceAppearance` from `from` (preserving `Color`).
- `merge`: models target parts as one combined mesh per appearance for costing (actually merging geometry requires an external mesh tool).

**Target selectors:** exact instance IDs, `mesh:<asset>` (all MeshParts/FileMeshes using that asset), or `under:<id>` (all descendants). Add `"classes": [...]` to filter by class.

Edits to the place's own `Lighting` (`GlobalShadows`, `Technology`, post effects) and `Terrain.Clouds` apply during trials; environments lent via `--lighting-from` or `--materials-from` stay fixed.

### `whatif.json` (`ror-cost-whatif/1`)

Records each trial's `edits`, `changes` count, `mean` (`before`, `after`, `delta`), `worstSaving`, `worstLook`, `changedInstances`, and per-view `before`/`after`/`delta` draws and triangles.

With `--look`, `ror` renders before/after comparisons at **quality 21 and full mesh detail** (so any visual change visible on desktop is caught) and adds a `look` object per view:
- `changed`: fraction of pixels differing by more than 8/255 in any channel.
- `max`: maximum channel delta.
- `crops`: paths to the 3 most-changed regions (`before | after | diff in red`) in `look/<trial>/`, alongside full `before.png` and `after.png`.

## Other cost-loop flags

- **`--explain ID`:** Prints and writes `explain-<id>.json` detailing per-view work, counts of drawn/occluded/culled parts, batch membership, and the most expensive descendants under `ID`.
- **`--export DIR`:** Writes a self-contained patch kit per trial under `DIR/<trial>/`:
  - `changes.json` (`ror-cost-changes/1`, with unique keys, ops, IDs, paths, classes, properties, before/after values, and predicted savings)
  - `changes.tsv`
  - `apply.luau` and `revert.luau` (requires Lune 0.10.5+)
  - `README.md`
  `apply.luau` validates every target path before modifying anything, moves removed instances to `ServerStorage.RorCostRemoved`, and tags edited instances with `RorCostKeys` so `revert.luau` can undo individual keys or everything. Merge ops are listed in the manifest but not auto-built.
- **`--budget SPEC` / `--fail-over-budget`:** Flags views exceeding thresholds (exits with status `2` when `--fail-over-budget` is set). Presets:
  - `phone`: 600 draws, 800,000 triangles, 10 ms frame time.
  - `desktop`: double the phone limits.
  - Custom: `draws=N,triangles=N,ms=N` (supports suffixes like `500k`).
- **`--heat`:** Writes diagnostic heatmaps over each view (for visual inspection; use `analysis.json` for automation):
  - `<view>.heat-cost.png`: GPU cost per pixel in viewport-units (log scale, green `1` to red `16`).
  - `<view>.heat-overdraw.png`: blended overdraw layers (log scale, `1` to `16+`).
  - `<view>.heat-lights.png`: overlapping local lights per pixel (`1` to `8+`).
  - `<view>.heat-density.png`: triangles per pixel (`0.001` to `1`, log scale).

## Model limits

- Local lights only incur per-pixel cost at quality 16+; voxel light-grid CPU overhead is not modeled.
- Transparent parts use fixed shading weights; local lighting on transparent surfaces is not counted.
- `merge` reduces draw calls only; triangles and pixel fill remain unchanged.
- Pixel attribution defaults to 1/3 resolution on phone (1/2 on desktop), so sub-3-pixel details may be missed. Pass `--pixel-scale 1` for full-resolution pixel attribution and heatmaps (slower).
- `ror cost --check` validates draw and triangle counts against Studio; millisecond estimates remain model outputs.
