"""Studio's screen-space ambient occlusion from the ao-* calibration experiments (notes/render.md "Ambient
occlusion").

    python tools/analysis/ao_fit.py profiles ao-distance d12.5,d25,d50,d100 0   # wall / open wall beside each bar
    python tools/analysis/ao_fit.py profiles ao-gap ambient,outdoor,sky,sun -24,-16,-8,0,8,16,24
    python tools/analysis/ao_fit.py lines ao-depth d3,d8,d20,d50,d100 [setting]  # the crease's AO along world lines
    python tools/analysis/ao_fit.py score "depth=1.5e-4,sigma=2"                 # render and score one setting
    python tools/analysis/ao_fit.py fit radius=33:15:80 depth=1.5e-4:1e-5:5e-4 ...  # Nelder-Mead over settings

The profiles: the wall's radiance (green, through radiance()) beside each black bar over the wall's
radiance halfway between bars, against the distance in pixels from the bar's dark edge, both sides
averaged; Studio and ror.

`lines`, `score` and `fit` compare AO factors: a run's radiance over ror's radiance without the
occlusion (the same run rendered with ROR_NO_AO=1, kept in out/ao-fit/noao), Studio's against ror's.
`score` and `fit` render the experiments through `ror studio calibrate` with ROR_TUNE_AO (the model's
parameters, render::ao `Tune`), so the model fitted is the shader itself: captures replay from the cache
(set ROR_CAPTURE_OFFLINE and ROR_STATE_DIR as for any calibration). The score: per run, Studio's AO
factor over its level where ror sees no occlusion, the squared differences from ror's over surface
pixels away from silhouettes where either darkens by 2 % or more, divided by the number of pixels
Studio darkens (below 0.995; at least 2000), weighted and averaged over runs, square-rooted. One
experiment renders at a time (ROR_AO_FIT_WORKERS more, each writing its own
out/calibrate directory): run nothing else that calibrates meanwhile.
"""

import concurrent.futures
import json
import os
import subprocess
import sys

import numpy as np
from PIL import Image
from scipy import ndimage, optimize

from light_fit import Run, radiance

ROR = os.path.join("target", "release", "ror.exe")
NOAO = os.path.join("out", "ao-fit", "noao")
MODELS = ""
NAMES = {}
# experiment: (weight, the runs scored or None for every run)
SETS = {
	"ao-distance": (1.0, None),
	"ao-gap": (1.0, ["ambient"]),
	"ao-scale": (1.0, ["small"]),
	"ao-corner": (1.0, ["ambient"]),
	"ao-depth": (1.0, None),
	"ao-dolly": (1.0, ["d3", "d20", "d50", "d100"]),
	"ao-size": (1.0, None),
	"ao-ramps": (1.0, None),
	"ao-layers": (1.0, None),
	"ao-fade": (1.0, ["d110", "d120", "d125-fov40", "d130", "d140", "d150-fov40"]),
	"ao-far": (1.0, ["d200"]),
	"ao-relief": (1.0, None),
	"ao-thin": (0.5, None),
}
LUT = radiance(np.arange(256, dtype=float))


