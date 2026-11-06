"""Which outline model reproduces Studio's Highlight outlines (experiment `highlights`): the object's
mask is taken from ror's render of the same scene (a pixel is inside when it is nearer the object's
colour than the background's), then each model predicts Studio's outline pixels.

    python tools/analysis/highlight_outline.py [run ...]
"""

import sys

import numpy as np
from PIL import Image
from scipy import ndimage

BASE = "out/calibrate/highlights/"
RUNS = {
	"default-top": (255, 255, 255),
	"default-near": (255, 255, 255),
	"default-far": (255, 255, 255),
	"outline-green-near": (0, 255, 0),
	"outline-green-far": (0, 255, 0),
	"outline-black-near": (0, 0, 0),
	"outline-black-far": (0, 0, 0),
	"ball-ontop": (255, 255, 255),
}
OBJECT = np.array([82.0, 82.0, 82.0])


def mask_from_ror(ror):
	h, w, _ = ror.shape
	distance_object = np.abs(ror - OBJECT).max(axis=2)
	inside = distance_object <= 3
	labels, _ = ndimage.label(inside)
	inside = labels == labels[h // 2, w // 2]
	# the background next to the object, for edge pixels' coverage
	ring = ndimage.binary_dilation(inside, iterations=3) & ~ndimage.binary_dilation(inside, iterations=1)
	background = np.median(ror[ring], axis=0)
	edge = ndimage.binary_dilation(inside, iterations=1) & ~inside
	coverage = np.zeros(inside.shape)
	span = background - OBJECT
	usable = np.abs(span) > 10
	for y, x in zip(*np.nonzero(edge)):
		pixel = ror[y, x]
		coverage[y, x] = np.mean(((background - pixel) / np.where(usable, span, 1.0))[usable])
	coverage[inside] = 1.0
	return coverage


def half_res(mask, offset):
	# pairs of pixels (2k + offset, 2k + 1 + offset) per axis, sampled at the pair's centre
	h, w = mask.shape
	padded = np.pad(mask, ((offset, 2), (offset, 2)))
	ys = np.arange(0, h + offset, 2)
	xs = np.arange(0, w + offset, 2)
	small = np.zeros((len(ys), len(xs)), bool)
	for i, y in enumerate(ys):
		for j, x in enumerate(xs):
			small[i, j] = padded[y : y + 2, x : x + 2].mean() >= 0.5
	return small


def up(small, offset, shape):
	big = np.repeat(np.repeat(small, 2, axis=0), 2, axis=1)
	return big[offset : offset + shape[0], offset : offset + shape[1]]


def models(coverage):
	inside = coverage >= 0.5
	square = np.ones((3, 3), bool)
	predictions = {
		"2 out (square)": ndimage.binary_dilation(inside, square, iterations=2) & ~inside,
		"2 out (cross)": ndimage.binary_dilation(inside, iterations=2) & ~inside,
		"1 out 1 in": ndimage.binary_dilation(inside, square) & ~ndimage.binary_erosion(inside, square),
	}
	for threshold in (0.02, 0.15, 0.3):
		touched = coverage > threshold
		predictions[f"touched>{threshold} ring"] = ndimage.binary_dilation(touched, square) & ~ndimage.binary_erosion(touched, square)
		predictions[f"touched>{threshold} ring (cross)"] = ndimage.binary_dilation(touched) & ~ndimage.binary_erosion(touched)
	for offset in (0, 1):
		small = half_res(coverage, offset)
		ring = ndimage.binary_dilation(small, square) & ~small
		predictions[f"half-res ring, offset {offset}"] = up(ring, offset, inside.shape)
		ring = ndimage.binary_dilation(small) & ~small
		predictions[f"half-res ring (cross), offset {offset}"] = up(ring, offset, inside.shape)
	return predictions


def main():
	runs = sys.argv[1:] or list(RUNS)
	totals = {}
	for run in runs:
		studio = np.asarray(Image.open(BASE + run + "/studio.png").convert("RGB"), dtype=float)
		ror = np.asarray(Image.open(BASE + run + "/ror.png").convert("RGB"), dtype=float)
		actual = np.abs(studio - np.array(RUNS[run], float)).max(axis=2) <= 10
		coverage = mask_from_ror(ror)
		# only near this object
		near = ndimage.binary_dilation(coverage > 0, iterations=6)
		actual &= near
		line = []
		for name, predicted in models(coverage).items():
			predicted = predicted & near
			union = (actual | predicted).sum()
			iou = (actual & predicted).sum() / max(union, 1)
			totals.setdefault(name, []).append(iou)
			line.append(f"{name} {iou:.3f}")
		print(f"{run:20s} outline px {actual.sum():6d}  IoU: " + ", ".join(line))
	print("mean IoU: " + ", ".join(f"{name} {np.mean(values):.3f}" for name, values in totals.items()))


main()
