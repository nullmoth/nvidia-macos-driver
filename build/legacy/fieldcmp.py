#!/usr/bin/env python3
# fieldcmp.py <reference kext> <other kext>: for every method the two binaries share, compare the sequence of
# memory displacements its instructions use. Equal sequences across all methods of a class mean its fields sit
# at the same offsets on both releases; a class whose methods differ has moved fields (or changed code).
import subprocess, sys, re, collections
def disp(path):
    out = subprocess.run(["otool", "-arch", "x86_64", "-tV", path], capture_output=True, text=True, errors="replace").stdout
    cur = None; d = {}
    for l in out.splitlines():
        if l.endswith(":") and not l.startswith("\t") and not l[0].isdigit(): cur = l[:-1]; d[cur] = []; continue
        if cur is None: continue
        for m in re.finditer(r"(-?0x[0-9a-f]+|\b\d+)?\(%r(?!ip|sp|bp)[a-z0-9]+(?:,%r[a-z0-9]+(?:,\d)?)?\)", l):
            d[cur].append(m.group(1) or "0")
    return d
a, b = disp(sys.argv[1]), disp(sys.argv[2])
cls = lambda n: (re.match(r"__ZNK?(\d+)", n) and n[re.match(r"__ZNK?(\d+)", n).end():][:int(re.match(r"__ZNK?(\d+)", n).group(1))]) or "?"
per = collections.defaultdict(lambda: [0, 0, 0, []])   # shared, identical, only-in-ref, differing names
for n, v in a.items():
    c = cls(n)
    if n not in b: per[c][2] += 1; continue
    per[c][0] += 1
    if v == b[n]: per[c][1] += 1
    else: per[c][3].append(n)
WANT = sys.argv[3:] or sorted(per)
tot = same = 0
for c in WANT:
    s, i, o, dn = per[c]; tot += s; same += i
    print(f"{c:34} methods shared {s:4}  identical field use {i:4}  differ {s - i:3}  only in reference {o:3}  {' '.join(x[:48] for x in dn[:2])}")
print(f"TOTAL shared {tot}, identical {same}, differ {tot - same}")
