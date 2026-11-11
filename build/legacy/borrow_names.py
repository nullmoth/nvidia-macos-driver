#!/usr/bin/env python3
# borrow_names.py <named tables of release A> <named tables of a neighbouring release B>
# Names a slot that A's own vendor drivers left unnamed from B, but only where the two tables line up around it:
# the class exists on both, and aligning A's table with B's by method name puts the unnamed pure slot opposite a
# named slot that is pure on B too. Rewrites A in place and reports what was borrowed and what stays unnamed.
import json, sys, difflib, re
A = json.load(open(sys.argv[1])); Bn = json.load(open(sys.argv[2]))
def key(names, slots, i):
    n = names[i]
    if n: 
        m = re.match(r"(K?)(\d+)", n); return (m.group(1) + n[len(m.group(1)) + len(m.group(2)):][:int(m.group(2))]) if m else n
    return "?pure" if slots[i] == "___cxa_pure_virtual" else "?"
borrowed = 0; left = []
for c, a in A.items():
    b = Bn.get(c)
    if not b: 
        left += [f"{c}[{i}]" for i in a["pure"] if a["names"][i] is None]; continue
    ka = [key(a["names"], a["slots"], i) for i in range(len(a["slots"]))]; kb = [key(b["names"], b["slots"], i) for i in range(len(b["slots"]))]
    # an unnamed pure slot matches any pure slot of the neighbour for the purpose of alignment
    kb2 = ["?pure" if (i in set(b["pure"])) else k for i, k in enumerate(kb)]
    sm = difflib.SequenceMatcher(None, ka, kb2, autojunk=False); mapping = {}
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            for d in range(i2 - i1): mapping[i1 + d] = j1 + d
    for i in a["pure"]:
        if a["names"][i] is not None: continue
        j = mapping.get(i)
        if j is not None and b["names"][j]: a["names"][i] = b["names"][j]; borrowed += 1
        else: left.append(f"{c}[{i}]")
json.dump(A, open(sys.argv[1], "w"), indent=0)
print(f"borrowed {borrowed} names from the neighbouring release; still unnamed {len(left)}: {left[:14]}")
