#!/usr/bin/env python3
"""A flip refused because the display is asleep is not counted as a refusal (issue 470, WindowServer hang after display sleep).

The log in the issue: display sleep, then three refused flips ('NVKMS ioctl 14 refused ... pm running') across two heads, which hit the
3-in-a-row rule and turned the asynchronous swap off for the rest of the boot; the WindowServer fence wait followed. The outcome
mapping is lifted from nvrm-accel.cpp and run with the framebuffer's answers. Control: rel-29 maps every failure to 'refused'."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
acc = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text(); fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert fb.count('(fHeadSlept || !rmGpuReady()) ? kIOReturnNotReady') == 2
assert 'outcome == kNvFlipAsleep' in acc and 'nvAccelFlipOutcome(rr) == kNvFlipAsleep) return true' in acc
a = acc.index('enum { kNvFlipShown'); b = acc.index('static bool nvAccelIopAsyncBusy', a)
fixture = r'''
#include <cassert>
#include <cstdio>
typedef int IOReturn; enum { kIOReturnSuccess = 0, kIOReturnBusy = 1, kIOReturnNotReady = 2, kIOReturnIOError = 3 };
''' + acc[a:b] + r'''
int main() {
    int run = 0, strikes = 0;          // the 3-in-a-row rule of nvAccelIopAsyncRun
    IOReturn sleepFlips[3] = { kIOReturnNotReady, kIOReturnNotReady, kIOReturnNotReady };   // display sleep, two heads
    for (IOReturn r : sleepFlips) { if (nvAccelFlipOutcome(r) == kNvFlipRefused) { if (++run >= 3) strikes++; } }
    assert(strikes == 0 && run == 0);                                   // sleeping frames never switch the swap off
    for (int i = 0; i < 3; i++) if (nvAccelFlipOutcome(kIOReturnIOError) == kNvFlipRefused) { if (++run >= 3) strikes++; }
    assert(strikes == 1);                                               // real refusals still do
    assert(nvAccelFlipOutcome(kIOReturnSuccess) == kNvFlipShown && nvAccelFlipOutcome(kIOReturnBusy) == kNvFlipKept);
    puts("PASS: display-sleep flips are dropped without a strike, real refusals still count, shown and busy unchanged");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    r = subprocess.run(['clang++','-std=c++17','-Wall',str(d/'t.cpp'),'-o',str(d/'t')],capture_output=True,text=True)
    if r.returncode: print(r.stderr[-1500:]); raise SystemExit(1)
    subprocess.run([str(d/'t')],check=True)
