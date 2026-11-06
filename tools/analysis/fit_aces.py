"""Fits ACES-style tonemapping (input matrix, per-channel RRT+ODT fit, output matrix) to the
sun-board samples, gray and coloured tiles together, per channel.

    python tools/analysis/fit_aces.py [samples.json]
"""

import json
import sys

import numpy as np
from scipy.optimize import least_squares

sys.path.insert(0, "tools/analysis")
from fit_tonemap import srgb_decode, srgb_encode  # noqa: E402

ACES_IN = np.array([[0.59719, 0.35458, 0.04823], [0.07600, 0.90834, 0.01566], [0.02840, 0.13383, 0.83777]])
ACES_OUT = np.array([[1.60475, -0.53108, -0.07367], [-0.10208, 1.10813, -0.00605], [-0.00327, -0.07276, 1.07602]])


def rrt_odt(v):
	a = v * (v + 0.0245786) - 0.000090537
	b = v * (0.983729 * v + 0.4329510) + 0.238081
	return a / b


def aces_fitted(rgb):
	v = rgb @ ACES_IN.T
	v = rrt_odt(v)
	return np.clip(v @ ACES_OUT.T, 0.0, 1.0)


def narkowicz(rgb):
	x = np.maximum(rgb, 0)
	return np.clip((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0, 1)


def load(path, prefixes=("gray", "color")):
	runs = json.load(open(path))
	b, albedo, studio = [], [], []
	for run in runs:
		for sample in run["samples"]:
			if not sample["probe"].startswith(prefixes):
				continue
			b.append(run["environment"]["brightness"])
			albedo.append(srgb_decode(np.array(sample["inputs"]["color"]) / 255.0))
			studio.append(np.array(sample["studio"]) / 255.0)
	return np.array(b), np.array(albedo), np.array(studio)


def main():
	path = sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/sun-board/samples.json"
	b, albedo, studio = load(path)
	for name, curve in [("aces-fitted", aces_fitted), ("narkowicz", narkowicz)]:
		for free_gamma in (False, True):

			def residual(p):
				kd, ks = p[0], p[1]
				gamma = p[2] if free_gamma else 1.0
				x = (b ** gamma)[:, None] * (kd * albedo + ks)
				return (srgb_encode(curve(x)) - studio).ravel()

			start = [1.0, 0.01] + ([1.0] if free_gamma else [])
			fit = least_squares(residual, np.array(start), max_nfev=20000)
			err = np.abs(fit.fun) * 255
			print(f"{name} gamma={'free' if free_gamma else '1'}: max {err.max():.2f} mean {err.mean():.2f}  params {np.round(fit.x, 5)}")
			worst = np.argsort(-err)[:6]
			predicted = (fit.fun + studio.ravel()) * 255
			for index in worst:
				sample, channel = divmod(index, 3)
				print(f"    b={b[sample]} albedo={np.round(srgb_encode(albedo[sample]) * 255)} ch={channel} studio={studio[sample][channel] * 255:.1f} fit={predicted[index]:.1f}")


if __name__ == "__main__":
	main()
