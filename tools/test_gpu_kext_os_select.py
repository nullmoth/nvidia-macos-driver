#!/usr/bin/env python3
"""The opt-in AMD/Intel/NPU kexts are cached per macOS (kexts/15, kexts/26) and selected by the running OS, like
NVAccel. NMXAccel is built per-OS (Tahoe privatised IOAcceleratorFamily2); NMXGPU/NMXNPU are the same binary on both.
Static arms check the path is $MAJ everywhere; a dynamic arm runs the installer's refresh block in a sandbox and the
negative control is the old hardcoded kexts/15 block, which picks the wrong OS's kext on macOS 26."""
import os, pathlib, re, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
INSTALL = ROOT / "package/install.sh"
APPINSTALL = ROOT / "app/Resources/nullmoth-install.sh"
SETUP = ROOT / "app/Resources/nullmoth-setup.sh"
fails = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    if not ok:
        fails.append(name)


# --- static: no NMX kext is pinned to kexts/15 any more; the path is $MAJ ---
for f in (INSTALL, APPINSTALL):
    t = f.read_text()
    arm(f"{f.name}: NMX kexts select kexts/$MAJ", "/Library/NullMoth/kexts/$MAJ/NMXGPU.kext" in t)
    arm(f"{f.name}: no NMX kext pinned to kexts/15", not re.search(r"kexts/15/NMX", t), )
st = SETUP.read_text()
arm("setup: --gpu-test stages from kexts/$KMAJ", "KD=/Library/NullMoth/kexts/$KMAJ" in st and "GK=$KD/NMXGPU.kext" in st)
arm("setup: no NMX kext pinned to kexts/15", "kexts/15/NMX" not in st)

# --- dynamic: the installer refresh block copies the kext for the running OS ---
block = re.search(r"(if \[ -d \"\$EXT/NMXGPU\.kext\".*?\nfi\n)", INSTALL.read_text(), re.S)
refresh = block.group(1) if block else ""
assert refresh, "could not extract the NMX refresh block from install.sh"


def run_refresh(body, maj, live):
    """Run one copy of the refresh block against a sandbox where /Library/NullMoth -> live, $EXT -> sandbox ext."""
    ext = live.parent / "Extensions"; ext.mkdir(exist_ok=True)
    # the opt-in is already on: an NMXGPU.kext exists in $EXT
    (ext / "NMXGPU.kext/Contents/MacOS").mkdir(parents=True, exist_ok=True)
    (ext / "NMXGPU.kext/Contents/MacOS/NMXGPU").write_text("stale")
    body = body.replace("/Library/NullMoth", str(live))
    script = f'die() {{ echo "DIE $*"; exit 1; }}\nEXT="{ext}"\nMAJ="{maj}"\nchown() {{ :; }}\nchmod() {{ :; }}\n' + body
    subprocess.run(["bash", "-c", script], capture_output=True, text=True, env={"PATH": "/usr/bin:/bin", "BK": "bk"})
    marker = (ext / "NMXGPU.kext/Contents/MacOS/NMXGPU")
    return marker.read_text() if marker.exists() else ""


with tempfile.TemporaryDirectory() as d:
    live = pathlib.Path(d) / "NullMoth"
    for maj in ("15", "26"):
        (live / f"kexts/{maj}/NMXGPU.kext/Contents/MacOS").mkdir(parents=True)
        (live / f"kexts/{maj}/NMXGPU.kext/Contents/MacOS/NMXGPU").write_text(f"built-for-{maj}")
    got26 = run_refresh(refresh, "26", live)
    arm("on macOS 26 the installer refreshes from the kexts/26 build", got26 == "built-for-26", got26)
    got15 = run_refresh(refresh, "15", live)
    arm("on macOS 15 it refreshes from the kexts/15 build", got15 == "built-for-15", got15)

# negative control: the OLD block hardcoded kexts/15 -> on macOS 26 it wrongly installs the 15 binary
old = refresh.replace("/Library/NullMoth/kexts/$MAJ/", "/Library/NullMoth/kexts/15/")
with tempfile.TemporaryDirectory() as d:
    live = pathlib.Path(d) / "NullMoth"
    for maj in ("15", "26"):
        (live / f"kexts/{maj}/NMXGPU.kext/Contents/MacOS").mkdir(parents=True)
        (live / f"kexts/{maj}/NMXGPU.kext/Contents/MacOS/NMXGPU").write_text(f"built-for-{maj}")
    got = run_refresh(old, "26", live)
    arm("NEGATIVE CONTROL: the old hardcoded kexts/15 block installs the WRONG (15) binary on macOS 26",
        got == "built-for-15", got)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
