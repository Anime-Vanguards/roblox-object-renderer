"""The sky specular's and the metalness's fade with the view depth (notes/render.md "Specular fade") and
the tone curve's toe (notes/render.md "Colour pipeline and tone curve"). Replay the experiments first
(`ror studio calibrate ibl-panels`, `ibl-fade`, `tone-toe`; captures are cached).

    python tools/analysis/specular_fade.py panels    ibl-panels: Studio's radiance per strip and distance, relative to 6 studs, and Studio / ror
    python tools/analysis/specular_fade.py spheres   ibl-fade: Studio / ror per sphere and distance (green), sky specular alone
    python tools/analysis/specular_fade.py score     log ratio rms over the bright cases (both), for fits with ROR_TUNE_SPECULAR_FADE
    python tools/analysis/specular_fade.py toe       tone-toe: the toe B and input scale k against Studio's gray tiles

Displays are turned back into radiance through ror's tone curve (shaders/resolve.wgsl).
"""

import json
import math
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares

ROOT = os.path.join("out", "calibrate")
A, C, D = 2.725891, 2.307040, 1.951868
TOE, SCALE = 0.00329, 1.0098


def curve(x, b=TOE, k=SCALE):
	x = np.maximum(np.asarray(x, dtype=np.float64) * k, 0)
	return np.minimum(x * (A * x + b) / (x * (C * x + D) + 1), 1)


_xs = np.concatenate([np.linspace(0, 1, 200001), np.linspace(1, 6, 100001)[1:]])
LUT = np.interp((np.arange(256) / 255.0) ** 2, curve(_xs), _xs)


def load(experiment, label, who):
	return LUT[np.asarray(Image.open(os.path.join(ROOT, experiment, label, f"{who}.png")).convert("RGB"))]


def view_rays(meta, width, height):
	c = meta["camera"]
	eye = np.array(c[:3])
	rotation = np.array(c[3:]).reshape(3, 3)
	right, up, look = rotation[:, 0], rotation[:, 1], -rotation[:, 2]
	t = math.tan(math.radians(meta["fov"]) / 2)
	ys, xs = np.mgrid[0:height, 0:width]
	nx = (xs + 0.5) / width * 2 - 1
	ny = 1 - (ys + 0.5) / height * 2
	d = look + right * (nx * t * width / height)[..., None] + up * (ny * t)[..., None]
	return eye, d / np.linalg.norm(d, axis=2, keepdims=True)


def strips():
	"""ibl-panels: {strip: [(distance, studio rgb, ror rgb)]}, each strip's middle (head-on)."""
	rows = {}
	for meta in json.load(open(os.path.join(ROOT, "ibl-panels", "samples.json"))):
		studio, ror = load("ibl-panels", meta["label"], "studio"), load("ibl-panels", meta["label"], "ror")
		eye, d = view_rays(meta, studio.shape[1], studio.shape[0])
		for sample in meta["samples"]:
			center, size = np.array(sample["inputs"]["center"]), np.array(sample["inputs"]["size"])
			t = (center[2] - size[2] / 2 - eye[2]) / d[..., 2]
			hit = eye + d * t[..., None]
			mask = (np.abs(hit[..., 0] - center[0]) < size[0] * 0.35) & (np.abs(hit[..., 1] - center[1]) < size[1] * 0.3)
			if mask.sum() < 500:
				continue
			name = sample["probe"].split("_", 1)[1]
			rows.setdefault(name, []).append((float(meta["label"][1:]), studio[mask].mean(axis=0), ror[mask].mean(axis=0)))
	return rows


