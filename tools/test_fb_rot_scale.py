#!/usr/bin/env python3
"""Rotated heads can offer other resolutions (nvfbrotscale=1, off by default): issue 138 / report C12.

Why the refusal existed (git log c4f3ec5): not hardware. WindowServer composites the turned desktop into raster-oriented frames in
its GPU pass, and CoreDisplay's own scaled-mode generator only makes mirror-only rotated products, so a rotated head offers just
the driver's own modes, each with its desktop equal to the raster turned. addRotatedDesktops() is lifted from nvrm-fb.cpp and run on
a 90-degree rotated 2560x1600 panel; the validator rule is compiled from nvrm-fb-safety.h. Control: with the switch off nothing
is added and a smaller rotated desktop is refused, as in rel-29."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert '!nvrmRotatedDesktopOk(gRotScale, d->horizontalScaled' in src and 'n = addRotatedDesktops(n);' in src
a = src.index('static void dtRasterTurned'); turned = src[a:src.index('\n}\n', a) + 3]
b = src.index('unsigned NVRMNVDAFramebuffer::addRotatedDesktops'); fn = src[b:src.index('\n}\n', b) + 3]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
using NvU32 = uint32_t; using UInt64 = uint64_t; using UInt32 = uint32_t;
enum { kIOScaleSwapAxes = 1, kIOScaleInvertX = 2, kIOScaleInvertY = 4, kIOScaleRotateFlags = 7 };
#define FBLOG(...) ((void)0)
#include "nvrm-fb-safety.h"
struct IODetailedTimingInformationV2 {
    UInt64 pixelClock = 0; NvU32 horizontalActive = 0, verticalActive = 0, horizontalBlanking = 0, verticalBlanking = 0,
    horizontalScaled = 0, verticalScaled = 0, scalerFlags = 0, detailedTimingModeID = 0;
};
static bool gRotScale = true;
''' + turned + r'''
struct NVRMNVDAFramebuffer {
    IODetailedTimingInformationV2 fDT[64]; unsigned fIndex = 0; UInt32 fCurTransform = 0;
    unsigned addRotatedDesktops(unsigned n);
};
''' + fn + r'''
int main() {
    NVRMNVDAFramebuffer f; f.fCurTransform = kIOScaleSwapAxes | kIOScaleInvertX;      // rotated 90 degrees
    IODetailedTimingInformationV2 &d = f.fDT[0];
    d.pixelClock = 268800000; d.horizontalActive = 2560; d.verticalActive = 1600; d.horizontalBlanking = 160; d.verticalBlanking = 44;
    d.scalerFlags = f.fCurTransform; d.horizontalScaled = 1600; d.verticalScaled = 2560;   // the raster turned
    unsigned n = f.addRotatedDesktops(1);
    assert(n == 4);                                          // 80%, 2/3 and 50% (1280x800 turned = 800x1280 is above the 640x480 floor)
    assert(f.fDT[1].horizontalScaled == 1280 && f.fDT[1].verticalScaled == 2048);
    assert(f.fDT[3].horizontalScaled == 800 && f.fDT[3].verticalScaled == 1280);
    assert((f.fDT[1].detailedTimingModeID >> 16) == 3 && (f.fDT[1].detailedTimingModeID & 0xfff) == 60);
    assert(f.fDT[1].scalerFlags == f.fCurTransform && f.fDT[1].horizontalActive == 2560);          // same raster, same rotation
    assert(f.addRotatedDesktops(n) == n);                    // idempotent
    NVRMNVDAFramebuffer flat; flat.fDT[0] = d; flat.fCurTransform = 0; assert(flat.addRotatedDesktops(1) == 1);   // not rotated: nothing
    gRotScale = false; NVRMNVDAFramebuffer off; off.fDT[0] = d; off.fCurTransform = f.fCurTransform; assert(off.addRotatedDesktops(1) == 1);   // switch off
    // the validator rule
    assert(nvrmRotatedDesktopOk(false, 1600, 2560, 1600, 2560));            // native turned desktop: always
    assert(!nvrmRotatedDesktopOk(false, 1280, 2048, 1600, 2560));           // rel-29: refused
    assert(nvrmRotatedDesktopOk(true, 1280, 2048, 1600, 2560));
    assert(!nvrmRotatedDesktopOk(true, 1700, 2048, 1600, 2560) && !nvrmRotatedDesktopOk(true, 1281, 2048, 1600, 2560) && !nvrmRotatedDesktopOk(true, 0, 0, 1600, 2560));
    puts("PASS: rotated head offers smaller turned desktops with the switch on, nothing with it off");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    r = subprocess.run(['clang++','-std=c++17','-Wall',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],capture_output=True,text=True)
    if r.returncode: print(r.stderr[-2000:]); raise SystemExit(1)
    subprocess.run([str(d/'t')],check=True)
