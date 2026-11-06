"""Terrain Clouds against the clear sky: per `clouds` run, the share of the sky clouded (a pixel more
than 24 of 255 off the same view's run without clouds) in bands of rows, and the clouded pixels'
mean colour, for Studio and for ror.

python tools/analysis/clouds.py [out/calibrate/clouds]
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image

BANDS = 6


def load(path: Path) -> np.ndarray:
	return np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)


def measure(image: np.ndarray, clear: np.ndarray) -> tuple[list[float], np.ndarray, float]:
	off = np.abs(image - clear).sum(axis=2) > 24
	rows = np.array_split(off, BANDS, axis=0)
	shares = [float(band.mean()) for band in rows]
	colour = image[off].mean(axis=0) if off.any() else np.zeros(3)
	return shares, colour, float(off.mean())


def main() -> None:
	root = Path(sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/clouds")
	runs = sorted(path.name for path in root.iterdir() if (path / "studio.png").exists())
	for run in runs:
		label, view = run.rsplit("-", 1)
		clear_label = "lobby-none" if label.startswith("lobby") else "noon" if "noon" in label else "none"
		if label == clear_label:
			continue
		clear_dir = root / f"{clear_label}-{view}"
		for side in ("studio", "ror"):
			image_path = root / run / f"{side}.png"
			clear_path = clear_dir / f"{side}.png"
			if not image_path.exists() or not clear_path.exists():
				continue
			shares, colour, total = measure(load(image_path), load(clear_path))
			bands = " ".join(f"{share * 100:5.1f}" for share in shares)
			print(f"{run:20} {side:6} {total * 100:5.1f} %  bands top->bottom {bands}  colour {colour.round(0)}")


if __name__ == "__main__":
	main()
