#!/usr/bin/env python3
"""check_accel_resolution.py: the accelerator global is read in a few named places only.

NVAccel::gAccel is the primary card's accelerator. Code that belongs to an object the accelerator made must use
NVAccel::resolve(owner); display, scanout and flip code uses NVAccel::forDisplay(). Anything else reading the global would
silently talk to the primary on a second card. This check fails if a raw read appears anywhere but the allowed places, and
that the objects the accelerator makes are given their owner.

Arms print PASS or FAIL; exit status 0 only when every arm passes. Run: python3 tools/check_accel_resolution.py"""
import re, sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
src = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "kexts/NVRM/accel/nvrm-accel.cpp").read_text()
stubs = Path(sys.argv[2] if len(sys.argv) > 2 else ROOT / "kexts/NVRM/accel/nvaccel_stubs.h").read_text()
fails = 0
def arm(what, ok, detail=""):
    global fails
    print(("PASS  " if ok else "FAIL  ") + what + (("  " + detail) if detail and not ok else ""))
    fails += 0 if ok else 1

lines = src.split("\n")
raw = [(i + 1, l.strip()) for i, l in enumerate(lines) if re.search(r"\bgAccel\b", l) and not l.strip().startswith("//")]
allowed = [r"static NVAccel \*gAccel;", r"return nvrm_resolve\(owner, gAccel\);", r"static NVAccel \*forDisplay\(\) \{ return gAccel; \}",
           r"NVAccel \*NVAccel::gAccel = nullptr;", r"gAccel = gAccelTab\.primary\(\)",
           r"me != gAccel",                                  # the framebuffer hook acts for the primary
           r"gAccel->|!gAccel|gAccel\)"]                      # debug.nvaccelfb sysctl and enableFromHook: they drive the primary
bad = [(n, l) for n, l in raw if not any(re.search(a, l) for a in allowed)]
arm("every read of the accelerator global is in a named place (declaration, resolve, forDisplay, table update, debug sysctls)", not bad, str(bad[:3]))
arm("no code outside the class reads NVAccel::gAccel", not [l for l in lines if re.search(r"NVAccel::gAccel\b", l) and "NVAccel::gAccel = nullptr" not in l])
for fn in ("newVidMemory", "newResource", "newMemoryMap", "newDisplayPipe"):
    i = src.index("OSObject *%s()" % fn); chunk = src[i:i + 400]
    arm("%s gives the object its accelerator" % fn, "fAccel = this" in chunk)
for cls in ("NVResource", "NVVidMemory", "NVDisplayPipe"):
    arm("%s carries fAccel" % cls, re.search(r"class %s .*?fAccel = nullptr" % cls, stubs, re.S) is not None)
arm("NVMemoryMap carries fAccel", re.search(r"class NVMemoryMap .*?NVAccel +\*fAccel", src, re.S) is not None)
arm("RM handles are per accelerator (no file-level handle cache)", "static struct NVRMRmHandles gRmHandles" not in src and "fRmHandlesValid" in src)
arm("the accelerator lock owner is per accelerator", "static nvaccel_lock_owner gLockOwner" not in src and "fLockOwner" in src)
arm("every nvAccelRmHandles call names its accelerator", not re.findall(r"nvAccelRmHandles\(&", src))
arm("every nvAccelNVRM call names its accelerator", not re.findall(r"nvAccelNVRM\(\)", src))
print("FAILED %d" % fails if fails else "PASS all arms")
sys.exit(1 if fails else 0)
