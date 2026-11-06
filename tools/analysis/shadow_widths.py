"""Penumbra widths (10-90 %, studs on the floor) of the shadow-steps calibration, Studio vs ror,
for the walls of height 4 and 8 (the lower walls' probe lines cross the wall itself).

    python tools/analysis/shadow_widths.py
"""

import json

import numpy as np


def width(offsets, values):
	v = np.array(values, dtype=float)
	low, high = v.min(), v.max()
	if high - low < 20:
		return float("nan")
	t = np.maximum.accumulate((v - low) / (high - low))
	return np.interp(0.9, t, offsets) - np.interp(0.1, t, offsets)


runs = json.load(open("out/calibrate/shadow-steps/samples.json"))
errors = []
for run in runs:
	line = f"{run['label']:16}"
	for height in (4.0, 8.0):
		samples = sorted((s for s in run["samples"] if s["inputs"]["height"] == height), key=lambda s: s["inputs"]["offset"])
		offsets = [s["inputs"]["offset"] for s in samples]
		studio = width(offsets, [s["studio"][0] for s in samples])
		ror = width(offsets, [s["ror"][0] for s in samples])
		errors.append(ror - studio)
		line += f"  h{height:.0f}: studio {studio:5.3f} ror {ror:5.3f}"
	print(line)
errors = np.array(errors)
print(f"width error: mean {errors.mean():+.3f} abs {np.abs(errors).mean():.3f} max {np.abs(errors).max():.3f}")
