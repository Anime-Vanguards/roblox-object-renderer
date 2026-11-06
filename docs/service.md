# The render service

`ror render` and `ror video` send their job to a render service, started on first use and shared by
every process using the same state dir (`.cache`, or `ROR_STATE_DIR`). It keeps the GPU device,
pipelines, meshes, textures and parsed models loaded and runs jobs side by side. The code is in
`crates/ror/src/service` (the service, its HTTP server and client), `crates/ror/src/job.rs` (a render
job) and `crates/ror/src/budget.rs` (memory admission). `--no-service` (or `ROR_NO_SERVICE=1`) runs a
job in the calling process instead.

Run as many `ror render` processes in parallel as you like: they share one GPU device, one set of
caches and the parsed models. A warm 1024² render takes about 20 ms of GPU time; the client round
trip is under 100 ms.

```sh
ror service status        # per build: port, pid, cache sizes, jobs running
ror service stop          # all of this state dir's services
ror service warm          # start it if needed and raise the GPU's clocks for a job to come
ror render Model.rbxm --no-service   # render in this process instead
```

## Lifetime

- A service runs from a copy of its executable in `<state dir>/bin`, so rebuilding ror while it runs
  is fine: it watches the executable that started it and, once a rebuild has replaced that, starts
  the new build's service, which has the old one exit once it is up (`ROR_SERVICE_SUCCESSOR=0` turns
  that off). Checkouts sharing one state dir (different builds) each get their own service.
- 10 s after its last job (`ROR_QUIET_RELEASE=<seconds>`) it hands over to a dormant copy of itself
  that holds the GPU's adapter but no device (~30 MB) and makes its device for the next job in
  0.12-0.25 s. The copy exits 10 minutes after the last job (`ROR_SERVICE_IDLE=<seconds>`);
  `ROR_SERVICE_HANDOVER=0` drops the device in the same process instead.
- It gives memory back as it goes quiet: 2 s after the last job the allocator's free pages and the
  GPU's dropped resources, 10 s after it the kept targets, buffers, preparations, shadow maps, clip
  inputs, GUI images and posed skins, 60 s after it the textures, meshes and sky maps.
- A new service starts in under a second: the GPU device on Vulkan (every backend only without a
  discrete Vulkan GPU), with only its GPU's Vulkan driver loaded once a service has found it
  (`<state dir>/gpu.json`; `ROR_VULKAN_FILTER=0` loads every driver; no ror process loads Vulkan's
  implicit layers, overlays and capture tools unless `ROR_VULKAN_LAYERS=1`), and its pipelines from
  ror's own pipeline cache (`<state dir>/pipelines`; `ROR_PIPELINE_CACHE=0` turns it off).
- `ror service warm` (`POST /warm`) starts the service if needed and keeps the GPU busy for one burst
  of at most 0.5 s, ended as soon as a job starts: a GPU idle a few seconds drops to its idle clocks,
  and a clip after a pause ran its first frames slowly. Nothing runs while the service is otherwise
  idle.
- The service runs in its state dir, never in the folder of the command that started it; the client
  sends every file a job reads as an absolute path and its working directory as the job's
  `relativeTo`, which relative local paths in the input's content are read from.

## Caches

- Meshes and textures stay on the GPU between renders up to a budget of those no render is drawing
  with (textures 384 MB, meshes 256 MB; `ROR_TEXTURE_CACHE_MB`, `ROR_MESH_CACHE_MB`); past it the
  least recently used go. Skies keep their decoded skyboxes (64 MB, `ROR_SKY_CACHE_MB`) and the maps
  made for each Lighting (64 MB, `ROR_ENVIRONMENT_CACHE_MB`); prepared parts and shadow maps
  `ROR_PREPARED_CACHE_MB` (256) and `ROR_SHADOW_CACHE_MB` (128).
- Before each job the service keeps within the GPU memory the system gives it: past 85 % of it, what
  it keeps for later renders goes, least recently used first (`/health` shows `caches.vram`). Each
  byte the service keeps on the GPU also commits a byte of system memory.

## Memory admission

Many callers at once can fill a machine, so jobs are admitted against memory
(`crate::budget::admit`):

1. A process's first job starts once the machine has a floor of memory free (`ROR_MEMORY_FLOOR_GB`,
   default 12 % of the machine's memory and at least 4 GB; 0 turns it off), and fails
   `ROR_MEMORY_WAIT` seconds (60) after it came rather than pile on, saying what it needs, what is
   free, the floor and how to lower it.
