"""Per-run pixel metrics of a calibration experiment's studio.png / ror.png pairs: the share of
pixels within 2 and 8 of 255 (largest channel difference), mean absolute error, mean signed
difference (ror - Studio), optionally over a centre crop.

    python tools/analysis/run_metrics.py <experiment> [crop fraction, e.g. 0.5] [run filter]
"""

import os
import sys

import numpy as np
from PIL import Image

experiment = sys.argv[1]
crop = float(sys.argv[2]) if len(sys.argv) > 2 else 1.0
needle = sys.argv[3] if len(sys.argv) > 3 else ""
root = os.path.join("out/calibrate", experiment)
rows = []
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run:
		continue
	studio = np.asarray(Image.open(os.path.join(directory, "studio.png")).convert("RGB"), dtype=np.int32)
	ror = np.asarray(Image.open(os.path.join(directory, "ror.png")).convert("RGB"), dtype=np.int32)
	h, w, _ = studio.shape
	ch, cw = int(h * crop), int(w * crop)
	y0, x0 = (h - ch) // 2, (w - cw) // 2
	s = studio[y0 : y0 + ch, x0 : x0 + cw]
	r = ror[y0 : y0 + ch, x0 : x0 + cw]
	diff = r - s
	largest = np.abs(diff).max(axis=2)
	rows.append((run, (largest <= 2).mean() * 100, (largest <= 8).mean() * 100, np.abs(diff).mean(), diff.reshape(-1, 3).mean(axis=0)))
for run, exact, within8, mae, signed in rows:
	print(f"{run:22s} exact {exact:5.1f}%  within8 {within8:5.1f}%  mae {mae:5.2f}  signed {np.round(signed, 1)}")
print(f"mean: exact {np.mean([r[1] for r in rows]):.1f}%  within8 {np.mean([r[2] for r in rows]):.1f}%  mae {np.mean([r[3] for r in rows]):.2f}")
