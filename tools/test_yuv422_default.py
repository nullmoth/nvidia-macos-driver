#!/usr/bin/env python3
"""4:2:2 video textures (BGRG422 / GBGR422) are created unless they are switched off.

Apple's drivers create both formats for any process, and CoreVideo gives camera frames (2vuy, yuvs) to Metal as these
textures. The plugin refused them unless NVMTL_YUV422=1 was set, so a camera preview read zeros. The refusal is now
the exception: only NVMTL_YUV422=0 refuses. The rule is modelled on the line the plugin uses."""
import pathlib, re, sys

R = pathlib.Path(__file__).resolve().parents[1]
src = (R / "plugin/NVMTLDevice.m").read_text()
fails = 0


def arm(name, ok):
    global fails
    print(("ok   " if ok else "FAIL ") + name)
    fails += not ok


i = src.find('const char *on = getenv("NVMTL_YUV422");')
gate = src[i:i + 900] if i >= 0 else ""
arm("the switch is read in one place", src.count('getenv("NVMTL_YUV422")') == 1 and i >= 0)
arm("the refusal is taken only for a set value that starts with 0", "if (on && on[0] == '0') {" in gate and "return nil; }" in gate)
arm("control: the old rule (refuse unless set to 1) is gone", "on[0] != '1'" not in src)


def refused(env):                       # the rule as written
    return env is not None and env[:1] == "0"


def refused_old(env):
    return env is None or env[:1] != "1"


arm("unset: created", not refused(None))
arm("set to 1: created, as before", not refused("1"))
arm("set to 0: refused", refused("0"))
arm("control: under the old rule an unset switch refused the texture", refused_old(None))
print("FAILED" if fails else "all ok")
sys.exit(1 if fails else 0)
