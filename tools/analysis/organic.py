"""The Organic pattern (notes/materials.md "Organic pattern") on flat, head-on captures of a white
part under flat ambient light (material-swatches, organic-variant, organic-slopes).

Model: k = noise.dds (Studio's 32 x 32 gray noise, bilinear, wrapping) at 0.03 t + P, t the face's
material coordinate in tiles; level i = floor(8 k), f = fract(8 k); the maps are read at
t + C + (sin 3i, sin 7i) and at the same for i + 1, cross-faded linearly by f. P (noise uv) and C
(tiles) are 0 for MaterialVariants; built-in materials draw them at random per part and load.

	python tools/analysis/organic.py offsets <run dir> <pack id> [studs per tile] [--json file] [--name part] [--far]
		C, P and the noise's frequency of a top view (the material-swatches / organic-variant
		camera; --far: organic-builtin's): C by patch matching, P and the frequency by registering
		the per-pixel level (the best pair of neighbouring levels and their cross-fade) against
		the noise; --json adds them to a ROR_ORGANIC_OFFSETS file.
	python tools/analysis/organic.py blend <run dir> <pack id> <studs per tile>
		a MaterialVariant top view (P = C = 0): the cross-fade weight against fract(8 k).
	python tools/analysis/organic.py slopes <organic-slopes dir>
		each wedge's slope coordinate v = a s + b (s studs down the slope).

Needs the packs' colour maps decoded into <state dir>/material-packs (the texture_pack example;
ROR_STATE_DIR or .cache) and a Studio install (noise.dds).
"""

import glob
import json
import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import radiance

STATE = os.environ.get("ROR_STATE_DIR") or ".cache"
SWATCH_PPS = 558.0 / (59.0 * np.tan(np.radians(10.0)))  # px per stud, 59 studs away, 20 degree fov
FAR_PPS = 558.0 / (236.0 * np.tan(np.radians(10.0)))  # organic-builtin: 236 studs away
NOISE_SCALE, LEVELS = 0.03, 8.0


def srgb_to_linear(c):
	return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def load_noise():
	versions = os.path.join(os.environ.get("LOCALAPPDATA", ""), "Roblox", "Versions")
	paths = sorted(glob.glob(os.path.join(versions, "*", "content", "textures", "noise.dds")), key=os.path.getmtime)
	if not paths:
		sys.exit("noise.dds: no Studio install found")
	return np.asarray(Image.open(paths[-1]))[..., 0].astype(np.float64) / 255.0


NOISE = load_noise()


def noise_at(tu, tv, offset=(0.0, 0.0)):
	"""k at tile coordinates: bilinear, wrapping, texel x = s N - 0.5 (texel centres on integers)"""
	n = NOISE.shape[0]
	x = (tu * NOISE_SCALE + offset[0]) * n - 0.5
	y = (tv * NOISE_SCALE + offset[1]) * n - 0.5
	x0, y0 = np.floor(x), np.floor(y)
	fx, fy = x - x0, y - y0
	x0, y0 = x0.astype(int) % n, y0.astype(int) % n
	x1, y1 = (x0 + 1) % n, (y0 + 1) % n
	top = NOISE[y0, x0] * (1 - fx) + NOISE[y0, x1] * fx
	bottom = NOISE[y1, x0] * (1 - fx) + NOISE[y1, x1] * fx
	return top * (1 - fy) + bottom * fy


def color_map(pack):
	texel = np.asarray(Image.open(os.path.join(STATE, "material-packs", f"{pack}_color.png")).convert("RGB"), dtype=np.float64) / 255.0
	return srgb_to_linear(texel).astype(np.float32)


def pixel_tiles(shape, pps, center, spt, sub=1):
	h, w = shape
	ys, xs = np.mgrid[0:h:sub, 0:w:sub].astype(np.float64)
	return (center[0] + (xs + 0.5 - w / 2) / pps) / spt, (center[1] + (ys + 0.5 - h / 2) / pps) / spt


def layer(small, tu, tv, du, dv):
	"""the texture (scaled to the capture's px per tile) at tile coordinates + offset"""
	n = small.shape[0]
	mx = ((((tu + du) % 1.0) * n) - 0.5).astype(np.float32)
	my = ((((tv + dv) % 1.0) * n) - 0.5).astype(np.float32)
	return cv2.remap(small, mx, my, cv2.INTER_LINEAR, borderMode=cv2.BORDER_WRAP)


