"""ror's performance suite: startup, stills, videos, parallel use and the cost grid, timed with their
CPU, commit and GPU memory, against one or more builds at once.

    python tools/perf_suite.py run <results.jsonl> --bins base=<exe>,work=<exe> [--reps 3] [--suite nosvc,svc,par,verify] [--only a,b]
    python tools/perf_suite.py report <results.jsonl> [base,work]     # median (min-max) per metric
    python tools/perf_suite.py same <results.jsonl> base work         # output hashes compared per workload
    python tools/perf_suite.py canon <a.json> <b.json> [--drop timings]   # JSON as content, lists as multisets
    python tools/perf_suite.py profile <exe> <workload> [seconds] [interval_ms] [top]

Needs `pip install psutil pywin32` (Windows). Each run samples its process tree and its build's own
render service (found through <state>/services/<build>.json) every 100 ms: private bytes (commit),
GPU dedicated memory (PDH), CPU (core-seconds and the busiest 100 ms / 500 ms). --no-service runs
also parse ROR_MEMORY_REPORT's exact peak commit. Binaries alternate within each rep so machine drift
hits them alike; copy each build's ror.exe somewhere first (a copy has its own service, so stopping
it between suites never touches the checkout's). Warm a new copy with one unrecorded pass: NVIDIA's
driver keeps a pipeline cache per executable, and a fresh one's first runs start 30-50 ms slower.

A run starts only with 12 GB of RAM free and while nvidia-smi shows less GPU memory used than
--gpu-limit (MiB). The inputs are a character model, a map (as one model file, its place for lighting
and materials, a folder of `ror cost` views of it and a recorded camera track), a recorded play session
with GUI and an effects bundle: ROR_PERF_<NAME> (SHAPES, UNIT, MAP, PLACE, VIEWS, TRACK, SESSION, VFX,
and VFX_SELECT, the effect's dotted path) names each, by default under perf-inputs/ in the checkout;
workloads whose inputs are missing are skipped. Output goes to ROR_PERF_OUT (default: OUT below).
`profile` attaches tools/perf-sampler (`cargo build --release --manifest-path
tools/perf-sampler/Cargo.toml`), a user-mode sampling profiler, to one --no-service run.
"""

import hashlib
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

import psutil
import win32pdh

MAIN = Path(__file__).resolve().parents[1]
STATE = Path(os.environ.get("ROR_STATE_DIR") or MAIN / ".cache")
OUT = Path(os.environ.get("ROR_PERF_OUT") or MAIN / "out" / "perf")
INPUT_DIR = MAIN / "perf-inputs"


def _input(name, default):
	return Path(os.environ.get(f"ROR_PERF_{name}") or default)


SHAPES = _input("SHAPES", MAIN / "fixtures" / "shapes.rbxmx")
# a character model, and a map as one model file
UNIT = _input("UNIT", INPUT_DIR / "unit.rbxm")
MAP_MODEL = _input("MAP", INPUT_DIR / "map.rbxm")
# the map's place (lighting and materials), views of it (a `ror cost` output) and a recorded camera track
MAP_PLACE = _input("PLACE", INPUT_DIR / "map.rbxl")
MAP_VIEWS = _input("VIEWS", INPUT_DIR / "map-views")
MAP_TRACK = _input("TRACK", INPUT_DIR / "camera-track.json")
# a recorded play session with GUI: scene.rbxm, playback.json, camera.json
SESSION = _input("SESSION", INPUT_DIR / "session")
# an effects bundle and the dotted path of the effect it plays
VFX = _input("VFX", INPUT_DIR / "effects.rbxm")
VFX_SELECT = os.environ.get("ROR_PERF_VFX_SELECT") or "Effect"
INPUTS = {str(path) for path in (SHAPES, UNIT, MAP_MODEL, MAP_PLACE, MAP_VIEWS, MAP_TRACK, SESSION / "scene.rbxm", SESSION / "playback.json", SESSION / "camera.json", VFX)}
TELE_CAMERA = json.dumps({"mode": "lookAt", "eye": [36.05887, 37.05836, -35.05887], "target": [30.28537, 31.28486, -29.28537], "fov": 70})

