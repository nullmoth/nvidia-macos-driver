#!/usr/bin/env python3
"""The installer names missing AMFI boot arguments. Without them WindowServer (a platform binary) refuses the Metal
plugin ("mapping process is a platform binary, but mapped file is not") and asserts "Failed to create MetalDevice";
two 2.6.x reports had nvfb=1 nvaccel=1 but neither AMFI argument."""
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
src = (ROOT / "package/install.sh").read_text()
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({detail.strip()[-300:]})"))


arm("install.sh defines amfi_note once", src.count("amfi_note() {") == 1, str(src.count("amfi_note() {")))
arm("and calls it on the live boot-args", 'amfi_note "$ba"' in src)
start = src.index("amfi_note() {")
fn = src[start:src.index("\n}\n", start) + 3]


def note(ba):
    p = subprocess.run(["/bin/bash", "-c", fn + 'amfi_note "$1"', "x", ba], capture_output=True, text=True, timeout=30)
    return p.stdout + p.stderr


field = "nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth ipc_control_port_options=0 nvrmacpibl=1"
out = note(field)
arm("the field boot-args (no AMFI argument) get the note", "amfi_get_out_of_my_way=0x1 amfi=0x80" in out, out)
out = note("-v keepsyms=1 amfi_get_out_of_my_way=0x1 amfi=0x80 nvfb=1 nvaccel=1")
arm("negative control: the 1401 boot-args get no note", out.strip() == "", out)
out = note("amfi=0x80 nvfb=1")
arm("negative control: amfi= alone gets no note", out.strip() == "", out)
out = note("nvfb=1 amfi_get_out_of_my_way=1")
arm("negative control: amfi_get_out_of_my_way alone gets no note", out.strip() == "", out)
out = note("")
arm("empty boot-args get the note", "NOTE" in out, out)
print(f"{fails} failed")
sys.exit(1 if fails else 0)
