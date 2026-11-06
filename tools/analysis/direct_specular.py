"""The direct specular law (notes/render.md "Direct specular") fitted jointly to the sun's term on
spheres and tiles: metal-sun (the sun run less the sky run, EnvironmentSpecularScale 1), sa-sun (the
sun alone, each sphere's unlit side as its floor), ibl-scale (sun-s1 less specular-s1, sun-s0.5 less
specular-s0.5) and sun-grazing (horizontal tiles seen 5-35 degrees up, the sun run less the sky run).

A candidate law's prediction for a pixel is ror's own render times the candidate over the law ror drew
with (so the tone curve, the bloom and ror's other terms stay as rendered). A bin is a sphere's
10-degree band of the half angle or a tile's 5-degree band; its residual is the log of Studio's mean
over the prediction's. Spheres whose render departs from ror's law by over 4 % (the bloom of bright
smooth highlights) and bins under 0.003 of radiance in Studio are left out.

    python tools/analysis/direct_specular.py show [name=value ...]          rms per experiment, tiles per view
    python tools/analysis/direct_specular.py before                         the law before the metal round
    python tools/analysis/direct_specular.py metal-round                    the metal round's law (one compensation)
    python tools/analysis/direct_specular.py fit <names> <start> [name=value ...]

Parameters, ror's in render::materials (DIRECT_SPECULAR, DIRECT_REFLECTANCE, DIRECT_METAL): w width k0
k1 (the lobe: GGX of width width (w + (1 - w) r^2), Schlick-GGX visibility with k = k0 + k1 r), g c
fresnel compensation (the reflectance: Schlick's rise of weight fresnel on the dielectric's F0, then
times g (1 + c a^2), and on the metal's albedo), a b metal_width separate (the metallic share: its
lobe metal_width times as wide; multiple scattering's energy per share, the dielectric's 1 + F0 (1 / E
- 1) with E the split-sum table at 0.075 + 0.9 r, the metal's 1 + compensation (1 / E_m - 1) with E_m
at a + b r; separate 0: one compensation of the mixed reflectance). The metalness fades with the view depth and
scales with EnvironmentSpecularScale ("Specular fade"). Replay the experiments first (`ror studio
calibrate metal-sun`, `sa-sun`, `ibl-scale`, `sun-grazing`; captures are cached): the prediction
assumes they were drawn with ror's law.
"""

import glob
import json
import math
import os
import sys

import numpy as np
from PIL import Image
from scipy.optimize import least_squares

from tone import radiance

ROOT = os.path.join("out", "calibrate")
F0 = 0.039
FADE = 1214.0
ROR = dict(w=0.038, width=1.026, k0=0.377, k1=0.159, g=1.089, c=0.945, fresnel=0.894, compensation=0.5607, a=0.2414, b=0.8079, metal_width=1.0181, separate=1.0)
# the metal round's law: one compensation of the mixed reflectance, read with the dielectric's table
METAL_ROUND = dict(ROR, compensation=1.064, a=0.075, b=0.9, metal_width=1.0, separate=0.0)
# the law before: Schlick-GGX k = kk (r + 1)^2 / 8, no Fresnel rise, the metal's own albedo compensated
BEFORE = dict(w=0.038, g=1.10, c=1.0, kk=1.17)
BALLS = {272: 0.41, 256: 0.6}


def studio_table():
	"""Studio's split-sum table, [n.v row, roughness column] -> (scale, bias)."""
	paths = sorted(glob.glob(os.path.expandvars(r"%LOCALAPPDATA%\Roblox\Versions\*\PlatformContent\pc\textures\brdfLUT.dds")), key=os.path.getmtime)
	data = open(paths[-1], "rb").read()
	size = int.from_bytes(data[16:20], "little")
	return np.frombuffer(data[148 : 148 + size * size * 4], dtype=np.float16).astype(np.float64).reshape(size, size, 2)


TABLE = studio_table()


