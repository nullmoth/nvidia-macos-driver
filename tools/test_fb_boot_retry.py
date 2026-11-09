#!/usr/bin/env python3
"""A boot that finds no display asks again (issue 88: DP AUX timed out for every connector in the first query; HDMI on the same card was fine).

Evidence from the issue's zip (own directory, read as data): the first dynamic query lands 14 ms after the firmware is up, seven
DP_AUXCH_CTRL calls 2 ms apart all return NV_ERR_TIMEOUT (0x65), all four connectors report connected 0 with connector 0, and the
driver gives up for the boot ('ENUMERATED: 0'); nothing ever looks again. On HDMI the same probe works. A sink that has not raised
HPD or woken its AUX channel by then is a timing fact of the monitor, not something to fix in the query. The fix is to look again:
6 more scans, 3 s apart, through the hot-plug path (which starts a framebuffer for a display that appears and, since #288, can reach
the idle nubs). nvfbbootretry=0 turns it off. The policy is compiled from nvrm-fb-safety.h; the wiring is checked in nvrm-fb.cpp."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['PE_parse_boot_argn("nvfbbootretry"', 'gNDpy == 0 && gBootRetry < kNvrmBootRetries', 'nvrmBootRetryDelayMs(gBootRetry++)', 'if (gNDpy == 0 && n > 0)']:
    assert need in src, need
assert src.index('static unsigned gBootRetry') < src.index('gBootRetry = off ?'), 'declared after use'
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
int main() {
    unsigned total = 0, tries = 0;
    for (unsigned a = 0; ; a++) { const unsigned ms = nvrmBootRetryDelayMs(a); if (!ms) break; total += ms; tries++; }
    assert(tries == 6 && total == 18000);           // six more looks over 18 s, then quiet
    assert(nvrmBootRetryDelayMs(6) == 0 && nvrmBootRetryDelayMs(1000) == 0);
    puts("PASS: six re-scans 3 s apart, then it stops");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
