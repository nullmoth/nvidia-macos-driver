#!/usr/bin/env python3
"""Compares a curves run (curves.m output) with reference_m1.txt. Exit 0 when every ray hits or misses as on the Apple
GPU and distances agree within 1e-3; the curve parameter is compared only where both picked the same segment, since
between segments hit at the same distance the choice is the traversal's.
usage: compare.py <run.txt> [reference.txt]"""
import os, sys

def rows(path):
    out = {}
    for line in open(path):
        if ':' not in line:
            continue
        name, vals = line.split(':', 1)
        out[name.strip()] = [tuple(v.split('/')) for v in vals.split()]
    return out

ref_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(os.path.abspath(__file__)), 'reference_m1.txt')
ref, run = rows(ref_path), rows(sys.argv[1])
bad, dt, du, hits = 0, 0.0, 0.0, 0
for name, want in ref.items():
    got = run.get(name)
    if got is None or len(got) != len(want):
        print(f'MISSING {name}'); bad += 1; continue
    for k, (w, g) in enumerate(zip(want, got)):
        wh, gh = float(w[0]) >= 0, float(g[0]) >= 0
        if wh != gh:
            print(f'{name} ray {k}: Apple {"hit" if wh else "miss"} {"/".join(w)}, here {"/".join(g)}'); bad += 1; continue
        if not wh:
            continue
        hits += 1
        dt = max(dt, abs(float(w[0]) - float(g[0])))
        if w[2] == g[2]:
            du = max(du, abs(float(w[1]) - float(g[1])))
print(f'{len(ref)} cases, {hits} hits, {bad} hit/miss mismatches, max |dt| {dt:.4f}, max |du| (same segment) {du:.4f}')
sys.exit(1 if bad or dt > 1e-3 else 0)
