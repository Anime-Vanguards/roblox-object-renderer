"""Refit the bloom's constants (render/mod.rs: threshold, knee, tents, level weights, BloomEffect's
threshold scale) against ror's own images, scored as the halo metric scores them (notes/render.md
"Bloom"). A delta proxy of each run,

    shown(p) = round(display(radiance(v + u) + glow(p) - glow(P0))),

v ror's displayed value (the run's ror.png, rendered with P0, the constants render/mod.rs holds), u a
fixed dither within its rounding cell, glow(p) the chain as ror computes it at the run's pixels, each
level's chain alone so that the level weights act linearly. ror's storage counts: the scene target's
√x and the bloom's working grid are half floats, which this GPU writes rounded toward zero (white
Neon capped at 7.84 reaches the grid as 7.777 with the cap 7.78), the upsampled and horizontally
blurred levels too, and every level as R11G11B10 keeps it (bloom_fit.stored). The excess of a capped
source sits near a 6-bit step of that storage (3.84375 and 3.875 lie 0.8 % apart), so the model
follows ror's images to 0.25 % only with them. White Neon's interiors are snapped to their exact x
(25.75 2^EV, capped), which the 8-bit linear PNGs round by up to 0.04. At P0 the proxy is ror's image;
a change responds as a render would, statistically. The halo pixels: Studio or the image shows 3..250
in the largest channel; within 2 of 255 in every channel; every 4th pixel of the region within 40 px
of what either image shows at 3 or more.

Run the experiments with `ROR_CALIBRATE_SOURCE=1` first (each run's ror.png, linear2.png and
linear16.png), then:

    python tools/analysis/bloom_refit.py score [params.json ...]   # the halo metric per experiment
    python tools/analysis/bloom_refit.py weights                   # the level weights alone
    python tools/analysis/bloom_refit.py search [params.json]      # and threshold, knee, tents, scale

The level weights: Gauss-Newton in display levels with a Cauchy loss of 1.75 levels (the metric counts
pixels within 2), every experiment's pixels weighted alike. The rest: a coordinate search on the
metric itself, each experiment's change against its number at P0 (or `REFERENCE`, a JSON of numbers
per experiment), losses counted 3x. A parameter file holds any of P0's keys. About 1 GB of memory.
"""

import copy
import json
import os
import re
import sys

import numpy as np
from PIL import Image
from scipy.ndimage import convolve1d, distance_transform_edt

import bloom_fit as bf
from bloom_fit import half
from tone import display, radiance

ROOT = "out/calibrate"
EXPERIMENTS = ["bloom-colors", "bloom-effect", "bloom-effect-size", "bloom-kernel", "bloom-threshold", "bloom-phase", "bloom-lines", "bloom-probe", "neon-colors", "neon-rows", "bloom-size", "bloom-wide", "bloom-dots", "bloom-pairs", "neon-slabs-bloom"]
# render/mod.rs: BLOOM_THRESHOLD, BLOOM_KNEE, BLOOM_CAP, BLOOM_TAPS (each level's outer tap weight),
# BLOOM_TENTS, BLOOM_LEVEL_WEIGHTS (each level's full weight; the built-in keeps log2(24) - 0.9 levels),
# BloomEffect's threshold per unit Threshold
P0 = {"threshold": 3.885, "knee": 0.06, "cap": 7.75, "taps": [0.3103, 0.3103, 0.125, 0.125, 0.125], "tents": [3.44, 4.00, 4.11, 4.11], "weights": [0.986, 1.010, 1.037, 1.056, 1.073], "effect_scale": 1.951}
STEP = 4
REACH = 40
F_SCALE = 1.75


def load(path):
	return np.asarray(Image.open(path).convert("RGB")).astype(np.int16)


def scene_source(directory, environment):
	"""ror's exposed radiance as the bloom reads it: the linear PNGs' x, white Neon's interiors snapped to
	their exact value, the scene target's √(x / exposure) a half float."""
	x = bf.source_x(directory)
	exposure = 2.0 ** environment.get("exposureCompensation", 0.0)
	exact = min(25.75 * exposure, 7.84)
	step = 2 * np.sqrt((2.0 if exact < 1.9 else 16.0) * exact) / 255
	x = np.where(np.abs(x - exact) < 1.5 * step, np.float32(exact), x)
	return (half(np.sqrt(x / exposure)) ** 2 * exposure).astype(np.float32)


def level_factors(size, amplitude):
	"""Each level's share of its weight for a bloom of BloomEffect.Size `size` (render/mod.rs
	bloom_levels)."""
	if size <= 0:
		return np.zeros(5)
	count = np.log2(min(max(size, 6.0), 56.0)) - 0.9
	return np.array([min(max(count - i, 0.0), 1.0) * amplitude for i in range(5)])


def factors(environment, p):
	"""A run's threshold, knee and level shares (render/mod.rs bloom_parameters)."""
	for effect in environment.get("effects", []):
		m = re.match(r"Bloom \{ intensity: ([-\d.e]+), size: ([-\d.e]+), threshold: ([-\d.e]+) \}", effect)
		if m:
			intensity, size, threshold = (float(v) for v in m.groups())
			t = p["effect_scale"] * max(threshold, 0.0)
			return t, p["knee"] * t / p["threshold"], level_factors(size, max(intensity, 0.0))
	return p["threshold"], p["knee"], level_factors(24.0, 1.0)