def directional_albedo(ndv, rough, a=0.075, b=0.9):
	"""The table's scale + bias at roughness a + b r (the prefiltered sky level's: 0.075 + 0.9 r),
	bilinear."""
	size = TABLE.shape[0]
	y = np.clip(ndv * size - 0.5, 0, size - 1)
	x = np.clip(np.broadcast_to(np.clip(a + b * rough, 0.0, 1.0), np.shape(ndv)) * size - 0.5, 0, size - 1)
	y0, x0 = np.floor(y).astype(int), np.floor(x).astype(int)
	y1, x1 = np.minimum(y0 + 1, size - 1), np.minimum(x0 + 1, size - 1)
	fy, fx = (y - y0)[..., None], (x - x0)[..., None]
	ab = (TABLE[y0, x0] * (1 - fx) + TABLE[y0, x1] * fx) * (1 - fy) + (TABLE[y1, x0] * (1 - fx) + TABLE[y1, x1] * fx) * fy
	return np.maximum(ab[..., 0] + ab[..., 1], 0.05)


def sun_direction(clock, latitude):
	h = (clock - 12) / 24 * 2 * math.pi
	t = math.radians(latitude - 23.5)
	return np.array([-math.sin(h) * math.cos(t), math.cos(h) * math.cos(t), math.sin(t)])


def rays(meta, width, height):
	"""The camera's eye and every pixel's unit direction."""
	c = meta["camera"]
	eye = np.array(c[:3])
	rot = np.array(c[3:]).reshape(3, 3)
	right, up, look = rot[:, 0], rot[:, 1], -rot[:, 2]
	t = math.tan(math.radians(meta["fov"]) / 2)
	ys, xs = np.mgrid[0:height, 0:width]
	nx = (xs + 0.5) / width * 2 - 1
	ny = 1 - (ys + 0.5) / height * 2
	d = look[None, None, :] + right[None, None, :] * (nx * t * width / height)[..., None] + up[None, None, :] * (ny * t)[..., None]
	return eye, d / np.linalg.norm(d, axis=2, keepdims=True)


def load(experiment, run, side):
	return radiance(np.asarray(Image.open(os.path.join(ROOT, experiment, run, f"{side}.png")).convert("RGB")).astype(float))


def material(inputs):
	"""(albedo, metalness, roughness) of a sphere probe, or None."""
	kind = inputs.get("kind")
	if kind == "tinted":
		return 0.971 * inputs["tint"] ** 2, inputs["metalness"], inputs["roughness"]
	if kind == "appearance":
		color = inputs.get("color", [0, 0, 0])
		if color[0] != color[1] or inputs.get("roughness") is None:
			return None
		return 0.971 * (color[0] / 255) ** 2, inputs.get("metalness", 0.0), inputs["roughness"]
	if kind == "ball" and inputs.get("material") in BALLS:
		return (inputs.get("color", [0, 0, 0])[0] / 255) ** 2, 0.0, BALLS[inputs["material"]]
	return None


