"""Studio's built-in bloom and BloomEffect from the bloom-* calibration experiments (notes/render.md
"Bloom").

Every source is a flat square, line or beam ribbon facing a camera that looks straight at its
plane, over a black, unlit backdrop. The fits compare displayed values (0-255): ror's bloom-free
source through the model below, against Studio's capture. The source is the exposed radiance x of
ror's render, each sample clamped as Studio clamps it: run the experiment with
`ROR_CALIBRATE_SOURCE=1` for each run's `linear2.png` and `linear16.png`.

    python tools/analysis/bloom_fit.py threshold   # halo onset per material (bloom-threshold)
    python tools/analysis/bloom_fit.py scales      # Neon / SmoothPlastic / beam brightness at EV -2, -1
    python tools/analysis/bloom_fit.py phase       # line halos by width and grid phase (bloom-phase)
    python tools/analysis/bloom_fit.py fit         # the chain's threshold, knee, tents and weights
    python tools/analysis/bloom_fit.py effect      # per BloomEffect run: threshold, level weights
    python tools/analysis/bloom_fit.py halos <experiment> ...  # halo pixels within 2 / 8 of 255
    python tools/analysis/bloom_fit.py fov         # halos through 20, 40 and 70 degree views

The model (render/mod.rs, shaders/bloom.wgsl): x clamped at CAP, a half float (`half`); level 1 at half
resolution, each texel the 4 x 4 pixels around its centre (a pixel corner) weighted per axis (a, 0.5 -
a, 0.5 - a, a), a LEVEL_TAPS[0], and by 1 / (1 + s m), m a pixel's brightest channel (normalised: a
weighted Karis average, s WEIGHTING[0]), then its brightest channel's excess over the threshold (soft
knee); the deeper levels the same way from the level above (a LEVEL_TAPS[k], s WEIGHTING[1]); back up,
each level adds the coarser one, upsampled bilinearly and blurred by a tent (the upsample and the
horizontal pass half floats), to its own texels times its weight; every level and every sum stored as
an R11G11B10 texture keeps it (`stored`); the first level sampled bilinearly at each pixel is the halo
added to x before the tone curve.
"""

import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.ndimage import convolve1d, maximum_filter
from scipy.optimize import least_squares

from tone import display, radiance

ROOT = "out/calibrate"

# the ror constants (render/mod.rs, shaders/bloom.wgsl); the built-in's fourth weight is the level's
# full weight times the 0.685 of it the built-in keeps
TAPS = np.array([0.3103, 0.1897, 0.1897, 0.3103])
# each level's outer tap weight a, the first level's first (BLOOM_TAPS)
LEVEL_TAPS = [0.3103, 0.3103, 0.125, 0.125, 0.125]
CAP = 7.75
WEIGHTING = (0.016, 0.010)
BUILT_IN = {"threshold": 3.885, "knee": 0.06, "tents": [3.44, 4.00, 4.11], "weights": [0.986, 1.010, 1.037, 0.723]}


def image(path):
	return np.asarray(Image.open(path).convert("RGB")).astype(float)


def source_x(directory):
	"""ror's exposed radiance without the bloom (ROR_CALIBRATE_SOURCE=1): √(x / 2) where x is under 2,
	√(x / 16) above."""
	low = (image(os.path.join(directory, "linear2.png")) / 255.0) ** 2 * 2
	high = (image(os.path.join(directory, "linear16.png")) / 255.0) ** 2 * 16
	return np.where(low < 1.9, low, high).astype(np.float32)


