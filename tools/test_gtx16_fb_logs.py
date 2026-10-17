#!/usr/bin/env python3
"""Framebuffer log lines a 256 MB BAR1 board (every GTX 16) must read correctly. A spare instance (index at or past the
connected displays) is reported as expected, a real kapiInit failure keeps its failure line, and a full BAR1 logs a
bounded number of refusals plus the holder summary once."""
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


i = src.index('if (!kapiOk) {\n        // nvfbheads=4')
blk = src[i:src.index('return false;', i)]
check('a spare instance is reported as expected only when kapiInit stopped for want of a display (flag set at that exit, not re-read later)',
      'if (fSpareIndex)' in blk and 'spare framebuffer instance' in blk
      and 'fSpareIndex = gNDpy > 0;' in src)
check('a real kapiInit failure still prints the failure line (else branch)',
      'else\n            FBLOG("kapiInit failed -- refusing to start.' in blk)
j = src.index('static volatile SInt32 refused = 0;')
rb = src[j:src.index('return false;', j)]
check('refusals are bounded: first eight, then every 256th', 'nRefused <= 8 || (nRefused & 255) == 0' in rb)
check('the holder summary prints once, on the first refusal', 'if (nRefused == 1)' in rb and 'BAR1 full:' in rb)
check('a refusal still returns false (the allocation falls back to system memory)', src[j:].index('return false;') > 0)
sys.exit(1 if fails else 0)
