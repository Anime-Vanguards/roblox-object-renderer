"""Local lights from the light-* calibration experiments (notes/render.md "Local lights").

    python tools/analysis/light_fit.py falloff    # PointLight on a white floor: x / (n.l) against distance
    python tools/analysis/light_fit.py range      # light-range: fit the fade near Range (all Ranges at once)
    python tools/analysis/light_fit.py color      # light-range: the (0.2, 0.5, 0.8) light per channel; two lights' sum
    python tools/analysis/light_fit.py spot       # light-spot: the cone against θ, and a free power fit
    python tools/analysis/light_fit.py surface    # light-surface: margins beyond the face under the model
    python tools/analysis/light_fit.py shadow     # light-materials g2: the light shadow's edge on the floor

Readings are tone-curve inputs x (radiance() inverts the display value), divided by 2^EV. The model:
x = 1.98 · Brightness · Color^2.2 · fade(d, Range) · cone · (albedo · n·l + π · spec · n·l), spec the
SmoothPlastic GGX term of notes/render.md "Specular".
"""

import json
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares

from tone import radiance

ROOT = "out/calibrate"
K = 1.98
ROUGHNESS = 0.43
F0 = 0.039


class Run:
	def __init__(self, experiment, label):
		self.dir = os.path.join(ROOT, experiment, label)
		meta = next(r for r in json.load(open(os.path.join(ROOT, experiment, "samples.json"))) if r["label"] == label)
		self.meta = meta
		self.samples = {s["probe"]: s for s in meta["samples"]}
		c = meta["camera"]
		self.eye = np.array(c[:3])
		R = np.array(c[3:]).reshape(3, 3)
		self.right, self.up, self.look = R[:, 0], R[:, 1], -R[:, 2]
		self.fov = meta["fov"]
		self.ev = meta["environment"]["exposureCompensation"]
		self.studio = np.asarray(Image.open(os.path.join(self.dir, "studio.png")).convert("RGB")).astype(float)
		self.ror = np.asarray(Image.open(os.path.join(self.dir, "ror.png")).convert("RGB")).astype(float)
		self.H, self.W = self.studio.shape[:2]

	def rays(self):
		t = np.tan(np.radians(self.fov) / 2)
		ys, xs = np.mgrid[0 : self.H, 0 : self.W]
		nx = (xs + 0.5) / self.W * 2 - 1
		ny = 1 - (ys + 0.5) / self.H * 2
		d = self.look[None, None, :] + self.right[None, None, :] * (nx * t * self.W / self.H)[..., None] + self.up[None, None, :] * (ny * t)[..., None]
		return d / np.linalg.norm(d, axis=2, keepdims=True)

	def plane_hits(self, axis, value):
		"""World points where each pixel's ray meets the plane coordinate[axis] = value."""
		d = self.rays()
		s = (value - self.eye[axis]) / d[..., axis]
		return self.eye[None, None, :] + d * s[..., None], d


def spec(n, v, l):
	"""SmoothPlastic's direct specular (GGX, uncorrelated Smith-Schlick, F0 0.039), per unit n.l."""
	h = v + l
	h /= np.linalg.norm(h, axis=-1, keepdims=True)
	ndh = np.clip((n * h).sum(-1), 0, 1)
	ndv = np.clip((n * v).sum(-1), 1e-4, 1)
	ndl = np.clip((n * l).sum(-1), 0, 1)
	a = ROUGHNESS * ROUGHNESS
	a2 = a * a
	d = a2 / (np.pi * (ndh * ndh * (a2 - 1) + 1) ** 2)
	k = a * 0.5
	vis = 0.25 / ((ndv * (1 - k) + k) * (ndl * (1 - k) + k))
	return d * vis * F0


def smoothstep(a, b, x):
	t = np.clip((x - a) / (b - a), 0, 1)
	return t * t * (3 - 2 * t)


def fade(d, r, start=0.71, length=10.5):
	"""Full strength to start · Range, then a smoothstep to Range over an inverse square of the studs past the start."""
	return (1 - smoothstep(start * r, r, d)) / (1 + (np.maximum(d - start * r, 0) / length) ** 2)


