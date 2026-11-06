"""Shadow edge profiles at full resolution, Studio vs ror, for the shadow experiments whose probes
describe straight edges (crates/ror/src/studio/calibrate/shadows.rs: inputs edge, point, normal,
mapScale, half), and for shadow-contact's block (built in here).

Each edge is sampled across, bilinearly in the PNG, every 0.005 studs along the lit normal on the
receiver; display values go back to the tone curve's input (a square, then the inverse of the
curve) and are normalised between the dark and lit plateaus (visibility 0..1). Reported per edge:
the 10-90 % width in receiver studs and in shadow-map-plane studs (x mapScale), the 50 % point
(+ = towards the lit side) and the 10-50 / 50-90 halves (asymmetry). `--fit` also fits the profile's
shape, Studio's filter: a Gaussian's CDF of sigma s (map-plane studs) centred at c, over-darkened
(shares up to a black, the rest rescaled), with its residual against a plain Gaussian's.

    python tools/analysis/shadow_profiles.py out/calibrate/shadow-cascades [run ...] [--fit] [--profiles out.json]
"""

import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares
from scipy.special import erf

from tone import radiance

STEP = 0.005


def projector(run, width, height):
	c = run["camera"]
	eye = np.array(c[0:3])
	rotation = np.array([[c[3], c[4], c[5]], [c[6], c[7], c[8]], [c[9], c[10], c[11]]])
	right, up, back = rotation[:, 0], rotation[:, 1], rotation[:, 2]
	tan_y = np.tan(np.radians(run["fov"]) / 2)
	tan_x = tan_y * width / height

	def project(points):
		p = np.asarray(points, dtype=float) - eye
		z = -(p @ back)
		x = (p @ right) / (z * tan_x)
		y = (p @ up) / (z * tan_y)
		return np.stack([(x * 0.5 + 0.5) * width, (0.5 - y * 0.5) * height], axis=-1)

	return project


def bilinear(image, pixels):
	x = pixels[..., 0] - 0.5
	y = pixels[..., 1] - 0.5
	x0 = np.floor(x).astype(int)
	y0 = np.floor(y).astype(int)
	fx = (x - x0)[..., None]
	fy = (y - y0)[..., None]
	h, w = image.shape[:2]
	x0c, x1c = np.clip(x0, 0, w - 1), np.clip(x0 + 1, 0, w - 1)
	y0c, y1c = np.clip(y0, 0, h - 1), np.clip(y0 + 1, 0, h - 1)
	return image[y0c, x0c] * (1 - fx) * (1 - fy) + image[y0c, x1c] * fx * (1 - fy) + image[y1c, x0c] * (1 - fx) * fy + image[y1c, x1c] * fx * fy


def load_image(path):
	return np.asarray(Image.open(path).convert("RGB"), dtype=float)


def edge_values(image, project, point, normal, half):
	"""Radiance across an edge (red channel) at offsets -half..half along the lit normal."""
	offsets = np.arange(-half, half + STEP / 2, STEP)
	points = np.asarray(point)[None, :] + offsets[:, None] * np.asarray(normal)[None, :]
	return offsets, radiance(bilinear(image, project(points))[:, 0])


def normalise(offsets, values, half, plateau=0.2):
	"""Visibility: 0 at the dark plateau (the outer `plateau` of the shadow side), 1 at the lit one."""
	dark = np.median(values[offsets < -half * (1 - plateau)])
	lit = np.median(values[offsets > half * (1 - plateau)])
	return (values - dark) / max(lit - dark, 1e-6), lit, dark


def stats(offsets, visibility):
	"""(10-90 % width, 50 % point, 10-50 half, 50-90 half) of a rising profile."""
	v = np.maximum.accumulate(np.clip(visibility, 0, 1))
	w10, w50, w90 = (float(np.interp(level, v, offsets)) for level in (0.1, 0.5, 0.9))
	return w90 - w10, w50, w50 - w10, w90 - w50


def darkened_gaussian(params, offsets):
	"""Studio's edge profile: a Gaussian's CDF (centre, sigma) over-darkened by `darken`."""
	centre, sigma, darken = params
	cdf = 0.5 * (1 + erf((offsets - centre) / (np.sqrt(2) * max(sigma, 1e-6))))
	return np.clip((cdf - darken) / (1 - darken), 0, 1)


def fit_shape(offsets, visibility, scale):
	"""(sigma, darkening, centre, residual, a plain Gaussian's residual) over the central 2 studs, in
	map-plane studs."""
	window = np.abs(offsets) < 1.0
	t, v = offsets[window], np.clip(visibility[window], -0.1, 1.1)
	shaped = least_squares(lambda p: darkened_gaussian(p, t) - v, [0.0, 0.1, 0.15], bounds=([-1, 1e-3, 0], [1, 2, 0.6]))
	plain = least_squares(lambda p: darkened_gaussian([p[0], p[1], 0.0], t) - v, [0.0, 0.1], bounds=([-1, 1e-3], [1, 2]))
	centre, sigma, darken = shaped.x
	return sigma * scale, darken, centre, float(np.sqrt(np.mean(shaped.fun ** 2))), float(np.sqrt(np.mean(plain.fun ** 2)))


