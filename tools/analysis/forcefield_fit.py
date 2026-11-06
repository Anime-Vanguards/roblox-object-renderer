"""ForceField fits on the forcefield-* calibration experiments (notes/render.md "ForceField").

Model: Studio blends ForceField like other transparency in its gamma-2 space,
sqrt(result) = a sqrt(s) + (1 - a) sqrt(background), unlit, with s = K c² (c the part colour in
0..1) and a = min(1, (1 - Transparency) (A0 + (1 - A0) max(rim, contact)^P)), where
rim = saturate(1 - n.v / EDGE) and contact = saturate(1 - gap / REACH) (gap: view depth from the
ForceField to the opaque surface behind it, studs).

    python tools/analysis/forcefield_fit.py colour   # K where the rim saturates the alpha
    python tools/analysis/forcefield_fit.py blend    # single-layer alpha over six backgrounds
    python tools/analysis/forcefield_fit.py rim      # the rim against n.v (turned quads)
    python tools/analysis/forcefield_fit.py contact  # the contact glow against the gap
    python tools/analysis/forcefield_fit.py time     # a TextureID's band: lit texel value over time

The quad experiments use Studio's CompositQuad, which Studio draws with the mesh origin (a corner)
at the part position; the pixel mappings here follow that placement.
"""

import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares

from tone import display, radiance

ROOT = "out/calibrate"
K, A0, EDGE, REACH, P = 1.93, 0.052, 0.42, 0.30, 1.7


class Run:
	"""A calibration run: its images, probes and Studio camera (rays, projection)."""

	def __init__(self, experiment, label):
		self.dir = os.path.join(ROOT, experiment, label)
		meta = next(r for r in json.load(open(os.path.join(ROOT, experiment, "samples.json"))) if r["label"] == label)
		self.samples = {s["probe"]: s for s in meta["samples"]}
		c = meta["camera"]
		self.eye = np.array(c[:3])
		R = np.array(c[3:]).reshape(3, 3)
		self.right, self.up, self.look = R[:, 0], R[:, 1], -R[:, 2]
		self.fov = meta["fov"]
		self.ev = meta["environment"]["exposureCompensation"]
		self.studio = self.image("studio.png")
		self.H, self.W = self.studio.shape[:2]

	def image(self, name):
		return np.asarray(Image.open(os.path.join(self.dir, name)).convert("RGB")).astype(float)

	def frames(self):
		times = json.load(open(os.path.join(self.dir, "frames.json")))
		return times, [self.image(f"frame{k}.png") for k in range(len(times))]

	def project(self, p):
		d = np.asarray(p, float) - self.eye
		t = np.tan(np.radians(self.fov) / 2)
		z = d @ self.look
		return np.array([((d @ self.right) / (z * t * self.W / self.H) * 0.5 + 0.5) * self.W, (0.5 - (d @ self.up) / (z * t) * 0.5) * self.H])

	def rays(self, xs, ys):
		t = np.tan(np.radians(self.fov) / 2)
		nx = (xs + 0.5) / self.W * 2 - 1
		ny = 1 - (ys + 0.5) / self.H * 2
		d = self.look[None, :] + self.right[None, :] * (nx * t * self.W / self.H)[:, None] + self.up[None, :] * (ny * t)[:, None]
		return d / np.linalg.norm(d, axis=1, keepdims=True)


def rotation(tilt=0.0, yaw=0.0):
	"""Studio CompositQuad orientation: turned to face -Z, then by `yaw` about Y, tilted by `tilt` about X."""
	cy, sy = np.cos(np.pi + yaw), np.sin(np.pi + yaw)
	ct, st = np.cos(-tilt), np.sin(-tilt)
	return np.array([[1, 0, 0], [0, ct, -st], [0, st, ct]]) @ np.array([[cy, 0, sy], [0, 1, 0], [-sy, 0, cy]])


def quad_pixels(run, corner, R, width, length, keep=(0.12, 0.88, 0.0, 0.95)):
	"""Pixels on a quad spanning corner + R (u width, v length, 0): ys, xs, rays, hit points, normal."""
	corner = np.asarray(corner, float)
	n = R @ np.array([0, 0, 1.0])
	p = np.array([run.project(corner + R @ np.array([u, v, 0])) for u in (0, width) for v in (0, length)])
	x0, y0 = np.maximum(np.floor(p.min(axis=0)).astype(int), 0)
	x1, y1 = np.minimum(np.ceil(p.max(axis=0)).astype(int), [run.W, run.H])
	if x1 <= x0 or y1 <= y0:
		return None
	ys, xs = (a.ravel() for a in np.mgrid[y0:y1, x0:x1])
	d = run.rays(xs, ys)
	hit = run.eye[None, :] + d * (((corner - run.eye) @ n) / (d @ n))[:, None]
	local = (hit - corner) @ R
	inside = (local[:, 0] > keep[0] * width) & (local[:, 0] < keep[1] * width) & (local[:, 1] > keep[2] * length) & (local[:, 1] < keep[3] * length)
	return ys[inside], xs[inside], d[inside], hit[inside], n


