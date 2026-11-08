#!/usr/bin/env python3
"""check_rehome_views.py - a texture whose memory can be re-homed owns no view of the image being replaced.

Two paths give a texture a new VkImage under the same NVMTLTexture: the res2 pager (nvmtl_res2_move ->
nvmtl_vk_image_migrate) and the purge reclaim (nvmtl_vk_image_reback). Both rebuild the image's own view (_i.view) and
the bindless slot, and nothing else. Views the texture made for itself stay bound to the old image, which is freed:
  _swzView           MTLTextureDescriptor.swizzle; every sampled bind and the gpuResourceID slot use it
  _ownView           a typed or ranged view made at creation
  _sampledDepthView  the depth-only view of a combined depth/stencil image
After a move, sampling such a texture reads freed memory through the stale view, and the slot rewrite drops the
swizzle. So each eligibility test must refuse a texture that holds any of them.

    python3 tools/check_rehome_views.py                     # exit 1 when a test lets one through
    python3 tools/check_rehome_views.py <NVMTLObjects.m>    # over another copy (the source before this check must fail)
"""
import os, re, sys
here = os.path.dirname(os.path.abspath(__file__))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin", "NVMTLObjects.m")
src = open(path).read()
VIEWS = ("_swzView", "_ownView", "_sampledDepthView")
TESTS = ("nvmtl_res2_movable", "nvmtl_purge_eligible")
fails = 0
for fn in TESTS:
    m = re.search(r"static BOOL " + fn + r"\(NVMTLTexture \*t\) \{(.*?)\n\}", src, re.S)
    if not m:
        print(f"FAIL  {fn}: not found in {path} - the check cannot run"); fails += 1; continue
    body = m.group(1)
    missing = [v for v in VIEWS if not re.search(r"!\s*t->" + v + r"\b", body)]
    ok = not missing
    fails += not ok
    print(("ok  " if ok else "FAIL") + f"  {fn} refuses a texture holding its own view"
          + (f" (lets through: {', '.join(missing)})" if missing else f" ({len(VIEWS)} of {len(VIEWS)})"))
print(f"{fails} failed"); sys.exit(1 if fails else 0)
