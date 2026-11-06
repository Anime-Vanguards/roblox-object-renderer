"""Scores every calibration run's ror.png against its studio.png, and compares two such scorecards:
which runs got better or worse between two builds (re-render with `ROR_RERENDER=1 ror studio
calibrate <experiment>`, which needs no Studio, between the two snapshots).

    python tools/analysis/scorecard.py snapshot <out.json> [experiment prefixes...]
    python tools/analysis/scorecard.py compare <before.json> <after.json> [--worse 0.5] [--top 30]

A run is any folder under out/calibrate holding both images (experiments' runs, sweeps' and
timelines' frames). Its score: the share of pixels whose largest channel difference is within 2
and within 8 of 255, and the mean absolute difference.
"""

import json
import os
import sys

import numpy as np
from PIL import Image

ROOT = os.path.join("out", "calibrate")


def runs(prefixes):
	for experiment in sorted(os.listdir(ROOT)):
		if prefixes and not any(experiment.startswith(prefix) for prefix in prefixes):
			continue
		top = os.path.join(ROOT, experiment)
		if not os.path.isdir(top):
			continue
		for directory, _, files in os.walk(top):
			if "studio.png" in files and "ror.png" in files:
				yield os.path.relpath(directory, ROOT).replace("\\", "/")


def score(run):
	directory = os.path.join(ROOT, run)
	studio = np.asarray(Image.open(os.path.join(directory, "studio.png")).convert("RGB"), dtype=np.int16)
	ror = np.asarray(Image.open(os.path.join(directory, "ror.png")).convert("RGB"), dtype=np.int16)
	if studio.shape != ror.shape:
		return None
	largest = np.abs(ror - studio).max(axis=2)
	return {"within2": float((largest <= 2).mean() * 100), "within8": float((largest <= 8).mean() * 100), "mae": float(np.abs(ror - studio).mean())}


def snapshot(out, prefixes):
	scores = {}
	for run in runs(prefixes):
		try:
			result = score(run)
		except (OSError, ValueError) as error:
			print(f"{run}: {error}", file=sys.stderr)
			continue
		if result is not None:
			scores[run] = result
	with open(out, "w") as file:
		json.dump(scores, file, indent=1, sort_keys=True)
	print(f"{len(scores)} runs scored -> {out}")


def compare(before_path, after_path, worse, top):
	before = json.load(open(before_path))
	after = json.load(open(after_path))
	common = sorted(set(before) & set(after))
	changes = [(after[run]["within2"] - before[run]["within2"], run) for run in common]
	by_experiment = {}
	for change, run in changes:
		by_experiment.setdefault(run.split("/")[0], []).append(change)
	print(f"{len(common)} runs in both ({len(set(before) - set(after))} only before, {len(set(after) - set(before))} only after)")
	moved = [(sum(values) / len(values), name, len(values)) for name, values in by_experiment.items() if any(abs(value) >= worse for value in values)]
	print(f"experiments with a run moving {worse} points or more (mean change in within-2 %):")
	for mean, name, count in sorted(moved):
		print(f"  {mean:+7.2f}  {name} ({count} runs)")
	print(f"worst runs:")
	for change, run in sorted(changes)[:top]:
		if change > -worse:
			break
		print(f"  {change:+7.2f}  {run}  {before[run]['within2']:.2f} -> {after[run]['within2']:.2f} (within 8 {before[run]['within8']:.2f} -> {after[run]['within8']:.2f})")
	print(f"best runs:")
	for change, run in sorted(changes, reverse=True)[:top]:
		if change < worse:
			break
		print(f"  {change:+7.2f}  {run}  {before[run]['within2']:.2f} -> {after[run]['within2']:.2f}")


if __name__ == "__main__":
	if len(sys.argv) >= 3 and sys.argv[1] == "snapshot":
		snapshot(sys.argv[2], sys.argv[3:])
	elif len(sys.argv) >= 4 and sys.argv[1] == "compare":
		args = sys.argv[4:]
		worse = float(args[args.index("--worse") + 1]) if "--worse" in args else 0.5
		top = int(args[args.index("--top") + 1]) if "--top" in args else 30
		compare(sys.argv[2], sys.argv[3], worse, top)
	else:
		print(__doc__)
		sys.exit(1)