def sphere_rows():
	sources = [
		("metal-sun", [(g, f"{g}-sun", f"{g}-sky", 1.0) for g in ("v0", "v45", "v90", "v135", "d30", "d150")]),
		("sa-sun", [(g, g, None, 0.0) for g in ("view0", "view30", "view60", "view90", "near0")]),
		("ibl-scale", [("s1", "sun-s1", "specular-s1", 1.0), ("s05", "sun-s0.5", "specular-s0.5", 0.5)]),
	]
	rows = []
	for experiment, groups in sources:
		path = os.path.join(ROOT, experiment, "samples.json")
		if not os.path.exists(path):
			print(f"({experiment} not replayed: left out)")
			continue
		metas = {m["label"]: m for m in json.load(open(path))}
		for group, with_sun, without, scale in groups:
			if with_sun not in metas:
				continue
			meta = metas[with_sun]
			l = sun_direction(meta["environment"]["clockTime"], meta["environment"]["latitude"])
			S, R = load(experiment, with_sun, "studio").mean(-1), load(experiment, with_sun, "ror").mean(-1)
			if without:
				S, R = S - load(experiment, without, "studio").mean(-1), R - load(experiment, without, "ror").mean(-1)
			height, width = S.shape
			eye, d = rays(meta, width, height)
			forward = -np.array(meta["camera"][3:]).reshape(3, 3)[:, 2]
			for sample in meta["samples"]:
				inputs = sample["inputs"]
				found = material(inputs) if "center" in inputs else None
				if found is None:
					continue
				c, radius = np.array(inputs["center"]), inputs["radius"]
				oc = eye - c
				b = (d * oc).sum(-1)
				mask = b * b - ((oc * oc).sum() - (0.95 * radius) ** 2) > 0
				s = -b - np.sqrt(np.maximum(b * b - ((oc * oc).sum() - radius * radius), 0))
				p = eye + d * s[..., None]
				n = (p - c) / radius
				ndl, ndv = (n * l).sum(-1), (n * -d).sum(-1)
				unlit = mask & (ndl < -0.15) & (ndv > 0.1)
				mask &= (ndl > 0.05) & (ndv > 0.1)
				if mask.sum() < 50:
					continue
				# runs with nothing subtracted: the sphere's unlit side is the floor (the bloom's widest
				# levels, the display's black)
				floor_s = float(np.median(S[unlit])) if without is None and unlit.sum() > 30 else 0.0
				floor_r = float(np.median(R[unlit])) if without is None and unlit.sum() > 30 else 0.0
				h = -d + l
				h /= np.linalg.norm(h, axis=-1, keepdims=True)
				rows.append(dict(experiment=experiment, group=group, label=sample["probe"], kind="sphere", albedo=found[0], metal=found[1], rough=found[2], s=scale,
					ndh=(n * h).sum(-1)[mask], ndv=ndv[mask], ndl=ndl[mask], depth=((p - eye) * forward).sum(-1)[mask], studio=S[mask] - floor_s, ror=R[mask] - floor_r))
	return rows


def tile_rows(margin=0.6):
	path = os.path.join(ROOT, "sun-grazing", "samples.json")
	if not os.path.exists(path):
		print("(sun-grazing not replayed: left out)")
		return []
	rows = []
	for meta in json.load(open(path)):
		if not meta["label"].endswith("-sun"):
			continue
		view = meta["label"][:-4]
		l = sun_direction(meta["environment"]["clockTime"], meta["environment"]["latitude"])
		shown_s = np.asarray(Image.open(os.path.join(ROOT, "sun-grazing", meta["label"], "studio.png")).convert("RGB")).astype(float)
		shown_r = np.asarray(Image.open(os.path.join(ROOT, "sun-grazing", meta["label"], "ror.png")).convert("RGB")).astype(float)
		S = radiance(shown_s).mean(-1) - load("sun-grazing", f"{view}-sky", "studio").mean(-1)
		R = radiance(shown_r).mean(-1) - load("sun-grazing", f"{view}-sky", "ror").mean(-1)
		unsaturated = (shown_s.max(-1) < 245) & (shown_r.max(-1) < 245)
		height, width = S.shape
		eye, d = rays(meta, width, height)
		forward = -np.array(meta["camera"][3:]).reshape(3, 3)[:, 2]
		t = -eye[1] / np.where(np.abs(d[..., 1]) > 1e-6, d[..., 1], -1e-6)
		p = eye + d * t[..., None]
		own = [s for s in meta["samples"] if s["probe"].startswith(view + "_")]
		centers = [np.array(s["inputs"]["center"]) for s in own]
		# the top faces, in from their edges and outside the corners where neighbours overlap
		inside = sum(((np.abs(p[..., 0] - c[0]) < 4) & (np.abs(p[..., 2] - c[2]) < 4)).astype(int) for c in centers)
		h = -d + l
		h /= np.linalg.norm(h, axis=-1, keepdims=True)
		for sample, c in zip(own, centers):
			inputs = sample["inputs"]
			mask = (t > 0) & (np.abs(p[..., 0] - c[0]) < 4 - margin) & (np.abs(p[..., 2] - c[2]) < 4 - margin) & (inside == 1) & unsaturated
			if mask.sum() < 30 or S[mask].mean() < 0.003:
				continue
			rows.append(dict(experiment="sun-grazing", group=view, label=sample["probe"][len(view) + 1 :], kind="tile", albedo=float(inputs["albedo"]), metal=float(inputs["metalness"]), rough=float(inputs["roughness"]), s=1.0,
				ndh=h[..., 1][mask], ndv=-d[..., 1][mask], ndl=np.full(mask.sum(), l[1]), depth=((p - eye) * forward).sum(-1)[mask], studio=S[mask], ror=R[mask]))
	return rows


