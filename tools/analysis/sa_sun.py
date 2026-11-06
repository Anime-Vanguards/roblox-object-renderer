"""The direct specular law as it stood until the metal round (notes/render.md "Direct specular")
against sa-sun's spheres, and meshpart-terms per part (notes/render.md "MeshParts"). The law ror draws
now (a linear visibility k, Schlick's rise, the metals' compensation) is fitted jointly to sa-sun,
metal-sun, ibl-scale and sun-grazing by tools/analysis/direct_specular.py.

    python tools/analysis/sa_sun.py fit [lowest roughness]     fit [w, g, c, kk] bin by bin
    python tools/analysis/sa_sun.py law [w,g,c,kk]              a law's ratios per sphere and band (default: ror's)
    python tools/analysis/sa_sun.py material                    SmoothPlastic's and Plastic's roughness under ror's law
    python tools/analysis/sa_sun.py terms                       meshpart-terms: Studio's radiance and Studio / ror per part

The law: GGX of width a = w + (1 - w) r^2, Schlick-GGX visibility with k = kk (r + 1)^2 / 8, the
dielectric reflectance 0.039 g (1 + c a^2), times 1 + F0 (1 / E - 1) with E Studio's split-sum table at
the prefiltered level's roughness (0.075 + 0.9 r). Each bin is one view, sphere and 10-degree band of
the half angle (sa-sun's black spheres: the sun's specular alone), weighted alike in log ratio, so the
lobe's tail counts as much as its core. Replay the experiments first (`ror studio calibrate sa-sun`,
`meshpart-terms`).
"""

import glob
import json
import math
import os
import subprocess
import sys

import numpy as np
from PIL import Image
from scipy import ndimage
from scipy.optimize import least_squares

from tone import GRID as _xs, TONED as _ys

ROR = os.path.join("target", "release", "ror.exe")
ROOT = os.path.join("out", "calibrate")
LUT = np.interp((np.arange(256) / 255.0) ** 2, _ys, _xs)
BRIGHTNESS = 2.0
F0 = 0.039
# Studio's display of black (radiance)
FLOOR = 0.0006
ROR_LAW = [0.038, 1.10, 1.0, 1.17]
SPHERES = [(f"sa_r{r}", r / 255) for r in [22, 43, 61, 76, 115, 130, 149, 168, 202, 255]] + [("ball272", 0.41), ("ball256", 0.6), ("mesh272", 0.41), ("mesh256", 0.6)]


def studio_table():
	"""Studio's split-sum table, [n.v row, roughness column] -> (scale, bias)."""
	paths = sorted(glob.glob(os.path.expandvars(r"%LOCALAPPDATA%\Roblox\Versions\*\PlatformContent\pc\textures\brdfLUT.dds")), key=os.path.getmtime)
	data = open(paths[-1], "rb").read()
	size = int.from_bytes(data[16:20], "little")
	return np.frombuffer(data[148 : 148 + size * size * 4], dtype=np.float16).astype(np.float64).reshape(size, size, 2)


TABLE = studio_table()


def table(ndv, rough):
	size = TABLE.shape[0]
	y = np.clip(ndv * size - 0.5, 0, size - 1)
	x = np.clip(np.full_like(ndv, rough) * size - 0.5, 0, size - 1)
	y0, x0 = np.floor(y).astype(int), np.floor(x).astype(int)
	y1, x1 = np.minimum(y0 + 1, size - 1), np.minimum(x0 + 1, size - 1)
	fy, fx = (y - y0)[..., None], (x - x0)[..., None]
	return (TABLE[y0, x0] * (1 - fx) + TABLE[y0, x1] * fx) * (1 - fy) + (TABLE[y1, x0] * (1 - fx) + TABLE[y1, x1] * fx) * fy


def sun_direction(clock, latitude):
	h = (clock - 12) / 24 * 2 * math.pi
	t = math.radians(latitude - 23.5)
	return np.array([-math.sin(h) * math.cos(t), math.cos(h) * math.cos(t), math.sin(t)])


