"""Bloom halo profiles from the bloom-probe calibration (out/calibrate/bloom-probe).

For each probe (a Neon tile centre), reads the Studio capture's row and column through it and
converts display values back to scene radiance x through the fitted tone curve (fit_tonemap.py),
so the halo is in the units bloom is added in.

    python tools/analysis/bloom_profiles.py [--run ev0] [--plot]
"""

import argparse
import json

import numpy as np
from PIL import Image

from tone import radiance


def profiles(image, cx, cy, reach):
	cx, cy = int(round(cx)), int(round(cy))
	height, width, _ = image.shape
	left, right = max(0, cx - reach), min(width, cx + reach + 1)
	top, bottom = max(0, cy - reach), min(height, cy + reach + 1)
	row = image[cy, left:right, :3].astype(np.float64)
	column = image[top:bottom, cx, :3].astype(np.float64)
	return np.arange(left, right) - cx, row, np.arange(top, bottom) - cy, column


def main():
	parser = argparse.ArgumentParser()
	parser.add_argument("--dir", default="out/calibrate/bloom-probe")
	parser.add_argument("--run", default="ev0")
	parser.add_argument("--reach", type=int, default=140)
	args = parser.parse_args()
	runs = {run["label"]: run for run in json.load(open(f"{args.dir}/samples.json"))}
	run = runs[args.run]
	image = np.asarray(Image.open(f"{args.dir}/{args.run}/studio.png").convert("RGB"))
	exposure = 2.0 ** run["environment"]["exposureCompensation"]
	for sample in run["samples"]:
		cx, cy = sample["pixel"]
		offsets, row, _, _ = profiles(image, cx, cy, args.reach)
		x = radiance(row[:, 1])
		inputs = sample["inputs"]
		right = offsets >= 0
		values = " ".join(f"{value:.3f}" for value in x[right][::6])
		print(f"{sample['probe']:8} size={inputs['size']} emission={inputs['emission'] * exposure:6.2f} centre={row[offsets == 0][0]}  x(right, every 6 px): {values}")


if __name__ == "__main__":
	main()
