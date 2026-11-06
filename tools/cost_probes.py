"""Runs the draw-call probes (tools/cost-probes/*.luau) in a capture Studio: each case after lib.luau,
its lines printed. The measurements behind notes/cost.md; the case names are the notes'.

    ROR_CAPTURE_PLACE=ror-capture-drawcalls ror studio open --technology future
    python tools/cost_probes.py a1 b1           # Edit mode, through `ror studio luau`
    python tools/cost_probes.py l1 --lod        # with MeshPartDetailLevel DistanceBased for the run
    python tools/cost_probes.py spot --play     # in the play-mode client (start play first:
                                                # the Studio MCP's start_stop_play)

A case builds in Workspace.Probe and reports each step's change from the empty scene (P.case) or
the raw counters (P.step). `--lod` sets Studio's MeshPartDetailLevel to DistanceBased and puts the
user's setting back at the end: Studio keeps it for every place.
"""

import argparse
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
PROBES = HERE / "cost-probes"
ROR = HERE.parent / "target" / "release" / ("ror.exe" if os.name == "nt" else "ror")


def script(case, lod):
	parts = [(PROBES / "lib.luau").read_text(encoding="utf-8")]
	body = (PROBES / f"{case}.luau").read_text(encoding="utf-8")
	# the level-of-detail helpers (l4 and on) for the cases that use them
	if any(name in body for name in ("P.level", "P.CHAINS", "P.probe_mesh", "P.along", "P.empty_tris", "P.cull")):
		parts.append((PROBES / "lodlib.luau").read_text(encoding="utf-8"))
	if lod:
		parts.append((PROBES / "lodon.luau").read_text(encoding="utf-8"))
		parts.append("local __ok, __err = pcall(function()\n" + (PROBES / f"{case}.luau").read_text(encoding="utf-8") + "\nend)")
		parts.append((PROBES / "lodoff.luau").read_text(encoding="utf-8"))
		parts.append('if not __ok then table.insert(P.out, "ERROR " .. tostring(__err)) end')
	else:
		parts.append(body)
	parts.append("return table.concat(P.out, string.char(10))")
	return "\n".join(parts)


def bridge_address():
	"""The Studio bridge's address: ROR_STUDIO_MCP, host:port or a port on 127.0.0.1 (as ror reads it)."""
	value = os.environ.get("ROR_STUDIO_MCP", "").strip()
	if value:
		host, _, port = value.rpartition(":")
		return (host or "127.0.0.1", int(port))
	sys.exit("set ROR_STUDIO_MCP to the address of a Studio MCP bridge (host:port; docs/parity.md)")


def mcp_client_run(code):
	"""execute_luau in the capture Studio's play-mode client, through the Studio bridge."""
	place = os.environ.get("ROR_CAPTURE_PLACE", "ror-capture") + ".rbxl"
	connection = socket.create_connection(bridge_address())
	stream = connection.makefile("rw", encoding="utf-8")
	counter = [0]

	def request(method, params):
		counter[0] += 1
		stream.write(json.dumps({"jsonrpc": "2.0", "id": counter[0], "method": method, "params": params}) + "\n")
		stream.flush()
		while True:
			message = json.loads(stream.readline())
			if message.get("id") == counter[0]:
				return message

	request("initialize", {"protocolVersion": "2024-11-05", "clientInfo": {"name": "cost-probes", "version": "0"}, "capabilities": {}})
	stream.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
	stream.flush()
	listed = request("tools/call", {"name": "list_roblox_studios", "arguments": {}})
	studios = json.loads(listed["result"]["content"][0]["text"])["studios"]
	studio = next((entry["id"] for entry in studios if place in (entry.get("name") or "")), None)
	if studio is None:
		sys.exit(f"no Studio has {place} open")
	result = request("tools/call", {"name": "execute_luau", "arguments": {"studio_id": studio, "datamodel_type": "Client", "code": code}})["result"]
	return "".join(item.get("text", "") for item in result.get("content", []))


def main():
	parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	parser.add_argument("cases", nargs="+", help="case names in tools/cost-probes (a1, b1, ...)")
	parser.add_argument("--lod", action="store_true", help="MeshPartDetailLevel DistanceBased for the run")
	parser.add_argument("--play", action="store_true", help="run in the play-mode client")
	args = parser.parse_args()
	for case in args.cases:
		code = script(case, args.lod)
		print(f"== {case}")
		if args.play:
			print(mcp_client_run(code))
			continue
		with tempfile.NamedTemporaryFile("w", suffix=".luau", delete=False, encoding="utf-8") as file:
			file.write(code)
		try:
			subprocess.run([str(ROR), "studio", "luau", file.name], check=False)
		finally:
			os.unlink(file.name)


if __name__ == "__main__":
	main()