def sphere_pixels(meta, center, radius, width, height, shrink=0.95):
	"""Pixels on a probe's sphere (inside `shrink` of its radius): mask, normals, view directions."""
	c = meta["camera"]
	eye = np.array(c[:3])
	rot = np.array(c[3:]).reshape(3, 3)
	right, up, look = rot[:, 0], rot[:, 1], -rot[:, 2]
	t = math.tan(math.radians(meta["fov"]) / 2)
	ys, xs = np.mgrid[0:height, 0:width]
	nx = (xs + 0.5) / width * 2 - 1
	ny = 1 - (ys + 0.5) / height * 2
	d = look[None, None, :] + right[None, None, :] * (nx * t * width / height)[..., None] + up[None, None, :] * (ny * t)[..., None]
	d /= np.linalg.norm(d, axis=2, keepdims=True)
	oc = eye - center
	b = (d * oc).sum(-1)
	disc = b * b - ((oc * oc).sum() - (radius * shrink) ** 2)
	s = -b - np.sqrt(np.maximum(disc, 0))
	n = (eye + d * s[..., None] - center) / radius
	n /= np.linalg.norm(n, axis=-1, keepdims=True)
	return disc > 0, n, -d


def bins():
	"""(sphere, roughness, view, band, n.h, n.v, n.l, Studio's mean radiance) for every bin of sa-sun."""
	out = []
	metas = [m for m in json.load(open(os.path.join(ROOT, "sa-sun", "samples.json"))) if m["label"].startswith("view")]
	for meta in metas:
		view = meta["label"][4:]
		env = meta["environment"]
		l = sun_direction(env["clockTime"], env["latitude"])
		studio = LUT[np.asarray(Image.open(os.path.join(ROOT, "sa-sun", meta["label"], "studio.png")).convert("RGB"))].mean(axis=2) - FLOOR
		height, width = studio.shape
		for name, rough in SPHERES:
			sample = next((s for s in meta["samples"] if s["probe"] == f"v{view}_{name}"), None)
			if sample is None:
				continue
			mask, n, v = sphere_pixels(meta, np.array(sample["inputs"]["center"]), sample["inputs"]["radius"], width, height)
			h = v + l
			h /= np.linalg.norm(h, axis=-1, keepdims=True)
			ndh, ndv, ndl = (n * h).sum(-1), (n * v).sum(-1), (n * l).sum(-1)
			ok = mask & (ndl > 0.1) & (ndv > 0.1)
			angle = np.degrees(np.arccos(np.clip(ndh, -1, 1)))
			for band in range(0, 80, 10):
				m = ok & (angle >= band) & (angle < band + 10)
				if m.sum() < 60 or studio[m].mean() < 0.004:
					continue
				out.append((name, rough, view, band, ndh[m], ndv[m], ndl[m], studio[m].mean()))
	return out


def law(p, rough, ndh, ndv, ndl):
	"""The law's mean radiance over a bin (a black dielectric, the sun at BRIGHTNESS)."""
	w, g, c, kk = p
	a = w + (1 - w) * rough * rough
	d = ndh * ndh * (a * a - 1) + 1
	distribution = a * a / (math.pi * d * d)
	k = min(kk * (rough + 1) ** 2 / 8, 1.0)
	visibility = 0.25 / ((ndv * (1 - k) + k) * (ndl * (1 - k) + k))
	ab = table(ndv, min(0.075 + 0.9 * rough, 1.0))
	energy = 1 + F0 * (1 / np.maximum(ab[:, 0] + ab[:, 1], 0.05) - 1)
	return (distribution * visibility * F0 * g * (1 + c * a * a) * energy * math.pi * BRIGHTNESS * ndl).mean()


def old_law(rough, ndh, ndv, ndl):
	"""ror before the law: GGX of width r^2, Schlick-GGX with k = a / 2, F0 alone."""
	a = rough * rough
	d = ndh * ndh * (a * a - 1) + 1
	k = a / 2
	visibility = 0.25 / ((ndv * (1 - k) + k) * (ndl * (1 - k) + k))
	return (a * a / (math.pi * d * d) * visibility * F0 * math.pi * BRIGHTNESS * ndl).mean()


def report(data, errors):
	print(f"   log ratio rms {math.sqrt(np.mean(np.square(errors))):.4f} over {len(errors)} bins; Studio / law per band:")
	by = {}
	for (name, _, _, band, *_), e in zip(data, errors):
		by.setdefault(name, {}).setdefault(band, []).append(e)
	for name, _ in SPHERES:
		if name in by:
			print(f"   {name:8s} " + " ".join(f"{band:2d}: {math.exp(-np.mean(v)):.2f}" for band, v in sorted(by[name].items())))


