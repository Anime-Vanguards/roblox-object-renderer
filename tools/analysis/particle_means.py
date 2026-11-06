"""Live particles: Studio's mean over a burst of frames (`frame<k>.png`) against ror's mean over seeded
snapshots (`ror-mean.png`) for each run of a particle experiment. Writes `means.png` (Studio mean |
ror mean | 4x difference) per run and prints, per run, the pixel metrics of the two means and the
summed brightness of each region (columns of `--regions` equal slices, default the whole image).

    python tools/analysis/particle_means.py <experiment> [regions] [run filter]
"""

import glob
import os
import sys

import numpy as np
from PIL import Image

experiment = sys.argv[1]
regions = int(sys.argv[2]) if len(sys.argv) > 2 else 1
needle = sys.argv[3] if len(sys.argv) > 3 else ""
root = os.path.join("out/calibrate", experiment)
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	frames = sorted(glob.glob(os.path.join(directory, "frame*.png")))
	if not frames or needle not in run or not os.path.exists(os.path.join(directory, "ror-mean.png")):
		continue
	studio = np.mean([np.asarray(Image.open(frame).convert("RGB")).astype(np.float64) for frame in frames], axis=0)
	ror = np.asarray(Image.open(os.path.join(directory, "ror-mean.png")).convert("RGB")).astype(np.float64)
	difference = ror - studio
	largest = np.abs(difference).max(axis=2)
	strip = np.concatenate([studio, ror, np.minimum(np.abs(difference) * 4, 255)], axis=1).astype(np.uint8)
	Image.fromarray(strip).save(os.path.join(directory, "means.png"))
	print(f"{run:12s} frames {len(frames)}  within2 {(largest <= 2).mean() * 100:5.1f}%  within8 {(largest <= 8).mean() * 100:5.1f}%  mae {np.abs(difference).mean():.3f}")
	if regions > 1:
		width = studio.shape[1]
		sums = []
		for index in range(regions):
			x0, x1 = index * width // regions, (index + 1) * width // regions
			# above each image's background (the slice's median, which Studio's noise leaves ~1 of 255
			# brighter) by more than that noise
			s = (studio[:, x0:x1] - np.median(studio[:, x0:x1]) - 3).clip(0).sum() / 1e5
			r = (ror[:, x0:x1] - np.median(ror[:, x0:x1]) - 3).clip(0).sum() / 1e5
			sums.append(f"{index}: {s:7.1f}/{r:7.1f} ({r / max(s, 1e-9):.2f})")
		print("   region brightness studio/ror (ratio): " + "  ".join(sums))
