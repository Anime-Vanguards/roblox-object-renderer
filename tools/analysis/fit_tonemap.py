"""Fits Studio's tone curve to the sun-board calibration (out/calibrate/sun-board/samples.json).

The board is seen from far away through a narrow field of view, so every tile has the same view
and light direction and one specular constant applies to all of them.

Model: display = 255 * sqrt(T(e * b * (albedo * ndotl + ks))), albedo = colour² (Roblox decodes
colours with gamma 2.0, and encodes the output with its square root: both fit far better than the
sRGB curves), b the Lighting.Brightness, e = 2^ExposureCompensation (tested), ks the sun's specular
in albedo units, T a rational tone curve (x (A x + B)) / (x (C x + D) + 1), fitted per channel on
gray and dark tiles of the bloom-free runs (b <= 3).

    python tools/analysis/fit_tonemap.py [samples.json] [--max-brightness 3]
"""

import argparse
import json

import numpy as np
from scipy.optimize import least_squares


def srgb_decode(c):
	c = np.asarray(c, dtype=np.float64)
	return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def srgb_encode(c):
	c = np.clip(np.asarray(c, dtype=np.float64), 0.0, 1.0)
	return np.where(c <= 0.0031308, c * 12.92, 1.055 * c ** (1.0 / 2.4) - 0.055)


def rational(x, p):
	a, b, c, d = p
	x = np.maximum(x, 0.0)
	return (x * (a * x + b)) / (x * (c * x + d) + 1.0)


def hable(x, p):
	A, B, C, D, E, F, W = p

	def f(v):
		return ((v * (A * v + C * B) + D * E) / (v * (A * v + B) + D * F)) - E / F

	return f(np.maximum(x, 0.0)) / f(W)


def load(path, prefixes, max_brightness, with_exposure):
	runs = json.load(open(path))
	rows = []
	for run in runs:
		environment = run["environment"]
		b = environment["brightness"]
		exposure = environment.get("exposureCompensation", 0.0)
		if b > max_brightness or (exposure != 0.0 and not with_exposure):
			continue
		for sample in run["samples"]:
			if not sample["probe"].startswith(prefixes):
				continue
			for channel in range(3):
				albedo = sample["inputs"]["color"][channel] / 255.0
				rows.append((b, exposure, albedo, sample["inputs"].get("ndotl", 1.0), sample["studio"][channel] / 255.0, sample["ror"][channel] / 255.0))
	return np.array(rows)


def main():
	parser = argparse.ArgumentParser()
	parser.add_argument("path", nargs="?", default="out/calibrate/sun-board/samples.json")
	parser.add_argument("--max-brightness", type=float, default=3.0)
	args = parser.parse_args()

	data = load(args.path, ("gray", "dark"), args.max_brightness, with_exposure=False)
	b, exposure, g, ndotl, studio, ror = data.T
	albedo = g * g

	def model(params, b, exposure, albedo, ndotl):
		ks = params[0]
		return np.sqrt(np.clip(rational(2.0 ** exposure * b * (albedo * ndotl + ks), params[1:5]), 0.0, 1.0))

	fit = least_squares(lambda p: model(p, b, exposure, albedo, ndotl) - studio, np.array([0.01, 0.3, 0.05, 0.3, 0.6]), max_nfev=100000)
	error = np.abs(fit.fun) * 255
	ks = fit.x[0]
	A, B, C, D = fit.x[1:5]
	print(f"{len(data)} gray samples (b <= {args.max_brightness}): max {error.max():.2f} mean {error.mean():.3f}")
	print(f"ks = {ks:.6f}")
	print(f"T(x) = (x ({A:.6f} x + {B:.6f})) / (x ({C:.6f} x + {D:.6f}) + 1)")
	for brightness in sorted(set(b)):
		mask = b == brightness
		print(f"   b={brightness}: mean signed {np.mean(fit.fun[mask]) * 255:+.3f} max {np.max(np.abs(fit.fun[mask])) * 255:.2f}")
	worst = np.argsort(-error)[:6]
	for index in worst:
		print(f"   worst: b={b[index]} gray={g[index] * 255:.0f} studio={studio[index] * 255:.1f} fit={(fit.fun[index] + studio[index]) * 255:.1f}")

	# validation on everything else: colours, tilted tiles, exposure runs, bloom runs
	for label, prefixes, max_b, with_exp in [
		("colours b<=3", ("color",), 3.0, False),
		("tilt b<=3", ("tilt",), 3.0, False),
		("exposure runs", ("gray", "dark"), 1.0, True),
		("all b (bloom)", ("gray", "dark", "color"), 100.0, False),
	]:
		check = load(args.path, prefixes, max_b, with_exp)
		cb, ce, cg, cn, cs, _ = check.T
		if with_exp:
			mask = ce != 0
			cb, ce, cg, cn, cs = cb[mask], ce[mask], cg[mask], cn[mask], cs[mask]
		residual = (model(fit.x, cb, ce, cg * cg, cn) - cs) * 255
		print(f"check {label}: n={len(cs)} mean abs {np.mean(np.abs(residual)):.3f} max {np.max(np.abs(residual)):.2f} mean signed {np.mean(residual):+.3f}")
		if with_exp:
			for value in sorted(set(ce)):
				mask = ce == value
				print(f"      ev={value:+}: mean signed {np.mean(residual[mask]):+.3f} max {np.max(np.abs(residual[mask])):.2f}")
	json.dump({"ks": ks, "A": A, "B": B, "C": C, "D": D}, open(args.path.replace("samples.json", "tonemap.json"), "w"), indent=1)


if __name__ == "__main__":
	main()
