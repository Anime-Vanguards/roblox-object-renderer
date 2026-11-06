"""Texture scale of a material-swatches capture by local matching: windows of the capture (radiance)
are matched anywhere in the pack's map tiled at S studs per tile, and the mean best NCC is the score
of S. A coarse log-spaced scan, then a fine one around the best. Works where Studio places the
texture at a random offset or in Wang cells (no global phase needed).

    python tools/analysis/swatch_scale.py <material> <pack id> [lo,hi studs]

Env: ROR_STATE_DIR (packs in <state dir>/material-packs), SWATCH_ROOT (default
out/calibrate/material-swatches), LIGHT (flat / sun), KIND (color / normal / roughness), ORIENT (r0,
r90, ..., fr270), WINDOW (studs, default 1.5).
"""

import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = os.environ.get("SWATCH_ROOT", "out/calibrate/material-swatches")
PACKS = os.path.join(os.environ.get("ROR_STATE_DIR") or ".cache", "material-packs")
PX = 558.0 / (59.0 * np.tan(np.radians(10.0)))
P = 20.0
LIGHT = os.environ.get("LIGHT", "flat")
KIND = os.environ.get("KIND", "color")
ORIENT = os.environ.get("ORIENT", "r0")


def orient(image, name):
	if name.startswith("f"):
		image = image[:, ::-1]
	return np.ascontiguousarray(np.rot90(image, int(name.lstrip("fr")) // 90))


def load_map(pack):
	full = os.path.join(PACKS, f"{pack}_{KIND}_full.png")
	path = full if os.path.exists(full) else os.path.join(PACKS, f"{pack}_{KIND}.png")
	texel = np.asarray(Image.open(path).convert("RGB"), dtype=np.float32) / 255.0
	return (texel ** 2).mean(axis=2)


def score(small, texture, studs, window, step):
	sh, sw = small.shape
	side = int(round(studs * P))
	tile = cv2.resize(texture, (side, side), interpolation=cv2.INTER_AREA if side < texture.shape[0] else cv2.INTER_CUBIC)
	k = int(round(window * P))
	reps = int(np.ceil((side + k) / side)) + 1
	big = np.ascontiguousarray(np.tile(tile, (reps, reps))[: side + k, : side + k])
	values = []
	for y in np.arange(k, sh - 2 * k, step * P):
		for x in np.arange(k, sw - 2 * k, step * P):
			y0, x0 = int(y), int(x)
			# skip the view centre (an overlay dot in some capture Studios)
			if abs(y0 + k / 2 - sh / 2) < k and abs(x0 + k / 2 - sw / 2) < k:
				continue
			patch = small[y0 : y0 + k, x0 : x0 + k]
			if patch.std() < 1e-7:
				continue
			values.append(cv2.minMaxLoc(cv2.matchTemplate(big, patch, cv2.TM_CCOEFF_NORMED))[1])
	return float(np.mean(values)) if values else 0.0


def main():
	material, pack = sys.argv[1], sys.argv[2]
	lo, hi = (float(v) for v in (sys.argv[3] if len(sys.argv) > 3 else "3,24").split(","))
	window = float(os.environ.get("WINDOW", "1.5"))
	display = np.asarray(Image.open(os.path.join(ROOT, f"{material}-{LIGHT}", "studio.png")).convert("RGB"), dtype=np.float64)
	capture = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
	small = cv2.resize(capture, None, fx=P / PX, fy=P / PX, interpolation=cv2.INTER_AREA)
	texture = orient(load_map(pack), ORIENT)
	coarse = [(score(small, texture, s, window, 2.0), s) for s in np.exp(np.linspace(np.log(lo), np.log(hi), 28))]
	best = max(coarse)
	fine = [(score(small, texture, s, window, 1.0), s) for s in np.linspace(best[1] * 0.95, best[1] * 1.05, 21)]
	top = max(fine)
	ranked = sorted(coarse, reverse=True)[:5]
	print(f"{material:14s} {pack} best S {top[1]:7.3f} score {top[0]:.3f}   coarse: " + "  ".join(f"{s:6.2f}:{v:.3f}" for v, s in ranked), flush=True)


main()
