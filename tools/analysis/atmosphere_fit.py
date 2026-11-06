"""The sky, the Atmosphere and the fog from their calibration experiments (notes/render.md "Sky
elements", "Atmosphere", "Fog"). Display values are read back through the tone curve (with its
shoulder) and the run's exposure.

    python tools/analysis/atmosphere_fit.py haze      # (τ0, b, floor) per Haze (atmosphere-haze)
    python tools/analysis/atmosphere_fit.py glare     # glare amplitude and lobe exponent per Decay (atmosphere-glare)
    python tools/analysis/atmosphere_fit.py distance  # extinction per Density (atmosphere-distance)
    python tools/analysis/atmosphere_fit.py fog       # the fog curve (fog)
    python tools/analysis/atmosphere_fit.py tint      # the time of day's tint table (sky-time, sky-elements dim)
    python tools/analysis/atmosphere_fit.py sky       # the sun's and moon's weights (sky-sun)
    python tools/analysis/atmosphere_fit.py shift     # ColorShift_Top's glare tint (atmosphere-shift, lobby-sky)
"""

import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares

from tone import radiance

ROOT = "out/calibrate"
SKY_RADIANCE = 1.935


def runs(experiment):
	return {run["label"]: run for run in json.load(open(os.path.join(ROOT, experiment, "samples.json")))}


def image(experiment, label, side="studio"):
	return np.asarray(Image.open(os.path.join(ROOT, experiment, label, f"{side}.png")).convert("RGB")).astype(float)


def directions(run, width, height):
	"""Each pixel's view direction from the run's camera CFrame components."""
	c = run["camera"]
	rotation = np.array([[c[3], c[4], c[5]], [c[6], c[7], c[8]], [c[9], c[10], c[11]]])
	tan = np.tan(np.radians(run["fov"]) / 2)
	ys, xs = np.mgrid[0:height, 0:width]
	local = np.stack([((xs + 0.5) / width * 2 - 1) * tan * width / height, (1 - (ys + 0.5) / height * 2) * tan, -np.ones((height, width))], axis=-1)
	world = local @ rotation.T
	return world / np.linalg.norm(world, axis=-1, keepdims=True)


def sun_direction(clock, latitude=0.0):
	h = (clock - 12) / 24 * 2 * np.pi
	t = np.radians(latitude - 23.5)
	return np.array([-np.sin(h) * np.cos(t), np.cos(h) * np.cos(t), np.sin(t)])


def setting(experiment, name, step=3):
	"""Directions and radiance (exposure undone) over every view of a setting (labels name-view)."""
	ds, xs = [], []
	environment = None
	for label, run in runs(experiment).items():
		if label.rsplit("-", 1)[0] != name:
			continue
		environment = run["environment"]
		pixels = image(experiment, label)
		h, w, _ = pixels.shape
		ds.append(directions(run, w, h)[::step, ::step].reshape(-1, 3))
		xs.append(radiance(pixels[::step, ::step].reshape(-1, 3)) / 2 ** environment["exposureCompensation"])
	return np.concatenate(ds), np.concatenate(xs), environment


def haze():
	"""White skybox through an orange Color: blue keeps only the skybox's share, 1 - h."""
	for name in ["haze0.05", "haze0.5", "haze1", "haze2", "haze2.2", "haze2.6", "haze3", "haze4", "haze5", "haze10"]:
		d, x, _ = setting("atmosphere-haze", name)
		up = d[:, 1] > np.sin(np.radians(0.3))
		s, keep = d[up, 1], x[up, 2] / SKY_RADIANCE
		fit = least_squares(lambda p: p[2] * np.exp(-p[0] * s ** -p[1]) - keep, [0.03, 1.0, 1.0], bounds=([1e-5, 0.1, 0.5], [10, 12, 1.1]))
		print(f"{name:8s} tau0 {fit.x[0]:.4f}  b {fit.x[1]:.3f}  floor {fit.x[2]:.3f}  rms {np.sqrt((fit.fun ** 2).mean()):.4f}")


def glare():
	"""Haze 10 over a black skybox: per channel A ((1 + cos θ) / 2)^n below 35 degrees."""
	for name in ["w-w-g1", "w-a-g1", "w-b-g1", "w-k-g1", "w-a-g4", "w-a-g0.25", "lobby-h10"]:
		d, x, environment = setting("atmosphere-glare", name)
		atmosphere = environment["atmosphere"]
		sun = sun_direction(environment["clockTime"], environment["latitude"])
		low = np.degrees(np.arcsin(d[:, 1])) < 35
		u = (1 + d[low] @ sun) / 2
		base = np.array(atmosphere["color"]) ** 2 * SKY_RADIANCE * (np.array([45, 47, 49]) / 255.0) ** 2
		for channel in range(3):
			fit = least_squares(lambda p: p[0] * u ** p[1] - (x[low, channel] - base[channel]), [2.0, 1.0], bounds=([0, 0], [100, 100]))
			decay = atmosphere["decay"][channel]
			model = 0.705 * (np.exp(3.785 * (1 - decay)) - 1)
			print(f"{name:10s} ch {channel} decay {decay:.3f}: amplitude / (Brightness * Glare) {fit.x[0] / (environment['brightness'] * atmosphere['glare']):.3f}  n {fit.x[1]:.3f} (model {model:.3f})")


