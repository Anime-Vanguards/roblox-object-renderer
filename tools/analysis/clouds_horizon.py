"""Terrain Clouds towards the horizon: per `clouds-horizon` run, Studio's and ror's clouded share by
bands of rows (a pixel more than 24 of 255 off the same view without clouds), the clouded pixels'
mean colour and brightness at the 10th / 50th / 90th percentile, how sharp they are (mean gradient
of the brightness, of 255 a pixel) and how streaky (the horizontal gradient's share against the
vertical's: streaks along the horizon change across rows), and the lowest row band still clouded.
Each view's figures are averaged over its four azimuths (the clouds are random, their statistics
are not).

python tools/analysis/clouds_horizon.py [out/calibrate/clouds-horizon]
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image

BANDS = 8


def load(path: Path) -> np.ndarray:
	return np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)


def measure(image: np.ndarray, clear: np.ndarray) -> dict:
	off = np.abs(image - clear).sum(axis=2) > 24
	luma = image.mean(axis=2)
	clouded = image[off] if off.sum() > 20 else image.reshape(-1, 3)
	gy, gx = np.abs(np.diff(luma, axis=0))[:, :-1], np.abs(np.diff(luma, axis=1))[:-1, :]
	region = off[:-1, :-1]
	return {
		"bands": np.array([band.mean() for band in np.array_split(off, BANDS, axis=0)]),
		"colour": clouded.mean(axis=0),
		"percentiles": np.percentile(luma[off] if off.sum() > 20 else luma, [10, 50, 90]),
		"sharp": float((gx + gy)[region].mean()) if region.any() else 0.0,
		"streak": float(gy[region].mean() / max(gx[region].mean(), 1e-3)) if region.any() else 0.0,
	}


def summary(root: Path) -> dict:
	"""{(label, view): {side: averaged measures}}"""
	runs = sorted(path.name for path in root.iterdir() if (path / "studio.png").exists())
	groups: dict = {}
	for run in runs:
		label, view = run.rsplit("-", 1)
		if label.endswith("none"):
			continue
		clear = root / f"{label.split('-')[0]}-none-{view}"
		for side in ("studio", "ror"):
			if not (root / run / f"{side}.png").exists():
				continue
			measures = measure(load(root / run / f"{side}.png"), load(clear / f"{side}.png"))
			groups.setdefault((label, view[0]), {}).setdefault(side, []).append(measures)
	out = {}
	for key, sides in groups.items():
		out[key] = {side: {name: np.mean([m[name] for m in items], axis=0) for name in items[0]} for side, items in sides.items()}
	return out


def main() -> None:
	root = Path(sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/clouds-horizon")
	for (label, view), sides in sorted(summary(root).items()):
		for side, m in sides.items():
			bands = " ".join(f"{share * 100:4.0f}" for share in m["bands"])
			print(f"{label:12} {view} {side:6} bands {bands}  colour {m['colour'].round(0)}  p10/50/90 {m['percentiles'].round(0)}  sharp {m['sharp']:5.2f}  streak {m['streak']:4.2f}")


if __name__ == "__main__":
	main()
