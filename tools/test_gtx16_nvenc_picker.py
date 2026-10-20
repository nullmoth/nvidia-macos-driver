#!/usr/bin/env python3
"""Which encoder library a GTX 16 card gets. NVRM names every GTX 16 "Turing GTX 16", so the plain "Turing" match never
reaches it. TU116 reports NVENC class 0xC4B7 (the RTX 20 class) and takes the Turing libraries; TU117 reports no NVENC
class and keeps Apple's encoder. The profile line in field logs: TU116 "nvenc 0xc4b7" on 52 boots, TU117 "nvenc 0x0" on 38."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLDevice.m').read_text()
nvrm = (root / 'kexts/NVRM/NVRM.cpp').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


def body(fn):
    i = src.index('static const char *' + fn + '(void)')
    return src[i:src.index('\n}\n', i)]


check('NVRM names TU116 and TU117 "Turing GTX 16" and publishes the NVENC class',
      '"Turing GTX 16" : "Turing"' in nvrm and 'num("class-nvenc", cenc, 32)' in nvrm)
check('a GTX 16 card qualifies only on NVENC class 0xC4B7',
      re.search(r'nvmtl_gtx16_has_turing_nvenc\(void\)\s*\{\s*return nvmtl_gpu_profile_number\(CFSTR\("class-nvenc"\)\) == 0xC4B7;', src)
      is not None)
for fn, lib in (('nvmtl_nvenc_library_for_this_gpu', 'libnvenc_h264_tu10x.dylib'),
                ('nvmtl_nvenc_hevc_library_for_this_gpu', 'libnvenc_hevc_tu10x.dylib')):
    b = body(fn)
    check(f'{fn}: GTX 16 with the Turing class gets {lib}',
          f'if (!strcmp(gen, "Turing GTX 16") && nvmtl_gtx16_has_turing_nvenc()) return "{lib}";' in b)
    check(f'{fn}: the RTX 20 match stays exact, so it cannot catch a TU117', 'if (!strcmp(gen, "Turing")) return' in b
          and 'strncmp(gen, "Turing"' not in b and 'strstr(gen, "Turing")' not in b)
    check(f'{fn}: anything else falls through to Apple\'s encoder', b.rstrip().endswith('return NULL;'))
sys.exit(1 if fails else 0)
