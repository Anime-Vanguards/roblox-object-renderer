"""Ranks a compare's parts by how many of their pixels differ from Studio: renders the same view with
`--ids` (part ids) and attributes every pixel's error to the part in front.

    python tools/analysis/part_errors.py <model> <compare dir> [--top 15] [--threshold 8] [--select <path>]

The compare dir is `ror compare`'s output (studio.png, ror.png, report.json with the camera).
"""

import json
import os
import subprocess
import sys

import numpy as np
from PIL import Image

ROR = os.path.join("target", "release", "ror.exe")


def main():
	args = sys.argv[1:]
	top = int(args[args.index("--top") + 1]) if "--top" in args else 15
	threshold = float(args[args.index("--threshold") + 1]) if "--threshold" in args else 8
	select = ["--select", args[args.index("--select") + 1]] if "--select" in args else []
	model, compare = args[0], args[1]
	report = json.load(open(os.path.join(compare, "report.json")))
	view = report["views"][0]
	c = view["camera"]
	eye = c[:3]
	look = [-c[5], -c[8], -c[11]]
	camera = {"mode": "lookAt", "eye": eye, "target": [e + 10 * d for e, d in zip(eye, look)], "fov": view["fov"]}
	out = os.path.join(".cache", "part-errors", os.path.basename(os.path.normpath(compare)))
	subprocess.run([ROR, "render", model, *select, "--size", f"{view['width']}x{view['height']}", "--camera", json.dumps(camera), "--ids", "--out", out], check=True, capture_output=True)
	ids = np.asarray(Image.open(os.path.join(out, "render.ids.png")).convert("RGB")).astype(np.int64)
	ids = ids[..., 0] + 256 * ids[..., 1] + 65536 * ids[..., 2]
	parts = json.load(open(os.path.join(out, "parts.json")))
	studio = np.asarray(Image.open(os.path.join(compare, "studio.png")).convert("RGB"), dtype=float)
	ror = np.asarray(Image.open(os.path.join(compare, "ror.png")).convert("RGB"), dtype=float)
	bad = np.abs(studio - ror).max(axis=2) > threshold
	print(f"{bad.sum()} pixels off by more than {threshold} ({bad.mean() * 100:.1f} %)")
	rows = []
	for part in np.unique(ids):
		mask = ids == part
		name = "(background)" if part == 0 else f"#{part - 1} {parts[part - 1]['path']} {parts[part - 1]['class']} material {parts[part - 1]['material']}{' SA' if parts[part - 1]['surfaceAppearance'] else ''}"
		rows.append((bad[mask].sum(), mask.sum(), (ror - studio)[mask].mean(axis=0), name))
	rows.sort(key=lambda row: -row[0])
	for count, total, signed, name in rows[:top]:
		print(f"{count:7d} of {total:7d} ({count / total * 100:5.1f} %) signed {np.round(signed, 1)}  {name}")


main()
