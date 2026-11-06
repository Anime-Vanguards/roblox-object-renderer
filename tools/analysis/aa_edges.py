"""Anti-aliasing (experiment `aa-edges`): black bars on a white wall. Edge pixels hold the bar's
coverage, averaged in some space; this prints the distribution of edge values in Studio and ror and
how they cluster as coverage under three resolve spaces: display values, linear radiance, and √
radiance (ror's scene target).

    python tools/analysis/aa_edges.py [out/calibrate/aa-edges/flat]
"""

import sys

import numpy as np
from PIL import Image

from tone import radiance


def main():
	run = sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/aa-edges/flat"
	for name in ("studio", "ror"):
		image = np.asarray(Image.open(f"{run}/{name}.png").convert("L"), dtype=float)
		h, w = image.shape
		region = image[h // 6 : 5 * h // 6, w // 8 : 7 * w // 8]
		white = np.percentile(region, 99)
		edge = region[(region > 3) & (region < white - 3)]
		values, counts = np.unique(np.round(edge).astype(int), return_counts=True)
		print(f"== {name}: wall {white:.0f}, {edge.size} edge pixels")
		top = np.argsort(-counts)[:16]
		print("   most common edge values:", sorted((int(values[i]), int(counts[i])) for i in top))
		coverage = {
			"display": edge / white,
			"linear": radiance(edge) / radiance(white),
			"sqrt": np.sqrt(radiance(edge) / radiance(white)),
		}
		for space, c in coverage.items():
			histogram, _ = np.histogram(c, bins=16, range=(0, 1))
			print(f"   coverage if averaged in {space:7s}: {histogram.tolist()}")


main()
