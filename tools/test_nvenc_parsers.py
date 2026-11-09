#!/usr/bin/env python3
"""The vendored video bitstream parsers (vendor/nvenc/src) must survive bitstreams written by someone hostile.

They run inside every app that decodes video through VideoToolbox, on bytes from a web page or a file. Each arm feeds a
parser a stream built to reach one index and checks that it refuses it (error flag set) instead of reading outside its
arrays; the harness is built with AddressSanitizer and UndefinedBehaviorSanitizer so an out-of-range index is a failure,
not luck. The headers from open-gpu-doc (nvdec_drv.h) are looked for in $NVDEC_HEADERS or the build headers folder; with
none the test says so and exits 2 (not a pass)."""
import os, pathlib, subprocess, sys, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = root / 'vendor/nvenc/src'

def bits(*parts):
    s = ''.join(parts)
    s += '0' * (-len(s) % 8)
    return bytes(int(s[i:i + 8], 2) for i in range(0, len(s), 8))

def ue_bits(v):
    z = (v + 1).bit_length() - 1
    return '0' * z + format(v + 1, 'b')

# list 0 of size 0 predicts from "delta" lists back: pred_mode_flag 0, then ue(delta). 0xBFFFFFFF is negative as an int.
hostile = bits('0', ue_bits(0xBFFFFFFF))
harness = r'''
#include "hevcparse.c"
#include <stdio.h>
static int run(const unsigned char *b, size_t n, int expect_err) {
    Br r; memset(&r, 0, sizeof r); memcpy(r.buf, b, n); r.n = n;
    HevcScaling sl; hevc_default_scaling(&sl);
    scaling_list_data(&r, &sl, 1);
    return (int)r.err == expect_err;
}
int main(void) {
    const unsigned char h[] = {%s};
    int ok = 1;
    ok &= run(h, sizeof h, 1) ? 1 : (puts("FAIL a scaling-list delta that wraps negative is refused"), 0);
    puts(ok ? "ok   a scaling-list delta that wraps negative as an int is refused, not used as an index" : "FAIL");
    return ok ? 0 : 1;
}
''' % ','.join(str(x) for x in hostile)
dirs = [os.environ.get('NVDEC_HEADERS', ''), str(pathlib.Path.home() / 'nvencvt-headers/ogd/classes/video')]
hdr = next((d for d in dirs if d and (pathlib.Path(d) / 'nvdec_drv.h').exists()), None)
if not hdr:
    print('SKIP: nvdec_drv.h (open-gpu-doc classes/video) not found; set NVDEC_HEADERS'); sys.exit(2)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / 't.c').write_text(harness)
    subprocess.run(['clang', '-g', '-O1', '-fsanitize=address,undefined', '-fno-sanitize-recover=all', f'-I{src}', f'-I{hdr}',
                    str(d / 't.c'), '-o', str(d / 't')], check=True)
    r = subprocess.run([str(d / 't')], capture_output=True, text=True)
    print(r.stdout, end='')
    if r.returncode:
        print(r.stderr[:600]); print('FAIL'); sys.exit(1)
print('PASS')
