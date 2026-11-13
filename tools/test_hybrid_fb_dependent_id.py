#!/usr/bin/env python3
"""Hybrid panel framebuffers (NMIntelFB, NMGopFB) set IOFBDependentID before IOFramebuffer::start (issue 555).

IOFramebuffer::start attaches a framebuffer to its shared controller only when the dependent ID already exists. Set
afterwards, the controller stays unset until open(), and the power change that registerPowerDriver() queues at the end
of start() takes the controller's lock through a null pointer. NVRMFB already sets it first.

   python3 tools/test_hybrid_fb_dependent_id.py [tree-root]
The negative control runs the same check on the pre-fix sources (git HEAD~ or a supplied old tree) and must fail."""
import subprocess
import sys
from pathlib import Path

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
FILES = {"NMIntelFB": "kexts/NMIntelFB/intelfb.cpp", "NMGopFB": "kexts/NMGopFB/gopfb.cpp"}
fails = []


def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        fails.append(name)


def start_bodies(src, cls):
    """Each `bool <cls>::start(` body, split at #ifdef branches so both start paths are checked on their own."""
    head = f"bool {cls}::start(IOService *provider)"
    i = src.index(head)
    end = src.index("\n}\n", i)
    body = src[i:end]
    if "#ifdef" in body and "#endif" in body:
        # NMIntelFB: the virtual panel path returns inside #ifdef ... #endif; the real panel path follows it
        a = body.index("#ifdef")
        c = body.index("#endif", a)
        return [body[a:c], body[c:]]
    return [body]


def ordered(body):
    """True when every IOFramebuffer::start call comes after both dependent properties are set."""
    if "IOFramebuffer::start(provider)" not in body:
        return None
    s = body.index("IOFramebuffer::start(provider)")
    d = body.find('setProperty("IOFBDependentID"')
    x = body.find('setProperty("IOFBDependentIndex"')
    return 0 <= d < s and 0 <= x < s


def check(read):
    results = {}
    for cls, rel in FILES.items():
        src = read(rel)
        for n, body in enumerate(start_bodies(src, cls)):
            r = ordered(body)
            if r is not None:
                results[f"{cls} start path {n}"] = r
    return results


now = check(lambda rel: (ROOT / rel).read_text(encoding="utf-8"))
arm("three start paths were found (NMIntelFB virtual, NMIntelFB panel, NMGopFB)", len(now) == 3)
for name, ok in now.items():
    arm(f"{name}: the dependent ID and index are set before IOFramebuffer::start", ok)

ifb = (ROOT / FILES["NMIntelFB"]).read_text(encoding="utf-8")
panel = start_bodies(ifb, "NMIntelFB")[-1]
arm("NMIntelFB: a surface that will not map stops start before IOFramebuffer::start (no controller-less framebuffer)",
    panel.find("if (!fSurfMap) {") != -1 and panel.find("if (!fSurfMap) {") < panel.index("IOFramebuffer::start(provider)"))
# Every failure after the capture, the Type-C monitor and the loader target exist goes through one unwind.
i = ifb.index("void NMIntelFB::startUnwind()"); unwind = ifb[i:ifb.index("void NMIntelFB::stop(", i)]
late = panel[panel.index("tcStart();"):]
arm("NMIntelFB: a failed IOFramebuffer::start and an unmapped surface both unwind before returning",
    "if (!IOFramebuffer::start(provider)) { startUnwind(); return false; }" in late
    and "startUnwind(); return false;" in late[late.index("if (!fSurfMap) {"):late.index("setProperty(\"IOFBDependentID\"")])
arm("NMIntelFB: no failure return after tcStart skips the unwind",
    late.count("return false;") == late.count("startUnwind(); return false;"))
for what, text in [("the loader target is cleared under its lock", "if (gDmcTarget == this) gDmcTarget = nullptr;"),
                   ("the firmware blob is freed", "IOFree(fDmcBlob, fDmcLen)"),
                   ("the Type-C timer is cancelled and freed", "thread_call_free(fTcTimer)"),
                   ("the resume call is cancelled and freed", "thread_call_free(fResumeCall)"),
                   ("external heads are stopped", "nmhd_stop(&fHeads, &fHeadOps)"),
                   ("the surface and register mappings are released", "OSSafeReleaseNULL(fSurfMap); OSSafeReleaseNULL(fMMIO); fRegs = nullptr;")]:
    arm("NMIntelFB unwind: " + what, text in unwind)
arm("control: the unwind check notices a return that skips it",
    (late + " return false;").count("return false;") != (late + " return false;").count("startUnwind(); return false;"))
nvfb = (ROOT / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text(encoding="utf-8")
arm("NVRMFB still sets its dependent ID before super::start (the reference order)",
    nvfb.index('setProperty("IOFBDependentID"') < nvfb.index("if (!super::start(provider)) {"))

# Negative control: the 2.9 order (properties after start) must be caught by the same check.
old_tree = Path(sys.argv[2]) if len(sys.argv) > 2 else None


def read_old(rel):
    if old_tree:
        return (old_tree / rel).read_text(encoding="utf-8")
    # the commit that moved the properties first, found by its comment text, so later commits do not move the base
    intro = subprocess.run(["git", "-C", str(ROOT), "log", "--format=%H", "-S", "Set before IOFramebuffer::start" if "gopfb" in rel
                            else "Before IOFramebuffer::start, so start() attaches", "--", rel],
                           capture_output=True, text=True, check=True).stdout.split()
    rev = intro[-1] + "~1" if intro else "HEAD"
    return subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{rel}"], capture_output=True, text=True,
                          check=True).stdout


try:
    old = check(read_old)
    arm("negative control: the check fails on the old order (pre-fix sources)", len(old) == 3 and not any(old.values()))
except (subprocess.CalledProcessError, ValueError, FileNotFoundError) as e:
    arm(f"negative control could not run ({e.__class__.__name__}): pass the pre-fix tree as the second argument", False)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
