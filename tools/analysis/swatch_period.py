"""Tiling period of each material-swatches capture: the strongest repeat of the 2D autocorrelation
along the image axes (px, then studs at the swatch camera's 53.64 px per stud).

    python tools/analysis/swatch_period.py [flat|sun]
"""

import os
import sys

import numpy as np
from PIL import Image

ROOT = "out/calibrate/material-swatches"
PX_PER_STUD = 558.0 / (59.0 * np.tan(np.radians(10.0)))
lighting = sys.argv[1] if len(sys.argv) > 1 else "flat"


def gray(path):
	image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float64) / 255.0
	return (image ** 2).mean(axis=2)


def autocorrelation(image):
	h, w = image.shape
	centered = image - image.mean()
	padded = np.zeros((2 * h, 2 * w))
	padded[:h, :w] = centered
	spectrum = np.fft.rfft2(padded)
	corr = np.fft.irfft2(spectrum * np.conj(spectrum), s=padded.shape)
	# normalise by the overlap area so long lags are not penalised
	overlap = np.outer(np.concatenate([np.arange(h, 0, -1), np.arange(0, h)]), np.concatenate([np.arange(w, 0, -1), np.arange(0, w)]))
	corr = corr / np.maximum(overlap, 1)
	return corr / corr[0, 0]


def strongest(profile, lo, hi):
	segment = profile[lo:hi]
	index = int(np.argmax(segment))
	return lo + index, float(segment[index])


for run in sorted(name for name in os.listdir(ROOT) if name.endswith("-" + lighting)):
	image = gray(os.path.join(ROOT, run, "studio.png"))
	h, w = image.shape
	corr = autocorrelation(image)
	# lags of 60 px .. 3/4 of the image along x (columns) and y (rows)
	px, vx = strongest(corr[0, :], 60, int(w * 0.75))
	py, vy = strongest(corr[:, 0], 60, int(h * 0.75))
	print(f"{run[:-len(lighting) - 1]:14s} x {px:5d} px {px / PX_PER_STUD:6.2f} studs ({vx:.2f})   y {py:5d} px {py / PX_PER_STUD:6.2f} studs ({vy:.2f})   contrast {image.std() / max(image.mean(), 1e-6):.3f}")
