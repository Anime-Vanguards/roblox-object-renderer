"""Roblox's light grid from the lightgrid and lg-* calibration experiments (notes/render.md "Light grid").

    python tools/analysis/lightgrid_fit.py grid     # fit the curve's exponent K and the sample offset d (8-neighbour mean)
    python tools/analysis/lightgrid_fit.py free     # the same with a free 3 x 3 kernel (edges e, corners c, centre 1 - 4e - 4c)
    python tools/analysis/lightgrid_fit.py shapes   # lg-shapes: a factor on the volume share or on the horizontal coverage
    python tools/analysis/lightgrid_fit.py terms    # lg-terms: roofed / open per lighting term, Studio and ror

Readings: OutdoorAmbient on white floors (green channel through radiance()) divided by the open floor's
0.953; walls divided by the same wall under Ambient alone (which the grid leaves alone) to cancel the
ambient occlusion. Model: voxels of 4 studs on multiples of 4; per voxel the light entering from above
L = the kernel over the layer above of L x transmission (1 outside the grid), transmission 1 - the
part's horizontal coverage of the voxel inside the voxel's layer; v = 1 - (1 - L)^K at voxel centres,
read trilinearly at p + d n.
"""

import sys

import numpy as np
from scipy.ndimage import convolve
from scipy.optimize import least_squares, minimize_scalar

from light_fit import Run, radiance

V = 4.0
OPEN = 0.953  # the open floor's OutdoorAmbient over its expected 2 x 0.7² (lg-height, lg-terms)
AMBIENT_OPEN = 0.967  # the same under Ambient (or GlobalShadows off)


def floor_samples(experiment, label, boxes, floor_top, region, step=6, roof=None):
	"""Floor points (x0, x1, z0, z1 region) under occluder boxes [(min, max, weight)]; with roof =
	(y0, y1, x0, x1, z0, z1), pixels whose ray crosses that slab first are dropped."""
	run = Run(experiment, label)
	hits, rays = run.plane_hits(1, floor_top)
	ok = (rays[..., 1] < 0) & (hits[..., 0] > region[0]) & (hits[..., 0] < region[1]) & (hits[..., 2] > region[2]) & (hits[..., 2] < region[3])
	if roof is not None:
		y0, y1, x0, x1, z0, z1 = roof
		for y in (y0, y1):
			s = (y - run.eye[1]) / rays[..., 1]
			p = run.eye[None, None, :] + rays * s[..., None]
			ok &= ~((s > 0) & (s < (floor_top - run.eye[1]) / rays[..., 1]) & (p[..., 0] > x0) & (p[..., 0] < x1) & (p[..., 2] > z0) & (p[..., 2] < z1))
	ys, xs = np.nonzero(ok)
	pick = (ys % step == 0) & (xs % step == 0)
	ys, xs = ys[pick], xs[pick]
	points = hits[ys, xs]
	return dict(name=f"{experiment}/{label}", boxes=boxes, points=points, normals=np.tile([0.0, 1.0, 0.0], (len(points), 1)), v=radiance(run.studio[ys, xs, 1]) / OPEN)


def wall_samples(experiment, label, ambient_label, gx, wall_xs, wall_zs, roof):
	"""Walls facing -X at gx + x (2 wide around z), OutdoorAmbient over the Ambient control."""
	run = Run(experiment, label)
	control = Run(experiment, ambient_label)
	outdoor = radiance(run.studio[..., 1]) / OPEN
	ambient = radiance(control.studio[..., 1]) / AMBIENT_OPEN
	points, values = [], []
	for x, z in zip(wall_xs, wall_zs):
		hits, rays = run.plane_hits(0, gx + x)
		on = (np.abs(hits[..., 2] - z) < 0.7) & (rays[..., 0] > 0) & (hits[..., 1] > 1.2) & (hits[..., 1] < 6.8)
		ys, xs = np.nonzero(on)
		pick = (ys % 3 == 0) & (xs % 3 == 0)
		ys, xs = ys[pick], xs[pick]
		points.append(hits[ys, xs])
		values.append(outdoor[ys, xs] / np.maximum(ambient[ys, xs], 0.5))
	points = np.concatenate(points)
	return dict(name=f"{experiment}/{label}-walls", boxes=[roof], points=points, normals=np.tile([-1.0, 0.0, 0.0], (len(points), 1)), v=np.concatenate(values))


def box(lo, hi, weight=1.0):
	return (np.array(lo, float), np.array(hi, float), weight)


