"""How a material texture maps onto a box face (material-mapping captures, white Brick): each
viewed face is resampled into its own plane at 32 px per stud, matched against the Brick colour
map (256 px per 8 studs) in eight orientations, and reported as the part-space directions of the
texture's u (right) and v (down) and the texture coordinate at the face's centre.

    python tools/analysis/face_mapping.py [run prefix ...]
"""

import json
import math
import os
import sys

import cv2
import numpy as np
from PIL import Image

from tone import GRID as _xs, TONED as _ys

ROOT = "out/calibrate/material-mapping"
BRICK = ".cache/material-packs/85077012310177_color_full.png"
PX = 32.0


def rot_y(degrees):
	a = math.radians(degrees)
	return np.array([[math.cos(a), 0, math.sin(a)], [0, 1, 0], [-math.sin(a), 0, math.cos(a)]])


# part name → (centre, rotation (columns = part axes in world), size)
PARTS = {
	"cubeA": (np.array([0.0, 0, 0]), np.eye(3), np.array([16.0, 16, 16])),
	"cubeB": (np.array([203.3, 5.1, 1.7]), np.eye(3), np.array([16.0, 16, 16])),
	"cubeC": (np.array([400.0, 0, 0]), rot_y(30), np.array([16.0, 16, 16])),
	"boxD": (np.array([600.0, 0, 0]), np.eye(3), np.array([13.0, 9, 11])),
	"ball": (np.array([800.0, 0, 0]), np.eye(3), np.array([16.0, 16, 16])),
	"cylinder": (np.array([1000.0, 0, 0]), np.eye(3), np.array([16.0, 16, 16])),
	"wedge": (np.array([1200.0, 0, 0]), np.eye(3), np.array([16.0, 16, 16])),
	"meshhead": (np.array([1600.0, 0, 0]), np.eye(3), np.array([16.0, 16, 16])),
}
# per part/view: the face plane's centre (part space, studs) and the patch centre on it (studs
# along e1, e2), where the default (box face centre) does not suit
SPECIAL = {
	"wedge-slope": (np.array([0.0, 0, 0]), (0.0, 0.0)),
	"wedge-px": (np.array([8.0, 0, 0]), (-4.0, -4.0)),
	"wedge-py": (np.array([0.0, 0, 0]), (0.0, 0.0)),
	"wedge-nz": (np.array([0.0, 0, 0]), (0.0, 0.0)),
}
# view suffix → face normal in part space; e1 (columns, right) and e2 (rows, up) seen from outside
FACES = {
	"px": (np.array([1.0, 0, 0]), np.array([0.0, 0, -1]), np.array([0.0, 1, 0])),
	"nx": (np.array([-1.0, 0, 0]), np.array([0.0, 0, 1]), np.array([0.0, 1, 0])),
	"py": (np.array([0.0, 1, 0]), np.array([-1.0, 0, 0]), np.array([0.0, 0, 1])),
	"ny": (np.array([0.0, -1, 0]), np.array([1.0, 0, 0]), np.array([0.0, 0, 1])),
	"pz": (np.array([0.0, 0, 1]), np.array([1.0, 0, 0]), np.array([0.0, 1, 0])),
	"nz": (np.array([0.0, 0, -1]), np.array([-1.0, 0, 0]), np.array([0.0, 1, 0])),
	"slope": (np.array([0.0, 1, -1]) / math.sqrt(2), np.array([-1.0, 0, 0]), np.array([0.0, 1, 1]) / math.sqrt(2)),
}
ORIENTATIONS = ["r0", "r90", "r180", "r270", "fr0", "fr90", "fr180", "fr270"]


