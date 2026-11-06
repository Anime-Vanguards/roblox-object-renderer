"""Studio's sun shadow filter against cascade and ShadowSoftness (notes/render.md "Shadows";
render::SUN_FILTER): fits sigma = t · √(sb² + base) with t = 2 R / texels the cascade's texel and
sb = clamp(A · √(softness / t), smin, smax) texels to the edge sigmas measured (Gaussian-CDF +
over-darkening fits of `shadow_profiles.py --fit`, shadow-softness groups 1 and 2; 10-90 % widths
/ 2.05 where only widths were measured: shadow-cascades, -cascade-map, -cascade-down).

    python tools/analysis/shadow_kernel_fit.py
"""

import numpy as np
from scipy.optimize import least_squares

ASPECT = 1644 / 1116
SPLITS = [19.6, 45.3, 131.8]


def radius(cascade, fov):
	"""The cascade's sphere: centred at the slice's far depth, through its far or near corners."""
	corner = np.tan(np.radians(fov) / 2) * np.sqrt(1 + ASPECT ** 2)
	far = SPLITS[cascade]
	near = SPLITS[cascade - 1] if cascade > 0 else 0.0
	return max(far * corner, np.hypot(far - near, near * corner))


# (cascade, field of view, ShadowSoftness, sigma in studs, weight)
DATA = []
for softness, sigma in [(0, 0.046), (0.05, 0.087), (0.1, 0.118), (0.2, 0.159), (0.3, 0.184), (0.5, 0.227), (0.75, 0.227), (1, 0.227)]:
	DATA.append((1, 30, softness, sigma, 1.0))
for softness, sigma in [(0, 0.123), (0.05, 0.123), (0.1, 0.193), (0.2, 0.233), (0.3, 0.320), (0.5, 0.415), (0.75, 0.477), (1, 0.541)]:
	DATA.append((2, 30, softness, sigma, 1.0))
for fov in (30, 45, 60):
	for softness, width in [(0, 0.146), (0.2, 0.402), (1, 0.666)]:
		DATA.append((0, fov, softness, width / 2.05, 0.5))
for softness, width in [(0, 0.175), (0.2, 0.448), (1, 0.80)]:
	DATA.append((1, 60, softness, width / 2.05, 1.0))
for softness, width in [(0, 0.40), (0.2, 0.69), (1, 1.33)]:
	DATA.append((2, 60, softness, width / 2.05, 1.0))
for cascade, fov, width in [(1, 45, 0.123), (1, 70, 0.207), (0, 70, 0.183), (2, 45, 0.30), (2, 70, 0.475)]:
	DATA.append((cascade, fov, 0, width / 2.05, 1.0))
DATA = np.array(DATA)
TEXELS = [512, 1024, 1280]
BASE = 0.245


def model(params):
	scale, least, most = params
	out = []
	for cascade, fov, softness, _, _ in DATA:
		texel = 2 * radius(int(cascade), fov) / TEXELS[int(cascade)]
		filter_sigma = np.clip(scale * np.sqrt(softness / texel), least, most)
		out.append(texel * np.sqrt(filter_sigma ** 2 + BASE))
	return np.array(out)


def main():
	fit = least_squares(lambda p: (np.log(model(p)) - np.log(DATA[:, 3])) * DATA[:, 4], [1.5, 0.7, 4.2])
	scale, least, most = fit.x
	print(f"texels {TEXELS}: sigma = clamp({scale:.3f} sqrt(softness / texel), {least:.3f}, {most:.3f}) texels, rms log {np.sqrt(np.mean(fit.fun ** 2)):.3f}")
	for row, value in zip(DATA, model(fit.x)):
		cascade, fov, softness, sigma, _ = row
		print(f"  cascade {int(cascade)} fov {int(fov)} softness {softness:<5} measured {sigma:.3f} model {value:.3f} ({(value / sigma - 1) * 100:+.0f} %)")


if __name__ == "__main__":
	main()