def distance():
	"""Black and grey Neon tile pairs: T = (grey - black) / 1.635, fit exp(-σ d) per run."""
	for label, run in runs("atmosphere-distance").items():
		atmosphere = run["environment"]["atmosphere"]
		pairs = {}
		for sample in run["samples"]:
			inputs = sample["inputs"]
			pairs.setdefault((inputs["row"], inputs["elevation"], inputs["distance"]), {})[inputs["kind"]] = np.array(sample["studio"])
		ds, ts = [], []
		for (_, _, d), pair in pairs.items():
			if "black" in pair and "gray" in pair:
				t = ((radiance(pair["gray"]) - radiance(pair["black"])) / 1.635).mean()
				if 0.03 < t < 0.97:
					ds.append(d)
					ts.append(t)
		if not ds or not atmosphere:
			continue
		ds, ts = np.array(ds), np.array(ts)
		sigma = -(np.log(ts) * ds).sum() / (ds**2).sum()
		print(f"{label:28s} density {atmosphere['density']:.3f}  sigma {sigma:.6f}  sigma / density^4 {sigma / atmosphere['density'] ** 4:.4f}")


def fog():
	"""Black tiles' fog share against u = (d - FogStart) / (FogEnd - FogStart)."""
	us, fs = [], []
	for label, run in runs("fog").items():
		# a fog colour to measure, no Atmosphere with Density in the way
		if not label.endswith("-a0") or label.startswith(("none", "black", "gray-100-100")) or "dense" in label:
			continue
		start, end = run["environment"]["fog"]
		color = SKY_RADIANCE * np.array(run["environment"]["fogColor"]) ** 2
		channel = int(np.argmax(color))
		for sample in run["samples"]:
			inputs = sample["inputs"]
			u = (inputs["distance"] - start) / (end - start)
			if inputs["kind"] == "black" and 0 < u < 1:
				us.append(u)
				fs.append(radiance(np.array(sample["studio"]))[channel] / 2 ** run["environment"]["exposureCompensation"] / color[channel])
	u, f = np.array(us), np.array(fs)
	fit = least_squares(lambda p: u * (1 + p[0] * (1 - u)) - f, [0.3])
	print(f"u (1 + a (1 - u)): a {fit.x[0]:.4f}  rms {np.sqrt((fit.fun ** 2).mean()):.4f}; linear rms {np.sqrt(((u - f) ** 2).mean()):.4f}")


def tint():
	"""√share = (1 - f) √above + f √below over every sky pixel, f = saturate(0.75 (1 - y / max(|x|, |z|)))."""
	rows = {}
	for experiment, prefix, exclude in [("sky-time", "t", np.array([0.0, -3.0, 20.0])), ("sky-elements", "dim", None)]:
		for label, run in runs(experiment).items():
			if not label.startswith(prefix):
				continue
			pixels = image(experiment, label)
			h, w, _ = pixels.shape
			d = directions(run, w, h)[::6, ::6].reshape(-1, 3)
			share = np.sqrt(radiance(pixels[::6, ::6].reshape(-1, 3)) / SKY_RADIANCE)
			keep = np.ones(len(d), dtype=bool) if exclude is None else d @ (exclude / np.linalg.norm(exclude)) < np.cos(np.radians(9))
			f = np.clip(0.75 * (1 - d[keep, 1] / np.maximum(np.abs(d[keep, 0]), np.abs(d[keep, 2]))), 0, 1)
			design = np.stack([1 - f, f], axis=1)
			solved = [np.linalg.lstsq(design, share[keep, channel], rcond=None)[0] for channel in range(3)]
			rows.setdefault(run["environment"]["clockTime"], []).append(np.array(solved).T)
	for clock in sorted(rows):
		above, below = np.mean(rows[clock], axis=0)
		print(f"({clock:.3f}, above sqrt {np.round(np.minimum(above, 1.0), 3)}, below sqrt {np.round(np.minimum(below, 1.0), 3)})")


def sky():
	"""The sun's (and moon's) centre over uniform skyboxes: sqrt(centre) - sqrt(sky) = w · texel."""
	for label, run in runs("sky-sun").items():
		environment = run["environment"]
		pixels = image("sky-sun", label)
		h, w, _ = pixels.shape
		d = directions(run, w, h)
		sun = sun_direction(environment["clockTime"], environment["latitude"])
		target = sun if sun[1] > 0 else np.array([-sun[0], -sun[1], sun[2]])
		angle = np.degrees(np.arccos(np.clip(d @ target, -1, 1)))
		x = radiance(pixels) / 2 ** environment["exposureCompensation"]
		centre = np.median(x[angle < 0.5].reshape(-1, 3), axis=0)
		sky_x = np.median(x[angle > 14].reshape(-1, 3), axis=0)
		print(f"{label:28s} centre {np.round(centre, 2)}  sky {np.round(sky_x, 3)}  sqrt(centre) - sqrt(sky) {np.round(np.sqrt(centre) - np.sqrt(sky_x), 3)}")


def shift():
	"""Glare tint under ColorShift_Top: (pink - haze light) / (black - haze light) per channel."""
	def centre(experiment, label):
		pixels = image(experiment, label)
		h, w, _ = pixels.shape
		run = runs(experiment)[label]
		return radiance(pixels[h // 2 - 60 : h // 2 + 60, w // 2 - 60 : w // 2 + 60].reshape(-1, 3).mean(0)) / 2 ** run["environment"]["exposureCompensation"]

	haze_light = np.array([0.045, 0.054, 0.0645])
	for experiment, black, tinted in [("atmosphere-shift", "glare-k-a0", ["glare-pink-a0", "glare-red-a0", "glare-white-a0"])] + [("lobby-sky", f"tint-black-{case}-k", [f"tint-black-{case}-pink"]) for case in ["unlit", "diffuse0.5", "diffuse"]]:
		base = centre(experiment, black) - haze_light
		for label in tinted:
			print(f"{experiment:17s} {label:26s} tint {np.round((centre(experiment, label) - haze_light) / base, 3)}")


if __name__ == "__main__":
	{"haze": haze, "glare": glare, "distance": distance, "fog": fog, "tint": tint, "sky": sky, "shift": shift}[sys.argv[1]]()