class Run:
	def __init__(self, experiment, label):
		self.experiment = experiment
		self.dir = os.path.join(ROOT, experiment, label)
		meta = next(r for r in json.load(open(os.path.join(ROOT, experiment, "samples.json"))) if r["label"] == label)
		self.samples = {s["probe"]: s for s in meta["samples"]}
		c = meta["camera"]
		self.eye = np.array(c[:3])
		R = np.array(c[3:]).reshape(3, 3)
		self.right, self.up, self.look = R[:, 0], R[:, 1], -R[:, 2]
		self.fov = meta["fov"]
		self.ev = meta["environment"]["exposureCompensation"]
		self.studio = self.image("studio.png")
		self.ror = self.image("ror.png")
		self.H, self.W = self.studio.shape[:2]

	def image(self, name):
		return image(os.path.join(self.dir, name))

	def project(self, p):
		d = np.asarray(p, float) - self.eye
		t = np.tan(np.radians(self.fov) / 2)
		z = d @ self.look
		return np.array([((d @ self.right) / (z * t * self.W / self.H) * 0.5 + 0.5) * self.W, (0.5 - (d @ self.up) / (z * t) * 0.5) * self.H])

	def depth(self, name):
		inputs = self.samples[name]["inputs"]
		if inputs.get("kind") == "beam":
			return 0.0
		return inputs.get("depth", 0.02 if self.experiment == "bloom-lines" else 0.2)

	def rect(self, name):
		"""The source's pixel rectangle (x0, x1, y0, y1): its front face and, for tiles, the back
		face's outline too (their sides show off-axis)."""
		inputs = self.samples[name]["inputs"]
		c = np.array(inputs["center"])
		w, h = inputs["size"]
		depth = self.depth(name)
		points = [self.project(c + np.array([sx * w / 2, sy * h / 2, dz])) for sx in (-1, 1) for sy in (-1, 1) for dz in (0.0, depth)]
		xs = [p[0] for p in points]
		ys = [p[1] for p in points]
		return min(xs), max(xs), min(ys), max(ys)

	def x(self, image="studio"):
		return radiance(self.studio if image == "studio" else self.ror)


def edge_band(run, name, near, far, image="studio"):
	"""Mean x (per channel) in the bands `near`..`far` px outside the middle half of each edge."""
	x0, x1, y0, y1 = run.rect(name)
	img = run.x(image)
	cx0, cx1 = int(x0 + (x1 - x0) * 0.25), int(x1 - (x1 - x0) * 0.25) + 1
	cy0, cy1 = int(y0 + (y1 - y0) * 0.25), int(y1 - (y1 - y0) * 0.25) + 1
	parts = [
		img[cy0:cy1, int(x0 - far) : int(x0 - near)],
		img[cy0:cy1, int(np.ceil(x1 + near)) : int(np.ceil(x1 + far))],
		img[int(y0 - far) : int(y0 - near), cx0:cx1],
		img[int(np.ceil(y1 + near)) : int(np.ceil(y1 + far)), cx0:cx1],
	]
	return np.concatenate([p.reshape(-1, 3) for p in parts]).mean(axis=0)


def threshold():
	"""The halo 4-10 px outside each ladder tile against its exposed x (model units)."""
	experiment = "bloom-threshold"
	rows = []
	for label in os.listdir(os.path.join(ROOT, experiment)):
		if not label.startswith("ev"):
			continue
		run = Run(experiment, label)
		for name, sample in run.samples.items():
			inputs = sample["inputs"]
			rows.append((inputs["kind"], inputs["x"] * 2**run.ev, edge_band(run, name, 4, 10)[1], edge_band(run, name, 4, 10, "ror")[1]))
	for kind in ("neon", "plastic", "beam"):
		near = sorted([r for r in rows if r[0] == kind and 3.6 < r[1] < 4.8], key=lambda r: r[1])
		print(f"{kind:8s}", " ".join(f"{r[1]:.2f}:{r[2]:.3f}/{r[3]:.3f}" for r in near))
	print("(exposed x : halo Studio / ror)")


def scales():
	"""Measured / modelled radiance of every ladder tile below the threshold, per material."""
	for label in ["ev-2", "ev-1", "dark-ev-2", "dark-ev-1"]:
		run = Run("bloom-threshold", label)
		out = {}
		for name, sample in run.samples.items():
			inputs = sample["inputs"]
			exposed = inputs["x"] * 2**run.ev
			x0, x1, y0, y1 = run.rect(name)
			d = run.studio[int(y0) + 12 : int(y1) - 12, int(x0) + 12 : int(x1) - 12].reshape(-1, 3).mean(axis=0)[1]
			if exposed > 2.0 or d < 5:
				continue
			out.setdefault(inputs["kind"], []).append(radiance(d) / exposed)
		print(label, {k: round(float(np.median(v)), 4) for k, v in out.items()})


