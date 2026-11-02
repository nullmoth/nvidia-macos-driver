#!/usr/bin/env python3
"""check_heap_rewrite.py - a heap slot that is rewritten after an image migration also gets its storage descriptor back.

A texture that holds a bindless heap slot has two descriptors in that slot: the sampled view at binding 0 and, when the
texture can be written by a kernel, its storage view at binding 2 (plugin/nvmtl_vk.c, nvmtl_vk_bindless_put_storage).
When residency moves the texture between VRAM and system memory (nvmtl_res2_move) the VkImage is re-created and the
old one is freed, so every descriptor that named the old view must be rewritten. The move used to rewrite binding 0
only; a kernel writing through binding 2 then stored into freed memory. This check lists every call of
nvmtl_vk_bindless_rewrite in plugin/NVMTLObjects.m and fails unless nvmtl_vk_bindless_put_storage is called within the
next three lines of each.

    python3 tools/check_heap_rewrite.py               # exit 1 when a rewrite has no storage rewrite beside it
    python3 tools/check_heap_rewrite.py <file.m>      # the same over another copy of the source (the pre-fix one must fail)
"""
import os, sys
here = os.path.dirname(os.path.abspath(__file__))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin", "NVMTLObjects.m")
src = open(path).read().split("\n")
sites = [n for n, line in enumerate(src) if "nvmtl_vk_bindless_rewrite(" in line and not line.strip().startswith("//")]
bad = [n + 1 for n in sites if not any("nvmtl_vk_bindless_put_storage(" in src[k] for k in range(n, min(len(src), n + 4)))]
print("heap slot rewrites: %d; without a storage rewrite beside them: %d" % (len(sites), len(bad)))
for n in bad: print("MISSING storage rewrite after line %d: %s" % (n, src[n - 1].strip()[:120]))
if not sites: print("FAIL: no nvmtl_vk_bindless_rewrite call found - the check cannot run"); sys.exit(2)
sys.exit(1 if bad else 0)
