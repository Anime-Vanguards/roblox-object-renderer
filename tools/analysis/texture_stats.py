"""Texture statistics of ror against Studio where the placement cannot be aligned (Studio's random
offsets, Wang cells, Organic regions, far tilings): mean display value and contrast (std) over the
centre crop with the view centre masked, and the ratio ror / Studio of the amplitude spectrum in four
bands (about 9, 3, 1 and 0.3 studs per cycle at the swatch camera).

    python tools/analysis/texture_stats.py <experiment> [run filter]
"""

import os
import sys

import numpy as np
from PIL import Image

root = os.path.join("out/calibrate", sys.argv[1])
needle = sys.argv[2] if len(sys.argv) > 2 else ""
BANDS = [(0.002, 0.01), (0.01, 0.03), (0.03, 0.1), (0.1, 0.3)]  # cycles per pixel


def load(path):
	image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64).mean(axis=2)
	h, w = image.shape
	return image[h // 2 - 400 : h // 2 + 400, w // 2 - 600 : w // 2 + 600]


def band_power(image):
	f = np.abs(np.fft.fftshift(np.fft.fft2(image - image.mean()))) ** 2
	h, w = f.shape
	yy, xx = np.mgrid[0:h, 0:w]
	r = np.hypot((yy - h / 2) / h, (xx - w / 2) / w)
	return [f[(r >= lo) & (r < hi)].mean() for lo, hi in BANDS]


print(f"{'run':22s} {'mean S':>7s} {'mean R':>7s} {'std S':>6s} {'std R':>6s} {'R/S':>5s}   amplitude R/S by band (~9, 3, 1, 0.3 studs)")
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run:
		continue
	studio = load(os.path.join(directory, "studio.png"))
	ror = load(os.path.join(directory, "ror.png"))
	mask = np.ones_like(studio, bool)
	mask[380:420, 580:620] = False
	ratios = [np.sqrt(b / a) if a > 0 else float("nan") for a, b in zip(band_power(studio), band_power(ror))]
	s, r = studio[mask].std(), ror[mask].std()
	print(f"{run:22s} {studio[mask].mean():7.1f} {ror[mask].mean():7.1f} {s:6.1f} {r:6.1f} {r / max(s, 1e-6):5.2f}   " + "  ".join(f"{v:5.2f}" for v in ratios))
