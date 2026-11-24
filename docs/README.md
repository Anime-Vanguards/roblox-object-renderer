# ror documentation

Reference guides for running `ror`, understanding its rendering rules, and extending the codebase. For an overview and quick start, see the [README](../README.md).

## Using ror

| Guide | Topics |
|---|---|
| [building.md](building.md) | Release binaries, verification, state directories, source builds, Cargo features, and x264 |
| [render.md](render.md) | `ror render`: inputs, cameras, LODs, map backdrops, collision view, outputs, lints, inspect/diff, and GUIs |
| [playback.md](playback.md) | Effect timelines: CLI playback flags, JSON format, emit modes, and dynamic Lighting |
| [video.md](video.md) | `ror video`: cameras, output formats, encoders, contact sheets, parallel copies, and VFX lints |
| [scripts.md](scripts.md) | Running `.luau` scripts via `ree` as inputs to `render` and `video` |
| [archives.md](archives.md) | Roblox archives (`.rbx`): what `ror` reads from one, what it checks and reports, and the `ror-archive` crate |
| [cost.md](cost.md) | `ror cost`: draw call batching, triangle counts, LODs, and software occlusion culling |
| [cost-loop.md](cost-loop.md) | Optimization loop: phone ms cost attribution, findings, in-memory what-if edits, visual checks, Lune export, and budgets |
| [service.md](service.md) | Background render service, concurrent callers, HTTP API, job JSON, caching, memory admission, and profiling |
| [viewport-worker.md](viewport-worker.md) | `ror viewport-worker`: persistent `ViewportFrame` renderer and its JSON protocol |
| [assets.md](assets.md) | Asset downloads, Open Cloud API keys, account broker, GPU texture caches, shared content store, and disk cleanup |
| [parity.md](parity.md) | Studio parity harness: capture Studio setup, pixel comparisons, agreement numbers, and calibration experiments |

## Rendering rules and architecture

| Guide | Topics |
|---|---|
| [rendering.md](rendering.md) | Scene extraction, supported objects, color pipeline, lighting, shadows, sky, atmosphere, fog, clouds, post effects, and GUIs |
| [materials.md](materials.md) | 2022 and pre-2022 materials, SurfaceAppearance, MaterialVariants, Neon, Glass, ForceField, decals, and characters |
| [particles.md](particles.md) | Particle emitters, beams, trails, and legacy effects |
| [layout.md](layout.md) | Repository crates, tools, and directory structure |

## Where the rendering rules come from

Every rendering and cost rule is derived from controlled captures in a dedicated Roblox Studio instance—diffed pixel by pixel or checked against Studio's own render counters—supplemented by public Roblox documentation and talks. Constants in the shaders and Rust code are fitted directly to those measurements. Code comments name the calibration experiment (`ror studio calibrate <experiment>`, see [parity.md](parity.md)) that produced each constant. Comments may also cite unpublished internal lab notes (`notes/<area>.md`); the pages here summarize every rule those notes established.
