"""What lights Terrain Clouds: per `clouds-light` run, the sky's mean colour (the veil at Cover 0.9 covers
nearly all of it) and the clouded pixels' (more than 24 of 255 off the same view's run without clouds),
Studio's beside ror's.

python tools/analysis/clouds_light.py [out/calibrate/clouds-light]
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image


def load(path: Path) -> np.ndarray:
	return np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)


def clear_of(label: str) -> str:
	if label.startswith("cutscene"):
		return "cutscene-none"
	if label.startswith("dark"):
		return "dark-none"
	return "day-none"


def main() -> None:
	root = Path(sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/clouds-light")
	runs = sorted(path.name for path in root.iterdir() if (path / "studio.png").exists())
	for run in runs:
		label, view = run.rsplit("-", 1)
		clear_dir = root / f"{clear_of(label)}-{view}"
		cells = []
		for side in ("studio", "ror"):
			image = load(root / run / f"{side}.png")
			mean = image.reshape(-1, 3).mean(axis=0)
			clear_path = clear_dir / f"{side}.png"
			clouded = ""
			if clear_path.exists() and clear_dir.name != run:
				off = np.abs(image - load(clear_path)).sum(axis=2) > 24
				colour = image[off].mean(axis=0) if off.any() else np.zeros(3)
				clouded = f" clouds {off.mean() * 100:5.1f} % {colour.round(0)}"
			cells.append(f"{side} {mean.round(0)}{clouded}")
		print(f"{run:30} " + "   ".join(cells))


if __name__ == "__main__":
	main()
