"""diff.png and side-by-side.png of comparison runs, drawn again from their studio.png and ror.png as
ror draws them (`studio/compare.rs` write_comparison: diff |Studio - ror| x 4 per channel, opaque; the
strip Studio | ror | diff). Calibration runs write only studio.png and ror.png unless
ROR_DIFF_IMAGES=1, and `ror clean --older-than` removes old diff and side-by-side images.

    python tools/analysis/side_by_side.py <experiment or folder>...   # e.g. material-swatches, out/compare/r15

An experiment name means out/calibrate/<name>; every folder below holding studio.png and ror.png is a
run. Pixels match ror's own images exactly (the PNG encoding differs).
"""

import os
import sys

import numpy as np
from PIL import Image


def runs(target):
	root = target if os.path.isdir(target) else os.path.join("out", "calibrate", target)
	if not os.path.isdir(root):
		raise SystemExit(f"no folder {target} (nor out/calibrate/{target})")
	for directory, _, files in os.walk(root):
		if "studio.png" in files and "ror.png" in files:
			yield directory


def save(pixels, path):
	"""Writes through a temporary name and a rename (files under out/ may be hard links)."""
	temporary = f"{path}.{os.getpid()}.tmp"
	Image.fromarray(pixels, "RGBA").save(temporary, format="PNG")
	os.replace(temporary, path)


def draw(directory):
	studio = np.asarray(Image.open(os.path.join(directory, "studio.png")).convert("RGBA"))
	ror = np.asarray(Image.open(os.path.join(directory, "ror.png")).convert("RGBA"))
	if studio.shape != ror.shape:
		print(f"{directory}: studio.png {studio.shape[1]}x{studio.shape[0]} and ror.png {ror.shape[1]}x{ror.shape[0]} differ, skipped")
		return False
	diff = np.empty_like(studio)
	diff[..., :3] = np.minimum(np.abs(studio[..., :3].astype(np.int16) - ror[..., :3]) * 4, 255)
	diff[..., 3] = 255
	save(diff, os.path.join(directory, "diff.png"))
	save(np.concatenate([studio, ror, diff], axis=1), os.path.join(directory, "side-by-side.png"))
	return True


def main():
	if len(sys.argv) < 2:
		print(__doc__)
		return
	drawn = sum(draw(directory) for target in sys.argv[1:] for directory in runs(target))
	print(f"{drawn} runs drawn")


if __name__ == "__main__":
	main()