def falloff():
	experiment = "light-falloff"
	labels = sorted({r["label"].rsplit("-ev", 1)[0] for r in json.load(open(os.path.join(ROOT, experiment, "samples.json")))})
	bins = np.array([0, 0.5, 1, 1.5, 2, 3, 4, 5, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 35, 40, 45, 50, 55, 60, 64])
	for label in labels:
		rows = []
		for ev in (0.0, -3.0):
			run = Run(experiment, f"{label}-ev{ev:g}")
			inputs = run.samples[label]["inputs"]
			origin = np.array(inputs["origin"])
			light = origin + np.array([0, inputs["height"], 0])
			hits, _ = run.plane_hits(1, 0.0)
			on = (hits[..., 0] > origin[0] - 5.5) & (hits[..., 0] < origin[0] + 63.5) & (np.abs(hits[..., 2] - origin[2]) < 13.5)
			to = light[None, None, :] - hits
			dist = np.linalg.norm(to, axis=2)
			ndl = to[..., 1] / dist
			for channel in range(3):
				x = radiance(run.studio[..., channel]) / 2**ev
				xr = radiance(run.ror[..., channel]) / 2**ev
				ok = on & (run.studio[..., channel] > 3) & (run.studio[..., channel] < 245)
				rows.append(np.stack([dist[ok], ndl[ok], x[ok], xr[ok], np.full(ok.sum(), channel), np.full(ok.sum(), ev)], axis=1))
		data = np.concatenate(rows)
		print(f"== {label}: {json.dumps({k: inputs[k] for k in ('brightness', 'range', 'color', 'height', 'shadows')})}")
		for channel in range(3):
			line = []
			for lo, hi in zip(bins[:-1], bins[1:]):
				m = (data[:, 4] == channel) & (data[:, 0] >= lo) & (data[:, 0] < hi)
				if m.sum() < 20:
					continue
				line.append(f"{(lo + hi) / 2:.1f}:{np.median(data[m, 2] / data[m, 1]):.4f}")
			print(f"   ch{channel} x/(n.l) by distance:", " ".join(line))


def collect_range(label, ev, channel=1):
	"""light-range floor pixels: per light the distance, n.l and specular; x measured and ror's."""
	run = Run("light-range", f"{label}-ev{ev:g}" if ev is not None else label)
	inputs = run.samples[label if ev is not None else "pair"]["inputs"]
	origin = np.array(inputs["origin"])
	hits, rays = run.plane_hits(1, 0.0)
	half = {"f4": 4, "f8": 8, "f16": 15, "f30": 27, "f45": 40, "f60": 54, "f60s": 54, "f120": 116, "color": 15}.get(label, 20)
	ok = (run.studio[..., channel] > 2) & (run.studio[..., channel] < 250)
	ok &= (np.abs(hits[..., 0] - origin[0]) < half - 0.3) & (np.abs(hits[..., 2] - origin[2]) < half - 0.3)
	p = hits[ok]
	v = -rays[ok]
	n = np.tile([0, 1.0, 0], (len(p), 1))
	lights = []
	for offset in inputs["offsets"]:
		to = origin + np.array([offset, inputs["height"], 0]) - p
		dist = np.linalg.norm(to, axis=1)
		l = to / dist[:, None]
		lights.append(dict(dist=dist, ndl=l[:, 1], spec=spec(n, v, l)))
	x = radiance(run.studio[..., channel][ok]) / 2**run.ev
	xr = radiance(run.ror[..., channel][ok]) / 2**run.ev
	return dict(x=x, xr=xr, lights=lights, range=inputs["range"], brightness=inputs["brightness"], p=p)


def range_fit():
	labels = ["f4", "f8", "f16", "f30", "f45", "f60", "f120"]
	data = []
	for label in labels:
		for ev in (0.0, -2.0):
			s = collect_range(label, ev)
			light = s["lights"][0]
			u = light["dist"] / s["range"]
			keep = (u > 0.3) & (u < 1.05)
			base = s["brightness"] * light["ndl"] * (1 + np.pi * light["spec"])
			data.append(dict(d=light["dist"][keep], R=s["range"], base=base[keep], x=s["x"][keep], label=label, ev=ev))

	def model(d, r, p):
		return (1 - smoothstep(p[0] * r, p[1] * r, d)) / (1 + (np.maximum(d - p[0] * r, 0) / p[2]) ** 2)

	def residual(p):
		return np.concatenate([(p[3] * e["base"] * model(e["d"], e["R"], p) - e["x"]) / np.maximum(p[3] * e["base"], 0.05) for e in data])

	fit = least_squares(residual, [0.72, 1.0, 10.0, 1.98])
	print(f"fade: start {fit.x[0]:.4f} R, end {fit.x[1]:.4f} R, inverse-square length {fit.x[2]:.3f} studs; K {fit.x[3]:.4f}; rms {np.sqrt((fit.fun ** 2).mean()):.4f}")
	print("measured / K B n.l (1 + pi spec) by d / Range (EV 0):")
	for e in data:
		if e["ev"] != 0.0:
			continue
		u = e["d"] / e["R"]
		r = e["x"] / (K * e["base"])
		line = []
		for lo in np.arange(0.70, 1.0, 0.02):
			m = (u >= lo) & (u < lo + 0.02)
			if m.sum() > 20:
				line.append(f"{lo + 0.01:.2f}:{np.median(r[m]):.3f}")
		print(f"   {e['label']:5s}", " ".join(line))