def profiles(experiment, label, bar_xs, y_world=0.0, reach_px=120):
	run = Run(experiment, label)
	hits, _ = run.plane_hits(2, 0.0)
	s = radiance(run.studio[..., 1])
	r = radiance(run.ror[..., 1])
	H, W = s.shape
	row = int(np.argmin(np.abs(hits[:, W // 2, 1] - y_world)))
	xs = hits[row, :, 0]
	px_per_stud = 1.0 / np.median(np.abs(np.diff(xs)))
	# the reference: the wall halfway between bars (and 4 studs outside the outer ones)
	spacing = (bar_xs[1] - bar_xs[0]) if len(bar_xs) > 1 else 8.0
	middles = [x + spacing / 2 for x in bar_xs] + [bar_xs[0] - spacing / 2]
	columns = [int(np.argmin(np.abs(xs - m))) for m in middles]
	ref_s = np.median([s[row, c] for c in columns])
	ref_r = np.median([r[row, c] for c in columns])
	out = {}
	for bar_x in bar_xs:
		left, right = bar_edges(run, row, xs, bar_x)
		rows = []
		for d in range(1, reach_px):
			values = []
			for edge, step in ((left, -1), (right, 1)):
				c = edge + step * d
				if 0 <= c < W:
					values.append((s[row, c] / ref_s, r[row, c] / ref_r, abs(xs[c] - xs[edge])))
			if values:
				v = np.array(values)
				rows.append((d, v[:, 2].mean(), v[:, 0].mean(), v[:, 1].mean(), v[:, 0].min()))
		out[bar_x] = np.array(rows)
	return out, px_per_stud, ref_s


def bar_edges(run, row, xs, bar_x):
	"""The bar's dark run in the row: nearest to where the wall point behind the bar's centre is."""
	c0 = int(np.argmin(np.abs(xs - bar_x)))
	dark = run.studio[row, :, 1] < 15
	if not dark[c0]:
		candidates = np.where(dark)[0]
		c0 = candidates[np.argmin(np.abs(candidates - c0))]
	left = c0
	while left > 0 and dark[left - 1]:
		left -= 1
	right = c0
	while right < len(dark) - 1 and dark[right + 1]:
		right += 1
	return left, right


def calibrate(experiment, tune=None, runs=None, no_ao=False):
	"""Renders an experiment with `ror studio calibrate` (its Studio captures from the cache): its
	output directory."""
	env = dict(os.environ)
	env.pop("ROR_NO_AO", None)
	if tune is not None:
		env["ROR_TUNE_AO"] = tune
	if no_ao:
		env["ROR_NO_AO"] = "1"
	if runs:
		env["ROR_CALIBRATE_FILTER"] = ",".join(runs)
	name = NAMES.get(experiment, "{}").format(MODELS) if experiment in NAMES else experiment
	result = subprocess.run([ROR, "studio", "calibrate", name], env=env, capture_output=True, text=True)
	if result.returncode != 0:
		raise RuntimeError(f"{experiment}: {result.stderr.strip()[-400:]}")
	return os.path.dirname(result.stdout.strip().splitlines()[-1].replace("\\\\?\\", ""))


def load(path):
	return LUT[np.asarray(Image.open(path).convert("RGB"))].sum(axis=2)


_references = {}


def unoccluded(experiment):
	"""Renders an experiment without the occlusion once, into out/ao-fit/noao (before the renders it
	is compared with: both write out/calibrate/<experiment>)."""
	if os.path.isdir(os.path.join(NOAO, experiment)):
		return
	source = calibrate(experiment, no_ao=True)
	for name in os.listdir(source):
		if os.path.isfile(os.path.join(source, name, "ror.png")):
			os.makedirs(os.path.join(NOAO, experiment, name), exist_ok=True)
			os.replace(os.path.join(source, name, "ror.png"), os.path.join(NOAO, experiment, name, "ror.png"))


def reference(experiment, run, rendered):
	"""ror's unoccluded radiance of a run, the surface pixels away from silhouettes, Studio's AO factor
	and the count of pixels Studio darkens."""
	key = (experiment, run)
	if key not in _references:
		n = load(os.path.join(NOAO, experiment, run, "ror.png"))
		root = np.sqrt(n)
		edge = ndimage.maximum_filter(np.abs(ndimage.sobel(root, 0)) + np.abs(ndimage.sobel(root, 1)), 5) > 0.08 * root.mean() + 0.02
		ok = (n > 0.05) & ~edge
		a_s = np.clip(load(os.path.join(rendered, run, "studio.png")) / np.maximum(n, 1e-6), 0, 1.5)
		# float32: some 60 runs stay cached
		_references[key] = (n.astype(np.float32), ok, a_s.astype(np.float32), max(int((ok & (a_s < 0.995)).sum()), 2000))
	return _references[key]


def score(tune, verbose=False):
	def job(item):
		experiment, (weight, runs) = item
		unoccluded(experiment)
		rendered = calibrate(experiment, tune, runs)
		names = runs or sorted(d for d in os.listdir(rendered) if os.path.isfile(os.path.join(rendered, d, "ror.png")))
		rows = []
		for run in names:
			n, ok, a_s, count = reference(experiment, run, rendered)
			a_r = np.clip(load(os.path.join(rendered, run, "ror.png")) / np.maximum(n, 1e-6), 0, 1.5)
			# Studio's level where ror sees no occlusion (exposure and material differences are not
			# the occlusion's); then the pixels either darkens by 2 % or more (the rest is the sky's and
			# the textures' noise), not brighter than open surfaces (edges misregistered)
			open_ = ok & (a_r > 0.999)
			level = float(np.median(a_s[open_])) if open_.sum() > 1000 else 1.0
			a_n = a_s / level
			used = ok & (a_n < 1.1) & ((a_n < 0.98) | (a_r < 0.98))
			d = (a_n - a_r)[used]
			rows.append((experiment, run, weight, float((d**2).sum()) / count, float(d.mean()) if d.size else 0.0))
		return rows

	rows = []
	# distinct experiments write distinct out/calibrate directories: ROR_AO_FIT_WORKERS at a time (one
	# by default: each render holds a GPU device of its own)
	with concurrent.futures.ThreadPoolExecutor(max_workers=int(os.environ.get("ROR_AO_FIT_WORKERS", "1"))) as pool:
		for part in pool.map(job, sorted(SETS.items(), key=lambda item: -len(item[1][1] or [0] * 5))):
			rows.extend(part)
	if verbose:
		for experiment, run, _, sse, mean in rows:
			print(f"   {experiment:12s} {run:10s} rms {np.sqrt(sse):.4f}  Studio - ror {mean:+.4f}")
	return float(np.sqrt(sum(w * sse for _, _, w, sse, _ in rows) / sum(w for _, _, w, _, _ in rows)))


def fit(specs, fixed):
	"""Nelder-Mead over the named parameters, each mapped into its (lo, hi) by a logistic."""
	history = []

	def settings(x):
		values = [lo + (hi - lo) / (1 + np.exp(-xi)) for xi, (_, _, lo, hi) in zip(x, specs)]
		return ",".join([f"{name}={value:.5g}" for (name, _, _, _), value in zip(specs, values)] + ([fixed] if fixed else []))

	def objective(x):
		tune = settings(x)
		value = score(tune)
		history.append((value, tune))
		print(f"{value:.5f}  {tune}", flush=True)
		return value

	x0 = np.array([np.log((start - lo) / (hi - start)) for _, start, lo, hi in specs])
	simplex = np.array([x0] + [x0 + 0.6 * np.eye(len(x0))[i] for i in range(len(x0))])
	optimize.minimize(objective, x0, method="Nelder-Mead", options={"maxfev": 220, "xatol": 0.02, "fatol": 1e-5, "initial_simplex": simplex})
	print("best %.5f  %s" % min(history))


# crease scene lines (ao-depth, ao-dolly, ao-size): (start, end) in the crease's own units
LINES = {
	"wall-up": ((-4.0, 0.0, 1.0), (-4.0, 1.5, 1.0)),
	"floor-out": ((-4.0, 0.0, 1.0), (-4.0, 0.0, -1.5)),
	"cube-floor": ((-1.25, 0.0, -1.0), (-1.25, 0.0, -2.5)),
	"cube-face": ((-1.25, 0.0, -1.0), (-1.25, 1.0, -1.0)),
	"wall-bar": ((0.75, 2.0, 1.0), (2.25, 2.0, 1.0)),
	"step-floor": ((2.0, 0.0, -2.0), (2.0, 0.0, -3.5)),
}
SIZES = [0.0625, 0.25, 1.0, 4.0]


def lines(experiment, labels, tune=None):
	unoccluded(experiment)
	rendered = calibrate(experiment, tune)
	metas = {r["label"]: r for r in json.load(open(os.path.join(rendered, "samples.json")))}
	fractions = np.array([0.0, 0.01, 0.02, 0.04, 0.07, 0.1, 0.15, 0.2, 0.3, 0.45, 0.6, 0.8, 1.0])
	for label in labels:
		scale, origin = 1.0, np.zeros(3)
		if label.startswith("x"):
			scale = float(label[1:])
			origin = np.array([SIZES.index(scale) * 200.0, 0.0, 0.0])
		n, _, a_s, _ = reference(experiment, label, rendered)
		a_r = load(os.path.join(rendered, label, "ror.png")) / np.maximum(n, 1e-6)
		H, W = n.shape
		c = metas[label]["camera"]
		eye = np.array(c[:3])
		R = np.array(c[3:]).reshape(3, 3)
		t = np.tan(np.radians(metas[label]["fov"]) / 2)
		print(f"== {label}: Studio / ror")
		for name, (a, b) in LINES.items():
			a, b = np.array(a), np.array(b)
			d = origin + (a + (b - a) * fractions[:, None]) * scale - eye
			z = d @ -R[:, 2]
			px = ((d @ R[:, 0]) / (z * t * W / H) + 1) / 2 * W - 0.5
			py = (1 - (d @ R[:, 1]) / (z * t)) / 2 * H - 0.5
			cells = []
			for x, y, f in zip(px, py, fractions):
				ix, iy = int(round(x)), int(round(y))
				if not (1 <= ix < W - 1 and 1 <= iy < H - 1):
					continue
				window = (slice(iy - 1, iy + 2), slice(ix - 1, ix + 2))
				cells.append(f"{f * np.linalg.norm(b - a):.2f}:{a_s[window].mean():.2f}/{a_r[window].mean():.2f}")
			print(f"   {name:10s} " + " ".join(cells))


if __name__ == "__main__":
	args = sys.argv[1:]
	if args[:1] == ["profiles"]:
		experiment, labels, bar_xs = args[1], args[2].split(","), [float(x) for x in args[3].split(",")]
		picks = [1, 2, 3, 4, 6, 8, 12, 16, 20, 24, 32, 40, 48, 64, 80, 100]
		for label in labels:
			out, px_per_stud, ref = profiles(experiment, label, bar_xs)
			print(f"== {label} ({px_per_stud:.1f} px a stud on the wall, open wall {ref:.3f}); Studio / ror")
			for bar_x, rows in out.items():
				print(f"   bar {bar_x:+6.2f}: " + " ".join(f"{int(d)}:{v:.2f}/{vr:.2f}" for d, _, v, vr, _ in rows if int(d) in picks))
	elif args[:1] == ["lines"]:
		lines(args[1], args[2].split(","), args[3] if len(args) > 3 else None)
	elif args[:1] == ["score"]:
		print(f"score {score(args[1] if len(args) > 1 else '', verbose=True):.5f}")
	elif args[:1] == ["fit"]:
		fixed = ""
		if "--fixed" in args:
			fixed = args[args.index("--fixed") + 1]
			del args[args.index("--fixed") : args.index("--fixed") + 2]
		specs = []
		for spec in args[1:]:
			name, _, rest = spec.partition("=")
			start, lo, hi = (float(v) for v in rest.split(":"))
			specs.append((name, start, lo, hi))
		fit(specs, fixed)
	else:
		raise SystemExit(__doc__)
