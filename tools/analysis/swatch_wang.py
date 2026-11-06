"""Wang tiling of a pre-2022 swatch (packs with <tiling>2</tiling>): the texture (S studs per tile) is a
4 x 4 atlas of quarter-tile cells and every quarter-tile cell of the surface shows one of them.

1. The grid: windows of the flat capture matched anywhere in the tiled texture land at whole-cell
   offsets; the common fraction is the grid origin (face u / v, mod one cell).
2. Every whole visible cell is matched against the 16 atlas cells.
3. That pattern is searched for in Studio's wang index under every convention (which channel is the
   column, flips, transpose, position in the 128 x 128 texture).
4. The best convention renders the whole frame (grid, index lookup, atlas cell), compared with the
   capture by NCC: it peaks sharply at the true S.

    python tools/analysis/swatch_wang.py <material>-flat <pack id> <studs per tile>

Env: ROR_STATE_DIR, SWATCH_ROOT; WANG_INDEX, a PNG of PlatformContent/pc/textures/wangIndex.dds
(default out/wangIndex.png; `cargo run -p ror --release --example tools -- decode_image out <that file>`).
"""

import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = os.environ.get("SWATCH_ROOT", "out/calibrate/material-swatches")
PACKS = os.path.join(os.environ.get("ROR_STATE_DIR") or ".cache", "material-packs")
WANG_INDEX = os.environ.get("WANG_INDEX", "out/wangIndex.png")
PX = 558.0 / (59.0 * np.tan(np.radians(10.0)))
P = 24.0
HALF = 18.0  # the swatch's top face: u / v of the image centre

run, pack, studs = sys.argv[1], sys.argv[2], float(sys.argv[3])
display = np.asarray(Image.open(os.path.join(ROOT, run, "studio.png")).convert("RGB"), dtype=np.float64)
capture = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
small = cv2.resize(capture, None, fx=P / PX, fy=P / PX, interpolation=cv2.INTER_AREA)
sh, sw = small.shape
full = os.path.join(PACKS, f"{pack}_color_full.png")
texel = np.asarray(Image.open(full if os.path.exists(full) else os.path.join(PACKS, f"{pack}_color.png")).convert("RGB"), dtype=np.float32) / 255.0
albedo = (texel ** 2).mean(axis=2)
side = int(round(studs * P))
tile = cv2.resize(albedo, (side, side), interpolation=cv2.INTER_AREA if side < albedo.shape[0] else cv2.INTER_CUBIC)
cell = studs / 4.0

# 1. the grid origin
k = int(1.2 * P)
big = np.ascontiguousarray(np.tile(tile, (2, 2))[: side + k, : side + k])
fractions = []
for y in np.arange(k, sh - 2 * k, 0.7 * P):
	for x in np.arange(k, sw - 2 * k, 0.7 * P):
		y0, x0 = int(y), int(x)
		if abs(y0 + k / 2 - sh / 2) < k and abs(x0 + k / 2 - sw / 2) < k:
			continue
		_, best, _, (bx, by) = cv2.minMaxLoc(cv2.matchTemplate(big, small[y0 : y0 + k, x0 : x0 + k], cv2.TM_CCOEFF_NORMED))
		if best >= 0.8:
			u = HALF + (x0 - sw / 2) / P
			v = HALF + (y0 - sh / 2) / P
			fractions.append(((u - bx / P) % cell, (v - by / P) % cell))
fractions = np.array(fractions)


def circular_mean(values):
	angle = values / cell * 2 * np.pi
	return (np.arctan2(np.sin(angle).mean(), np.cos(angle).mean()) / (2 * np.pi) * cell) % cell


gu, gv = circular_mean(fractions[:, 0]), circular_mean(fractions[:, 1])
print(f"{run}: {len(fractions)} windows, grid origin at face u {gu:.3f} v {gv:.3f} (mod {cell:.4f})")

