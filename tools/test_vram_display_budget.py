#!/usr/bin/env python3
"""The accel surface budget reserves the BAR1 the framebuffer's own scanout and cursor already hold.

nvrmSurfaceBudget is compiled straight out of kexts/NVRMFB/fb/nvrm-fb.cpp and run on the numbers from the RTX 2070 Super
report (256 MB BAR1, no larger size offered, 3840x2560). On that machine the fixed 3/4 budget (192 MB) was spent and
WindowServer's surfaces were refused -- "BAR1 budget spent (200015872 of 201326592 bytes)", NVAccel allocPhysical
REFUSED -- while ~40 MB of scanout sat in BAR1 uncounted. Counting it: a small display frees the BAR1 the old fraction
wasted, and a large display lowers the budget instead of over-promising the aperture.

Negative control: the old fixed 192 MB budget plus a large display exceeds the whole 256 MB BAR1, which is exactly the
mapMemory failure small-BAR machines hit; the new budget keeps display + budget + margin inside the aperture.
"""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
if 'nvrmSurfaceBudget' not in src:
    print('FAIL the display-aware budget nvrmSurfaceBudget is not present in nvrm-fb.cpp')
    print('FAILED')
    sys.exit(1)
start = src.index('static inline uint64_t nvrmVramBudget(uint64_t barLen)')
end = src.index('\n}\n', src.index('static inline uint64_t nvrmSurfaceBudget')) + len('\n}\n')
body = src[start:end]
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


prog = r'''#include <stdio.h>
#include <stdlib.h>
typedef unsigned long long uint64_t; typedef long long SInt64;
#define NVRM_VRAM_BAR1_BUDGET (192ull * 1024ull * 1024ull)
''' + body + r'''
int main(int c, char **v) {
    (void)gVramDisplayBytes;  /* the live kext counter; this harness passes the value as an argument instead */
    if (c > 3) gVramDisplayAware = atoi(v[3]);
    printf("%llu\n", (unsigned long long)nvrmSurfaceBudget(strtoull(v[1], 0, 0), (SInt64)strtoll(v[2], 0, 0)));
    return 0;
}
'''
MB = 1 << 20
BAR256 = 256 * MB
OLD192 = 192 * MB                      # NVRM_VRAM_BAR1_BUDGET, the budget the logs show spent (201326592)
SPENT = 200015872                      # the "of 201326592" the RTX 2070 Super refused a request at
disp_3840 = 3840 * 2560 * 4 + 256 * 256 * 4   # the measured mode's scanout + a 256-px cursor (~40 MB)
disp_big = 96 * MB                     # a large/multi-plane scanout that the fixed budget would over-promise

with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    (t / 'c.c').write_text(prog)
    r = subprocess.run(['/usr/bin/clang', '-Wall', '-Werror', str(t / 'c.c'), '-o', str(t / 'c')],
                       capture_output=True, text=True, timeout=120)
    check('nvrmSurfaceBudget compiles out of nvrm-fb.cpp', r.returncode == 0, r.stderr[:300])
    if r.returncode == 0:
        def budget(barlen, disp, aware=1):
            out = subprocess.run([str(t / 'c'), str(barlen), str(disp), str(aware)],
                                 capture_output=True, text=True, timeout=30).stdout
            return int(out)

        # 1. before the scanout is mapped, the conservative fixed policy stands, so nothing is over-granted at boot.
        check('256 MB BAR1, display not measured yet -> the old 192 MB policy',
              budget(BAR256, 0) == OLD192, budget(BAR256, 0))

        # 2. the measured machine: with its ~40 MB scanout counted, the request the old budget refused now fits.
        b = budget(BAR256, disp_3840)
        check('256 MB BAR1, 3840x2560 scanout counted -> budget admits the request the old 192 MB refused',
              b > SPENT, f'{b} vs spent {SPENT}')
        check('   ...and the budget stays inside the 256 MB aperture (budget + display <= BAR1)',
              b + disp_3840 <= BAR256, f'{b}+{disp_3840}={b + disp_3840} vs {BAR256}')

        # 3. negative control: the OLD fixed budget plus a large display exceeds the whole BAR1 (the mapMemory overflow);
        #    the new budget lowers itself so display + budget + a 16 MB margin stay within the aperture.
        bl = budget(BAR256, disp_big)
        check('negative control: old fixed 192 MB + a 96 MB display overruns the 256 MB BAR1',
              OLD192 + disp_big > BAR256, f'{OLD192}+{disp_big}={OLD192 + disp_big} vs {BAR256}')
        check('   ...the display-aware budget does not (budget + display + 16 MB margin <= BAR1)',
              bl + disp_big + 16 * MB <= BAR256 and bl < OLD192, f'{bl}+{disp_big}+16MB vs {BAR256}')

        # 4. the display toggle off restores the exact old fixed fraction (the safety valve).
        check('negative control: debug.nvrmfb_vram_display_aware=0 restores the old 192 MB fraction',
              budget(BAR256, disp_3840, aware=0) == OLD192, budget(BAR256, disp_3840, aware=0))

        # 5. a resizable BAR (>= 4 GB) is untouched: half, display is a rounding error.
        check('8 GB BAR1 keeps its half-aperture policy regardless of the display',
              budget(8 * 1024 * MB, disp_3840) == 4 * 1024 * MB, budget(8 * 1024 * MB, disp_3840))

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
