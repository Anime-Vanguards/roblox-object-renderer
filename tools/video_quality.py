"""Compare decoded video frames with another clip and its lossless reference PNG frames.

python tools/video_quality.py BASE_VIDEO NEW_VIDEO REFERENCE_FRAMES --out REPORT_DIRECTORY
Reports YUV420 PSNR/SSIM, including each encoder against the same lossless reference.
Decoded PNGs are temporary and removed after measurement; ffmpeg uses bounded CPU threads.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("base", type=Path)
parser.add_argument("new", type=Path)
parser.add_argument("reference", type=Path)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--ffmpeg", default="ffmpeg")
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0


def run(arguments):
    result = subprocess.run([args.ffmpeg, "-hide_banner", *arguments], capture_output=True, text=True, creationflags=flags)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result.stderr


def metrics(first, second):
    graph = "[0:v]format=yuv420p,split[a][b];[1:v]format=yuv420p,split[c][d];[a][c]psnr[p];[b][d]ssim[s]"
    logs = run(["-threads", "2", "-framerate", "30", "-i", str(first), "-threads", "2", "-framerate", "30", "-i", str(second), "-filter_complex_threads", "1", "-filter_complex", graph, "-map", "[p]", "-map", "[s]", "-f", "null", "-"])
    psnr = re.search(r"PSNR .*?average:(\S+)", logs)
    ssim = re.search(r"SSIM .*?All:(\S+)", logs)
    if not psnr or not ssim:
        raise RuntimeError(logs)
    return {"psnr_db": psnr.group(1), "ssim": float(ssim.group(1))}


with tempfile.TemporaryDirectory(prefix="decoded-", dir=args.out) as temporary:
    temporary = Path(temporary)
    assert temporary.resolve().is_relative_to(args.out.resolve())
    decoded = []
    counts = []
    for name, video in [("base", args.base), ("new", args.new)]:
        directory = temporary / name
        directory.mkdir()
        pattern = directory / "%04d.png"
        run(["-loglevel", "error", "-threads", "2", "-i", str(video), "-fps_mode", "passthrough", "-threads", "2", "-start_number", "0", str(pattern)])
        decoded.append(pattern)
        counts.append(len(list(directory.glob("*.png"))))
    reference = args.reference / "%04d.png"
    expected = len(list(args.reference.glob("*.png")))
    if counts != [expected, expected]:
        raise RuntimeError(f"Frame counts differ: decoded {counts}, reference {expected}")
    report = {
        "base": str(args.base),
        "new": str(args.new),
        "frames": expected,
        "decoded_base_vs_new": metrics(*decoded),
        "base_vs_lossless": metrics(decoded[0], reference),
        "new_vs_lossless": metrics(decoded[1], reference),
        "base_bytes": args.base.stat().st_size,
        "new_bytes": args.new.stat().st_size,
    }
    (args.out / "quality.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report), flush=True)
