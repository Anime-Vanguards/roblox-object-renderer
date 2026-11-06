"""Finds the shift between Studio's and ror's texture pattern on a head-on face: normalised cross-
correlation of the high-passed captures (zero-padded, so partial overlaps count by their own energy)
over shifts up to `--max` studs.

    python tools/analysis/grain_shift.py <run dir> <px per stud> [--box x0,y0,x1,y1] [--max 4.5]
"""

import sys

import numpy as np
from PIL import Image
from scipy.signal import fftconvolve


def high_pass(image):
	return image - fftconvolve(image, np.ones((9, 9)) / 81, mode="same")


def main():
	args = sys.argv[1:]
	run, pps = args[0], float(args[1])
	limit = float(args[args.index("--max") + 1]) if "--max" in args else 4.5
	studio = np.asarray(Image.open(run + "/studio.png").convert("L"), dtype=float)
	ror = np.asarray(Image.open(run + "/ror.png").convert("L"), dtype=float)
	h, w = studio.shape
	if "--box" in args:
		x0, y0, x1, y1 = [int(v) for v in args[args.index("--box") + 1].split(",")]
	else:
		x0, y0, x1, y1 = w // 2 - 400, h // 2 - 300, w // 2 + 400, h // 2 + 300
	a = high_pass(studio)[y0:y1, x0:x1]
	b = high_pass(ror)[y0:y1, x0:x1]
	a -= a.mean()
	b -= b.mean()
	# correlation of a with b shifted, and each one's energy over the overlap
	ones = np.ones_like(a)
	cross = fftconvolve(a, b[::-1, ::-1], mode="full")
	energy_a = fftconvolve(a * a, ones[::-1, ::-1], mode="full")
	energy_b = fftconvolve(ones, (b * b)[::-1, ::-1], mode="full")
	overlap = fftconvolve(ones, ones[::-1, ::-1], mode="full")
	ncc = cross / np.sqrt(np.maximum(energy_a * energy_b, 1e-9))
	ncc[overlap < 0.25 * a.size] = -1
	cy, cx = a.shape[0] - 1, a.shape[1] - 1
	r = int(limit * pps)
	window = ncc[max(cy - r, 0) : cy + r + 1, max(cx - r, 0) : cx + r + 1]
	iy, ix = np.unravel_index(window.argmax(), window.shape)
	dy, dx = iy + max(cy - r, 0) - cy, ix + max(cx - r, 0) - cx
	print(f"{run}: Studio's pattern is ror's moved by ({dx / pps:+.3f}, {dy / pps:+.3f}) studs (right, down), NCC {window.max():.3f}, at zero {ncc[cy, cx]:.3f}")


main()