def color():
	def srgb(c):
		c = np.asarray(c, float)
		return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)

	for ev in (0.0, -2.0):
		ratios = []
		for channel in range(3):
			s = collect_range("color", ev, channel)
			light = s["lights"][0]
			keep = light["dist"] / s["range"] < 0.68
			ratios.append(np.median(s["x"][keep] / (K * light["ndl"] * (1 + np.pi * light["spec"]))[keep]))
		c = np.array([0.2, 0.5, 0.8])
		print(f"color (0.2, 0.5, 0.8) EV {ev:g}: x / K per channel {np.round(ratios, 4).tolist()}; c^2.2 {np.round(c**2.2, 4).tolist()} sRGB {np.round(srgb(c), 4).tolist()} c^2 {np.round(c * c, 4).tolist()}")
	for label, ev, ambient in (("pair", 0.0, 0.0), ("pair", -2.0, 0.0), ("pair-ambient", None, 0.5)):
		s = collect_range(label, ev)
		pred = 2 * ambient**2 + sum(K * light["ndl"].clip(0) * (1 + np.pi * light["spec"]) * fade(light["dist"], s["range"]) for light in s["lights"])
		both = (s["lights"][0]["dist"] < 12) & (s["lights"][1]["dist"] < 12)
		print(f"{label} EV {ev}: measured / summed model {np.median((s['x'] / pred)[s['x'] > 0.05]):.4f}, where both lights reach {np.median((s['x'] / pred)[both]):.4f}")


def collect_spot(angle, ev):
	run = Run("light-spot", f"a{angle}-ev{ev:g}")
	inputs = run.samples[f"spot{angle}"]["inputs"]
	origin = np.array(inputs["origin"])
	light = origin + np.array([0, 0, -inputs["distance"]])
	hits, rays = run.plane_hits(2, 0.0)
	ok = (run.studio[..., 1] > 3) & (run.studio[..., 1] < 245) & (np.abs(hits[..., 0] - origin[0]) < 44) & (np.abs(hits[..., 1]) < 44)
	p = hits[ok]
	to = light - p
	dist = np.linalg.norm(to, axis=1)
	l = to / dist[:, None]
	n = np.tile([0, 0, -1.0], (len(p), 1))
	x = radiance(run.studio[..., 1][ok]) / 2**ev
	theta = np.degrees(np.arccos(np.clip(-l[:, 2], -1, 1)))
	base = K * inputs["brightness"] * fade(dist, inputs["range"]) * -l[:, 2] * (1 + np.pi * spec(n, -rays[ok], l))
	return dict(theta=theta, x=x, base=base, half=float(angle) / 2)


def spot():
	for angle in ["30", "60", "90", "120", "150", "180"]:
		data = [collect_spot(angle, ev) for ev in (0.0, -2.0)]

		def cone_t(s):
			h = np.radians(s["half"])
			return np.clip((np.cos(np.radians(s["theta"])) - np.cos(h)) / (1 - np.cos(h)), 0, 1)

		fit = least_squares(lambda p: np.concatenate([(s["base"] * p[0] * cone_t(s) ** p[1] - s["x"]) / np.maximum(s["x"], 0.05) for s in data]), [1.0, 2.0])
		theta = np.concatenate([s["theta"] for s in data])
		ratio = np.concatenate([s["x"] / s["base"] for s in data])
		t2 = np.concatenate([cone_t(s) ** 2 for s in data])
		half = float(angle) / 2
		step = max(half / 8, 1.0)
		line = []
		for lo in np.arange(0, min(half + step, 80), step):
			m = (theta >= lo) & (theta < lo + step)
			if m.sum() > 20:
				line.append(f"{lo + step / 2:.0f}:{np.median(ratio[m]):.3f}/{np.median(t2[m]):.3f}")
		print(f"Angle {angle}: free fit {fit.x[0]:.3f} * t^{fit.x[1]:.3f}; measured / t^2 by theta (degrees):", " ".join(line))