def texture_offset(gray, tex, pps, center, spt, patch=48, min_ncc=float(os.environ.get("ORGANIC_MIN_NCC", "0.75"))):
	"""C: every patch matched in the wrapped texture; its offset is C + (sin 3i, sin 7i) for some
	level i, so C takes the most votes over i (then refined to sub-pixel on the dense fit)"""
	h, w = gray.shape
	n = int(round(spt * pps))
	small = cv2.resize(tex, (n, n), interpolation=cv2.INTER_AREA)
	wrapped = np.tile(small, (2, 2))[: n + patch, : n + patch].copy()
	levels = np.arange(0, 12)
	votes = np.zeros((200, 200))
	offsets = []
	for y0 in range(0, h - patch + 1, patch):
		for x0 in range(0, w - patch + 1, patch):
			res = cv2.matchTemplate(wrapped, gray[y0 : y0 + patch, x0 : x0 + patch], cv2.TM_CCOEFF_NORMED)
			_, value, _, (mx, my) = cv2.minMaxLoc(res)
			if value < min_ncc:
				continue
			du = (mx / n - (center[0] + (x0 - w / 2) / pps) / spt) % 1.0
			dv = (my / n - (center[1] + (y0 - h / 2) / pps) / spt) % 1.0
			offsets.append((du, dv))
			cu = np.floor(((du - np.sin(3.0 * levels)) % 1.0) * 200).astype(int) % 200
			cv = np.floor(((dv - np.sin(7.0 * levels)) % 1.0) * 200).astype(int) % 200
			np.add.at(votes, (cu, cv), 1)
	smooth = sum(np.roll(np.roll(votes, a, 0), b, 1) for a in (-1, 0, 1) for b in (-1, 0, 1))
	iu, iv = np.unravel_index(np.argmax(smooth), smooth.shape)
	C = ((iu + 0.5) / 200, (iv + 0.5) / 200)
	# the share of matched patches that sit on C + (sin 3i, sin 7i) for some level
	near = lambda d: np.abs((d + 0.5) % 1.0 - 0.5)
	fits = [np.min(near(du - C[0] - np.sin(3.0 * levels)) + near(dv - C[1] - np.sin(7.0 * levels))) < 0.03 for du, dv in offsets]
	# a view inside one level's band shows one offset only, and C is that offset minus any level's
	# shift: every level is then a candidate
	single = None
	if offsets:
		first = offsets[0]
		if np.mean([near(du - first[0]) + near(dv - first[1]) < 0.03 for du, dv in offsets]) > 0.9:
			single = first
	return C, float(np.mean(fits)) if fits else 0.0, single


def level_fit(observed, small, tu, tv, C, window=7):
	"""per pixel, the pair of neighbouring levels (j, j + 1) and cross-fade w that fit the colour
	best in a small window: l = j + w; also the residual"""
	layers = [layer(small, tu, tv, C[0] + np.sin(3.0 * j), C[1] + np.sin(7.0 * j)) for j in range(10)]
	# the light's scale: the observed over the best single layer
	scale = float(np.median([np.median(observed / np.maximum(t, 1e-3)) for t in layers]))
	observed = observed / scale
	best = np.full(observed.shape[:2], np.inf, dtype=np.float32)
	level = np.zeros(observed.shape[:2], dtype=np.float32)
	for j in range(9):
		a, d = layers[j], layers[j + 1] - layers[j]
		num = cv2.blur(((observed - a) * d).sum(axis=2), (window, window))
		den = cv2.blur((d * d).sum(axis=2), (window, window))
		w = np.clip(num / np.maximum(den, 1e-6), 0.0, 1.0)
		residual = cv2.blur(((observed - a) ** 2).sum(axis=2), (window, window)) - 2 * w * num + w * w * den
		better = residual < best
		best = np.where(better, residual, best)
		level = np.where(better, j + w, level)
	return level, best, scale


