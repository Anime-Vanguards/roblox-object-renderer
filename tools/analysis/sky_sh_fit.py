"""Studio's sky light as a function of the normal, from the sky-light experiment's balls.

Every ball pixel's normal comes from the run's camera (ray-sphere); both images are taken back to
radiance through the display square and the tone curve, divided by the albedo, and fitted with the
9 quadratic polynomials of the normal (the span of L2 spherical harmonics), per channel. Printed: the
fit's per-band coefficients for Studio and ror and their ratio, then Studio / ror over a few normals.

    python tools/analysis/sky_sh_fit.py [out/calibrate/sky-light] [--runs a-d1-s0,b-d1-s0]
"""

import json
import os
import sys

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(__file__))
from sky_ratio import radiance  # noqa: E402

BALLS = [(np.array([-5.0, 5.0, 0.0]), 255), (np.array([0.0, 5.0, 0.0]), 160)]
RADIUS = 1.5


def basis(n):
	x, y, z = n[:, 0], n[:, 1], n[:, 2]
	return np.stack([np.ones_like(x), x, y, z, x * y, y * z, x * z, x * x - z * z, 3 * y * y - 1], axis=1)


BANDS = [("L0", [0]), ("L1", [1, 2, 3]), ("L2", [4, 5, 6, 7, 8])]


def ball_pixels(run, folder):
	camera = np.array(run["camera"], dtype=float)
	position = camera[:3]
	rotation = camera[3:].reshape(3, 3)
	right, up, back = rotation[:, 0], rotation[:, 1], rotation[:, 2]
	studio = np.asarray(Image.open(os.path.join(folder, "studio.png")).convert("RGB"), dtype=float)
	ror = np.asarray(Image.open(os.path.join(folder, "ror.png")).convert("RGB"), dtype=float)
	height, width, _ = studio.shape
	tan = np.tan(np.radians(run["fov"]) / 2)
	ys, xs = np.mgrid[0:height, 0:width]
	ndc_x = (2 * (xs + 0.5) / width - 1) * tan * width / height
	ndc_y = (1 - 2 * (ys + 0.5) / height) * tan
	directions = -back[None, None, :] + ndc_x[..., None] * right + ndc_y[..., None] * up
	directions /= np.linalg.norm(directions, axis=-1, keepdims=True)
	results = []
	for center, gray in BALLS:
		offset = position - center
		b = (directions * offset).sum(-1)
		c = (offset * offset).sum() - RADIUS * RADIUS
		disc = b * b - c
		hit = disc > 0
		t = -b - np.sqrt(np.maximum(disc, 0))
		point = position + directions * t[..., None]
		normal = (point - center) / RADIUS
		facing = -(normal * directions).sum(-1)
		mask = hit & (t > 0) & (facing > 0.35)
		albedo = (gray / 255.0) ** 2
		results.append((gray, normal[mask], radiance(studio[mask]) / albedo, radiance(ror[mask]) / albedo))
	return results


def main():
	args = sys.argv[1:]
	runs = ["a-d1-s0", "b-d1-s0"]
	if "--runs" in args:
		index = args.index("--runs")
		runs = args[index + 1].split(",")
		del args[index : index + 2]
	root = args[0] if args else "out/calibrate/sky-light"
	data = {run["label"]: run for run in json.load(open(os.path.join(root, "samples.json")))}
	gathered = {}
	for label in runs:
		for gray, normals, studio, ror in ball_pixels(data[label], os.path.join(root, label)):
			entry = gathered.setdefault(gray, [[], [], []])
			entry[0].append(normals)
			entry[1].append(studio)
			entry[2].append(ror)
	for gray, (normals, studio, ror) in gathered.items():
		normals = np.concatenate(normals)
		studio = np.concatenate(studio)
		ror = np.concatenate(ror)
		matrix = basis(normals)
		fit_studio, *_ = np.linalg.lstsq(matrix, studio, rcond=None)
		fit_ror, *_ = np.linalg.lstsq(matrix, ror, rcond=None)
		residual_studio = np.sqrt(np.mean((matrix @ fit_studio - studio) ** 2, axis=0))
		residual_ror = np.sqrt(np.mean((matrix @ fit_ror - ror) ** 2, axis=0))
		print(f"== ball {gray}: {len(normals)} pixels; fit rms studio {np.round(residual_studio, 4)} ror {np.round(residual_ror, 4)}")
		names = ["1", "x", "y", "z", "xy", "yz", "xz", "x2-z2", "3y2-1"]
		for index, name in enumerate(names):
			ratio = fit_studio[index] / np.where(np.abs(fit_ror[index]) > 1e-6, fit_ror[index], np.nan)
			print(f"  {name:6s} studio {np.round(fit_studio[index], 4)}  ror {np.round(fit_ror[index], 4)}  ratio {np.round(ratio, 3)}")
		for label, n in [("up", [0, 1, 0]), ("down", [0, -1, 0]), ("+x", [1, 0, 0]), ("-x", [-1, 0, 0]), ("+z", [0, 0, 1]), ("-z", [0, 0, -1])]:
			row = basis(np.array([n], dtype=float))
			print(f"  {label:5s} studio/ror {np.round((row @ fit_studio) / (row @ fit_ror), 3)[0]}  studio {np.round(row @ fit_studio, 4)[0]}")


if __name__ == "__main__":
	main()
