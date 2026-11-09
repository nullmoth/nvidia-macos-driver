#!/usr/bin/env python3
"""A mode change waits a bounded time for the display gate instead of forever (WindowServer watchdog class 2).

nvrmTryUntil() is compiled from nvrm-fb-safety.h and driven by a simulated lock that frees after N sleeps; the call site is
checked in nvrm-fb.cpp. Arms: free lock = immediate; busy then free = acquired within the bound; held beyond the bound =
gives up after about the bound and no longer; step granularity respected. The control is the old behaviour, an unconditional
IORecursiveLockLock, which never returns while the lock is held."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['NVRMBoundedGate modeGate(kModeChangeGateMs);', 'return kIOReturnTimeout;', 'IORecursiveLockTryLock(lock)']:
    assert need in src, 'missing: ' + need
assert src.index('NVRMBoundedGate modeGate') > src.index('IOReturn NVRMNVDAFramebuffer::setDisplayMode(')
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
int main() {
    unsigned slept = 0, tries = 0, busy = 0;
    auto sleepFn = [&](unsigned ms) { slept += ms; };
    auto tryFn = [&]() { tries++; return slept >= busy; };
    busy = 0;     assert(nvrmTryUntil(tryFn, sleepFn, 20000, 2) && slept == 0 && tries == 1);          // free: immediate
    slept = tries = 0; busy = 50;   assert(nvrmTryUntil(tryFn, sleepFn, 20000, 2) && slept == 50);     // busy for 50 ms: got it then
    slept = tries = 0; busy = 1000000; assert(!nvrmTryUntil(tryFn, sleepFn, 20000, 2) && slept == 20000 && tries == 10001);   // never free: gives up at the bound
    slept = tries = 0; busy = 19998; assert(nvrmTryUntil(tryFn, sleepFn, 20000, 2));                    // freed just inside the bound
    slept = tries = 0; busy = 1000000; assert(!nvrmTryUntil(tryFn, sleepFn, 10, 0) && slept == 10);     // a zero step is treated as 1 ms
    puts("PASS: free gate immediate, busy gate acquired within the bound, held gate abandoned at the bound");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
