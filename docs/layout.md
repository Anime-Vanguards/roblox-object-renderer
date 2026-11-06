# Repository layout

- `crates/ror`: the `ror` binary: model loading and scene extraction (`scene/`), the wgpu renderer
  (`render/`, shaders in `shaders/`), what a scene costs Roblox to draw (`cost/`), the render service
  and its client (`service/`), and the Studio parity harness (`studio/`).
- `crates/ror/examples/tools`: ror's tools (scans, dumps, probes, one-off renders) in one example, so
  checking or building them is one crate and one link instead of one of each per tool.
  `cargo run -p ror --release --example tools -- <tool> [arguments]` runs one, and `--example tools`
  alone lists them. A new tool is a file there, written like a standalone example (`pub fn main`, its
  arguments from `std::env::args()`), with its name added to the list in `main.rs`.
- `crates/ror-mesh`: Roblox mesh decoding: FileMesh versions 1.00-7.00 (Draco), union mesh data.
- `crates/ror-image`: texture decoding: PNG, JPEG, WebP, GIF, BMP, TGA, DDS, KTX2 (zstd, BCn).
- `crates/ror-assets`: content URIs (`rbxasset://`, `rbxassetid://`, asset URLs, files), downloads,
  the disk cache and the shared content store, the Studio install, the cookie ([assets.md](assets.md)).
- `tools/`: Python scripts for measurements and analysis (numpy, scipy, Pillow, opencv-python;
  `tools/perf_suite.py` also psutil and pywin32), the cost and occlusion probes run in the capture
  Studio, and `tools/perf-sampler`, a Windows sampling profiler in its own workspace.
- `vendor/x264`: ror's entry points into x264 and the script that builds them into a library
  ([building.md](building.md)).
- `fixtures/`: small hand-made test models and scripts.
- `docs/`: these pages; `docs/media/`, the README's pictures and clips, rendered by ror.
