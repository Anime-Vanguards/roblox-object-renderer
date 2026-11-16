# The cost loop: finding what to cut in a map

`ror cost` attributes everything a view costs Roblox to draw to the instances that cause it, prices
it in milliseconds of an average phone's frame, ranks what would save most with the edit that would
do it, tries edits in memory against the same views, checks what they do to the picture, and writes
accepted edits as a change list with Lune scripts for the real place. Everything it writes is JSON
with a schema version, sorted and rounded so that the same input gives the same bytes and two runs
diff cleanly. The code is `crates/ror/src/cost/agent.rs` (the loop), `attribute.rs` (who pays for
what), `pixels.rs` (each view's pixels), `findings.rs`, `edits.rs`, `prices.rs` and `ids.rs`.

## The loop

```sh
# 1. measure: every view of a camera set, for a phone
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map
#    -> out/map/analysis.txt (read this first), analysis.json, tree.json

# 2. drill into what costs: one instance view by view
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map --explain "Workspace.Map.Backdrop"

# 3. try edits: copy findings' edits into an edits file (several trials at once), cost and look
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map --edits trials.json --look
#    -> out/map/whatif.txt, whatif.json, look/<trial>/<view>.crop1.png ...

# 4. export what you keep, apply it to a saved copy of the place, revert key by key
ror cost Map.rbxl --cameras cameras.json --preset phone --no-images --out out/map --edits keep.json --export out/map/kit
lune run out/map/kit/edits/apply.luau Map.rbxl Map-cut.rbxl out/map/kit/edits/changes.json

# 5. check it: the applied place costs what the what-if said
ror cost Map-cut.rbxl --cameras cameras.json --preset phone --no-images --out out/map-cut --budget phone --fail-over-budget
```

Each run costs every view in the warm render service: the place read, every view counted and drawn
once at a third of the phone's resolution for its pixels. A large map's 16 views take a few seconds,
each trial about as long again, and `--look` adds two full renders per view.

## Camera sets

Never judge one view: the analysis reports the mean over the views and the worst one. Use the
cameras players see the map from:

- `--cameras FILE`: named cameras, `[{"name", "eye": [x, y, z], "target": [x, y, z], "fov"}]` or a
  game camera's `cframe`; for a place where players walk, cameras at the places they stand, looking
  the ways they look.
- `--paths N`: gameplay cameras along a match map's enemy paths, near and far, four ways.
- `--sweep N`: the densest spots, four ways from eye height.
- `--grid STUDS` makes no analysis: it paints each cell's draws and triangles from above
  ([cost.md](cost.md)); the loop's flags need one of the others.
- `--camera-from DIR`: an earlier run's views.

## Presets

`--preset phone` counts for a phone: 2532 x 1170 pixels (844 x 390 points at 3x) at graphics quality
2. `--preset desktop` counts 1920 x 1080 at quality 21. Without a preset the run's `--size` and
`--quality` are used. What a quality draws, measured in Studio under Future at each level:

| quality | sun shadows | local lights | bloom |
|---|---|---|---|
| 1-5 | none | the voxel light grid: soft, no shadows, no per-pixel cost | none |
| 6-8 | none | voxel | faint |
| 10 | a short reach only | voxel | faint |
| 12-14 | soft | voxel | full |
| 16-21 | full | per pixel, with their shadows | full |

The model counts sun shadows from 12, per-pixel lights from 16 and bloom from 6. The quality also
culls parts past its view distance (300 studs at 1-3) and the smallest on screen (measured,
[cost.md](cost.md)), and a phone makes at most 100 particles a second per emitter (estimated). Prices are a phone's at every
preset: the desktop preset shows what desktop quality draws, priced the same way, which ranks it but
does not time a desktop.

## What is attributed

Every view's work, charged to the instance that causes it (`attribute.rs`):

| work | how it is counted | charged to |
|---|---|---|
| draw calls | Roblox's batching as `ror cost` counts it; a batch's draws shared among the copies it draws | each copy |
| triangles | each copy at its level of detail | the copy |
| parts | every part in the place, drawn or not (culling and the render queue walk them all) | the part |
| pixels | the opaque pixels a part is in front at (a render of the view's opaque parts' ids and depth), by how it shades: plain, textured, Neon, Glass | the part |
| blended | every fragment of a see-through part or decal in front of the opaque surface (counted on the GPU), and every particle, beam and trail fragment (rasterized against the view's depth) | the part or effect |
| light pixels | each opaque pixel inside a local light's range and cone (`lightPixels`) | the light; the surface lists them as `litPixels`, not charged |
| shadow triangles | where the quality draws sun shadows, each caster within 400 studs at full detail | the caster |
| the sky, clouds, Atmosphere, post effects | the sky's pixels, and each enabled effect's full-screen pass | its instance in Lighting (`(shared passes)` when `--lighting-from` lends another file's) |
| legacy effects (Fire, Smoke, Sparkles, Explosion) | their particles' draws and fill | the Fire, Smoke or Sparkles instance |

The tree (`tree.json`) sums each instance's work and its descendants', so a map's folders, models
and parts can be read from the top down. Draw calls, triangles and blended pixels are also given as
counts, so a finding can be checked against Roblox's own counters (`ror cost --check`).

## Prices

Every price says how it was found (`analysis.json` lists them as `prices`):

| price | value | basis |
|---|---|---|
| a draw call | 15.8 us of phone frame | fitted to phone measurements of a large place, where parts, draws and triangles were added and removed and the frame timed |
| a part in the place | 0.26 us | the same fit |
| 1,000 triangles drawn | 2.2 us | the same fit (phones rarely wait for their GPU: triangles barely move the frame) |
| a phone GPU ms in the frame | 0.195 ms | measured: the share of the GPU's time the frame waits for |
| a viewport of opaque Plastic | 0.42 phone GPU ms | anchored: the reference place's views average the GPU time the live game loses when its map is hidden |
| per-pixel weights | textured 1.4, Neon 0.8, Glass 1.2, sky 0.3, blended 1.6 (2.0 for a SurfaceAppearance with alpha), particle 1.3, local light 0.25, sun shadow 0.2 | estimated: relative shading work, not measured on a phone |
| shadow redraws | a quarter of the casters' triangles a frame | estimated (Roblox caches its shadow maps and redraws them as the camera moves) |

A finding's saving and a trial's delta are model numbers. The CPU prices come from phones; the GPU's
relative weights are estimates. Read small differences (under about 0.05 ms a view) as ties.

## Files

### `analysis.json` (`ror-cost-analysis/1`)

- `preset`, `quality` (what the quality draws: sun shadows, per-pixel lights, the particle rate),
  `prices`, `pixelsCounted`.
- `views`: per view `name`, Roblox's `draws` and `triangles` (every pass but Shadows), its `work`
  (below) and `overBudget`.
- `mean` and `worst`: the work averaged over the views, and the costliest view.
- `topInstances`: the costliest single instances.
- `findings`: ranked (below).

`work`, here and in the tree: `draws`, `triangles`, `pixels` and `blended` (at the preset's
resolution), `lightPixels` (on a light: the pixels it lights), `litPixels` (on a surface: the lights
over its pixels; the same evaluations, charged to the lights), `shadowTriangles`, `parts`, `cpuMs`,
`gpuMs` and `ms` (the phone frame: CPU plus the GPU's share).

### `tree.json` (`ror-cost-tree/1`)

`nodes`: every instance that costs anything in some view and each of its ancestors: `id`, `uid`,
`class`, `name`, `parent`, `mean` work (itself and everything under it, per view), `worstView` and
`worst` work, `views` (the views it draws or shades in), `leaves` (instances under it that cost
anything). Sorted by mean ms; `(shared passes)` holds what no instance owns (the particle pass, the
built-in bloom).

### Ids

An instance's id is its path from the file's root as ror writes paths, a name its siblings repeat
numbered by its place among them from 1: `Workspace.Map.Rock[3]`, `Workspace.Map["Rock.001"][2]`.
Two reads of one file give the same ids; edits name what they change by id; `uid` is Roblox's own
UniqueId where the file keeps it. Ids are taken before edits: removing an instance renumbers nothing.

### Findings

Each finding has a stable `id`, its `rank`, a `kind`, a `title`, its `targets` (the first few;
`targetCount` all), `why` (what the targets cost now, per view), `saving` (`meanMs` per view over
every view, `worstMs` and `worstView`, the draws, triangles and blended pixels saved, and `views`: ms
per view), the `edit` that realises it, `look` (`none`: the same pixels; `unseen`: nothing these
cameras show; `changes`: check it with `--look`), `basis` (`attribution`: the targets' share of the
work, or `batches`: draw calls that would join) and a `note`.

| kind | what | edit |
|---|---|---|
| `unseen` | parts no view shows at the highest quality (no opaque pixel in front, no blended fragment), grouped by the highest model all of whose parts are unseen | hide |
| `seeThrough` | see-through parts and alpha SurfaceAppearances by the blended pixels they shade, by mesh | hide |
| `appearance` | one geometry's copies in several appearances, a batch each | the most common appearance |
| `duplicateMesh` | one mesh uploaded under several ids | the most used id |
| `levelOfDetail` | meshes where RenderFidelity Performance saves more triangles than the batches it adds | RenderFidelity Performance |
| `merge` | several meshes in one appearance under one model, each mesh's every copy there | merge (one draw) |
| `emitterFill` | emitters by the blended pixels their particles shade | half the particles it makes (its Rate under the quality's cap, halved) |
| `invisibleEffect` | beams and emitters that show nothing and still draw | Enabled false |
| `lightRange` | local lights by the pixels they light, where the quality lights per pixel | Enabled false |
| `lightReachesNothing` | local lights that reach no pixel any view shows | Enabled false |
| `castShadow` | casters under a stud across, where the quality draws sun shadows | CastShadow false |

"Unseen" means unseen from the cameras given, drawn at the highest quality (an edit reaches every
device: a part a phone culls past its view distance still shows on a desktop; the run draws the views
a second time without the preset's culling for this). Use a camera set that covers where players go
before removing anything. A hidden or removed part no longer occludes either, which can add draws
behind it: try it with `--edits`.

### Edits (`ror-edits/1`)

```json
{"schema": "ror-edits/1", "trials": [
  {"name": "fog off", "edits": [{"op": "hide", "targets": ["mesh:1234567890"]}]},
  {"name": "lamps", "edits": [{"op": "set", "targets": ["under:Workspace.Map.Lamps"], "classes": ["PointLight"], "properties": {"Shadows": false, "Range": 12}}]},
  {"name": "rocks", "edits": [{"op": "appearance", "targets": ["Workspace.Map.Rock[2]"], "from": "Workspace.Map.Rock[1]"}]}
]}
```

- `remove`: the instances and everything under them.
- `hide`: every part at or under the targets takes Transparency 1 and no shadow, its decals
  Transparency 1; collision, joints, scripts, effects and lights stay.
- `set`: properties by name (`Transparency`, `CastShadow`, `Material`, `MaterialVariant`, `Color`,
  `RenderFidelity`, `MeshId`, `TextureID`, `Enabled`, `Range`, `Brightness`, `Shadows`, `Rate`, any
  other the class has): numbers, booleans, strings, enum item names, `[r, g, b]` in 0-1 or `"#rrggbb"`.
- `appearance`: the targets take `from`'s Material, MaterialVariant, MeshPart texture and
  SurfaceAppearance (what Roblox batches by; Color stays).
- `merge`: the parts at or under the targets counted as one mesh: copies of one appearance draw
  once. The cost model counts it; building the mesh is a merge tool's job.

Targets are ids, `mesh:<asset>` (every MeshPart or FileMesh of that mesh the scene draws) or
`under:<id>` (everything below); `classes` keeps those of the listed classes. A file's top-level
`edits` are one trial named `edits`. A part's `MeshId` or `TextureId` set where the part has none goes
to its SpecialMesh. A property set twice in a trial is one change, from the file's value to the last.
Edits to the place's own Lighting (GlobalShadows, Technology, effects) and Terrain's Clouds count in
a trial; a Lighting or MaterialService lent with `--lighting-from` or `--materials-from` stays as the
baseline drew it.

### `whatif.json` (`ror-cost-whatif/1`)

The look is drawn at the highest quality and full detail (what the highest graphics setting shows), so
a change any device would see shows. Per trial: its `edits`, the number of `changes`, `mean` work `before`, `after` and `delta`,
`worstSaving` and `worstLook` views, `changedInstances` (the instances whose cost moved most, by id),
and per view `before`, `after`, `delta`, Roblox's draws and triangles before and after, and with
`--look` the `look`: `changed` (the share of pixels more than 8 of 255 apart in any channel), `max`,
and `crops` (before | after | the changed pixels in red, the three regions that changed most, in
`look/<trial>/`, with each view's whole `before.png` and `after.png`).

### `--explain ID`

Per view the instance's work, how many of its parts drew, were hidden by occlusion or were culled
(out of view or by the quality), and the batches its parts are in; then the costliest instances under
it. Written to `explain-<id>.json` and printed.

### `--export DIR`

Per trial `DIR/<trial>/`: `changes.json` (`ror-cost-changes/1`: every change with its key, op, id,
the id's `steps`, class, property, `before` and `after`, and the trial's predicted saving),
`changes.tsv`, `apply.luau` and `revert.luau` (Lune 0.10.5 or later), and a README. `apply.luau`
resolves every id against the place as read before changing anything, moves removed instances to
`ServerStorage.RorCostRemoved` and marks changed ones (`RorCostKeys`); `revert.luau` undoes any keys,
or all. Merges are listed, not built.

### Budgets

`--budget phone` flags views over 600 draws, 800,000 triangles (Roblox's guidance for most devices)
or 10 ms of the model's phone frame (a suggestion); `desktop` doubles them; `draws=N,triangles=N,ms=N`
sets them (`500k`). `--fail-over-budget` exits with status 2 when a view is over, for a check.

### Heat images (`--heat`)

Over each view's picture (grey where nothing counts): `<view>.heat-cost.png` (each pixel's GPU cost in
viewport-units on a log scale, 1 green to 16 red), `-overdraw` (blended layers on a log scale, 1 to 16
and more), `-lights` (local lights holding the pixel, 1 to 8 and more) and `-density` (its part's
triangles per pixel, a thousandth to one, log). They are for reading,
not the interface: `analysis.json` holds the numbers.

## Limits

- Lights cost per pixel only where the quality lights per pixel (16 and up); a light's light-grid
  CPU is not priced.
- See-through parts are lit and blended at the weights above; their lights are not counted.
- A merge keeps its members' triangles and pixels; only the draws change.
- Pixels are counted at the preset's resolution scaled from a third of it (a half for desktop):
  details under about three phone pixels can be missed. `--pixel-scale 1` counts every pixel (and
  draws the heat images at full size), slower.
- `ror cost --check` measures draws and triangles against Studio; the frame ms are the model's.