def alpha(ndv, gap, transparency):
	glow = np.maximum(np.clip(1 - ndv / EDGE, 0, 1), np.clip(1 - gap / REACH, 0, 1)) ** P
	return np.minimum(1, (1 - transparency) * (A0 + (1 - A0) * glow))


def colour():
	"""Where the rim saturates the alpha (Transparency -1, -2), the display reads the radiance K c²."""
	for label in ("dark", "dark-ev-2"):
		run = Run("forcefield-basics", label)
		for name in ("ball_white_t-2_black", "ball_white_t-1_black", "ball_sage_t-2_black", "ball_sage_t-1_black"):
			s = run.samples[name]
			center, radius = np.array(s["inputs"]["center"]), s["inputs"]["size"] / 2
			p = run.project(center)
			r = np.linalg.norm(run.project(center + run.right * radius) - p)
			ys, xs = (a.ravel() for a in np.mgrid[int(p[1] - r) : int(p[1] + r), int(p[0] - r) : int(p[0] + r)])
			dist = np.hypot(xs + 0.5 - p[0], ys + 0.5 - p[1]) / r
			ring = run.studio[ys[(dist > 0.9) & (dist < 0.98)], xs[(dist > 0.9) & (dist < 0.98)]]
			peak = np.percentile(ring, 98, axis=0)
			c2 = (np.array(s["inputs"]["color"]) / 255) ** 2
			low, high = radiance(peak - 0.5) / 2 ** run.ev / c2, radiance(peak + 0.5) / 2 ** run.ev / c2
			print(f"{label:9s} {name:24s} peak {peak.round(0)}  K {low.round(3)} .. {high.round(3)}")


def blend():
	"""Single quads across six backgrounds of known radiance: the gamma-2 blend and its alpha."""
	data = []
	for label in ("flat-ev0", "flat-ev-1"):
		run = Run("forcefield-blend", label)
		for name, s in run.samples.items():
			if name.startswith("bg"):
				continue
			inputs = s["inputs"]
			for ch in range(3):
				if s["studio"][ch] < 253:
					data.append((name.rsplit("_", 1)[0], inputs["transparency"], inputs["background"], inputs["color"][ch] / 255, run.ev, s["studio"][ch]))

	def predict(a0, rows):
		_, t, b, c, ev, _ = zip(*rows)
		a = np.minimum(1, (1 - np.array(t)) * a0)
		return display((a * np.sqrt(K) * np.array(c) + (1 - a) * np.sqrt(b)) ** 2 * 2 ** np.array(ev))

	for row in sorted({d[0] for d in data}):
		rows = [d for d in data if d[0] == row]
		fit = least_squares(lambda p: predict(p[0], rows) - np.array([r[5] for r in rows]), [0.05])
		t = rows[0][1]
		print(f"{row:12s} T={t:5.2f}  alpha/(1-T) {fit.x[0]:.4f}  mean abs {np.abs(fit.fun).mean():.2f}")
	fit = least_squares(lambda p: predict(p[0], data) - np.array([r[5] for r in data]), [0.05])
	print(f"all rows: alpha = {fit.x[0]:.4f} (1 - T), mean abs {np.abs(fit.fun).mean():.2f}, max {np.abs(fit.fun).max():.2f} (display levels)")


