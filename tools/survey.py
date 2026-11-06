"""Parity survey: `ror compare` on many instances of one place (or files), default view each, then a
table sorted by agreement. Studio captures are cached, so a re-run only renders.

    python tools/survey.py <place file> <dotted path> ... [--out out/survey]
    python tools/survey.py --files <model> ...
"""

import json
import os
import subprocess
import sys

ROR = os.path.join("target", "release", "ror.exe")


def main():
	args = sys.argv[1:]
	out_root = "out/survey"
	if "--out" in args:
		index = args.index("--out")
		out_root = args[index + 1]
		del args[index : index + 2]
	if args and args[0] == "--files":
		cases = [(path, None, os.path.splitext(os.path.basename(path))[0]) for path in args[1:]]
	else:
		place, paths = args[0], args[1:]
		cases = [(place, path, path.split(".")[-1].replace('"', "").replace("[", "").replace("]", "")) for path in paths]
	rows = []
	for source, select, name in cases:
		out = os.path.join(out_root, name)
		command = [ROR, "compare", source, "--out", out]
		if select:
			command += ["--select", select]
		result = subprocess.run(command, capture_output=True, text=True)
		if result.returncode != 0:
			rows.append((name, None, None, (result.stderr or result.stdout).strip().splitlines()[-1:]))
			print(f"{name}: FAILED {rows[-1][3]}")
			continue
		pixels = json.load(open(os.path.join(out, "report.json")))["views"][0]["pixels"]
		rows.append((name, pixels["exactPercent"], pixels["within8Percent"], None))
		print(f"{name:32s} within 2 {pixels['exactPercent']:6.2f}%  within 8 {pixels['within8Percent']:6.2f}%", flush=True)
	print("\nworst first:")
	for name, within2, within8, error in sorted((row for row in rows if row[1] is not None), key=lambda row: row[1]):
		print(f"{name:32s} within 2 {within2:6.2f}%  within 8 {within8:6.2f}%")


main()
