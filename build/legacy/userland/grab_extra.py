#!/usr/bin/env python3
# grab_extra.py <release> <path>...: copy further paths out of a release's recovery image into OUT/sys/<release>/.
# Same environment and read-only attach as grab.py.
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import importlib.util
spec = importlib.util.spec_from_file_location("grab", os.path.join(os.path.dirname(os.path.abspath(__file__)), "grab.py"))
src = open(spec.origin).read().split("\nfor n in sys.argv[1:]:")[0]
g = {}; exec(compile(src, spec.origin, "exec"), g)
import glob
name = sys.argv[1]; d = f"{g['BASE']}/recovery/{name}"; o = f"{g['OUT']}/sys/{name}"
dmgs = sorted(glob.glob(d + "/*.dmg"), key=os.path.getsize, reverse=True)
mps, err = g["attach"](dmgs[0]); extra = []
if not mps: print("attach failed", err); sys.exit(1)
try:
    R, extra = g["find_root"](mps)
    for p in sys.argv[2:]:
        ok = os.path.exists(R + p) and g["cp"](R + p, o + p)
        print(name, p, "copied" if ok else "ABSENT on the image")
finally:
    for mp in extra + mps: g["detach"](mp)