def rim():
	"""White quads turned so the camera sees them at n.v 0.03-0.5, over black: the rim."""
	rows = []
	runs = [Run("forcefield-angles", label) for label in ("ev0", "ev-1", "ev-2")]
	for name, s in runs[0].samples.items():
		inputs = s["inputs"]
		R = rotation(yaw=np.radians(inputs["yaw"]))
		corner = np.array(inputs["center"]) - R @ np.array([2.0, 2.0, 0])
		found = quad_pixels(runs[0], corner, R, 4.0, 4.0, keep=(0.2, 0.8, 0.2, 0.8))
		if found is None:
			continue
		ys, xs, d, _, n = found
		pick = np.arange(0, len(ys), max(1, len(ys) // 400))
		for run in runs:
			for ndv, value in zip(np.abs(d[pick] @ n), run.studio[ys[pick], xs[pick], 1]):
				rows.append((ndv, inputs["transparency"], run.ev, value))
	ndv, t, ev, value = np.array(rows).T
	ok = value < 250

	def residuals(p):
		a0, edge, power = p
		a = np.minimum(1, (1 - t) * (a0 + (1 - a0) * np.clip(1 - ndv / edge, 0, 1) ** power))
		return (display((a * np.sqrt(K)) ** 2 * 2**ev) - value)[ok]

	fit = least_squares(residuals, [0.05, 0.4, 1.5], loss="soft_l1", f_scale=3.0)
	print("A0 %.4f  EDGE %.3f  P %.3f" % tuple(fit.x), f"  mean abs {np.abs(residuals(fit.x)).mean():.2f} display levels over {ok.sum()} pixels")


def contact():
	"""Quads tilted off a backdrop (gap 0-1 stud), head-on and from 45°: the contact glow."""
	rows = []
	for label in ("ev0", "ev-1", "side"):
		run = Run("forcefield-depth", label)
		for s in run.samples.values():
			inputs = s["inputs"]
			found = quad_pixels(run, inputs["corner"], rotation(tilt=inputs["tilt"]), inputs["width"], inputs["length"])
			if found is None:
				continue
			ys, xs, d, hit, n = found
			gap = (-hit[:, 2] / d[:, 2]) * (d @ run.look)
			pick = np.arange(0, len(ys), max(1, len(ys) // 1500))
			background = 0.0 if inputs["backdrop"] == 0 else 25 * (100 / 255) ** 4
			for ch in range(3):
				if ch != 1 and inputs["color"][0] == 255:
					continue
				for g, ndv, value in zip(gap[pick], np.abs(d[pick] @ n), run.studio[ys[pick], xs[pick], ch]):
					rows.append((g, ndv, inputs["transparency"], background, inputs["color"][ch] / 255, run.ev, value))
	gap, ndv, t, bg, c, ev, value = np.array(rows).T
	ok = (value < 252) & (gap > 0.005)

	def residuals(p):
		strength, reach, power = p
		glow = np.maximum(np.clip(1 - ndv / EDGE, 0, 1) ** P, strength * np.clip(1 - gap / reach, 0, 1) ** power)
		a = np.minimum(1, (1 - t) * (A0 + (1 - A0) * glow))
		return (display((a * np.sqrt(K) * c + (1 - a) * np.sqrt(bg)) ** 2 * 2**ev) - value)[ok]

	fit = least_squares(residuals, [0.8, 0.3, 1.5], loss="soft_l1", f_scale=3.0)
	print("strength %.3f  REACH %.3f  power %.3f" % tuple(fit.x), f"  mean abs {np.abs(residuals(fit.x)).mean():.2f} display levels over {ok.sum()} pixels")
	# both glows at once: steep quads near the contact line
	rows = []
	for label in ("ev0", "ev-1"):
		run = Run("forcefield-contact", label)
		for s in run.samples.values():
			inputs = s["inputs"]
			found = quad_pixels(run, inputs["corner"], rotation(tilt=inputs["tilt"]), inputs["width"], inputs["length"], keep=(0.1, 0.9, 0.0, 0.97))
			ys, xs, d, hit, n = found
			for g, nv, value in zip((-hit[:, 2] / d[:, 2]) * (d @ run.look), np.abs(d @ n), run.studio[ys, xs, 1]):
				rows.append((g, nv, inputs["transparency"], run.ev, value))
	gap, ndv, t, ev, value = np.array(rows).T
	r, c = np.clip(1 - ndv / EDGE, 0, 1) ** P, np.clip(1 - gap / REACH, 0, 1) ** P
	both = (r > 0.05) & (c > 0.05) & (value < 250)
	for mode, glow in (("max", np.maximum(r, c)), ("sum", np.minimum(r + c, 1))):
		a = np.minimum(1, (1 - t) * (A0 + (1 - A0) * glow))
		err = display((a * np.sqrt(K)) ** 2 * 2**ev) - value
		print(f"rim and contact combined by {mode}: mean signed {err[both].mean():+.2f} over {both.sum()} pixels with both")


def time():
	"""The glow-texture quad of forcefield-time: the median texel value of its lit pixels per frame."""
	run = Run("forcefield-time", "dark-ev2")
	times, frames = run.frames()
	path = os.path.join(os.environ.get("LOCALAPPDATA", ""), "Roblox", "Versions")
	versions = [v for v in os.listdir(path) if os.path.exists(os.path.join(path, v, "RobloxStudioBeta.exe"))]
	texture = np.asarray(Image.open(os.path.join(path, versions[0], "content", "textures", "glow.png")).convert("RGBA")).astype(float) / 255
	value = texture[..., :3].mean(axis=2)
	s = run.samples["glow"]
	center, size = np.array(s["inputs"]["center"]), s["inputs"]["size"]
	R = rotation()
	ys, xs, d, hit, n = quad_pixels(run, center - R @ np.array([size / 2, size / 2, 0]), R, size, size, keep=(0.03, 0.97, 0.03, 0.97))
	local = (hit - (center - R @ np.array([size / 2, size / 2, 0]))) @ R
	h, w = value.shape
	texel = value[np.clip(((1 - local[:, 1] / size) * h).astype(int), 0, h - 1), np.clip((local[:, 0] / size * w).astype(int), 0, w - 1)]
	rest = np.median([f[ys, xs, 1] for f in frames])
	for t, frame in zip(times, frames):
		lit = frame[ys, xs, 1] > rest + 25
		print(f"t {t:6.2f}s  lit {lit.sum():6d}  texel value {np.median(texel[lit]) if lit.sum() > 3 else float('nan'):.3f}")


if __name__ == "__main__":
	{"colour": colour, "blend": blend, "rim": rim, "contact": contact, "time": time}[sys.argv[1]]()
