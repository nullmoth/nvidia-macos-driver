#!/usr/bin/env python3
# Compiles and runs tools/test_tex_upload.c against plugin/nvmtl_tex_upload.h (async texture-upload ordering
# and lifetime decisions). Then proves the decision functions themselves are load-bearing by rebuilding the
# test against a stubbed header whose guards are disabled (before_consume always 0, no finish-first) and
# asserting the test FAILS -- i.e. without the real guards the #96 use-after-free returns.
import pathlib, subprocess, tempfile

root = pathlib.Path(__file__).resolve().parent.parent
test_c = root / 'tools' / 'test_tex_upload.c'
hdr_dir = root / 'plugin'

STUB = r'''#ifndef NVMTL_TEX_UPLOAD_H
#define NVMTL_TEX_UPLOAD_H
#include <stdint.h>
typedef struct { uint64_t infl; } nvmtl_up_state;
typedef enum { NVMTL_UP_SUBMIT_WAIT = 0, NVMTL_UP_SUBMIT_ASYNC = 1 } nvmtl_up_submit;
/* guards DISABLED: never finish-first, consumers never wait -- the reverted #96 behaviour */
static inline nvmtl_up_submit nvmtl_up_before_upload(const nvmtl_up_state *s, int async_flag, uint64_t *finish_first){
    (void)s; *finish_first = 0; return async_flag ? NVMTL_UP_SUBMIT_ASYNC : NVMTL_UP_SUBMIT_WAIT; }
static inline uint64_t nvmtl_up_before_consume(const nvmtl_up_state *s){ (void)s; return 0; }
static inline void nvmtl_up_set(nvmtl_up_state *s, uint64_t i){ if(s) s->infl = i; }
static inline void nvmtl_up_clear(nvmtl_up_state *s){ if(s) s->infl = 0; }
#endif
'''

def build_run(include_dir, expect_ok):
    with tempfile.TemporaryDirectory() as bd:
        exe = pathlib.Path(bd) / 'ttu'
        subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-I', str(include_dir),
                        str(test_c), '-o', str(exe)], check=True)
        rc = subprocess.run([str(exe)]).returncode
        if expect_ok:
            assert rc == 0, 'real header must pass'
        else:
            assert rc != 0, 'stubbed (guard-disabled) header must FAIL the suite'

build_run(hdr_dir, expect_ok=True)
with tempfile.TemporaryDirectory() as d:
    (pathlib.Path(d) / 'nvmtl_tex_upload.h').write_text(STUB)
    build_run(d, expect_ok=False)

print('PASS: real decision logic passes; guard-disabled stub fails (the #96 use-after-free returns without it)')
