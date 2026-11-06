"""Which of an effect's emitters ror draws too bright, too dim, or that Studio does not show: each
timeline run's effect groups are drawn alone by `examples/tools/vfx_groups.rs` (the emitters sharing a
name and texture, the trails, the beams), and Studio's frame is fitted as the bare background plus a
gain times each group's light (blurred over boxes wider than particles are apart, so random positions
average out), with a weak prior pulling every gain to 1. ror's own image is fitted the same way
(overlaps and clipping keep its gains under 1); a group's ratio is Studio's gain over ror's. Then
the ratios of every group that adds a fair share of a run's light, pooled by property value, rank
what makes ror's image differ.

    python tools/analysis/vfx_attribution.py [experiment filter] [--groups] [--min-share 0.04] [--json out.json]

`--groups` (re)renders the groups first (`target/release/examples/tools.exe vfx_groups`, at half size into
out/groups/<experiment>/<run>; `VFX_GROUPS` / `VFX_GROUPS_OUT` another build's tool and directory).
"""

import json
import math
import os
import subprocess
import sys

import numpy as np
from PIL import Image
from scipy.optimize import lsq_linear

ROOT = os.path.join("out", "calibrate")
GROUPS = os.path.join("out", "groups")
# `VFX_GROUPS`: another build's tools binary (a baseline's; an older build's vfx_groups.exe works too)
TOOL = os.environ.get("VFX_GROUPS") or os.path.join("target", "release", "examples", "tools.exe")
TOOL = [TOOL] if os.path.basename(TOOL).startswith("vfx_groups") else [TOOL, "vfx_groups"]
GROUPS = os.environ.get("VFX_GROUPS_OUT") or GROUPS


def load(path, size=None):
	image = Image.open(path).convert("RGB")
	if size and image.size != size:
		image = image.resize(size, Image.BOX)
	return np.asarray(image, dtype=np.float64)


