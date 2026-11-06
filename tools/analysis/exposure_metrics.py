"""Long exposures (notes/render.md "Playback", "Long exposures"): Studio's and ror's averages of an
effect over its timeline (`ror studio calibrate exposure:<timeline experiment>`, which writes each
time window's average per group of plays as 16-bit images of the displayed values), compared in linear
light per window and over the whole exposure, against the noise floor.

    python tools/analysis/exposure_metrics.py <experiment>... [--sheets]   # measure, write metrics.json
    python tools/analysis/exposure_metrics.py rank [<filter>] [--json <file>]  # every measured exposure
    python tools/analysis/exposure_metrics.py noise <experiment>...   # the floor against the plays
    python tools/analysis/exposure_metrics.py table [<filter>]        # the ranking as a markdown table

Over the effect's footprint (pixels where Studio's or ror's average differs from its own effect-free
average by more than 3 of 255, the union of the two):
- `iou`: the two footprints' overlap; `fx8` / `fx2`: pixels within 8 / 2 of 255; `image2` / `image8`:
  the same over the whole image;
- `light`: ror's summed light above its effect-free average over Studio's, in linear light (the tone
  curve inverted, resolve.wgsl's), and per channel (`light_rgb`); `dark`: the same for light taken
  away (smoke, negative LightEmission);
- `shift`: the distance between the two lights' centroids (a share of the image height), `spread`:
  ror's RMS radius of its light about its centroid over Studio's;
- `studio_failures`: the model's assets the capture Studio could not fetch (whatever uses them draws
  nothing there, while ror draws it: a capture artefact, flagged in `rank` and the table);
- `fx_rms`: the RMS difference of what the effect adds to each side's own effect-free average (in
  displayed levels), `z`: that over the RMS the noise alone would give (Studio's and ror's groups of
  plays against each other, scaled to the whole exposure's plays), `excess`: the part of `fx_rms`
  beyond the noise. Both only where the two effect-free averages agree within 4 of 255: where they do
  not, an effect covering the scene would carry the scene's difference as its own (an effect's black disc
  ends 1-2 px sooner in ror's scene; under the white disc the effect draws that ring was 98.5 % of
  the squared difference); `static` is the share of the footprint left out, `image_rms` the RMS of
  the images' difference over all of it.

Three comparisons per window and for the whole exposure: `floor`, Studio's group a against group b
(two independent exposures of half the plays each: how close any renderer could get at that count);
`half`, ror's groups against Studio's other groups (the same counts: comparable to the floor metric by
metric); `full`, all of Studio's plays against all of ror's (the systematic difference, with `z`).
`--sheets` writes Studio | ror | signed difference of what the effect adds (128 is equal, 2x gain),
one row for the whole exposure and one per window, to out/exposures/<experiment>.jpg. `noise` (for
exposures of 4 or more groups) gives the floor's metrics at a quarter and at half the plays.
"""

import glob
import json
import os
import sys

import cv2
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tone  # noqa: E402

THRESHOLD = 3.0
# effect-free averages within this many levels of each other count as one scene
STATIC = 4.0


def display(x):
	"""Displayed levels (0-255, unrounded, clipped) of tone curve inputs (tone.py's default curve)."""
	return tone.display(x).astype(np.float32)


# each 16-bit code's tone curve input (the exposure's images hold the displayed value, 0-65535; 65535
# reads as where the curve reaches 1, as exposure.rs averages it)
LEVELS16 = tone.radiance(np.arange(65536) / 257.0).astype(np.float32)


class Image:
	"""An average: linear light and its displayed levels."""

	def __init__(self, lin, shown=None):
		self.lin = lin
		self.shown = display(lin) if shown is None else shown


def load(path):
	"""A 16-bit exposure image, None when missing."""
	if not os.path.exists(path):
		return None
	codes = cv2.imread(path, cv2.IMREAD_UNCHANGED)
	if codes is None or codes.dtype != np.uint16:
		raise ValueError(f"{path} is not a 16-bit exposure")
	codes = codes[:, :, ::-1]
	return Image(LEVELS16[codes], codes.astype(np.float32) / 257.0)


