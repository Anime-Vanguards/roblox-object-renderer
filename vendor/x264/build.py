# SPDX-License-Identifier: GPL-2.0-or-later
"""Builds ror_x264.lib, the H.264 encoder ror links with the `x264` feature (docs/building.md):
x264 at the commit below with its assembly, 8-bit only, and ror_x264.c (ror's entry points into it),
into the folder given last (vendor/x264/x86_64-pc-windows-msvc by default); point ROR_X264_LIB_DIR at
that folder when you build ror. x264 is GPL-2.0-or-later, like ror (THIRD_PARTY_NOTICES.md).

    git clone https://code.videolan.org/videolan/x264.git x264
    git -C x264 checkout c24e06c2e184345ceb33eb20a15d1024d9fd3497
    # nasm 2.16.03: https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/win64/nasm-2.16.03-win64.zip
    # (sha256 3ee4782247bcb874378d02f7eab4e294a84d3d15f3f6ee2de2f47a46aa7226e6), unzipped anywhere
    python vendor/x264/build.py x264 nasm-2.16.03/nasm.exe [<folder>]

It needs Visual Studio 2022's C tools (cl and lib, found through vswhere) and Git for Windows' bash for
x264's configure; there is no make: the Makefile's `lib-static` is done here, every C file through cl,
every .asm through nasm, one lib.exe archive. cl, nasm and lib run with /Brepro / --reproducible, so the
same tools make the same bytes (BUILD.txt records them and the library's SHA-256).

The commit is the x264 in ffmpeg 7.1 (gyan.dev's build, "core 164 r3192 c24e06c"), which encoded ror's
MP4s through a pipe until in-process encoding: the same encoder, set up as ffmpeg sets it up.

One change to x264: common/win32thread.c starts each thread at `x264_win32_thread_priority`
(ror_x264_thread_priority), so the encoder's threads run below ror's draws as ffmpeg's process did.
"""

import hashlib
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

COMMIT = "c24e06c2e184345ceb33eb20a15d1024d9fd3497"
VERSION = '#define X264_VERSION " r3192 c24e06c"'
NASM_VERSION = "2.16.03"
HERE = Path(__file__).resolve().parent
TARGET = "x86_64-pc-windows-msvc"

CONFIGURE = [
    "--disable-cli",
    "--enable-static",
    "--bit-depth=8",
    "--disable-opencl",
    "--disable-avs",
    "--disable-swscale",
    "--disable-lavf",
    "--disable-ffms",
    "--disable-gpac",
    "--disable-lsmash",
    # the dynamic C runtime, as Rust's x86_64-pc-windows-msvc links
    "--extra-cflags=-MD",
]

# The Makefile's lists (c24e06c) for an x86-64 Windows build with threads and 8-bit depth
SRCS = ["common/osdep.c", "common/base.c", "common/cpu.c", "common/tables.c", "encoder/api.c", "common/win32thread.c"]
SRCS_X = [
    "common/mc.c", "common/predict.c", "common/pixel.c", "common/macroblock.c", "common/frame.c", "common/dct.c",
    "common/cabac.c", "common/common.c", "common/rectangle.c", "common/set.c", "common/quant.c", "common/deblock.c",
    "common/vlc.c", "common/mvpred.c", "common/bitstream.c", "encoder/analyse.c", "encoder/me.c",
    "encoder/ratecontrol.c", "encoder/set.c", "encoder/macroblock.c", "encoder/cabac.c", "encoder/cavlc.c",
    "encoder/encoder.c", "encoder/lookahead.c", "common/threadpool.c", "common/x86/mc-c.c", "common/x86/predict-c.c",
]
ASM = ["common/x86/cpu-a.asm"]
ASM_X = [
    "common/x86/dct-64.asm", "common/x86/trellis-64.asm", "common/x86/bitstream-a.asm", "common/x86/const-a.asm",
    "common/x86/cabac-a.asm", "common/x86/dct-a.asm", "common/x86/deblock-a.asm", "common/x86/mc-a.asm",
    "common/x86/mc-a2.asm", "common/x86/pixel-a.asm", "common/x86/predict-a.asm", "common/x86/quant-a.asm",
    "common/x86/sad-a.asm",
]

