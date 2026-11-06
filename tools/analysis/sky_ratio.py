"""Studio / ror radiance per probe and channel in a calibration run's samples: both display values
taken back through the sRGB-style square and the tone curve, so a ratio is a ratio of light.

    python tools/analysis/sky_ratio.py out/calibrate/sky-light [--runs a-d1-s0,b-d1-s0] [--min 20]
"""

import json
import os
import sys

import numpy as np

from tone import radiance


def main():
	args = sys.argv[1:]
	runs = None
	floor = 20.0
	if "--runs" in args:
		index = args.index("--runs")
		runs = set(args[index + 1].split(","))
		del args[index : index + 2]
	if "--min" in args:
		index = args.index("--min")
		floor = float(args[index + 1])
		del args[index : index + 2]
	data = json.load(open(os.path.join(args[0], "samples.json")))
	for run in data:
		if runs and run["label"] not in runs:
			continue
		print(f"== {run['label']}")
		for sample in run["samples"]:
			studio = np.asarray(sample["studio"], dtype=float)
			ror = np.asarray(sample["ror"], dtype=float)
			if studio.max() < floor:
				continue
			ratio = radiance(studio) / np.maximum(radiance(ror), 1e-6)
			normal = sample.get("inputs", {}).get("normal")
			print(f"{sample['probe']:22s} n={normal}  studio {np.round(studio, 1)}  ror {np.round(ror, 1)}  studio/ror {np.round(ratio, 3)}")


if __name__ == "__main__":
	main()
