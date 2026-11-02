#!/usr/bin/env python3
"""check_resid_tokens.py - a resource id without a heap slot never looks like one.

A translated shader turns a texture's gpuResourceID into a bindless heap slot by masking the handle's low 17 bits
(reflect::BINDLESS_HEAP_SLOTS = 131072); it has no way to tell a slot-carrying token from any other. So the token the
plugin hands out for a texture that got NO slot (nvmtl_resid_assign with heapSlot 0) must keep its low 17 bits zero:
the shader then reads slot 0, the dummy. A plain sequence number there aliased other textures' slots, and a slot whose
storage descriptor nobody ever wrote, which a kernel then stored through (the first-scene page fault).

This check reads the no-slot token expression in plugin/NVMTLEmbedded.m and fails unless the sequence is shifted
left by at least 17 before the tag is or-ed in.

    python3 tools/check_resid_tokens.py              # exit 1 on a token whose low 17 bits carry the sequence
    python3 tools/check_resid_tokens.py <file.m>     # over another copy (the pre-fix source must fail)
"""
import os, re, sys
here = os.path.dirname(os.path.abspath(__file__))
path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin", "NVMTLEmbedded.m")
src = open(path).read()
m = re.search(r"uint64_t token = heapSlot \? \(NVMTL_RESID_HEAP \| heapSlot\) : \((.*?)\);", src)
if not m: print("FAIL: the no-slot token expression was not found - the check cannot run"); sys.exit(2)
expr = m.group(1)
shift = re.search(r"<<\s*(\d+)", expr)
ok = shift is not None and int(shift.group(1)) >= 17
print("no-slot token expression: %s -> %s" % (expr, "low 17 bits zero" if ok else "LOW BITS CARRY THE SEQUENCE"))
sys.exit(0 if ok else 1)
