"""Texture phase of each material-offsets run: the slab's top face resampled into its own (u, v)
plane (u along the part's -X, v along its -Z, 32 px per stud), matched against the material's
colour map (or normal map when the colour map is flat), giving the texture coordinate at the
face's centre. Studio and ror side by side; ror puts the corner-origin rule's value there.

    python tools/analysis/material_phase.py [material ...]
"""

import json
import math
import os
import re
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = "out/calibrate/material-offsets"
PX = 32
MATERIALS = ["WoodPlanks", "Marble", "CrackedLava", "Pavement", "Brick", "Pebble", "Cobblestone", "DiamondPlate", "Foil", "Metal", "Fabric", "Salt", "Glass", "Cardboard", "Carpet", "CeramicTiles", "ClayRoofTiles", "RoofShingles", "Leather", "Plaster", "Rubber"]


def packs():
	source = open("crates/ror/src/render/materials.rs", encoding="utf-8").read()
	table = {}
	for value, pack, comment in re.findall(r"\((\d+), \"(\d+)\", (?:true|false)\),\s*// (\w+)", source):
		table[comment] = pack
	return table


def texture(pack):
	for name in ("color", "normal"):
		path = f".cache/material-packs/{pack}_{name}_full.png"
		if not os.path.exists(path):
			os.system(f"target\\release\\examples\\tools texture_pack .cache/material-packs --full {pack} > NUL 2>&1")
		if os.path.exists(path):
			image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float32) / 255.0
			plane = (image ** 2).mean(axis=2) if name == "color" else image[..., 0]
			if plane.std() > 0.01:
				return cv2.resize(plane, (8 * PX, 8 * PX), interpolation=cv2.INTER_AREA).astype(np.float32)
	return None


def centre_uv(label, who, index, variant, tex):
	run = RUNS[label]
	display = np.asarray(Image.open(f"{ROOT}/{label}/{who}.png").convert("RGB"), dtype=np.float32)
	radiance = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
	height, width = radiance.shape
	eye = np.array(run["camera"][0:3])
	m = np.array(run["camera"][3:12]).reshape(3, 3)
	right, up, back = m[:, 0], m[:, 1], m[:, 2]
	scale = (height / 2) / math.tan(math.radians(run["fov"]) / 2)
	center = np.array([index * 60.0, 0.5, 0.0]) + (np.array([37.3, 5.1, 11.7]) if variant == "b" else 0)
	angle = math.radians(90.0 if variant == "b" else 0.0)
	# part axes in world: X and Z rotated about Y
	x_axis = np.array([math.cos(angle), 0, -math.sin(angle)])
	z_axis = np.array([math.sin(angle), 0, math.cos(angle)])
	a = (np.arange(12 * PX) + 0.5) / PX - 6
	aa, bb = np.meshgrid(a, a)
	points = center + aa[..., None] * -x_axis + bb[..., None] * -z_axis
	d = points - eye
	x, y, z = d @ right, d @ up, d @ back
	face = cv2.remap(radiance, (width / 2 + x / -z * scale - 0.5).astype(np.float32), (height / 2 - y / -z * scale - 0.5).astype(np.float32), cv2.INTER_LINEAR)
	if tex is not None and tex.std() > 0.01 and np.abs(np.corrcoef(tex.ravel()[:1000], tex.ravel()[:1000])[0, 1]) >= 0:
		patch = face[4 * PX : 8 * PX, 4 * PX : 8 * PX]
		result = cv2.matchTemplate(np.tile(tex, (3, 3)), patch, cv2.TM_CCOEFF_NORMED)
		_, value, _, (lx, ly) = cv2.minMaxLoc(result)
		return value, ((lx + 2 * PX) / (8 * PX)) % 1, ((ly + 2 * PX) / (8 * PX)) % 1
	return None


RUNS = {run["label"]: run for run in json.load(open(f"{ROOT}/samples.json"))}
table = packs()
only = set(sys.argv[1:])
for index, material in enumerate(MATERIALS):
	if only and material not in only:
		continue
	tex = texture(table[material]) if material in table else None
	line = f"{material:13s}"
	for variant in ("a", "b"):
		for who in ("studio", "ror"):
			result = centre_uv(f"{material}-{variant}", who, index, variant, tex)
			line += f"  {variant}/{who} " + (f"({result[1]:.3f},{result[2]:.3f}) {result[0]:.2f}" if result else "-")
	print(line, flush=True)
