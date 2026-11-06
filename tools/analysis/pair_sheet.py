"""A contact sheet of a calibration experiment's runs: Studio | ror per run, centre-cropped and
labelled, for eyeballing geometry.

    python tools/analysis/pair_sheet.py <experiment> [run filter] [crop fraction, default 0.6] [columns, default 3]

Writes out/calibrate/<experiment>/pairs.png.
"""

import os
import sys

from PIL import Image, ImageDraw

experiment = sys.argv[1]
needle = sys.argv[2] if len(sys.argv) > 2 else ""
crop = float(sys.argv[3]) if len(sys.argv) > 3 else 0.6
columns = int(sys.argv[4]) if len(sys.argv) > 4 else 3
root = os.path.join("out/calibrate", experiment)
rows = []
for run in sorted(os.listdir(root)):
	directory = os.path.join(root, run)
	if not os.path.isdir(directory) or needle not in run:
		continue
	studio = Image.open(os.path.join(directory, "studio.png")).convert("RGB")
	ror = Image.open(os.path.join(directory, "ror.png")).convert("RGB")
	w, h = studio.size
	cw, ch = int(w * crop), int(h * crop)
	box = ((w - cw) // 2, (h - ch) // 2, (w + cw) // 2, (h + ch) // 2)
	tile_w = 300
	tile_h = int(tile_w * ch / cw)
	a = studio.crop(box).resize((tile_w, tile_h))
	b = ror.crop(box).resize((tile_w, tile_h))
	row = Image.new("RGB", (tile_w * 2 + 4, tile_h + 16), "white")
	row.paste(a, (0, 16))
	row.paste(b, (tile_w + 4, 16))
	ImageDraw.Draw(row).text((4, 2), f"{run} (studio | ror)", fill="black")
	rows.append(row)
tw, th = rows[0].size
sheet = Image.new("RGB", ((tw + 8) * columns, th * ((len(rows) + columns - 1) // columns)), "white")
for index, row in enumerate(rows):
	sheet.paste(row, ((index % columns) * (tw + 8), (index // columns) * th))
sheet.save(os.path.join(root, "pairs.png"))
print(os.path.join(root, "pairs.png"))
