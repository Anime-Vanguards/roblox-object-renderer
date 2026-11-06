"""Measure tagged video against lossless ror PNGs without retaining decoded frame dumps.

PSNR/SSIM compare decoded RGB directly with the rendered PNGs, without another YUV quantization.
Channel errors are measured directly in RGB, optionally forcing a player's matrix assumption.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

os.environ["OPENBLAS_NUM_THREADS"] = "1"
import numpy as np
from PIL import Image

FLAGS = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0


def run(command):
    result = subprocess.run(command, capture_output=True, text=True, creationflags=FLAGS)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result.stdout, result.stderr


def quality(video, reference, ffmpeg="ffmpeg", matrix=None, fps=30, accurate=True, metric="rgb"):
    frames = sorted(reference.glob("*.png"))
    with Image.open(frames[0]) as image:
        width, height = image.size
    options = ([f"in_color_matrix={matrix}"] if matrix else []) + (["flags=bicubic+accurate_rnd+full_chroma_int"] if accurate else [])
    conversion = "scale=" + ":".join(options) + "," if options else ""
    metric_space = "format=gbrp" if metric == "rgb" else "format=gbrp,scale=out_color_matrix=bt601:out_range=tv,format=yuv420p"
    graph = (f"[0:v]{conversion}{metric_space},split[a][b];"
             f"[1:v]format=gbrap,premultiply=inplace=1:planes=7,{metric_space},split[c][d];[a][c]psnr[p];[b][d]ssim[s]")
    _, logs = run([ffmpeg, "-hide_banner", "-threads", "2", "-i", str(video),
                   "-threads", "2", "-framerate", str(fps), "-start_number", "0", "-i", str(reference / "%04d.png"),
                   "-filter_complex_threads", "1", "-filter_complex", graph,
                   "-map", "[p]", "-map", "[s]", "-f", "null", "-"])
    psnr = re.search(r"PSNR .*?average:(\S+)", logs)
    ssim = re.search(r"SSIM .*?All:(\S+)", logs)
    if not psnr or not ssim:
        raise RuntimeError(logs)
    command = [ffmpeg, "-hide_banner", "-loglevel", "error", "-threads", "2", "-i", str(video),
               "-filter_threads", "1", "-vf", conversion + "format=rgb24", "-fps_mode", "passthrough",
               "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]
    absolute = np.zeros(3, dtype=np.int64)
    signed = np.zeros(3, dtype=np.int64)
    maximum = np.zeros(3, dtype=np.int16)
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=FLAGS) as process:
        for frame in frames:
            data = bytearray(width * height * 3)
            view = memoryview(data)
            offset = 0
            while offset < len(data):
                count = process.stdout.readinto(view[offset:])
                if not count:
                    raise RuntimeError(f"Missing decoded frame {frame}: {process.stderr.read().decode()}")
                offset += count
            decoded = np.frombuffer(data, dtype=np.uint8).reshape(height, width, 3)
            with Image.open(frame) as image:
                source = np.asarray(image.convert("RGBA"))
                # Match MP4's integer alpha flattening over black, including transparent references.
                source = ((source[:, :, :3].astype(np.uint16) * source[:, :, 3:4] + 127) // 255).astype(np.uint8)
            for row in range(0, height, 128):
                difference = decoded[row:row + 128].astype(np.int16) - source[row:row + 128]
                magnitude = np.abs(difference)
                signed += difference.sum(axis=(0, 1))
                absolute += magnitude.sum(axis=(0, 1))
                maximum = np.maximum(maximum, magnitude.max(axis=(0, 1)))
        if process.stdout.read(1):
            raise RuntimeError("Video has more frames than the reference")
        errors = process.stderr.read().decode()
        if process.wait():
            raise RuntimeError(errors)
    pixels = width * height * len(frames)
    metadata, _ = run([str(Path(ffmpeg).with_name("ffprobe.exe")) if Path(ffmpeg).is_absolute() else "ffprobe",
                       "-v", "error", "-select_streams", "v:0", "-show_entries",
                       "stream=width,height,pix_fmt,color_space,color_range,color_transfer,color_primaries,chroma_location,nb_frames",
                       "-of", "json", str(video)])
    return {"video": str(video), "frames": len(frames), "bytes": video.stat().st_size,
            "psnr_db": psnr.group(1), "ssim": float(ssim.group(1)),
            "rgb_mean_absolute": (absolute / pixels).tolist(), "rgb_maximum": maximum.tolist(),
            "rgb_mean_signed": (signed / pixels).tolist(), "decode_matrix": matrix or "tags/default",
            "accurate_decode": accurate,
            "metric_space": metric,
            "metadata": json.loads(metadata)["streams"][0]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("video", type=Path)
    parser.add_argument("reference", type=Path)
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--matrix", choices=["bt601", "bt709"])
    parser.add_argument("--fps", type=float, default=30)
    parser.add_argument("--fast-decode", action="store_true", help="Reproduce ffmpeg's default RGB rounding/interpolation")
    parser.add_argument("--metric", choices=["rgb", "yuv420p"], default="rgb")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    report = quality(args.video, args.reference, args.ffmpeg, args.matrix, args.fps, not args.fast_decode, args.metric)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report), flush=True)
