#!/usr/bin/env python3
"""Which encoder library a GTX 16 card gets. NVRM names every GTX 16 "Turing GTX 16", so an exact "Turing" match never
reaches it. Both GTX 16 chips take the Turing libraries: on a GTX 1650 (TU117) H.264 and HEVC encoded 5 of 5 frames with
no errors through them, although that card's profile lists no encoder class. Generations match on their first word."""
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


check('NVRM names TU116 and TU117 "Turing GTX 16"', '"Turing GTX 16" : "Turing"' in nvrm)
m = re.search(r'static bool nvmtl_gen_is\(const char \*gen, const char \*name\)\s*\{(.*?)\n\}', src, re.S)
check('generations match on their first word', m is not None and "gen[n] == ' '" in m.group(1) and "gen[n] == '\\0'" in m.group(1))
check('no encoder-class gate keeps a TU117 on Apple\'s encoder', 'class-nvenc' not in src)
for fn, lib in (('nvmtl_nvenc_library_for_this_gpu', 'libnvenc_h264_tu10x.dylib'),
                ('nvmtl_nvenc_hevc_library_for_this_gpu', 'libnvenc_hevc_tu10x.dylib')):
    b = body(fn)
    check(f'{fn}: "Turing" and "Turing GTX 16" both get {lib}', f'if (nvmtl_gen_is(gen, "Turing")) return "{lib}";' in b)
    check(f'{fn}: no exact compare left on a generation', 'strcmp(gen' not in b)
    check(f'{fn}: a generation with no library (Ada) falls through to Apple\'s encoder', b.rstrip().endswith('return NULL;')
          and '"Ada"' not in b)
sys.exit(1 if fails else 0)
