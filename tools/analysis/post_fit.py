"""The post effects from their calibration experiments (notes/render.md "Post effects").

    python tools/analysis/post_fit.py color      # ColorCorrection: luma weights, order of operations (post-color)
    python tools/analysis/post_fit.py blur       # BlurEffect: sigma per Size on the shown values (post-blur)
    python tools/analysis/post_fit.py sunrays    # SunRays: profile per Spread, colour by elevation (post-sunrays-*)
    python tools/analysis/post_fit.py shade      # SunRays: the path weights that shade the rays (post-sunrays, -sky)

DepthOfField's blur by circle of confusion was read off per-row Gaussian fits of post-depth's
checker rows in radiance (sigma 6.6 px at c = 1, smoothstep(0.08, 1, c) · min(1, 2 FarIntensity)).
"""

import itertools
import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.ndimage import gaussian_filter
from scipy.optimize import minimize, minimize_scalar

sys.path.insert(0, os.path.dirname(__file__))
from atmosphere_fit import ROOT, directions, image, radiance, runs, sun_direction  # noqa: E402

SPREADS = {
	0.0: [("post-sunrays-sky", "s0", 0.05)],
	0.05: [("post-sunrays-time", "s0.05", 0.01)],
	0.1: [("post-sunrays-time", "s0.1", 0.01), ("post-sunrays-sky", "s0.1", 0.05), ("post-sunrays-sky", "i0.2-s0.1", 0.2)],
	0.15: [("post-sunrays-time", "s0.15", 0.01)],
	0.25: [("post-sunrays-time", "s0.25", 0.02), ("post-sunrays-sky", "s0.25", 0.05)],
	0.5: [("post-sunrays-sky", "s0.5", 0.05)],
	0.75: [("post-sunrays-time", "s0.75", 0.05)],
	1.0: [("post-sunrays-time", "fov90", 0.05), ("post-sunrays-sky", "hidden", 0.05), ("post-sunrays-sky", "i0.2-s1", 0.2)],
}
ANGLES = [0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 25, 30, 35, 40, 50, 60, 70, 80, 90, 120, 180]
# red per unit Intensity at Spread 1, 5-8 degrees, at 16:30 (the profile's unit)
RED = 7.61


def color():
	def values(label):
		return {sample["probe"]: np.array(sample["studio"]) / 255.0 for sample in runs("post-color")[label]["samples"]}

	base, grey = values("none"), values("saturation-1")
	colours = [probe for probe in base if probe.startswith("color")]
	weights, *_ = np.linalg.lstsq(np.array([base[p] for p in colours]), np.array([grey[p][0] for p in colours]), rcond=None)
	print("luma weights", weights.round(4), "sum", weights.sum().round(4))
	weights = weights / weights.sum()
	tint = np.array([233 / 255, 247 / 255, 1.0])
	steps = {
		"c": lambda c: (c - 0.5) * 1.3 + 0.5,
		"s": lambda c: (c @ weights) + (c - (c @ weights)) * 0.85,
		"t": lambda c: c * tint,
	}
	lobby = values("lobby")
	for order in itertools.permutations("cst"):
		errors = []
		for probe, value in base.items():
			c = value.copy()
			for step in order:
				c = steps[step](c)
			errors.append(np.abs(np.clip(c, 0, 1) - lobby[probe]).max() * 255)
		print(f"lobby as {''.join(order)}: mean {np.mean(errors):.2f}  max {np.max(errors):.2f} of 255")


def blur():
	sharp = image("post-blur", "none")[..., 0]
	probes = {sample["probe"]: sample["pixel"] for sample in runs("post-blur")["none"]["samples"]}
	regions = {"square": (probes["square"], 150), "line": (probes["line"], 120), "dot": (probes["dot"], 90)}
	for label in ["size2", "size4", "size8", "size16", "size24", "size40", "size56"]:
		blurred = image("post-blur", label)[..., 0]
		cells = []
		for name, (centre, half) in regions.items():
			x, y = int(centre[0]), int(centre[1])
			window = (slice(y - half, y + half), slice(x - half, x + half))
			source = sharp[y - half - 80 : y + half + 80, x - half - 80 : x + half + 80]
			inner = (slice(80, 80 + 2 * half), slice(80, 80 + 2 * half))
			best = minimize_scalar(lambda sigma: np.abs(gaussian_filter(source, sigma)[inner] - blurred[window]).mean(), bounds=(0.1, 60), method="bounded")
			cells.append(f"{name} sigma {best.x:5.2f} (energy {blurred[window].sum() / sharp[window].sum():.2f})")
		print(f"{label:7s} " + " | ".join(cells))


