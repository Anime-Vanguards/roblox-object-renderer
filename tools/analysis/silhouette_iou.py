"""Silhouette agreement of a calibration experiment's runs: a gray model in front of the blue sky
(skinning experiments), masked by chroma (pixels whose blue exceeds red by less than 20 are the
model). Prints the IoU of the Studio and ror masks and the pixels only one of them covers.

    python tools/analysis/silhouette_iou.py <experiment> [run filter]
"""

import os
import sys

import numpy as np
from PIL import Image

experiment = sys.argv[1]
needle = sys.argv[2] if len(sys.argv) > 2 else ""
root = os.path.join("out/calibrate", experiment)


def mask(path):
	pixels = np.asarray(Image.open(path).convert("RGB"), dtype=np.int32)
	return (pixels[:, :, 2] - pixels[:, :, 0]) < 20


rows = []
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run:
		continue
	studio = mask(os.path.join(directory, "studio.png"))
	ror = mask(os.path.join(directory, "ror.png"))
	union = (studio | ror).sum()
	iou = (studio & ror).sum() / max(union, 1)
	rows.append((run, iou, (studio & ~ror).sum(), (ror & ~studio).sum(), studio.sum()))
for run, iou, studio_only, ror_only, area in rows:
	print(f"{run:26s} IoU {iou:.4f}  Studio only {studio_only:6d}  ror only {ror_only:6d}  of {area} px")
print(f"mean IoU {np.mean([row[1] for row in rows]):.4f}")
