"""Which of two near-identical texture packs a material-swatches capture shows, by brightness: the
capture's mean display value per channel against the display each pack's colour map predicts under
the flat-ambient run (radiance 1.98 · 0.7² · c², the tone curve, the √ encode).

    python tools/analysis/swatch_brightness.py <material>=<pack id>[,<pack id>...] ...
"""

import os
import sys

import numpy as np
from PIL import Image

from tone import display

PACKS = os.path.join(os.environ.get("ROR_STATE_DIR") or ".cache", "material-packs")


for arg in sys.argv[1:]:
	material, _, packs = arg.partition("=")
	capture = np.asarray(Image.open(f"out/calibrate/material-swatches/{material}-flat/studio.png").convert("RGB"), dtype=np.float64)
	h, w, _ = capture.shape
	centre = capture[h // 2 - 300 : h // 2 + 300, w // 2 - 400 : w // 2 + 400].reshape(-1, 3)
	line = f"{material:14s} studio {np.round(centre.mean(axis=0), 1)}"
	for pack in packs.split(","):
		texel = np.asarray(Image.open(os.path.join(PACKS, f"{pack}_color.png")).convert("RGB"), dtype=np.float64) / 255.0
		predicted = display(1.98 * 0.49 * texel ** 2).reshape(-1, 3).mean(axis=0)
		line += f"   {pack} {np.round(predicted, 1)}"
	print(line)
