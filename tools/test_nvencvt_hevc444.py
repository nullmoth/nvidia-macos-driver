#!/usr/bin/env python3
"""The Screen Sharing host sets the encoder's ProfileLevel to HEVC_Main444_AutoLevel before it will offer high-performance
mode. NVEncVT could already produce 4:4:4 from a 4:4:4 input; with NVMTL_NVENC_HEVC_444=1 it now also names the profile.
Source checks (the build and export comparison are done by build/build_nvencvt.sh).

usage: test_nvencvt_hevc444.py"""
import pathlib, sys

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'vendor/nvenc/vt/nvencvt.c').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


check('the profile strings are the ones macOS spells', 'CFSTR("HEVC_Main444_AutoLevel")' in src and 'CFSTR("HEVC_Main44410_AutoLevel")' in src)
check('the profiles are listed only when NVMTL_NVENC_HEVC_444=1', 'if (hevc444_enabled()) { CFArrayAppendValue(v, NV_PROFILE_MAIN444)' in src
      and 'e[0] == \'1\' && !e[1]' in src)
check('the switch is read from the environment and is off when unset', 'getenv("NVMTL_NVENC_HEVC_444")' in src and 'return e && e[0]' in src)
check('naming the profile selects 4:4:4 input formats only, so VideoToolbox converts anything else', 'if (s->want444 && !s->av1) {' in src
      and src.count('kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange') >= 3)
check('the 10-bit 4:4:4 profile selects 10-bit', 'bool ten = CFEqual(v, kVTProfileLevel_HEVC_Main10_AutoLevel) || CFEqual(v, NV_PROFILE_MAIN44410);' in src)
check('the profile reads back as the one asked for', 'ProfileLevel) && s->hevc && s->want444) v = CFRetain(s->ten ? NV_PROFILE_MAIN44410 : NV_PROFILE_MAIN444)' in src)
check('a frame that is not 4:4:4 under a 4:4:4 profile is refused', '(s->want444 && !s->c444) ||' in src)
sys.exit(1 if fails else 0)
