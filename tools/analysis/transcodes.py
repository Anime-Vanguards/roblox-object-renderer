"""Roblox's KTX2 transcodes of the built-in packs' maps against the PNG sources ror downloads
(notes/materials.md "Transcodes"): Studio draws the transcodes, which Roblox's HTTP cache
(%LOCALAPPDATA%/Roblox/rbx-storage, RBXH entries) holds after Studio has shown them. The cached
transcodes are found by thumbnail correlation, so a pack whose maps show the same image (the 4096²
table's and the 2022 table's) matches either: the large differences this once fitted were between
those packs. Against the 2022 table's own PNGs (PACKS) the fits are near 1 (colour means 0.99-1.00,
slopes 1.00-1.06), a check that Roblox has not re-transcoded the packs. The maps must be in the asset
cache: crates/ror/examples/tools/pack_layers.rs fetches a pack's maps and its layers.

	python tools/analysis/transcodes.py index                 # KTX2 entries and their key/values -> out/transcodes/index.json
	python tools/analysis/transcodes.py color [material ...]  # colour maps: per channel gain / offset (linear) on 8 x 8 texel means
	python tools/analysis/transcodes.py normal [material ...] # normal maps: slope gains (x, y)

Thumbnails and full decodes come from ror's `transcode_thumbs` and `decode_image` tools
(target/release/examples/tools, built by `cargo build --release --example tools`). Reads content only: the
asset cache under ROR_STATE_DIR (or .cache) and Roblox's HTTP cache.
"""

import io
import json
import os
import re
import struct
import subprocess
import sys

import numpy as np
from PIL import Image

STATE = os.environ.get("ROR_STATE_DIR") or ".cache"
OUT = os.path.join("out", "transcodes")
STORAGE = os.path.join(os.environ["LOCALAPPDATA"], "Roblox", "rbx-storage")
EXAMPLES = os.path.join("target", "release", "examples")
KTX2_ID = b"\xabKTX 20\xbb\r\n\x1a\n"
# thumbnail path -> width (thumbnails)
WIDTHS = {}
# render::materials::PACKS_2022
PACKS = dict(
	Plastic="9475422736", Wood="9920625499", WoodPlanks="9920626896", Marble="9873292869", Basalt="9920482224", Slate="9920600052",
	CrackedLava="9920485426", Concrete="9920484334", Limestone="9920561624", Granite="9920550720", Pavement="9920580094",
	Brick="9920482992", Pebble="9920581197", Cobblestone="9919719550", Rock="10129366149", Sandstone="9920596353",
	CorrodedMetal="9920589512", DiamondPlate="10237721036", Foil="9438410239", Metal="9920574966", Grass="9920552044",
	LeafyGrass="9920558145", Sand="9920591862", Fabric="9920517963", Snow="9920620451", Mud="9920578676",
	Ground="9920554695", Asphalt="9930003180", Salt="9920590478", Ice="9920556429", Glacier="9920518995", Glass="9873284556",
	Cardboard="14108654559", Carpet="14108663921", CeramicTiles="17429458668", ClayRoofTiles="18147685875", RoofShingles="91579661064923",
	Leather="14108671010", Plaster="14108672569", Rubber="14108675388",
)


def asset(uri_id):
	"""Bytes of a cached asset (ror-assets' disk cache: FNV-1a 128 of the canonical URI)."""
	h = 0x6C62272E07BB014262B821756295C58D
	for b in f"rbxassetid://{uri_id}".encode():
		h = ((h ^ b) * 0x0000000001000000000000000000013B) & ((1 << 128) - 1)
	key = f"{h:032x}"
	with open(os.path.join(STATE, "assets", key[:2], key + ".bin"), "rb") as f:
		return f.read()


def pack(pack_id):
	text = asset(pack_id).decode("utf-8", "replace")
	return {name: m.group(1) for name in ["color", "normal", "metalness", "roughness", "emissive"] if (m := re.search(rf"<{name}>\s*(\d+)\s*</{name}>", text))}