def half(a):
	"""What an Rgba16Float texture keeps of a float as ror's GPU writes it: 10 mantissa bits, rounded
	towards zero (the scene target's √x and the bloom's working grid: 7.78 becomes 7.777)."""
	bits = np.ascontiguousarray(np.maximum(a, 0), dtype=np.float32).view(np.uint32).copy()
	bits &= np.uint32(0xFFFFE000)
	return bits.view(np.float32)


def stored(a):
	"""What an R11G11B10 float texture keeps: mantissas cut to 6 bits (red, green) and 5 (blue),
	rounded towards zero."""
	bits = np.ascontiguousarray(np.maximum(a, 0), dtype=np.float32).view(np.uint32).copy()
	bits[..., :2] &= np.uint32(0xFFFE0000)
	bits[..., 2:] &= np.uint32(0xFFFC0000)
	return bits.view(np.float32)


def down(a, weighting=0.0, outer=None):
	"""The next level: texel j from finer texels 2j - 1 .. 2j + 2 per axis, weighted (outer, 0.5 - outer,
	0.5 - outer, outer) (TAPS without `outer`) and each texel by 1 / (1 + weighting m), m its brightest
	channel (normalised), clamped to the edge."""
	taps = TAPS if outer is None else np.array([outer, 0.5 - outer, 0.5 - outer, outer])
	if weighting > 0:
		w = 1 / (1 + np.float32(weighting) * a.max(axis=-1, keepdims=True))
		return down(a * w, 0.0, outer) / np.maximum(down(w, 0.0, outer), 1e-9)
	for axis in (0, 1):
		n = a.shape[axis]
		m = (n + 1) // 2
		pad = [(0, 0)] * a.ndim
		pad[axis] = (1, 2 * m + 2 - n)
		p = np.pad(a, pad, mode="edge")
		out = 0
		for i, w in enumerate(taps):
			index = [slice(None)] * a.ndim
			index[axis] = slice(i, i + 2 * m, 2)
			out = out + np.float32(w) * p[tuple(index)]
		a = out
	return a


def excess(a, threshold, knee):
	"""The brightest channel's excess over the threshold (a soft knee around it), the colour kept."""
	peak = a.max(axis=-1, keepdims=True)
	over = np.maximum(peak - threshold, 0)
	if knee > 0:
		over = np.maximum(over, np.clip(peak - threshold + knee, 0, 2 * knee) ** 2 / (4 * knee))
	return np.where(peak > 1e-6, a * over / np.maximum(peak, 1e-6), 0)


def upsample(a, shape):
	"""A level bilinearly at the next finer level's texel centres, clamped to the edge."""
	h, w = shape[:2]
	u = (np.arange(h) + 0.5) / 2 - 0.5
	v = (np.arange(w) + 0.5) / 2 - 0.5
	y0, x0 = np.floor(u).astype(int), np.floor(v).astype(int)
	fy, fx = (u - y0)[:, None, None], (v - x0)[None, :, None]
	H, W = a.shape[:2]
	y0c, y1c = np.clip(y0, 0, H - 1), np.clip(y0 + 1, 0, H - 1)
	x0c, x1c = np.clip(x0, 0, W - 1), np.clip(x0 + 1, 0, W - 1)
	return (1 - fy) * ((1 - fx) * a[y0c][:, x0c] + fx * a[y0c][:, x1c]) + fy * ((1 - fx) * a[y1c][:, x0c] + fx * a[y1c][:, x1c])


def tent(a, radius):
	"""A tent blur of `radius` texels: across (a half float, as ror's horizontal pass keeps it), then
	down."""
	reach = max(int(np.ceil(radius)) - 1, 0)
	i = np.arange(-reach, reach + 1)
	k = 1 - np.abs(i) / radius
	k /= k.sum()
	return convolve1d(half(convolve1d(a, k, axis=1, mode="nearest")), k, axis=0, mode="nearest")


