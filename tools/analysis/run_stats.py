"""Pixel agreement of every run of a calibration experiment (studio.png against ror.png), over the
whole frame or a centred crop: percentage of pixels within 2 and within 8 levels (max channel) and the
mean signed error per channel (ror - Studio).

    python tools/analysis/run_stats.py out/calibrate/<experiment> [--crop 0.5] [--filter text]
"""

import os
import sys

import numpy as np
from PIL import Image


def main():
	args = sys.argv[1:]
	crop = 1.0
	text = None
	if "--crop" in args:
		crop = float(args[args.index("--crop") + 1])
		del args[args.index("--crop") : args.index("--crop") + 2]
	if "--filter" in args:
		text = args[args.index("--filter") + 1]
		del args[args.index("--filter") : args.index("--filter") + 2]
	root = args[0]
	totals = []
	for name in sorted(os.listdir(root)):
		run = os.path.join(root, name)
		if not os.path.isfile(os.path.join(run, "studio.png")) or (text and text not in name):
			continue
		studio = np.asarray(Image.open(os.path.join(run, "studio.png")).convert("RGB"), dtype=np.int16)
		ror = np.asarray(Image.open(os.path.join(run, "ror.png")).convert("RGB"), dtype=np.int16)
		h, w, _ = studio.shape
		ch, cw = int(h * crop), int(w * crop)
		y0, x0 = (h - ch) // 2, (w - cw) // 2
		studio = studio[y0 : y0 + ch, x0 : x0 + cw]
		ror = ror[y0 : y0 + ch, x0 : x0 + cw]
		error = np.abs(ror - studio).max(axis=2)
		within2 = (error <= 2).mean() * 100
		within8 = (error <= 8).mean() * 100
		signed = (ror - studio).reshape(-1, 3).mean(axis=0)
		totals.append((within2, within8))
		print(f"{name:40s} within 2 {within2:6.2f}%  within 8 {within8:6.2f}%  signed {signed.round(2)}")
	if totals:
		mean = np.mean(totals, axis=0)
		print(f"{'mean':40s} within 2 {mean[0]:6.2f}%  within 8 {mean[1]:6.2f}%")


main()