def png(asset_id):
	return np.asarray(Image.open(io.BytesIO(asset(asset_id))).convert("RGBA"), dtype=np.float64) / 255.0


def srgb(c):
	return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def rbxh_content(data):
	if data[:4] != b"RBXH":
		return data
	pos = 8 + 4 + struct.unpack_from("<I", data, 8)[0] + 1
	_status, header_len, _hash, content_len = struct.unpack_from("<IIII", data, pos)
	pos += 24 + header_len
	return data[pos : pos + content_len]


def ktx2_meta(data):
	if data[:12] != KTX2_ID:
		return None
	vk, _type_size, width, height, _depth, _layers, _faces, levels, _scheme = struct.unpack_from("<9I", data, 12)
	_dfd_off, _dfd_len, kvd_off, kvd_len = struct.unpack_from("<4I", data, 48)
	meta = {"vkFormat": vk, "width": width, "height": height, "levels": levels}
	pos, end = kvd_off, kvd_off + kvd_len
	while pos + 4 <= end:
		(length,) = struct.unpack_from("<I", data, pos)
		key, _, value = data[pos + 4 : pos + 4 + length].partition(b"\0")
		meta[key.decode("utf-8", "replace")] = value.rstrip(b"\0").decode("utf-8", "replace")
		pos += 4 + length + (-(4 + length)) % 4
	return meta


def index():
	entries = []
	for sub in sorted(os.listdir(STORAGE)):
		folder = os.path.join(STORAGE, sub)
		if len(sub) != 2 or not os.path.isdir(folder):
			continue
		for name in os.listdir(folder):
			path = os.path.join(folder, name)
			try:
				meta = ktx2_meta(rbxh_content(open(path, "rb").read()))
			except OSError:
				continue
			if meta:
				meta["path"] = path
				entries.append(meta)
	os.makedirs(OUT, exist_ok=True)
	json.dump(entries, open(os.path.join(OUT, "index.json"), "w"), indent=0)
	print(f"{len(entries)} KTX2 entries")


def thumbnails(color_space, mode):
	"""64 x 64 thumbnails of every cached KTX2 of a colour space (cached per mode)."""
	path = os.path.join(OUT, f"thumbs-{mode}.f32")
	if not os.path.exists(path):
		entries = json.load(open(os.path.join(OUT, "index.json")))
		paths = "\n".join(e["path"] for e in entries if e.get("colorSpace") == color_space and e["width"] >= 256)
		subprocess.run([os.path.join(EXAMPLES, "tools"), "transcode_thumbs", path, mode], input=paths, text=True, check=True)
	planes = 4 if mode == "raw4" else 1
	thumbs = np.fromfile(path, dtype=np.float32).astype(np.float64).reshape(-1, planes, 64, 64)
	names = []
	for line in open(path + ".txt").read().splitlines():
		name, size = line.rsplit(" ", 1)
		WIDTHS[name] = int(size.split("x")[0])
		names.append(name)
	return thumbs, names


def best_match(thumbs, names, target, size):
	"""The transcode whose thumbnail correlates best with `target` (64 x 64); of those within 0.02 of
	the best, the largest up to `size` (the cache can hold a map at several sizes)."""
	f = np.fft.fft2(thumbs - thumbs.mean(axis=(1, 2), keepdims=True))
	std = thumbs.std(axis=(1, 2))
	t = target - target.mean()
	corr = np.real(np.fft.ifft2(f * np.conj(np.fft.fft2(t))[None])) / (std[:, None, None] * target.std() * 4096 + 1e-12)
	best = corr.reshape(len(corr), -1).max(axis=1)
	order = [i for i in np.argsort(-best)[:8] if best[i] > 0.8]
	if not order:
		return None, 0.0
	close = [i for i in order if best[i] >= best[order[0]] - 0.02]
	width = lambda i: WIDTHS.get(names[i], 0) if WIDTHS.get(names[i], 0) <= size else -1
	chosen = max(close, key=lambda i: (width(i), best[i]))
	return chosen, best[chosen]


