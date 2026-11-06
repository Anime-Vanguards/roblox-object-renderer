"""Which projection maps material textures onto a Ball: every ball pixel of a material-mapping
capture traced to the sphere, given a texture coordinate by each hypothesis (on the cube face of
the dominant axis, with the box face rules), the Brick colour map sampled there, and the result
correlated with the capture.

  gnomonic  the sphere point pushed radially onto the cube face (p / |p_axis|)
  planar    the sphere point projected along the axis onto the face (p itself)
  angle     equal-angle: atan of the in-face components over the axis component

    python tools/analysis/ball_mapping.py [run ...]
"""

import json
import math
import sys

import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = "out/calibrate/material-mapping"
CENTER = np.array([800.0, 0, 0])
RADIUS = 8.0
SIZE = 16.0
TILE = 8.0
# per dominant axis/sign: (u direction, v direction) on the face (box face rules)
FACES = {
	(0, 1): (np.array([0, 0, -1.0]), np.array([0, -1.0, 0])),
	(0, -1): (np.array([0, 0, 1.0]), np.array([0, -1.0, 0])),
	(1, 1): (np.array([-1.0, 0, 0]), np.array([0, 0, -1.0])),
	(1, -1): (np.array([-1.0, 0, 0]), np.array([0, 0, 1.0])),
	(2, 1): (np.array([1.0, 0, 0]), np.array([0, -1.0, 0])),
	(2, -1): (np.array([-1.0, 0, 0]), np.array([0, -1.0, 0])),
}

texel = np.asarray(Image.open(".cache/material-packs/85077012310177_color_full.png").convert("RGB"), dtype=np.float64) / 255.0
albedo = (texel ** 2).mean(axis=2)


def sample(u, v):
	h, w = albedo.shape
	x = (np.mod(u, 1.0) * w).astype(int) % w
	y = (np.mod(v, 1.0) * h).astype(int) % h
	return albedo[y, x]


def rays(run, width, height):
	eye = np.array(run["camera"][0:3])
	m = np.array(run["camera"][3:12]).reshape(3, 3)
	right, up, back = m[:, 0], m[:, 1], m[:, 2]
	t = math.tan(math.radians(run["fov"]) / 2)
	px, py = np.meshgrid(np.arange(width) + 0.5, np.arange(height) + 0.5)
	x = (px - width / 2) / (height / 2) * t
	y = -(py - height / 2) / (height / 2) * t
	d = x[..., None] * right + y[..., None] * up - back
	return eye, d / np.linalg.norm(d, axis=2, keepdims=True)


def main():
	runs = sys.argv[1:] or ["ball-nz", "ball-py", "ball-px", "ball-iso"]
	samples = {run["label"]: run for run in json.load(open(f"{ROOT}/samples.json"))}
	for label in runs:
		display = np.asarray(Image.open(f"{ROOT}/{label}/studio.png").convert("RGB"), dtype=np.float64)
		radiance = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2)
		height, width = radiance.shape
		eye, d = rays(samples[label], width, height)
		oc = eye - CENTER
		b = d @ oc
		c = oc @ oc - RADIUS * RADIUS
		disc = b * b - c
		hit = disc > 0
		t = -b - np.sqrt(np.maximum(disc, 0))
		p = (eye + d * t[..., None] - CENTER)[hit]
		# stay inside the silhouette, away from the faceted rim
		normal = p / RADIUS
		facing = -(d[hit] * normal).sum(axis=1) > 0.35
		p, observed = p[facing], radiance[hit][facing]
		axis = np.argmax(np.abs(p), axis=1)
		sign = np.sign(p[np.arange(len(p)), axis])
		line = f"{label:9s}"
		for name in ("gnomonic", "planar", "angle"):
			u = np.zeros(len(p))
			v = np.zeros(len(p))
			for (a, s), (ud, vd) in FACES.items():
				mask = (axis == a) & (sign == s)
				q = p[mask]
				along = np.abs(q[:, a])
				if name == "gnomonic":
					face = q * (SIZE / 2 / along)[:, None]
				elif name == "planar":
					face = q
				else:
					face = np.arctan2(q, along[:, None]) / (math.pi / 4) * (SIZE / 2)
				u[mask] = (face @ ud + SIZE / 2) / TILE
				v[mask] = (face @ vd + SIZE / 2) / TILE
			predicted = sample(u, v)
			ncc = np.corrcoef(predicted, observed)[0, 1]
			line += f"  {name} {ncc:.3f}"
		print(line, f"({len(p)} px)")


main()
