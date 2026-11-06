"""Which LOD Studio drew in each run of `mesh-lod`: the silhouette (gray mesh against the blue sky)
of studio.png against ror renders of every LOD (ror-lod<k>.png, from `examples/tools/rerender.rs` with
ROR_TUNE_LOD=k). Prints, per run, the IoU with each LOD and the best match.

    python tools/analysis/lod_match.py [experiment, default mesh-lod] [levels, default 4]
"""

import os
import sys

import numpy as np
from PIL import Image

experiment = sys.argv[1] if len(sys.argv) > 1 else "mesh-lod"
levels = int(sys.argv[2]) if len(sys.argv) > 2 else 4
root = os.path.join("out/calibrate", experiment)


def mask(path):
	pixels = np.asarray(Image.open(path).convert("RGB"), dtype=np.int32)
	return (pixels[:, :, 2] - pixels[:, :, 0]) < 20


def iou(a, b):
	union = (a | b).sum()
	return (a & b).sum() / union if union else 1.0


def order(run):
	label, _, distance = run.rpartition("-d")
	return (label, float(distance) if distance.replace(".", "").isdigit() else 0.0)


for run in sorted(os.listdir(root), key=order):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory):
		continue
	studio = mask(os.path.join(directory, "studio.png"))
	scores = []
	for level in range(levels):
		path = os.path.join(directory, f"ror-lod{level}.png")
		scores.append(iou(studio, mask(path)) if os.path.exists(path) else float("nan"))
	best = int(np.nanargmax(scores))
	area = studio.sum()
	print(f"{run:32s} area {area:8d} px  " + "  ".join(f"lod{level} {score:.4f}" for level, score in enumerate(scores)) + f"  -> LOD{best}")
