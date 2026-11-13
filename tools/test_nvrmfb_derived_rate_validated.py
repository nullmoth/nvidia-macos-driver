#!/usr/bin/env python3
"""Derived refresh rates (half / quarter of a 240 Hz+ panel mode) are offered only when NVKMS accepts the timing (issue 549).

A derived timing is not one of the display's own modes. It used to be listed without asking NVKMS; on panels that take
only their EDID timings the mode set's test pass refused it, and the rate showed as selected while the panel stayed put.
Reads the kernel source.
   python3 tools/test_nvrmfb_derived_rate_validated.py [tree-root]
The negative control runs the same checks on the pre-fix source (the commit before the last one touching the file, or
HEAD while the change is uncommitted) and the validation arm must fail there."""
import subprocess
import sys
from pathlib import Path

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
REL = "kexts/NVRMFB/fb/nvrm-fb.cpp"
fails = []


def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        fails.append(name)


def derived_loop(src):
    a = src.index("// Derived refresh rates:")
    b = src.index('setProperty("NVRMRefreshModesAdded"', a)
    return src[a:b]


def validated(src):
    loop = derived_loop(src)
    add = loop.find("fDT[n++] = b; added++;")
    val = loop.find("fKms->validateDisplayMode(fDev, fDisplay, &dm)")
    conv = loop.find("dtToKapi(&b, &dm);")
    skip = loop.find("continue;", val) if val >= 0 else -1
    return 0 <= conv < val < skip < add


src = (ROOT / REL).read_text(encoding="utf-8")
loop = derived_loop(src)
arm("the derived-rate loop converts the derived timing exactly as setDisplayMode will (dtToKapi) and asks NVKMS before adding it",
    validated(src))
arm("a refused derived rate is logged with the rate it would have offered", "NVKMS refused the timing -- not offered" in loop)
arm("a missing validateDisplayMode entry point offers nothing derived (fails closed)", "!fKms->validateDisplayMode ||" in loop)
arm("native NVKMS refresh modes are not put through the new check (only the derived loop changed)",
    src.count("fKms->validateDisplayMode(fDev, fDisplay, &dm)") == 1)
sw = src[src.index("IOReturn NVRMNVDAFramebuffer::switchMode("):]
sw = sw[:sw.index("struct NvKmsKapiSurface *old = fSurf;")]
arm("a refused test pass still returns an error to IOFramebuffer (kIOReturnUnsupportedMode) and never commits",
    "if (!ok) break;" in sw and "return kIOReturnUnsupportedMode;" in sw)


def old_source():
    # the commit that introduced the check, found by its log text, so later commits to the file do not move the base
    intro = subprocess.run(["git", "-C", str(ROOT), "log", "--format=%H", "-S", "NVKMS refused the timing -- not offered",
                            "--", REL], capture_output=True, text=True, check=True).stdout.split()
    rev = intro[-1] + "~1" if intro else "HEAD"
    return subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{REL}"], capture_output=True, text=True,
                          check=True).stdout


try:
    old = old_source()
    if "NVKMS refused the timing -- not offered" in old:
        arm("negative control: the previous revision already had the check (the history is not the pre-fix one)", False)
    else:
        arm("negative control: the validation check fails on the pre-fix source", not validated(old))
except (subprocess.CalledProcessError, ValueError) as e:
    arm(f"negative control could not run ({e.__class__.__name__})", False)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
