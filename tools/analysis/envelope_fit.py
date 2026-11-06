"""particle-envelope: each particle's size (or opacity, or squash) in Studio's and ror's grids of 10 x
10 single particles (4 studs apart, 80 studs from the camera through a 35 degree field of view),
their distributions side by side, and what they say about how envelopes vary a keypoint.

    python tools/analysis/envelope_fit.py
"""

import os

import numpy as np
from PIL import Image

ROOT = "out/calibrate/particle-envelope"
PPS = 1116 / (2 * 80 * np.tan(np.radians(17.5)))
CELL = 4 * PPS


def cells(path):
	image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)
	height, width, _ = image.shape
	cx, cy = width / 2, height / 2
	values = []
	for row in range(10):
		for column in range(10):
			# column grows along +X world, which is screen-left from the front
			x = cx - (column - 4.5) * CELL
			y = cy - (row - 4.5) * CELL
			patch = image[int(y - CELL / 2) : int(y + CELL / 2), int(x - CELL / 2) : int(x + CELL / 2)]
			lit = patch.max(axis=2) > 40
			if not lit.any():
				values.append((0.0, 0.0, 0.0, 0.0))
				continue
			rows = np.where(lit.any(axis=1))[0]
			cols = np.where(lit.any(axis=0))[0]
			values.append((lit.sum(), (cols.max() - cols.min() + 1) / PPS, (rows.max() - rows.min() + 1) / PPS, patch.max(axis=2)[lit].mean()))
	return np.array(values)


def describe(name, values):
	values = np.sort(values)
	quantiles = np.percentile(values, [0, 10, 25, 50, 75, 90, 100])
	return f"{name:6s} mean {values.mean():6.3f} std {values.std():6.3f} q {np.round(quantiles, 3)}"


for run in sorted(os.listdir(ROOT)):
	directory = os.path.join(ROOT, run)
	if not os.path.exists(os.path.join(directory, "ror.png")):
		continue
	studio = cells(os.path.join(directory, "studio.png"))
	ror = cells(os.path.join(directory, "ror.png"))
	print(run)
	if run == "transparency":
		for name, data in (("studio", studio), ("ror", ror)):
			print("  brightness", describe(name, data[:, 3]))
	elif run == "squash":
		for name, data in (("studio", studio), ("ror", ror)):
			lit = data[:, 2] > 0
			print("  height/width", describe(name, data[lit, 2] / np.maximum(data[lit, 1], 1e-3)))
	else:
		for name, data in (("studio", studio), ("ror", ror)):
			# Size is the half side of the square
			print("  size", describe(name, data[:, 1] / 2), "invisible", int((data[:, 0] == 0).sum()))