def terms(row):
	"""The fade k, the effective metalness m, the diffuse term and the dielectric F0 mix of a row."""
	k = np.clip(1 - row["depth"] / FADE, 0, 1)
	m = k * row["metal"] * row["s"]
	f0 = F0 * (1 - m) + row["albedo"] * m
	diffuse = (1 - m) * (1 - k * row["s"] * f0) * row["albedo"] / math.pi * row["ndl"]
	return k, m, diffuse


def ggx(ndh, a):
	dd = ndh * ndh * (a * a - 1) + 1
	return a * a / (math.pi * dd * dd)


def law(row, p):
	"""ror's law (shaders/mesh.wgsl `direct_light`) at parameters p: the sun's term per pixel for a
	sun of unit irradiance."""
	_, m, diffuse = terms(row)
	rough, ndh, ndv, ndl = row["rough"], row["ndh"], row["ndv"], row["ndl"]
	a = (p["w"] + (1 - p["w"]) * rough * rough) * p["width"]
	k = np.minimum(p["k0"] + p["k1"] * rough, 1.0)
	visibility = 0.25 / ((ndv * (1 - k) + k) * (ndl * (1 - k) + k))
	vdh = np.clip((ndv + ndl) / np.maximum(2 * ndh, 1e-3), 0, 1)
	rise = p["fresnel"] * (1 - vdh) ** 5
	gain = p["g"] * (1 + p["c"] * a * a)
	f_d = ggx(ndh, a) * gain * (F0 + (1 - F0) * rise)
	f_m = ggx(ndh, a * p["metal_width"]) * (row["albedo"] + (1 - row["albedo"]) * rise)
	scatter = 1 / directional_albedo(ndv, rough) - 1
	scatter_m = 1 / directional_albedo(ndv, rough, p["a"], p["b"]) - 1
	if p["separate"] > 0.5:
		# each share keeps its own multiple scattering's energy
		specular = (1 - m) * f_d * (1 + F0 * scatter) + m * f_m * (1 + p["compensation"] * scatter_m)
	else:
		specular = ((1 - m) * f_d + m * f_m) * (1 + ((1 - m) * F0 + m * p["compensation"]) * scatter_m)
	return diffuse + specular * visibility * ndl


def law_before(row, p=BEFORE):
	_, m, diffuse = terms(row)
	rough, ndh, ndv, ndl = row["rough"], row["ndh"], row["ndv"], row["ndl"]
	a = p["w"] + (1 - p["w"]) * rough * rough
	k = min(p["kk"] * (rough + 1) ** 2 / 8, 1.0)
	visibility = 0.25 / ((ndv * (1 - k) + k) * (ndl * (1 - k) + k))
	f0 = F0 * (1 - m) + row["albedo"] * m
	f = f0 + F0 * (p["g"] * (1 + p["c"] * a * a) - 1) * (1 - m)
	return diffuse + ggx(ndh, a) * visibility * f * (1 + f0 * (1 / directional_albedo(ndv, rough) - 1)) * ndl