def blend(parts):
	"""The frame-weighted mean of (Image, frames) pairs."""
	parts = [(image, weight) for image, weight in parts if image is not None and weight > 0]
	if not parts:
		return None
	total = sum(weight for _, weight in parts)
	return Image(sum(image.lin * (weight / total) for image, weight in parts))


class Exposure:
	def __init__(self, experiment):
		self.name = experiment
		self.directory = os.path.join("out", "calibrate", experiment, "exposure")
		with open(os.path.join(self.directory, "exposure.json")) as file:
			self.info = json.load(file)
		self.groups = self.info["groups"]
		self.windows = len(self.info["edges"]) + 1
		self.counts = np.array(self.info["counts"], dtype=np.float64)
		self.bare_counts = np.array(self.info["studioBare"], dtype=np.float64)

	def image(self, side, group, window):
		suffix = "bare" if group is None else "abcdefgh"[group]
		return load(os.path.join(self.directory, f"{side}-{suffix}-w{window}.png"))

	def label(self, window):
		if window is None:
			return f"0-{self.info['end']:g} s"
		edges = [0.0] + list(self.info["edges"]) + [self.info["end"]]
		return f"{edges[window]:.2f}-{edges[window + 1]:.2f} s"

	def slices(self):
		"""Each window's images (dict of (side, group or None) -> Image, the groups' frames), then the
		whole exposure's (each window weighed by its frames, the bare averages by their own)."""
		totals = {}
		for window in range(self.windows):
			counts = self.counts[:, window]
			if counts.sum() == 0:
				continue
			images = {}
			for side in ("studio", "ror"):
				for group in range(self.groups):
					images[(side, group)] = self.image(side, group, window)
				images[(side, None)] = self.image(side, None, window)
			for (side, group), image in images.items():
				weight = (self.bare_counts[window] if side == "studio" else counts.sum()) if group is None else counts[group]
				totals.setdefault((side, group), []).append((image, weight))
			yield window, images, counts
		whole = {key: blend(parts) for key, parts in totals.items()}
		yield None, whole, self.counts.sum(axis=1)


GRID = {}


def grid(shape):
	if shape not in GRID:
		GRID.clear()
		GRID[shape] = np.mgrid[0 : shape[0], 0 : shape[1]].astype(np.float32)
	return GRID[shape]


def compare(s, r, sb, rb):
	"""Metrics of r against s (Images), each with its own effect-free average; the effect's difference
	(displayed levels) and the footprints' union where the effect-free averages agree."""
	fs = np.abs(s.shown - sb.shown).max(axis=2) > THRESHOLD
	fr = np.abs(r.shown - rb.shown).max(axis=2) > THRESHOLD
	union = fs | fr
	agree = union & (np.abs(sb.shown - rb.shown).max(axis=2) <= STATIC)
	largest = np.abs(r.shown - s.shown).max(axis=2)
	delta = (r.shown - rb.shown) - (s.shown - sb.shown)
	m = {"pixels": int(union.sum()), "image2": float((largest <= 2).mean() * 100), "image8": float((largest <= 8).mean() * 100)}
	if not union.any():
		return m, delta, agree
	m["static"] = float(1.0 - agree.sum() / union.sum())
	m["image_rms"] = float(np.sqrt(((r.shown - s.shown)[union] ** 2).mean()))
	m["iou"] = float((fs & fr).sum() / union.sum())
	m["fx2"] = float((largest[union] <= 2).mean() * 100)
	m["fx8"] = float((largest[union] <= 8).mean() * 100)
	excess_s = np.clip(s.lin - sb.lin, 0, None) * union[:, :, None]
	excess_r = np.clip(r.lin - rb.lin, 0, None) * union[:, :, None]
	es, er = excess_s.sum(axis=(0, 1)), excess_r.sum(axis=(0, 1))
	m["light"] = float(er.sum() / max(es.sum(), 1e-6))
	m["light_rgb"] = [float(a / max(b, 1e-6)) for a, b in zip(er, es)]
	m["light_studio"] = float(es.sum())
	ks = (np.clip(sb.lin - s.lin, 0, None) * union[:, :, None]).sum()
	kr = (np.clip(rb.lin - r.lin, 0, None) * union[:, :, None]).sum()
	m["dark"] = float(kr / ks) if ks > 1e-3 * max(es.sum(), 1e-6) else None
	m["dark_share"] = float(ks / max(ks + es.sum(), 1e-6))
	ys, xs = grid(union.shape)
	ws, wr = excess_s.sum(axis=2), excess_r.sum(axis=2)
	if ws.sum() > 0 and wr.sum() > 0:
		cs = np.array([(xs * ws).sum(), (ys * ws).sum()]) / ws.sum()
		cr = np.array([(xs * wr).sum(), (ys * wr).sum()]) / wr.sum()
		m["shift"] = float(np.hypot(*(cr - cs)) / union.shape[0])
		m["shift_xy"] = [float(v / union.shape[0]) for v in cr - cs]
		ss = np.sqrt((((xs - cs[0]) ** 2 + (ys - cs[1]) ** 2) * ws).sum() / ws.sum())
		sr = np.sqrt((((xs - cr[0]) ** 2 + (ys - cr[1]) ** 2) * wr).sum() / wr.sum())
		m["spread"] = float(sr / max(ss, 1e-6))
	if agree.any():
		m["fx_rms"] = float(np.sqrt((delta[agree] ** 2).mean()))
		m["fx_mean_rgb"] = [float(v) for v in delta[agree].mean(axis=0)]
	return m, delta, agree


