"""Counts the corners of a Ball / Cylinder silhouette in the tessellation captures: the boundary's
radius around the centroid peaks at the polygon's vertices and dips at mid-edge.

    python tools/analysis/tessellation.py tessellation-ball [studio|ror]
"""

import sys

import numpy as np
from PIL import Image
from scipy import ndimage, signal

experiment = sys.argv[1] if len(sys.argv) > 1 else "tessellation-ball"
side = sys.argv[2] if len(sys.argv) > 2 else "studio"
for view in ["x", "y", "z"]:
	image = np.asarray(Image.open(f"out/calibrate/{experiment}/{view}/{side}.png").convert("RGB")).astype(float)
	height, width, _ = image.shape
	# the shape is gray; the sky is blue (b clearly above r)
	shape = (image[..., 2] - image[..., 0]) < 25
	labels, count = ndimage.label(shape)
	center_label = labels[height // 2, width // 2]
	region = labels == center_label
	edge = region & ~ndimage.binary_erosion(region)
	ys, xs = np.nonzero(edge)
	cy, cx = ys.mean(), xs.mean()
	angles = np.arctan2(ys - cy, xs - cx)
	radii = np.hypot(ys - cy, xs - cx)
	bins = 2048
	index = ((angles + np.pi) / (2 * np.pi) * bins).astype(int) % bins
	profile = np.zeros(bins)
	for i, r in zip(index, radii):
		profile[i] = max(profile[i], r)
	# fill empty bins, smooth lightly, find peaks (circular)
	filled = np.where(profile > 0, profile, np.interp(np.arange(bins), np.nonzero(profile)[0], profile[profile > 0]))
	smooth = ndimage.gaussian_filter1d(filled, 3, mode="wrap")
	extended = np.concatenate([smooth, smooth[:64]])
	peaks, _ = signal.find_peaks(extended, prominence=0.15)
	peaks = peaks[peaks < bins]
	span = smooth.max() - smooth.min()
	print(f"{experiment} view {view} ({side}): region {region.sum()} px, radius {smooth.min():.1f}-{smooth.max():.1f} (ripple {span:.2f} px), corners {len(peaks)}")
