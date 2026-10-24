#!/usr/bin/env python3
"""A third-party kext in /Library/Extensions whose dependency this macOS lacks stops kmutil for every kext. The stop
names that kext, its folder and what to do (2.5.3 report: RtWlanU1827 needed com.apple.iokit.IOUSBFamily)."""
import pathlib
import plistlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
src = (ROOT / "package/install.sh").read_text()
start = src.index("kmutil_blocker() {")
fn = src[start:src.index("\n}\n", start) + 3]
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({detail.strip()[-300:]})"))


def run(log_text):
    with tempfile.TemporaryDirectory() as d:
        ext = pathlib.Path(d) / "Extensions"
        k = ext / "RtWlanU1827.kext/Contents"
        k.mkdir(parents=True)
        (k / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.realtek.driver.RtWlanU1827"}))
        log = pathlib.Path(d) / "kmutil.log"
        log.write_text(log_text)
        p = subprocess.run(["/bin/bash", "-c", fn + 'kmutil_blocker "$1" "$2"', "x", str(log), str(ext)],
                           capture_output=True, text=True, timeout=30)
        return p.stdout + p.stderr, str(ext)


out, ext = run("No variant specified, falling back to release\nUnable to resolve dependencies: 'com.realtek.driver.RtWlanU1827' "
               "names a dependency on 'com.apple.iokit.IOUSBFamily', which was not found.\n")
arm("the stop names the kext and the dependency it lacks", "com.realtek.driver.RtWlanU1827 needs com.apple.iokit.IOUSBFamily" in out, out)
arm("and says where it is and what to do", f"Move {ext}/RtWlanU1827.kext out of {ext}" in out, out)
out, _ = run("Unable to resolve dependencies: 'com.nullmoth.NVAccel' names a dependency on 'com.apple.iokit.IOGraphicsFamily', which was not found.\n")
arm("a dependency failure of our own kexts is not blamed on the user", out.strip() == "", out)
out, _ = run("Error: some other kmutil failure\n")
arm("another kmutil failure adds nothing", out.strip() == "", out)
print(f"{fails} failed")
sys.exit(1 if fails else 0)
