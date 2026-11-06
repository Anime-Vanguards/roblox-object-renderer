# What a scene costs Roblox to draw

`ror cost` estimates, for each view, the draw calls and triangles Roblox's renderer would submit and
what its occlusion culling hides, and says what would cut them most. The code is
`crates/ror/src/cost` (`mod.rs` the batches and report, `cull.rs` the software occlusion, `meshes.rs`
mesh occluders, `studio.rs` Studio's own counts) and `crates/ror/src/lod.rs` (levels of detail).
The rules were measured in a capture Studio against Roblox's own counters, one property at a time,
with the probe scripts in `tools/cost-probes` and `tools/occlusion-probes` (run by
`tools/cost_probes.py` and `tools/occlusion_probes.py`).

## Running it

```sh
ror cost Map.rbxm --sweep 5                 # the 5 densest spots, looking 4 ways from eye height
ror cost Map.rbxm --paths 4                 # gameplay views: 4 points along the enemy paths, near and far, 4 ways
ror cost Place.rbxl --cameras cameras.json  # named cameras: {"name", "eye", "target", "fov"} or {"name", "cframe", "fov"}
ror cost Map.rbxm --grid 60                 # the cost by where the camera stands, painted over the map from above
ror cost After.rbxm --camera-from out/before --compare out/before   # an earlier run's views, before and after
ror cost Map.rbxm --paths 4 --check --profile   # also Studio's counts, and its MicroProfiler's render timings
```

- Prints the scene's counts, each view's draws and triangles per pass (Opaque, Transparent,
  Particles, UI, Sky, Cloud, GenericPostProcess; characters' Shadows apart, outside the totals as in
  Roblox's guidance), what it shows and culls, where the draws go (batches of copies, parts drawn
  alone, decals, sorted parts, characters, emitters, beams, GUIs, the sky), the biggest draw-call
  gains over the views (one appearance for a geometry's copies, one MeshId for uploads of one mesh,
  decals on one face, beams and emitters that show nothing, SelectionBoxes, Highlights, Humanoid
  display models, levels of detail), the heaviest batches by triangles and the costliest batches.
  Gains rank by Roblox's own weighting, a draw call as 20,000 triangles.
- Writes `cost.json` (every view's passes and batches, with paths), `<view>.png`, `<view>.draws.png`
  (every draw call its own colour: what one call draws shares it) and `<view>.heat.png` (red: a draw
  call of its own, yellow: ten copies a call, blue: a hundred or more; grey what the view culls);
  `--no-images` skips the images. `summary.json` holds the report with each view's costliest batches,
  largest occluders, missed occluders and tested groups (`--top`, 24 of each; `cost.json` has them
  all), a render job the same with `"cost": {...}`. A grid's `cost.json` (over 64 views) is compact
  JSON.
- Culling as Roblox's: per instance by the frustum, and by occlusion, rasterized in software from
  the faces toward the camera of every occluder in the view (opaque blocks, wedges, balls,
  cylinders, and meshes of at most 2,048 distinct vertex positions large enough on screen for their
  detail; not unions, trusses or cut-out SurfaceAppearances). Roblox's culler adapts its work to the
  machine, so a single view can be off by tens of percent ([accuracy](#accuracy)). `--no-occlusion`
  counts the frustum alone (what Roblox draws at most).
- Occlusion's own cost: each view's line says what the culling jobs test and rasterize and an
  estimate of their time (`queryOcclusion`, fitted to Studio's MicroProfiler: the median view within
  19 %); the report lists where the tested objects are, the occluders that hide most, the costliest
  mesh occluders (their triangles, nearly all of the culling jobs' time) beside what they hide, mesh
  occluders that hide nothing, and missed occluders (large solids that cannot occlude, by what a
  block inside each would hide).
- Levels of detail, as `render --lod` draws them ([below](#levels-of-detail)): each MeshPart at the
  level its distance to its box and its RenderFidelity give, and the parts the quality leaves out not
  counted. The report lists the meshes where Performance would pay: the triangles it saves per view
  against the batches it adds where copies sit on different sides of a switch distance (a draw
  weighed as 20,000 triangles). On match maps that is far scenery (mountains); buildings near the
  track gain nothing.
- `--paths N` (gameplay views along a map's enemy paths: parts under its outermost folders named
  Path(s), Node(s) or Waypoint(s)), `--grid STUDS [--region X0,Z0,X1,Z1[,Y]]` (a far gameplay camera
  every STUDS studs, 20 for a match map, over the track's placement area: the path nodes' box and 40
  studs around it, where a match's costliest views are; the walls enclose several times that, mostly
  scenery. Each cell is painted by its views' draws, triangles and occlusion work over the map seen
  from above: `grid.draws.png`, `grid.triangles.png`, `grid.occlusion.png`, `grid.json`; the grid's
  views are never drawn, only their cameras taken), `--compare DIR` (draws, triangles and occlusion
  work per view against an earlier run; `--camera-from DIR` alone takes that run's views), `--items`
  (`items.json`: every item the scene draws, with its path, part index, mesh, pass, triangles per
  level and box, and per view the items drawn, at which level and for how many triangles, and the ones
  occlusion hides).
- `--technology` (the scene's Lighting's by default) and `--quality` (21) change what transparent
  Glass and characters cost and what the quality culls; `--full-detail` keeps every mesh at full
  detail, as Studio draws by default (its MeshPartDetailLevel setting; the culling stays, Studio's
  too). `--lod roblox:<quality>` is `--quality <quality>`, and with `--check` has the capture Studio
  count at its levels of detail instead of at full detail.
- `--check` loads the scene into the capture Studio (`ror studio open`, `--technology` to match the
  map's) and prints Roblox's counts beside ror's for every view (`cost-check.json`); the views take
  the capture viewport's shape. `--profile` adds the render job's CPU time per view from Studio's
  MicroProfiler (through Roblox's LibMP): occlusion culling's `queryOcclusion` on the culling jobs,
  the render thread's wait for it and its own work. Both need a Studio bridge ([parity.md](parity.md)).
  `python tools/cost_probes.py <case>` and `python tools/occlusion_probes.py <case>` rerun a
  measurement probe in the capture Studio.

## Counting

Roblox exposes its renderer's counts to scripts, in edit mode as in play:
`Stats.SceneDrawcallCount` / `SceneTriangleCount` (every pass of the last frame), `ShadowsDrawcallCount`,
the Shift+F2 render stats, and `SceneAnalysisService:GetTriangleCompositionAsync()` by pass (Opaque, Transparent, Particles, UI,
Sky, Cloud, GenericPostProcess, Shadows). `ror cost --check` reads them from the capture Studio beside
ror's estimate. Roblox's guidance weighs a draw call as about 20,000 triangles and leaves Shadows out
of budgets; `ror cost` does both.

## Batches

Roblox draws the world's static parts in batches: every copy of one geometry with one appearance is
one draw call, however many and wherever they are.

- One draw whatever their Color, Size, rotation, Reflectance, DoubleSided, Anchored, CastShadow,
  surface types or the amount of their transparency. No cap: 6,000 meshes were one draw.
- Apart: each geometry (a part shape, a MeshId, a SpecialMesh's mesh, a union) and each appearance
  (Material and MaterialVariant, MeshPart.TextureID, a SurfaceAppearance's maps). Transparent and
  opaque copies are two batches; copies at different levels of detail are a batch per level.
- And per distance from the camera, in three bands: under 600 studs, to 1,200, beyond.
- A SurfaceAppearance decides alone: copies with identical maps are one batch across separate
  instances; Material and TextureID no longer split; a different normal map does.
- A SpecialMesh FileMesh and a MeshPart of the same mesh are two batches. Each distinct union is a draw
  of its own; copies of one union instance like meshes.
- Two uploads of one mesh are two batches: `ror cost` finds them by the shape of their full-detail
  geometry (`duplicateMeshes`).
- Triangles: Block 12, Ball 432, Cylinder 96, Wedge and CornerWedge 10, SpecialMesh Sphere 432, Head
  846; a mesh its file's level of detail.
- Drawn one at a time: ForceField parts, transparent parts holding decals, emitters, beams (even
  invisible ones), trails, Humanoid models' clusters and their transparent parts; decals batch per
  image and face; a SelectionBox is twelve draws.

## Levels of detail

As `crate::lod` has it (Studio 0.742, `Workspace.MeshStreamingAndImprovedLods` Disabled,
`MeshPartDetailLevel` DistanceBased as games draw):

- A MeshPart's level comes from the straight-line distance from the camera to the part's box (0
  inside it), scaled by the field of view: `d · tan(fov / 2) / tan(35°)`. Full detail below 250, then
  the file's next level below 500, 2,000 and 3,000, the fifth beyond; no hysteresis, one frame late.
- RenderFidelity Performance takes every level the file has; Automatic only its high-quality levels;
  Precise, and SpecialMeshes, full detail. A Performance mesh without a chain gets levels made at run
  time, which ror cannot make: it draws those at full detail and says so. Shadows are drawn at full
  detail.
- The graphics quality also culls parts too small on screen or past its view distance (300 studs at
  quality 1-3, 700 at 10).

## Occlusion

Roblox culls with a software rasterizer on the CPU: occluders are rasterized into a depth buffer and
each object is tested by its screen-space 2D bounding box, per instance (a draw call disappears only
when every copy in it is hidden), on worker threads, adapting its work to the machine (so a single view
can differ by tens of percent while averages agree).

- Occluders: opaque parts (Transparency 0, any material, any thickness); blocks, wedges, cylinders and
  balls; meshes of at most about 2,048 distinct vertex positions, and only while large enough for
  their detail (about 0.67 positions per pixel of their screen box on a 256-pixel-wide view); faces
  toward the camera only.
- Not occluders: unions, trusses, SpecialMeshes, Humanoid models and skinned meshes, a
  SurfaceAppearance with AlphaMode Transparency, anything above Transparency 0, a part the camera is
  inside, and parts under about 2 pixels of a 256-pixel-wide view.
- `ror cost` rasterizes the same occluders in software (`cost::cull`) and estimates the culling
  jobs' time (`queryOcclusion`, fitted to Studio's MicroProfiler through Roblox's LibMP).

## Accuracy

Over 472 views of ten game maps the draws came within 0.3 % of Studio's, the median view within 4 %.
`--no-occlusion` counts the frustum alone, what Roblox draws at most. Decals on meshes are counted as
the mesh's triangles facing the decal's face, which Studio does not match exactly.
