"""Interleave software and NVENC encoding of real NV12 frames, including cold probe cost.

The source is a 60-frame 3840x2160 raw NV12 clip. Smaller raw inputs are regenerable CPU
resizes. Each timing starts below 11000 MiB GPU memory and at most 10% GPU utilization.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import time

os.environ["OPENBLAS_NUM_THREADS"] = "1"
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
parser.add_argument("--sizes", default="1280x720,1920x1080,2560x1440,3840x2160")
parser.add_argument("--frames", default="12,30,60,120,240")
parser.add_argument("--rounds", type=int, default=3)
parser.add_argument("--timeout", type=int, default=300)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
(args.out / "runner.json").write_text(json.dumps({"pid": os.getpid(), "source": str(args.source), "started": time.time()}), encoding="utf-8")
tags = ["-colorspace", "bt709", "-color_range", "tv", "-color_primaries", "bt709", "-color_trc", "iec61966-2-1", "-chroma_sample_location", "center"]
software = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "17", "-pix_fmt", "nv12"]
hardware = ["-c:v", "h264_nvenc", "-preset", "p1", "-tune", "hq", "-rc", "vbr", "-cq", "19", "-b:v", "0", "-bf", "3", "-pix_fmt", "nv12", "-spatial-aq", "1", "-aq-strength", "8", "-bsf:v", "h264_metadata=chroma_sample_loc_type=1"]


def run(command):
    started = time.perf_counter()
    result = subprocess.run(command, capture_output=True, text=True, creationflags=FLAGS)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return (time.perf_counter() - started) * 1000


def gate():
    deadline = time.monotonic() + args.timeout
    while True:
        memory, utilization = map(int, subprocess.check_output(
            ["nvidia-smi", "--query-gpu=memory.used,utilization.gpu", "--format=csv,noheader,nounits"],
            text=True, creationflags=FLAGS).strip().split(","))
        if memory < 11000 and utilization <= 10:
            return {"memory_mib": memory, "utilization_pct": utilization}
        if time.monotonic() > deadline:
            raise RuntimeError(f"GPU remained busy: {memory} MiB, {utilization}%")
        time.sleep(1)


inputs = {}
completed = set()
if (args.out / "results.jsonl").exists():
    for line in (args.out / "results.jsonl").read_text().splitlines():
        row = json.loads(line)
        completed.add((row["size"], row["frames"], row["repeat"], row["encoder"]))
for size in args.sizes.split(","):
    if size == "3840x2160":
        inputs[size] = args.source
        continue
    target = args.out / f"raw-{size}.nv12"
    if not target.exists():
        run(["ffmpeg", "-v", "error", "-y", "-filter_threads", "1", "-f", "rawvideo", "-pix_fmt", "nv12", "-s", "3840x2160", "-i", str(args.source),
             "-vf", f"scale={size.replace('x', ':')}", "-pix_fmt", "nv12", "-f", "rawvideo", str(target)])
    inputs[size] = target
for size, source in inputs.items():
    width, height = map(int, size.split("x"))
    for frames in map(int, args.frames.split(",")):
        for repeat in range(args.rounds):
            for encoder, options in [("libx264", software), ("h264_nvenc", hardware)]:
                if (size, frames, repeat, encoder) in completed:
                    continue
                before = gate()
                probe = 0.0
                if encoder == "h264_nvenc":
                    probe = run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", f"color=c=black:s={size}:r=1", "-frames:v", "1", *options, "-f", "null", "-"])
                # A second gate excludes GPU activity that began during the probe.
                before = gate()
                video = args.out / f"{encoder}-{size}-{frames}-{repeat}.mp4"
                elapsed = run(["ffmpeg", "-v", "error", "-y", "-filter_threads", "2", "-stream_loop", "-1", "-f", "rawvideo", "-pix_fmt", "nv12", "-s", size, "-framerate", "30", *tags,
                               "-i", str(source), "-frames:v", str(frames), *options, *tags, "-movflags", "+faststart", str(video)])
                row = {"size": size, "frames": frames, "repeat": repeat, "encoder": encoder, "probe_ms": probe,
                       "encode_ms": elapsed, "cold_ms": probe + elapsed, "pixels_frames": width * height * frames,
                       "bytes": video.stat().st_size, "gpu_before": before}
                with (args.out / "results.jsonl").open("a", encoding="utf-8") as file:
                    file.write(json.dumps(row) + "\n")
                print(json.dumps(row), flush=True)
