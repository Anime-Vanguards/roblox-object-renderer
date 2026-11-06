"""Studio's sky light as an ambient cube: E(n) = sum over axes of n_i^2 C(sign(n_i) e_i), six values
per channel (the light facing +x, -x, +y, -y, +z, -z), fitted to the balls of `sky-kernel` (or
`sky-light`) per sky, against the 9-polynomial (L2 spherical harmonics) fit of the same pixels.

    python tools/analysis/sky_cube.py [out/calibrate/sky-kernel] [sky ...]
"""

import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from sky_sh_fit import ball_pixels, basis  # noqa: E402

AXES = ["+x", "-x", "+y", "-y", "+z", "-z"]


def cube_basis(n):
	columns = []
	for axis in range(3):
		component = n[:, axis]
		columns.append(np.where(component > 0, component * component, 0.0))
		columns.append(np.where(component < 0, component * component, 0.0))
	return np.stack(columns, axis=1)


def gather(root, data, sky, views):
	normals, studio, ror = [], [], []
	for view in views:
		label = f"{sky}-{view}" if view else sky
		if label not in data:
			continue
		_, n, s, r = ball_pixels(data[label], os.path.join(root, label))[0]
		normals.append(n)
		studio.append(s)
		ror.append(r)
	return np.concatenate(normals), np.concatenate(studio), np.concatenate(ror)


def main():
	args = sys.argv[1:]
	root = args[0] if args else "out/calibrate/sky-kernel"
	data = {run["label"]: run for run in json.load(open(os.path.join(root, "samples.json")))}
	skies = args[1:] or ["none", "white", "dark", "bk", "dn", "ft", "lf", "rt", "up"]
	for sky in skies:
		normals, studio, ror = gather(root, data, sky, ["a", "b", "c", "d"])
		cube = cube_basis(normals)
		quadratic = basis(normals)
		fit_cube, *_ = np.linalg.lstsq(cube, studio, rcond=None)
		fit_quadratic, *_ = np.linalg.lstsq(quadratic, studio, rcond=None)
		rms_cube = np.sqrt(np.mean((cube @ fit_cube - studio) ** 2, axis=0))
		rms_quadratic = np.sqrt(np.mean((quadratic @ fit_quadratic - studio) ** 2, axis=0))
		ror_cube, *_ = np.linalg.lstsq(cube, ror, rcond=None)
		print(f"== {sky}: rms ambient cube {np.round(rms_cube, 4)}  quadratic {np.round(rms_quadratic, 4)}")
		for index, axis in enumerate(AXES):
			print(f"  {axis}: studio {np.round(fit_cube[index], 4)}  (ror's light there {np.round(ror_cube[index], 4)})")


if __name__ == "__main__":
	main()
