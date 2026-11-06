"""Which texture pack each material-swatches capture shows: 2-stud patches of the flat-ambient
capture (radiance, at 32 px per stud) matched against every pack's colour map (albedo, 256 px per
8 studs, eight orientations, wrapped) by normalised cross-correlation.

Needs the packs decoded by the texture_pack example into <state dir>/material-packs (and its log;
the state dir is ROR_STATE_DIR, else .cache).

    python tools/analysis/swatch_identify.py [material ...]
"""

import os
import re
import sys

import cv2
import numpy as np
from PIL import Image

from tone import radiance

ROOT = "out/calibrate/material-swatches"
STATE = os.environ.get("ROR_STATE_DIR") or ".cache"
PACKS = os.path.join(STATE, "material-packs")
PX_PER_STUD = 558.0 / (59.0 * np.tan(np.radians(10.0)))


def packs():
	result = []
	index = -1
	for line in open(os.path.join(STATE, "material-packs.log")):
		match = re.match(r"(\d+): texturepack", line)
		if match:
			index += 1
			result.append({"index": index, "id": match.group(1), "maps": {}})
			continue
		match = re.match(r"\s+(\w+) (\d+): \S+ (\d+)x", line)
		if match:
			result[-1]["maps"][match.group(1)] = int(match.group(3))
	return result


def orientations(image):
	for flip in (False, True):
		base = image[:, ::-1] if flip else image
		for turns in range(4):
			yield f"{'f' if flip else ''}r{turns * 90}", np.ascontiguousarray(np.rot90(base, turns))


def main():
	only = set(sys.argv[1:])
	candidates = []
	for pack in packs():
		if pack["maps"].get("color", 0) <= 4:
			continue
		texel = np.asarray(Image.open(os.path.join(PACKS, f"{pack['id']}_color.png")).convert("RGB"), dtype=np.float32) / 255.0
		gray = (texel ** 2).mean(axis=2)
		gray = cv2.resize(gray, (256, 256), interpolation=cv2.INTER_AREA)
		for name, oriented in orientations(gray):
			candidates.append((pack, name, np.tile(oriented, (2, 2))))
	scale = 32.0 / PX_PER_STUD
	for run in sorted(name for name in os.listdir(ROOT) if name.endswith("-flat")):
		material = run[: -len("-flat")]
		if only and material not in only:
			continue
		display = np.asarray(Image.open(os.path.join(ROOT, run, "studio.png")).convert("RGB"), dtype=np.float32)
		gray = radiance(display).mean(axis=2).astype(np.float32)
		small = cv2.resize(gray, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA)
		h, w = small.shape
		if small.std() < 1e-3 * max(small.mean(), 1e-6):
			print(f"{material:14s} untextured (contrast {small.std() / max(small.mean(), 1e-6):.4f})")
			continue
		patches = [small[y : y + 64, x : x + 64] for y in (h // 2 - 112, h // 2 - 32, h // 2 + 48) for x in (w // 2 - 112, w // 2 - 32, w // 2 + 48)]
		scores = {}
		for pack, name, tiled in candidates:
			values = [float(cv2.matchTemplate(tiled, patch, cv2.TM_CCOEFF_NORMED).max()) for patch in patches]
			score = float(np.mean(values))
			key = pack["index"]
			if key not in scores or score > scores[key][0]:
				scores[key] = (score, pack["id"], name)
		ranked = sorted(scores.items(), key=lambda item: -item[1][0])[:4]
		text = "  ".join(f"#{index} {pid} {name} {score:.3f}" for index, (score, pid, name) in ranked)
		print(f"{material:14s} {text}", flush=True)


main()
