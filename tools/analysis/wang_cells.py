"""Organic materials: is a material-swatches capture made of 2-stud cells, each one of the 16 tiles
of the colour map's 4 x 4 atlas? Finds the cell grid's phase, then which atlas tile each cell
shows, and searches Studio's wangIndex.dds for that arrangement.

    python tools/analysis/wang_cells.py <material> <pack id> [cell px at 32 px/stud, default 64]
"""

import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = "out/calibrate/material-swatches"
PX_PER_STUD = 558.0 / (59.0 * np.tan(np.radians(10.0)))

material, pack = sys.argv[1], sys.argv[2]
cell = int(sys.argv[3]) if len(sys.argv) > 3 else 64

display = np.asarray(Image.open(os.path.join(ROOT, f"{material}-flat", "studio.png")).convert("RGB"), dtype=np.float32)
gray = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
small = cv2.resize(gray, None, fx=32.0 / PX_PER_STUD, fy=32.0 / PX_PER_STUD, interpolation=cv2.INTER_AREA)
texel = np.asarray(Image.open(f".cache/material-packs/{pack}_color.png").convert("RGB"), dtype=np.float32) / 255.0
texture = cv2.resize((texel ** 2).mean(axis=2), (256, 256), interpolation=cv2.INTER_AREA).astype(np.float32)
# wrapped, so cells that straddle the atlas edge still match
wrapped = np.tile(texture, (2, 2))[: 256 + cell, : 256 + cell].copy()

h, w = small.shape
cells_x, cells_y = (w - cell) // cell - 1, (h - cell) // cell - 1
best = None
for oy in range(0, cell, 2):
	for ox in range(0, cell, 2):
		scores = []
		for j in range(cells_y):
			for i in range(cells_x):
				patch = small[oy + j * cell : oy + (j + 1) * cell, ox + i * cell : ox + (i + 1) * cell]
				scores.append(cv2.matchTemplate(wrapped, patch, cv2.TM_CCOEFF_NORMED).max())
		mean = float(np.mean(scores))
		if best is None or mean > best[0]:
			best = (mean, ox, oy)
mean, ox, oy = best
print(f"cell grid phase ({ox}, {oy}) px, mean NCC {mean:.3f}, {cells_x} x {cells_y} cells")
tiles = np.zeros((cells_y, cells_x, 2), dtype=int)
for j in range(cells_y):
	row = []
	for i in range(cells_x):
		patch = small[oy + j * cell : oy + (j + 1) * cell, ox + i * cell : ox + (i + 1) * cell]
		result = cv2.matchTemplate(wrapped, patch, cv2.TM_CCOEFF_NORMED)
		_, value, _, (x, y) = cv2.minMaxLoc(result)
		tiles[j, i] = (x, y)
		row.append(f"{x:3d},{y:3d}({value:.2f})")
	print(" ".join(row))
np.save(f".cache/wang-{material}.npy", tiles)