def tent_kernel(radius):
	reach = max(int(np.ceil(radius)) - 1, 0)
	i = np.arange(-reach, reach + 1)
	k = 1 - np.abs(i) / radius
	return k / k.sum()


def unit_levels(first, threshold, knee, tents, count, ys, xs, taps):
	"""Each level's chain alone (unit weight), at the pixels: count x N x 3. Going up, the upsampled level
	and its horizontal blur are half floats, the vertical blur (plus the level) is stored."""
	levels = [bf.stored(bf.excess(first, threshold, knee))]
	for k in range(1, count):
		levels.append(bf.stored(bf.down(levels[-1], bf.WEIGHTING[1], taps[min(k, len(taps) - 1)])))
	out = np.zeros((count, len(ys), 3), np.float32)
	for k in range(count):
		total = levels[k]
		for n in range(k - 1, -1, -1):
			kernel = tent_kernel(tents[n])
			across = half(convolve1d(half(bf.upsample(total, levels[n].shape)), kernel, axis=1, mode="nearest"))
			total = bf.stored(convolve1d(across, kernel, axis=0, mode="nearest"))
		out[k] = bf.at_pixels(total, ys, xs)
	return out


class Run:
	def __init__(self, experiment, label):
		self.experiment, self.label = experiment, label
		self.dir = os.path.join(ROOT, experiment, label)
		meta = next(r for r in json.load(open(os.path.join(ROOT, experiment, "samples.json"))) if r["label"] == label)
		self.environment = meta["environment"]
		studio = load(os.path.join(self.dir, "studio.png"))
		ror = load(os.path.join(self.dir, "ror.png"))
		H, W = studio.shape[:2]
		s, r = studio.max(axis=2), ror.max(axis=2)
		# the halo region (ror shows the fogged black backdrop as 1 at EV >= 0, which no glow reaches)
		lattice = (np.arange(H)[:, None] + np.arange(W)[None, :]) % STEP == 0
		near = distance_transform_edt(~((s >= 3) | (r >= 3))) <= REACH
		self.ys, self.xs = np.nonzero(lattice & near & (((s >= 3) & (s <= 250)) | ((r >= 1) & (r <= 252))))
		self.s = studio[self.ys, self.xs]
		self.v = ror[self.ys, self.xs]
		u = np.random.default_rng(len(self.ys)).uniform(-0.5, 0.5, self.v.shape)
		self.xr = radiance(np.clip(self.v + u, 0, None)).astype(np.float32)
		self.key = None
		self.L = None
		self.base = self.glow(P0)

	def levels(self, p):
		threshold, knee, c = factors(self.environment, p)
		count = int(np.nonzero(c > 0)[0].max() + 1) if (c > 0).any() else 0
		key = (round(threshold, 6), round(knee, 6), p["cap"], tuple(round(t, 5) for t in p["tents"][: max(count - 1, 0)]), tuple(p["taps"]), count)
		if key != self.key:
			# the working grid: the source capped, a half float, then the first downsample
			first = bf.down(half(np.minimum(scene_source(self.dir, self.environment), p["cap"])), bf.WEIGHTING[0], p["taps"][0]).astype(np.float32)
			self.L = unit_levels(first, threshold, knee, p["tents"], count, self.ys, self.xs, p["taps"]).astype(np.float16) if count else np.zeros((0, len(self.ys), 3), np.float16)
			self.key = key
		return self.L, c

	def glow(self, p):
		L, c = self.levels(p)
		out = np.zeros((len(self.ys), 3), np.float32)
		for k in range(len(L)):
			out += np.float32(p["weights"][k] * c[k]) * L[k].astype(np.float32)
		return out

	def shown(self, p):
		return np.round(display(self.xr + (self.glow(p) - self.base))).astype(np.int16)

	def counts(self, p):
		shown = self.shown(p)
		worst = np.abs(shown - self.s).max(axis=1)
		sm, rm = self.s.max(axis=1), shown.max(axis=1)
		mask = ((sm >= 3) & (sm <= 250)) | ((rm >= 3) & (rm <= 250))
		return np.array([(worst[mask] <= 2).sum(), mask.sum()], float)


def load_runs():
	runs = []
	for experiment in EXPERIMENTS:
		for label in sorted(os.listdir(os.path.join(ROOT, experiment))):
			if os.path.isfile(os.path.join(ROOT, experiment, label, "linear2.png")):
				run = Run(experiment, label)
				if len(run.ys):
					runs.append(run)
	return runs


def score(runs, p):
	"""Halo pixels within 2 per experiment (percent)."""
	totals = {}
	for run in runs:
		totals[run.experiment] = totals.get(run.experiment, 0) + run.counts(p)
	return {e: 100 * c[0] / max(c[1], 1) for e, c in totals.items()}


