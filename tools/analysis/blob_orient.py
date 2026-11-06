"""Each particle of a run (bright blobs), Studio's and ror's side by side by position: its centre,
area and the angle of its principal axis (degrees, image moments), to compare orientations.

    python tools/analysis/blob_orient.py <experiment> <run> [threshold]
"""

import sys

import numpy as np
from PIL import Image
from scipy import ndimage


def blobs(path, threshold):
	image = np.asarray(Image.open(path).convert("L")).astype(np.float64)
	labels, count = ndimage.label(ndimage.binary_closing(image > threshold, iterations=2))
	found = []
	for index in range(1, count + 1):
		ys, xs = np.nonzero(labels == index)
		if len(xs) < 20:
			continue
		weights = image[ys, xs]
		cx, cy = np.average(xs, weights=weights), np.average(ys, weights=weights)
		dx, dy = xs - cx, ys - cy
		cxx, cyy, cxy = np.average(dx * dx, weights=weights), np.average(dy * dy, weights=weights), np.average(dx * dy, weights=weights)
		angle = 0.5 * np.degrees(np.arctan2(2 * cxy, cxx - cyy))
		found.append((cx, cy, len(xs), angle))
	return sorted(found)


experiment, run = sys.argv[1], sys.argv[2]
threshold = float(sys.argv[3]) if len(sys.argv) > 3 else 60
root = f"out/calibrate/{experiment}/{run}"
studio, ror = blobs(f"{root}/studio.png", threshold), blobs(f"{root}/ror.png", threshold)
print(f"{len(studio)} Studio blobs, {len(ror)} ror blobs")
for s in studio:
	nearest = min(ror, key=lambda r: (r[0] - s[0]) ** 2 + (r[1] - s[1]) ** 2) if ror else None
	line = f"studio ({s[0]:7.1f}, {s[1]:7.1f}) area {s[2]:5d} angle {s[3]:6.1f}"
	if nearest:
		line += f" | ror ({nearest[0]:7.1f}, {nearest[1]:7.1f}) area {nearest[2]:5d} angle {nearest[3]:6.1f}"
	print(line)