def surface():
	for label in ["s1-d4", "s6-d4", "s12-d4", "s1-d12", "s6-d12", "s12-d12", "s6-d4-a30", "s6-d4-a180"]:
		rows = []
		for ev in (0.0, -2.0):
			run = Run("light-surface", f"{label}-ev{ev:g}")
			inputs = run.samples[label]["inputs"]
			origin = np.array(inputs["origin"])
			hits, rays = run.plane_hits(2, 0.0)
			ok = (run.studio[..., 1] > 3) & (run.studio[..., 1] < 245) & (np.abs(hits[..., 0] - origin[0]) < 44) & (np.abs(hits[..., 1]) < 44)
			p = hits[ok]
			side = inputs["side"]
			source = np.stack([np.clip(p[:, 0], origin[0] - side / 2, origin[0] + side / 2), np.clip(p[:, 1], -side / 2, side / 2), np.full(len(p), -inputs["distance"])], axis=1)
			to = source - p
			d = np.linalg.norm(to, axis=1)
			l = to / d[:, None]
			h = np.radians(inputs["angle"] / 2)
			t = np.clip((-l[:, 2] - np.cos(h)) / (1 - np.cos(h)), 0, 1)
			n = np.tile([0, 0, -1.0], (len(p), 1))
			g = radiance(run.studio[..., 1][ok]) / 2**ev / (K * inputs["brightness"] * fade(d, inputs["range"]) * (1 + np.pi * spec(n, -rays[ok], l)))
			off = np.maximum(np.abs(p[:, 0] - origin[0]), np.abs(p[:, 1])) - side / 2
			rows.append(np.stack([off, g, t * t, t * t * -l[:, 2]], axis=1))
		r = np.concatenate(rows)
		line = []
		for lo in np.arange(0, 26, 1.0):
			m = (r[:, 0] >= lo) & (r[:, 0] < lo + 1)
			if m.sum() > 5:
				line.append(f"{lo + 0.5:.1f}:{np.median(r[m, 1]):.3f}|{np.median(r[m, 2]):.3f}|{np.median(r[m, 3]):.3f}")
		print(f"{label:11s} beyond the edge: measured | t^2 | t^2 n.l:", " ".join(line))
	surface_receiver()


def _surface_case(experiment, label, ev):
	"""One SurfaceLight case: lit pixels with the face (centre, normal, axes, half extents)."""
	run = Run(experiment, f"{label}-ev{ev:g}")
	inputs = run.samples[label]["inputs"]
	origin = np.array(inputs["origin"])
	if experiment == "light-surface":
		hits, rays = run.plane_hits(2, 0.0)
		ok = (np.abs(hits[..., 0] - origin[0]) < 44) & (np.abs(hits[..., 1]) < 44)
		center, normal = np.array([origin[0], 0.0, -inputs["distance"]]), np.array([0, 0, 1.0])
		half, axes, receiver = np.array([inputs["side"] / 2] * 2), (np.array([1.0, 0, 0]), np.array([0, 1.0, 0])), np.array([0, 0, -1.0])
	else:
		hits, rays = run.plane_hits(1, 0.0)
		ok = (np.abs(hits[..., 0] - origin[0]) < 29) & (hits[..., 2] > -5) & (hits[..., 2] < 39)
		size, box = np.array(inputs["size"]), np.array(inputs["center"])
		if inputs["face"] == 2:
			normal, axes, half = np.array([0, 0, 1.0]), (np.array([1.0, 0, 0]), np.array([0, 1.0, 0])), size[[0, 1]] / 2
			center = box + normal * size[2] / 2
		else:
			normal, axes, half = np.array([0, -1.0, 0]), (np.array([1.0, 0, 0]), np.array([0, 0, 1.0])), size[[0, 2]] / 2
			center = box + normal * size[1] / 2
		receiver = np.array([0, 1.0, 0])
	ok &= (run.studio[..., 1] > 3) & (run.studio[..., 1] < 245)
	return dict(p=hits[ok], v=-rays[ok], x=radiance(run.studio[..., 1][ok]) / 2**ev, center=center, normal=normal, half=half, axes=axes, n=np.tile(receiver, (ok.sum(), 1)), inputs=inputs)


