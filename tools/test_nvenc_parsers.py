#!/usr/bin/env python3
"""The vendored video bitstream parsers (vendor/nvenc/src) must survive bitstreams written by someone hostile.

They run inside every app that decodes video through VideoToolbox, on bytes from a web page or a file. Each arm feeds a
parser a stream built (or found by a coverage-guided fuzzer) to reach one index, size or loop, and checks that it is
refused instead of reading or writing outside its arrays, overflowing a size or spinning for seconds. The harness is built
with AddressSanitizer and UndefinedBehaviorSanitizer and stops on the first report, so an out-of-range access is a
failure, not luck. The headers from open-gpu-doc (nvdec_drv.h) are looked for in $NVDEC_HEADERS or the build headers
folder; with none the test says so and exits 2 (not a pass)."""
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

def arr(b):
    return ','.join(str(x) for x in b)

# list 0 of size 0 predicts from "delta" lists back: pred_mode_flag 0, then ue(delta). 0xBFFFFFFF is negative as an int.
scaling_hostile = bits('0', ue_bits(0xBFFFFFFF))
# found by libFuzzer: inter reference picture sets whose lists pass 32 entries before the old check ran
hevc_rps_overflow = bytes([0,215,0,0,0,0,0,0,121,121,121,121,121,121,121,121,121,121,121,121,121,3,121,0,0,0,0,250,192,192,0,0,253,255,94,59,232,255,40,118,255,40,118,252,1,0,0,0,252,214,214,250,255,255,253,1,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,10,10,1])
# found by libFuzzer: an HRD whose cpb count is 2^28 on a stream that has already ended
hevc_hrd_spin = bytes([83,83,115,83,37,83,83,83,199,48,10,182,83,182,182,0,83,83,199,48,10,182,80,0,0,0,37,73,182,83,0,0,182,37,73,182,83,0,0,182,65,255,13,92,255,23,0,0,23,23,255,255,255,255,255,255,255,255,255,255,255,0,0,0,0,0,0,0,0,255,0,37,73,182,71,0,0,32,75])
# an id that is negative as an int
hevc_pps_bad_id = bytes([68,1,0,0,0,1,128,0,0,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0])
hevc_sps_bad_id = bytes([66,1,1,1,0,0,0,0,0,0,0,0,0,0,93,0,0,0,1,128,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0])
# 2^28 macroblocks wide: 16 * width overflowed a signed int
h264_sps_huge = bytes([103,66,0,10,218,0,0,0,15,255,255,241,228])

common = r'''
#include <stdio.h>
#include <time.h>
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }
'''
hevc_prog = r'''
#include "hevcparse.c"
static const unsigned char SCAL[] = {@SCAL@};
static const unsigned char RPS[] = {@RPS@};
static const unsigned char HRD[] = {@HRD@};
static const unsigned char PPSID[] = {@PPSID@};
static const unsigned char SPSID[] = {@SPSID@};
typedef struct { unsigned char before[256]; HevcParams p; unsigned char after[256]; } Fenced;
int main(void) {
    { Br r; memset(&r, 0, sizeof r); memcpy(r.buf, SCAL, sizeof SCAL); r.n = sizeof SCAL;
      HevcScaling sl; hevc_default_scaling(&sl); scaling_list_data(&r, &sl, 1);
      arm("a scaling-list delta that wraps negative as an int is refused, not used as an index", r.err); }
    static Fenced f; memset(&f, 0xA5, sizeof f); memset(&f.p, 0, sizeof f.p);
    arm("an HEVC SPS whose inter reference picture sets overrun the 32-entry lists is refused", hevc_parse_sps(&f.p, RPS, sizeof RPS) < 0);
    double t0 = now(); int rc = hevc_parse_sps(&f.p, HRD, sizeof HRD); double dt = now() - t0;
    arm("an HEVC SPS whose HRD claims 2^28 buffers is refused at once (not after seconds of spinning)", rc < 0 && dt < 1.0);
    arm("an HEVC PPS whose id is negative as an int is refused", hevc_parse_pps(&f.p, PPSID, sizeof PPSID) < 0);
    arm("an HEVC SPS whose id is negative as an int is refused", hevc_parse_sps(&f.p, SPSID, sizeof SPSID) < 0);
    int fence = 1; for (size_t i = 0; i < sizeof f.before; i++) if (f.before[i] != 0xA5 || f.after[i] != 0xA5) fence = 0;
    arm("none of those wrote outside the parameter sets", fence);
    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}
'''
h264_prog = r'''
#include "h264parse.c"
static const unsigned char H264[] = {@H264@};
int main(void) {
    static H264Params hp; memset(&hp, 0, sizeof hp);
    arm("an H.264 SPS 2^28 macroblocks wide is refused (the size arithmetic would overflow)", h264_parse_sps(&hp, H264, sizeof H264) < 0);
    printf("%s: %d failed\n", fails ? "FAIL" : "PASS", fails);
    return fails ? 1 : 0;
}
'''
for k, v in (('SCAL', scaling_hostile), ('RPS', hevc_rps_overflow), ('HRD', hevc_hrd_spin), ('PPSID', hevc_pps_bad_id),
             ('SPSID', hevc_sps_bad_id), ('H264', h264_sps_huge)):
    hevc_prog = hevc_prog.replace('@%s@' % k, arr(v)); h264_prog = h264_prog.replace('@%s@' % k, arr(v))
dirs = [os.environ.get('NVDEC_HEADERS', ''), str(pathlib.Path.home() / 'nvencvt-headers/ogd/classes/video')]
hdr = next((d for d in dirs if d and (pathlib.Path(d) / 'nvdec_drv.h').exists()), None)
if not hdr:
    print('SKIP: nvdec_drv.h (open-gpu-doc classes/video) not found; set NVDEC_HEADERS'); sys.exit(2)
bad = 0
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    for name, prog in (('hevc', hevc_prog), ('h264', h264_prog)):
        (d / (name + '.c')).write_text(common + prog)
        subprocess.run(['clang', '-g', '-O1', '-fsanitize=address,undefined', '-fno-sanitize-recover=all', f'-I{src}', f'-I{hdr}',
                        str(d / (name + '.c')), '-o', str(d / name)], check=True)
        r = subprocess.run([str(d / name)], capture_output=True, text=True)
        print(r.stdout, end='')
        if r.returncode:
            print(r.stderr[:800]); bad = 1
print('FAIL' if bad else 'PASS')
sys.exit(bad)
