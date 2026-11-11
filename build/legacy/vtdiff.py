#!/usr/bin/env python3
# vtdiff.py <reference json> <other json>: aligned difference of each class's virtual table, plus sizes.
import json, difflib, re, sys
r = json.load(open(sys.argv[1])); d = json.load(open(sys.argv[2]))
def short(n): return re.sub(r"^__ZNK?\d+", "", n)[:64]
classes = 0; runs = 0
for c in sorted(r):
    if c not in d: continue
    a, b = d[c]["slots"], r[c]["slots"]; sa, sb = d[c].get("size"), r[c].get("size")
    if a == b and sa == sb: continue
    ops = [o for o in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes() if o[0] != "equal"]
    classes += 1; runs += len(ops)
    print(f"== {c}: other {len(a)} slots size {sa} | reference {len(b)} slots size {sb} | {len(ops)} changed runs")
    for tag, i1, i2, j1, j2 in ops[:int(sys.argv[3]) if len(sys.argv) > 3 else 5]:
        print(f"   {tag:7} other[{i1}:{i2}] {[short(x) for x in a[i1:i2]][:3]}  reference[{j1}:{j2}] {[short(x) for x in b[j1:j2]][:3]}")
print(f"{classes} classes differ, {runs} changed runs in all")