def at_pixels(level, ys, xs):
	"""The first level (texels 2 px a side) bilinearly at pixel centres, clamped to the edge."""
	h, w = level.shape[:2]
	u, v = (ys + 0.5) / 2 - 0.5, (xs + 0.5) / 2 - 0.5
	y0, x0 = np.floor(u).astype(int), np.floor(v).astype(int)
	fy, fx = (u - y0)[:, None], (v - x0)[:, None]
	y0c, y1c = np.clip(y0, 0, h - 1), np.clip(y0 + 1, 0, h - 1)
	x0c, x1c = np.clip(x0, 0, w - 1), np.clip(x0 + 1, 0, w - 1)
	return (1 - fy) * ((1 - fx) * level[y0c, x0c] + fx * level[y0c, x1c]) + fy * ((1 - fx) * level[y1c, x0c] + fx * level[y1c, x1c])


def chain(first, threshold, knee, tents, weights, weighting=None):
	"""The bloom's first level after the way back up, from the first level before its threshold (the
	deeper downsamples' weighting `weighting`, else WEIGHTING's)."""
	weighting = WEIGHTING[1] if weighting is None else weighting
	levels = [stored(excess(first, threshold, knee))]
	for k in range(1, len(weights)):
		levels.append(stored(down(levels[-1], weighting, LEVEL_TAPS[min(k, len(LEVEL_TAPS) - 1)])))
	total = weights[-1] * levels[-1]
	for n in range(len(weights) - 2, -1, -1):
		total = stored(weights[n] * levels[n] + tent(half(upsample(total, levels[n].shape)), tents[n]))
	return total


class Case:
	"""One run: the first level before the threshold (ror's source clamped at CAP) and the pixels to
	compare, the halo pixels the metric counts (Studio's largest channel 3..250 outside the sources)
	or, `inside`, the unsaturated channels inside large sources (neon-slabs-bloom)."""

	def __init__(self, experiment, label, channels=(1,), inside=False, step=3):
		directory = os.path.join(ROOT, experiment, label)
		studio = image(os.path.join(directory, "studio.png"))
		ror = image(os.path.join(directory, "ror.png"))
		source = source_x(directory)
		H, W = source.shape[:2]
		self.label = f"{experiment}/{label}"
		self.channels = list(channels)
		subset = (np.arange(H)[:, None] * 7 + np.arange(W)[None, :]) % step == 0
		if inside:
			keep = subset & (studio[..., self.channels] < 245).all(axis=-1)
		else:
			lit = source.max(axis=2) > 0.02
			largest = studio.max(axis=2)
			keep = subset & (~lit) & (largest >= 3) & (largest <= 250)
		self.ys, self.xs = np.nonzero(keep)
		self.capped = half(np.minimum(source, CAP))
		self.firsts = {}
		self.source = source[self.ys, self.xs][:, self.channels].astype(float)
		self.studio = studio[self.ys, self.xs][:, self.channels]
		self.ror = ror[self.ys, self.xs][:, self.channels]

	def first(self, weighting):
		"""The first level before the threshold (the last two weightings asked for kept)."""
		key = round(float(weighting), 7)
		if key not in self.firsts:
			if len(self.firsts) >= 2:
				self.firsts.pop(next(iter(self.firsts)))
			self.firsts[key] = down(self.capped, weighting).astype(np.float32)
		return self.firsts[key]

	def shown(self, params):
		"""params: ((first level's weighting, the others'), threshold, knee, tents, weights)"""
		bloom = at_pixels(chain(self.first(params[0][0]), *params[1:], weighting=params[0][1]), self.ys, self.xs)[:, self.channels]
		return display(self.source + bloom)


SLABS = [("r255-g96-b48", (1, 2)), ("r255-g128-b64", (2,)), ("r255-g40-b40", (1, 2)), ("r200-g100-b50", (1, 2)), ("r160-g80-b40", (1, 2)), ("b255-r64-g64", (0, 1)), ("g255-r96-b48", (0, 2))]