MAP = ["--lighting-from", str(MAP_PLACE), "--materials-from", str(MAP_PLACE)]
STILL_CAMERA = json.dumps({"mode": "cFrame", "components": [-209.78845, 198.8, -263.27557, -1.0, 0.0, -0.0, 0.0, 0.90044713, 0.43496555, 0.0, 0.43496555, -0.90044713], "fov": 70.0})
WORKLOADS = {
	"tiny": ["render", str(SHAPES), "--views", "iso", "--size", "64x64"],
	"unit": ["render", str(UNIT), "--views", "front,iso,back", "--size", "1024x1024"],
	"maps": ["render", str(MAP_MODEL), *MAP, "--camera-from", str(MAP_VIEWS), "--size", "1280x720"],
	"gui": ["render", str(SESSION / "scene.rbxm"), "--playback", str(SESSION / "playback.json"), "--time", "1.0", "--camera", TELE_CAMERA, "--size", "1920x1080"],
	"video-map": ["video", str(MAP_MODEL), *MAP, "--camera-track", str(MAP_TRACK), "--start", "10", "--duration", "2", "--size", "1920x1080", "--sheet", "12"],
	# a still camera in the map, as an effect shown in a match map: the sun's shadows drawn once
	"video-still": ["video", str(MAP_MODEL), *MAP, "--camera", STILL_CAMERA, "--duration", "2", "--size", "1920x1080", "--sheet", "12"],
	"video-vfx": ["video", str(VFX), "--select", VFX_SELECT, "--duration", "2", "--size", "1920x1080", "--view", "iso", "--sheet", "12"],
	"video-gui": ["video", str(SESSION / "scene.rbxm"), "--playback", str(SESSION / "playback.json"), "--camera-track", str(SESSION / "camera.json"), "--duration", "2.6", "--fps", "30", "--size", "1920x1080", "--sheet", "12"],
	"grid": ["cost", str(MAP_MODEL), "--grid", "20", "--technology", "future", "--no-images"],
	# frames as PNGs: the byte-for-byte check of what videos draw
	"frames-map": ["video", str(MAP_MODEL), *MAP, "--camera-track", str(MAP_TRACK), "--start", "10", "--duration", "0.5", "--size", "1280x720", "--format", "png", "--sheet", "0"],
	"frames-vfx": ["video", str(VFX), "--select", VFX_SELECT, "--duration", "1", "--fps", "15", "--size", "1280x720", "--view", "iso", "--format", "png", "--sheet", "0"],
	"frames-gui": ["video", str(SESSION / "scene.rbxm"), "--playback", str(SESSION / "playback.json"), "--camera-track", str(SESSION / "camera.json"), "--duration", "2.6", "--fps", "5", "--size", "1280x720", "--format", "png", "--sheet", "0"],
}
PARALLEL = {
	"par4-unit": [("unit", True)] * 4,
	"par4-mixed": [("unit", True), ("gui", True), ("video-vfx", True), ("maps", True)],
	"par4-video": [("video-map", True), ("video-gui", True), ("video-vfx", True), ("maps", True)],
	"proc2-unit": [("unit", False)] * 2,
}


def missing(workload):
	"""The inputs a workload (or a parallel set's workloads) needs and this machine lacks."""
	names = [name for name, _ in PARALLEL[workload]] if workload in PARALLEL else [workload]
	return sorted({arg for name in names for arg in WORKLOADS[name] if arg in INPUTS and not Path(arg).exists()})


def version():
	match = re.search(r'^version\s*=\s*"([^"]+)"', (MAIN / "crates" / "ror" / "Cargo.toml").read_text(encoding="utf-8"), re.M)
	return match.group(1) if match else "0.1.0"


