#!/usr/bin/env python3
"""check_barrier_sites.py - every full barrier an encoder records names its site for the barrier census.

The bar census line attributes each full pipeline barrier to the site that asked for it (first dispatch, later dispatch,
encoder end, owed flush, fence/API, hazard restart, the Vulkan layer's own copies). A call from NVMTLObjects.m that
does not name its site is counted under the layer, so the census would lie about where the barriers come from. This
fails on any untagged encoder-side call and on a missing census line.

    python3 tools/check_barrier_sites.py [plugin-dir]
"""
import re, sys, os
here = os.path.dirname(os.path.abspath(__file__))
pdir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin")
def read(n):
    try: return open(os.path.join(pdir, n)).read()
    except OSError: return ""
objc = read("NVMTLObjects.m"); vk = read("nvmtl_vk.c")
calls = [m.start() for m in re.finditer(r"nvmtl_vk_cmd_barrier\(&", objc)]
untagged = [objc.count("\n", 0, p) + 1 for p in calls if "nvmtl_vk_barrier_site(" not in objc[max(0, p - 140):p]]
census = len(re.findall(r'nvlog_every\("bar census: per frame', objc))
counted = len(re.findall(r"atomic_fetch_add\(&g_bars\[", vk))
print(f"barrier sites: {len(calls)} encoder-side full barriers, {len(untagged)} untagged; census line {census}; layer counters {counted}")
for l in untagged: print("UNTAGGED NVMTLObjects.m line", l)
if not calls: print("FAIL: no encoder-side barrier call found - the check cannot run"); sys.exit(2)
sys.exit(1 if untagged or census != 1 or counted < 5 else 0)
