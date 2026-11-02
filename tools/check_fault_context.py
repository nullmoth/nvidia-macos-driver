#!/usr/bin/env python3
"""check_fault_context.py - the fault-context rings (plugin/nvmtl_fault_context.h) are wired into the plugin.

A GPU fault names an address and a semaphore number. The rings turn that into a kernel name, the storage views it had
bound, and the resources that died just before, but only if every entry point records: each dispatch path, the storage
bind, the submit, the image death, the heap-slot death, the texture's label, and both device-loss sites dump. This
lists them with the least count each needs and fails on any short.

    python3 tools/check_fault_context.py [plugin-dir]
"""
import re, sys, os
here = os.path.dirname(os.path.abspath(__file__))
pdir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin")
def read(n):
    try: return open(os.path.join(pdir, n)).read()
    except OSError: return ""
vk = read("nvmtl_vk.c"); objc = read("NVMTLObjects.m"); hdr = read("nvmtl_fault_context.h"); vkh = read("nvmtl_vk.h")
hooks = [
    ("nvmtl_vk.c", r"nvmtl_fx_note_dispatch\(c, p\);\n\s*pvkCmdDispatch(Indirect)?\(", 4),   # threads, indirect tg, groups, indirect
    ("nvmtl_vk.c", r"nvmtl_image_grace_reap\(\);", 1),                                       # NVMTL_IMAGE_GRACE: dead images reaped as submits finish
    ("nvmtl_vk.c", r"c->fx_seq = g_cmd_submitted;", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_bind_storage\(&c->fx", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_submit\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_image_gone\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_slot_gone\(&g_fx, g_cmd_submitted", 1),
    ("nvmtl_vk.c", r"nvmtl_fx_dump\(\"", 2),                                              # queue submit failed, fence wait failed
    ("NVMTLObjects.m", r"nvmtl_vk_image_dying_label\(_label\.UTF8String\)", 1),
    ("nvmtl_vk.h", r"nvmtl_fx_cb fx;", 1),
]
src = {"nvmtl_vk.c": vk, "NVMTLObjects.m": objc, "nvmtl_vk.h": vkh}
short = []; total = 0
for f, pat, least in hooks:
    n = len(re.findall(pat, src[f])); total += n
    if n < least: short.append(f"{f}: {pat} found {n}, needs {least}")
if not hdr: short.append("plugin/nvmtl_fault_context.h is missing")
print(f"fault context: {len(hooks)} entry points, {total} sites, {len(short)} short")
for s in short: print("SHORT", s)
sys.exit(1 if short else 0)
