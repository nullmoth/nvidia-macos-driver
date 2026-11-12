#!/usr/bin/env python3
"""A monitor that identifies itself late still gets its own resolutions.

Report: after some restarts a 3440x1440 DisplayPort monitor came up as a 1920x1080 television until the next cold
start. The display link had returned a placeholder 1920x1080 display while the monitor was still waking; the head
reserved scanout memory for that list (1920x1200), and the real EDID that arrived five seconds later no longer fitted.
The reservation now has a floor that does not depend on the first EDID. The rule is compiled from
kexts/NVRMFB/fb/nvrmfb_scanout_reserve.h and run; the kext's use of it is checked in the source.

usage: test_scanout_reserve.py [repo root]"""
import pathlib, platform, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


hdr = root / 'kexts/NVRMFB/fb/nvrmfb_scanout_reserve.h'
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
check('the rule has its own header', hdr.exists())
check('the start-up reservation goes through the rule', 'fAllocSize = nvrmfb_scanout_reserve(fAllocSize, fSize, pitchAlign);' in src)
check('a monitor identified later is measured against the memory really allocated',
      'nvrmfb_scanout_fits(fScalerMaxW, fScalerMaxH, fPitchAlign, fMemBytes)' in src)
check('the old comparison against the first EDID\'s reservation is gone', 'const bool short_ = fAllocSize > keepAlloc;' not in src)
check('a reservation that cannot be allocated still falls back to the boot size',
      'falling back to the boot size' in src and 'fAllocSize = fSize; fScalerMaxW = fW; fScalerMaxH = fH;' in src)

if hdr.exists() and shutil.which('clang') and platform.system() == 'Darwin':
    prog = ('#include <stdio.h>\n#include "%s"\n' % hdr +
            'int main(void) {\n'
            '  const unsigned a = 256;\n'
            '  unsigned long long placeholder = nvrmfb_scanout_bytes(1920, 1200, a), boot = nvrmfb_scanout_bytes(1920, 1080, a);\n'
            '  unsigned long long got = nvrmfb_scanout_reserve(placeholder, boot, a);\n'
            '  printf("%llu %llu ", placeholder, got);\n'
            '  printf("%d %d %d %d ", nvrmfb_scanout_fits(3440, 1440, a, got), nvrmfb_scanout_fits(3440, 1440, a, placeholder),\n'
            '         nvrmfb_scanout_fits(3840, 2160, a, got), nvrmfb_scanout_fits(5120, 2880, a, got));\n'
            '  unsigned long long big = nvrmfb_scanout_bytes(7680, 4320, a);\n'
            '  printf("%d %d %llu %llu %d\\n", nvrmfb_scanout_reserve(big, boot, a) == big, nvrmfb_scanout_reserve(0, big, a) == big,\n'
            '         nvrmfb_scanout_bytes(0, 1080, a), nvrmfb_scanout_bytes(1920, 1080, 3), nvrmfb_scanout_fits(0, 0, a, got));\n'
            '  return 0; }\n')
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d); (d / 't.c').write_text(prog)
        r = subprocess.run(['clang', '-Wall', '-Werror', '-o', str(d / 't'), str(d / 't.c')], capture_output=True, text=True)
        check('the rule compiles without warnings', r.returncode == 0, r.stderr[-300:])
        if r.returncode == 0:
            g = subprocess.run([str(d / 't')], capture_output=True, text=True).stdout.split()
            placeholder, got = int(g[0]), int(g[1])
            check('the placeholder list alone reserved about 9 MB', 9_000_000 < placeholder < 9_500_000, str(placeholder))
            check('the floor reserves a 3840x2160 desktop', got == ((3840 * 4 * 2160 + 0xffff) & ~0xffff), str(got))
            check('the reported 3440x1440 monitor fits in the floor', g[2] == '1')
            check('control: it did not fit in the placeholder reservation', g[3] == '0')
            check('a 3840x2160 monitor fits exactly', g[4] == '1')
            check('a 5120x2880 monitor does not fit and keeps the old ceilings until a restart', g[5] == '0')
            check('a mode list larger than the floor is reserved as before', g[6] == '1')
            check('a boot raster larger than the floor is reserved', g[7] == '1')
            check('a zero dimension or a bad alignment gives no size', g[8] == '0' and g[9] == '0')
            check('a monitor with no size never "fits"', g[10] == '0')
else:
    print('skip the compiled part: needs clang on macOS')
print('test_scanout_reserve: %s' % ('OK' if not fails else f'{fails} FAILED'))
sys.exit(1 if fails else 0)