def surface_receiver():
	"""SurfaceLight's receiver factor F = x / (K B t² fade (1 + π spec)), everything from the nearest
	face point, against n.l to that point, n.l to the face centre, and min(1, n.l to the centre / the
	emission cosine towards it): rms per case, walls parallel to the face (light-surface) and a floor
	at right angles to it or under it (light-surface-floor)."""
	cases = [("light-surface", label) for label in ["s1-d4", "s6-d4", "s12-d4", "s1-d12", "s6-d12", "s12-d12", "s6-d4-a180"]]
	cases += [("light-surface-floor", label) for label in ["f1", "f6", "f12", "f6-a180", "down6"]]
	print("receiver factor F, rms of F - candidate: n.l nearest | n.l centre | min(1, n.l centre / cos emission centre)")
	for experiment, label in cases:
		res = [[], [], []]
		for ev in (0.0, -2.0):
			e = _surface_case(experiment, label, ev)
			a0, a1 = e["axes"]
			off = e["p"] - e["center"]
			nearest = e["center"] + a0 * np.clip(off @ a0, -e["half"][0], e["half"][0])[:, None] + a1 * np.clip(off @ a1, -e["half"][1], e["half"][1])[:, None]
			to = nearest - e["p"]
			d = np.linalg.norm(to, axis=1)
			l = to / d[:, None]
			h = np.radians(e["inputs"]["angle"] / 2)
			t = np.clip((-(l @ e["normal"]) - np.cos(h)) / (1 - np.cos(h)), 0, 1)
			base = K * e["inputs"]["brightness"] * fade(d, e["inputs"]["range"]) * t * t * (1 + np.pi * spec(e["n"], e["v"], l))
			keep = base > 0.05
			F = e["x"][keep] / base[keep]
			to_c = e["center"] - e["p"][keep]
			l_c = to_c / np.linalg.norm(to_c, axis=1)[:, None]
			ndl_c = np.clip((e["n"][keep] * l_c).sum(1), 0, 1)
			ndl_n = np.clip((e["n"][keep] * l[keep]).sum(1), 0, 1)
			ratio = np.minimum(1, ndl_c / np.clip(-(l_c @ e["normal"]), 1e-3, 1))
			for k, candidate in enumerate((ndl_n, ndl_c, ratio)):
				res[k].append(F - candidate)
		print(f"   {experiment:20s} {label:11s}", " | ".join(f"{np.sqrt((np.concatenate(r) ** 2).mean()):.3f}" for r in res))


def shadow():
	"""light-materials g2 (Shadows on) against g0: the lit share across the slab's shadow edge on the
	floor, by distance (studs) outside its geometric boundary, Studio and ror."""
	light = np.array([0.0, 8.0, -6.0])
	top, half = 5.2, 1.5
	for ev in (0.0, -2.0):
		lit = Run("light-materials", f"g0-ev{ev:g}")
		shadowed = Run("light-materials", f"g2-ev{ev:g}")
		hits, _ = shadowed.plane_hits(1, 0.0)
		p = hits - np.array([300.0, 0.0, 0.0])
		on = (np.abs(p[..., 0]) < 19) & (p[..., 2] > -9.5) & (p[..., 2] < 7.5)
		t = (light[1] - top) / light[1]
		qx = light[0] + (p[..., 0] - light[0]) * t
		qz = light[2] + (p[..., 2] - light[2]) * t
		beyond = (np.abs(qx - light[0]) - half) / t
		m = on & (np.abs(qz - light[2]) < half - 0.3) & (np.abs(beyond) < 0.4)
		for name, image, reference in (("studio", shadowed.studio, lit.studio), ("ror", shadowed.ror, lit.ror)):
			base = radiance(reference[..., 1])
			share = radiance(image[..., 1]) / np.maximum(base, 1e-6)
			k = m & (base > 0.02)
			line = []
			for lo in np.arange(-0.4, 0.4, 0.05):
				b = k & (beyond >= lo) & (beyond < lo + 0.05)
				if b.sum() > 3:
					line.append(f"{lo + 0.025:+.3f}:{np.median(share[b]):.2f}")
			print(f"EV {ev:g} {name:6s}", " ".join(line))


if __name__ == "__main__":
	{"falloff": falloff, "range": range_fit, "color": color, "spot": spot, "surface": surface, "shadow": shadow}[sys.argv[1]]()