def pair_noise(images, union):
	"""Mean squared difference (displayed levels) over `union` between the groups' averages, pair by
	pair."""
	shown = [image.shown for image in images if image is not None]
	values = [((shown[i] - shown[j])[union] ** 2).mean() for i in range(len(shown)) for j in range(i + 1, len(shown))]
	return float(np.mean(values)) if values else None


def measure(exposure, sheets=False):
	result = {"experiment": exposure.name, "inner": exposure.info["inner"], "end": exposure.info["end"], "edges": exposure.info["edges"], "plays": exposure.info["plays"], "groups": exposure.groups, "frames": int(exposure.counts.sum()), "windows": []}
	# the model's assets the capture Studio could not fetch (exposure.rs): what uses them draws nothing
	# there, a capture artefact
	result["studio_failures"] = exposure.info.get("studioFailures")
	g = exposure.groups
	rows = []
	for window, images, counts in exposure.slices():
		rb = images[("ror", None)]
		sb = images.get(("studio", None)) or rb
		s_all = blend([(images[("studio", group)], counts[group]) for group in range(g)])
		r_all = blend([(images[("ror", group)], counts[group]) for group in range(g)])
		full, delta, union = compare(s_all, r_all, sb, rb)
		floor, _, _ = compare(images[("studio", 0)], images[("studio", 1)], sb, sb)
		halves = [compare(images[("studio", a)], images[("ror", b)], sb, rb)[0] for a, b in ((0, 1), (1, 0))]
		half = {key: float(np.mean([h[key] for h in halves])) for key in halves[0] if all(isinstance(h.get(key), (int, float)) for h in halves)}
		if union.any() and "fx_rms" in full:
			noise_s = pair_noise([images[("studio", group)] for group in range(g)], union)
			noise_r = pair_noise([images[("ror", group)] for group in range(g)], union)
			# var(S - S') between groups of N / G plays is 2 G σ² / N; the full comparison's noise is
			# σ_S² / N + σ_R² / N
			if noise_s is not None and noise_r is not None and noise_s + noise_r > 0:
				expected = (noise_s + noise_r) / (2.0 * g)
				full["noise_rms"] = float(np.sqrt(expected))
				full["z"] = float(full["fx_rms"] / np.sqrt(expected))
				full["excess"] = float(np.sqrt(max(full["fx_rms"] ** 2 - expected, 0.0)))
				full["noise_ror_over_studio"] = float(np.sqrt(noise_r / max(noise_s, 1e-9)))
		static = compare(sb, rb, sb, sb)[0] if images.get(("studio", None)) is not None else {}
		entry = {"window": window, "span": exposure.label(window), "frames": int(counts.sum()), "full": full, "half": half, "floor": floor, "static_image2": static.get("image2")}
		if window is None:
			result["whole"] = entry
		else:
			result["windows"].append(entry)
		if sheets:
			rows.append((exposure.label(window), s_all, r_all, delta))
	if sheets and rows:
		sheet(exposure.name, [rows[-1]] + rows[:-1])
	return result