SUITES = {
	"nosvc": ["tiny", "unit", "maps", "gui", "video-map", "video-still", "video-vfx", "video-gui", "grid"],
	"svc": ["cold", "tiny", "unit", "maps", "gui", "video-map", "video-still", "video-vfx", "video-gui", "grid", "idle"],
	"par": ["par4-unit", "par4-mixed", "par4-video", "proc2-unit"],
	"verify": ["unit", "maps", "gui", "frames-map", "frames-vfx", "frames-gui"],
}


def base_env():
	env = dict(os.environ)
	env["ROR_STATE_DIR"] = str(STATE)
	env["ROR_RUR"] = str(MAIN.parent / "roblox-ui-renderer")
	env.pop("ROR_MEMORY_REPORT", None)
	return env


def build_id(exe):
	st = os.stat(exe)
	return f"{version()}-{st.st_size}-{int(st.st_mtime_ns // 1_000_000)}"


def service_info(exe):
	path = STATE / "services" / f"{build_id(exe)}.json"
	try:
		info = json.loads(path.read_text())
		proc = psutil.Process(info["pid"])
		if proc.is_running():
			return info
	except Exception:
		pass
	return None


def shutdown_service(exe):
	"""Asks this build's own service (by its recorded port) to exit, and waits for its pid."""
	info = service_info(exe)
	if not info:
		return
	try:
		req = urllib.request.Request(f"http://127.0.0.1:{info['port']}/shutdown", data=b"{}", method="POST", headers={"X-ROR": "1"})
		urllib.request.urlopen(req, timeout=5).read()
	except Exception:
		pass
	try:
		psutil.Process(info["pid"]).wait(timeout=60)
	except Exception:
		pass


def free_gb():
	return psutil.virtual_memory().available / 2**30


