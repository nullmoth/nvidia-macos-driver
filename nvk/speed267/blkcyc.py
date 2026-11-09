#!/usr/bin/env python3
# blkcyc.py <sass>: per block: instruction count, sum of delay= (issue cycles), loads, and the block's successors
import re, sys
cur = None; out = []
for l in open(sys.argv[1]):
    m = re.match(r'(block\S*) (\d+) (L\d+) \[(.*?)\] -> \{', l)
    if m:
        cur = [int(m.group(2)), m.group(1), 0, 0, 0, '']; out.append(cur); continue
    m = re.match(r'\} -> \[(.*)\]', l)
    if m and cur: cur[5] = m.group(1); continue
    if cur is None or not l.strip(): continue
    d = re.search(r'delay=(\d+)', l)
    cur[2] += 1; cur[3] += int(d.group(1)) if d else 1
    if re.search(r'= ld\.global', l): cur[4] += 1
for b in out:
    if b[2]: print("b%-3d %-8s instrs %4d issue %5d loads %2d -> [%s]" % tuple(b))
