# The background render service

`ror render` and `ror video` automatically submit jobs to a local background render service, started on first use and shared by every `ror` process pointing at the same [state directory](building.md#standalone-commands-and-state) (`<checkout>/.cache` in a source build, overridable via `ROR_STATE_DIR`).

The service keeps the GPU device, compiled pipelines, meshes, textures, and parsed models warm in memory and executes concurrent jobs in parallel. A warm 1024x1024 render takes ~20 ms of GPU time with a client round trip under 100 ms.

Pass `--no-service` (or set `ROR_NO_SERVICE=1`) to render in-process instead. Implementation: `crates/ror/src/service/`, `crates/ror/src/job.rs`, and `crates/ror/src/budget.rs`.

## Service commands

```sh
ror service status                   # Port, PID, cache sizes, and active jobs per build
ror service stop                     # Stop all services for this state directory
ror service warm                     # Start service if needed and briefly boost GPU clocks
ror render Model.rbxm --no-service   # Bypass the service for one invocation
```

## Process lifecycle

- **Safe rebuilds:** Each service runs from a copy of the binary under `<state dir>/bin`, so recompiling `ror` never fails on a locked executable. When the source binary changes, the service spawns a successor from the new build and exits cleanly once the successor is ready (`ROR_SERVICE_SUCCESSOR=0` disables auto-handoff). Distinct builds sharing a state directory run side-by-side services; online and `--offline` callers of the same build share one service (`X-ROR-Offline: 1` forbids network access per request), and records of exited service processes are pruned automatically.
- **Dormant mode & idle exit:** After `ROR_QUIET_RELEASE` seconds without jobs (default `10`), the service releases transient render buffers, preparations, shadows, and download buffers in-process while keeping its GPU device and warm texture, mesh, sky, and parsed-scene caches hot across pauses in a session. After `ROR_QUIET_DORMANT` seconds without jobs (default `120`), it hands off to a lightweight dormant process (~30 MB RSS holding only the GPU adapter; `ROR_SERVICE_HANDOVER=0` keeps it in-process). Waking on the next job takes 0.12–0.25 s. After `ROR_SERVICE_IDLE` seconds of inactivity (default `600`), it exits completely.
- **Large playbacks & automatic fallback:** `--playback <file>` passes the playback file by path (`playbackFile`) rather than inlining the JSON in the HTTP body, and the service streams request bodies up to the 512 MiB safety limit (rejecting oversized bodies with HTTP 413). Any refused or failed service request is logged to `<state dir>/serve.log` and reported back to the client; `ror render`, `ror video`, and `ror cost` automatically fall back to in-process rendering (printing why) if the service refuses or cannot handle the request.
- **Startup & Vulkan tuning:** Starts in under a second and prefers a discrete Vulkan GPU. On Windows it caches the selected driver in `<state dir>/gpu.json` (`ROR_VULKAN_FILTER=0` disables) and disables implicit Vulkan layers (overlays/capture hooks) unless `VK_LOADER_LAYERS_DISABLE` is set or `ROR_VULKAN_LAYERS=1`. Pipelines cache under `<state dir>/pipelines` (`ROR_PIPELINE_CACHE=0` disables).
- **GPU clock warmup (`ror service warm` / `POST /warm`):** Runs a short GPU burst (at most 0.5 s, canceled immediately when a real job arrives) to wake an idle GPU out of low-power clocks before a clip starts.
- **Working directory:** The service runs inside `<state dir>`, not the caller's working directory. Clients send absolute paths plus `relativeTo` for resolving relative asset links inside input files.
- **Local file invalidation:** Local models, maps, textures, meshes, skies, and GUI assets are keyed by file stamp rather than path alone so edits invalidate warm caches automatically; see [local-files.md](local-files.md).

## GPU caches

Idle assets remain resident on the GPU up to per-cache budgets (LRU eviction when exceeded):

| Cache | Default | Environment variable |
|---|---|---|
| Textures | 384 MB | `ROR_TEXTURE_CACHE_MB` |
| Meshes | 256 MB | `ROR_MESH_CACHE_MB` |
| Prepared parts | 256 MB | `ROR_PREPARED_CACHE_MB` |
| Shadow maps | 128 MB | `ROR_SHADOW_CACHE_MB` |
| Skyboxes | 64 MB | `ROR_SKY_CACHE_MB` |
| Lighting environments | 64 MB | `ROR_ENVIRONMENT_CACHE_MB` |

Before each job, the service checks both adapter-wide free VRAM (when available on Windows) and Vulkan's `VK_EXT_memory_budget` (reported under `caches.vram` in `/health`), trimming idle caches if adapter-wide free VRAM drops below 10% or process usage passes 85% of budget (down to 70%). This never blocks a job or changes rendered output. (On Windows, GPU allocations also count against system commit.)

## Memory admission and concurrency limits

To prevent concurrent callers from exhausting RAM or VRAM, the service gates job admission (`crate::budget::admit`). System and process memory accounting is implemented on **Windows only**; Linux and macOS run without memory floors, queues, or caps.

On Windows:
1. **Free-memory floor:** The first job starts only when free system RAM exceeds `ROR_MEMORY_FLOOR_GB` (default: `4 GB`; `0` disables). If unavailable, it waits up to `ROR_MEMORY_WAIT` seconds (default `60`) before failing with a clear breakdown of memory needed vs. available.
2. **Concurrent job sizing:** Additional jobs start only when free RAM covers the floor, in-flight file parses, active job headroom, and the incoming job's estimate (either measured from a prior solo run or estimated via `budget::clip_need` / `budget::still_need`), while keeping the service under 85% of its commit cap.
3. **Cache relief & queuing:** If memory is tight, idle caches are purged first; if still short, the job queues until an active job finishes (notifying the caller once) or times out after `ROR_MEMORY_WAIT` seconds. Active services also re-check and trim caches every 3 s while below the floor.
4. **Hard process cap:** Each `ror` process (plus its `ffmpeg` and `rur` children) runs inside a Windows Job Object capping committed memory at `ROR_MAX_MEMORY_GB` (default: `max(6 GB, 40% of RAM)`; `0` disables) to fail cleanly on OOM instead of thrashing the OS.

Additional guardrails:
- `ROR_MEMORY_FLOOR_GB` and `ROR_MEMORY_WAIT` are forwarded from the invoking CLI command.
- Parsing `.rbxm`/`.rbxl` peaks around ~80x file size, so file loads reserve 100x file size and deduplicate concurrent loads of the same file.
- Thread pools use half of logical cores, capped at 8 threads per pool (override with `RAYON_NUM_THREADS`).
- On Vulkan (`VK_EXT_memory_budget`) or Windows adapter counters, concurrent service draws wait for free VRAM when needed (a solo draw always starts without waiting). Set `ROR_GPU_PRIORITY=low` to request low global Vulkan queue priority where supported.


## HTTP API

The service listens on `127.0.0.1` only. Find the active port in `<state dir>/service.json` (latest) or `<state dir>/services/<build>.json` (per build). Every request must include the header `X-ROR: 1` to block cross-site browser requests ([SECURITY.md](../SECURITY.md)).

| Endpoint | Behavior |
|---|---|
| `GET /health` | Service status, cache stats, running/queued jobs, in-flight loads, and `commitBytes`. |
| `POST /render` | Run a `RenderJob` JSON payload. Send `X-ROR-Offline: 1` to forbid network downloads for this job, `X-ROR-Notes: 1` for streaming `application/x-ror-notes` lines (`{"note": "..."}` progress followed by `{"status": ..., "answer": ...}`), and `X-ROR-Setting: ROR_MEMORY_FLOOR_GB=<GB>` or `ROR_MEMORY_WAIT=<s>` for per-job limits. |
| `POST /batch` | Run `{"jobs": [job, ...]}` concurrently (supports `X-ROR-Offline: 1`); returns `{"results": [...]}`. |
| `POST /video` | Run a `VideoJob` (`crates/ror/src/video.rs`; supports `X-ROR-Offline: 1`). |
| `POST /warm` | Trigger GPU warmup burst; returns `{"ok": true, "warming": bool}`. |
| `POST /shutdown` | Stop the service cleanly. |

Example `RenderJob` (`crates/ror/src/job.rs`):

```json
{
  "input": "C:/models/Model.rbxm",
  "select": null,
  "out": "C:/renders/model",
  "views": [
    { "name": "front", "width": 1024, "height": 1024, "camera": { "mode": "orbit", "yaw": 0, "pitch": 10 } },
    {
      "name": "close",
      "width": 512,
      "height": 512,
      "camera": { "mode": "lookAt", "eye": [0, 5, -8], "target": [0, 4, 0], "fov": 40 },
      "background": "transparent",
      "supersample": 2
    }
  ],
  "materialsFrom": "C:/places/Place.rbxl",
  "lightingFrom": null,
  "noPng": false
}
```

## Benchmarks and profiling

- **Concurrency benchmark:** `python tools/throughput.py <models...> --jobs 32 --rounds 4` (on a 16 GB GPU, 32 concurrent `ror render` processes across 18 models at 1024x1024 completed 72 renders in 0.6 s warm—120–130 renders/s, ~240 ms median round trip).
- **A/B build comparison:** `python tools/perf_suite.py run results.jsonl --bins base=<exe>,work=<exe>`. Windows sampling profiler lives in `tools/perf-sampler`.
- **GPU & load tracing:** Set `ROR_GPU_PROFILE=1` to log per-view GPU pass times in ms (stderr, or `serve.log` in service mode). Set `ROR_PROFILE=1` to log file load waits exceeding 50 ms.
- **RenderDoc captures (Windows):** Run with `--no-service` and `ROR_RENDERDOC=<path/to/renderdoc.dll>`. Select views via `ROR_RENDERDOC_VIEWS=<n>` or `<first>-<last>` (default: first view) and customize filenames with `ROR_RENDERDOC_OUT`.