# 2. the atlas cell of every whole visible cell
csz = int(round(cell * P))
margin = int(0.12 * cell * P)
atlas = [[tile[r * csz : (r + 1) * csz, c * csz : (c + 1) * csz] for c in range(4)] for r in range(4)]
pattern = {}
scores = []
for b in range(int(np.ceil((HALF - sh / 2 / P - gv) / cell)), int(np.floor((HALF + sh / 2 / P - gv) / cell))):
	for a in range(int(np.ceil((HALF - sw / 2 / P - gu) / cell)), int(np.floor((HALF + sw / 2 / P - gu) / cell))):
		x = int(round((gu + a * cell - HALF) * P + sw / 2))
		y = int(round((gv + b * cell - HALF) * P + sh / 2))
		inner = small[y + margin : y + csz - margin, x + margin : x + csz - margin]
		if inner.shape != (csz - 2 * margin, csz - 2 * margin):
			continue
		best = max((float(np.corrcoef(inner.ravel(), atlas[r][c][margin : csz - margin, margin : csz - margin].ravel())[0, 1]), (c, r)) for r in range(4) for c in range(4))
		pattern[(a, b)] = best[1]
		scores.append(best[0])
print(f"cells {len(pattern)}, mean match {np.mean(scores):.3f}, min {np.min(scores):.3f}")

# 3. the pattern in the wang index (two 0..3 indices per texel, stored as multiples of 64)
wang = np.asarray(Image.open(WANG_INDEX).convert("RGBA")).astype(int) // 64
channels = {"red": wang[..., 0], "alpha": wang[..., 3]}
n = wang.shape[0]
results = []
for column, row_channel in (("red", "alpha"), ("alpha", "red")):
	for transpose in (False, True):
		for su in (1, -1):
			for sv in (1, -1):
				for flip_c in (False, True):
					for flip_r in (False, True):
						cols = 3 - channels[column] if flip_c else channels[column]
						rows = 3 - channels[row_channel] if flip_r else channels[row_channel]
						hits = np.zeros((n, n), np.int32)
						for (a, b), (c, r) in pattern.items():
							tx, ty = (sv * b, su * a) if transpose else (su * a, sv * b)
							hits += np.roll(cols == c, shift=(-ty, -tx), axis=(0, 1))
							hits += np.roll(rows == r, shift=(-ty, -tx), axis=(0, 1))
						y0, x0 = np.unravel_index(np.argmax(hits), hits.shape)
						results.append((int(hits.max()), column, transpose, su, sv, flip_c, flip_r, (int(x0), int(y0))))
results.sort(key=lambda item: -item[0])
for hits, column, transpose, su, sv, flip_c, flip_r, at in results[:3]:
	print(f"  {hits}/{2 * len(pattern)} indices: column={column} transpose={transpose} u{'+' if su > 0 else '-'} v{'+' if sv > 0 else '-'} flip column={flip_c} row={flip_r}, at texel {at}")

# 4. the whole frame under the best convention (column in red, row in alpha, u+ v+ expected)
hits, column, transpose, su, sv, flip_c, flip_r, (x0, y0) = results[0]
ys, xs = np.mgrid[0:sh, 0:sw]
u = HALF + (xs + 0.5 - sw / 2) / P
v = HALF + (ys + 0.5 - sh / 2) / P
a = np.floor((u - gu) / cell).astype(int)
b = np.floor((v - gv) / cell).astype(int)
col = channels[column][(y0 + b) % n, (x0 + a) % n]
row = channels["alpha" if column == "red" else "red"][(y0 + b) % n, (x0 + a) % n]
tx = ((col + (u - gu) / cell - a) / 4 * albedo.shape[1]).astype(np.float32)
ty = ((row + (v - gv) / cell - b) / 4 * albedo.shape[0]).astype(np.float32)
blurred = cv2.resize(cv2.resize(albedo, (side, side), interpolation=cv2.INTER_AREA), albedo.shape[::-1], interpolation=cv2.INTER_LINEAR)
predicted = cv2.remap(blurred, tx, ty, cv2.INTER_LINEAR, borderMode=cv2.BORDER_WRAP)
mask = np.ones_like(small, bool)
mask[int(sh / 2 - P) : int(sh / 2 + P), int(sw / 2 - P) : int(sw / 2 + P)] = False
print(f"whole-frame NCC of the Wang model: {np.corrcoef(small[mask], predicted[mask])[0, 1]:.4f}")
