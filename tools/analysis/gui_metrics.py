"""GUI parity per calibration run, before and after: the share of pixels within 2 and 8 of 255
(largest channel difference, as `ror compare` reports) of ror-before.png (ROR_NO_GUIS=1) and
ror.png against studio.png, over the whole image and over the GUIs' footprint (pixels ror's GUIs
change, two pixels around them, and pixels where Studio differs from the image without GUIs by more
than 24: the sky's own differences stay out). `--own` keeps only what ror's GUIs change (and the two
pixels around it): for scenes whose other pixels already differ by more than 24 (`gui-lobby`).

    python tools/analysis/gui_metrics.py <experiment> [run filter] [--own]

Make ror-before.png first: run the experiment with ROR_NO_GUIS=1 and rename each run's ror.png.
"""

import os
import sys

import numpy as np
from PIL import Image

arguments = [argument for argument in sys.argv[1:] if argument != "--own"]
own = "--own" in sys.argv[1:]
experiment = arguments[0]
needle = arguments[1] if len(arguments) > 1 else ""
root = os.path.join("out/calibrate", experiment)


def load(path):
	return np.asarray(Image.open(path).convert("RGB"), dtype=np.int32)


def score(a, b, mask=None):
	largest = np.abs(a - b).max(axis=2)
	if mask is not None:
		largest = largest[mask]
	if largest.size == 0:
		return float("nan"), float("nan")
	return (largest <= 2).mean() * 100, (largest <= 8).mean() * 100


rows = []
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run or not os.path.exists(os.path.join(directory, "ror-before.png")):
		continue
	studio = load(os.path.join(directory, "studio.png"))
	after = load(os.path.join(directory, "ror.png"))
	before = load(os.path.join(directory, "ror-before.png"))
	drawn = np.abs(after - before).max(axis=2) > 2
	# two pixels around what ror drew
	grown = drawn.copy()
	for dy in range(-2, 3):
		for dx in range(-2, 3):
			grown |= np.roll(np.roll(drawn, dy, axis=0), dx, axis=1)
	footprint = grown if own else grown | (np.abs(studio - before).max(axis=2) > 24)
	rows.append((run, score(before, studio), score(after, studio), score(before, studio, footprint), score(after, studio, footprint), footprint.mean() * 100))
for run, whole_before, whole_after, gui_before, gui_after, share in rows:
	print(
		f"{run:24s} image {whole_before[0]:5.1f} / {whole_before[1]:5.1f} -> {whole_after[0]:5.1f} / {whole_after[1]:5.1f}"
		f"   GUIs ({share:4.1f}% of the image) {gui_before[0]:5.1f} / {gui_before[1]:5.1f} -> {gui_after[0]:5.1f} / {gui_after[1]:5.1f}"
	)
if rows:
	mean = lambda index, part: np.nanmean([row[index][part] for row in rows])
	print(f"{'mean':24s} image {mean(1, 0):5.1f} / {mean(1, 1):5.1f} -> {mean(2, 0):5.1f} / {mean(2, 1):5.1f}   GUIs {mean(3, 0):5.1f} / {mean(3, 1):5.1f} -> {mean(4, 0):5.1f} / {mean(4, 1):5.1f}")
