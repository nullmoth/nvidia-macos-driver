#!/usr/bin/env python3
"""Variable refresh (adaptive sync) as IOGraphics models it: timing-range flag, per-mode blanking extension, attribute.

Apple's public IOGraphics types define the contract: kIORangeSupportsVRR in IODisplayTimingRange.supportedSyncFlags,
IODetailedTimingInformationV2.verticalBlankingExtension (lines a frame can be stretched by; 0 = none) and the
kIOFBVariableRefreshRate attribute. The EDID parser and the extension arithmetic are compiled from nvrm-fb-safety.h; the
call sites are checked in nvrm-fb.cpp. Off unless boot-arg nvfbvrr=1. The control is the old driver, which advertised none
of it, so a VRR-capable display was driven at a fixed rate."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['kIORangeSupportsVRR', 'verticalBlankingExtension = nvrmVrrBlankExtension', 'attribute == kIOFBVariableRefreshRate) return setVrr',
             'vh->modeSetConfig.vrrEnabled = NV_TRUE;', 'if (vh->flags.modeChanged || vh->flags.activeChanged) vh->flags.vrrEnabledChanged = NV_TRUE', 'PE_parse_boot_argn("nvfbvrr"', 'r->vrr = dd->vrrSupported']:
    assert need in src, 'missing: ' + need
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include "nvrm-fb-safety.h"
static void range(unsigned char *e, unsigned lo, unsigned hi, unsigned char flags) {
    memset(e, 0, 128); unsigned char *d = e + 54 + 18;       // second descriptor slot
    d[3] = 0xFD; d[4] = flags; d[5] = lo; d[6] = hi;
}
int main() {
    unsigned char e[128]; unsigned lo = 0, hi = 0;
    range(e, 48, 165, 0);
    assert(nvrmEdidVerticalRange(e, 128, &lo, &hi) && lo == 48 && hi == 165);
    range(e, 48, 100, 2); assert(nvrmEdidVerticalRange(e, 128, &lo, &hi) && hi == 355);        // +255 Hz max-rate offset (a 355 Hz panel)
    range(e, 30, 60, 3);  assert(nvrmEdidVerticalRange(e, 128, &lo, &hi) && lo == 285 && hi == 315);
    memset(e, 0, 128); assert(!nvrmEdidVerticalRange(e, 128, &lo, &hi));                         // no range descriptor
    range(e, 90, 60, 0); assert(!nvrmEdidVerticalRange(e, 128, &lo, &hi));                       // nonsense range refused
    assert(!nvrmEdidVerticalRange(nullptr, 128, &lo, &hi) && !nvrmEdidVerticalRange(e, 64, &lo, &hi));
    // 1080p144 (1125 lines) on a 48-165 Hz panel can stretch to 48 Hz: 1125*144/48 = 3375 lines, 2250 extra
    assert(nvrmVrrBlankExtension(1125, 144, 48, 165) == 2250);
    assert(nvrmVrrBlankExtension(1125, 60, 48, 165) == 281);
    assert(nvrmVrrBlankExtension(1125, 30, 48, 165) == 0);     // below the range: no extension
    assert(nvrmVrrBlankExtension(1125, 200, 48, 165) == 0);    // above it
    assert(nvrmVrrBlankExtension(1125, 144, 0, 0) == 0);       // no range known
    // OLD DRIVER (control): nothing advertised -> every mode had extension 0
    puts("PASS: EDID range parsed with offsets, refused when absent or inverted, blanking extension per mode in range only");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