def grid_cases():
	cases = []
	# lg-size: roofs 4 thick at bottom b, side s, their corner at (100 i, 0)
	for index, (bottom, side) in enumerate([(8, 4), (8, 8), (8, 16), (8, 24), (4, 8), (4, 16), (16, 16), (16, 32)]):
		gx, c = index * 100.0, side / 2
		cases.append(floor_samples("lg-size", f"b{bottom}-s{side}", [box([gx, bottom, 0], [gx + side, bottom + 4, side])], 0.0, (gx + c - 12, gx + c + 12, c - 12, c + 12), step=8))
	# lg-edge: an 8-12 roof 48 wide from z = e to e + 40
	for index, edge in enumerate([0, 1, 2, 3]):
		x = index * 100.0
		cases.append(floor_samples("lg-edge", f"e{edge}-outdoor", [box([x - 24, 8, edge], [x + 24, 12, edge + 40])], 0.0, (x - 4, x + 4, edge - 10, edge + 9), step=4))
	# lightgrid: 40 x 20 roofs 1 thick at height h
	for index, height in enumerate([2, 4, 8, 16]):
		x = index * 60.0
		roof = box([x - 20, height, 0], [x + 20, height + 1, 20])
		cases.append(floor_samples("lightgrid", f"roof{height}-outdoor", [roof], 0.0, (x - 5, x + 5, -6, 20), step=4, roof=(height, height + 1, x - 20, x + 20, 0, 20)))
	# lg-height: 48-stud roofs at bottom h (1 thick; one 4) over floors at 0 (or f)
	heights = [(f"h{h}", 0.0, h, 1.0) for h in [2, 3, 4, 5, 6, 7, 8, 10, 12, 16]]
	heights += [("f1-h8", 1.0, 8, 1.0), ("f2-h8", 2.0, 8, 1.0), ("f3-h8", 3.0, 8, 1.0), ("h8-thick", 0.0, 8, 4.0)]
	for index, (label, top, bottom, thickness) in enumerate(heights):
		x = index * 100.0
		cases.append(floor_samples("lg-height", f"{label}-outdoor", [box([x - 24, bottom, -24], [x + 24, bottom + thickness, 24])], top, (x - 3, x + 3, -3, 3), step=10))
	# lg-steps: roofs open at z = 0 at 12, 16, 20 over floors at 0, and 12 over 4
	for index, (bottom, top) in enumerate([(12, 0), (16, 0), (20, 0), (12, 4)]):
		x = index * 100.0
		case = floor_samples("lg-steps", f"b{bottom}-f{top}", [box([x - 24, bottom, 0], [x + 24, bottom + 4, 40])], float(top), (x - 4, x + 4, -10, 10), step=4)
		cases.append(case)
	# walls under 8-12 roofs open on the -X side
	cases.append(wall_samples("lg-walls", "outdoor", "ambient", 0.0, [1, 2, 3, 4, 5, 6, 8], [-28.0 + i * 8 for i in range(7)], box([0, 8, -36], [64, 12, 36])))
	cases.append(wall_samples("lg-steps", "walls", "walls-ambient", 500.0, [10, 12, 14, 16], [-9.0 + i * 6 for i in range(4)], box([500, 8, -36], [564, 12, 36])))
	return cases


def overlap(lo, hi, edges):
	return np.clip(np.minimum(hi, edges + V) - np.maximum(lo, edges), 0, V)


def propagate(transmission, kernel, exponent):
	"""Visibility per voxel (x, y, z) from transmissions, light entering from the top layer down."""
	nx, ny, nz = transmission.shape
	light = np.ones(transmission.shape)
	for k in range(ny - 2, -1, -1):
		light[:, k, :] = convolve(light[:, k + 1, :] * transmission[:, k + 1, :], kernel, mode="constant", cval=1.0)
	return 1 - (1 - np.clip(light, 0, 1)) ** exponent


def trilinear(vis, origin, points):
	g = (points - origin - V / 2) / V
	i0 = np.clip(np.floor(g).astype(int), 0, np.array(vis.shape) - 2)
	f = np.clip(g - i0, 0, 1)
	out = np.zeros(len(points))
	for dx in (0, 1):
		for dy in (0, 1):
			for dz in (0, 1):
				w = (f[:, 0] if dx else 1 - f[:, 0]) * (f[:, 1] if dy else 1 - f[:, 1]) * (f[:, 2] if dz else 1 - f[:, 2])
				out += w * vis[i0[:, 0] + dx, i0[:, 1] + dy, i0[:, 2] + dz]
	return out


def kernel_for(params):
	e, c = params["e"], params["c"]
	return np.array([[c, e, c], [e, 1 - 4 * e - 4 * c, e], [c, e, c]])