WORKER = """static unsigned __stdcall win32thread_worker( void *arg )
{
    x264_pthread_t *h = arg;
"""
PATCHED = """/* ror: the priority each thread x264 starts runs at (vendor/x264/ror_x264.c) */
int x264_win32_thread_priority = 0;

static unsigned __stdcall win32thread_worker( void *arg )
{
    x264_pthread_t *h = arg;
    if( x264_win32_thread_priority )
        SetThreadPriority( GetCurrentThread(), x264_win32_thread_priority );
"""


def msvc_environment():
    vswhere = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
    install = subprocess.run([str(vswhere), "-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"], capture_output=True, text=True, check=True).stdout.strip()
    vcvars = Path(install) / "VC" / "Auxiliary" / "Build" / "vcvars64.bat"
    listed = subprocess.run(f'cmd /d /c ""{vcvars}" >nul && set"', capture_output=True, text=True, shell=True, check=True).stdout
    environment = {}
    for line in listed.splitlines():
        name, _, value = line.partition("=")
        if name:
            environment[name] = value
    return environment


def git_bash():
    git = shutil.which("git")
    if git is None:
        sys.exit("git (Git for Windows) is needed")
    # git.exe is in <Git>/cmd, <Git>/bin or <Git>/mingw64/bin
    for root in Path(git).resolve().parents:
        for candidate in (root / "bin" / "bash.exe", root / "usr" / "bin" / "bash.exe"):
            if candidate.is_file():
                return candidate
    sys.exit("Git for Windows' bash.exe not found")


def remove(path):
    # git's objects are read-only
    def writable(function, name, _):
        os.chmod(name, stat.S_IWRITE)
        function(name)

    if path.exists():
        shutil.rmtree(path, onerror=writable)


