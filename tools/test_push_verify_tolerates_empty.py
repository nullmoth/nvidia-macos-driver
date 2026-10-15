#!/usr/bin/env python3
"""A zero-count non-immediate push method header is a hardware no-op; it must not abort the app.

The shipped NVK is built -Dbuildtype=debugoptimized (build/build263.sh), so assert() is live. __push_verify()
in src/nouveau/headers/nv_push.h asserts that the previous non-immediate method header carried a nonzero data
count. An empty method (count 0) is benign to the GPU, but the assert turned it into a hard abort: the highest
machine-count NVK crash, under vk_meta_copy_memory_to_image / vk_meta_draw_volume (Chess, mediaanalysisd, video
apps). The fix in nvk/nvk-macos.patch downgrades it to a warn-once.

This extracts BOTH the pre-fix (removed, '-') and post-fix (added, '+') __push_verify bodies from the patch hunk,
compiles each with assertions enabled, and runs four header scenarios. The post-fix build must tolerate the empty
method; the pre-fix build is the negative control and must SIGABRT on exactly that scenario.
"""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
patch = (root / 'nvk/nvk-macos.patch').read_text().splitlines()

# locate the nv_push.h hunk appended for this fix
start = next(i for i, l in enumerate(patch) if l.startswith('diff --git a/src/nouveau/headers/nv_push.h'))
hunk = []
for l in patch[start + 1:]:
    if l.startswith('diff ') or l.startswith('Index: '):
        break
    hunk.append(l)
body = hunk[hunk.index(next(l for l in hunk if l.startswith('@@'))) + 1:]
old_body = [l[1:] for l in body if l and l[0] in ' -' and not l.startswith('---')]
new_body = [l[1:] for l in body if l and l[0] in ' +' and not l.startswith('+++')]
assert any('assert(last_count)' in l for l in old_body), 'pre-fix body not found in hunk'
assert any('warned = 1' in l and True for l in new_body) and any('last_count == 0' in l for l in new_body), \
    'post-fix warn-once not found in hunk'

# the hunk's context begins at the immd check; prepend the stable signature + null/immd guard
PREFIX = '''static void nvk__push_verify(struct nv_push *push)
{
   if (push->last_hdr == NULL)
      return;
'''
FIXTURE = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define ASSERTED
struct nv_push { uint32_t *last_hdr; uint32_t last_hdr_dw; };
/* method header layout: SEC_OP in bits 31:29 (IMMD == 4), METHOD_COUNT in bits 28:16 */
#define NVDEF_TEST(v, a, b, field, op, name) ((((v) >> 29) & 0x7) op 4)
#define NVVAL_GET(v, a, b, field)            (((v) >> 16) & 0x1fff)
'''
MAIN = r'''
int main(int argc, char **argv) {
    struct nv_push p; uint32_t slot = 0; p.last_hdr = &slot;
    const char *s = argc > 1 ? argv[1] : "";
    if      (!strcmp(s, "null"))    { p.last_hdr = 0;          p.last_hdr_dw = 0x20000000; }
    else if (!strcmp(s, "immd"))    { p.last_hdr_dw = 0x80000000u; }        /* SEC_OP = 4 */
    else if (!strcmp(s, "nonzero")) { p.last_hdr_dw = 0x20010000u; }        /* incr, count = 1 */
    else                            { p.last_hdr_dw = 0x20000000u; }        /* incr, count = 0 (empty) */
    nvk__push_verify(&p);
    fputs("OK\n", stdout);
    return 0;
}
'''


def build(tmp, name, fn_body_lines):
    src = FIXTURE + PREFIX + '\n'.join(fn_body_lines) + '\n' + MAIN
    (tmp / (name + '.c')).write_text(src)
    subprocess.run(['clang', '-std=c11', '-UNDEBUG', str(tmp / (name + '.c')), '-o', str(tmp / name)], check=True)


def run(tmp, name, scenario):
    r = subprocess.run([str(tmp / name), scenario], capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


fails = 0
with tempfile.TemporaryDirectory() as d:
    tmp = pathlib.Path(d)
    build(tmp, 'newv', new_body)      # the delivered fix
    build(tmp, 'oldv', old_body)      # negative control

    for sc in ('null', 'immd', 'nonzero', 'empty'):
        rc, out = run(tmp, 'newv', sc)
        ok = rc == 0 and out == 'OK'
        print(('ok   ' if ok else 'FAIL ') + f'new tolerates {sc} (rc={rc})')
        fails += not ok

    for sc in ('null', 'immd', 'nonzero'):
        rc, out = run(tmp, 'oldv', sc)
        ok = rc == 0 and out == 'OK'
        print(('ok   ' if ok else 'FAIL ') + f'old accepts {sc} (rc={rc})')
        fails += not ok

    rc, out = run(tmp, 'oldv', 'empty')   # the bug: pre-fix aborts the whole app on an empty method
    ok = rc != 0 and out != 'OK'
    print(('ok   ' if ok else 'FAIL ') + f'NEGATIVE CONTROL: old SIGABRTs on empty method (rc={rc})')
    fails += not ok

print('PASS' if not fails else f'{fails} FAILED')
sys.exit(1 if fails else 0)
