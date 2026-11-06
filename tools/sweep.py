"""Robustness sweep: renders every model / place file under the given directories through the render
service (in parallel, like many agents would), and reports failures, missing assets and timings.

    python tools/sweep.py <dir or file> ... [--jobs 8] [--size 512x512]
"""

import concurrent.futures
import json
import os
import subprocess
import sys
import time

ROR = os.path.join("target", "release", "ror.exe")


def files(paths):
	for path in paths:
		if os.path.isfile(path):
			yield path
			continue
		for root, _, names in os.walk(path):
			for name in names:
				if name.lower().endswith((".rbxm", ".rbxmx", ".rbxl", ".rbxlx")):
					yield os.path.join(root, name)


def render(path, size, index):
	start = time.time()
	out = os.path.join(".cache", "sweep", f"{index:03d}")
	result = subprocess.run([ROR, "render", path, "--size", size, "--views", "iso", "--json", "--out", out], capture_output=True, text=True, timeout=600)
	elapsed = time.time() - start
	if result.returncode != 0:
		return path, elapsed, None, (result.stderr or result.stdout).strip().splitlines()[-1:] if (result.stderr or result.stdout) else ["exit %d" % result.returncode]
	try:
		summary = json.loads(result.stdout.strip().splitlines()[-1])
	except Exception as error:
		return path, elapsed, None, [f"bad json: {error}"]
	return path, elapsed, summary, []


def main():
	args = sys.argv[1:]
	jobs = 8
	size = "512x512"
	if "--jobs" in args:
		jobs = int(args[args.index("--jobs") + 1])
		del args[args.index("--jobs") : args.index("--jobs") + 2]
	if "--size" in args:
		size = args[args.index("--size") + 1]
		del args[args.index("--size") : args.index("--size") + 2]
	paths = list(files(args))
	print(f"{len(paths)} files, {jobs} at a time")
	failures = 0
	missing_total = 0
	with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
		for path, elapsed, summary, errors in pool.map(lambda item: render(item[1], size, item[0]), enumerate(paths)):
			if errors:
				failures += 1
				print(f"FAIL {elapsed:5.1f}s {path}: {' '.join(errors)[:300]}")
				continue
			missing = summary["stats"]["missing"]
			missing_total += len(missing)
			note = f" missing {len(missing)}: {missing[0][:120]}" if missing else ""
			print(f"ok   {elapsed:5.1f}s parts {summary['parts']:5d} render {summary['timings']['renderMs']:7.1f} ms {os.path.basename(path)}{note}")
	print(f"{len(paths) - failures}/{len(paths)} rendered, {missing_total} missing assets")


main()