2. Every other job starts when the free memory covers the floor, the loads in flight, what the
   running jobs have yet to grow into and its own need, and the process stays under 85 % of its cap.
   A need is what the job makes resident and what it commits: what the same job took the last time it
   ran alone in this process, else a guess from its pixels and its input files' bytes
   (`budget::clip_need`, `budget::still_need`).
3. A job short of that first has the caches let go of what they keep, then queues until a running
   job ends, saying so once to its command, and fails with that explanation `ROR_MEMORY_WAIT` after it
   came, or once its command has gone.
4. While jobs run, a service whose machine falls under the floor has the caches relieved, every 3 s
   while it lasts.
5. On Windows every ror process (and the ffmpeg and rur processes it starts) runs in a job object
   capping its committed memory (`ROR_MAX_MEMORY_GB`, default 40 % of the machine's memory and at
   least 6 GB; 0 turns it off), so one growing past it fails with an out-of-memory error instead of the
   machine paging until it stalls.

`ROR_MEMORY_FLOOR_GB` and `ROR_MEMORY_WAIT` are the command's: `ror render` and `ror video` send
theirs with the job. Parsing a model or place peaks at about 80 times the file's size, so every load
reserves 100 times its input's size while it runs, and jobs missing the same file wait for one load of
it. A ror process works with half the logical processors and at most 8 threads in each of its two
thread pools (`RAYON_NUM_THREADS` sets another number).

Beside a game: if a monitor writes a status file and `ROR_GAMING_STATUS` names it, then while it says
a game runs the floor counts the game's working set as taken and every draw waits until the GPU keeps
10 % of its memory free (`crate::gaming` has the JSON format). `ROR_GPU_PRIORITY=low` makes the
device's queue use Vulkan's low global priority.


## HTTP API

The service speaks HTTP on `127.0.0.1`; the latest service's port is in `.cache/service.json`, each
build's in `.cache/services/<build>.json`. Every request needs the header `X-ROR: 1` (it keeps
browsers out; see [SECURITY.md](../SECURITY.md)).

- `GET /health`: status, caches, admission (running and queued jobs, their needs), loads in flight,
  `commitBytes`.
- `POST /render` with a job: its result. With `X-ROR-Notes: 1` what the job says before it ends (a wait
  for memory) comes first: an answer of type `application/x-ror-notes`, a JSON line `{"note": "..."}`
  each, then `{"status": <HTTP status>, "answer": <the result>}`. `X-ROR-Setting:
  ROR_MEMORY_FLOOR_GB=<GB>` and `ROR_MEMORY_WAIT=<s>` lines are that job's settings.
- `POST /batch` with `{"jobs": [job, ...]}`: `{"results": [...]}`, the jobs run concurrently.
- `POST /video` with a video job (`crates/ror/src/video.rs` `VideoJob`): its result.
- `POST /warm`: `{"ok": true, "warming": <a burst began>}`.
- `POST /shutdown`.

A job is the JSON form of the CLI options (`crates/ror/src/job.rs` `RenderJob`):

```json
{
  "input": "C:/models/Model.rbxm",
  "select": null,
  "out": "C:/renders/model",
  "views": [
    {"name": "front", "width": 1024, "height": 1024, "camera": {"mode": "orbit", "yaw": 0, "pitch": 10}},
    {"name": "close", "width": 512, "height": 512, "camera": {"mode": "lookAt", "eye": [0, 5, -8], "target": [0, 4, 0], "fov": 40},
     "background": "transparent", "supersample": 2}
  ],
  "materialsFrom": "C:/places/Place.rbxl",
  "lightingFrom": null,
  "noPng": false
}
```

## Throughput and profiling

`python tools/throughput.py <models...> --jobs 32 --rounds 4` measures many concurrent callers: on a
test machine with a 16 GB GPU, 32 concurrent `ror render` processes over eighteen models at 1024²
finished 72 renders in 0.6 s once warm (120-130 renders/s; median client round trip ~240 ms with 32
in flight). `python tools/perf_suite.py run results.jsonl --bins base=<exe>,work=<exe>` times builds
against each other (its docstring lists the inputs and the machine guards), and `tools/perf-sampler`
is a sampling profiler for Windows.

`ROR_GPU_PROFILE=1` prints per-view GPU pass times in milliseconds to stderr (a service writes them
to its `serve.log`); `ROR_PROFILE=1` logs each wait for a load over 50 ms.
`ROR_RENDERDOC=<path to renderdoc.dll>` (with `--no-service`) captures views for RenderDoc: each
view's submission is one capture, `ROR_RENDERDOC_VIEWS=<n>` or `<first>-<last>` picks which (default
the first), `ROR_RENDERDOC_OUT` sets the file template.
