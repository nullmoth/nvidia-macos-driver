#!/usr/bin/env python3
"""check_dealloc_order.py - a dying texture gives its heap slot back before any view it names is destroyed.

A heap slot's descriptor element stores the index of the texture's VkImageView in NVK's descriptor table; a destroyed
view's index goes to the next view created. So -[NVMTLTexture dealloc] must call nvmtl_resid_texture_gone (the slot
goes back to the dummies) BEFORE it destroys any image view or buffer view: _sampledDepthView, the texel views,
_ownView, _swzView and the image itself. This check reads that dealloc and fails if any destroy call precedes the drop.

    python3 tools/check_dealloc_order.py              # exit 1 when a destroy comes before the slot drop
    python3 tools/check_dealloc_order.py <file.m>     # over another copy (the pre-fix source must fail)
"""
import os, re, sys
here = os.path.dirname(os.path.abspath(__file__))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin", "NVMTLObjects.m")
src = open(path).read().split("\n")
# the texture dealloc: the one that drops a resource id and destroys _ownView
start = None
for n, line in enumerate(src):
    if line.startswith("- (void)dealloc {") and any("nvmtl_resid_texture_gone(_resid)" in l for l in src[n:n + 25]) \
            and any("_ownView" in l for l in src[n:n + 25]):
        start = n; break
if start is None: print("FAIL: the texture dealloc was not found - the check cannot run"); sys.exit(2)
body = src[start:start + 25]
drop = next((i for i, l in enumerate(body) if "nvmtl_resid_texture_gone(_resid)" in l), None)
destroys = [(i, l.strip()[:90]) for i, l in enumerate(body)
            if re.search(r"nvmtl_vk_(image_view_destroy|texel_view_destroy|image_destroy)\(", l) and not l.strip().startswith("//")]
early = [(i, l) for i, l in destroys if drop is None or i < drop]
print("texture dealloc at line %d: slot drop at +%s, %d destroy calls, %d before the drop" % (start + 1, drop, len(destroys), len(early)))
for i, l in early: print("EARLY DESTROY +%d: %s" % (i, l))
sys.exit(1 if early else 0)