def fit_cases():
	runs = [("bloom-phase", ["h-ev0", "h-ev-2", "h-ev-2.5", "v-ev0", "v-ev-2"]), ("bloom-dots", ["ev0", "ev-2"]), ("bloom-pairs", ["ev0", "ev-1"]), ("bloom-lines", ["lines-ev0", "lines-ev-2"]), ("bloom-size", ["white-ev0", "white-ev-2", "gray-ev1"]), ("bloom-kernel", ["fov40", "fov40-ev-2"]), ("bloom-threshold", ["ev1", "ev2"]), ("bloom-wide", ["white6"]), ("particle-overlap", ["stack"])]
	cases = [Case(experiment, label) for experiment, labels in runs for label in labels]
	cases += [Case("neon-slabs-bloom", label, channels, inside=True, step=81) for label, channels in SLABS]
	cases += [Case("bloom-colors", label, channels=(0, 1, 2)) for label in ["ev-1", "ev0", "ev1"]]
	return cases


def unpack(p):
	"""(2 weightings, threshold, knee, 3 tents, 4 weights) -> the chain's parameters"""
	return (p[0], p[1]), p[2], p[3], list(p[4:7]), list(p[7:11])


def report(cases, params):
	total = np.zeros(3)
	for case in cases:
		d = np.abs(np.round(case.shown(params)) - case.studio).ravel()
		r = np.abs(case.ror - case.studio).ravel()
		total += [(d <= 2).sum(), (r <= 2).sum(), d.size]
		print(f"  {case.label:32s} within 2 {100 * (d <= 2).mean():5.1f} %  within 8 {100 * (d <= 8).mean():5.1f} %   (ror.png {100 * (r <= 2).mean():5.1f} %)")
	print(f"  all: within 2 {100 * total[0] / total[2]:.1f} % (ror.png {100 * total[1] / total[2]:.1f} %)")


def built_in():
	return (WEIGHTING, BUILT_IN["threshold"], BUILT_IN["knee"], BUILT_IN["tents"], BUILT_IN["weights"])


def fit():
	"""The downsamples' weighting, threshold, knee, the three upsampling tents and the four level
	weights, in displayed levels."""
	cases = fit_cases()

	def residuals(p):
		params = unpack(p)
		return np.concatenate([(case.shown(params) - case.studio).ravel() for case in cases])

	start = [*WEIGHTING, BUILT_IN["threshold"], BUILT_IN["knee"], *BUILT_IN["tents"], *BUILT_IN["weights"]]
	# a step the first level's float32 resolves
	result = least_squares(residuals, start, bounds=([0, 0, 3, 0, 0.5, 0.5, 0.5, 0, 0, 0, 0], [0.3, 0.3, 5, 1, 12, 12, 12, 20, 20, 20, 20]), x_scale="jac", diff_step=1e-3, max_nfev=40)
	params = unpack(result.x)
	print("weighting", np.round(params[0], 4), "threshold", round(params[1], 4), "knee", round(params[2], 4), "tents", np.round(params[3], 3), "weights", np.round(params[4], 4), f"rms {np.sqrt((result.fun ** 2).mean()):.3f} levels")
	report(cases, params)


def effect():
	"""Per BloomEffect run, the built-in chain's threshold and level weights (levels 1-6, tents and
	knee as built in): Size picks how many levels, Intensity scales them, Threshold the threshold."""
	for label in ["none", "default", "intensity1", "intensity2", "size8", "size56", "threshold0.5", "threshold1.5", "threshold3"]:
		case = Case("bloom-effect", label, channels=(0, 1, 2))

		def residuals(p):
			tents = BUILT_IN["tents"] + [BUILT_IN["tents"][-1]] * 2
			params = (WEIGHTING, p[0], BUILT_IN["knee"] * p[0] / BUILT_IN["threshold"], tents, list(p[1:]))
			return (case.shown(params) - case.studio).ravel()

		# the threshold from either side of the expected one (the fit has local minima)
		expected = {"none": BUILT_IN["threshold"], "threshold0.5": 1.0, "threshold1.5": 2.9, "threshold3": 5.6}.get(label, 1.87)
		fits = [least_squares(residuals, [start, 0.9, 1.1, 0.9, 0.6, 0.1, 0.05], bounds=([0.1] + [0] * 6, [8] + [20] * 6), x_scale="jac", diff_step=1e-3, max_nfev=60) for start in (expected - 0.15, expected + 0.15)]
		result = min(fits, key=lambda fit: fit.cost)
		print(f"{label:13s} threshold {result.x[0]:.3f}  weights {np.round(result.x[1:], 3)}  rms {np.sqrt((result.fun ** 2).mean()):.2f} levels")