def edges_of(run):
	"""The edge probes of a run: (label, point, normal, map scale, half extent, inputs)."""
	result = []
	for sample in run["samples"]:
		inputs = sample["inputs"]
		if isinstance(inputs, dict) and "edge" in inputs:
			result.append((inputs["edge"], np.array(inputs["point"]), np.array(inputs["normal"]), inputs.get("mapScale", 1.0), inputs.get("half", 1.0), inputs))
	return result


def contact_edges():
	"""shadow-contact's block (4 studs, on the floor) under the default sun: its vertical edges and
	top edges cast on the floor."""
	sun = np.array([-0.5583, 0.7276, -0.3987])
	t = -sun[[0, 2]] / sun[1]
	edges = []
	for y in (0.5, 1.0, 2.0, 3.0):
		edges.append((f"vert-a-y{y}", np.array([-2 + t[0] * y, 0.001, 2 + t[1] * y]), np.array([-0.5812, 0, 0.8138]), 1.0, 1.0, {"distance": y / sun[1]}))
		edges.append((f"vert-b-y{y}", np.array([2 + t[0] * y, 0.001, -2 + t[1] * y]), np.array([0.5812, 0, -0.8138]), 1.0, 1.0, {"distance": y / sun[1]}))
	for z in (1.0, 2.0, 3.0):
		edges.append((f"top-x-z{z}", np.array([2 + 4 * t[0], 0.001, z + 4 * t[1] - 2]), np.array([1.0, 0, 0]), 0.7934, 1.0, {"distance": 4 / sun[1]}))
	for x in (2.0, 3.0):
		edges.append((f"top-z-x{x}", np.array([x + 4 * t[0] - 2, 0.001, 2 + 4 * t[1]]), np.array([0, 0, 1.0]), 0.8770, 1.0, {"distance": 4 / sun[1]}))
	return edges


def measure(directory, labels=None, fit=False):
	"""Every edge of every run: a list of dicts with the Studio and ror profiles and their stats."""
	runs = json.load(open(os.path.join(directory, "samples.json")))
	contact = os.path.basename(os.path.normpath(directory)) == "shadow-contact"
	results = []
	for run in runs:
		if labels and run["label"] not in labels:
			continue
		studio = load_image(os.path.join(directory, run["label"], "studio.png"))
		ror = load_image(os.path.join(directory, run["label"], "ror.png"))
		project = projector(run, studio.shape[1], studio.shape[0])
		eye = np.array(run["camera"][0:3])
		for label, point, normal, scale, half, inputs in contact_edges() if contact else edges_of(run):
			entry = {"run": run["label"], "edge": label, "mapScale": scale, "cameraDistance": float(np.linalg.norm(point - eye)), "inputs": inputs}
			for name, image in (("studio", studio), ("ror", ror)):
				offsets, values = edge_values(image, project, point, normal, half)
				visibility, lit, dark = normalise(offsets, values, half)
				width, centre, low, high = stats(offsets, visibility)
				entry[name] = {"width": width, "mapWidth": width * scale, "centre": centre, "low": low, "high": high, "lit": float(lit), "dark": float(dark), "profile": visibility.tolist()}
				if fit:
					entry[name]["shape"] = dict(zip(("sigma", "darken", "centre", "residual", "gaussianResidual"), fit_shape(offsets, visibility, scale)))
			entry["offsets"] = offsets.tolist()
			pixels = project(np.stack([point - normal * 0.5, point + normal * 0.5]))
			entry["pixelsPerStud"] = float(np.linalg.norm(pixels[1] - pixels[0]))
			results.append(entry)
	return results


def main():
	args = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
	directory = args[0] if args else "out/calibrate/shadow-cascades"
	profiles = sys.argv[sys.argv.index("--profiles") + 1] if "--profiles" in sys.argv else None
	if profiles in args:
		args.remove(profiles)
	fit = "--fit" in sys.argv
	results = measure(directory, args[1:] or None, fit)
	for entry in results:
		s, r = entry["studio"], entry["ror"]
		line = (
			f"{entry['run']:12} {entry['edge']:14} cam {entry['cameraDistance']:6.1f} px/stud {entry['pixelsPerStud']:5.0f}  "
			f"map width studio {s['mapWidth']:.3f} ror {r['mapWidth']:.3f}  centre {s['centre']:+.3f} / {r['centre']:+.3f}  "
			f"halves {s['low']:.3f}/{s['high']:.3f} vs {r['low']:.3f}/{r['high']:.3f}"
		)
		if fit:
			for name in ("studio", "ror"):
				shape = entry[name]["shape"]
				line += f"  {name} sigma {shape['sigma']:.3f} darken {shape['darken']:.2f} residual {shape['residual']:.4f} (Gaussian {shape['gaussianResidual']:.4f})"
		print(line)
	if profiles:
		with open(profiles, "w") as file:
			json.dump(results, file)


if __name__ == "__main__":
	main()
