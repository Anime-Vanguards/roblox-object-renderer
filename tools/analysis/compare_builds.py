"""Per experiment, the share of pixels within 2 and 8 of 255 (studio.png against ror.png) for two
checkouts' re-renders of the same captures: a regression check between builds.

    python tools/analysis/compare_builds.py <before checkout> <after checkout> <experiment>...
"""

import os
import sys

import numpy as np
from PIL import Image


def stats(root, experiment):
	rows = {}
	directory = os.path.join(root, "out", "calibrate", experiment)
	for run in sorted(os.listdir(directory)):
		path = os.path.join(directory, run)
		if not os.path.isdir(path) or not os.path.exists(os.path.join(path, "ror.png")):
			continue
		studio = np.asarray(Image.open(os.path.join(path, "studio.png")).convert("RGB"), dtype=np.int32)
		ror = np.asarray(Image.open(os.path.join(path, "ror.png")).convert("RGB"), dtype=np.int32)
		largest = np.abs(ror - studio).max(axis=2)
		rows[run] = ((largest <= 2).mean() * 100, (largest <= 8).mean() * 100)
	return rows


before, after = sys.argv[1], sys.argv[2]
for experiment in sys.argv[3:]:
	a, b = stats(before, experiment), stats(after, experiment)
	runs = sorted(set(a) & set(b))
	if not runs:
		print(f"{experiment}: no runs")
		continue
	mean = lambda rows, k: np.mean([rows[r][k] for r in runs])
	changed = [r for r in runs if abs(a[r][0] - b[r][0]) > 0.05 or abs(a[r][1] - b[r][1]) > 0.05]
	print(f"{experiment:22s} within 2: {mean(a, 0):6.2f} -> {mean(b, 0):6.2f}  within 8: {mean(a, 1):6.2f} -> {mean(b, 1):6.2f}  ({len(runs)} runs, {len(changed)} changed)")
	for r in changed:
		print(f"    {r:28s} {a[r][0]:6.2f} -> {b[r][0]:6.2f}   {a[r][1]:6.2f} -> {b[r][1]:6.2f}")