def noise(exposure):
	"""The floor at a quarter and at half the plays: single groups against each other (every pair), and
	the even groups against the odd ones."""
	g = exposure.groups
	print(f"{exposure.name}: {exposure.info['plays']} plays in {g} groups")
	print(f"  {'window':14s} {'plays':>5s} | {'fx_rms':>6s} {'iou':>5s} {'light':>6s} {'fx8':>5s} {'fx2':>5s} {'shift':>6s} {'spread':>6s}")
	for window, images, counts in exposure.slices():
		sb = images.get(("studio", None)) or images[("ror", None)]
		pairs = [compare(images[("studio", a)], images[("studio", b)], sb, sb)[0] for a in range(g) for b in range(a + 1, g)]
		even = blend([(images[("studio", group)], counts[group]) for group in range(0, g, 2)])
		odd = blend([(images[("studio", group)], counts[group]) for group in range(1, g, 2)])
		halves = [compare(even, odd, sb, sb)[0]]
		for plays, metrics in ((exposure.info["plays"] // g, pairs), (exposure.info["plays"] // 2, halves)):
			values = {key: np.mean([m[key] for m in metrics if isinstance(m.get(key), float)]) if any(isinstance(m.get(key), float) for m in metrics) else None for key in ("fx_rms", "iou", "light", "fx8", "fx2", "shift", "spread")}
			# the light ratio's spread: its mean distance from 1 in log terms
			lights = [abs(np.log(max(m["light"], 1e-6))) for m in metrics if "light" in m]
			deviation = float(np.exp(np.mean(lights))) if lights else None
			print(f"  {exposure.label(window):14s} {plays:5d} | {fmt(values['fx_rms'], 2):>6s} {fmt(values['iou'], 2):>5s} {fmt(deviation):>6s} {fmt(values['fx8'], 1):>5s} {fmt(values['fx2'], 1):>5s} {fmt(values['shift']):>6s} {fmt(values['spread'], 2):>6s}")


def sheet(name, rows):
	from PIL import Image as Picture, ImageDraw

	os.makedirs(os.path.join("out", "exposures"), exist_ok=True)
	tiles = []
	for label, s, r, delta in rows:
		row = []
		for caption, image in (("Studio", np.round(s.shown)), ("ror", np.round(r.shown)), ("ror - Studio (effect)", 128 + 2 * delta)):
			tile = Picture.fromarray(np.clip(image, 0, 255).astype(np.uint8))
			tile = tile.resize((tile.width // 2, tile.height // 2), Picture.LANCZOS)
			draw = ImageDraw.Draw(tile)
			draw.rectangle((0, 0, 300, 24), fill=(0, 0, 0))
			draw.text((8, 6), f"{caption}  {label}", fill=(255, 255, 255))
			row.append(tile)
		tiles.append(row)
	width, height = tiles[0][0].size
	picture = Picture.new("RGB", (width * 3, height * len(tiles)))
	for y, row in enumerate(tiles):
		for x, tile in enumerate(row):
			picture.paste(tile, (x * width, y * height))
	path = os.path.join("out", "exposures", name.removeprefix("exposure-") + ".jpg")
	picture.save(path, quality=88)
	print(path)


def fmt(value, digits=3):
	if value is None:
		return "  -  "
	if isinstance(value, (float, np.floating)):
		return f"{value:.{digits}f}"
	return str(value)


def report(result):
	print(f"{result['experiment']}: {result['plays']} plays in {result['groups']} groups, {result['frames']} frames, to {result['end']} s")
	print(f"  {'window':14s} {'frames':>6s} | {'z':>6s} {'excess':>6s} {'noise':>6s} | {'light':>6s} {'floor':>6s} | {'iou':>5s} {'floor':>5s} | {'fx8':>5s} {'half':>5s} {'floor':>5s} | {'shift':>5s} {'spread':>6s} | light r/g/b")
	for entry in result["windows"] + [result["whole"]]:
		full, floor, half = entry["full"], entry["floor"], entry["half"]
		rgb = "/".join(fmt(v, 2) for v in full.get("light_rgb", []))
		print(
			f"  {entry['span']:14s} {entry['frames']:6d} | {fmt(full.get('z'), 2):>6s} {fmt(full.get('excess'), 2):>6s} {fmt(full.get('noise_rms'), 2):>6s} | {fmt(full.get('light')):>6s} {fmt(floor.get('light')):>6s} | {fmt(full.get('iou'), 2):>5s} {fmt(floor.get('iou'), 2):>5s} | {fmt(full.get('fx8'), 1):>5s} {fmt(half.get('fx8'), 1):>5s} {fmt(floor.get('fx8'), 1):>5s} | {fmt(full.get('shift')):>5s} {fmt(full.get('spread'), 2):>6s} | {rgb}"
		)


def rank(args):
	needle = args[0] if args and not args[0].startswith("--") else ""
	results = []
	for path in sorted(glob.glob(os.path.join("out", "calibrate", "exposure-*", "exposure", "metrics.json"))):
		if needle in path:
			with open(path) as file:
				results.append(json.load(file))
	results.sort(key=lambda result: -(result["whole"]["full"].get("z") or 0))
	print(f"{'exposure':62s} {'z':>6s} {'max z':>6s} {'excess':>6s} {'light':>6s} {'floor':>6s} {'iou':>5s} {'floor':>5s} {'fx8':>5s} {'floor':>5s} {'shift':>6s} {'spread':>6s} {'static':>6s} {'fails':>5s}")
	for result in results:
		whole = result["whole"]
		full, floor = whole["full"], whole["floor"]
		worst = max((entry["full"].get("z") or 0 for entry in result["windows"]), default=0)
		print(f"{result['experiment'].removeprefix('exposure-')[:62]:62s} {fmt(full.get('z'), 2):>6s} {fmt(worst, 2):>6s} {fmt(full.get('excess'), 2):>6s} {fmt(full.get('light')):>6s} {fmt(floor.get('light')):>6s} {fmt(full.get('iou'), 2):>5s} {fmt(floor.get('iou'), 2):>5s} {fmt(full.get('fx8'), 1):>5s} {fmt(floor.get('fx8'), 1):>5s} {fmt(full.get('shift')):>6s} {fmt(full.get('spread'), 2):>6s} {fmt(full.get('static'), 3):>6s} {fmt(None if result.get('studio_failures') is None else len(result['studio_failures'])):>5s}")
		if "--windows" in args:
			# each window's light ratio and z, and the whole exposure's light per channel
			cells = [f"{entry['span']} {fmt(entry['full'].get('light'), 2)}/{fmt(entry['full'].get('z'), 1)}" for entry in result["windows"] if entry["full"].get("light_studio", 0) > 0.01 * (full.get("light_studio") or 1)]
			print(f"    light/z by window: {'; '.join(cells)} | rgb {'/'.join(fmt(v, 2) for v in full.get('light_rgb', []))}")
	if "--json" in args:
		with open(args[args.index("--json") + 1], "w") as file:
			json.dump(results, file, indent=1)


def differs(result):
	"""What differs in an exposure, in words: brightness, colour, size, place, coverage, darkening and
	timing, each beyond what its floor (Studio against Studio) shows."""
	full, floor = result["whole"]["full"], result["whole"]["floor"]
	notes = []
	failures = result.get("studio_failures") or []
	if failures:
		users = sorted({user.split(" ", 1)[-1] for _, names in failures for user in names})
		notes.append(f"capture artefact: {len(failures)} of its assets fail to load in the capture Studio ({', '.join(users)[:160]})")
	light, light_floor = full.get("light"), floor.get("light") or 1.0
	margin = max(0.1, 3 * abs(np.log(max(light_floor, 1e-6))))
	if light is not None and abs(np.log(max(light, 1e-6))) > margin:
		notes.append(f"{'too bright' if light > 1 else 'too dim'} ({light:.2f} of Studio's light)")
	rgb = full.get("light_rgb")
	if rgb and min(rgb) > 0:
		strongest, weakest = int(np.argmax(rgb)), int(np.argmin(rgb))
		if rgb[strongest] / rgb[weakest] > 1.15:
			names = "rgb"
			notes.append(f"colour: {names[strongest]} {rgb[strongest]:.2f} against {names[weakest]} {rgb[weakest]:.2f}")
	if full.get("spread") is not None and abs(np.log(full["spread"])) > 0.07:
		notes.append(f"{'larger' if full['spread'] > 1 else 'smaller'} (spread {full['spread']:.2f})")
	if (full.get("shift") or 0) > max(0.02, 3 * (floor.get("shift") or 0)):
		dx, dy = full.get("shift_xy", [0, 0])
		notes.append(f"moved {full['shift']:.3f} of the height ({'right' if dx > 0 else 'left'} {abs(dx):.3f}, {'down' if dy > 0 else 'up'} {abs(dy):.3f})")
	if full.get("iou") is not None and floor.get("iou") is not None and full["iou"] < floor["iou"] - 0.08:
		notes.append(f"coverage IoU {full['iou']:.2f} against {floor['iou']:.2f}")
	if (full.get("static") or 0) > 0.05:
		notes.append(f"the scene without the effect differs under {full['static'] * 100:.0f} % of its footprint (left out of z)")
	if (full.get("dark_share") or 0) > 0.15 and full.get("dark") is not None and abs(np.log(max(full["dark"], 1e-6))) > 0.2:
		notes.append(f"darkening {full['dark']:.2f} of Studio's")
	# timing: windows carrying light whose ratio stands apart from the whole's
	lit = [entry for entry in result["windows"] if (entry["full"].get("light_studio") or 0) > 0.05 * (full.get("light_studio") or 1) and entry["full"].get("light") is not None]
	odd = [entry for entry in lit if abs(np.log(max(entry["full"]["light"], 1e-6) / max(light or 1, 1e-6))) > max(0.2, 3 * abs(np.log(max(entry["floor"].get("light") or 1, 1e-6))))]
	if odd:
		notes.append("timing: " + ", ".join(f"{entry['span']} {entry['full']['light']:.2f}" for entry in odd))
	return notes


def table(args):
	"""The ranked exposures as a markdown table: z (the worst window's), the light and its floor, per
	channel, IoU and its floor, the centroid's shift and the spread."""
	needle = args[0] if args and not args[0].startswith("--") else ""
	results = []
	for path in sorted(glob.glob(os.path.join("out", "calibrate", "exposure-*", "exposure", "metrics.json"))):
		if needle in path:
			with open(path) as file:
				results.append(json.load(file))
	results.sort(key=lambda result: -(result["whole"]["full"].get("z") or 0))
	print("| effect | z (worst window) | light (floor) | light r / g / b | IoU (floor) | shift / spread | what differs |")
	print("|---|---|---|---|---|---|---|")
	for result in results:
		full, floor = result["whole"]["full"], result["whole"]["floor"]
		worst = max(result["windows"], key=lambda entry: entry["full"].get("z") or 0)
		name = result["experiment"].removeprefix("exposure-vfx-attack-").removeprefix("auto-")
		rgb = " / ".join(fmt(v, 2) for v in full.get("light_rgb", []))
		print(f"| {name} | {fmt(full.get('z'), 1)} ({fmt(worst['full'].get('z'), 1)} at {worst['span']}) | {fmt(full.get('light'), 2)} ({fmt(floor.get('light'), 2)}) | {rgb} | {fmt(full.get('iou'), 2)} ({fmt(floor.get('iou'), 2)}) | {fmt(full.get('shift'), 3)} / {fmt(full.get('spread'), 2)} | {'; '.join(differs(result))} |")


def main():
	args = sys.argv[1:]
	if not args:
		print(__doc__)
		return
	if args[0] == "rank":
		rank(args[1:])
		return
	if args[0] == "table":
		table(args[1:])
		return
	if args[0] == "noise":
		for experiment in args[1:]:
			noise(Exposure(experiment))
		return
	sheets = "--sheets" in args
	for experiment in [arg for arg in args if not arg.startswith("--")]:
		exposure = Exposure(experiment)
		result = measure(exposure, sheets)
		with open(os.path.join(exposure.directory, "metrics.json"), "w") as file:
			json.dump(result, file, indent=1)
		report(result)


if __name__ == "__main__":
	main()
