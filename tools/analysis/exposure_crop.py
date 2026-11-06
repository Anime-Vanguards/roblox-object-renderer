"""A long exposure's whole average around its effect, enlarged: Studio | ror | the signed difference of
what the effect adds (128 is equal, 2x gain), over the effect's footprint's bounding box (or a given
box), each pixel `zoom` times (out/exposures/<experiment>-crop.png).

    python tools/analysis/exposure_crop.py <experiment> [x0,y0,x1,y1] [--zoom N] [--window K]
"""

import os
import sys

import numpy as np
from PIL import Image as Picture

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from exposure_metrics import THRESHOLD, Exposure, blend  # noqa: E402


def main():
	args = sys.argv[1:]
	if not args:
		print(__doc__)
		return
	exposure = Exposure(args[0])
	zoom = int(args[args.index("--zoom") + 1]) if "--zoom" in args else 0
	wanted = int(args[args.index("--window") + 1]) if "--window" in args else None
	box = next((tuple(int(v) for v in arg.split(",")) for arg in args[1:] if arg.count(",") == 3), None)
	for window, images, counts in exposure.slices():
		if window != wanted:
			continue
		s = blend([(images[("studio", group)], counts[group]) for group in range(exposure.groups)])
		r = blend([(images[("ror", group)], counts[group]) for group in range(exposure.groups)])
		sb = images.get(("studio", None)) or images[("ror", None)]
		rb = images[("ror", None)]
		if box is None:
			footprint = (np.abs(s.shown - sb.shown).max(axis=2) > THRESHOLD) | (np.abs(r.shown - rb.shown).max(axis=2) > THRESHOLD)
			ys, xs = np.nonzero(footprint)
			if not len(xs):
				print("no footprint")
				return
			margin = max(8, int(0.15 * max(xs.max() - xs.min(), ys.max() - ys.min())))
			box = (max(xs.min() - margin, 0), max(ys.min() - margin, 0), min(xs.max() + margin, s.shown.shape[1]), min(ys.max() + margin, s.shown.shape[0]))
		x0, y0, x1, y1 = box
		delta = (r.shown - rb.shown) - (s.shown - sb.shown)
		tiles = [np.round(s.shown), np.round(r.shown), 128 + 2 * delta]
		crops = [np.clip(tile[y0:y1, x0:x1], 0, 255).astype(np.uint8) for tile in tiles]
		scale = zoom or max(1, min(8, 600 // max(x1 - x0, y1 - y0)))
		pictures = [Picture.fromarray(crop).resize(((x1 - x0) * scale, (y1 - y0) * scale), Picture.NEAREST) for crop in crops]
		sheet = Picture.new("RGB", (pictures[0].width * 3, pictures[0].height))
		for index, picture in enumerate(pictures):
			sheet.paste(picture, (index * pictures[0].width, 0))
		os.makedirs(os.path.join("out", "exposures"), exist_ok=True)
		path = os.path.join("out", "exposures", exposure.name.removeprefix("exposure-") + ("" if wanted is None else f"-w{wanted}") + "-crop.png")
		sheet.save(path)
		print(f"{path}: box {box}, x{scale}")


if __name__ == "__main__":
	main()
