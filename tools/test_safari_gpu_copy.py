#!/usr/bin/env python3
"""package/safari_gpu_copy.sh: the optional driver copy for Safari's sandboxed graphics process.

Runs the script against a temp folder standing in for the Preboot volume (NULLMOTH_SAFARI_COPY_ROOT).
"""
import os, pathlib, subprocess, sys, tempfile

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "package/safari_gpu_copy.sh"
fails = 0


def check(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    fails += not ok


def run(root, *args):
    env = dict(os.environ, NULLMOTH_SAFARI_COPY_ROOT=str(root))
    return subprocess.run(["/bin/bash", str(SCRIPT), *args], capture_output=True, text=True, env=env)


with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    src = t / "src/NVMTLDriver.bundle/Contents/MacOS"; src.mkdir(parents=True)
    (src / "NVMTLDriver").write_bytes(b"binary")
    bundle = t / "src/NVMTLDriver.bundle"

    root = t / "preboot"
    r = run(root, "install", str(bundle))
    check("no cryptex OS folder: skipped, nothing created", "does not exist" in r.stdout and not root.exists(), r.stdout)

    (root / "Cryptexes/OS").mkdir(parents=True)
    r = run(root, "install", str(bundle))
    dest = root / "Cryptexes/OS/Library/GPUBundles"
    check("install copies the bundle under the cryptex GPUBundles folder", (dest / "NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver").read_bytes() == b"binary", r.stdout + r.stderr)
    check("install leaves a marker naming the binary hash", (dest / ".nullmoth-safari-copy").exists())
    check("status reports it", "installed" in run(root, "status").stdout)

    (dest / "Other.bundle").mkdir()
    run(root, "remove")
    check("remove deletes the bundle and the marker", not (dest / "NVMTLDriver.bundle").exists() and not (dest / ".nullmoth-safari-copy").exists())
    check("remove leaves a bundle it did not make", (dest / "Other.bundle").exists())

    (dest / "Other.bundle").rmdir()
    run(root, "install", str(bundle)); run(root, "remove")
    check("remove takes the folders it made when they end up empty", not (root / "Cryptexes/OS/Library").exists())

    r = run(root, "remove")
    check("remove with nothing installed does nothing", "none installed" in r.stdout and r.returncode == 0)

    # an OS folder that is a link out of the volume is refused
    outside = t / "outside"; outside.mkdir()
    root2 = t / "preboot2"; (root2 / "Cryptexes").mkdir(parents=True); os.symlink(outside, root2 / "Cryptexes/OS")
    r = run(root2, "install", str(bundle))
    check("an OS folder that links out of the volume is refused", "not on the Preboot volume" in r.stdout and not list(outside.iterdir()), r.stdout)

    # a folder that refuses writes leaves nothing behind
    root3 = t / "preboot3"; osd = root3 / "Cryptexes/OS"; osd.mkdir(parents=True); osd.chmod(0o555)
    if os.geteuid() != 0:
        r = run(root3, "install", str(bundle))
        check("a folder that refuses writes: nothing changed, exit 0", "refuses writes" in r.stdout and r.returncode == 0 and not (osd / "Library").exists(), r.stdout)
    osd.chmod(0o755)
print("FAILED" if fails else "all arms pass")
sys.exit(1 if fails else 0)
