#!/usr/bin/env python3
# Compiles and runs tools/test_eld.c against kexts/NVRMFB/nvrm_eld.h, then proves the suite is meaningful by running
# it a second time against a baseline-only stub builder (which ignores the EDID's CEA block, as the driver did before
# this change) and asserting that stub FAILS. See test_eld.c for the per-case negative controls.
import pathlib, subprocess, tempfile

root = pathlib.Path(__file__).resolve().parent.parent
test_c = root / 'tools' / 'test_eld.c'
hdr_dir = root / 'kexts' / 'NVRMFB'

STUB = r'''#ifndef NVRM_ELD_H
#define NVRM_ELD_H
#include <stdint.h>
#include <stddef.h>
enum { NVRM_ELD_CONN_HDMI = 0, NVRM_ELD_CONN_DP = 1 };
#define NVRM_ELD_MAX 128
enum { NVRM_CEA_TAG_AUDIO = 1, NVRM_CEA_TAG_SPEAKER = 4, NVRM_CEA_TAG_VENDOR = 3 };
/* baseline-only: ignores the CEA block entirely -- the driver's behaviour before nvrm_eld.h existed */
static inline size_t nvrm_build_eld(const uint8_t *e, size_t n, int conn, uint8_t *o){
    if(!e||!o||n<128) return 0;
    for(int i=0;i<20;i++) o[i]=0;
    o[0]=(2u<<3); o[2]=4; o[4]=(1u<<5); o[5]=(uint8_t)((conn&3)<<2); return 20;
}
#endif
'''

def run(include_dir, expect_ok):
    with tempfile.TemporaryDirectory() as bd:
        exe = pathlib.Path(bd) / 'eld_test'
        subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-I', str(include_dir),
                        str(test_c), '-o', str(exe)], check=True)
        rc = subprocess.run([str(exe)]).returncode
    if expect_ok:
        assert rc == 0, 'the real builder must pass all assertions'
    else:
        assert rc != 0, 'the baseline-only stub must FAIL (negative control for the whole suite)'

# 1. the real header must pass
run(hdr_dir, expect_ok=True)

# 2. negative control: a baseline-only stub must fail
with tempfile.TemporaryDirectory() as d:
    (pathlib.Path(d) / 'nvrm_eld.h').write_text(STUB)
    run(d, expect_ok=False)

print('PASS: nvrm_build_eld passes every case; the baseline-only stub fails the suite as the negative control')
