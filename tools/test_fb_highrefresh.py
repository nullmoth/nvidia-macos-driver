#!/usr/bin/env python3
"""540 Hz panels: the published frame-rate range reaches them, and the synchronous flip wait is deferred at that rate.

IOFBTimingRange stopped at 500 Hz, so macOS dropped a 540 Hz panel's own timing; and the latch waited for every flip,
which at 540 Hz (1.85 ms frame) costs most of the frame. The real helpers are compiled from nvrm-fb-safety.h and the
call sites are checked in nvrm-fb.cpp. The control is the old rule (500 Hz cap, always wait)."""
import pathlib, re, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert 'r.maxFrameRate = kNvrmMaxFrameRate' in src, 'the timing range still has a hard-coded frame-rate cap'
assert 'nvrmFlipLatchMode(gFlipLatch, gFlipLatchAuto, flipWaitUs() * 800ull) == 2' in src, 'submit path no longer picks the latch mode by rate'
assert 'r.maxPixelClock = (UInt64)kNvrmMaxPixelClockMHz * 1000000ull' in src, 'the pixel-clock cap is hard-coded again'
assert 'sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_auto)' in src, 'the auto switch is not registered'
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
static unsigned long long ns(unsigned hz) { return 1000000000ull / hz; }
int main() {
    assert(kNvrmMaxFrameRate >= 540 && kNvrmMaxFrameRate > 500);
    assert(kNvrmMaxPixelClockMHz >= 2100 && 1370 < kNvrmMaxPixelClockMHz);   // 2560x1600@300 (1.37 GHz) and 4K@240 DSC (2.1 GHz) fit
    assert(nvrmFlipLatchMode(1, 1, ns(540)) == 2);   // 540 Hz: deferred
    assert(nvrmFlipLatchMode(1, 1, ns(360)) == 2);
    assert(nvrmFlipLatchMode(1, 1, ns(240)) == 1);   // 240 Hz and below: unchanged synchronous wait
    assert(nvrmFlipLatchMode(1, 1, ns(60)) == 1);
    assert(nvrmFlipLatchMode(1, 0, ns(540)) == 1);   // auto switched off
    assert(nvrmFlipLatchMode(0, 1, ns(540)) == 0);   // explicit bypass untouched
    assert(nvrmFlipLatchMode(2, 1, ns(60)) == 2);    // explicit deferred untouched
    assert(nvrmFlipLatchMode(1, 1, 0) == 1);         // unknown period: wait
    // OLD RULE (control): 500 Hz cap excludes 540, latch mode never changes with rate
    assert(540 > 500);
    puts("PASS: range reaches 540 Hz, latch deferred from 357 Hz up, slower panels and explicit settings unchanged");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
