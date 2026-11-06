"""Pixel agreement of every run of a calibration experiment over the subject alone: studio.png against
ror.png (or another of ror's images) where a mask image's alpha is above zero (`rerender <experiment>
mask.png 1644 1116 transparent` writes one per run: ror's coverage over a transparent background).
Prints the share of masked pixels within 2 and within 8 levels (max channel) and the mean signed error
per channel (ror - Studio).

    python tools/analysis/masked_stats.py out/calibrate/<experiment> [--ror ror.png] [--mask mask.png] [--filter text]
"""

import os
import sys

import numpy as np
from PIL import Image


def option(args, name, default):
	if name in args:
		index = args.index(name)
		value = args[index + 1]
		del args[index : index + 2]
		return value
	return default


def main():
	args = sys.argv[1:]
	ror_name = option(args, "--ror", "ror.png")
	mask_name = option(args, "--mask", "mask.png")
	text = option(args, "--filter", None)
	root = args[0]
	totals = []
	for name in sorted(os.listdir(root)):
		run = os.path.join(root, name)
		paths = [os.path.join(run, file) for file in ("studio.png", ror_name, mask_name)]
		if not all(os.path.isfile(path) for path in paths) or (text and text not in name):
			continue
		studio = np.asarray(Image.open(paths[0]).convert("RGB"), dtype=np.int16)
		ror = np.asarray(Image.open(paths[1]).convert("RGB"), dtype=np.int16)
		mask = np.asarray(Image.open(paths[2]).convert("RGBA"))[..., 3] > 0
		if not mask.any():
			continue
		error = np.abs(ror - studio).max(axis=2)[mask]
		within2 = (error <= 2).mean() * 100
		within8 = (error <= 8).mean() * 100
		signed = (ror - studio)[mask].mean(axis=0)
		totals.append((within2, within8))
		print(f"{name:40s} within 2 {within2:6.2f}%  within 8 {within8:6.2f}%  signed {signed.round(2)}  ({mask.sum()} px)")
	if totals:
		mean = np.mean(totals, axis=0)
		print(f"{'mean':40s} within 2 {mean[0]:6.2f}%  within 8 {mean[1]:6.2f}%")


main()
