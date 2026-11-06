"""Many agents at once: renders the given models `--rounds` times each, `--jobs` concurrent `ror render`
processes through the warm service, and reports wall time, renders per second and per-render latency
(client wall clock and the service's render time).

    python tools/throughput.py <model> ... [--jobs 16] [--rounds 3] [--size 1024x1024]
"""

import concurrent.futures
import json
import os
import subprocess
import sys
import time

ROR = os.path.join("target", "release", "ror.exe")


def main():
	args = sys.argv[1:]

	def option(name, default):
		if name in args:
			index = args.index(name)
			value = args[index + 1]
			del args[index : index + 2]
			return value
		return default

	jobs = int(option("--jobs", "16"))
	rounds = int(option("--rounds", "3"))
	size = option("--size", "1024x1024")
	models = args
	tasks = [(model, round_index) for round_index in range(rounds) for model in models]

	def render(task):
		model, round_index = task
		out = os.path.join(".cache", "throughput", f"{os.path.basename(model)}-{round_index}-{time.time_ns()}")
		start = time.time()
		result = subprocess.run([ROR, "render", model, "--size", size, "--views", "default", "--json", "--no-png", "--out", out], capture_output=True, text=True)
		wall = time.time() - start
		if result.returncode != 0:
			return model, wall, None
		summary = json.loads(result.stdout.strip().splitlines()[-1])
		return model, wall, summary["timings"]["renderMs"]

	# one render first so the service is up and warm with every model
	start = time.time()
	with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
		results = list(pool.map(render, tasks))
	elapsed = time.time() - start
	failures = sum(1 for _, _, render_ms in results if render_ms is None)
	walls = sorted(wall for _, wall, render_ms in results if render_ms is not None)
	renders = sorted(render_ms for _, _, render_ms in results if render_ms is not None)
	print(f"{len(tasks)} renders ({len(models)} models x {rounds}) at {size}, {jobs} at a time: {elapsed:.1f} s, {len(tasks) / elapsed:.1f} renders/s, {failures} failed")
	if walls:
		pick = lambda values, q: values[min(len(values) - 1, int(q * len(values)))]
		print(f"client wall per render: median {pick(walls, 0.5) * 1000:.0f} ms, p90 {pick(walls, 0.9) * 1000:.0f} ms")
		print(f"service render time:    median {pick(renders, 0.5):.0f} ms, p90 {pick(renders, 0.9):.0f} ms")


main()
