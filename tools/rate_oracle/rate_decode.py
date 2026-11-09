#!/usr/bin/env python3
"""rate_decode.py <parameter-buffer.bin> - prints the (scale, offset) pairs a real MTLRasterizationRateMap wrote with
copyParameterDataToBuffer (tools/rate_oracle/rate_params.m). The buffer is 12300 32-bit words of float pairs in several tables;
this lists each table's non-empty run. Measured so far: scale is quantized to multiples of 1/32; the 1000x500 uniform 0.5 map has
fifteen (1.96875, 0) entries (1008 / 512) and then (1.71875, -69.8164); the 1.0 -> 0.5 two-cell map has 26 entries whose scale rises
from 1.0 to 1.84 with offsets that are not continuous from entry to entry, so entry k is NOT simply screen = phys * scale + offset
for k = phys // 16 or phys // 32 (tried, it disagrees with the measured mapping). Not yet reproduced."""
import struct, sys
d = open(sys.argv[1], 'rb').read()
f = struct.unpack('<%df' % (len(d) // 4), d)
pairs = [(f[i], f[i + 1]) for i in range(0, len(f), 2)]
run = None
for i, p in enumerate(pairs + [(0.0, 0.0)]):
    live = p != (0.0, 0.0)
    if live and run is None: run = i
    if not live and run is not None:
        print("table entries %d..%d:" % (run, i - 1), ' '.join("(%.5g,%.5g)" % pairs[k] for k in range(run, min(i, run + 12))), '...' if i - run > 12 else '')
        run = None