def gpu_used_mib():
	try:
		out = subprocess.run(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"], capture_output=True, text=True, timeout=10).stdout
		return int(out.strip().splitlines()[0])
	except Exception:
		return 0


GPU_LIMIT = [9500]


def gate():
	for _ in range(120):
		free, gpu = free_gb(), gpu_used_mib()
		if free >= 12 and gpu < GPU_LIMIT[0]:
			return free, gpu
		print(f"  waiting: {free:.1f} GB free, GPU {gpu} MiB used", flush=True)
		time.sleep(10)
	raise SystemExit("machine too busy")


class Sampler:
	"""Samples a root pid's tree plus extra pids (the service) every 100 ms."""

	def __init__(self, root=None, extra=()):
		self.root = root
		self.extra = list(extra)
		self.stop = threading.Event()
		self.cpu = {}  # pid -> last cpu seconds seen
		self.cpu0 = {}
		self.samples = []  # (t, private, gpu, cpu_total)
		self.query = win32pdh.OpenQuery()
		self.counter = win32pdh.AddEnglishCounter(self.query, r"\GPU Process Memory(*)\Dedicated Usage")
		self.thread = threading.Thread(target=self.loop, daemon=True)

	def pids(self):
		pids = set(self.extra)
		if self.root is not None:
			try:
				p = psutil.Process(self.root)
				pids.add(p.pid)
				pids.update(c.pid for c in p.children(recursive=True))
			except Exception:
				pass
		return pids

	def sample(self):
		private = 0
		for pid in self.pids():
			try:
				p = psutil.Process(pid)
				with p.oneshot():
					mi = p.memory_info()
					ct = p.cpu_times()
				private += mi.private
				total = ct.user + ct.system
				if pid not in self.cpu0:
					# a pid seen first now: a child started since the last sample started from zero; the service (extra) from its value now
					self.cpu0[pid] = total if pid in self.extra else 0.0
				self.cpu[pid] = total
			except Exception:
				pass
		gpu = 0
		try:
			win32pdh.CollectQueryData(self.query)
			items = win32pdh.GetFormattedCounterArray(self.counter, win32pdh.PDH_FMT_LARGE)
			mine = self.pids()
			for name, value in items.items():
				if name.startswith("pid_"):
					pid = int(name.split("_")[1])
					if pid in mine:
						gpu += value
		except Exception:
			pass
		cpu_total = sum(self.cpu[p] - self.cpu0[p] for p in self.cpu)
		self.samples.append((time.perf_counter(), private, gpu, cpu_total))

	def loop(self):
		while not self.stop.is_set():
			self.sample()
			self.stop.wait(0.1)

	def start(self):
		self.sample()
		self.thread.start()
		return self

	def finish(self):
		self.stop.set()
		self.thread.join()
		self.sample()
		s = self.samples
		peak_cores = 0.0
		burst = 0.0
		for i in range(1, len(s)):
			dt = s[i][0] - s[i - 1][0]
			if dt > 0.05:
				peak_cores = max(peak_cores, (s[i][3] - s[i - 1][3]) / dt)
		j = 0
		for i in range(len(s)):
			while s[i][0] - s[j][0] > 0.5:
				j += 1
			if i > j and s[i][0] - s[j][0] >= 0.4:
				burst = max(burst, (s[i][3] - s[j][3]) / (s[i][0] - s[j][0]))
		return {
			"peak_private_mb": round(max(x[1] for x in s) / 2**20),
			"end_private_mb": round(s[-1][1] / 2**20),
			"peak_gpu_mb": round(max(x[2] for x in s) / 2**20),
			"core_s": round(s[-1][3], 2),
			"peak_cores": round(peak_cores, 1),
			"burst500_cores": round(burst, 1),
		}


def hash_outputs(folder):
	hashes = {}
	if not folder.exists():
		return hashes
	for path in sorted(folder.rglob("*")):
		if path.suffix.lower() in (".png",) and path.is_file():
			hashes[str(path.relative_to(folder)).replace("\\", "/")] = hashlib.sha256(path.read_bytes()).hexdigest()[:16]
	return hashes


def parse_memory(text):
	for line in text.splitlines()[::-1]:
		if line.startswith("ror memory: peak commit"):
			parts = line.split()
			return int(parts[4]), int(parts[9])
	return None, None


def parse_json(text):
	for line in text.splitlines()[::-1]:
		line = line.strip()
		if line.startswith("{"):
			try:
				return json.loads(line)
			except Exception:
				pass
	return None


def run_one(exe, label, workload, service, rep, extra_env=None):
	args = WORKLOADS[workload]
	out = OUT / label / f"{workload}-{rep}"
	if out.exists():
		import shutil
		shutil.rmtree(out, ignore_errors=True)
	cmd = [exe, *args, "--out", str(out), "--json"]
	env = base_env()
	if extra_env:
		env.update(extra_env)
	if not service:
		cmd.append("--no-service")
		env["ROR_MEMORY_REPORT"] = "1"
	info = service_info(exe) if service else None
	start = time.perf_counter()
	proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8", errors="replace")
	sampler = Sampler(proc.pid, [info["pid"]] if info else []).start()
	stdout, stderr = proc.communicate()
	wall = time.perf_counter() - start
	stats = sampler.finish()
	record = {"workload": workload, "mode": "svc" if service else "nosvc", "wall_s": round(wall, 3), "exit": proc.returncode, **stats}
	commit, ws = parse_memory(stderr)
	if commit is not None:
		record["peak_commit_mb"] = commit
		record["peak_ws_mb"] = ws
	result = parse_json(stdout)
	if result:
		timings = result.get("timings") or {}
		record["timings"] = {k: v for k, v in timings.items() if isinstance(v, (int, float))}
		if "frames" in result:
			record["frames"] = result["frames"] if isinstance(result["frames"], int) else len(result["frames"])
	if info:
		try:
			mi = psutil.Process(info["pid"]).memory_info()
			record["svc_after_mb"] = round(mi.private / 2**20)
			record["svc_peak_commit_mb"] = round(mi.peak_pagefile / 2**20)
		except Exception:
			pass
	record["hashes"] = hash_outputs(out)
	if proc.returncode != 0:
		record["stderr"] = stderr[-2000:]
	return record


def run_parallel(exe, label, kind, rep):
	"""Concurrent clients on this build's warm service, or separate --no-service processes."""
	jobs = PARALLEL[kind]
	service = jobs[0][1]
	info = service_info(exe) if service else None
	procs = []
	start = time.perf_counter()
	for i, (workload, svc) in enumerate(jobs):
		out = OUT / label / f"{kind}-{rep}-{i}"
		cmd = [exe, *WORKLOADS[workload], "--out", str(out), "--json"]
		env = base_env()
		if not svc:
			cmd.append("--no-service")
			env["ROR_MEMORY_REPORT"] = "1"
		procs.append((workload, out, subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True, encoding="utf-8", errors="replace")))
	# one sampler over every client and the service
	sampler = Sampler(None, [p.pid for _, _, p in procs] + ([info["pid"]] if info else [])).start()
	walls, commits, codes = [], [], []
	for workload, out, p in procs:
		stdout, stderr = p.communicate()
		walls.append(round(time.perf_counter() - start, 3))
		codes.append(p.returncode)
		c, _ = parse_memory(stderr)
		if c:
			commits.append(c)
	stats = sampler.finish()
	record = {"workload": kind, "mode": "par", "wall_s": max(walls), "walls": walls, "exit": max(codes), **stats}
	if commits:
		record["sum_peak_commit_mb"] = sum(commits)
	if info:
		try:
			mi = psutil.Process(info["pid"]).memory_info()
			record["svc_after_mb"] = round(mi.private / 2**20)
		except Exception:
			pass
	record["hashes"] = {}
	for i, (workload, out, _) in enumerate(procs):
		for k, v in hash_outputs(out).items():
			record["hashes"][f"{i}-{workload}/{k}"] = v
	return record


