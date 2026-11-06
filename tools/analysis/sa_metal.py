"""SurfaceAppearance metalness / roughness levels (experiment `sa-metal`): Studio against ror at each
head's probes, per lighting run, as display values and as the ratio of radiances (through the
inverse tone curve).

    python tools/analysis/sa_metal.py [out/calibrate/sa-metal]
"""

import json
import sys

import numpy as np

from tone import radiance


def main():
	root = sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/sa-metal"
	runs = json.load(open(root + "/samples.json"))
	for run in runs:
		print(f"== {run['label']}")
		rows = {}
		for sample in run["samples"]:
			metal = sample["inputs"]["metalness"]
			rough = sample["inputs"]["roughness"]
			where = sample["probe"].rsplit("-", 1)[1]
			studio = np.mean(sample["studio"])
			ror = np.mean(sample["ror"])
			ratio = radiance(studio) / max(radiance(ror), 1e-6)
			rows.setdefault((rough, where), []).append((metal, studio, ror, ratio))
		for (rough, where), values in sorted(rows.items()):
			cells = "  ".join(f"m{metal:3d} {studio:5.1f}/{ror:5.1f} ({ratio:4.2f})" for metal, studio, ror, ratio in sorted(values))
			print(f"r{rough:3d} {where:6s} {cells}")


main()
