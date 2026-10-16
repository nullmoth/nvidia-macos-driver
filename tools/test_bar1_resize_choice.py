#!/usr/bin/env python3
"""BAR1 size choice in NVRM::placeLargeBar1, compiled from the kext source itself and run on the masks user logs show.

A Turing card offers 64-256 MB (Resizable BAR sizes mask 0x1c0). Firmware left a GTX 1630, a GTX 1660 SUPER and an
RTX 2060 at 64 MB, and the driver only ever tried 4-8 GB, so they stayed at 64 MB. They now take 256 MB; cards that
offer 4-8 GB still take 8 GB; a card already at its largest size is left alone unless the boot screen sits inside its
BAR1, in which case it moves at its own size. -nvrmsmallbar sizes a card to 256 MB."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRM/NVRM.cpp').read_text()
start = src.index('    UInt64 newSize = 0;\n    if (smallBar) {')
tail = '            bar1Size = newSize;\n        }\n    }\n'
end = src.index(tail, start) + len(tail)
body = src[start:end]
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


# BAR1 sits at 0xE0000000; the console is either inside it or elsewhere
prog = r'''#include <stdio.h>
#include <stdlib.h>
#include <strings.h>
typedef unsigned long long UInt64; typedef unsigned int UInt32;
#define LOG(...) ((void)0)
#define kIOReturnSuccess 0
static UInt64 g_con;
struct PE_Video { unsigned long v_baseAddr; };
struct Platform { int getConsoleInfo(PE_Video *v) { v->v_baseAddr = g_con; return 0; } };
static Platform g_platform;
static Platform *getPlatform() { return &g_platform; }
struct PCI { UInt32 configRead32(int off) { return off == 0x14 ? 0xE000000Cu : 0; } };
static PCI g_pci, *fPCI = &g_pci;
static UInt64 choose(UInt64 bar1Size, UInt32 rbSupported, UInt32 rbCtlOff, bool smallBar) {
''' + body + r'''
    return newSize ? newSize : bar1Size;
}
int main(int c, char **v) {
    g_con = strtoull(v[4], 0, 0);
    printf("%llu\n", choose(strtoull(v[1], 0, 0) << 20, (UInt32)strtoul(v[2], 0, 0), 0x108, atoi(v[3])) >> 20);
    return 0;
}
'''
# "return false" in the excerpt means "not placing": the harness reads that as 0 MB
prog = prog.replace('return false;', 'return 0;')
with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    (t / 'c.cpp').write_text(prog)
    r = subprocess.run(['/usr/bin/clang++', '-std=c++17', '-Wall', '-Werror', t / 'c.cpp', '-o', t / 'c'],
                       capture_output=True, text=True, timeout=120)
    check('the size choice compiles out of NVRM.cpp', r.returncode == 0, r.stderr[:300])
    if r.returncode == 0:
        def mb(size_mb, mask, small=0, console=0x80000000):
            return int(subprocess.run([t / 'c', str(size_mb), hex(mask), str(small), hex(console)],
                                      capture_output=True, text=True, timeout=30).stdout)
        inside = 0xE0001000
        check('a Turing card left at 64 MB (mask 0x1c0) is resized to 256 MB', mb(64, 0x1c0) == 256, mb(64, 0x1c0))
        check('a card at 64 MB that offers 8 GB (mask 0x7fc0) still takes 8 GB', mb(64, 0x7fc0) == 8192, mb(64, 0x7fc0))
        check('a card at 256 MB offering 4 GB at most (mask 0x1fc0) takes 4 GB', mb(256, 0x1fc0) == 4096, mb(256, 0x1fc0))
        check('negative control: a Turing card at 256 MB, console elsewhere, is not moved', mb(256, 0x1c0) == 0, mb(256, 0x1c0))
        check('a Turing card at 256 MB with the console inside BAR1 moves at 256 MB', mb(256, 0x1c0, console=inside) == 256,
              mb(256, 0x1c0, console=inside))
        check('negative control: a card offering only its current 64 MB is not moved', mb(64, 0x40) == 0, mb(64, 0x40))
        check('a card at 8 GB keeps 8 GB (no resize)', mb(8192, 0x7fc0) == 8192, mb(8192, 0x7fc0))
        check('-nvrmsmallbar sizes an 8 GB card to 256 MB', mb(8192, 0x7fc0, small=1) == 256, mb(8192, 0x7fc0, small=1))
        check('negative control: -nvrmsmallbar on a card without 256 MB is refused', mb(8192, 0x6000, small=1) == 0,
              mb(8192, 0x6000, small=1))

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
