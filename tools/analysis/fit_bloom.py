"""Fits Studio's built-in bloom to the bloom-probe halos.

Model (per channel, in tone-curve input units x): the bloom source is max(0, min(x, cap) - t),
blurred with a sum of isotropic Gaussians (weights w_i, widths s_i in pixels at the capture's
1116 px viewport height), added to x before the tone curve. A square Neon tile of width L px and
emission e then adds, at distance d from its centre along the row through it,

    halo(d) = (min(e, cap) - t)+ * sum_i w_i * [Phi((d + L/2)/s_i) - Phi((d - L/2)/s_i)] * erfw_i

with erfw_i = Phi(L/2/s_i) - Phi(-L/2/s_i) (the perpendicular extent). Halo samples are read
outside the tile (tools/analysis/bloom_profiles.py converts display values to x).

    python tools/analysis/fit_bloom.py
"""

import json
import math

import numpy as np
from PIL import Image
from scipy.optimize import least_squares
from scipy.special import ndtr

import bloom_profiles as bp

PIXELS_PER_STUD = 1116 / 2 / math.tan(math.radians(35)) / 52


def load(directory="out/calibrate/bloom-probe"):
	rows = []
	for run in json.load(open(f"{directory}/samples.json")):
		exposure = 2.0 ** run["environment"]["exposureCompensation"]
		image = np.asarray(Image.open(f"{directory}/{run['label']}/studio.png").convert("RGB"))
		for sample in run["samples"]:
			size = sample["inputs"]["size"] * PIXELS_PER_STUD
			emission = sample["inputs"]["emission"] * exposure
			cx, cy = sample["pixel"]
			for axis in (0, 1):
				offsets, row, offsets_y, column = bp.profiles(image, cx, cy, 140)
				distances, values = (offsets, row) if axis == 0 else (offsets_y, column)
				for distance, value in zip(distances, values[:, 1]):
					d = abs(distance)
					# outside the tile and its anti-aliased edge, inside the measurable range
					if d < size / 2 + 2.5 or d > size / 2 + 90 or value >= 252:
						continue
					rows.append((emission, size, d, bp.radiance(value)))
	return np.array(rows)


def halo(params, emission, size, distance, kernels):
	threshold, cap = params[0], params[1]
	weights = params[2 : 2 + kernels]
	widths = np.abs(params[2 + kernels : 2 + 2 * kernels])
	source = np.maximum(np.minimum(emission, cap) - threshold, 0.0)
	total = np.zeros_like(distance)
	for weight, width in zip(weights, widths):
		along = ndtr((distance + size / 2) / width) - ndtr((distance - size / 2) / width)
		across = ndtr(size / 2 / width) - ndtr(-size / 2 / width)
		total += weight * along * across
	return source * total


def main():
	data = load()
	emission, size, distance, measured = data.T
	print(f"{len(data)} halo samples")
	for kernels in (1, 2, 3):
		start = [3.5, 9.0] + [0.3] * kernels + [3.0 * (2.5 ** i) for i in range(kernels)]
		fit = least_squares(lambda p: halo(p, emission, size, distance, kernels) - measured, np.array(start), max_nfev=50000)
		# compare in display units (what the capture shows)
		predicted = halo(fit.x, emission, size, distance, kernels)
		display_error = (np.sqrt(bp.tonemap(predicted)) - np.sqrt(bp.tonemap(measured))) * 255
		print(f"{kernels} gaussian(s): threshold {fit.x[0]:.3f} cap {fit.x[1]:.3f} weights {np.round(fit.x[2:2 + kernels], 4)} widths px {np.round(np.abs(fit.x[2 + kernels:]), 3)}")
		print(f"    display error: mean {np.abs(display_error).mean():.2f} max {np.abs(display_error).max():.1f}; x error mean {np.abs(predicted - measured).mean():.4f}")
	json.dump({"threshold": fit.x[0], "cap": fit.x[1], "weights": list(fit.x[2:5]), "widthsPx": list(np.abs(fit.x[5:8])), "viewportHeight": 1116}, open("out/calibrate/bloom-probe/bloom.json", "w"), indent=1)


if __name__ == "__main__":
	main()