def pool(image, factor):
	# area means over factor x factor blocks (the image cut to whole blocks)
	h, w = image.shape[0] // factor * factor, image.shape[1] // factor * factor
	return image[:h, :w].reshape(h // factor, factor, w // factor, factor, -1).mean(axis=(1, 3))


def fit(target, columns, prior=0.15):
	# target ≈ columns · gains, gains in [0, 8], pulled weakly to 1: a gain off by d costs each column
	# d times its norm in the fit and `prior` times that in the prior
	matrix = np.stack(columns, axis=1)
	scale = np.sqrt((matrix**2).sum(axis=0)) + 1e-9
	augmented = np.vstack([matrix / scale, np.diag(np.full(len(columns), prior))])
	rhs = np.concatenate([target, prior * scale])
	result = lsq_linear(augmented, rhs, bounds=(0, 8 * scale))
	return result.x / scale


def attribute(run_dir, group_dir, factor=6):
	listing = json.load(open(os.path.join(group_dir, "groups.json"), encoding="utf-8"))
	bare = load(os.path.join(group_dir, "bare.png"))
	size = (bare.shape[1], bare.shape[0])
	studio = load(os.path.join(run_dir, "studio.png"), size)
	ror = load(os.path.join(group_dir, "all.png"))
	entries, columns = [], []
	for entry in listing:
		path = os.path.join(group_dir, entry["file"] + ".png")
		if not os.path.exists(path):
			continue
		excess = pool(load(path) - bare, factor)
		if np.abs(excess).sum() < 1e-6:
			continue
		entries.append(entry)
		columns.append(excess.ravel())
	if not columns:
		return []
	target_s = pool(studio - bare, factor).ravel()
	target_r = pool(ror - bare, factor).ravel()
	gains_s = fit(target_s, columns)
	gains_r = fit(target_r, columns)
	# the control: ror's own effect from other random streams, fitted the same way (what random
	# positions alone do to the gains)
	alt_path = os.path.join(group_dir, "alt.png")
	gains_c = fit(pool(load(alt_path) - bare, factor).ravel(), columns) if os.path.exists(alt_path) else gains_r
	energy = np.array([np.abs(column).sum() for column in columns])
	share = energy / energy.sum()
	residual = target_s - np.stack(columns, axis=1) @ gains_s
	unexplained = float(np.abs(residual).sum() / max(np.abs(target_s).sum(), 1e-9))
	# where a group draws most of ror's light (its region): Studio's excess there over ror's, of the
	# group's own sign (light, or darkening for smoke and negative LightEmission)
	pixels = len(target_s) // 3
	magnitude = np.stack([np.abs(column).reshape(pixels, 3).sum(axis=1) for column in columns])
	total = magnitude.sum(axis=0) + 1e-9
	studio_px = target_s.reshape(pixels, 3).sum(axis=1)
	ror_px = target_r.reshape(pixels, 3).sum(axis=1)
	alt_px = pool(load(alt_path) - bare, factor).ravel().reshape(pixels, 3).sum(axis=1) if os.path.exists(alt_path) else ror_px
	rows = []
	for index, (entry, a, b, c, s) in enumerate(zip(entries, gains_s, gains_r, gains_c, share)):
		region = (magnitude[index] > 0.6 * total) & (magnitude[index] > 6)
		sign = 1.0 if columns[index].sum() >= 0 else -1.0
		ror_light = (np.clip(sign * ror_px, 0, None) * region).sum()
		studio_light = (np.clip(sign * studio_px, 0, None) * region).sum()
		alt_light = (np.clip(sign * alt_px, 0, None) * region).sum()
		region_ratio = float(studio_light / ror_light) if ror_light > 30 else None
		region_control = float(alt_light / ror_light) if ror_light > 30 else None
		rows.append({"entry": entry, "studio": float(a), "ror": float(b), "control": float(c / max(b, 0.05)), "ratio": float(a / max(b, 0.05)), "share": float(s), "unexplained": unexplained, "region_ratio": region_ratio, "region_control": region_control, "region_pixels": int(region.sum()), "dark": sign < 0})
	return rows


def buckets(entry):
	# the property values a group is pooled by
	props = entry.get("props", {})
	kind = entry["kind"]
	if kind != "emitters":
		return [kind]
	labels = []
	emission = props["lightEmission"]
	labels.append("LightEmission " + ("<0" if emission < 0 else "0" if emission == 0 else "1" if emission >= 1 else "0-1"))
	brightness = props["brightness"]
	labels.append("Brightness " + ("<=1" if brightness <= 1 else "1-5" if brightness <= 5 else "5-20" if brightness <= 20 else ">20"))
	labels.append("orientation " + ["FacingCamera", "WorldUp", "VelocityParallel", "VelocityPerpendicular"][min(props["orientation"], 3)])
	layout = props["flipbookLayout"]
	labels.append("flipbook " + ["none", "2x2", "4x4", "8x8", "custom"][min(layout, 4)] + ("" if layout == 0 else " " + ["Loop", "OneShot", "PingPong", "Random"][min(props["flipbookMode"], 3)]))
	if layout and props["flipbookStartRandom"]:
		labels.append("flipbook start random")
	labels.append("ZOffset " + ("0" if props["zOffset"] == 0 else ">0" if props["zOffset"] > 0 else "<0"))
	labels.append("Drag " + ("0" if props["drag"] == 0 else ">0"))
	labels.append("Acceleration " + ("0" if props["acceleration"] == 0 else ">0"))
	labels.append("Squash " + ("0" if props["squash"] == 0 else ">0"))
	labels.append("LockedToPart " + str(props["lockedToPart"]))
	labels.append("parent " + ("Attachment" if props["attachment"] else "part shape " + ["Box", "Sphere", "Cylinder", "Disc"][min(props["shape"], 3)] + (" Surface" if props["shapeStyle"] == 1 else "") + (" InOut" if props["shapeInOut"] else "")))
	labels.append("LightInfluence " + ("0" if props["lightInfluence"] <= 0.01 else ">0"))
	labels.append("TimeScale " + ("1" if props["timeScale"] >= 0.999 else "<1"))
	lifetime = props["lifetime"][1]
	labels.append("Lifetime max " + ("<0.25" if lifetime < 0.25 else "<0.5" if lifetime < 0.5 else "<1" if lifetime < 1 else ">=1"))
	labels.append("Rate emitter " + str(props["enabled"] and props["rate"] > 0))
	labels.append("size " + ("<1" if props["maxSize"] < 1 else "<4" if props["maxSize"] < 4 else "<10" if props["maxSize"] < 10 else ">=10"))
	speed = props["speed"][1]
	labels.append("speed " + ("0" if speed < 0.01 else "<10" if speed < 10 else "<50" if speed < 50 else ">=50"))
	transparency = props["minTransparency"]
	labels.append("min Transparency " + ("0" if transparency <= 0.01 else "<0.5" if transparency < 0.5 else ">=0.5"))
	return labels


def main():
	args = sys.argv[1:]
	needle = args[0] if args and not args[0].startswith("--") else "vfx-attack-auto"
	min_share = float(args[args.index("--min-share") + 1]) if "--min-share" in args else 0.04
	records = []
	for experiment in sorted(os.listdir(ROOT)):
		if needle not in experiment or not os.path.isdir(os.path.join(ROOT, experiment)):
			continue
		for run in sorted(os.listdir(os.path.join(ROOT, experiment))):
			run_dir = os.path.join(ROOT, experiment, run)
			if not os.path.exists(os.path.join(run_dir, "studio.json")):
				continue
			group_dir = os.path.join(GROUPS, experiment, run)
			if "--groups" in args or not os.path.exists(os.path.join(group_dir, "groups.json")):
				subprocess.run(TOOL + [run_dir, group_dir, "0.5"], capture_output=True, text=True, check=False)
			if not os.path.exists(os.path.join(group_dir, "groups.json")):
				print(f"{experiment}/{run}: no groups")
				continue
			for row in attribute(run_dir, group_dir):
				row["experiment"], row["run"] = experiment, run
				records.append(row)
	pooled = {}
	for row in records:
		if row["share"] < min_share:
			continue
		values = {"fit": math.log(max(row["ratio"], 0.02)), "control": math.log(max(row["control"], 0.02))}
		if row["region_ratio"] is not None:
			values["region"] = math.log(max(row["region_ratio"], 0.02))
			values["region control"] = math.log(max(row["region_control"], 0.02))
		for label in buckets(row["entry"]):
			pooled.setdefault(label, []).append((values, row["share"]))
	print(f"{len(records)} group-runs, {sum(1 for row in records if row['share'] >= min_share)} with a share of {min_share} or more")
	print("fit: Studio's gain over ror's; region: Studio's light over ror's where the group draws most of ror's; each beside its")
	print("control (ror's own effect from other random streams in Studio's place): geometric means weighted by share\n")
	columns = ("fit", "control", "region", "region control")
	print(f"{'property value':42s} {'n':>4s} " + " ".join(f"{name:>14s}" for name in columns) + f" {'fit/control':>12s} {'region/control':>15s}")
	table = []
	for label in sorted(pooled):
		means = {}
		for name in columns:
			items = [(values[name], share) for values, share in pooled[label] if name in values]
			if items:
				logs = np.array([value for value, _ in items])
				weights = np.array([share for _, share in items])
				means[name] = float(np.exp((logs * weights).sum() / weights.sum()))
		table.append((label, len(pooled[label]), means))
		corrected = means["fit"] / means["control"]
		region = means["region"] / means["region control"] if "region" in means else float("nan")
		print(f"{label:42s} {len(pooled[label]):4d} " + " ".join(f"{means.get(name, float('nan')):14.3f}" for name in columns) + f" {corrected:12.3f} {region:15.3f}")
	print("\nlargest groups by share, their ratios:")
	for row in sorted(records, key=lambda row: -row["share"])[:40]:
		entry = row["entry"]
		region = f"{row['region_ratio']:.2f}/{row['region_control']:.2f}" if row["region_ratio"] is not None else "-"
		print(f"  {row['experiment'][16:52]:36s} {row['run']:6s} {entry['name'][:24]:24s} share {row['share']:.2f} studio {row['studio']:.2f} ror {row['ror']:.2f} ratio {row['ratio']:.2f} control {row['control']:.2f} region {region}")
	if "--json" in args:
		with open(args[args.index("--json") + 1], "w") as file:
			json.dump([{key: value for key, value in row.items()} for row in records], file, indent=1)


if __name__ == "__main__":
	main()
