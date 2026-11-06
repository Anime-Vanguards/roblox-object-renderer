"""Which emitters make a long exposure's error (notes/render.md "Long exposures"): ror's exposure drawn
by groups of its effects (`cargo run -p ror --release --example tools -- exposure_groups <exposure dir>`:
the emitters sharing a name and texture, the trails, the beams, each alone) against Studio's
exposure of the whole effect, over the whole exposure, in linear light at the groups' scale:

- `share`: the group's part of the light ror's effect adds (each group's average less the effect-free
  one, the positive part, over the sum of the groups');
- `gain`: Studio's added light fitted as the groups' added light, each times a gain (non-negative
  least squares over the footprint): 1 where Studio draws the group as ror does, below where Studio's
  is dimmer or smaller, above where brighter, 0 where Studio shows none of it;
- `error`: the group's part of ror's absolute error (each pixel's |ror - Studio| of the added light
  split between the groups by their light there);
- `alone`: ror's whole effect against the sum of its groups (1: the groups add up; below, they cover
  each other).

    python tools/analysis/exposure_attribution.py <experiment>
"""

import json
import os
import sys

import cv2
import numpy as np
from scipy.optimize import nnls

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from exposure_metrics import THRESHOLD, Exposure, blend, display, load  # noqa: E402


def shrink(image, size):
	"""A full-size average (linear light) at the groups' size, by area."""
	return cv2.resize(image, size, interpolation=cv2.INTER_AREA)


def attribute(studio, studio_bare, groups, ror_all, ror_bare, labels):
	"""Gains, shares and error shares of the groups (linear images at one size)."""
	added_s = studio - studio_bare
	added_r = ror_all - ror_bare
	parts = [group - ror_bare for group in groups]
	footprint = (np.abs(display(studio) - display(studio_bare)).max(axis=2) > THRESHOLD) | (np.abs(display(ror_all) - display(ror_bare)).max(axis=2) > THRESHOLD)
	if not footprint.any():
		return None
	light = np.array([np.clip(part, 0, None)[footprint].sum() for part in parts])
	share = light / max(light.sum(), 1e-9)
	# the fit over the footprint's pixels (every channel), at most 300k rows
	rows = np.flatnonzero(np.repeat(footprint.ravel(), 3))
	if rows.size > 300_000:
		rows = np.random.default_rng(1).choice(rows, 300_000, replace=False)
	matrix = np.stack([part.ravel()[rows] for part in parts], axis=1).astype(np.float64)
	target = added_s.ravel()[rows].astype(np.float64)
	active = light > 1e-6 * max(light.max(), 1e-9)
	gains = np.full(len(parts), np.nan)
	if active.any():
		gains[active], _ = nnls(matrix[:, active], target)
	residual = target - matrix[:, active] @ gains[active] if active.any() else target
	explained = 1.0 - float((residual**2).sum() / max((target**2).sum(), 1e-12))
	# ror's error split by the groups' light at each pixel
	error = np.abs(added_r - added_s).sum(axis=2) * footprint
	weights = np.stack([np.abs(part).sum(axis=2) for part in parts])
	weights = weights / np.maximum(weights.sum(axis=0), 1e-9)
	error_share = (weights * error).sum(axis=(1, 2)) / max(error.sum(), 1e-9)
	alone = float(np.clip(added_r, 0, None)[footprint].sum() / max(np.clip(sum(parts), 0, None)[footprint].sum(), 1e-9))
	light_ratio = float(np.clip(added_r, 0, None)[footprint].sum() / max(np.clip(added_s, 0, None)[footprint].sum(), 1e-9))
	return {
		"light": light_ratio,
		"alone": alone,
		"explained": explained,
		"groups": [{"label": label, "share": float(s), "gain": None if np.isnan(g) else float(g), "error": float(e)} for label, s, g, e in zip(labels, share, gains, error_share)],
	}


def main():
	args = sys.argv[1:]
	if not args:
		print(__doc__)
		return
	experiment = args[0]
	exposure = Exposure(experiment)
	directory = os.path.join(exposure.directory, "groups")
	with open(os.path.join(directory, "groups.json")) as file:
		listing = json.load(file)
	size = (listing["width"], listing["height"])
	entries = [entry for entry in listing["groups"] if entry["file"] not in ("all", "bare")]
	labels = [entry["label"] for entry in entries]
	# Studio's whole exposure (each window weighed by its frames), against ror's groups over the frames
	# of every `step`-th play
	whole = None
	for window, images, counts in exposure.slices():
		if window is None:
			whole, counts_whole = images, counts
	studio = blend([(whole[("studio", group)], counts_whole[group]) for group in range(exposure.groups)])
	studio_bare = whole.get(("studio", None)) or whole[("ror", None)]
	parts = {entry["file"]: load(os.path.join(directory, f"{entry['file']}-w0.png")) for entry in listing["groups"]}
	groups = [parts[entry["file"]].lin if parts.get(entry["file"]) is not None else parts["bare"].lin for entry in entries]
	result = attribute(shrink(studio.lin, size), shrink(studio_bare.lin, size), groups, parts["all"].lin, parts["bare"].lin, labels)
	if result is None:
		print(f"{experiment}: no footprint")
		return
	print(f"{experiment}: ror's light {result['light']:.3f} of Studio's, its groups add up to {result['alone']:.2f} of its whole, the fit explains {result['explained'] * 100:.0f} % of Studio's added light")
	for group in sorted(result["groups"], key=lambda group: -group["error"]):
		if group["share"] < 0.01 and group["error"] < 0.02:
			continue
		gain = "  -  " if group["gain"] is None else f"{group['gain']:.2f}"
		print(f"  {group['label'][:72]:72s} share {group['share']:.3f}  gain {gain}  error {group['error']:.3f}")
	with open(os.path.join(directory, "attribution.json"), "w") as file:
		json.dump(result, file, indent=1)


if __name__ == "__main__":
	main()