def objective(scores, reference):
	return np.mean([d if d >= 0 else 3 * d for d in (scores[e] - reference[e] for e in scores)])


def slope(x):
	h = np.maximum(1e-4, 1e-3 * x)
	lower = np.maximum(x - h, 0)
	return (display(x + h) - display(lower)) / (x + h - lower)


def solve_weights(runs, p, iterations=6):
	"""Gauss-Newton on the level weights, each experiment's pixels weighted by 1 / its pixel count."""
	p = copy.deepcopy(p)
	w = np.array(p["weights"], float)
	pixels = {}
	for run in runs:
		pixels[run.experiment] = pixels.get(run.experiment, 0) + len(run.ys)
	for _ in range(iterations):
		A, b = np.zeros((5, 5)), np.zeros(5)
		for run in runs:
			L, c = run.levels(p)
			x = run.xr + (run.glow(p) - run.base)
			shown = display(x)
			r = shown - run.s
			sm, rm = run.s.max(axis=1), np.round(shown).max(axis=1)
			halo = (((sm >= 3) & (sm <= 250)) | ((rm >= 3) & (rm <= 250)))[:, None]
			weight = np.where(halo, 1 / (1 + (r / F_SCALE) ** 2), 0) / pixels[run.experiment]
			J = [slope(x) * c[k] * L[k].astype(np.float32) for k in range(len(L))]
			for i in range(len(J)):
				b[i] -= (weight * J[i] * r).sum()
				for j in range(len(J)):
					A[i, j] += (weight * J[i] * J[j]).sum()
		free = [k for k in range(5) if A[k, k] > 0]
		step = np.zeros(5)
		step[free] = np.linalg.solve(A[np.ix_(free, free)], b[free])
		w = np.maximum(w + step, 0)
		p["weights"] = list(w)
		if np.abs(step).max() < 2e-4:
			break
	return p


def table(columns):
	print(f"{'experiment':18s}", " ".join(f"{name:>12s}" for name, _ in columns))
	for e in columns[0][1]:
		print(f"{e:18s}", " ".join(f"{scores[e]:12.1f}" for _, scores in columns))


def describe(p):
	return f"threshold {p['threshold']:.4f} knee {p['knee']:.4f} cap {p['cap']:.5f} taps {np.round(p['taps'], 4)} tents {np.round(p['tents'], 3)} weights {np.round(p['weights'], 4)} scale {p['effect_scale']:.4f}"


def search(runs, p, reference):
	best = solve_weights(runs, p)
	best_j = objective(score(runs, best), reference)
	print(f"start: J {best_j:+.3f}  {describe(best)}", flush=True)
	# each parameter's step, halved when neither direction gains, down to an eighth
	steps = {"threshold": 0.02, "knee": 0.04, "taps": 0.03, "tent0": 0.3, "tent1": 0.3, "tent2": 0.3, "effect_scale": 0.03}
	smallest = {name: step / 8 for name, step in steps.items()}
	while any(steps[name] >= smallest[name] for name in steps):
		for name in steps:
			if steps[name] < smallest[name]:
				continue
			for sign in (1, -1):
				q = copy.deepcopy(best)
				if name.startswith("tent"):
					i = int(name[4])
					q["tents"][i] = max(1.0, q["tents"][i] + sign * steps[name])
					if i == 2:
						q["tents"][3] = q["tents"][2]
				elif name == "taps":
					# the third level's and the deeper ones' (the first two keep the phase-fitted taps)
					q["taps"] = q["taps"][:2] + [min(0.5, max(0.0, a + sign * steps[name])) for a in q["taps"][2:]]
				else:
					q[name] = max(0.0, q[name] + sign * steps[name])
				q = solve_weights(runs, q, iterations=4)
				j = objective(score(runs, q), reference)
				print(f"  {name} {'+' if sign > 0 else '-'}{steps[name]:.4f}: J {j:+.3f}  {describe(q)}", flush=True)
				if j > best_j + 1e-3:
					best, best_j = q, j
					break
			else:
				steps[name] /= 2
		print(f"J {best_j:+.3f}  {describe(best)}", flush=True)
	return best


def main():
	command, paths = sys.argv[1], sys.argv[2:]
	runs = load_runs()
	start = score(runs, P0)
	reference = json.load(open(os.environ["REFERENCE"])) if "REFERENCE" in os.environ else start
	reference = {e: reference.get(e, start[e]) for e in start}
	if command == "score":
		columns = [("P0", start)]
		for path in paths:
			p = dict(P0, **json.load(open(path)))
			columns.append((os.path.basename(path)[:12], score(runs, p)))
			print(path, describe(p), f"J {objective(columns[-1][1], reference):+.3f}")
		table(columns)
	elif command == "weights":
		p = solve_weights(runs, P0)
		print(describe(p))
		table([("P0", start), ("weights", score(runs, p))])
	elif command == "search":
		p = dict(P0, **json.load(open(paths[0]))) if paths else copy.deepcopy(P0)
		best = search(runs, p, reference)
		print(json.dumps(best))
		table([("P0", start), ("refit", score(runs, best))])


if __name__ == "__main__":
	main()
