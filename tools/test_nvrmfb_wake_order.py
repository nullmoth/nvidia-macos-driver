#!/usr/bin/env python3
"""A system wake must hand paging back before it replays the owed DidWake(false) notifications.

WindowServer sleeps on fServerConnect until pagingState is set, and nothing retries the wake once
familyDidWakeGated returns, so the replay (one synchronous round per dark wake since the last full wake)
has to come after paging is on and the gate has been woken. The negative control runs the same check on
the pre-fix source, where both came last.
"""
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


def wake_body(src):
    i = src.index("IOReturn NVRMNVDAFramebuffer::familyDidWakeGated(")
    return src[i:src.index("\n}\n", i)]


def order_ok(src):
    body = wake_body(src)
    paging = body.index("pagingState = true;")
    wakeup = body.index("commandWakeup(&fServerConnect")
    replay = body.index("for (SInt32 i = 1; i < owed; ++i)")
    return paging < replay and wakeup < replay


def old_source(src):
    dirty = subprocess.run(["git", "-C", str(ROOT), "diff", "--quiet", "HEAD", "--", REL]).returncode != 0
    if dirty:
        rev = "HEAD"
    else:
        # The commit that put "pagingState = true;" where it is now is the one that moved it; its parent is the
        # source the check has to fail on.
        line = src[:src.index("pagingState = true;")].count("\n") + 1
        blame = subprocess.run(["git", "-C", str(ROOT), "blame", "-L", f"{line},{line}", "--porcelain", "--", REL],
                               capture_output=True, text=True, check=True).stdout
        rev = blame.split()[0] + "~1"
    return subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{REL}"], capture_output=True, text=True,
                          check=True).stdout


source = (ROOT / REL).read_text()
arm("paging is on and the gate is woken before the owed DidWake(false) replay", order_ok(source))
try:
    arm("negative control: the pre-fix order fails the same check", not order_ok(old_source(source)))
except (subprocess.CalledProcessError, ValueError, IndexError) as e:
    arm(f"negative control could not run ({e.__class__.__name__})", False)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
