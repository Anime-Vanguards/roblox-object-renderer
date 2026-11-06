"""Pre-2022 far tiling: the weight of a colour map's far tiling in straight-down captures at known
depths (experiments material-far, `<material>-d<depth>-f<fov>`, and material-depths,
`<material>-d<depth>`, 30 degrees). The capture (flat light, radiance) is regressed on the map at the
near scale and at the far scale, each placed at the phase that correlates best:
capture ~ a + b_near * near + b_far * far, weight = b_far / (b_near + b_far). Close views
underestimate it (the far tiles barely vary across the frame).

    python tools/analysis/far_weight.py <material> <pack id> <near studs> <far studs>

Env: ROR_STATE_DIR, ROOT (default out/calibrate/material-far).
"""

import os
import re
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = os.environ.get("ROOT", "out/calibrate/material-far")
PACKS = os.path.join(os.environ.get("ROR_STATE_DIR") or ".cache", "material-packs")


def albedo_map(pack):
	full = os.path.join(PACKS, f"{pack}_color_full.png")
	texel = np.asarray(Image.open(full if os.path.exists(full) else os.path.join(PACKS, f"{pack}_color.png")).convert("RGB"), dtype=np.float32) / 255.0
	return np.where(texel <= 0.04045, texel / 12.92, ((texel + 0.055) / 1.055) ** 2.4).mean(axis=2).astype(np.float32)


def place(texture, studs, q, template):
	"""The map tiled at q px per stud, cut where it correlates best with the template."""
	side = int(round(studs * q))
	tile = cv2.resize(texture, (side, side), interpolation=cv2.INTER_AREA if side < texture.shape[0] else cv2.INTER_CUBIC)
	th, tw = template.shape
	big = np.ascontiguousarray(np.tile(tile, (int(np.ceil((th + side) / side)) + 1, int(np.ceil((tw + side) / side)) + 1))[: th + side, : tw + side])
	_, best, _, (bx, by) = cv2.minMaxLoc(cv2.matchTemplate(big, template, cv2.TM_CCOEFF_NORMED))
	return big[by : by + th, bx : bx + tw], best


def main():
	material, pack, near, far = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
	texture = albedo_map(pack)
	runs = []
	for name in os.listdir(ROOT):
		m = re.fullmatch(rf"{material}-d([\d.]+)(?:-f([\d.]+))?", name)
		if m:
			runs.append((float(m.group(2) or 30.0), float(m.group(1)), name))
	for fov, depth, name in sorted(runs):
		display = np.asarray(Image.open(os.path.join(ROOT, name, "studio.png")).convert("RGB"), dtype=np.float32)
		gray = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
		px = 558.0 / (depth * np.tan(np.radians(fov / 2)))
		q = min(px, 2048.0 / far, 1024.0 / near * 1.5)
		small = cv2.resize(gray, None, fx=q / px, fy=q / px, interpolation=cv2.INTER_AREA)
		h, w = small.shape
		# the left half of the frame, beside the centre
		template = np.ascontiguousarray(small[int(h * 0.1) : int(h * 0.9), int(w * 0.05) : int(w * 0.47)])
		near_image, near_score = place(texture, near, q, template)
		far_image, far_score = place(texture, far, q, template)
		X = np.stack([np.ones(template.size), near_image.ravel(), far_image.ravel()], axis=1)
		coef, *_ = np.linalg.lstsq(X, template.ravel(), rcond=None)
		fit = X @ coef
		r2 = 1 - ((template.ravel() - fit) ** 2).sum() / ((template.ravel() - template.mean()) ** 2).sum()
		weight = coef[2] / (coef[1] + coef[2])
		print(f"{material:13s} fov {fov:3.0f} depth {depth:5.0f}  NCC near {near_score:.3f} far {far_score:.3f}  far weight {weight:5.2f} (depth / weight {depth / max(weight, 1e-3):6.1f})  R2 {r2:.3f}", flush=True)


main()
