#!/usr/bin/env python3
# trace2blob.py: pull one buffer dump out of an nvtrace log into a binary file (zero rows restored).
#   scripts/trace2blob.py LOG EXEC_NUMBER "picture setup" out.bin [size]
import re, sys
log, n, what, out = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
size = int(sys.argv[5], 0) if len(sys.argv) > 5 else 0x8000
buf = bytearray(size); inexec = grab = False
for line in open(log):
    if '==== EXECUTE #' in line: inexec = f'EXECUTE #{n} ' in line; grab = False; continue
    if not inexec: continue
    if line.strip().startswith(what): grab = True; continue
    m = re.match(r'\s+([0-9a-f]{4}):((?: [0-9a-f]{8})+)\s*$', line)
    if grab and m:
        off = int(m.group(1), 16)
        for i, w in enumerate(m.group(2).split()):
            if off + 4 * i + 4 <= size: buf[off + 4 * i:off + 4 * i + 4] = int(w, 16).to_bytes(4, 'little')
    elif grab and not m: grab = False
open(out, 'wb').write(buf)
