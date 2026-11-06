"""How Studio turns a sky into sky light (experiment `sky-kernel`).

For each sky that is white in one image only, the white ball's light minus its light under the
all-dark sky (pixel by pixel: same camera, same normal) is the response to that image. It is
compared with the exact clamped-cosine integral of the image (the true irradiance / pi of a white
face over dark) and with ror's response; printed per image: the least-squares scale of each
against Studio, the rms residual after that scale, and Studio / exact at a few normals.

    python tools/analysis/sky_kernel.py [out/calibrate/sky-kernel]
"""

import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from sky_sh_fit import ball_pixels  # noqa: E402

# Back, Down, Front, Left, Right, Up (Sky.Skybox*) and the direction each one covers
FACES = {"bk": (0, 0, 1), "dn": (0, -1, 0), "ft": (0, 0, -1), "lf": (1, 0, 0), "rt": (-1, 0, 0), "up": (0, 1, 0)}
VIEWS = ["a", "b", "c", "d"]


def face_directions(axis, resolution=96):
	"""Directions and solid angles over one cube face."""
	axis = np.array(axis, dtype=float)
	helper = np.array([0.0, 1.0, 0.0]) if abs(axis[1]) < 0.5 else np.array([1.0, 0.0, 0.0])
	u_axis = np.cross(helper, axis)
	v_axis = np.cross(axis, u_axis)
	steps = (np.arange(resolution) + 0.5) / resolution * 2 - 1
	u, v = np.meshgrid(steps, steps)
	points = axis + u[..., None] * u_axis + v[..., None] * v_axis
	length = np.linalg.norm(points, axis=-1)
	weights = (2.0 / resolution) ** 2 / length**3
	return (points / length[..., None]).reshape(-1, 3), weights.reshape(-1)


def clamped_cosine(normals, axis):
	"""Irradiance / pi of a unit-radiance face over black: (1 / pi) integral of max(0, n . w) dw."""
	directions, weights = face_directions(axis)
	result = np.empty(len(normals))
	for start in range(0, len(normals), 4096):
		chunk = normals[start : start + 4096]
		result[start : start + 4096] = (np.maximum(chunk @ directions.T, 0.0) * weights).sum(axis=1) / np.pi
	return result


def gather(root, data, sky):
	normals, studio, ror = [], [], []
	for view in VIEWS:
		label = f"{sky}-{view}"
		gray, n, s, r = ball_pixels(data[label], os.path.join(root, label))[0]
		normals.append(n)
		studio.append(s)
		ror.append(r)
	return np.concatenate(normals), np.concatenate(studio), np.concatenate(ror)


def main():
	root = sys.argv[1] if len(sys.argv) > 1 else "out/calibrate/sky-kernel"
	data = {run["label"]: run for run in json.load(open(os.path.join(root, "samples.json")))}
	normals, dark_studio, dark_ror = gather(root, data, "dark")
	_, white_studio, white_ror = gather(root, data, "white")
	print(f"white - dark: studio {np.round((white_studio - dark_studio).mean(0), 4)}  ror {np.round((white_ror - dark_ror).mean(0), 4)}")
	step = max(1, len(normals) // 40000)
	pick = slice(None, None, step)
	total_exact = np.zeros(len(normals[pick]))
	total_studio = np.zeros(len(normals[pick]))
	for face, axis in FACES.items():
		face_normals, studio, ror = gather(root, data, face)
		assert np.allclose(face_normals, normals)
		delta_studio = (studio - dark_studio).mean(axis=1)[pick]
		delta_ror = (ror - dark_ror).mean(axis=1)[pick]
		exact = clamped_cosine(normals[pick], axis)
		total_exact += exact
		total_studio += delta_studio
		lines = []
		for name, model in [("exact", exact), ("ror", delta_ror)]:
			scale = (model * delta_studio).sum() / (model * model).sum()
			rms = np.sqrt(np.mean((scale * model - delta_studio) ** 2))
			lines.append(f"{name} x{scale:.3f} rms {rms:.4f}")
		probes = []
		for label, direction in [("axis", axis), ("side", np.cross(axis, (1, 0, 0)) if abs(axis[0]) < 0.5 else (0, 1, 0)), ("away", -np.array(axis))]:
			direction = np.array(direction, dtype=float)
			direction /= np.linalg.norm(direction)
			near = (normals[pick] @ direction) > 0.97
			if near.any():
				probes.append(f"{label} studio {delta_studio[near].mean():.4f} exact {exact[near].mean():.4f} ror {delta_ror[near].mean():.4f}")
		print(f"{face}: {'; '.join(lines)}\n    {' | '.join(probes)}")
	scale = (total_exact * total_studio).sum() / (total_exact * total_exact).sum()
	print(f"all six faces: studio / exact x{scale:.3f}, rms {np.sqrt(np.mean((scale * total_exact - total_studio) ** 2)):.4f}")


main()
