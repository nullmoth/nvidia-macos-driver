#!/usr/bin/env python3
"""The framebuffer withdraws only the scale-resolutions it offered itself.

For a rotated display the driver offers two scaled sizes in the display's IODisplayAttributes and takes them back when
the display is no longer rotated. The take-back removed any scale-resolutions it found, including a list a display
override supplies to turn HiDPI on for a monitor, so that monitor lost its HiDPI sizes. The driver now remembers
whether the list is its own. The decision is modelled from the line in the source."""
import pathlib, sys

R = pathlib.Path(__file__).resolve().parents[1]
src = (R / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text()
i = src.index("void NVRMNVDAFramebuffer::publishRotatedSizes(bool force)")
body = src[i:src.index("\nbool NVRMNVDAFramebuffer::dtIsCurrent", i)]
fails = 0


def arm(name, ok):
    global fails
    print(("ok   " if ok else "FAIL ") + name)
    fails += not ok


RULE = "if ((!want && (!have || !fRotSizesOurs)) || (want && have && !force)) { attrs->release(); return; }"
arm("the early return leaves a list that is not ours alone", RULE in body)
arm("ownership is recorded where the property is written", body.index("fRotSizesOurs = want;") < body.index('disp->setProperty("IODisplayAttributes", d);'))
arm("control: the old rule (any list is withdrawn) is gone", "if ((!want && !have) || (want && have && !force))" not in body)


def acts(want, have, ours, force):          # True = the routine goes on to rewrite the attributes
    return not ((not want and (not have or not ours)) or (want and have and not force))


def acts_old(want, have, force):
    return not ((not want and not have) or (want and have and not force))


arm("not rotated, an override's list present: left alone", not acts(False, True, False, True))
arm("not rotated, our own list present: withdrawn", acts(False, True, True, True))
arm("rotated, nothing there: offered", acts(True, False, False, True))
arm("rotated, already offered, not forced: nothing to do", not acts(True, True, True, False))
arm("control: the old rule removed the override's list", acts_old(False, True, True))
print("FAILED" if fails else "all ok")
sys.exit(1 if fails else 0)