def phase():
	"""The halo energy (x summed over +-40 px across the line, the middle 80 px of its length) of the
	bloom-phase lines by width and the phase of their top / left edge, Studio / model / ror."""
	for label in ["h-ev0", "h-ev-2", "h-ev-2.5", "v-ev-2"]:
		directory = os.path.join(ROOT, "bloom-phase", label)
		source = source_x(directory)
		studio, ror = radiance(image(os.path.join(directory, "studio.png")))[..., 1], radiance(image(os.path.join(directory, "ror.png")))[..., 1]
		H, W = source.shape[:2]
		ys, xs = np.mgrid[0:H, 0:W]
		model = at_pixels(chain(down(half(np.minimum(source, CAP)), WEIGHTING[0]), *built_in()[1:]), ys.ravel(), xs.ravel())[:, 1].reshape(H, W)
		print(label, "(phase: Studio / model / ror)")
		for row, px in enumerate([2.5, 3.0, 3.5, 4.0, 5.0, 6.0]):
			cells = []
			for k in range(8):
				if label[0] == "h":
					start, along = 60 + row * 184 + k, slice(42 + k * 205 + 20, 42 + k * 205 + 100)
					window = (slice(start - 40, start + int(np.ceil(px)) + 40), along)
				else:
					start, along = 60 + k * 201, slice(60 + row * 184 + 20, 60 + row * 184 + 100)
					window = (along, slice(start - 40, start + int(np.ceil(px)) + 40))
				outside = source[window][..., 1] < 0.02
				energy = [((image_x[window] - source[window][..., 1]) * outside).sum() / 80 for image_x in (studio, model + source[..., 1], ror)]
				cells.append(f"{start % 8}:" + "/".join(f"{e:.2f}" for e in energy))
			print(f"  {px:4.1f} px", "  ".join(cells))


def halos(experiments):
	"""Halo pixels (Studio or ror between 3 and 250 in the largest channel) within 2 and 8 of 255,
	per experiment over all its runs."""
	for experiment in experiments:
		counts = np.zeros(3)
		for label in sorted(os.listdir(os.path.join(ROOT, experiment))):
			directory = os.path.join(ROOT, experiment, label)
			if not os.path.isfile(os.path.join(directory, "studio.png")):
				continue
			studio = image(os.path.join(directory, "studio.png")).astype(int)
			ror = image(os.path.join(directory, "ror.png")).astype(int)
			s, r = studio.max(axis=2), ror.max(axis=2)
			mask = ((s >= 3) & (s <= 250)) | ((r >= 3) & (r <= 250))
			difference = np.abs(studio - ror).max(axis=2)[mask]
			counts += [(difference <= 2).sum(), (difference <= 8).sum(), mask.sum()]
		print(f"{experiment:18s} within 2 {100 * counts[0] / counts[2]:5.1f} %  within 8 {100 * counts[1] / counts[2]:5.1f} %  ({int(counts[2])} halo pixels)")


def fov():
	runs = {label: Run("bloom-kernel", label) for label in ["fov40", "fov40-ev-2", "fov70", "fov20"]}
	for name in ["dot", "small", "medium", "large"]:
		print(name, "(x every 6 px from the right edge)")
		for label, run in runs.items():
			x0, x1, y0, y1 = run.rect(name)
			cy = int(round((y0 + y1) / 2))
			xs = np.arange(int(np.ceil(x1)) + 2, int(np.ceil(x1)) + 70, 6)
			print(f"  {label:11s}", " ".join(f"{v:.3f}" for v in run.x()[cy, xs, 1]))


if __name__ == "__main__":
	command = sys.argv[1]
	if command == "halos":
		halos(sys.argv[2:])
	else:
		{"threshold": threshold, "scales": scales, "phase": phase, "fit": fit, "effect": effect, "fov": fov}[command]()