def idle_probe(exe):
	info = service_info(exe)
	if not info:
		return {"workload": "idle", "mode": "svc", "error": "no service"}
	p = psutil.Process(info["pid"])
	record = {"workload": "idle", "mode": "svc"}
	t0 = time.perf_counter()
	cpu0 = sum(p.cpu_times()[:2])
	for at in (1, 4, 13, 40, 70):
		while time.perf_counter() - t0 < at:
			time.sleep(0.2)
		record[f"private_{at}s_mb"] = round(p.memory_info().private / 2**20)
	record["idle_core_s"] = round(sum(p.cpu_times()[:2]) - cpu0, 2)
	record["svc_peak_commit_mb"] = round(p.memory_info().peak_pagefile / 2**20)
	return record


def cmd_run(argv):
	import argparse
	ap = argparse.ArgumentParser()
	ap.add_argument("results")
	ap.add_argument("--bins", required=True)
	ap.add_argument("--reps", type=int, default=3)
	ap.add_argument("--suite", default="nosvc,svc,par")
	ap.add_argument("--only", default="")
	ap.add_argument("--env", default="", help="label:NAME=VALUE;... extra environment per bin label")
	ap.add_argument("--gpu-limit", type=int, default=9500, help="start a run only while nvidia-smi shows less GPU memory used (MiB)")
	a = ap.parse_args(argv)
	GPU_LIMIT[0] = a.gpu_limit
	bins = [(label, str(Path(exe).resolve())) for label, exe in (b.split("=", 1) for b in a.bins.split(","))]
	only = set(filter(None, a.only.split(",")))
	extra = {}
	for part in filter(None, a.env.split(";")):
		label, kv = part.split(":", 1)
		k, v = kv.split("=", 1)
		extra.setdefault(label, {})[k] = v
	results = open(a.results, "a", encoding="utf-8")
	for w in sorted(set(WORKLOADS) | set(PARALLEL)):
		if missing(w):
			print(f"skipping {w}: missing {', '.join(missing(w))}", flush=True)

	def emit(record, label, rep):
		record.update({"bin": label, "rep": rep, "at": time.strftime("%H:%M:%S")})
		results.write(json.dumps(record) + "\n")
		results.flush()
		brief = {k: record.get(k) for k in ("wall_s", "peak_commit_mb", "peak_private_mb", "peak_gpu_mb", "core_s", "peak_cores", "svc_after_mb", "exit") if k in record}
		if record["workload"] == "idle":
			brief = {k: v for k, v in record.items() if k.startswith("private_") or k in ("idle_core_s", "svc_peak_commit_mb")}
		print(f"{label:>6} r{rep} {record['mode']:>5} {record['workload']:<11} {brief}", flush=True)

	for rep in range(a.reps):
		for suite in a.suite.split(","):
			order = bins if rep % 2 == 0 else bins[::-1]
			for label, exe in order:
				env = extra.get(label)
				if suite in ("nosvc", "verify"):
					for w in SUITES[suite]:
						if (only and w not in only) or missing(w):
							continue
						gate()
						emit(run_one(exe, label, w, False, rep, env), label, rep)
				elif suite == "svc":
					shutdown_service(exe)
					for w in SUITES["svc"]:
						if (only and w not in only and w not in ("cold", "idle")) or (w not in ("cold", "idle") and missing(w)):
							continue
						if w == "idle":
							emit(idle_probe(exe), label, rep)
							continue
						gate()
						rec = run_one(exe, label, "tiny" if w == "cold" else w, True, rep, env)
						if w == "cold":
							rec["workload"] = "cold"
						emit(rec, label, rep)
					shutdown_service(exe)
				elif suite == "par":
					shutdown_service(exe)
					# warm the service with the jobs' inputs first
					for w in ("unit", "gui", "video-vfx", "maps", "video-map", "video-gui"):
						if not missing(w):
							run_one(exe, label, w, True, "warm", env)
					for kind in SUITES["par"]:
						if (only and kind not in only) or missing(kind):
							continue
						gate()
						emit(run_parallel(exe, label, kind, rep), label, rep)
					shutdown_service(exe)


