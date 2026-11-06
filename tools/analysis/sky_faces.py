"""Which skybox image (and rotation / flip) Studio shows along each axis.

With a 90° vertical field of view from the origin, the central square of the capture is exactly one
cube face. Each such crop is compared (normalised correlation of luminance) with the six decoded
default sky images under the 8 rotations / flips of the square.

    python tools/analysis/sky_faces.py
"""

import numpy as np
from PIL import Image

FACES = ["bk", "dn", "ft", "lf", "rt", "up"]
images = {name: np.asarray(Image.open(f"out/sky/sky512_{name}.png").convert("L").resize((128, 128), Image.BILINEAR)).astype(float) for name in FACES}


def orientations(array):
	for flip in (False, True):
		base = np.fliplr(array) if flip else array
		for turns in range(4):
			yield (f"{'flip+' if flip else ''}rot{turns * 90}", np.rot90(base, turns))


def correlation(a, b):
	a = (a - a.mean()) / (a.std() + 1e-9)
	b = (b - b.mean()) / (b.std() + 1e-9)
	return float((a * b).mean())


for view in ["front", "back", "right", "left", "up", "down"]:
	capture = Image.open(f"out/calibrate/sky-faces/{view}/studio.png").convert("L")
	width, height = capture.size
	left = (width - height) // 2
	crop = np.asarray(capture.crop((left, 0, left + height, height)).resize((128, 128), Image.BILINEAR)).astype(float)
	scores = []
	for name, image in images.items():
		for label, oriented in orientations(image):
			scores.append((correlation(crop, oriented), name, label))
	scores.sort(reverse=True)
	best = scores[0]
	print(f"{view:6}: {best[1]} {best[2]} (r={best[0]:.3f}; next {scores[1][1]} {scores[1][2]} r={scores[1][0]:.3f})")