def decode(path, name):
	data = rbxh_content(open(path, "rb").read())
	os.makedirs(os.path.join(OUT, "decoded"), exist_ok=True)
	ktx = os.path.join(OUT, "decoded", f"{name}.ktx2")
	open(ktx, "wb").write(data)
	subprocess.run([os.path.join(EXAMPLES, "tools"), "decode_image", os.path.join(OUT, "decoded"), ktx], capture_output=True, check=True)
	return np.asarray(Image.open(os.path.join(OUT, "decoded", f"{name}.png")).convert("RGBA"), dtype=np.float64) / 255.0


def cells(a, n=128):
	h, w = a.shape[:2]
	return a.reshape(n, h // n, n, w // n, *a.shape[2:]).mean(axis=(1, 3))


def thumb(a):
	h, w = a.shape
	return a.reshape(64, h // 64, 64, w // 64).mean(axis=(1, 3))


def fit(source, transcode):
	"""transcode = gain * source + offset on 8 x 8 texel means, and the correlation."""
	x, y = cells(source).ravel(), cells(transcode).ravel()
	gain, offset = np.polyfit(x, y, 1)
	return gain, offset, np.corrcoef(x, y)[0, 1]


def colour(materials):
	thumbs, names = thumbnails("Gamma2RGB", "square")
	for material in materials or PACKS:
		maps = pack(PACKS[material])
		if "color" not in maps:
			continue
		source = srgb(png(maps["color"])[..., :3])
		i, ncc = best_match(thumbs[:, 0], names, thumb(source.mean(axis=2)), source.shape[0])
		if i is None:
			print(f"{material:14s} no transcode cached")
			continue
		# Gamma2RGB stores the square root of linear values
		transcode = decode(names[i], f"{material}-color")[..., :3] ** 2
		if transcode.shape != source.shape:
			k = source.shape[0] // transcode.shape[0]
			source = source.reshape(transcode.shape[0], k, transcode.shape[1], k, 3).mean(axis=(1, 3))
		fits = [fit(source[..., c], transcode[..., c]) for c in range(3)]
		means = transcode.reshape(-1, 3).mean(axis=0) / source.reshape(-1, 3).mean(axis=0)
		print(f"{material:14s} {maps['color']:>16s} NCC {ncc:.2f}  " + "  ".join(f"{g:.3f} x {o:+.3f} ({r:.2f})" for g, o, r in fits) + f"  mean x {np.round(means, 3)}")


def normal(materials):
	thumbs, names = thumbnails("Linear", "raw4")
	alpha_x = thumbs[:, 3] * 2 - 1
	for material in materials or PACKS:
		maps = pack(PACKS[material])
		if "normal" not in maps:
			continue
		source = png(maps["normal"])
		sx, sy = source[..., 0] * 2 - 1, source[..., 1] * 2 - 1
		i, ncc = best_match(alpha_x, names, thumb(sx), source.shape[0])
		if i is None:
			print(f"{material:14s} no transcode cached")
			continue
		# DXT5nm: x in alpha, y in green
		t = decode(names[i], f"{material}-normal")
		if t.shape[:2] != source.shape[:2]:
			print(f"{material:14s} transcode {t.shape[1]} px, source {source.shape[1]}")
			continue
		gx, _, rx = fit(sx, t[..., 3] * 2 - 1)
		gy, _, ry = fit(sy, t[..., 1] * 2 - 1)
		print(f"{material:14s} {maps['normal']:>16s} NCC {ncc:.2f}  x {gx:+.3f} ({rx:+.2f})  y {gy:+.3f} ({ry:+.2f})")


if __name__ == "__main__":
	command, rest = sys.argv[1], sys.argv[2:]
	{"index": lambda: index(), "color": lambda: colour(rest), "normal": lambda: normal(rest)}[command]()
