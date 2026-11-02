#!/usr/bin/env python3
# mixsetup.py: build picture setups that take some regions from one blob and the rest from another (T2.05e bisection).
#   scripts/mixsetup.py BASE OTHER OUTDIR [region=start:end ...]   -> OUTDIR/<region>.bin = BASE with OTHER's region
import os, sys
base, other, outdir = open(sys.argv[1], 'rb').read(), open(sys.argv[2], 'rb').read(), sys.argv[3]
R = {'magic': (0x0, 0x4), 'refcfg': (0x4, 0x24), 'incfg': (0x24, 0x44), 'outcfg': (0x44, 0x64), 'sps': (0x64, 0x68), 'pps': (0x68, 0x70),
     'rc': (0x70, 0xc8), 'pic': (0xc8, 0x1dc), 'tail': (0x1dc, 0x200), 'x200': (0x200, 0x3000), 'me': (0x3000, 0x30c0),
     'md': (0x3c00, 0x3c80), 'q': (0x4400, 0x44c0), 'slice': (0x5000, 0x5080)}
for a in sys.argv[4:]:
    n, rng = a.split('='); s, e = rng.split(':'); R[n] = (int(s, 0), int(e, 0))
os.makedirs(outdir, exist_ok=True)
for n, (s, e) in R.items():
    b = bytearray(base); b[s:e] = other[s:e]
    if b != bytearray(base): open(os.path.join(outdir, n + '.bin'), 'wb').write(b)
print(' '.join(sorted(os.listdir(outdir))))