def local_ncc(a, b, window):
	k = (window, window)
	ma, mb = cv2.blur(a, k), cv2.blur(b, k)
	va = cv2.blur(a * a, k) - ma * ma
	vb = cv2.blur(b * b, k) - mb * mb
	return (cv2.blur(a * b, k) - ma * mb) / np.sqrt(np.maximum(va * vb, 1e-12))


def refine_texture_offset(gray, small_gray, tu, tv, C, span, steps, window=15):
	"""C to a fraction of a pixel: the shift where the best-matching layer's local NCC is highest
	on average"""
	best = None
	for du in np.linspace(-span, span, steps):
		for dv in np.linspace(-span, span, steps):
			c = (C[0] + du, C[1] + dv)
			layers = [layer(small_gray, tu, tv, c[0] + np.sin(3.0 * j), c[1] + np.sin(7.0 * j)) for j in range(10)]
			score = float(np.stack([local_ncc(gray, t, window) for t in layers]).max(axis=0).mean())
			if best is None or score > best[0]:
				best = (score, c)
	return best[1]


def noise_offset(level, tu, tv, sub_texels=16):
	"""P: least-squares registration of the level field against 8 k over every shift of the noise
	(FFT); returns P (noise uv), the rms in levels and the next-best shift's rms"""
	n = NOISE.shape[0]
	sigma = NOISE_SCALE * n  # noise texels per tile
	m = sub_texels
	x = (np.arange(n * m) + 0.5) / m - 0.5
	i0 = np.floor(x).astype(int)
	fr = x - i0
	i0 %= n
	i1 = (i0 + 1) % n
	rows = NOISE[np.ix_(i0, i0)] * (1 - fr)[None, :] + NOISE[np.ix_(i0, i1)] * fr[None, :]
	rows1 = NOISE[np.ix_(i1, i0)] * (1 - fr)[None, :] + NOISE[np.ix_(i1, i1)] * fr[None, :]
	field = (rows * (1 - fr)[:, None] + rows1 * fr[:, None]) * LEVELS
	# the level estimate resampled on a grid of 1/m noise texels
	u0, v0 = tu[0, 0], tv[0, 0]
	su, sv = tu[0, 1] - tu[0, 0], tv[1, 0] - tv[0, 0]
	# at most one period of the noise
	nu = min(int((tu[0, -1] - u0) * sigma * m), n * m - 1)
	nv = min(int((tv[-1, 0] - v0) * sigma * m), n * m - 1)
	gu = (np.arange(nu) / (sigma * m) / su).astype(np.float32)
	gv = (np.arange(nv) / (sigma * m) / sv).astype(np.float32)
	smooth = cv2.GaussianBlur(level.astype(np.float32), (0, 0), 2)
	grid = cv2.remap(smooth, np.tile(gu, (nv, 1)), np.tile(gv[:, None], (1, nu)), cv2.INTER_LINEAR)
	g = np.zeros_like(field)
	g[:nv, :nu] = grid
	mask = np.zeros_like(field)
	mask[:nv, :nu] = 1.0
	cross = np.real(np.fft.ifft2(np.conj(np.fft.fft2(g)) * np.fft.fft2(field)))
	energy = np.real(np.fft.ifft2(np.conj(np.fft.fft2(mask)) * np.fft.fft2(field * field)))
	sse = (grid * grid).sum() - 2 * cross + energy
	oy, ox = np.unravel_index(np.argmin(sse), sse.shape)
	yy, xx = np.mgrid[0 : sse.shape[0], 0 : sse.shape[1]]
	dist = np.minimum(np.abs(yy - oy), sse.shape[0] - np.abs(yy - oy)) ** 2 + np.minimum(np.abs(xx - ox), sse.shape[1] - np.abs(xx - ox)) ** 2
	rms = np.sqrt(max(sse[oy, ox], 0) / (nu * nv))
	second = np.sqrt(max(sse[dist > (m // 2) ** 2].min(), 0) / (nu * nv))
	# grid sample 0 sits at tile (u0, v0): noise texel (o + 0.5) / m - 0.5 = s N - 0.5 with s = 0.03 t + P
	P = (((ox + 0.5) / m / n - NOISE_SCALE * u0) % 1.0, ((oy + 0.5) / m / n - NOISE_SCALE * v0) % 1.0)
	return P, rms, second


def noise_scale(level, tu, tv):
	"""The noise's frequency (noise uv per tile): 0.03 on MaterialVariants, per material on built-in
	ones. Scans for the registration with the least rms, then refines it."""
	global NOISE_SCALE
	best = None
	for scales in (np.arange(0.0075, 0.0801, 0.0025), None):
		if scales is None:
			scales = np.arange(best[1] - 0.002, best[1] + 0.00201, 0.0002)
		for scale in scales:
			NOISE_SCALE = scale
			P, rms, second = noise_offset(level, tu, tv)
			if best is None or rms < best[0]:
				best = (rms, scale, P, second)
	NOISE_SCALE = best[1]
	return best[1]


def offsets(run_dir, pack, spt, json_path=None, name=None, far=False):
	display = np.asarray(Image.open(os.path.join(run_dir, "studio.png")).convert("RGB"), dtype=np.float64)
	observed = radiance(display).astype(np.float32)
	gray = observed.mean(axis=2)
	tex = color_map(pack)
	# the swatch camera on a 36-stud slab, or organic-builtin's on a 200-stud one
	pps, center = (FAR_PPS, (100.0, 100.0)) if far else (SWATCH_PPS, (18.0, 18.0))
	C, share, single = texture_offset(gray, tex.mean(axis=2), pps, center, spt)
	sub = 2
	n = int(round(spt * pps))
	small = cv2.resize(tex, (n, n), interpolation=cv2.INTER_AREA)
	tu, tv = pixel_tiles(gray.shape, pps, center, spt, sub)
	obs = observed[::sub, ::sub]
	gray_obs, small_gray = np.ascontiguousarray(obs.mean(axis=2)), np.ascontiguousarray(small.mean(axis=2))
	candidates = [C] if single is None else [((single[0] - np.sin(3.0 * j)) % 1.0, (single[1] - np.sin(7.0 * j)) % 1.0) for j in range(9)]
	best = None
	for C in candidates:
		C = refine_texture_offset(gray_obs, small_gray, tu, tv, C, 0.012, 7)
		C = refine_texture_offset(gray_obs, small_gray, tu, tv, C, 0.002, 5)
		level, _, scale = level_fit(obs, small, tu, tv, C)
		frequency = noise_scale(level, tu, tv)
		P, rms, second = noise_offset(level, tu, tv)
		if best is None or rms < best[0]:
			best = (rms, C, scale, frequency, P, second)
	rms, C, scale, frequency, P, second = best
	label = name or os.path.basename(os.path.normpath(run_dir))
	if single is not None:
		label_note = f" (one offset in view: the best of {len(candidates)} levels)"
		print(f"{label}:{label_note}")
	print(
		f"{label}: C = ({C[0] % 1:.4f}, {C[1] % 1:.4f}) tiles ({share * 100:.0f}% of matched patches on one C + sin(3i, 7i)); "
		f"noise {frequency:.4f} per tile, P = ({P[0]:.4f}, {P[1]:.4f}), level rms {rms:.3f} (next shift {second:.3f}); light x {scale:.3f}"
	)
	if json_path:
		data = json.load(open(json_path)) if os.path.exists(json_path) else {}
		data[label] = [round(P[0], 5), round(P[1], 5), round(C[0] % 1.0, 5), round(C[1] % 1.0, 5), round(frequency, 5), round(rms, 3)]
		json.dump(data, open(json_path, "w"), indent=1)


def blend(run_dir, pack, spt):
	display = np.asarray(Image.open(os.path.join(run_dir, "studio.png")).convert("RGB"), dtype=np.float64)
	observed = radiance(display).astype(np.float32)
	tex = color_map(pack)
	n = int(round(spt * SWATCH_PPS))
	small = cv2.resize(tex, (n, n), interpolation=cv2.INTER_AREA)
	tu, tv = pixel_tiles(observed.shape[:2], SWATCH_PPS, (18.0, 18.0), spt)
	l = noise_at(tu, tv) * LEVELS
	i = np.floor(l)
	f = l - i
	a = layer(small, tu, tv, np.sin(3 * i), np.sin(7 * i))
	b = layer(small, tu, tv, np.sin(3 * (i + 1)), np.sin(7 * (i + 1)))
	scale = float(np.median((observed / np.maximum(a, 1e-3))[f < 0.1]))
	o = observed / scale
	d = b - a
	den = (d * d).sum(axis=2)
	w = ((o - a) * d).sum(axis=2) / np.maximum(den, 1e-9)
	good = den > 0.02
	print(f"light x {scale:.3f}; median cross-fade weight per tenth of f (pixels where the layers differ):")
	for lo in np.arange(0.0, 1.0, 0.1):
		m = good & (f >= lo) & (f < lo + 0.1)
		print(f"  f {lo:.1f}-{lo + 0.1:.1f}: w {np.median(w[m]):.3f}  ({m.sum()} px)")


def slopes(experiment_dir):
	tex = color_map("122577965407434").mean(axis=2)
	for run in sorted(os.listdir(experiment_dir)):
		path = os.path.join(experiment_dir, run, "studio.png")
		if not run.startswith("w") or not os.path.exists(path):
			continue
		sy, sz = (float(value) for value in run[1:].split("x")[1:])
		distance = 30.0 + max(sy, sz)
		pps = 558.0 / (distance * np.tan(np.radians(10.0)))
		l2, l1 = np.hypot(sy, sz), sy + sz
		gray = radiance(np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)).mean(axis=2).astype(np.float32)
		h, w = gray.shape
		patch = 40
		nu, nv = int(round(8 * pps)), int(round(8 * pps * l2 / l1))
		small = cv2.resize(tex, (nu, nv), interpolation=cv2.INTER_AREA)
		wrapped = np.tile(small, (2, 2))[: nv + patch, : nu + patch].copy()
		x_lo, x_hi = int(w / 2 - 8 * pps + 10), int(w / 2 + 8 * pps - 10)
		y_lo, y_hi = int(max(0, h / 2 - l2 / 2 * pps + 10)), int(min(h, h / 2 + l2 / 2 * pps - 10))
		down, along = [], []
		for y0 in range(y_lo, y_hi - patch + 1, 20):
			for x0 in range(x_lo, x_hi - patch + 1, 20):
				res = cv2.matchTemplate(wrapped, gray[y0 : y0 + patch, x0 : x0 + patch], cv2.TM_CCOEFF_NORMED)
				_, value, _, (mx, my) = cv2.minMaxLoc(res)
				if value < 0.85 or not 0 < my < res.shape[0] - 1:
					continue
				den = res[my - 1, mx] - 2 * res[my, mx] + res[my + 1, mx]
				fy = my + (0.5 * (res[my - 1, mx] - res[my + 1, mx]) / den if den != 0 else 0.0)
				s = l2 / 2 + (y0 - h / 2) / pps
				u = 8.0 + (x0 + patch / 2 - w / 2) / pps
				v_model = s * l1 / l2
				i = np.floor(noise_at(np.array(u / 8.0), np.array((s + patch / 2 / pps) * l1 / l2 / 8.0)) * LEVELS + 0.5)
				residual = ((fy / nv - v_model / 8.0 - np.sin(7 * i)) + 0.5) % 1.0 - 0.5
				if abs(residual) < 0.08:
					down.append(s)
					along.append(v_model + 8.0 * residual)
		a, b = np.polyfit(down, along, 1)
		print(f"{run}: v = {a:.4f} s {b:+.3f} studs over {len(down)} patches; (|y| + |z|) / length {l1 / l2:.4f}, ratio {a / (l1 / l2):.4f}")


if __name__ == "__main__":
	command = sys.argv[1] if len(sys.argv) > 1 else ""
	args, flags = [], {}
	rest = iter(sys.argv[2:])
	for arg in rest:
		if arg == "--far":
			flags[arg] = True
		elif arg.startswith("--"):
			flags[arg] = next(rest, None)
		else:
			args.append(arg)
	if command == "offsets":
		offsets(args[0], args[1], float(args[2]) if len(args) > 2 else 8.0, flags.get("--json"), flags.get("--name"), "--far" in flags)
	elif command == "blend":
		blend(args[0], args[1], float(args[2]))
	elif command == "slopes":
		slopes(args[0])
	else:
		print(__doc__)
