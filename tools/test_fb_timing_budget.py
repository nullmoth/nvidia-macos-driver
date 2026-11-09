#!/usr/bin/env python3
"""When macOS proposes more timings than the framebuffer holds, the highest refresh rates must survive.

A 180 Hz panel gets 536 timings from macOS and its first 180 Hz scaled mode is number 307; keeping the first N in
arrival order (N was 224) left only 60 Hz HiDPI desktops. The real selector is compiled from nvrm-fb-safety.h; the
negative control is the old keep-the-first-N rule, which must lose every 180 Hz mode on the same input."""
import pathlib
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
cap = int(re.search(r'IODetailedTimingInformationV2 fDT\[(\d+)\]', src).group(1))
budget = int(re.search(r'kMaxMacTimings = (\d+);', src).group(1))
assert budget + 32 <= cap, 'the macOS timing budget leaves no room for the NVKMS refresh rates and the alias'
assert 'nvrmPickTimings(rate, must, offered, kMaxMacTimings, keep)' in src, 'setDetailedTimings no longer prioritises by refresh rate'

fixture = r'''
#include <cassert>
#include <cstdio>
#include <vector>
#include "nvrm-fb-safety.h"
int main() {
    const unsigned BUDGET = %d;
    // 536 timings as macOS lists them: 60 Hz first, 144 Hz from 200, 180 Hz from 307.
    std::vector<unsigned> hz(536);
    for (unsigned i = 0; i < 536; i++) hz[i] = i < 200 ? 60 : i < 307 ? 144 : 180;
    std::vector<unsigned char> must(536, 0), keep(536, 0);
    must[0] = 1;
    unsigned n = nvrmPickTimings(hz.data(), must.data(), 536, BUDGET, keep.data());
    assert(n == BUDGET);
    unsigned k180 = 0, k144 = 0, k60 = 0;
    for (unsigned i = 0; i < 536; i++) if (keep[i]) { if (hz[i] == 180) k180++; else if (hz[i] == 144) k144++; else k60++; }
    assert(k180 == 229 && k144 == 107);            // every 180 and 144 Hz timing kept
    assert(keep[0] && k60 == BUDGET - 229 - 107);  // the mode on the head kept, 60 Hz fills the rest
    // under budget: everything stays
    assert(nvrmPickTimings(hz.data(), nullptr, 100, BUDGET, keep.data()) == 100);
    // the mode on the head survives even when it is the lowest rate and the budget is tiny
    std::vector<unsigned char> m2(536, 0); m2[5] = 1;
    nvrmPickTimings(hz.data(), m2.data(), 536, 10, keep.data());
    assert(keep[5]);
    // OLD RULE (negative control): first N in arrival order
    unsigned old180 = 0;
    for (unsigned i = 0; i < 224 && i < 536; i++) if (hz[i] == 180) old180++;
    printf("old rule keeps %%u 180 Hz timings, new keeps %%u\n", old180, k180);
    assert(old180 == 0);
    puts("PASS: 180 Hz scaled modes survive a 536-timing list, the mode on the head is never dropped, short lists untouched");
}
''' % budget
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / 't.cpp').write_text(fixture)
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Werror', f'-I{root}/kexts/NVRMFB/fb', str(d / 't.cpp'), '-o', str(d / 't')], check=True)
    subprocess.run([str(d / 't')], check=True)
