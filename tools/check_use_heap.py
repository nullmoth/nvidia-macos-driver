#!/usr/bin/env python3
"""check_use_heap.py - every useHeap:/useHeaps: method in the plugin has a body.

Metal lets an app declare a heap with useHeap: instead of naming each resource inside it, so an encoder method with an
empty body `{}` leaves those resources unheld and unresident: a heap freed after encoding, or a member paged out, while
submitted work still reads it is a GPU page fault. This check lists every such method in plugin/NVMTLObjects.m and fails
on any empty body. Residency-set methods are reported but not failed (not implemented yet, same hazard).

    python3 tools/check_use_heap.py            # exit 1 on any empty useHeap body
"""
import re, sys, os
here = os.path.dirname(os.path.abspath(__file__))
src = open(os.path.join(here, "..", "plugin", "NVMTLObjects.m")).read().split("\n")
impl = None; empty = []; bodies = 0; residency_empty = 0
for n, line in enumerate(src, 1):
    if line.startswith("@implementation"): impl = line.split()[1]
    m = re.match(r"- \(void\)(useHeaps?:[^{]*)\{(.*)$", line)
    if m:
        body = m.group(2).strip()
        if body in ("}", "}  ", "(void)hs; (void)n; }") or re.fullmatch(r"\(void\)\w+;\s*\(void\)\w+;\s*\}", body):
            empty.append(f"{impl} line {n}: {m.group(1).strip()}")
        else: bodies += 1
    if re.match(r"- \(void\)(use|add|remove)ResidencySets?:[^{]*\{\s*\}", line): residency_empty += 1
print(f"useHeap/useHeaps methods with a body: {bodies}; empty: {len(empty)}; residency-set methods still empty: {residency_empty}")
for e in empty: print("EMPTY", e)
if bodies == 0: print("FAIL: no useHeap method found at all - the check cannot run"); sys.exit(2)
sys.exit(1 if empty else 0)
