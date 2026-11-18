# The viewport worker

`ror viewport-worker [--offline]` draws ViewportFrames for another program on a renderer it keeps warm
between requests. [roblox-ui-renderer](https://github.com/Anime-Vanguards/roblox-ui-renderer) (rur)
starts it to draw the 3D content of the ViewportFrames in its UI renders and storybooks: build ror beside a
rur checkout, or set `RUR_VIEWPORT_WORKER` to a ror executable (a release's works), and rur finds it
(rur's docs/viewports.md).

The protocol is one JSON request a line on stdin and one JSON reply a line on stdout, one request at a
time. A failed request is answered with `{ "error": "..." }` and the worker goes on; it exits when stdin
closes. `--offline` uses only what is already on disk.

| Request | Reply |
|---|---|
| `{ "viewport": <snapshot>, "out": "<file.png>" }` | `{ "file", "warnings", "background" }` once `out` holds the picture |
| `{ "command": "asset", "uri": "<content id>", "out": "<file>" }` | `{ "file", "kind" }` once `out` holds the asset's bytes |

A snapshot is what one ViewportFrame shows:

```json
{
  "version": 1,
  "width": 320, "height": 240,
  "camera": { "mode": "cFrame", "components": [0, 5, 10, 1, 0, 0, 0, 1, 0, 0, 0, 1], "fov": 70 },
  "background": { "color": [0.1, 0.1, 0.1], "transparency": 0 },
  "instances": [
    { "id": 1, "parent": 0, "class": "ViewportFrame", "name": "Frame", "properties": { ... } },
    { "id": 2, "parent": 1, "class": "Part", "name": "Block", "properties": { ... }, "references": { ... } }
  ]
}
```

- `width` and `height`: the picture's size in pixels, the frame's logical size (Studio draws a
  ViewportFrame at its logical size whatever the device scale), at most 2048 each.
- `camera`: the frame's CurrentCamera, in any of ror's camera forms (`cFrame` with the CFrame's 12
  components and FieldOfView is the one a Camera gives).
- `background`: the frame's fill, BackgroundColor3 (0..1) and BackgroundTransparency. Studio draws the
  world into a target cleared to it, so the picture holds it and the reply says `"background": true`;
  a part's anti-aliased edge then averages its colour with the fill's at full strength, as in Studio.
  Without it the picture is transparent around the world.
- `instances`: the ViewportFrame first, with its `Ambient`, `LightColor` and `LightDirection` (its own
  light: no shadows, fog or post effects; a Sky child gives reflections), then the instances under it,
  each after its parent. `parent` 0 is the snapshot's root, so other roots the picture reads come after:
  the place's MaterialService with its MaterialVariants. `properties` are rbx-dom's JSON forms of the
  property values; `references` gives each Ref property as another instance's `id` (one the snapshot does
  not hold is nil). WorldModels are containers like Models, and MeshParts keep `InitialSize`, so a mesh is
  scaled by `Size / InitialSize`.

Meshes and textures load as for `ror render`: from the caches, the local Roblox install and the
downloads ([assets.md](assets.md)). A picture whose assets can't be had is an error that names them.

The worker gives back what it holds for the next requests after 30 seconds without one (the renderer's
leftovers and the downloads held in memory); the GPU device and the textures stay warm.

A program that links the `ror` crate can run the same worker with commands of its own:
`ror::viewport::serve(assets, &mut commands)`, where `commands` implements `ror::viewport::Commands`
(its `handle` answers before ror's, and `release_idle` frees its own caches when the worker goes quiet).
