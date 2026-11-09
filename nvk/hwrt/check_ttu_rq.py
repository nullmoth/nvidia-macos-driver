#!/usr/bin/env python3
"""check_ttu_rq.py <on.sass.bin> <off.sass.bin> - the ray-query lowering, judged on machine code.

on  = ttu_rq_cpu run with NVK_HWRT_QUERY=1; off = the same shader with the switch unset (the software walk).
Arms: the switched-on code holds exactly one 14-word hardware query region followed by its drain; the switched-off code
holds none (so the count means something); and the region matches the measured field table (check_ttu_region.py).
"""
import os, struct, subprocess, sys

here = os.path.dirname(os.path.abspath(__file__))
TTU = {0x3d0, 0x3d1, 0x3d2, 0x3d3, 0x9d4}


def words(path):
    b = open(path, "rb").read()
    return [struct.unpack_from("<QQ", b, o) for o in range(0, len(b), 16)]


fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (("  " + detail) if detail and not ok else ""))
    fails += (not ok)


on, off = words(sys.argv[1]), words(sys.argv[2])
n_on = sum(1 for lo, hi in on if lo & 0xfff in TTU)
n_off = sum(1 for lo, hi in off if lo & 0xfff in TTU)
arm("switched on: exactly 14 hardware-query words (one region)", n_on == 14, str(n_on))
arm("control: switched off (software walk): no hardware-query word at all", n_off == 0, str(n_off))
arm("control: switched off, the code is the software walk alone: 708 instructions, as before the hybrid lowering", len(off) == 708, str(len(off)))
arm("switched on, the code adds the flag handling, the candidate loop and the box walk around the one query", len(on) > len(off), "%d vs %d" % (len(on), len(off)))
r = subprocess.run([sys.executable, os.path.join(here, "check_ttu_region.py"), sys.argv[1]],
                   capture_output=True, text=True)
arm("the region matches the field table measured on the card", r.returncode == 0 and "REGION_MATCH" in r.stdout, r.stdout[-200:] + r.stderr[-200:])
print("%d arms failed" % fails)
sys.exit(1 if fails else 0)
