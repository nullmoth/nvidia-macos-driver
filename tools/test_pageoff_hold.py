#!/usr/bin/env python3
"""The opt-in unlocked page-off wait lets go of the accelerator lock while it waits for the GPU. While the lock is free
another thread can drop the last reference to the resource or to the accelerator, and the function then read both again:
freed memory. Both are now retained before the lock is released and let go only when the function returns. The control
is the source before the change (commit 406d9824), which unlocks with no reference held."""
import pathlib, subprocess, sys

R = pathlib.Path(__file__).resolve().parents[1]
SRC = "kexts/NVRM/accel/nvrm-accel.cpp"
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({str(detail).strip()[-200:]})" if detail and not ok else ""))
    fails += not ok


def held(src):
    """True when pageoff retains the resource and the accelerator before the unlock and releases them at scope exit."""
    a = src.index("void NVResource::pageoff(")
    body = src[a:src.index("\n}\n", a)]
    if "nvFamUnlock(a);" not in body:
        return None
    u = body.index("nvFamUnlock(a);")
    return ("retain(); holdRes.o = this;" in body[:u] and "a->retain(); holdAccel.o = a;" in body[:u]
            and "~NVHold() { if (o) o->release(); }" in body[:u])


new = held((R / SRC).read_text())
arm("the unlocked wait is still in pageoff (the check looked at the right function)", new is not None)
arm("the resource and the accelerator are held before the lock is released, until the function returns", new is True)
old = subprocess.run(["git", "-C", str(R), "show", f"406d9824:{SRC}"], capture_output=True, text=True)
if old.returncode == 0:
    arm("control: the earlier source released the lock with nothing held", held(old.stdout) is False)
else:
    arm("control: the earlier source is readable", False, old.stderr)
src = (R / SRC).read_text()
arm("the switch stays off by default and only root can turn it on", "&gPoUnlocked, 0," in src and "static int gPoUnlocked" in src or "gPoUnlocked = 0" in src)
print("FAILED" if fails else "PASS"); sys.exit(1 if fails else 0)
