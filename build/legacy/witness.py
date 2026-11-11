#!/usr/bin/env python3
# witness.py <reference kext> <other kext>...: for each (class, offset) our accelerator kext reads or writes inside
# one of Apple's objects, count the methods of that class that use that displacement on the reference release and
# how many of the same methods still use it on each other release. A field that moved loses its witnesses.
import subprocess, sys, re, collections
PAIRS = [("IOAccelMemoryMap", 0x80), ("IOAccelDisplayMachine", 0x108), ("IOAccelDisplayMachine", 0x10c), ("IOAccelLegacyDisplayMachine", 0x108),
         ("IOGraphicsAccelerator2", 0xc78), ("IOGraphicsAccelerator2", 0xcb0), ("IOAccelDisplayPipe", 0x98), ("IOAccelLegacyDisplayPipe", 0x98),
         ("IOAccelSurface", 0x98), ("IOAccelSysMemory", 0xc8), ("IOAccelSysMemory", 0x40), ("IOAccelMemory", 0x40), ("IOAccelVidMemory", 0x40),
         ("IOAccelResource2", 0x88), ("IOAccelResource2", 0xb0), ("IOAccelResource2", 0xb2), ("IOAccelResource2", 0xb8), ("IOAccelResource2", 0xc0),
         ("IOAccelResource2", 0xc), ("IOAccelResource2", 0x30), ("IOAccelResource2", 0xe0), ("IOAccelResource2", 0x14)]
def disp(path):
    out = subprocess.run(["otool", "-arch", "x86_64", "-tV", path], capture_output=True, text=True, errors="replace").stdout
    cur = None; d = {}
    for l in out.splitlines():
        if l.endswith(":") and not l.startswith("\t") and not l[0].isdigit(): cur = l[:-1]; d[cur] = collections.Counter(); continue
        if cur is None: continue
        for m in re.finditer(r"(0x[0-9a-f]+)\(%r(?!ip|sp|bp)[a-z0-9]+\)", l): d[cur][int(m.group(1), 16)] += 1
    return d
def cls(n):
    m = re.match(r"__ZNK?(\d+)", n); return n[m.end():m.end() + int(m.group(1))] if m else None
ref = disp(sys.argv[1]); others = [(p.split("/kdk/")[1].split("/")[0] if "/kdk/" in p else p, disp(p)) for p in sys.argv[2:]]
print(f"{'class':28} {'offset':>7} {'witnesses on reference':>23}   " + "   ".join(f"{n:>22}" for n, _ in others))
for c, off in PAIRS:
    W = [n for n, v in ref.items() if cls(n) == c and off in v]
    row = []
    for n, d in others:
        shared = [m for m in W if m in d]; keep = sum(1 for m in shared if off in d[m]); same = sum(1 for m in shared if d[m] == ref[m])
        row.append(f"{keep:3}/{len(shared):<3} keep, {same:3} same")
    print(f"{c:28} {hex(off):>7} {len(W):>23}   " + "   ".join(f"{r:>22}" for r in row))
