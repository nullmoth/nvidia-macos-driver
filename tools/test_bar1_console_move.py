#!/usr/bin/env python3
"""Verbose console silenced when moving BAR1 leaves it at an address nothing decodes (NVRM::placeLargeBar1).

A GTX 1650 on an AMD board booted with -v: firmware BAR1 64 MB at 0xfff80000000 held the console, the driver resized it
to 256 MB at 0x3ffa0000000, and every later IOLog line was drawn through the old, no longer decoded address at about
half a second a line. RmInitAdapter took 247 s, the boot hold expired, and the desktop came up with no Metal device.
The predicate is compiled out of NVRM.cpp itself; the arms include the cases that must NOT silence the console."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRM/NVRM.cpp').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


head = 'static inline bool nvrmConsoleLostByMove('
check('the predicate exists in NVRM.cpp', src.count(head) == 1, src.count(head))
start = src.index(head)
body = src[start:src.index('\n}\n', start) + 3]

placed = src.index('LOG("bar1: PLACED')
call = src.find('nvrmConsoleLostByMove(con, fwBar1, fwBar1Size, bar1Base, bar1Size, verbose)', placed)
check('placeLargeBar1 asks the predicate right after PLACED', 0 < call - placed < 600, call - placed)
check('the console is silenced with kPEDisableScreen there',
      src.find('setConsoleInfo(0, kPEDisableScreen)', call) - call < 200 if call > 0 else False)
check('the firmware size is kept before any resize',
      src.index('const UInt64 fwBar1Size = bar1Size;') < src.index('    UInt64 newSize = 0;\n    if (smallBar) {'))
check('verbose is read from the -v boot argument', 'PE_parse_boot_argn("-v", vb, sizeof vb)' in src[placed:placed + 900])

prog = '#include <stdio.h>\n#include <stdlib.h>\n' + body + r'''
int main(int c, char **v) {
    printf("%d\n", nvrmConsoleLostByMove(strtoull(v[1], 0, 0), strtoull(v[2], 0, 0), strtoull(v[3], 0, 0),
                                         strtoull(v[4], 0, 0), strtoull(v[5], 0, 0), atoi(v[6]) != 0));
    return 0;
}
'''
MB = 1 << 20
with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    (t / 'c.cpp').write_text(prog)
    r = subprocess.run(['/usr/bin/clang++', '-std=c++17', '-Wall', '-Werror', t / 'c.cpp', '-o', t / 'c'],
                       capture_output=True, text=True, timeout=120)
    check('the predicate compiles out of NVRM.cpp', r.returncode == 0, r.stderr[:300])
    if r.returncode == 0:
        def lost(con, ob, os_, nb, ns, verbose=1):
            out = subprocess.run([t / 'c', hex(con), hex(ob), hex(os_), hex(nb), hex(ns), str(verbose)],
                                 capture_output=True, text=True, timeout=30).stdout.strip()
            return out == '1'
        gtx = (0xfff80000000, 0xfff80000000, 64 * MB, 0x3ffa0000000, 256 * MB)
        check('the field GTX 1650 (-v, console at the old BAR1 base, BAR1 moved) is silenced', lost(*gtx))
        check('negative control: the same move without -v leaves the console alone', not lost(*gtx, verbose=0))
        check('negative control: a console on another GPU (outside the old BAR1) is left alone',
              not lost(0x4000000000, 0xfff80000000, 64 * MB, 0x3ffa0000000, 256 * MB))
        check('negative control: no console at all is left alone', not lost(0, 0xfff80000000, 64 * MB, 0x3ffa0000000, 256 * MB))
        check('negative control: a resize in place that still covers the console is left alone',
              not lost(0xe0001000, 0xe0000000, 64 * MB, 0xe0000000, 256 * MB))
        check('the console one byte inside the end of the old BAR1 is silenced',
              lost(0xfff80000000 + 64 * MB - 4, 0xfff80000000, 64 * MB, 0x3ffa0000000, 256 * MB))
        check('negative control: the console one byte past the old BAR1 is left alone',
              not lost(0xfff80000000 + 64 * MB, 0xfff80000000, 64 * MB, 0x3ffa0000000, 256 * MB))

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
