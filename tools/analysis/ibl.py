"""Image-based lighting fits (notes/render.md "Sky specular", "Metals").

    python tools/analysis/ibl.py lut                        Studio's brdfLUT.dds against split-sum variants
    python tools/analysis/ibl.py spheres <experiment> [run]  per sphere: Studio / ror and rms per pixel
    python tools/analysis/ibl.py collapse <probe>           ibl-spheres split sky: one sphere binned by R.up, N.up, n.v
    python tools/analysis/ibl.py kernel                     ibl-spheres split sky: Studio's prefilter kernel per roughness
    python tools/analysis/ibl.py scale                      ibl-scale: the specular against s, the diffuse attenuation fit

Reads out/calibrate/<experiment>/ (studio.png, ror.png, samples.json; sphere probes carry their
centre and radius) and Studio's install for the table.
"""

import glob
import json
import math
import os
import sys

import numpy as np
from PIL import Image

from tone import radiance

ROOT = os.path.join("out", "calibrate")
WHITE = 1.935
DARK = ((np.array([45, 47, 49]) / 255.0) ** 2 * 1.935).mean()


def load_runs(experiment):
	with open(os.path.join(ROOT, experiment, "samples.json")) as f:
		return {run["label"]: run for run in json.load(f)}


def load_image(experiment, run, which):
	path = os.path.join(ROOT, experiment, run, which + ".png")
	return radiance(np.asarray(Image.open(path).convert("RGB"), dtype=np.float64))


def camera_rays(run, width=1644, height=1116):
	c = run["camera"]
	eye = np.array(c[:3])
	right, up, back = np.array([c[3], c[6], c[9]]), np.array([c[4], c[7], c[10]]), np.array([c[5], c[8], c[11]])
	t = math.tan(math.radians(run["fov"]) * 0.5)
	ys, xs = np.mgrid[0:height, 0:width]
	d = ((2 * (xs + 0.5) / width - 1) * t * width / height)[..., None] * right + ((1 - 2 * (ys + 0.5) / height) * t)[..., None] * up - back
	return eye, d / np.linalg.norm(d, axis=-1, keepdims=True)


def sphere_pixels(eye, rays, center, radius, margin=3.0):
	"""Pixels hitting the sphere away from its outline; normals and view vectors."""
	oc = eye - np.asarray(center)
	b = (rays * oc).sum(-1)
	disc = b * b - ((oc * oc).sum() - radius * radius)
	t = -b - np.sqrt(np.maximum(disc, 0))
	n = (eye + rays * t[..., None] - np.asarray(center)) / radius
	v = -rays
	nv = (n * v).sum(-1)
	mask = (disc > 0) & (nv > math.sin(margin / rays.shape[0] * np.linalg.norm(oc) / radius * 2))
	return mask, n, v


def reflect(v, n):
	return 2 * (n * v).sum(-1, keepdims=True) * n - v


def spheres_in_view(run):
	return [(s["probe"], s["inputs"]) for s in run["samples"] if "radius" in s["inputs"] and 0 <= s["pixel"][0] < 1644 and 0 <= s["pixel"][1] < 1116]


def studio_lut():
	"""Studio's split-sum table, [n.v row, roughness column] -> (scale, bias)."""
	paths = sorted(glob.glob(os.path.expandvars(r"%LOCALAPPDATA%\Roblox\Versions\*\PlatformContent\pc\textures\brdfLUT.dds")), key=os.path.getmtime)
	data = open(paths[-1], "rb").read()
	size = int.from_bytes(data[16:20], "little")
	return np.frombuffer(data[148 : 148 + size * size * 4], dtype=np.float16).astype(np.float64).reshape(size, size, 2)


def hammersley(count):
	i = np.arange(count, dtype=np.uint32)
	b = (i << 16) | (i >> 16)
	b = ((b & 0x55555555) << 1) | ((b & 0xAAAAAAAA) >> 1)
	b = ((b & 0x33333333) << 2) | ((b & 0xCCCCCCCC) >> 2)
	b = ((b & 0x0F0F0F0F) << 4) | ((b & 0xF0F0F0F0) >> 4)
	b = ((b & 0x00FF00FF) << 8) | ((b & 0xFF00FF00) >> 8)
	return i / count, b.astype(np.float64) * 2.3283064365386963e-10


