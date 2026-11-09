#!/usr/bin/env python3
"""A refresh-rate change keeps the frame on screen (nvfbkeepfront=1, off by default): issues 153, 161, 302 item 1.

Reported: after WindowServer commits a new refresh rate the panel is black or shows only the cursor, the screenshot shows the full
desktop, and anything that makes WindowServer present a new frame brings it back. The mode set attached the empty scanout surface;
the desktop lives in the accelerator's zero-copy surface. The decision is compiled from nvrm-fb-safety.h; the switchMode wiring is
checked in the source. Control: rel-29 always attached the scanout surface."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['PE_parse_boot_argn("nvfbkeepfront"', 'lr->config.surface = shown;', 'fFrontSurf = shown;', 'nvrmKeepFrontSurface(keepArg != 0']:
    assert need in src, need
assert 'lr->config.surface = ns; lr->config.srcWidth = (NvU16)w;' not in src.split('IOReturn NVRMNVDAFramebuffer::switchMode')[1].split('IOReturn NVRMNVDAFramebuffer::validateDetailedTiming')[0]
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
int main() {
    // 3840x2160 60 -> 120 Hz: same raster, front surface is the accelerator's: keep it
    assert(nvrmKeepFrontSurface(true, true, false, 3840, 2160, 3840, 2160, 0, 0));
    assert(!nvrmKeepFrontSurface(false, true, false, 3840, 2160, 3840, 2160, 0, 0));    // switch off: old behaviour
    assert(!nvrmKeepFrontSurface(true, false, false, 3840, 2160, 3840, 2160, 0, 0));    // nothing presented yet
    assert(!nvrmKeepFrontSurface(true, true, true, 3840, 2160, 3840, 2160, 0, 0));      // the front is the scanout surface already
    assert(!nvrmKeepFrontSurface(true, true, false, 3840, 2160, 2560, 1440, 0, 0));     // a different size: WindowServer redraws
    assert(!nvrmKeepFrontSurface(true, true, false, 3840, 2160, 3840, 2160, 0, 0x400)); // a rotation change
    puts("PASS: front surface carried through same-size mode changes only, off by default");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
