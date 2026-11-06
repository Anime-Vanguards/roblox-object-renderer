"""Contact sheet of the material-swatches captures (Studio, centre crops) for identifying which
texture pack each material uses.

    python tools/analysis/swatch_sheet.py [flat|sun] [out.png]
"""

import os
import sys

from PIL import Image, ImageDraw

ROOT = "out/calibrate/material-swatches"
lighting = sys.argv[1] if len(sys.argv) > 1 else "flat"
out = sys.argv[2] if len(sys.argv) > 2 else f".cache/swatches-{lighting}.png"

runs = sorted(
	(name for name in os.listdir(ROOT) if name.endswith("-" + lighting)),
	key=lambda name: name.lower(),
)
cell = 220
cols = 8
rows = (len(runs) + cols - 1) // cols
sheet = Image.new("RGB", (cols * cell, rows * (cell + 16)), (40, 40, 40))
draw = ImageDraw.Draw(sheet)
for index, run in enumerate(runs):
	image = Image.open(os.path.join(ROOT, run, "studio.png")).convert("RGB")
	w, h = image.size
	side = min(w, h)
	crop = image.crop(((w - side) // 2, (h - side) // 2, (w + side) // 2, (h + side) // 2)).resize((cell - 4, cell - 4), Image.LANCZOS)
	x, y = (index % cols) * cell, (index // cols) * (cell + 16)
	sheet.paste(crop, (x + 2, y + 2))
	draw.text((x + 4, y + cell - 2), run[: -len(lighting) - 1], fill=(255, 255, 255))
sheet.save(out)
print(out, len(runs), "runs")
