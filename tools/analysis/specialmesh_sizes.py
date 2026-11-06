"""Measures each SpecialMesh case's extent (studs) in the specialmesh-sizes captures by flood-filling
from its projected centre: front view gives width (x) and height (y), top view width and depth (z).

    python tools/analysis/specialmesh_sizes.py [studio|ror]
"""

import json
import math
import sys

import numpy as np
from PIL import Image
from scipy import ndimage

side = sys.argv[1] if len(sys.argv) > 1 else "studio"
runs = {run["label"]: run for run in json.load(open("out/calibrate/specialmesh-sizes/samples.json"))}
TAN = math.tan(math.radians(5.0))


def extent(view, sample):
	image = np.asarray(Image.open(f"out/calibrate/specialmesh-sizes/{view}/{side}.png").convert("RGB")).astype(int)
	cx, cy = (int(round(value)) for value in sample["pixel"])
	seed = image[cy, cx]
	# pixels close to the seed colour, connected to it
	close = np.abs(image - seed).max(axis=2) <= 18
	labels, _ = ndimage.label(close)
	region = labels == labels[cy, cx]
	ys, xs = np.nonzero(region)
	center = np.array(sample["inputs"]["size"])  # unused, keeps the case visible in errors
	position = np.array(json.loads(json.dumps(sample.get("position", [0, 0, 0]))))
	return xs.max() - xs.min() + 1, ys.max() - ys.min() + 1


print(f"{'case':5} {'type':5} {'size':16} {'scale':16} {'front w,h (studs)':20} {'top w,d (studs)':20}")
for index, (front, top) in enumerate(zip(runs["front"]["samples"], runs["top"]["samples"])):
	inputs = front["inputs"]
	row = index // 4
	depth_front = 200.0 + (row - 1.5) * 6.0  # camera at z = -200, part z = (row - 1.5) * 6
	depth_top = 200.0 - (row - 1.5) * 6.0  # camera at y = 200, part y = (row - 1.5) * 6
	scale_front = 558.0 / TAN / depth_front
	scale_top = 558.0 / TAN / depth_top
	fw, fh = extent("front", front)
	tw, td = extent("top", top)
	print(f"{index:<5} {inputs['meshType']:<5} {str(inputs['size']):16} {str(inputs['scale']):16} {fw / scale_front:6.3f} {fh / scale_front:6.3f}        {tw / scale_top:6.3f} {td / scale_top:6.3f}")
