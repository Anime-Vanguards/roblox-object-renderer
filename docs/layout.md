# Repository layout

This page maps out the crates, tools, and directories in `roblox-object-renderer`.

- **`crates/ror`** — The `ror` CLI binary and library:
  - `scene/`: model loading, scene extraction, playback timelines, GUIs, and collision geometry.
  - `render/` and `shaders/`: the wgpu renderer and WGSL shaders.
  - `cost/`: draw-call, triangle, LOD, and occlusion estimation.
  - `service/`: the background render service and its client.
  - `viewport.rs`: the `ror viewport-worker` JSON protocol for `ViewportFrame` rendering.
  - `studio/`: the Studio parity harness and calibration experiments.
- **`crates/ror/examples/tools`** — Diagnostic and probe utilities (scans, dumps, one-off renders) grouped into a single example binary so Cargo compiles and links them once instead of once per tool. Run `cargo run -p ror --release --example tools` to list available tools, or pass `-- <tool> [args]` to run one. To add a tool, create a file with a `pub fn main()` reading `std::env::args()` and register it in `main.rs`.
- **`crates/ror-mesh`** — Roblox mesh decoder covering FileMesh versions 1.00–7.00 (including Draco-compressed meshes) and union mesh data.
- **`crates/ror-image`** — Texture decoder supporting PNG, JPEG, WebP, GIF, BMP, TGA, DDS, and KTX2 (with zstd and BCn block compression).
- **`crates/ror-assets`** — Content URI resolution (`rbxasset://`, `rbxassetid://`, asset URLs, local files), HTTP downloads, the disk cache and shared content store, Roblox install detection, and authentication ([assets.md](assets.md)).
- **`tools/`** — Python measurement and analysis scripts (requires `numpy`, `scipy`, `Pillow`, and `opencv-python`; `tools/perf_suite.py` also uses `psutil` and `pywin32`), cost and occlusion probes for the capture Studio, and `tools/perf-sampler` (a standalone Windows sampling profiler workspace).
- **`vendor/x264`** — Entry points and build script for the optional in-process x264 H.264 encoder ([building.md](building.md)).
- **`fixtures/`** — Small hand-built test models and scripts.
- **`docs/`** — Documentation and the rendered media in `docs/media/` shown in the README.
