"""Specular error for SmoothPlastic fits: the black sphere of every specular-spheres view and the
sun3 backdrop of neon-rows (black SmoothPlastic under the sun), Studio vs ror, in radiance.

    python tools/analysis/spec_error.py
"""

import math

import numpy as np
from PIL import Image

from tone import radiance


def sphere_mask(view, index, row):
	H = (14.5 - 12) / 24 * 2 * math.pi
	t = math.radians(-23.5)
	sun = np.array([-math.sin(H) * math.cos(t), math.cos(H) * math.cos(t), math.sin(t)])
	a = math.radians(view)
	c, s = math.cos(a), math.sin(a)
	direction = np.array([c * sun[0] + s * sun[2], sun[1], -s * sun[0] + c * sun[2]])
	direction /= np.linalg.norm(direction)
	across = np.cross(direction, [0, 1, 0])
	across /= np.linalg.norm(across)
	origin = np.array([0, row * 40.0, 0])
	eye = origin + direction * 120
	center = origin + across * ((index - 3.5) * 6)
	f = origin - eye
	f /= np.linalg.norm(f)
	r = np.cross(f, [0, 1, 0])
	r /= np.linalg.norm(r)
	u = np.cross(r, f)
	d = center - eye
	x, y, z = d @ r, d @ u, d @ f
	th = math.tan(math.radians(13))
	px, py = (x / (z * th * 1644 / 1116) * 0.5 + 0.5) * 1644, (0.5 - y / (z * th) * 0.5) * 1116
	radius_px = 2.5 / (z * th) * 0.5 * 1116 * 0.9
	yy, xx = np.mgrid[0:1116, 0:1644]
	return (xx - px) ** 2 + (yy - py) ** 2 < radius_px**2


errors = []
for row, view in enumerate([10, 40, 80]):
	studio = radiance(np.asarray(Image.open(f"out/calibrate/specular-spheres/view{view}/studio.png").convert("L")).astype(float))
	ror = radiance(np.asarray(Image.open(f"out/calibrate/specular-spheres/view{view}/ror.png").convert("L")).astype(float))
	mask = sphere_mask(view, 0, row)
	errors.append(np.mean(np.abs(studio[mask] - ror[mask])))
	print(f"black sphere view {view}: mean |x| error {errors[-1]:.4f}  studio mean {studio[mask].mean():.4f} ror mean {ror[mask].mean():.4f}")
studio = radiance(np.asarray(Image.open("out/calibrate/neon-rows/sun3/studio.png").convert("L")).astype(float))[:350]
ror = radiance(np.asarray(Image.open("out/calibrate/neon-rows/sun3/ror.png").convert("L")).astype(float))[:350]
errors.append(np.mean(np.abs(studio - ror)))
print(f"sun3 backdrop top band: mean |x| error {errors[-1]:.4f}  studio mean {studio.mean():.4f} ror mean {ror.mean():.4f}")
plastic = []
for row, view in enumerate([10, 40, 80]):
	studio = radiance(np.asarray(Image.open(f"out/calibrate/specular-spheres/view{view}/studio.png").convert("L")).astype(float))
	ror = radiance(np.asarray(Image.open(f"out/calibrate/specular-spheres/view{view}/ror.png").convert("L")).astype(float))
	mask = sphere_mask(view, 4, row)
	plastic.append(np.mean(np.abs(studio[mask] - ror[mask])))
print(f"plastic spheres: {' '.join(f'{e:.4f}' for e in plastic)} mean {np.mean(plastic):.4f}")
print(f"TOTAL {np.mean(errors):.5f}")
