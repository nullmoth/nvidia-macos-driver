#!/usr/bin/env python3
"""Exercise the production BAR1 budget against display and encoder pressure.

An RTX 2060 SUPER with 256 MiB BAR1 failed H.264 session creation while
desktop surfaces held most of the aperture. The encoder maps 33.5 MiB of
video memory; the old 16 MiB RM margin could not accommodate it. Keep the
display accounting, leave room for the codec plus existing RM use, and
retain the policy on larger apertures. This host check does not qualify
encoder operation on an installed kernel.
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
disp_3840 = 3840 * 2560 * 4 + 256 * 256 * 4   # the measured mode's scanout + a 256-px cursor (~40 MB)
disp_big = 96 * MB                     # a large/multi-plane scanout that the fixed budget would over-promise
disp_recording = 66977792              # two connected heads, including reserved scanout and cursors
encoder_maps = 35127296                # traced H.264 1080p CPU mappings of video memory

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

        # 2. the original large-scanout case still stays inside the aperture.
        b = budget(BAR256, disp_3840)
        check('   ...and the budget stays inside the 256 MB aperture (budget + display <= BAR1)',
              b + disp_3840 <= BAR256, f'{b}+{disp_3840}={b + disp_3840} vs {BAR256}')

        # 3. negative control: the OLD fixed budget plus a large display exceeds the whole BAR1 (the mapMemory overflow);
        #    the new budget lowers itself so display + budget + a 16 MB margin stay within the aperture.
        bl = budget(BAR256, disp_big)
        check('negative control: old fixed 192 MB + a 96 MB display overruns the 256 MB BAR1',
              OLD192 + disp_big > BAR256, f'{OLD192}+{disp_big}={OLD192 + disp_big} vs {BAR256}')
        check('   ...the display-aware budget does not (budget + display + 16 MB margin <= BAR1)',
              bl + disp_big + 16 * MB <= BAR256 and bl < OLD192, f'{bl}+{disp_big}+16MB vs {BAR256}')

        # The old display-aware policy left only 16 MiB for every non-surface mapping.
        for display in (disp_3840, disp_recording, disp_big):
            old_budget = BAR256 - display - 16 * MB
            check(f'negative control: old margin cannot fit 1080p encoder with display={display}',
                  old_budget + display + encoder_maps + 16 * MB > BAR256)
            current = budget(BAR256, display)
            check(f'desktop, display, 1080p encoder and existing RM mappings fit with display={display}',
                  current > 0 and current + display + encoder_maps + 16 * MB <= BAR256)

        # Smaller apertures reserve proportionally; a display that consumes the window
        # must refuse video-memory grants instead of restoring an overcommitted budget.
        for bar in (64 * MB, 128 * MB, BAR256):
            display = bar // 4
            check(f'{bar // MB} MiB BAR leaves room for display and other RM mappings',
                  budget(bar, display) + display + bar // 4 <= bar)
            check(f'{bar // MB} MiB BAR with no remaining room refuses new surface grants',
                  budget(bar, bar) == 0 and budget(bar, bar + MB) == 0)

        # 4. the display toggle off restores the exact old fixed fraction (the safety valve).
        check('negative control: debug.nvrmfb_vram_display_aware=0 restores the old 192 MB fraction',
              budget(BAR256, disp_3840, aware=0) == OLD192, budget(BAR256, disp_3840, aware=0))

        # 5. a resizable BAR (>= 4 GB) is untouched: half, display is a rounding error.
        check('8 GB BAR1 keeps its half-aperture policy regardless of the display',
              budget(8 * 1024 * MB, disp_3840) == 4 * 1024 * MB, budget(8 * 1024 * MB, disp_3840))
        for bar in (512 * MB, 1024 * MB, 2 * 1024 * MB):
            check(f'{bar // MB} MiB BAR retains the preceding display-aware budget',
                  budget(bar, disp_recording) == bar - disp_recording - 16 * MB)
        for bar in (4 * 1024 * MB, 8 * 1024 * MB, 16 * 1024 * MB):
            for display in (0, disp_recording, bar):
                check(f'{bar // MB} MiB BAR keeps half-window policy with display={display}',
                      budget(bar, display) == bar // 2)

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
