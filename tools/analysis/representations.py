"""Which representation of an asset Studio fetched to draw it: the uploaded image, or Roblox's KTX2
texture transcode, and which of the transcode's progressive packs (notes/image.md "Texture
transcodes"). Looks the asset up in Roblox's HTTP cache (%LOCALAPPDATA%/Roblox/rbx-storage files and
the small entries inline in rbx-storage.db) by content: the upload by the MD5 of ror's cached copy,
the transcode by the contentHash of ror's cached pack 0 (render the asset with ror first, so both are
in ror's cache). Reads content only.

	python tools/analysis/representations.py image:<asset id> ... pack:<TexturePack id> ...

`image:` an image asset (its `Image` transcode), `pack:` a TexturePack (its colour, normal and
metalness / roughness layers). Times are the cache entries' last use (UTC).
"""

import base64
import collections
import datetime
import hashlib
import os
import shutil
import sqlite3
import struct
import sys
import tempfile

STATE = os.environ.get("ROR_STATE_DIR") or ".cache"
ROBLOX = os.path.join(os.environ["LOCALAPPDATA"], "Roblox")
KTX2_ID = b"\xabKTX 20\xbb\r\n\x1a\n"


def cached(key):
	"""Bytes ror-assets cached under a key (FNV-1a 128 of the canonical URI), or None."""
	h = 0x6C62272E07BB014262B821756295C58D
	for b in key.encode():
		h = ((h ^ b) * 0x0000000001000000000000000000013B) & ((1 << 128) - 1)
	name = f"{h:032x}"
	path = os.path.join(STATE, "assets", name[:2], name + ".bin")
	return open(path, "rb").read() if os.path.exists(path) else None


def fidelity(layer, pack=0):
	"""ror_assets::Transcode::fidelity: images FF FF FF FC (pack + 1); TexturePack layers a 3-bit layer,
	4 option bits, a 3-bit pack + 1."""
	if layer is None:
		return base64.b64encode(bytes([0xFF, 0xFF, 0xFF, 0xFC, pack + 1])).decode()
	p = pack + 1
	return base64.b64encode(bytes([(layer << 5) | (p >> 2), (p & 3) << 6])).decode()


def rbxh_content(data):
	if data[:4] != b"RBXH":
		return None
	pos = 12 + struct.unpack_from("<I", data, 8)[0] + 1
	_status, header_len, _hash, content_len = struct.unpack_from("<IIII", data, pos)
	pos += 24 + header_len
	return data[pos : pos + content_len]


def ktx2_keys(data):
	kvd_off, kvd_len = struct.unpack_from("<2I", data, 56)
	keys, pos, end = {}, kvd_off, kvd_off + kvd_len
	while pos + 4 <= end:
		(length,) = struct.unpack_from("<I", data, pos)
		key, _, value = data[pos + 4 : pos + 4 + length].partition(b"\0")
		keys[key.decode("utf-8", "replace")] = value.rstrip(b"\0").decode("utf-8", "replace")
		pos += 4 + length + (-(4 + length)) % 4
	return keys


def roblox_cache():
	"""(contentHash -> [(packIndex, last use)], upload MD5 -> last use) over Roblox's HTTP cache."""
	transcodes, uploads = collections.defaultdict(list), {}
	storage = os.path.join(ROBLOX, "rbx-storage")
	with tempfile.TemporaryDirectory() as temp:
		db = os.path.join(temp, "rbx-storage.db")
		shutil.copyfile(os.path.join(ROBLOX, "rbx-storage.db"), db)
		con = sqlite3.connect(db)
		rows = con.execute("select id, content, atime from files where category = 10").fetchall()
		con.close()
	for id_, content, atime in rows:
		if content is None:
			path = os.path.join(storage, id_.hex()[:2], id_.hex())
			if not os.path.exists(path):
				continue
			content = open(path, "rb").read()
		body = rbxh_content(content)
		if body is None:
			continue
		if body[:12] == KTX2_ID:
			keys = ktx2_keys(body)
			transcodes[keys.get("contentHash")].append((keys.get("packIndex", "-"), atime))
		else:
			uploads[hashlib.md5(body).hexdigest()] = atime
	return transcodes, uploads


def when(ms):
	return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime("%m-%d %H:%M") if ms else "?"


def main():
	transcodes, uploads = roblox_cache()
	for arg in sys.argv[1:]:
		kind, _, asset = arg.partition(":")
		uri = f"rbxassetid://{asset}"
		layers = [("image", None)] if kind == "image" else [("color", 0), ("normal", 1), ("metalness/roughness", 2)]
		source = cached(uri)
		if kind == "image" and source is not None:
			md5 = hashlib.md5(source).hexdigest()
			print(f"{asset} upload: {'in Roblox cache, used ' + when(uploads[md5]) if md5 in uploads else 'not in Roblox cache'}")
		for name, layer in layers:
			pack0 = cached(f"{uri}#ktx2/7rdo/{fidelity(layer)}")
			if pack0 is None or pack0[:12] != KTX2_ID:
				print(f"{asset} {name} transcode: not in ror's cache (render it with ror first)")
				continue
			keys = ktx2_keys(pack0)
			total = sum(1 for key in keys if key.startswith("pack_")) or 1
			held = transcodes.get(keys.get("contentHash"), [])
			packs = sorted(set(index for index, _ in held))
			last = max((atime for _, atime in held), default=0)
			print(f"{asset} {name} transcode ({keys.get('transcodedWidth')}x{keys.get('transcodedHeight')}, {total} packs): Roblox cache holds packs {packs or 'none'}{', used ' + when(last) if held else ''}")


main()
