#!/usr/bin/env python3
"""Resolve GPU fault addresses against the fault file the plugin writes at device loss ($TMPDIR/nvmtl-fault-<pid>.txt).

usage: fault_resolve.py <nvmtl-fault-PID.txt> <address> [<address> ...]

Each address is reported as the live range that holds it (kind, range, size, the submit it was made at), else the
recorded death that held it, else "never mapped by this process's buffers or pages (an image, NVK's own memory, or a computed pointer)" - an image, NVK's own memory, or a pointer the kernel
computed. The dispatch roots the file holds (every binding-table buffer address of the dying command buffer's last
dispatches, already resolved by the plugin) are printed after."""
import sys

def main():
    if len(sys.argv) < 3: print(__doc__); return 2
    live, dead, pages, roots, was = [], [], [], [], []
    for ln in open(sys.argv[1]):
        w = ln.split()
        if not w: continue
        if w[0] == "live":            # live <kind words...> 0xlo 0xhi bytes seq S
            i = next(k for k, x in enumerate(w) if x.startswith("0x")); live.append((int(w[i], 16), int(w[i + 1], 16), " ".join(w[1:i]), int(w[-1])))
        elif w[0] == "was":           # was <kind words...> 0xlo 0xhi bytes seq S died D
            i = next(k for k, x in enumerate(w) if x.startswith("0x")); was.append((int(w[i], 16), int(w[i + 1], 16), " ".join(w[1:i]), int(w[i + 4]), int(w[i + 6])))
        elif w[0] == "dead":          # dead buffer 0xlo 0xhi bytes seq S [flags]
            dead.append((int(w[2], 16), int(w[3], 16), int(w[6]), " ".join(w[7:])))
        elif w[0] == "page":          # page own|cooling|cached 0xlo 0xhi bytes seq S
            pages.append((int(w[2], 16), int(w[3], 16), w[1], int(w[-1])))
        elif w[0] == "root": roots.append(ln.rstrip())
    print(f"{len(live)} live ranges, {len(was)} ranges that once lived, {len(dead)} recorded deaths, {len(pages)} pages, {len(roots)} dispatch roots")
    rc = 0
    for a in sys.argv[2:]:
        addr = int(a, 16)
        hit = [(lo, hi, k, s) for lo, hi, k, s in pages if lo <= addr < hi]
        if hit: lo, hi, k, s = hit[0]; print(f"0x{addr:x} lies in ADDRESS PAGE ({k}) 0x{lo:x}..0x{hi:x} last referenced by submit {s}"); continue
        hit = [(lo, hi, k, s) for lo, hi, k, s in live if lo <= addr < hi]
        if hit: lo, hi, k, s = hit[0]; print(f"0x{addr:x} lies in LIVE {k} 0x{lo:x}..0x{hi:x} ({hi - lo} bytes), alive since submit {s}"); continue
        hit = [(lo, hi, s, f) for lo, hi, s, f in dead if lo <= addr < hi]
        if hit: lo, hi, s, f = hit[0]; print(f"0x{addr:x} lies in DEAD buffer 0x{lo:x}..0x{hi:x} ({hi - lo} bytes{', ' + f if f else ''}) freed after submit {s}"); continue
        hit = [(lo, hi, k, s, d) for lo, hi, k, s, d in was if lo <= addr < hi]
        if hit: lo, hi, k, s, d = hit[-1]; print(f"0x{addr:x} WAS {k} 0x{lo:x}..0x{hi:x} ({hi - lo} bytes), created at submit {s}, died at submit {d} (newest of {len(hit)} that held it)"); continue
        near = sorted(live, key=lambda e: min(abs(e[0] - addr), abs(e[1] - addr)))[:2]
        print(f"0x{addr:x} is NEVER MAPPED by this plugin's buffers or pages (an image, NVK's own memory, or a computed pointer); nearest live: "
              + "; ".join(f"{k} 0x{lo:x}..0x{hi:x}" for lo, hi, k, s in near)); rc = 1
    for r in roots: print(r)
    return rc

if __name__ == "__main__": sys.exit(main())