def ring_profile(experiment, label, intensity):
	"""What the rays add (run minus its '-none' run), red per unit Intensity over RED, by angle."""
	run = runs(experiment)[label]
	before, after = radiance(image(experiment, f"{label}-none")), radiance(image(experiment, label))
	h, w, _ = after.shape
	environment = run["environment"]
	sun = sun_direction(environment["clockTime"], environment.get("latitude", 0.0))
	theta = np.degrees(np.arccos(np.clip(directions(run, w, h) @ sun, -1, 1)))
	valid = (after.max(axis=2) < 4.0) & (before.max(axis=2) > 0.004)
	added = after - before
	values, amounts = [], []
	for angle in np.arange(0.0, 90.0, 1.0):
		ring = valid & (np.abs(theta - angle) < 0.5)
		amount = added[ring][:, 0].mean() if ring.sum() > 30 else np.nan
		values.append(amount / intensity / RED)
		amounts.append(amount)
	return np.array(values), np.array(amounts)


def sunrays():
	steps = np.arange(0.0, 90.0, 1.0)
	print("profile (render::post::SUN_RAYS_PROFILE): per Spread, at", ANGLES)
	for spread, sources in SPREADS.items():
		measured = [ring_profile(*source) for source in sources]
		merged = np.full(len(steps), np.nan)
		for index in range(len(steps)):
			# runs adding at least 0.1 (precise in 8 bits) without saturating, else the one adding most
			good = [values[index] for values, amounts in measured if np.isfinite(values[index]) and amounts[index] >= 0.1]
			candidates = [(amounts[index], values[index]) for values, amounts in measured if np.isfinite(values[index])]
			merged[index] = np.mean(good) if good else (max(candidates)[1] if candidates else np.nan)
		known = np.isfinite(merged)
		merged[: np.argmax(known)] = merged[np.argmax(known)]
		known = np.isfinite(merged)
		last = len(known) - 1 - np.argmax(known[::-1])
		tail = np.arange(last - 10, last + 1)
		slope = np.polyfit(np.log(steps[tail]), np.log(merged[tail]), 1)[0]
		table = [np.interp(a, steps[known], merged[known]) if a <= steps[last] else merged[last] * (a / steps[last]) ** slope for a in ANGLES]
		print(f"  {spread}: " + ", ".join(f"{v:.4g}" for v in table))
	print("colour per unit Intensity at 5-8 degrees (post-sunrays-clock), against the sun's elevation:")
	for label, run in sorted(runs("post-sunrays-clock").items(), key=lambda item: item[1]["environment"]["clockTime"]):
		if label.endswith("-none"):
			continue
		environment = run["environment"]
		sun = sun_direction(environment["clockTime"], environment.get("latitude", 0.0))
		before, after = radiance(image("post-sunrays-clock", f"{label}-none")), radiance(image("post-sunrays-clock", label))
		h, w, _ = after.shape
		theta = np.degrees(np.arccos(np.clip(directions(run, w, h) @ sun, -1, 1)))
		near = (theta > 5) & (theta < 8)
		print(f"  {label:12s} elevation {np.degrees(np.arcsin(sun[1])):5.1f}: {np.round((after - before)[near].mean(axis=0) / 0.05, 2)}")


def shade():
	"""The share of the path weight before f (from the pixel), from the wall below the sun (blocked
	from the pixel to the wall's top, post-sunrays-sky) and the slab over it (blocked from 140 px to
	the sun, post-sunrays); w(f) = (f / f1)^k below f1, 1 beyond."""
	wall = [(10 / 300, 0.01), (70 / 360, 0.04), (100 / 390, 0.07), (130 / 420, 0.10), (160 / 450, 0.13), (190 / 480, 0.16), (220 / 510, 0.20), (250 / 540, 0.24)]
	streak = [(20 / 160, 0.02), (60 / 200, 0.14), (90 / 230, 0.25), (120 / 260, 0.35), (160 / 300, 0.47), (210 / 350, 0.58), (260 / 400, 0.64), (310 / 450, 0.69), (360 / 500, 0.73), (400 / 540, 0.74)]

	def cdf(f, f1, k):
		x = np.linspace(0.0, 1.0, 2001)
		c = np.cumsum(np.where(x < f1, (x / f1) ** k, 1.0))
		return np.interp(f, x, c / c[-1])

	best = minimize(lambda p: sum((cdf(f, *p) - v) ** 2 for f, v in wall + streak) if 0.01 < p[0] < 1 and p[1] >= 0 else 1e9, [0.5, 2.0], method="Nelder-Mead")
	print(f"f1 {best.x[0]:.3f}  k {best.x[1]:.2f}  rms {np.sqrt(best.fun / len(wall + streak)):.3f}")


if __name__ == "__main__":
	{"color": color, "blur": blur, "sunrays": sunrays, "shade": shade}[sys.argv[1]]()
