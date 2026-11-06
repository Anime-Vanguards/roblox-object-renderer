"""Runs the occlusion-culling probes (tools/occlusion-probes/*.luau) in a capture Studio: each case
after prof.luau (MicroProfiler timings through Roblox's LibMP, `require("@rbx/LibMP")`) and occ.luau
(scene builders), its lines printed. The measurements behind notes/occlusion.md; the case names are
the notes'.

    ROR_CAPTURE_PLACE=ror-capture-drawcalls ror studio open --technology future
    python tools/occlusion_probes.py a1 b2      # Edit mode, through `ror studio luau`

A case builds in Workspace.OccProbe, with the loaded map (Workspace.RorCapture) moved to
ServerStorage meanwhile, and reports queryOcclusion (the culling jobs' occlusion work), CullJob, the
render thread's Perform and the counters per rendered frame. `ablate` and `ablate2` instead take
apart a map loaded by `ror cost <map> --cameras <one view> --check`, in that view: what share of
queryOcclusion is rasterizing, and which classes of parts it goes to.
Timings are CPU milliseconds of this machine: compare cases of one run, not runs of two machines.
"""

import argparse
import os
import pathlib
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
PROBES = HERE / "occlusion-probes"
ROR = HERE.parent / "target" / "release" / ("ror.exe" if os.name == "nt" else "ror")


def script(case):
	parts = [(PROBES / "prof.luau").read_text(encoding="utf-8")]
	if not case.startswith("ablate"):
		parts.append((PROBES / "occ.luau").read_text(encoding="utf-8"))
	parts.append((PROBES / f"{case}.luau").read_text(encoding="utf-8").replace("__CAMERA__", ""))
	return "\n".join(parts)


def main():
	parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	parser.add_argument("cases", nargs="+", help="case names in tools/occlusion-probes (a1, b2, ablate, ...)")
	args = parser.parse_args()
	for case in args.cases:
		print(f"== {case}")
		with tempfile.NamedTemporaryFile("w", suffix=".luau", delete=False, encoding="utf-8") as file:
			file.write(script(case))
		try:
			subprocess.run([str(ROR), "studio", "luau", file.name], check=False)
		finally:
			os.unlink(file.name)


if __name__ == "__main__":
	main()