def spheres(limits=None):
	"""ibl-fade's sky runs: {sphere: [(distance, studio green, ror green)]} over each sphere (n.v > 0.15)."""
	rows = {}
	for meta in json.load(open(os.path.join(ROOT, "ibl-fade", "samples.json"))):
		if not meta["label"].startswith("sky-d"):
			continue
		distance = float(meta["label"][5:])
		studio, ror = load("ibl-fade", meta["label"], "studio"), load("ibl-fade", meta["label"], "ror")
		eye, d = view_rays(meta, studio.shape[1], studio.shape[0])
		for sample in meta["samples"]:
			prefix, name = sample["probe"].split("_", 1)
			if prefix != f"d{meta['label'][5:]}" or (limits is not None and distance > limits.get(name, -1)):
				continue
			center, radius = np.array(sample["inputs"]["center"]), sample["inputs"]["radius"]
			oc = eye - center
			b = (d * oc).sum(-1)
			disc = b * b - ((oc * oc).sum() - (0.9 * radius) ** 2)
			mask = disc > 0
			if mask.sum() < 500:
				continue
			rows.setdefault(name, []).append((distance, studio[mask].mean(axis=0)[1], ror[mask].mean(axis=0)[1]))
	return rows


def main():
	command = sys.argv[1] if len(sys.argv) > 1 else "panels"
	if command == "panels":
		for name, items in strips().items():
			first = items[0][1].mean()
			print(name)
			for distance, studio, ror in items:
				print(f"   {distance:5.0f} studs: Studio {studio.round(4)} ({studio.mean() / first:.3f} of the nearest)  Studio / ror {(studio / ror).round(3)}")
	elif command == "spheres":
		for name, items in spheres().items():
			print(f"{name:22s} " + "  ".join(f"{distance:.0f}: {studio / ror:.3f}" for distance, studio, ror in sorted(items)))
	elif command == "score":
		errors = {}
		for name, items in strips().items():
			group = "metal wall" if name == "sa_metal" else "dielectric walls"
			errors.setdefault(group, []).extend(math.log(studio.mean() / ror.mean()) for _, studio, ror in items)
		# the bright spheres, out to where the fog's grey is still small against them
		for name, items in spheres({"sa_c255_m255_r43": 700, "sa_c130_m255_r149": 450}).items():
			errors.setdefault(name, []).extend(math.log(studio / ror) for _, studio, ror in items)
		total = [value for values in errors.values() for value in values]
		print(f"rms {math.sqrt(np.mean(np.square(total))):.4f}")
		for group, values in errors.items():
			print(f"   {group:20s} mean {np.mean(values):+.3f}  sd {np.std(values):.3f}  ({len(values)})")
	elif command == "toe":
		points = []
		for run in json.load(open(os.path.join(ROOT, "tone-toe", "samples.json"))):
			if not run["label"].startswith("nofog"):
				continue
			ev = run["environment"]["exposureCompensation"]
			ambient = run["environment"]["ambient"][0]
			for sample in run["samples"]:
				# flat ambient: 1.98 Ambient² times the albedo (notes/render.md "Ambient")
				x = 1.98 * ambient * ambient * (sample["inputs"]["gray"] / 255) ** 2 * 2**ev
				points.append((x, np.mean(sample["studio"])))
		x, studio = np.array(points).T
		keep = studio > 0.5
		show = lambda b, k: 255 * np.sqrt(curve(x, b, k))
		for name, start, free_k in [("sun-board's toe", (0.022270, 1.0), None), ("toe alone", (0.022270, 1.0), False), ("toe and scale", (0.022270, 1.0), True)]:
			if free_k is None:
				b, k = start
			elif free_k:
				b, k = least_squares(lambda p: (show(p[0], p[1]) - studio)[keep], list(start)).x
			else:
				b, k = least_squares(lambda p: (show(p[0], 1.0) - studio)[keep], [start[0]]).x[0], 1.0
			error = (show(b, k) - studio)[keep]
			print(f"{name:16s} B {b:.5f} k {k:.4f}: rms {math.sqrt(np.mean(error**2)):.3f} mean {error.mean():+.3f} of 255")
	else:
		print(__doc__)


if __name__ == "__main__":
	main()