def command_fit(low="0.08"):
	data = [b for b in bins() if b[1] >= float(low) - 1e-6 and not b[0].startswith("mesh")]
	fit = least_squares(lambda p: [math.log(law(p, r, ndh, ndv, ndl) / s) for _, r, _, _, ndh, ndv, ndl, s in data], ROR_LAW, bounds=([0.0, 0.3, -0.5, 0.3], [0.3, 4.0, 4.0, 4.0]))
	print(f"w {fit.x[0]:.4f} g {fit.x[1]:.4f} c {fit.x[2]:.4f} kk {fit.x[3]:.4f}")
	report(data, fit.fun)


def command_law(params=None):
	p = [float(v) for v in params.split(",")] if params else ROR_LAW
	data = [b for b in bins() if not b[0].startswith("mesh")]
	print(f"law {p}")
	report(data, [math.log(law(p, r, ndh, ndv, ndl) / s) for _, r, _, _, ndh, ndv, ndl, s in data])
	print("before (GGX of width r^2, k = a / 2, F0 alone; SmoothPlastic 0.43)")
	report(data, [math.log(old_law(0.43 if name.endswith("272") else r, ndh, ndv, ndl) / s) for name, r, _, _, ndh, ndv, ndl, s in data])


def command_material():
	data = bins()
	for material, names in [("SmoothPlastic", ["ball272", "mesh272"]), ("Plastic", ["ball256", "mesh256"])]:
		print(material)
		for rough in [0.37, 0.39, 0.41, 0.43, 0.45, 0.55, 0.58, 0.60, 0.62, 0.65]:
			errors = np.array([math.log(law(ROR_LAW, rough, ndh, ndv, ndl) / s) for name, _, _, _, ndh, ndv, ndl, s in data if name in names])
			print(f"   r {rough:.2f}: log ratio rms {math.sqrt(np.mean(errors**2)):.4f}, mean {np.mean(errors):+.4f} ({len(errors)} bins)")


def command_terms():
	directory = os.path.join(ROOT, "meshpart-terms")
	metas = json.load(open(os.path.join(directory, "samples.json")))
	c = metas[0]["camera"]
	eye, look = c[:3], [-c[5], -c[8], -c[11]]
	camera = {"mode": "lookAt", "eye": eye, "target": [e + 10 * d for e, d in zip(eye, look)], "fov": metas[0]["fov"]}
	out = os.path.join(".cache", "sa-sun-terms")
	subprocess.run([ROR, "render", os.path.join(directory, "model.rbxm"), "--size", "1644x1116", "--camera", json.dumps(camera), "--ids", "--out", out], check=True, capture_output=True)
	ids = np.asarray(Image.open(os.path.join(out, "render.ids.png")).convert("RGB")).astype(np.int64)
	ids = ids[..., 0] + 256 * ids[..., 1] + 65536 * ids[..., 2]
	parts = json.load(open(os.path.join(out, "parts.json")))
	runs = [m["label"] for m in metas]
	images = {run: (LUT[np.asarray(Image.open(os.path.join(directory, run, "studio.png")).convert("RGB"))].mean(axis=2), LUT[np.asarray(Image.open(os.path.join(directory, run, "ror.png")).convert("RGB"))].mean(axis=2)) for run in runs}
	print("Studio's mean radiance / Studio over ror, per part and run")
	print(f"{'part':18s}" + "".join(f"{run:>16s}" for run in runs))
	for index, part in enumerate(parts):
		mask = ndimage.binary_erosion(ids == index + 1, iterations=2)
		if mask.sum() < 40:
			continue
		cells = []
		for run in runs:
			studio, ror = images[run]
			cells.append(f"{studio[mask].mean():8.4f} {studio[mask].mean() / max(ror[mask].mean(), 1e-6):5.3f}")
		print(f"{part['path'].split('.')[-1]:18s}" + "".join(f"{cell:>16s}" for cell in cells))


if __name__ == "__main__":
	command, args = sys.argv[1], sys.argv[2:]
	{"fit": command_fit, "law": command_law, "material": command_material, "terms": command_terms}[command](*args)
