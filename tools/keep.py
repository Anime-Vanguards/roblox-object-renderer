"""Keeps each calibration run's ror.png (and background.png) under a tag, for before -> after
comparisons: `<tag>.png` (`<tag>-background.png`) beside it in each run of the experiments chosen.

A kept render is another name of the same file (a hard link; a copy across volumes): it costs no
space until ror draws that run again, which replaces ror.png by a rename and leaves the tag on the
old render. `cp` / `shutil.copyfile` onto an existing name write into its file, which may be shared
with other names (`ror clean` links identical files): keep_file replaces the name instead.

    python tools/keep.py <tag> [experiment filter]...   # every experiment in out/calibrate without one
"""

import os
import shutil
import sys

CALIBRATE = os.path.join("out", "calibrate")


def keep_file(source, target):
	"""`target` becomes another name of `source`'s file (a copy where no link can be made), replacing
	what was at `target` by a rename: never written into."""
	temporary = f"{target}.{os.getpid()}.keep.tmp"
	if os.path.exists(temporary):
		os.remove(temporary)
	try:
		os.link(source, temporary)
	except OSError:
		shutil.copyfile(source, temporary)
	os.replace(temporary, target)
	# replacing a name of the same file can leave the temporary one
	if os.path.exists(temporary):
		os.remove(temporary)


def keep(tag, filters=()):
	"""Keeps the chosen experiments' renders as `tag`; returns how many files."""
	kept = 0
	for experiment in sorted(os.listdir(CALIBRATE)):
		root = os.path.join(CALIBRATE, experiment)
		if not os.path.isdir(root) or (filters and not any(text in experiment for text in filters)):
			continue
		for run in sorted(os.listdir(root)):
			for name, target in (("ror.png", f"{tag}.png"), ("background.png", f"{tag}-background.png")):
				source = os.path.join(root, run, name)
				if os.path.isfile(source):
					keep_file(source, os.path.join(root, run, target))
					kept += 1
	return kept


def main():
	if len(sys.argv) < 2:
		print(__doc__)
		return
	print(f"{keep(sys.argv[1], sys.argv[2:])} files kept as {sys.argv[1]}")


if __name__ == "__main__":
	main()
