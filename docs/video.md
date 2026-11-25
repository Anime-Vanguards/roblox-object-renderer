# Video output (`ror video`)

`ror video` (and `POST /video` on the [render service](service.md)) renders animated timelines, effect playbacks, or camera turntables to video files and contact sheets.

Implementation: `crates/ror/src/video.rs`, `mp4.rs` (built-in MP4 muxer), `x264.rs` (optional in-process encoder), `nvenc.rs` (optional hardware NVENC), and `render/nv12.rs` (GPU RGB-to-NV12 conversion).

## Quick examples

```sh
ror video Effect.rbxm                                            # 2 s clip using emitter attributes
ror video Cutscene.rbxm --emit =cutscene                         # Cutscene emit timing
ror video Effect.rbxm --duration 1.5 --speed 0.25 --view iso     # 0.25x slow-mo (6 s output)
ror video Model.rbxm --orbit 360 --duration 4 --view front       # Full 360° turntable
ror video Effect.rbxm --background transparent                   # Transparent video.webm
ror video Effect.rbxm --playback effect.json --start 0.2 --fps 60
```

## Timeline, camera, and output options

- **Playback:** Accepts the same playback flags as `ror render` (`--emit`, `--enable`, `--disable`, `--playback`, `--set`, `--tween`, `--parent`; see [playback.md](playback.md)). If none of the first four flags are given, `ror video` defaults to `--emit =attributes` at `t = 0`. Pass `--no-emit` to render only emitters already enabled in the file.
- **Timing:** `--start` (start timestamp), `--duration` (timeline seconds), `--fps` (frame rate), and `--speed` (playback rate—`0.25` plays at quarter speed for a 4x longer video).
- **Camera:** Pick a single camera via `--view`, `--yaw`/`--pitch`, `--fov`, or `--camera`. Orbit cameras fit once across all visible parts plus 9 sampled moments of live particles (trimming the farthest 3% outliers), beams, and trails so the camera stays locked throughout the clip. Override framing with `--frame` or `--focus`, or add `--orbit <degrees>` to spin around the subject at fixed distance.
- **Reusing cameras:** `--camera-from <dir>` copies an earlier video's framed bounding box (keeping `--view` and `--orbit` identical for before/after comparisons) or copies a matching named view from an earlier render as a fixed camera.
- **Camera tracks:** `--camera-track <file>` drives the camera from a JSON array of keyframes (`{"at": <s>, "cframe": [x,y,z, R00..R22], "fov": <deg>}`, such as `ree`'s `camera.json`), holding each keyframe until the next. Off-screen emitters freeze automatically as the camera moves, matching Studio.
- **Output location & size:** Writes to `--out` (default `<state dir>/out/<input>-video`; see [building.md](building.md#standalone-commands-and-state)) at `--size WxH` (default `1920x1080`, even dimensions required for MP4/WebM). `summary.json` saves the first frame's camera in `views`, so `ror render <input> --views <view> --camera-from <out>` captures a matching still.

### Fast contact sheet (`--sheet-first`)

When running through the render service, `--sheet-first` (job JSON: `"sheetFirst": true`) renders the contact sheet frames first and returns immediately once `sheet.png` is on disk, encoding the full video in the background (for example, returning in ~113 ms instead of ~359 ms on a 4 s 1440p orbit).

- The initial response includes `"pending"` pointing to `<out>/clip.pending`, which remains until `video.mp4` and `summary.json` finish (or turns into `clip.failed` containing the error on failure).
- Produced bytes are identical to a normal run. Multiple clips targeting the same `--out` serialize automatically. Tools reading `--out` should wait for `clip.pending` to disappear.

### Parallel secondary copies (`--copy`)

For MP4 clips, pass `--copy '{"file": "small.mp4", "args": [...]}'` (repeatable; job JSON: `"copies"`) to spawn a parallel `ffmpeg` process fed the same raw NV12 frames during rendering, avoiding a second decode/re-encode pass afterward.

- `args` are inserted as `ffmpeg` output flags (codec, bitrate, `-vf`, `-movflags`, etc.); the file extension determines the container (`mp4`, `m4v`, `mov`, `mkv`, `webm`).
- `summary.json` reports each copy's `written` flag, any `error`, and `waitedMs`. (For example, a downscaled NVENC copy finishes ~0.3 s after a 4K clip instead of taking 2.4–5.9 s to transcode afterward.) If you only need the smaller size, encode directly with `--nvenc` instead.


## Formats

| Format (`--format`) | Details |
|---|---|
| `mp4` *(default)* | H.264 encoded from GPU-converted limited-range BT.709 NV12 (1.5 bytes/px readback instead of 4; transparent backgrounds flatten over black). Tagged BT.709 matrix/primaries, sRGB transfer, centered chroma, with `moov` atom at the start (`+faststart`). |
| `webm` | VP9 via `ffmpeg`, preserving alpha (automatically chosen when `--background transparent` is set). |
| `gif` | Animated GIF via `ffmpeg`. |
| `apng` | Lossless animated PNG written directly by `ror` (large files; also used as fallback when `ffmpeg` is missing). |
| `png` | Individual PNG frames written to `frames/0000.png`, ... |

Every format also writes `sheet.png` (a contact sheet of 12 labeled frames by default; configure with `--sheet N`) and `summary.json`.

## Encoders

Select the MP4 encoder with `ROR_VIDEO_ENCODER=auto|x264|libx264|h264_nvenc`:

- **`x264` (in-process, Windows `x86_64` MSVC only):** Enabled via `--features x264` (off by default). Links `ror_x264.lib` built by `vendor/x264/build.py` (x264 commit `c24e06c`, core 164 r3192, 8-bit + assembly + `vendor/x264/ror_x264.c`) from `ROR_X264_LIB_DIR`. Produces a byte-identical H.264 stream to `ffmpeg`'s `libx264`, reading frames directly from mapped GPU buffers on below-normal priority threads. Licensed GPL-2.0-or-later ([THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)).
- **`libx264`:** Pipes NV12 frames to `ffmpeg`'s `libx264` (the default for `auto` without `--features x264`).
- **`h264_nvenc`:** Uses NVIDIA hardware encoding through `ffmpeg` (`p1`, VBR CQ 19, spatial AQ), automatically falling back to `libx264` if NVENC rejects the resolution or driver.

**Direct in-process NVENC (`--nvenc <quality>[:<kbps>]`):** Overrides `ROR_VIDEO_ENCODER` (job JSON: `"nvenc"`) to encode directly through NVIDIA's driver (`nvEncodeAPI64.dll` / `nvcuda.dll`, driver 522.25+). Uses constant quality (`1–51`, lower is better) with an optional max bitrate cap in kbps—matching `ffmpeg` `-preset p3 -tune hq -rc vbr -cq <q> -maxrate <kbps>k -bf 3 -rc-lookahead 8 -spatial-aq 1`, with B-frame composition offsets and an edit list in the MP4. Fails explicitly if no compatible NVIDIA GPU/driver is present.

**Default x264 settings:** `ultrafast` preset at CRF 19 with 6 frame threads below 4K (8 at 4K+) for deterministic output across machines. Compared to the older `veryfast` CRF 17 profile, encoding is 1.6–2x faster at equal PSNR (SSIM >= 0.98) with 2–3x larger files (`ROR_VIDEO_X264=veryfast` restores the old `ffmpeg` settings).

`ffmpeg` is located via `ROR_FFMPEG`, then `PATH`, then winget's `Gyan.FFmpeg` install path. If `ffmpeg` is absent and required by the format, `ror` falls back to `apng` and notes it in `summary.json`.

## Frame pipelining and reuse

- **Minimal re-extraction:** Models parse once. Emit-only playbacks extract the scene once and step only particles/trails per frame; Lighting-only playbacks re-read only `Lighting`; general playbacks apply and revert frame deltas incrementally while reusing unmodified subtrees.
- **Lookahead:** Scene extraction and GUI image fetching (16 frames ahead) run asynchronously ahead of GPU submission.
- **Deduplication:** Identical consecutive frames under a fixed camera reuse the previous rendered frame (`ROR_FRAME_REUSE=off` disables). Static cameras reuse SSAO history across frames, recomputing only around moving parts (`ROR_AO_HISTORY=off` disables).
- **Interleaving:** Frames claim service GPU slots individually, allowing multiple clips and still renders to interleave smoothly.

## What the summary measures

`summary.json` records frame counts, timings, framed bounds, peak/per-sheet particle counts, active part/emitter/beam/trail counts, `guis` (`drawn` and `skipped` with reasons), missing assets, warnings, and the first-frame camera. Set `ROR_VIDEO_PROFILE=1` to include detailed stage timings under `outputProfile`.

Built-in VFX review lints (`crates/ror/src/lint.rs`, summarized to stderr and listed in `lines`):

| Field | What it reports |
|---|---|
| `frameStats` | Per-frame `{time, frame, nearWhite, bright, luma}`: fraction of pixels with R/G/B >= 230, Rec.709 luma >= 0.8, and mean luma (`0–1`). |
| `weightless` | Active emitters flying faster than 8 studs/s with neither `Drag` nor `Acceleration`. |
| `lint.fades` | Purely linear opacity fades (`sequence` keypoints on a line or linear `tween` keys). |
| `lint.restraint` | Sibling emitters under one parent sharing duplicate textures (`duplicateTextures`) or mixing incompatible `LightEmission` ranges (`mixedBlends`, spread > 1 or mixed signs). |
| `lint.frozen` | Emitters with `Drag >= 15` and `Lifetime.Max >= 1` s (clouds that stop dead and hang in midair). |
| `lint.refused` | Textures permanently refused by Roblox (`moderated`, `archived`, `unreviewed`). |
| `lint.rigid` | Beams rendered as static straight lines (`TextureSpeed == 0` and uncurved across all active frames). |
| `counts.effects` | Per top-level child effect: `{root, emitters, textures, beams, trails}`. |
