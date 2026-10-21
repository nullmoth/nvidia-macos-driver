#!/usr/bin/env python3
"""A binding slot whose handle cannot be a live NVK object never reaches vkUpdateDescriptorSets. NVK reads through every
handle in a descriptor write; WindowServer faulted there on a buffer handle of 0x01006000000748c0 and a sampler handle of
0xa3. The guard's own C is compiled and run on those values and on real handles, and the source is checked for where it
sits: at bind time (a bad handle never enters the table) and at snapshot time (a slot that changed after binding)."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/nvmtl_vk.c').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


m = re.search(r'static int nvmtl_handle_plausible\(const void \*h\)\n\{.*?\n\}\n', src, re.S)
check('the plausibility check exists', m is not None)
if m:
    cases = [('0x01006000000748c0', 0, 'the buffer handle WindowServer faulted on'),
             ('0xa3', 0, 'the sampler handle WindowServer faulted on'),
             ('0x01006000000d4150', 0, 'the second machine\'s buffer handle'),
             ('0xbadbad00badbad00', 0, 'a freed-memory pattern'),
             ('0x6000000748c4', 0, 'a misaligned pointer'),
             ('0x6000000748c0', 1, 'a small-heap object'),
             ('0x600003e5d170', 1, 'a heap object from the same crash report'),
             ('0x7ff74b089e98', 1, 'a stack-range user address'),
             ('0x1000', 0, 'the first pages')]
    body = '#include <stdint.h>\n#include <stdio.h>\n' + m.group(0) + 'int main(void){\n'
    for v, want, _ in cases:
        body += f'  printf("%d\\n", nvmtl_handle_plausible((const void *)(uintptr_t){v}ull));\n'
    body += '  return 0;\n}\n'
    with tempfile.TemporaryDirectory() as d:
        c = pathlib.Path(d) / 'g.c'; exe = pathlib.Path(d) / 'g'
        c.write_text(body)
        r = subprocess.run(['cc', '-O1', '-o', str(exe), str(c)], capture_output=True, text=True)
        check('the guard compiles on its own', r.returncode == 0)
        got = subprocess.run([str(exe)], capture_output=True, text=True).stdout.split() if r.returncode == 0 else []
    for (v, want, what), g in zip(cases, got + [''] * len(cases)):
        check(f'{v} ({what}) reads {"live" if want else "impossible"}', g == str(want))

i0 = src.index('static int nvmtl_bind_slot(')
bind = src[i0:src.index('\n}\n', i0)]
check('bind time: an impossible handle leaves the slot empty before the table is touched',
      'nvmtl_handle_plausible(h)' in bind and bind.find('nvmtl_handle_plausible(h)') < bind.index('if (!c->table)')
      and 'kind = NVMTL_SLOT_NONE; h = NULL; off = 0;' in bind)
check('snapshot time: both descriptor loops read their slot through the check',
      src.count('nvmtl_slot_checked(t ? &t->s[which][i] : NULL, which, i)') == 2)
check('no descriptor loop reads a slot unchecked', 'const nvmtl_slot *sl = t ? &t->s[which][i] : NULL;' not in src)
k = src.find('static const nvmtl_slot *nvmtl_slot_checked(')
chk = src[k:src.index('\n}\n', k)] if k >= 0 else ''
check('a rejected slot returns NULL, so each loop writes its dummy', 'return NULL;' in chk)
check('both rejections are logged, a bounded number of times', src.count('(said up to 8 times)') >= 2)
sys.exit(1 if fails else 0)