def load(path):
	return [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]


def stat(values):
	values = sorted(v for v in values if v is not None)
	if not values:
		return None
	n = len(values)
	med = values[n // 2] if n % 2 else (values[n // 2 - 1] + values[n // 2]) / 2
	return med, values[0], values[-1]


def fmt(s, unit=""):
	if s is None:
		return "-"
	med, lo, hi = s
	if isinstance(med, float) and med < 100:
		return f"{med:.2f} ({lo:.2f}-{hi:.2f})"
	return f"{med:.0f} ({lo:.0f}-{hi:.0f})"


def cmd_report(argv):
	rows = load(argv[0])
	bins = argv[1].split(",") if len(argv) > 1 else sorted({r["bin"] for r in rows})
	keys = ["wall_s", "peak_commit_mb", "peak_private_mb", "peak_gpu_mb", "core_s", "peak_cores", "burst500_cores", "svc_after_mb", "sum_peak_commit_mb"]
	groups = {}
	for r in rows:
		groups.setdefault((r["mode"], r["workload"]), {}).setdefault(r["bin"], []).append(r)
	for (mode, workload), by in sorted(groups.items()):
		print(f"\n{mode} {workload}")
		ks = keys if workload != "idle" else sorted({k for b in by.values() for r in b for k in r if k.startswith("private_") or k in ("idle_core_s", "svc_peak_commit_mb")})
		for k in ks:
			cells = []
			for b in bins:
				s = stat([r.get(k) for r in by.get(b, [])])
				cells.append(fmt(s))
			if any(c != "-" for c in cells):
				print(f"  {k:<20} " + "  |  ".join(f"{b}: {c}" for b, c in zip(bins, cells)))
		fails = [(b, r["rep"], r.get("stderr", "")[-300:]) for b in bins for r in by.get(b, []) if r.get("exit") not in (0, None)]
		for f in fails:
			print("  FAILED", f)


def cmd_same(argv):
	rows = load(argv[0])
	a, b = argv[1], argv[2]
	by = {}
	for r in rows:
		by.setdefault((r["mode"], r["workload"]), {}).setdefault(r["bin"], []).append(r["hashes"] if "hashes" in r else {})
	for key, d in sorted(by.items()):
		if a not in d or b not in d:
			continue
		ha, hb = d[a], d[b]
		# within-bin stability and across bins
		def norm(h):
			return {k.split("/", 1)[-1] if k[0].isdigit() and "-" in k.split("/")[0] else k: v for k, v in h.items()}
		sa = [norm(h) for h in ha]
		sb = [norm(h) for h in hb]
		stable_a = all(x == sa[0] for x in sa)
		stable_b = all(x == sb[0] for x in sb)
		diff = [k for k in sa[0] if sb and sa[0].get(k) != sb[0].get(k)]
		print(f"{key[0]:>6} {key[1]:<12} files {len(sa[0])}: {a} stable {stable_a}, {b} stable {stable_b}, differ {len(diff)} {diff[:6]}")




def canon(value, drop):
	if isinstance(value, dict):
		return {k: canon(v, drop) for k, v in sorted(value.items()) if k not in drop}
	if isinstance(value, list):
		items = [canon(v, drop) for v in value]
		return sorted(items, key=lambda item: json.dumps(item, sort_keys=True))
	return value


def diff(a, b, path, out, limit=8):
	if len(out) >= limit:
		return
	if type(a) is not type(b):
		out.append(f"{path}: type {type(a).__name__} vs {type(b).__name__}")
	elif isinstance(a, dict):
		for k in sorted(set(a) | set(b)):
			if k not in a or k not in b:
				out.append(f"{path}.{k}: only in {'b' if k not in a else 'a'}")
			else:
				diff(a[k], b[k], f"{path}.{k}", out, limit)
	elif isinstance(a, list):
		if len(a) != len(b):
			out.append(f"{path}: {len(a)} vs {len(b)} items")
		for i, (x, y) in enumerate(zip(a, b)):
			if x != y:
				diff(x, y, f"{path}[{i}]", out, limit)
	elif a != b:
		out.append(f"{path}: {str(a)[:120]} vs {str(b)[:120]}")


def cmd_canon(argv):
	"""Two JSON files compared as content: every list a multiset (cost.json's tie order differs
	between two runs of one build), objects by key, --drop keys (timings) left out."""
	args = list(argv)
	drop = set()
	if "--drop" in args:
		i = args.index("--drop")
		drop = set(args[i + 1].split(","))
		del args[i : i + 2]
	a, b = (canon(json.load(open(p, encoding="utf-8")), drop) for p in args[:2])
	if a == b:
		print("same content")
		return
	out = []
	diff(a, b, "$", out)
	print("DIFFERENT")
	for line in out:
		print(" ", line)
	sys.exit(1)


def cmd_profile(argv):
	"""One --no-service run of a workload with tools/perf-sampler attached for its whole run."""
	exe = str(Path(argv[0]).resolve())
	workload = argv[1]
	seconds, interval, top = (argv[2:] + ["10", "5", "40"][len(argv[2:]) :])[:3]
	sampler = MAIN / "tools" / "perf-sampler" / "target" / "release" / "sampler.exe"
	if not sampler.exists():
		raise SystemExit(f"build the sampler first: cargo build --release --manifest-path {MAIN / 'tools' / 'perf-sampler' / 'Cargo.toml'}")
	out = OUT / "profile" / workload
	proc = subprocess.Popen([exe, *WORKLOADS[workload], "--out", str(out), "--json", "--no-service"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=base_env())
	# the target's PDB beside it or in its build's target/release
	env = dict(os.environ, SAMPLER_SYMPATH=os.environ.get("SAMPLER_SYMPATH") or str(Path(exe).parent))
	result = subprocess.run([str(sampler), str(proc.pid), seconds, interval, top], capture_output=True, text=True, env=env)
	proc.wait()
	print(result.stdout)
	print(result.stderr[-2000:])


if __name__ == "__main__":
	cmd = sys.argv[1] if len(sys.argv) > 1 else ""
	commands = {"run": cmd_run, "report": cmd_report, "same": cmd_same, "canon": cmd_canon, "profile": cmd_profile}
	if cmd not in commands:
		raise SystemExit(__doc__)
	commands[cmd](sys.argv[2:])