def predict(case, params, rows=slice(None)):
	"""Box occluders (axis-aligned): horizontal coverage inside each voxel's layer."""
	points = case["points"][rows] + case["normals"][rows] * params["d"]
	x0 = np.floor((points[:, 0].min() - 48) / V) * V
	z0 = np.floor((points[:, 2].min() - 48) / V) * V
	nx = int((points[:, 0].max() + 48 - x0) / V) + 2
	nz = int((points[:, 2].max() + 48 - z0) / V) + 2
	y0, ny = -8.0, 16
	xe, ye, ze = x0 + V * np.arange(nx), y0 + V * np.arange(ny), z0 + V * np.arange(nz)
	occupancy = np.zeros((nx, ny, nz))
	for lo, hi, weight in case["boxes"]:
		share = (overlap(lo[0], hi[0], xe) / V)[:, None, None] * (overlap(lo[1], hi[1], ye) > 0)[None, :, None] * (overlap(lo[2], hi[2], ze) / V)[None, None, :]
		occupancy = 1 - (1 - occupancy) * (1 - weight * share)
	vis = propagate(1 - occupancy, kernel_for(params), params["K"])
	return trilinear(vis, np.array([x0, y0, z0]), points)


def fit_grid(names, start, lo, hi, fixed, subsample=9):
	cases = grid_cases()

	def residuals(x):
		params = dict(fixed, **dict(zip(names, x)))
		return np.concatenate([predict(c, params, slice(None, None, subsample)) - c["v"][::subsample] for c in cases])

	fit = least_squares(residuals, start, bounds=(lo, hi))
	params = dict(fixed, **dict(zip(names, fit.x)))
	print({k: round(float(v), 4) for k, v in params.items()}, "centre", round(1 - 4 * params["e"] - 4 * params["c"], 3), "rms", round(float(np.sqrt((fit.fun**2).mean())), 4))
	for c in cases:
		error = predict(c, params) - c["v"]
		print(f"   {c['name']:28s} {len(error):5d} points  rms {np.sqrt((error**2).mean()):.3f}  mean {error.mean():+.3f}")
	# the offset is piecewise (voxel layers): a scan shows its optimum where the gradient does not
	scan = []
	for d in [4.0, 5.0, 5.5, 6.0, 6.5, 7.0, 8.0]:
		error = np.concatenate([predict(c, dict(params, d=d), slice(None, None, subsample)) - c["v"][::subsample] for c in cases])
		scan.append(f"{d:g}: {np.sqrt((error**2).mean()):.4f}")
	print("rms against the offset d:", ", ".join(scan))
	scan = []
	for exponent in [2.0, 3.0, 3.5, 4.0, 4.5, 5.0, 6.0]:
		error = np.concatenate([predict(c, dict(params, K=exponent), slice(None, None, subsample)) - c["v"][::subsample] for c in cases])
		scan.append(f"{exponent:g}: {np.sqrt((error**2).mean()):.4f}")
	print("rms against the exponent K:", ", ".join(scan))


def rot_y(degrees):
	a = np.radians(degrees)
	return np.array([[np.cos(a), 0, np.sin(a)], [0, 1, 0], [-np.sin(a), 0, np.cos(a)]])


def inside(kind, size, rot, center, points):
	"""Points (M x 3) inside a part's shape (rot's columns are the part's axes)."""
	local = (points - center) @ rot
	h = size / 2
	box_inside = np.all(np.abs(local) <= h, axis=1)
	if kind == "block":
		return box_inside
	if kind == "ball":
		r = min(size) / 2
		return (local**2).sum(1) <= r * r
	if kind == "ellipsoid":
		return ((local / h) ** 2).sum(1) <= 1
	if kind == "cylinder":
		r = min(size[1], size[2]) / 2
		return (np.abs(local[:, 0]) <= h[0]) & (local[:, 1] ** 2 + local[:, 2] ** 2 <= r * r)
	if kind == "wedge":
		# full height at +Z, sloping down to the -Z edge
		return box_inside & (local[:, 1] / h[1] <= local[:, 2] / h[2])
	raise ValueError(kind)


def shape_share(kind, size, rot, center, origin, dims, mode, n=8):
	"""Per voxel: the shape's horizontal coverage inside the voxel's layer, or its volume share."""
	share = np.zeros(dims)
	sub = (np.arange(n) + 0.5) / n * V
	extent = np.abs(rot) @ (size / 2)
	lo = np.floor((center - extent - origin) / V).astype(int)
	hi = np.floor((center + extent - origin) / V).astype(int)
	for i in range(max(lo[0], 0), min(hi[0], dims[0] - 1) + 1):
		for j in range(max(lo[2], 0), min(hi[2], dims[2] - 1) + 1):
			gx, gz = np.meshgrid(origin[0] + i * V + sub, origin[2] + j * V + sub, indexing="ij")
			for k in range(max(lo[1], 0), min(hi[1], dims[1] - 1) + 1):
				ys = origin[1] + k * V + sub
				points = np.stack([np.repeat(gx.ravel(), len(ys)), np.tile(ys, gx.size), np.repeat(gz.ravel(), len(ys))], axis=1)
				hit = inside(kind, size, rot, center, points).reshape(gx.size, len(ys))
				share[i, k, j] = hit.any(axis=1).mean() if mode == "coverage" else hit.mean()
	return share


