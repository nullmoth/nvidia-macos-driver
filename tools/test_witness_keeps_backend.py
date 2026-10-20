#!/usr/bin/env python3
"""A missing completion witness after the fence has signalled must not switch the Vulkan backend off for the process.
Issue 322: terminals printing fast froze for good after "completion witness missing; disabling backend" - every later
command buffer came back nil and CoreAnimation waited forever. The command buffer's pool and pages are leaked instead,
and after a few lost witnesses pool reuse stops for the process."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/nvmtl_vk.c').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


i = src.index('int nvmtl_vk_submit_finish(nvmtl_inflight *f)')
fin = src[i:src.index('\n}\n', i)]
w = fin.index('if (c->wseq && c->wseq != NVMTL_WSEQ_BLIND)')
wit = fin[w:fin.index('c->timing.host_after_ns', w)]
check('a lost witness after the fence does not set g_state', 'g_state' not in wit)
check('a lost witness does not return from submit_finish', 'return' not in wit)
check('a lost witness marks the command buffer LOST so retire leaks its pool', 'c->wseq = NVMTL_WSEQ_LOST;' in wit)
check('the lost-witness line passes the release log filter ("missing")', 'missing after its fence signalled' in wit)
v = src.index('static int nvmtl_keep_verdict(uint32_t s)')
verdict = src[v:src.index('\n}\n', v)]
check('the verdict answers LOST at once, without another wait', 'if (s == NVMTL_WSEQ_LOST) return -1;' in verdict
      and verdict.index('NVMTL_WSEQ_LOST') < verdict.index('wait_ms'))
check('sequence numbers never reach the LOST or BLIND markers', 'while (s == 0 || s >= NVMTL_WSEQ_LOST);' in src)
k = src.index('static int nvmtl_keep_on(void)')
check('pool reuse stops once the process has lost NVMTL_WIT_LOST_MAX witnesses',
      'if (__atomic_load_n(&g_keep_retired, __ATOMIC_ACQUIRE)) return 0;' in src[k:k + 400]
      and re.search(r'n == NVMTL_WIT_LOST_MAX && !__atomic_exchange_n\(&g_keep_retired', wit) is not None)
sys.exit(1 if fails else 0)
