"""Agreement of the local light experiments (notes/render.md "Local lights"): per run and per experiment,
the share of pixels within 2 and within 8 of 255 (largest channel difference) over the pixels Studio or
ror shows brighter than 3 of 255 (the light's own pixels; the dark rest agrees anyway), and over the
whole frame.

    python tools/analysis/light_metrics.py <experiment> [...] [--runs] [--filter text] [--root out/calibrate]
"""

import os
import sys

import numpy as np
from PIL import Image


def pair(directory):
	studio = np.asarray(Image.open(os.path.join(directory, "studio.png")).convert("RGB"), dtype=np.int32)
	ror = np.asarray(Image.open(os.path.join(directory, "ror.png")).convert("RGB"), dtype=np.int32)
	return studio, ror


def metrics(studio, ror):
	largest = np.abs(ror - studio).max(axis=2)
	lit = (studio.max(axis=2) > 3) | (ror.max(axis=2) > 3)
	share = lit.mean() * 100
	# a run both draw dark (its light out of reach) agrees everywhere
	if not lit.any():
		lit = np.ones_like(lit)
	n = lit.sum()
	return {
		"lit2": (largest[lit] <= 2).sum() / n * 100,
		"lit8": (largest[lit] <= 8).sum() / n * 100,
		"all2": (largest <= 2).mean() * 100,
		"all8": (largest <= 8).mean() * 100,
		"signed": (ror - studio)[lit].mean(axis=0),
		"lit": share,
	}


def main():
	args = sys.argv[1:]
	root = "out/calibrate"
	show_runs = "--runs" in args
	args = [a for a in args if a != "--runs"]
	needle = ""
	if "--filter" in args:
		i = args.index("--filter")
		needle = args[i + 1]
		del args[i : i + 2]
	if "--root" in args:
		i = args.index("--root")
		root = args[i + 1]
		del args[i : i + 2]
	for experiment in args:
		base = os.path.join(root, experiment)
		rows = []
		for run in sorted(os.listdir(base)):
			directory = os.path.join(base, run)
			if not os.path.isfile(os.path.join(directory, "studio.png")) or needle not in run:
				continue
			m = metrics(*pair(directory))
			rows.append((run, m))
			if show_runs:
				print(f"  {experiment}/{run:28s} lit {m['lit2']:6.2f} / {m['lit8']:6.2f}   all {m['all2']:6.2f} / {m['all8']:6.2f}   lit share {m['lit']:5.1f}%  ror-studio {np.round(m['signed'], 2)}")
		if not rows:
			continue
		mean = lambda key: np.mean([m[key] for _, m in rows])
		print(f"{experiment:28s} lit within 2 / 8: {mean('lit2'):6.2f} / {mean('lit8'):6.2f}   whole frame {mean('all2'):6.2f} / {mean('all8'):6.2f}   ({len(rows)} runs)")


main()
