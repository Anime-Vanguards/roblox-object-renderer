"""Normal-map check on the material-swatches low-sun captures: the capture's radiance against the
Lambert shading each candidate normal map predicts (times its colour map, if any) on the slab's top
face, where u runs along -X and v along -Z (face_mapping.py) and the sun comes from +X at 27°.

    python tools/analysis/swatch_normals.py <material>=<pack id>[:<studs per tile>][,...] ...

(8 studs per tile unless given; the pre-2022 packs tile at 4-40.)
"""

import math
import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

PACKS = os.path.join(os.environ.get("ROR_STATE_DIR") or ".cache", "material-packs")
PX_PER_STUD = 558.0 / (59.0 * np.tan(np.radians(10.0)))

H = (8.0 - 12.0) / 24.0 * 2 * math.pi
T = math.radians(-23.5)
SUN = np.array([-math.sin(H) * math.cos(T), math.cos(H) * math.cos(T), math.sin(T)])
# tangent space of the top face: x along u (-X), y up the image (+Z), z the normal (+Y)
LIGHT = np.array([-SUN[0], SUN[2], SUN[1]])


def load(path, size):
	image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float32) / 255.0
	return cv2.resize(image, (size, size), interpolation=cv2.INTER_AREA)


for arg in sys.argv[1:]:
	material, _, packs = arg.partition("=")
	display = np.asarray(Image.open(f"out/calibrate/material-swatches/{material}-sun/studio.png").convert("RGB"), dtype=np.float32)
	gray = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
	scale = 32.0 / PX_PER_STUD
	small = cv2.resize(gray, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA)
	h, w = small.shape
	patches = [small[y : y + 96, x : x + 96] for y in (h // 2 - 108, h // 2 + 12) for x in (w // 2 - 108, w // 2 + 12)]
	line = f"{material:13s} contrast {small.std() / max(small.mean(), 1e-6):.4f}"
	for entry in packs.split(","):
		pack, _, studs = entry.partition(":")
		size = int(round(32 * float(studs or 8)))
		normal_path = os.path.join(PACKS, f"{pack}_normal.png")
		if not os.path.exists(normal_path):
			line += f"  {pack}: no normal"
			continue
		normal = load(normal_path, size) * 2.0 - 1.0
		color_path = os.path.join(PACKS, f"{pack}_color.png")
		albedo = (load(color_path, size) ** 2).mean(axis=2) if os.path.exists(color_path) else np.ones((size, size), np.float32)
		results = []
		for green in (1.0, -1.0):
			n = normal * np.array([1.0, green, 1.0], dtype=np.float32)
			n /= np.linalg.norm(n, axis=2, keepdims=True) + 1e-6
			shading = (np.maximum(n @ LIGHT, 0.0) * albedo).astype(np.float32)
			tiled = np.tile(shading, (int(np.ceil(192 / size)) + 1,) * 2)
			results.append(np.mean([cv2.matchTemplate(tiled, patch, cv2.TM_CCOEFF_NORMED).max() for patch in patches]))
		line += f"  {pack}: green-up {results[0]:.3f} green-down {results[1]:.3f}"
	print(line)
