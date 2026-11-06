"""Where Studio's sun shadow cascades are: the penumbra width (10-90 %, Studio and ror) every few
pixels along each edge of shadow-cascade-map, shadow-cascade-down or shadow-cascades, binned by
view depth and (straight-down runs) by floor radius from the camera's foot; the width steps where
the cascade does. Writes each run's samples to <run>/widths.json for fitting spheres.

    python tools/analysis/shadow_cascades.py out/calibrate/shadow-cascade-map [run ...]
"""

import json
import os
import sys
from collections import defaultdict

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from shadow_profiles import edge_values, load_image, normalise, projector, stats  # noqa: E402


def scan(run, studio, ror, extent=(-150.0, 700.0)):
	"""Samples along every edge probe of a run: pixel, world point, view depth, distance, widths."""
	height, width = studio.shape[:2]
	project = projector(run, width, height)
	c = run["camera"]
	eye = np.array(c[0:3])
	forward = -np.array([c[5], c[8], c[11]])
	samples = []
	for probe in run["samples"]:
		inputs = probe["inputs"]
		if not isinstance(inputs, dict) or "along" not in inputs:
			continue
		start, along, normal = np.array(inputs["point"]), np.array(inputs["along"]), np.array(inputs["normal"])
		half = inputs.get("half", 1.0)
		t = extent[0]
		while t < extent[1]:
			point = start + along * t
			pixel = project(point[None, :])[0]
			ahead = project((point + along)[None, :])[0]
			t += max(0.05, 3.0 / max(np.linalg.norm(ahead - pixel), 1e-3))
			if not (8 <= pixel[0] < width - 8 and 8 <= pixel[1] < height - 8) or np.dot(point - eye, forward) <= 0:
				continue
			across = np.linalg.norm(project((point + normal * 0.5)[None, :])[0] - project((point - normal * 0.5)[None, :])[0])
			if across < 8:
				continue
			offsets, values = edge_values(studio, project, point, normal, half)
			visibility, lit, dark = normalise(offsets, values, half)
			if lit < 0.5 or dark > 0.2 * lit:
				continue
			offsets_ror, values_ror = edge_values(ror, project, point, normal, half)
			visibility_ror, _, _ = normalise(offsets_ror, values_ror, half)
			samples.append({
				"pixel": pixel.tolist(),
				"point": point.tolist(),
				"depth": float(np.dot(point - eye, forward)),
				"distance": float(np.linalg.norm(point - eye)),
				"radius": float(np.hypot(*(point - eye)[[0, 2]])),
				"pxPerStud": float(across),
				"studio": stats(offsets, visibility)[0] * inputs.get("mapScale", 1.0),
				"ror": stats(offsets_ror, visibility_ror)[0] * inputs.get("mapScale", 1.0),
			})
	return samples


def main():
	directory = sys.argv[1]
	labels = sys.argv[2:]
	runs = json.load(open(os.path.join(directory, "samples.json")))
	for run in runs:
		if labels and run["label"] not in labels:
			continue
		studio = load_image(os.path.join(directory, run["label"], "studio.png"))
		ror = load_image(os.path.join(directory, run["label"], "ror.png"))
		samples = scan(run, studio, ror)
		with open(os.path.join(directory, run["label"], "widths.json"), "w") as file:
			json.dump(samples, file)
		straight_down = abs(run["camera"][8]) > 0.999
		key, step = ("radius", 2.0) if straight_down else ("depth", 4.0)
		bins = defaultdict(list)
		for sample in samples:
			bins[int(sample[key] // step)].append((sample["studio"], sample["ror"]))
		print(f"== {run['label']}: {len(samples)} samples, median width (studio / ror) by {key}")
		print("  " + "  ".join(f"{index * step:.0f}: {np.median([v[0] for v in values]):.3f}/{np.median([v[1] for v in values]):.3f}" for index, values in sorted(bins.items()) if len(values) >= 5))


if __name__ == "__main__":
	main()