def oriented(image, code):
	base = image[:, ::-1] if code.startswith("f") else image
	return np.ascontiguousarray(np.rot90(base, int(code.lstrip("fr")) // 90))


def project(camera, fov, width, height, points):
	eye = np.array(camera[0:3])
	m = np.array(camera[3:12]).reshape(3, 3)
	right, up, back = m[:, 0], m[:, 1], m[:, 2]
	d = points - eye
	x, y, z = d @ right, d @ up, d @ back
	scale = (height / 2) / math.tan(math.radians(fov) / 2)
	return width / 2 + x / -z * scale, height / 2 - y / -z * scale


def face_image(run, camera, fov, capture, part, face):
	center, rotation, size = PARTS[part]
	normal, e1, e2 = FACES[face]
	half = size / 2
	plane, _ = SPECIAL.get(run, (normal * (np.abs(normal) @ half), None))
	extent1, extent2 = np.linalg.norm(e1 * size), np.linalg.norm(e2 * size)
	cols, rows = int(extent1 * PX), int(extent2 * PX)
	a = (np.arange(cols) + 0.5) / PX - extent1 / 2
	b = extent2 / 2 - (np.arange(rows) + 0.5) / PX
	aa, bb = np.meshgrid(a, b)
	local = plane + aa[..., None] * e1 + bb[..., None] * e2
	world = local.reshape(-1, 3) @ rotation.T + center
	h, w = capture.shape[:2]
	px, py = project(camera, fov, w, h, world)
	mapx = px.reshape(rows, cols).astype(np.float32) - 0.5
	mapy = py.reshape(rows, cols).astype(np.float32) - 0.5
	return cv2.remap(capture, mapx, mapy, cv2.INTER_LINEAR)


def main():
	prefixes = sys.argv[1:]
	samples = {run["label"]: run for run in json.load(open(os.path.join(ROOT, "samples.json")))}
	texel = np.asarray(Image.open(BRICK).convert("RGB"), dtype=np.float32) / 255.0
	texture = cv2.resize((texel ** 2).mean(axis=2), (256, 256), interpolation=cv2.INTER_AREA).astype(np.float32)
	for label in sorted(samples):
		part, _, face = label.partition("-")
		if part not in PARTS or face not in FACES or (prefixes and not any(label.startswith(p) for p in prefixes)):
			continue
		run = samples[label]
		display = np.asarray(Image.open(os.path.join(ROOT, label, "studio.png")).convert("RGB"), dtype=np.float32)
		gray = np.interp((display / 255.0) ** 2, _ys, _xs).mean(axis=2).astype(np.float32)
		image = face_image(label, run["camera"], run["fov"], gray, part, face)
		rows, cols = image.shape
		# a 4 x 4 stud patch around the face centre (or the SPECIAL patch centre)
		pa, pb = SPECIAL.get(label, (None, (0.0, 0.0)))[1]
		r0, c0 = int(rows // 2 - pb * PX) - 64, int(cols // 2 + pa * PX) - 64
		patch = image[r0 : r0 + 128, c0 : c0 + 128]
		best = None
		for code in ORIENTATIONS:
			# the face image in texture orientation: undo the orientation on the patch instead
			wrapped = np.tile(texture, (2, 2))
			candidate = oriented(patch, code)
			result = cv2.matchTemplate(wrapped, candidate, cv2.TM_CCOEFF_NORMED)
			_, value, _, (x, y) = cv2.minMaxLoc(result)
			if best is None or value > best[0]:
				best = (value, code, x, y)
		value, code, x, y = best
		# where the patch's corners land in the texture: map face-image axes to texture axes
		# (oriented() applied to the patch maps patch (col,row) → texture (x,y) directions)
		probe = np.zeros((128, 128), dtype=np.float32)
		probe[0, 0], probe[0, 127] = 1.0, 2.0
		moved = oriented(probe, code)
		(ty0, tx0), (ty1, tx1) = np.argwhere(moved == 1.0)[0], np.argwhere(moved == 2.0)[0]
		probe = np.zeros((128, 128), dtype=np.float32)
		probe[0, 0], probe[127, 0] = 1.0, 2.0
		moved2 = oriented(probe, code)
		(sy0, sx0), (sy1, sx1) = np.argwhere(moved2 == 1.0)[0], np.argwhere(moved2 == 2.0)[0]
		normal, e1, e2 = FACES[face]
		# texture direction of the face's +e1 (columns) and -e2 (rows) axes
		col_dir = np.array([tx1 - tx0, ty1 - ty0]) / 127.0
		row_dir = np.array([sx1 - sx0, sy1 - sy0]) / 127.0
		# u (texture x) and v (texture y) as part-space directions
		u_axis = col_dir[0] * e1 + row_dir[0] * -e2
		v_axis = col_dir[1] * e1 + row_dir[1] * -e2
		# texture coordinate of the face centre: the patch's top-left corner lands at (x, y) + the
		# corner's position inside the oriented patch
		corner = np.array([x + tx0, y + ty0], dtype=float)
		offset_cols, offset_rows = cols / 2 - c0, rows / 2 - r0
		centre = corner + col_dir * offset_cols + row_dir * offset_rows
		uv = (centre / 256.0) % 1.0
		print(f"{label:9s} ncc {value:.3f} {code:5s} u {np.round(u_axis, 2)} v {np.round(v_axis, 2)} centre uv ({uv[0]:.3f}, {uv[1]:.3f})")


main()
