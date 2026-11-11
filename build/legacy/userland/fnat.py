#!/usr/bin/env python3
# fnat.py <binary> <listing> <line number>... : print the notes of the function that contains each listing line.
# fnat.py <binary> --data <symbol substring>... : initial bytes (8) of every data symbol whose name contains it.
import sys, re, subprocess, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import machofn
img = machofn.Img(sys.argv[1])
if sys.argv[2] == "--data":
    out = subprocess.run(["nm", "-arch", "x86_64", sys.argv[1]], capture_output=True, text=True).stdout
    for l in out.splitlines():
        p = l.split(None, 2)
        if len(p) == 3 and any(s in p[2] for s in sys.argv[3:]):
            d = img.rd(int(p[0], 16), 8)
            print(p[2], p[1], "file bytes:", d.hex() if d else "zero-fill (no file bytes: starts as 0)")
else:
    L = open(sys.argv[2], errors="replace").read().split("\n")
    for n in sys.argv[3:]:
        i = int(n) - 1; s = i
        while s > 0 and not (L[s].endswith(":") and not L[s][:1].isdigit()): s -= 1
        e = i
        while e + 1 < len(L) and not (L[e + 1].endswith(":") and not L[e + 1][:1].isdigit()): e += 1
        print("==", L[s], "(listing lines %d-%d)" % (s + 1, e + 1))
        for x in machofn.notes(img, [l for l in L[s + 1:e + 1] if l[:1].isdigit()]): print("   ", x[:170])
