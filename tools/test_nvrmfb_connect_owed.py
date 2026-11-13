#!/usr/bin/env python3
"""A connect interrupt that comes due before the framework registers its handler is fired on registration (issue 581).

A display plugged in after boot gets a new framebuffer; enableController schedules its connect interrupt 1.5 s after
the mode set. When IOFramebuffer::open had not registered the connect handler by then, connectNotifyFire logged
"nothing registered yet" and dropped it, and the display stayed dark until a restart. Reads the kernel source.
   python3 tools/test_nvrmfb_connect_owed.py [tree-root]
The negative control runs the owed-interrupt arms on the pre-fix source and they must fail there."""
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


def fn(src, head):
    i = src.index(head)
    return src[i:src.index("\n}\n", i)]


def owed_arms(src):
    fire = fn(src, "void NVRMNVDAFramebuffer::connectNotifyFire(")
    reg = fn(src, "IOReturn NVRMNVDAFramebuffer::registerForInterruptType(")
    no_handler = fire[fire.index("if (!me->fConnect.proc)"):]
    no_handler = no_handler[:no_handler.index("return;")]
    conn = reg[reg.index("if (type == kIOFBConnectInterruptType)"):]
    conn = conn[:conn.index("return kIOReturnSuccess;")]
    return {
        "with no handler registered, the notification is owed instead of dropped":
            "OSCompareAndSwap(0, 1, &me->fConnectOwed)" in no_handler,
        "registering the connect handler pays an owed notification":
            "OSAddAtomic(0, &fConnectOwed)" in conn and "notifyConnectionChanged(" in conn,
        "it is paid from the thread call, never by calling the handler inside registerForInterruptType":
            "fConnect.proc(" not in conn and "proc(target" not in conn,
        "a registration with a null handler pays nothing": "if (proc && OSAddAtomic(0, &fConnectOwed))" in conn,
        "a fired notification clears the debt, so it is not fired twice":
            "OSCompareAndSwap(1, 0, &me->fConnectOwed)" in fire[fire.index("return;", fire.index("if (!me->fConnect.proc)")):],
    }


src = (ROOT / REL).read_text(encoding="utf-8")
for name, ok in owed_arms(src).items():
    arm(name, ok)
arm("the hot-plugged framebuffer still schedules its interrupt only after a successful mode set (unchanged)",
    "if (ok && fHotplugBorn && !fConnectFired) {\n        notifyConnectionChanged(1500);" in src)
arm("the debt starts at zero on every framebuffer", "volatile SInt32 fConnectOwed = 0;" in src)


def old_source():
    dirty = subprocess.run(["git", "-C", str(ROOT), "diff", "--quiet", "HEAD", "--", REL]).returncode != 0
    if dirty:
        rev = "HEAD"
    else:
        log = subprocess.run(["git", "-C", str(ROOT), "log", "--format=%H", "-S", "fConnectOwed", "--", REL],
                             capture_output=True, text=True, check=True).stdout.split()
        rev = log[-1] + "~1"
    return subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{REL}"], capture_output=True, text=True,
                          check=True).stdout


try:
    old = owed_arms(old_source())
    arm("negative control: the owed-interrupt arms fail on the pre-fix source",
        not any(v for k, v in old.items() if "owed" in k or "pays" in k or "debt" in k))
except (subprocess.CalledProcessError, ValueError, IndexError) as e:
    arm(f"negative control could not run ({e.__class__.__name__})", False)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
