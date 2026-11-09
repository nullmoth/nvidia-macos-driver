#!/usr/bin/env python3
"""DSC hints are added per connector type, not on pixel clock alone.

The mode-publication path marked every mode above 25.92 Gbit/s uncompressed as DSC. That limit is DisplayPort 1.4's;
an HDMI 2.1 link carries 42.67 Gbit/s uncompressed, so a 4K@120 HDMI mode got DSC fields the link never uses and
macOS planned a different pipe than NVKMS drove (dual-screen flicker, issue 77 class). The real helper is compiled
from nvrm-fb-safety.h; the negative control is the old fixed limit."""
import pathlib, re, subprocess, tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert 'nvrmDscThresholdBps(' in src, 'addKmsRefreshModes no longer asks the connector for its DSC limit'
assert '25920000000ull && !b.dscCompressedBitsPerPixel' not in src, 'the fixed DP limit is still applied to every connector'

fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
static bool dsc(unsigned long long pclk, unsigned type, bool internal) {
    unsigned long long lim = nvrmDscThresholdBps(type, internal);
    return lim && pclk * 24 > lim;
}
int main() {
    const unsigned long long k4k120 = 1188000000ull, k4k240 = 2376000000ull, k4k60 = 594000000ull;
    assert( dsc(k4k120, 0x400, false));   // DisplayPort 4K120 needs DSC
    assert(!dsc(k4k120, 0x800, false));   // HDMI 2.1 FRL carries 4K120 uncompressed: no DSC fields
    assert( dsc(k4k240, 0x800, false));   // 4K240 over HDMI does need it
    assert(!dsc(k4k60, 0x400, false));
    assert( dsc(k4k120, 0, true));        // eDP panel follows the DP limit
    assert(!dsc(k4k120, 0, false));       // unknown connector: no hint
    // OLD RULE (negative control): fixed 25.92 Gbit/s flags the HDMI mode
    assert(k4k120 * 24 > 25920000000ull);
    puts("PASS: DP and eDP get DSC above 25.92 Gbit/s, HDMI only above 42.67, unknown connectors never");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
