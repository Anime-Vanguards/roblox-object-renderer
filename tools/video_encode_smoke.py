"""Short memory-gated format, odd-size, Highlight and one-slot POST /video checks.

Never stops an unrelated service. Every child console is hidden, and only the explicitly
started service PID is shut down. The ffmpeg shim advertises NVENC but rejects its open probe.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

os.environ["OPENBLAS_NUM_THREADS"] = "1"
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--base", type=Path, required=True)
parser.add_argument("--new", type=Path, required=True)
parser.add_argument("--input", type=Path, required=True)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
state = Path(os.environ["ROR_STATE_DIR"])


def run(command, **kwargs):
    result = subprocess.run(list(map(str, command)), capture_output=True, creationflags=FLAGS, **kwargs)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result.stdout


def gate():
    deadline = time.monotonic() + 300
    while True:
        memory = int(run(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits"], text=True))
        if memory < 11000:
            return
        if time.monotonic() > deadline:
            raise RuntimeError("GPU memory remained above the safety limit")
        time.sleep(2)


def video(binary, output, format, size="319x179", source=None):
    gate()
    result = run([binary, "video", source or args.input, "--view", "iso", "--background", "transparent",
                  "--size", size, "--duration", "0.2", "--fps", "30", "--format", format,
                  "--out", output, "--json", "--no-service"], text=True)
    return json.loads(result[result.index("{"):])


def hashes(directory):
    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in directory.glob("*.png")}


def decode(path, alpha=False):
    command = ["ffmpeg", "-v", "error", "-threads", "2", "-filter_threads", "1"]
    if alpha:
        command += ["-c:v", "libvpx-vp9"]
    return run([*command, "-i", path, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])


report = {}
for format in ["png", "apng", "gif", "webm", "mp4"]:
    summaries = [video(binary, args.out / f"{name}-{format}", format) for name, binary in [("base", args.base), ("new", args.new)]]
    assert [(summary["width"], summary["height"]) for summary in summaries] == ([(320, 180)] * 2 if format in ["webm", "mp4"] else [(319, 179)] * 2)
    identical = hashes(Path(summaries[0]["out"])) == hashes(Path(summaries[1]["out"]))
    assert identical, "Contact sheets differ"
    if format == "png":
        assert hashes(Path(summaries[0]["video"])) == hashes(Path(summaries[1]["video"]))
    elif format != "mp4":
        first, second = [decode(Path(summary["video"]), format == "webm") for summary in summaries]
        assert first == second, format + " decoded pixels differ"
        if format == "webm":
            alpha = first[3::4]
            assert min(alpha) < max(alpha), "WebM alpha was lost"
    report[format] = {"dimensions": [summaries[1]["width"], summaries[1]["height"]], "frames": summaries[1]["frames"], "sheet_identical": identical, "encoder": summaries[1]["encoder"]}
    print(format, report[format], flush=True)

for size in ["321x179", "2x2"]:
    summaries = [video(binary, args.out / f"{name}-compact-{size}", "mp4", size)
                 for name, binary in [("base", args.base), ("new", args.new)]]
    assert all(summary["frames"] == 6 for summary in summaries)
    assert [(summary["width"], summary["height"]) for summary in summaries] == (
        [(322, 180)] * 2 if size == "321x179" else [(2, 2)] * 2)
    assert hashes(Path(summaries[0]["out"])) == hashes(Path(summaries[1]["out"])), "Compact sheet pixels differ"
    report[f"compact-{size}"] = "MP4 frame count/dimensions and exact sheet pixels pass"

fixture = args.out / "highlight.rbxmx"
fixture.write_text('''<roblox version="4"><Item class="Model" referent="MODEL"><Properties><string name="Name">HighlightCheck</string></Properties><Item class="Part" referent="PART"><Properties><bool name="Anchored">true</bool><Vector3 name="size"><X>4</X><Y>4</Y><Z>4</Z></Vector3><Color3 name="Color"><R>1</R><G>0.2</G><B>0.05</B></Color3></Properties><Item class="Highlight" referent="HIGHLIGHT"><Properties><Ref name="Adornee">PART</Ref><Color3 name="FillColor"><R>0</R><G>1</G><B>1</B></Color3><float name="FillTransparency">0.3</float></Properties></Item></Item></Item></roblox>''', encoding="utf-8")
for name, binary in [("base", args.base), ("new", args.new)]:
    video(binary, args.out / f"{name}-highlight-png", "png", "320x180", fixture)
    video(binary, args.out / f"{name}-highlight-mp4", "mp4", "319x179", fixture)
    gate()
    run([binary, "render", fixture, "--views", "iso", "--size", "319x179", "--ids", "--background", "transparent", "--out", args.out / f"{name}-still", "--json", "--no-service"])
assert hashes(args.out / "base-highlight-png" / "frames") == hashes(args.out / "new-highlight-png" / "frames")
assert hashes(args.out / "base-highlight-mp4") == hashes(args.out / "new-highlight-mp4")
assert hashes(args.out / "base-still") == hashes(args.out / "new-still")
report["highlight_and_still"] = "RGBA frames, MP4 sheets, still image and IDs identical"

shim_source = args.out / "ffmpeg-shim.rs"
shim_source.write_text('''use std::process::{Command, Stdio};
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.iter().any(|value| value == "-encoders") {
        println!(" V....D libx264 H.264\\n V....D h264_nvenc NVIDIA\\n V....D libvpx-vp9 VP9\\n V....D gif GIF");
        return;
    }
    if arguments.windows(2).any(|values| values[0] == "-c:v" && values[1] == "h264_nvenc") {
        std::process::exit(1);
    }
    let mut command = Command::new(std::env::var("ROR_TEST_FFMPEG").unwrap());
    command.args(arguments).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    std::process::exit(command.status().unwrap().code().unwrap_or(1));
}''', encoding="utf-8")
shim = args.out / "ffmpeg-shim.exe"
run(["rustc", "-O", shim_source, "-o", shim])
environment = os.environ.copy()
environment.update(ROR_VIDEO_ENCODER="h264_nvenc", ROR_TEST_FFMPEG=shutil.which("ffmpeg"))
build = f"d-encode-smoke-{os.getpid()}"
gate()
with (args.out / "service.log").open("wb") as log:
    process = subprocess.Popen([str(args.new.resolve()), "serve", "--state-dir", str(state), "--gpu-slots", "1", "--idle", "60", "--build", build, "--source", str(args.new.resolve())],
                               stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=environment, creationflags=FLAGS)
    info = None
    try:
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            record = state / "services" / f"{build}.json"
            if record.exists():
                info = json.loads(record.read_text())
                if info["pid"] == process.pid:
                    break
            if process.poll() is not None:
                raise RuntimeError("Owned service exited during startup")
            time.sleep(0.1)
        if info is None or info["pid"] != process.pid:
            raise RuntimeError("Owned service did not start")
        def request(path, data=None):
            request = urllib.request.Request(f"http://127.0.0.1:{info['port']}{path}", data=data, headers={"X-ROR": "1", "Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        assert request("/health")["pid"] == process.pid
        job = {"input": str(args.input.resolve()), "out": str(args.out / "service-fallback"), "view": {"width": 319, "height": 179, "background": "transparent"},
               "duration": 0.2, "fps": 30, "format": "mp4", "ffmpeg": str(shim.resolve())}
        gate()
        result = request("/video", json.dumps(job).encode())
        assert result["encoder"] == "libx264" and result["frames"] == 6
        assert (result["width"], result["height"]) == (320, 180)
        report["one_slot_service_failed_probe"] = {"encoder": result["encoder"], "frames": result["frames"], "pid": process.pid}
    finally:
        if info is not None and info["pid"] == process.pid:
            try:
                request("/shutdown", b"{}")
            except OSError:
                pass
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=10)
(args.out / "smoke.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
print(json.dumps(report), flush=True)