def run(command, **options):
    # Windows finds a bare program name on this process's PATH, not the environment given
    path = options.get("env", os.environ)["PATH"]
    command = [shutil.which(command[0], path=path) or command[0], *command[1:]]
    result = subprocess.run(command, capture_output=True, text=True, **options)
    if result.returncode != 0:
        sys.exit(f"failed: {command}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def make_variables(text):
    variables = {}
    for line in text.splitlines():
        match = re.match(r"^([A-Za-z_]+)\s*=\s?(.*)$", line)
        if match:
            variables[match.group(1)] = match.group(2)
    return variables


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    source = Path(sys.argv[1]).resolve()
    nasm = Path(sys.argv[2]).resolve()
    out = Path(sys.argv[3]).resolve() if len(sys.argv) > 3 else HERE / TARGET
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"]).strip()
    if head != COMMIT:
        sys.exit(f"{source} is at {head}, not {COMMIT}")
    nasm_version = run([str(nasm), "-v"]).strip()
    if NASM_VERSION not in nasm_version:
        sys.exit(f"{nasm} is {nasm_version!r}, not nasm {NASM_VERSION}")
    environment = msvc_environment()
    environment["PATH"] = str(nasm.parent) + os.pathsep + environment["PATH"]
    environment["CC"] = "cl"
    environment["AS"] = "nasm"
    bash = git_bash()
    # a path without spaces for configure, the same every build (the objects and the archive name their
    # paths), and a clone of the checkout (its history gives the version)
    work = Path(tempfile.gettempdir()) / "ror-x264-build"
    remove(work)
    work.mkdir()
    try:
        tree = work / "x264"
        run(["git", "clone", "--quiet", str(source), str(tree)])
        run(["git", "-C", str(tree), "checkout", "--quiet", COMMIT])
        build = work / "build"
        build.mkdir()
        run([str(bash), "../x264/configure", *CONFIGURE], cwd=build, env=environment)
        config = (build / "x264_config.h").read_text()
        if VERSION not in config:
            sys.exit(f"x264_config.h names another version:\n{config}")
        for define in ("HAVE_WIN32THREAD 1", "HAVE_THREAD 1", "HAVE_BITDEPTH8 1", "ARCH_X86_64 1"):
            if define not in (build / "config.h").read_text():
                sys.exit(f"config.h lacks {define}")
        variables = make_variables((build / "config.mak").read_text())
        cflags = variables["CFLAGS"].replace("$(SRCPATH)", str(tree)).split()
        asflags = variables["ASFLAGS"].replace("$(SRCPATH)", str(tree)).split()
        if "-fp:fast" not in cflags and "-O2" not in cflags:
            print("note: CFLAGS", cflags)
        patched = work / "patched" / "common"
        patched.mkdir(parents=True)
        threads = (tree / "common" / "win32thread.c").read_text()
        if threads.count(WORKER) != 1:
            sys.exit("common/win32thread.c's worker is not as expected")
        (patched / "win32thread.c").write_text(threads.replace(WORKER, PATCHED))
        objects = []
        compiler = ["cl", "-Brepro", *cflags, f"-I{build}", f"-I{tree}", f"-I{tree / 'common'}"]

        def compile_c(path, extra, name):
            obj = build / "obj" / name
            obj.parent.mkdir(parents=True, exist_ok=True)
            run([*compiler, *extra, "-c", str(path), f"-Fo{obj}"], cwd=build, env=environment)
            objects.append(obj)

        def assemble(path, extra, name):
            obj = build / "obj" / name
            obj.parent.mkdir(parents=True, exist_ok=True)
            run(["nasm", "--reproducible", *asflags, f"-I{tree / 'common' / 'x86'}/", *extra, "-o", str(obj), str(path)], cwd=build, env=environment)
            objects.append(obj)

        for file in SRCS:
            path = patched / "win32thread.c" if file == "common/win32thread.c" else tree / file
            compile_c(path, [], file.replace("/", "_").replace(".c", ".obj"))
        for file in SRCS_X:
            compile_c(tree / file, ["-DHIGH_BIT_DEPTH=0", "-DBIT_DEPTH=8"], file.replace("/", "_").replace(".c", "-8.obj"))
        for file in ASM:
            assemble(tree / file, [], file.replace("/", "_").replace(".asm", ".obj"))
        for file in ASM_X:
            assemble(tree / file, ["-DBIT_DEPTH=8", "-Dprivate_prefix=x264_8"], file.replace("/", "_").replace(".asm", "-8.obj"))
        compile_c(HERE / "ror_x264.c", [], "ror_x264.obj")
        out.mkdir(parents=True, exist_ok=True)
        library = out / "ror_x264.lib"
        if library.exists():
            library.unlink()
        run(["lib", "/nologo", "/Brepro", f"/OUT:{library}", *(str(obj.relative_to(build)) for obj in objects)], cwd=build, env=environment)
        digest = hashlib.sha256(library.read_bytes()).hexdigest()
        cl_version = subprocess.run([shutil.which("cl", path=environment["PATH"])], capture_output=True, text=True, env=environment).stderr.splitlines()[0].strip()
        (out / "BUILD.txt").write_bytes(
            "\n".join([
                "ror_x264.lib: vendor/x264/build.py",
                f"x264 {COMMIT} ({VERSION.split(chr(34))[1].strip()}), configure {' '.join(CONFIGURE)}",
                f"{cl_version}",
                f"{nasm_version}",
                f"CFLAGS {' '.join(variables['CFLAGS'].split())}",
                f"ASFLAGS {' '.join(variables['ASFLAGS'].split())}",
                f"sha256 {digest}",
                "",
            ]).encode()
        )
        shutil.copyfile(tree / "COPYING", HERE / "COPYING")
        print(f"{library} ({library.stat().st_size} bytes, sha256 {digest})")
    finally:
        remove(work)


if __name__ == "__main__":
    main()
