# Viewport worker (`ror viewport-worker`)

`ror viewport-worker [--offline]` runs a persistent, single-request-at-a-time JSON worker over stdin/stdout that renders Roblox `ViewportFrame` snapshots using a warm GPU renderer.

[roblox-ui-renderer](https://github.com/Anime-Vanguards/roblox-ui-renderer) (`rur`) spawns this worker automatically to render 3D `ViewportFrame` contents inside UI previews and its desktop storybook. Build `ror` beside a `rur` checkout, install `ror` on your `PATH`, or point `RUR_VIEWPORT_WORKER` at any `ror` binary (see `rur`'s `docs/viewports.md`).

## Wire protocol

Send one JSON request per line on `stdin`; receive one JSON reply per line on `stdout`. Errors return `{ "error": "..." }` without terminating the worker. The process exits cleanly when `stdin` closes. Pass `--offline` to restrict asset resolution to local caches and installs.

| Request | Reply |
|---|---|
| `{ "viewport": <snapshot>, "out": "<file.png>" }` | `{ "file", "warnings", "background" }` after writing the PNG |
| `{ "command": "asset", "uri": "<content id>", "out": "<file>" }` | `{ "file", "kind" }` after writing the asset bytes |

## Snapshot format

```json
{
  "version": 1,
  "width": 320,
  "height": 240,
  "camera": { "mode": "cFrame", "components": [0, 5, 10, 1, 0, 0, 0, 1, 0, 0, 0, 1], "fov": 70 },
  "background": { "color": [0.1, 0.1, 0.1], "transparency": 0 },
  "instances": [
    { "id": 1, "parent": 0, "class": "ViewportFrame", "name": "Frame", "properties": { ... } },
    { "id": 2, "parent": 1, "class": "Part", "name": "Block", "properties": { ... }, "references": { ... } }
  ]
}
```

- **`width` / `height`:** Output size in logical GUI pixels (Studio always renders `ViewportFrame`s at logical size regardless of device DPI scale), capped at 2048 per axis.
- **`camera`:** Any `ror` camera object (typically `cFrame` with 12 CFrame components and `fov`).
- **`background`:** Optional `BackgroundColor3` (`0..1`) and `BackgroundTransparency`. When provided, the render target clears to this color so anti-aliased geometry edges blend against the fill just like Studio, and the reply includes `"background": true`. When omitted, the background is transparent.
- **`instances`:** Ordered parent-before-child. The `ViewportFrame` comes first (carrying its own `Ambient`, `LightColor`, and `LightDirection`; no shadows, fog, or post effects, though a child `Sky` supplies reflections), followed by its descendants. Additional roots (`parent: 0`) can follow—such as `MaterialService` with `MaterialVariant`s.
  - `properties` use `rbx-dom` JSON encodings.
  - `references` map `Ref` properties to another instance's `id` (unknown IDs resolve to `nil`).
  - `WorldModel` acts as a container like `Model`, and `MeshPart`s preserve `InitialSize` so meshes scale by `Size / InitialSize`.

## Assets, caching, and embedding

- Meshes and textures resolve through the same caches, local Roblox installation, and downloader as `ror render` ([assets.md](assets.md)). Missing required assets fail the request with an error naming them.
- Built-in material textures (`Plastic`, `Wood`, etc.) require a local Roblox Studio or Player installation; without one, only textureless materials like `SmoothPlastic` can draw. A `ViewportFrame` without a child `Sky` needs no skybox textures.
- After 30 seconds idle, the worker releases transient renderer state and in-memory download buffers while keeping the GPU device and texture caches warm.
- **Rust library embedding:** Link the `ror` crate and call `ror::viewport::serve(assets, &mut commands)` with a custom `ror::viewport::Commands` implementation (`handle` intercepts requests before built-in handlers; `release_idle` frees custom caches on idle).
