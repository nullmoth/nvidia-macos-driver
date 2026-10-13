#!/usr/bin/env python3
"""Surface budget and park ceiling follow the real BAR1. Compiles nvrmVramBudget and nvrmParkCeiling from NVRMFB and
checks every window size seen in reports (64 MB RTX 2060, 256 MB firmware default, 8 GB resizable), and that BAR1 is
found when it is under 256 MB (a 64 MB BAR1 used to read as 0: 192 MB budget, 64 MB park ceiling, three times the
window, so allocations failed in mapMemory)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
abi = (root / 'kexts/NVRMFB/nvrm_vram_abi.h').read_text()


def func(name):
    i = src.index(name)
    i = src.rindex('\n', 0, i) + 1
    j = src.index('\n}\n', i) + 3
    return src[i:j]


code = ('#include <stdint.h>\n#include <stdio.h>\ntypedef int64_t SInt64; typedef uint64_t NvU64;\n'
        + '\n'.join(l for l in abi.splitlines() if l.startswith('#define NVRM_VRAM_'))
        + '\n#define NVRM_VRAM_PARK_CEILING (64ull * 1024ull * 1024ull)\n'
        + func('static inline uint64_t nvrmVramBudget(') + func('static inline SInt64 nvrmParkCeiling(')
        + r'''
int main(void) {
    const uint64_t sizes[] = { 0, 64ull << 20, 128ull << 20, 256ull << 20, 1ull << 30, 8ull << 30 };
    for (int i = 0; i < 6; i++) printf("%llu %llu %lld\n", (unsigned long long)(sizes[i] >> 20),
        (unsigned long long)(nvrmVramBudget(sizes[i]) >> 20), (long long)(nvrmParkCeiling(sizes[i]) >> 20));
    return 0;
}
''')
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


with tempfile.TemporaryDirectory() as d:
    c, exe = pathlib.Path(d) / 'b.c', pathlib.Path(d) / 'b'
    c.write_text(code)
    r = subprocess.run(['cc', '-w', '-o', str(exe), str(c)], capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-1500:])
        sys.exit('could not compile the budget functions - check is UNKNOWN, not clean')
    rows = {int(a): (int(b), int(p)) for a, b, p in (l.split() for l in subprocess.check_output([str(exe)], text=True).splitlines())}
check('a 64 MB BAR1 gets 48 MB of surfaces, not 192', rows[64][0] == 48, f'{rows[64][0]} MB')
check('a 64 MB BAR1 parks at most 16 MB', rows[64][1] == 16, f'{rows[64][1]} MB')
check('a 128 MB BAR1 gets 96 MB and parks at most 32 MB', rows[128] == (96, 32), str(rows[128]))
check('the 256 MB firmware window keeps 192 MB and a 32 MB ceiling', rows[256] == (192, 32), str(rows[256]))
check('an unknown window keeps the 192 MB default', rows[0][0] == 192, f'{rows[0][0]} MB')
check('an 8 GB resizable BAR1 keeps half for surfaces and a 64 MB ceiling', rows[8192] == (4096, 64), str(rows[8192]))
disc = func('static void nvrmDiscoverBar1(')
check('BAR1 discovery skips BAR0 and takes the largest range, with no 256 MB minimum',
      'for (IOItemCount i = 1; i < count; i++)' in disc and '0x10000000ull' not in disc)
sys.exit(1 if fails else 0)
