# Measuring parity with Roblox Studio

`ror` derives its rendering and cost rules from controlled measurements against a dedicated Roblox Studio instance, comparing renders pixel by pixel and checking `ror cost` against Studio's own counters.

Code lives in `crates/ror/src/studio/`: `mod.rs` (capture Studio management), `place.rs` (capture place generation), `capture.rs` (Windows viewport capture), `compare.rs`, `export.rs`, `mcp.rs` (bridge client), and `calibrate.rs` + `calibrate/*` (calibration experiments).

`<state dir>` refers to the [state directory](building.md#standalone-commands-and-state) (`<checkout>/.cache` in a source checkout).

## Requirements

- **Live captures:** Roblox Studio on Windows (viewport capture reads pixels from the Studio window) plus a local Studio MCP bridge—a newline-delimited JSON-RPC TCP server exposing `list_roblox_studios` and `execute_luau` (`studio_id`, `datamodel_type`, `code`). The bridge is not included in this repository; point `ROR_STUDIO_MCP` at `host:port` (or a port on `127.0.0.1`). Always keep the bridge bound to `localhost` ([SECURITY.md](../SECURITY.md)).
- **Offline reruns:** Replaying stored captures requires neither Studio nor a bridge.

## The capture Studio

`ror studio open` generates a local place (`ror-capture.rbxl`) and launches it in Studio. Captures only ever modify this dedicated place; all other open Studios receive read-only queries (`SerializationService` exports and property reads).

Each capture Studio is protected by a lock file in `<state dir>/studio/` so only one process drives it at a time. Run multiple capture Studios side by side by setting `ROR_CAPTURE_PLACE=<name>` (for example, keeping a separate `--pre2022` instance).

```sh
ror studio open                                   # Open capture Studio (--pre2022 for legacy materials)
ror studio open --technology Future --mesh-lods disabled   # Set place-level properties scripts cannot write
ror studio status                                 # List open Studio windows and viewports
ror studio luau -e "return workspace.CurrentCamera.ViewportSize"
ror studio export "My Place" Workspace.Map --out map.rbxm  # Read-only export from any open Studio
ror studio close
```

## Comparing scenes (`ror compare`)

```sh
ror compare Model.rbxm --views default,iso
ror compare Model.rbxm --camera '{"mode":"orbit","yaw":25,"pitch":10,"fov":35}'
ror compare Map.rbxm --camera-from out/map-cost --views path1-0-far --lod roblox
```

`ror compare` loads the model into capture Studio with matching camera and lighting, captures Studio's viewport, renders `ror` at the exact same resolution, and writes:
- `report.json`: percentage of pixels within `2/255` and `8/255` of Studio per view.
- `studio.png`, `ror.png`, `diff.png`, and `side-by-side.png`.

To keep comparisons deterministic, `ror compare` freezes `Beam.TextureSpeed` to `0` and disables Terrain `Clouds` on both sides (Studio clouds drift continuously).

**Batch comparison and caching:**
- Studio captures are cached in `<state dir>/captures` keyed by model, lighting, camera, resolution, Studio version, and material generation, so reruns only re-render `ror`.
- Set `ROR_RECAPTURE=1` to force fresh Studio captures.
- Set `ROR_CAPTURE_OFFLINE="<version>|<materials>"` to replay cached captures without Studio open.
- `tools/parity.py` runs a fixed benchmark (R15/R6 rigs and four classic catalog items by asset ID). `tools/survey.py` compares many instances from one place. Combine `ror render --ids` with `tools/analysis/part_errors.py` to rank parts by pixel error.

## Measured parity

Pixel agreement (percentage of pixels within `2/255` / `8/255` of Studio):
- **R15 rig:** 97.7% / 99.6%
- **R6 rig:** 98.3% / 99.7%
- **Catalog gear and hats:** 99.0–99.9% / 99.7–100.0%
- **Game characters, props, and places:** 98.0–99.99% / 99.4–100.0%


## Calibration experiments

`ror studio calibrate <experiment>` builds a synthetic test scene in code (swatches, spheres, panels, emitters, lights), captures it in Studio, renders it in `ror`, and outputs `out/calibrate/<experiment>/<run>/studio.png`, `ror.png`, and `samples.json` (sampled pixel values at known world points for curve fitting).

Available experiment families are registered in `experiment()` in `crates/ror/src/studio/calibrate.rs`, including `material-*`, `ibl-*`, `sky-*`, `atmosphere-*`, `fog*`, `post-*`, `bloom-*`, `neon-*`, `glass-*`, `light-*`, `lg-*` (light grid), `ao-*`, `shadow-*`, `particle-*`, `beam-*`, `trail-*`, `legacy-*`, `gui-*`, and `forcefield-*`.

**VFX and timeline calibration (`calibrate/timeline.rs`):**
- `vfx-attack:<bundle.rbxm>|auto:<effect path>`: plays an effect by its emit attributes frame by frame in Studio vs. `ror`.
- `vfx-emitter:<bundle>|<emitter path>|<moment>|<variants>`: duplicates one emitter with a single property varied per copy.
- `exposure:<timeline experiment>`: averages many runs into a long exposure (useful when random particle placement makes single frames noisy).
- `vfx-sweep:<spec.json>`: runs batches of effects several per view.

Fitting scripts live in `tools/analysis/` (using `numpy` and `scipy`); each script's docstring notes which experiments it consumes and which constants it fits.

**Offline regression checks:**
1. Capture baseline `studio.png` runs once (`out/` and `.cache/` are git-ignored).
2. Re-render `ror` offline against stored captures: `ROR_RERENDER=1 ror studio calibrate <experiment>`.
3. Score regressions with `python tools/analysis/scorecard.py snapshot <before.json> <prefixes...>` and `compare <before.json> <after.json>`.

**Fuzz testing:**
```sh
cargo run -p ror --release --example tools -- vfx_fuzz 500 1 out/vfx-fuzz extreme
```
Generates randomized scenes across particle, beam, trail, light, decal, Glass, and Highlight properties (normal and edge values) and checks for panics, slow renders, non-determinism, blank effects (`sane`), and NaN/Inf radiance (`ROR_NAN_CHECK=1` highlights non-finite pixels in magenta in any render).

Note: Maintainer experiments that depend on private/unpublished game assets are excluded from this repository, though the rules derived from them remain in code and documentation.

## Reporting a visual difference

Open an issue with the **"Looks different from Studio"** form and include your Studio version, graphics settings, a minimal self-contained scene, the `ror` command, and side-by-side images. See [CONTRIBUTING.md](../CONTRIBUTING.md#measuring-against-studio).