def rows():
	"""Every sphere and tile row; those whose render is not ror's law (over 4 % off their experiment's
	median scale: a bright smooth highlight's bloom, a dim tile's noise) left out."""
	every = sphere_rows() + tile_rows()
	scales = {}
	for row in every:
		row["scale"] = float(np.median(row["ror"] / np.maximum(law(row, ROR), 1e-9)))
		scales.setdefault(row["experiment"], []).append(row["scale"])
	reference = {experiment: float(np.median(values)) for experiment, values in scales.items()}
	return [row for row in every if abs(row["scale"] / reference[row["experiment"]] - 1) < 0.04]


def cells(rows, predict):
	"""(experiment, group, label, band, Studio's mean, the prediction's mean) per bin."""
	out = []
	for row in rows:
		prediction = row["ror"] * predict(row) / np.maximum(law(row, ROR), 1e-9)
		angle = np.degrees(np.arccos(np.clip(row["ndh"], -1, 1)))
		step, least = (5, 200) if row["kind"] == "tile" else (10, 40)
		for band in range(0, 90, step):
			k = (angle >= band) & (angle < band + step)
			if k.sum() < least:
				continue
			studio = row["studio"][k].mean()
			if studio < 0.003:
				continue
			out.append((row["experiment"], row["group"], row["label"], band, studio, max(prediction[k].mean(), 1e-5)))
	return out


def summary(cs):
	parts = []
	for experiment in sorted(set(c[0] for c in cs)):
		r = np.array([math.log(s / p) for e, *_, s, p in cs if e == experiment])
		parts.append(f"{experiment} {np.sqrt((r ** 2).mean()):.4f} ({len(r)})")
	r = np.array([math.log(s / p) for *_, s, p in cs])
	return f"log ratio rms {np.sqrt((r ** 2).mean()):.4f} over {len(r)} bins: " + ", ".join(parts)


def parameters(pairs, base=ROR):
	p = dict(base)
	for pair in pairs:
		name, value = pair.split("=")
		p[name] = float(value)
	return p


def tiles(cs):
	by = {}
	for experiment, group, label, _, studio, prediction in cs:
		if experiment == "sun-grazing":
			by.setdefault(label, {}).setdefault(group, []).append(studio / prediction)
	views = [f"e{e}-a{a}" for a in (180, 90, 0) for e in (5, 10, 20, 35)]
	print("Studio / law per tile and view (e: degrees up, a: degrees around from the sun)")
	print(f"{'':16s} " + " ".join(f"{v:>9s}" for v in views))
	for label in sorted(by):
		print(f"{label:16s} " + " ".join(f"{np.mean(by[label][v]):9.3f}" if v in by[label] else f"{'-':>9s}" for v in views))


def main():
	command = sys.argv[1] if len(sys.argv) > 1 else "show"
	data = rows()
	if command == "show":
		p = parameters(sys.argv[2:])
		cs = cells(data, lambda row: law(row, p))
		print(summary(cs))
		tiles(cs)
	elif command == "before":
		cs = cells(data, law_before)
		print(summary(cs))
		tiles(cs)
	elif command == "metal-round":
		cs = cells(data, lambda row: law(row, METAL_ROUND))
		print(summary(cs))
		tiles(cs)
	elif command == "fit":
		names = sys.argv[2].split(",")
		start = [float(x) for x in sys.argv[3].split(",")]
		fixed = parameters(sys.argv[4:])

		def at(x):
			p = dict(fixed)
			p.update(zip(names, x))
			return p

		def residuals(x):
			return np.array([math.log(s / p) for *_, s, p in cells(data, lambda row: law(row, at(x)))])

		fit = least_squares(residuals, start, diff_step=1e-3)
		print("fit " + " ".join(f"{name} {value:.4f}" for name, value in zip(names, fit.x)))
		print(summary(cells(data, lambda row: law(row, at(fit.x)))))
	else:
		print(__doc__)


if __name__ == "__main__":
	main()