def fit_shapes():
	"""lg-shapes: the floor under each occluder against a factor s on its share (8-neighbour mean,
	K = 4, d = 6); the turned block by coverage (s = 1) as the check; a SpecialMesh sphere as the
	ellipsoid in its block."""
	kernel = np.full((3, 3), 0.125)
	kernel[1, 1] = 0
	cases = [
		(0, "block45", "block", np.array([24.0, 1.0, 24.0]), 8.5, 45.0),
		(1, "ball", "ball", np.array([20.0, 20.0, 20.0]), 18.0, 0.0),
		(2, "cylinder", "cylinder", np.array([24.0, 12.0, 12.0]), 14.0, 0.0),
		(3, "wedge", "wedge", np.array([24.0, 12.0, 24.0]), 14.0, 0.0),
		(5, "special-sphere", "ellipsoid", np.array([24.0, 4.0, 24.0]), 10.0, 0.0),
	]
	for index, label, kind, size, height, turn in cases:
		x = index * 100.0
		run = Run("lg-shapes", label)
		hits, rays = run.plane_hits(1, 0.0)
		ys, xs = np.nonzero(rays[..., 1] < 0)
		pick = (ys % 6 == 0) & (xs % 6 == 0)
		points = hits[ys[pick], xs[pick]] + np.array([0.0, 6.0, 0.0])
		measured = radiance(run.studio[ys[pick], xs[pick], 1]) / OPEN
		origin = np.array([np.floor((x - 60) / V) * V, -8.0, -60.0])
		for mode in ("coverage", "volume"):
			share = shape_share(kind, size, rot_y(turn), np.array([x, height, 0.0]), origin, (30, 12, 30), mode)

			def error(s):
				vis = propagate(1 - np.clip(s * share, 0, 1), kernel, 4.0)
				return ((trilinear(vis, origin, points) - measured) ** 2).mean()

			best = minimize_scalar(error, bounds=(0.0, 1.5), method="bounded")
			print(f"{label:9s} {mode:8s}: s {best.x:.3f}  rms {np.sqrt(best.fun):.3f}  (s = 1: rms {np.sqrt(error(1.0)):.3f})")
	for label, what in (("meshhead", "a head MeshPart"), ("special-head", "a block drawn as the head FileMesh")):
		run = Run("lg-shapes", label)
		mean = radiance(run.studio[..., 1]).mean() / OPEN
		print(f"{label:14s}: the floor under {what} 24 x 4 x 24 reads {mean:.3f} of open (not in the grid)")


def fit_terms():
	"""lg-terms: the floor's centre (the middle 20 % of the image) roofed over open, per term."""
	def centre(image):
		h, w = image.shape[:2]
		return image[int(h * 0.4) : int(h * 0.6), int(w * 0.4) : int(w * 0.6)]

	print(f"{'term':20s} {'Studio roof/open':>18s} {'ror roof/open':>15s}   Studio open, ror open (x, per channel)")
	for term in ["outdoor", "ambient", "sky", "specular", "outdoor-noshadows"]:
		roof, open_ = Run("lg-terms", f"roof-{term}"), Run("lg-terms", f"open-{term}")
		s_roof, s_open = radiance(centre(roof.studio)).mean(axis=(0, 1)), radiance(centre(open_.studio)).mean(axis=(0, 1))
		r_roof, r_open = radiance(centre(roof.ror)).mean(axis=(0, 1)), radiance(centre(open_.ror)).mean(axis=(0, 1))
		print(f"{term:20s} {s_roof[1] / s_open[1]:18.3f} {r_roof[1] / r_open[1]:15.3f}   {np.round(s_open, 3)} {np.round(r_open, 3)}")
	roof, open_ = Run("lg-terms", "roof-light"), Run("lg-terms", "open-light")
	print(f"{'PointLight':20s} {radiance(roof.studio[..., 1]).mean() / radiance(open_.studio[..., 1]).mean():18.3f} {radiance(roof.ror[..., 1]).mean() / radiance(open_.ror[..., 1]).mean():15.3f}")


if __name__ == "__main__":
	which = sys.argv[1] if len(sys.argv) > 1 else "grid"
	if which == "grid":
		fit_grid(["K", "d"], [4.0, 6.0], [1.0, 2.0], [12.0, 10.0], {"e": 0.125, "c": 0.125})
	elif which == "free":
		fit_grid(["e", "c", "K", "d"], [0.2, 0.08, 4.0, 6.0], [0.0, 0.0, 1.0, 2.0], [0.25, 0.25, 12.0, 10.0], {})
	elif which == "shapes":
		fit_shapes()
	elif which == "terms":
		fit_terms()
	else:
		raise SystemExit(__doc__)