def split_sum(n_dot_v, roughness, visibility, samples=2048):
	"""(scale, bias) of F0 for arrays of n.v and roughness."""
	nv = np.asarray(n_dot_v, dtype=np.float64)[:, None]
	a = np.asarray(roughness, dtype=np.float64)[:, None] ** 2
	x1, x2 = hammersley(samples)
	cos_t = np.sqrt((1 - x2) / (1 + (a * a - 1) * x2))
	sin_t = np.sqrt(np.maximum(0, 1 - cos_t * cos_t))
	hx, hz = sin_t * np.cos(2 * np.pi * x1), cos_t
	vh = np.maximum(np.sqrt(1 - nv * nv) * hx + nv * hz, 0)
	nl = 2 * vh * hz - nv
	ok = nl > 0
	nl = np.maximum(nl, 1e-9)
	if visibility == "height-correlated":
		g = 2 * nv * nl / (nl * np.sqrt(nv * nv * (1 - a * a) + a * a) + nv * np.sqrt(nl * nl * (1 - a * a) + a * a))
	else:
		k = a / 2
		g = nv / (nv * (1 - k) + k) * nl / (nl * (1 - k) + k)
	gvis = np.where(ok, g * vh / np.maximum(hz * nv, 1e-9), 0)
	fc = (1 - vh) ** 5
	return ((1 - fc) * gvis).mean(1), (fc * gvis).mean(1)


def command_lut():
	table = studio_lut()
	rows, cols = np.meshgrid(np.arange(0, 256, 16), np.arange(0, 256, 16), indexing="ij")
	nv, rough = (rows.ravel() + 0.5) / 256, (cols.ravel() + 0.5) / 256
	studio = table[rows.ravel(), cols.ravel()]
	for visibility in ["height-correlated", "schlick-ggx k = a / 2"]:
		a, b = split_sum(nv, rough, visibility)
		print(f"{visibility:24s} mean |error| scale {np.abs(a - studio[:, 0]).mean():.4f} bias {np.abs(b - studio[:, 1]).mean():.4f}")


def command_spheres(experiment, needle=""):
	for label, run in load_runs(experiment).items():
		if needle not in label:
			continue
		studio, ror = load_image(experiment, label, "studio"), load_image(experiment, label, "ror")
		eye, rays = camera_rays(run)
		print(f"== {label}")
		for probe, inputs in spheres_in_view(run):
			mask, n, v = sphere_pixels(eye, rays, inputs["center"], inputs["radius"])
			mask &= (n * v).sum(-1) > 0.25
			s, r = studio[mask], ror[mask]
			print(f"  {probe:20s} studio {s.mean():.4f}  studio / ror {s.mean() / r.mean():.3f}  rms {np.sqrt(np.mean((s - r) ** 2)) / s.mean():.3f}")


def command_collapse(probe_name):
	runs = load_runs("ibl-spheres")
	edges = np.linspace(-1, 1, 11)
	for key in ["R.up", "N.up", "n.v"]:
		print(f"== {probe_name} by {key} (bins {', '.join(f'{e:.1f}' for e in (edges[:-1] + edges[1:]) / 2)})")
		for label in ["split-horizon", "split-above", "split-top"]:
			run = runs[label]
			studio = load_image("ibl-spheres", label, "studio").mean(-1)
			eye, rays = camera_rays(run)
			for probe, inputs in spheres_in_view(run):
				if probe != probe_name:
					continue
				mask, n, v = sphere_pixels(eye, rays, inputs["center"], inputs["radius"])
				x = {"R.up": reflect(v[mask], n[mask])[:, 1], "N.up": n[mask][:, 1], "n.v": (n[mask] * v[mask]).sum(-1) * 2 - 1}[key]
				s = studio[mask]
				cells = [f"{s[(x >= lo) & (x < hi)].mean():5.3f}" if ((x >= lo) & (x < hi)).sum() > 20 else "  -  " for lo, hi in zip(edges[:-1], edges[1:])]
				print(f"  {label:14s} " + " ".join(cells))


