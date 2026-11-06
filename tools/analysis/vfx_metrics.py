"""Metrics of effects (trails, particles, beams) in calibration runs: over the pixels the effect
touches (where Studio's or ror's image differs from the effect-free background by more than a
threshold), the share within 2 and 8 of 255, the mean error, the coverage IoU of the two footprints,
and the light ratio (ror / Studio, summed √-display excess over the background); and over the whole
image. `metrics()` also gives what tells the kind of miss apart: the light ratio per channel
(colour), the darkening ratio (summed deficit under the background: smoke, negative LightEmission),
the shift between the two footprints' centroids (a share of the image height) and the ratio of
their spreads (RMS radius about their centroids, ror / Studio: too big or too small).

    python tools/analysis/vfx_metrics.py <experiment> [run filter] [--background <png or 'black'>] [--ror <image>]

`--ror before.png` measures another image of ror's in each run's directory (e.g. a baseline build's).

The background is pure black unless given (a run's own `background.png` is used when present).
"""

import os
import sys

import numpy as np
from PIL import Image


def load(path):
	return np.asarray(Image.open(path).convert("RGB"), dtype=np.int32)


def box_blur(image, radius):
	# a separable box blur by cumulative sums (edges clamped)
	padded = np.pad(image.astype(np.float64), ((radius + 1, radius), (radius + 1, radius), (0, 0)), mode="edge")
	cumulative = padded.cumsum(axis=0)
	rows = (cumulative[2 * radius + 1 :] - cumulative[: -2 * radius - 1]) / (2 * radius + 1)
	cumulative = rows.cumsum(axis=1)
	return (cumulative[:, 2 * radius + 1 :] - cumulative[:, : -2 * radius - 1]) / (2 * radius + 1)


def metrics(studio, ror, background, threshold=6, blur=12):
	diff = ror - studio
	largest = np.abs(diff).max(axis=2)
	excess_s = np.clip(studio - background, 0, None).sum(axis=2)
	excess_r = np.clip(ror - background, 0, None).sum(axis=2)
	touched_s = np.abs(studio - background).max(axis=2) > threshold
	touched_r = np.abs(ror - background).max(axis=2) > threshold
	region = touched_s | touched_r
	union = region.sum()
	result = {
		"exact": (largest <= 2).mean() * 100,
		"within8": (largest <= 8).mean() * 100,
		"mae": np.abs(diff).mean(),
	}
	if union:
		result.update(
			{
				"fx_exact": (largest[region] <= 2).mean() * 100,
				"fx_within8": (largest[region] <= 8).mean() * 100,
				"fx_mae": np.abs(diff)[region].mean(),
				"iou": (touched_s & touched_r).sum() / union,
				"light": excess_r[region].sum() / max(excess_s[region].sum(), 1),
				"pixels": int(union),
			}
		)
		# random particles cannot match pixel for pixel: their light, blurred, over the same region
		soft = np.abs(box_blur(ror, blur) - box_blur(studio, blur)).max(axis=2)
		result["blur_within8"] = (soft[region] <= 8).mean() * 100
		result["blur_mae"] = soft[region].mean()
		# the kind of miss: colour, darkening, place and size
		channel_s = np.clip(studio - background, 0, None)[region].sum(axis=0)
		channel_r = np.clip(ror - background, 0, None)[region].sum(axis=0)
		result["light_rgb"] = [float(r / max(s, 1)) for r, s in zip(channel_r, channel_s)]
		deficit_s = np.clip(background - studio, 0, None).sum(axis=2)[region].sum()
		deficit_r = np.clip(background - ror, 0, None).sum(axis=2)[region].sum()
		result["dark"] = float(deficit_r / max(deficit_s, 1))
		result["dark_share"] = float(deficit_s / max(deficit_s + excess_s[region].sum(), 1))
		ys, xs = np.mgrid[0 : studio.shape[0], 0 : studio.shape[1]]
		weight_s = np.abs(studio - background).max(axis=2) * touched_s
		weight_r = np.abs(ror - background).max(axis=2) * touched_r
		if weight_s.sum() > 0 and weight_r.sum() > 0:
			centre_s = np.array([(xs * weight_s).sum(), (ys * weight_s).sum()]) / weight_s.sum()
			centre_r = np.array([(xs * weight_r).sum(), (ys * weight_r).sum()]) / weight_r.sum()
			result["shift"] = float(np.hypot(*(centre_r - centre_s)) / studio.shape[0])
			spread_s = np.sqrt((((xs - centre_s[0]) ** 2 + (ys - centre_s[1]) ** 2) * weight_s).sum() / weight_s.sum())
			spread_r = np.sqrt((((xs - centre_r[0]) ** 2 + (ys - centre_r[1]) ** 2) * weight_r).sum() / weight_r.sum())
			result["spread"] = float(spread_r / max(spread_s, 1e-6))
	return result


def main():
	args = [a for a in sys.argv[1:]]
	background_arg = None
	ror_name = "ror.png"
	if "--ror" in args:
		index = args.index("--ror")
		ror_name = args[index + 1]
		del args[index : index + 2]
	if "--background" in args:
		index = args.index("--background")
		background_arg = args[index + 1]
		del args[index : index + 2]
	experiment = args[0]
	needle = args[1] if len(args) > 1 else ""
	root = os.path.join("out/calibrate", experiment)
	rows = []
	for run in sorted(os.listdir(root)):
		directory = os.path.join(root, run)
		if not os.path.isdir(directory) or needle not in run or not os.path.exists(os.path.join(directory, ror_name)):
			continue
		studio = load(os.path.join(directory, "studio.png"))
		ror = load(os.path.join(directory, ror_name))
		own = os.path.join(directory, "background.png")
		if os.path.exists(own):
			background = load(own)
		elif background_arg and background_arg != "black":
			background = load(background_arg)
		else:
			background = np.zeros_like(studio)
		rows.append((run, metrics(studio, ror, background)))
	for run, m in rows:
		line = f"{run:28s} image {m['exact']:5.1f}/{m['within8']:5.1f}% mae {m['mae']:5.2f}"
		if "iou" in m:
			line += f" | fx {m['fx_exact']:5.1f}/{m['fx_within8']:5.1f}% mae {m['fx_mae']:5.2f} iou {m['iou']:.3f} light {m['light']:.3f} blurred within8 {m['blur_within8']:5.1f}% ({m['pixels']} px)"
		print(line)
	if rows:
		fx = [m for _, m in rows if "iou" in m]
		print(f"mean: image {np.mean([m['exact'] for _, m in rows]):.1f}/{np.mean([m['within8'] for _, m in rows]):.1f}%", end="")
		if fx:
			print(f" | fx {np.mean([m['fx_exact'] for m in fx]):.1f}/{np.mean([m['fx_within8'] for m in fx]):.1f}% iou {np.mean([m['iou'] for m in fx]):.3f} light {np.mean([m['light'] for m in fx]):.3f} blurred within8 {np.mean([m['blur_within8'] for m in fx]):.1f}%")
		else:
			print()


if __name__ == "__main__":
	main()
