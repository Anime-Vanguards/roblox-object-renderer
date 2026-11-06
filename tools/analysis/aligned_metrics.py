"""Pixel metrics after undoing Studio's random texture placement (pre-2022 materials start at a new
random offset per part on every load): a 500 x 400 px region of Studio's capture, left of the view
centre, is found in ror's render by normalised cross-correlation and the two regions are compared.
Meaningful for regular tilings up to ~10 studs per tile (the search covers one tile) and for
deterministic materials (shift 0); not for Wang, Organic or far-tiled materials (texture_stats.py).

    python tools/analysis/aligned_metrics.py <experiment> [run filter] [material,material,...]
"""

import os
import sys

import cv2
import numpy as np
from PIL import Image

root = os.path.join("out/calibrate", sys.argv[1])
needle = sys.argv[2] if len(sys.argv) > 2 else ""
names = sys.argv[3].split(",") if len(sys.argv) > 3 else None
rows = []
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run or (names and run.rsplit("-", 1)[0] not in names):
		continue
	studio = np.asarray(Image.open(os.path.join(directory, "studio.png")).convert("RGB"), dtype=np.float32)
	ror = np.asarray(Image.open(os.path.join(directory, "ror.png")).convert("RGB"), dtype=np.float32)
	h, w, _ = studio.shape
	y0, x0 = h // 2 - 200, w // 2 - 530
	region = studio[y0 : y0 + 400, x0 : x0 + 500]
	if region.mean(axis=2).std() < 0.5:
		by, bx = y0, x0
	else:
		_, _, _, (bx, by) = cv2.minMaxLoc(cv2.matchTemplate(ror.mean(axis=2), region.mean(axis=2), cv2.TM_CCOEFF_NORMED))
	diff = ror[by : by + 400, bx : bx + 500] - region
	largest = np.abs(diff).max(axis=2)
	rows.append((run, (largest <= 2).mean() * 100, (largest <= 8).mean() * 100, np.abs(diff).mean(), diff.reshape(-1, 3).mean(axis=0), (bx - x0, by - y0)))
for run, exact, within8, mae, signed, shift in rows:
	print(f"{run:22s} exact {exact:5.1f}%  within8 {within8:5.1f}%  mae {mae:5.2f}  signed {np.round(signed, 1)}  shift {shift}")
print(f"mean: exact {np.mean([r[1] for r in rows]):.1f}%  within8 {np.mean([r[2] for r in rows]):.1f}%  mae {np.mean([r[3] for r in rows]):.2f}")