def command_kernel(step=5.0):
	from scipy.optimize import nnls

	runs = load_runs("ibl-spheres")
	count = 60000
	i = np.arange(count) + 0.5
	phi, theta = np.arccos(1 - 2 * i / count), np.pi * (1 + 5**0.5) * i
	grid = np.stack([np.cos(theta) * np.sin(phi), np.cos(phi), np.sin(theta) * np.sin(phi)], -1)
	sky = np.where((np.abs(grid[:, 0]) < grid[:, 1]) & (np.abs(grid[:, 2]) < grid[:, 1]), WHITE, DARK)
	edges = np.radians(np.arange(0, 180 + step, step))
	solid = 2 * np.pi * (np.cos(edges[:-1]) - np.cos(edges[1:]))
	data = {}
	for label in ["split-horizon", "split-above", "split-top"]:
		run = runs[label]
		studio = load_image("ibl-spheres", label, "studio").mean(-1)
		eye, rays = camera_rays(run)
		for probe, inputs in spheres_in_view(run):
			if inputs.get("kind") != "appearance" or inputs["color"] != [255, 255, 255] or inputs["metalness"] < 1:
				continue
			mask, n, v = sphere_pixels(eye, rays, inputs["center"], inputs["radius"])
			idx = np.argwhere(mask)[:: max(1, int(mask.sum()) // 700)]
			entry = data.setdefault(inputs["roughness"], ([], []))
			entry[0].append(reflect(v[idx[:, 0], idx[:, 1]], n[idx[:, 0], idx[:, 1]]))
			entry[1].append(studio[idx[:, 0], idx[:, 1]])
	centers = np.degrees((edges[:-1] + edges[1:]) / 2)
	for rough, (rs, ss) in sorted(data.items()):
		r, s = np.concatenate(rs), np.concatenate(ss)
		bins = np.digitize(np.arccos(np.clip(r @ grid.T, -1, 1)), edges) - 1
		design = np.stack([((bins == b) * sky[None, :]).sum(1) * 4 * np.pi / count for b in range(len(solid))], 1)
		weights, _ = nnls(design, s)
		level = (weights * solid).sum()
		half = centers[np.searchsorted(np.cumsum(weights * solid) / level, 0.5)]
		rms = np.sqrt(np.mean((design @ weights - s) ** 2)) / s.mean()
		print(f"roughness {rough:.3f}: level {level:.3f} half-energy angle {half:5.1f} deg rms {rms:.3f}")


def command_scale():
	from scipy.optimize import least_squares

	runs = load_runs("ibl-scale")
	run = next(iter(runs.values()))
	eye, rays = camera_rays(run)
	table = studio_lut()
	images = {label: load_image("ibl-scale", label, "studio") for label in runs}
	rows = []
	print("specular alone relative to s = 1:")
	for probe, inputs in spheres_in_view(run):
		if inputs.get("kind") != "appearance":
			continue
		mask, n, v = sphere_pixels(eye, rays, inputs["center"], inputs["radius"])
		one = images["specular-s1"][mask].mean()
		curve = [images[f"specular-s{s:g}"][mask].mean() / one for s in [0.1, 0.25, 0.5, 0.75, 0.9]]
		print(f"  {probe:18s} " + " ".join(f"{x:.3f}" for x in curve))
		albedo = np.asarray(inputs["color"], dtype=np.float64) / 255.0
		albedo = np.where(albedo <= 0.04045, albedo / 12.92, ((albedo + 0.055) / 1.055) ** 2.4)
		if albedo.max() < 0.01:
			continue
		idx = np.argwhere(mask)[:: max(1, int(mask.sum()) // 300)]
		nv = (n[idx[:, 0], idx[:, 1]] * v[idx[:, 0], idx[:, 1]]).sum(-1)
		alone = images["diffuse-s0"][idx[:, 0], idx[:, 1]]
		for s in [0.25, 0.5, 0.75, 1.0]:
			added = images[f"diffuse-s{s:g}"][idx[:, 0], idx[:, 1]] - images[f"specular-s{s:g}"][idx[:, 0], idx[:, 1]]
			for ch in range(3):
				for value, base, x in zip(added[:, ch], alone[:, ch], nv):
					if base > 0.01:
						rows.append((s, inputs["metalness"], inputs["roughness"], albedo[ch], x, value / base, base))
	s, m, rough, albedo, nv, observed, weight = np.array(rows).T
	lod = 0.375 + 4.5 * rough
	y = np.clip(nv * 256 - 0.5, 0, 255)
	x = np.clip(lod / 5 * 256 - 0.5, 0, 255)
	ab = table[np.round(y).astype(int), np.round(x).astype(int)]

	def model(p):
		mu, c, f0 = p
		me = mu * m * s
		f0e = f0 * (1 - me) + albedo * me
		return (1 - me) * (1 - s * c * (f0e * ab[:, 0] + ab[:, 1]) / (ab[:, 0] + ab[:, 1]))

	fit = least_squares(lambda p: (model(p) - observed) * np.sqrt(weight), (1.0, 1.0, 0.04))
	print(f"diffuse kd = (1 - mu m s)(1 - s c F): mu {fit.x[0]:.3f} c {fit.x[1]:.3f} f0 {fit.x[2]:.4f} rms {np.sqrt(np.mean((model(fit.x) - observed) ** 2)):.4f}")


if __name__ == "__main__":
	command, args = sys.argv[1], sys.argv[2:]
	{"lut": command_lut, "spheres": command_spheres, "collapse": command_collapse, "kernel": command_kernel, "scale": command_scale}[command](*args)
