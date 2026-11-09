#!/usr/bin/env python3
"""Scaled modes beyond what the head scaler can move are no longer offered (issue 360 class), learned or probed.

Compiled from nvrm-fb-safety.h: the load metric, learning from a refusal, and the binary search a probe runs over trial
commits. Source checks for the call sites. Arms use the reported figures: 3840x2400@165, 4096x2304@120 and 3360x2100@240
refused while the same rasters at 60 Hz commit, so a limit between ~0.5 and ~1.1 Gpx/s hides exactly the fast ones. The control
is the old driver, which offered every scaled mode and refused it only at the set."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['why = "beyond the head scaler\'s pixel rate"', 'fScalerLimit = nvrmScalerLearn(fScalerLimit, fPendingLoad)',
             'nvrmScalerSearch(loads, n, test, &trials)', 'PE_parse_boot_argn("nvfbscalerprobe"', 'bool NVRMNVDAFramebuffer::trialScaledMode']:
    assert need in src, 'missing: ' + need
fixture = r'''
#include <cassert>
#include <cstdio>
#include <vector>
#include <algorithm>
#include "nvrm-fb-safety.h"
int main() {
    typedef unsigned long long U;
    // loads of the reported modes (desktop == raster here: the larger side is used)
    const U a = nvrmScalerLoad(3840, 2400, 1920, 1200, 165), b = nvrmScalerLoad(4096, 2304, 2048, 1152, 120), c = nvrmScalerLoad(3360, 2100, 1680, 1050, 240);
    const U ok60 = nvrmScalerLoad(3840, 2400, 1920, 1200, 60);
    assert(a == 3840ull * 2400 * 165 && ok60 == 3840ull * 2400 * 60);
    assert(nvrmScalerLoad(1920, 1080, 3840, 2160, 60) == 3840ull * 2160 * 60);          // a downscaled desktop larger than the raster counts
    // learning from one refusal: 3840x2400@165 refused, so that load and everything above it is hidden, 60 Hz stays
    U limit = nvrmScalerLearn(0, a);
    assert(!nvrmScalerAllows(limit, a) && nvrmScalerAllows(limit, ok60) && nvrmScalerAllows(limit, a - 1));
    limit = nvrmScalerLearn(limit, b);                                                  // a lighter refusal lowers it
    assert(limit == b - 1 && !nvrmScalerAllows(limit, c ? b : 0));
    limit = nvrmScalerLearn(limit, a);                                                  // a heavier one never raises it
    assert(limit == b - 1);
    assert(nvrmScalerLearn(5, 0) == 5 && nvrmScalerAllows(0, ~0ull));                    // nothing refused: no limit
    // binary search over 300 ascending loads with the true limit at load 777
    std::vector<U> loads; for (U i = 1; i <= 300; i++) loads.push_back(i * 3);
    unsigned trials = 0;
    U found = nvrmScalerSearch(loads.data(), (unsigned)loads.size(), [](U l) { return l <= 777; }, &trials);
    assert(found == 777 && trials <= 9);                                                // 9 trials, not 300 mode sets
    assert(nvrmScalerSearch(loads.data(), 300, [](U) { return true; }, &trials) == ~0ull);          // everything passes: no limit
    assert(nvrmScalerSearch(loads.data(), 300, [](U) { return false; }, &trials) == 0);              // even the lightest fails
    puts("PASS: scaler load metric, refusals teach a never-rising limit, probe finds it in log2(n) trials");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
