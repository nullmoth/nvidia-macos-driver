#!/usr/bin/env python3
"""check_hazard_hooks.py - every entry point of the hazard model (plugin/nvmtl_hazard.h) is wired into the plugin.

The model only sees what the encoders tell it: a bind, a draw, a dispatch, a pass begin and end, a blit, a clear, a
barrier, a used resource or heap. A hook left out does not fail to compile; it makes the model blind to that command,
and a blind spot is a skipped barrier, which is a wrong pixel or a GPU fault. This lists each hook and the least number
of call sites it must have in the plugin sources, taken from the tree the model was measured in, and fails on any short.

    python3 tools/check_hazard_hooks.py [plugin-dir]     # exit 1 when a hook is missing
"""
import re, sys, os
here = os.path.dirname(os.path.abspath(__file__))
pdir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "plugin")
def read(name):
    try: return open(os.path.join(pdir, name)).read()
    except OSError: return ""
objc = read("NVMTLObjects.m"); vk = read("nvmtl_vk.c"); hdr = read("nvmtl_hazard.h"); dev = read("NVMTLDevice.m")
# hook: (source, pattern, least count)
hooks = [
    ("nvmtl_vk.c", r"#include \"nvmtl_hazard.h\"", 1),
    ("nvmtl_vk.c", r"\bhz_pass_decide\(", 1), ("nvmtl_vk.c", r"\bhz_pass_enter\(", 1), ("nvmtl_vk.c", r"\bnvmtl_hz_begin_rendering\(", 1),
    ("nvmtl_vk.c", r"\bnvmtl_hz_pass_exit\(", 1), ("nvmtl_vk.c", r"\bnvmtl_hz_slot_bound\(", 1),
    ("nvmtl_vk.c", r"\bnvmtl_hz_pend_res\(", 1), ("nvmtl_vk.c", r"\bnvmtl_hz_pend_clear\(", 1),
    ("nvmtl_vk.c", r"\bnvmtl_hz_clean_src\(", 1), ("nvmtl_vk.c", r"\bnvmtl_hz_full_barrier\(", 1),
    ("nvmtl_vk.c", r"\bnvmtl_hz_forget\(", 1), ("nvmtl_vk.c", r"HZS_CLEAR", 1),
    ("nvmtl_vk.c", r"pvkCmdEndRendering\(c->cb\)", 1), ("nvmtl_vk.c", r"\bg_hz_dynr = ", 1),
    ("nvmtl_vk.c", r"out->hz_off = ", 4),   # placed colour, placed depth, buffer-backed, pooled images
    ("NVMTLObjects.m", r"\bnvmtl_hz_use_heap\(", 1), ("NVMTLObjects.m", r"_hzLegacy = YES", 3),
    ("NVMTLObjects.m", r"\bnvmtl_hz_draw\(", 1), ("NVMTLObjects.m", r"\bnvmtl_hz_dispatch\(", 1),
    ("NVMTLObjects.m", r"\bnvmtl_hz_encoder_end\(", 2),   # render and compute
    ("NVMTLObjects.m", r"\bnvmtl_hz_render_writes\(", 1), ("NVMTLObjects.m", r"\bnvmtl_hz_compute_writes\(", 1),
    ("NVMTLObjects.m", r"\bnvmtl_hz_bind_buffer\(", 7),    # vertex, fragment, bytes; compute nil, buffer, bytes x2
    ("NVMTLObjects.m", r"\bnvmtl_hz_bind_texture\(", 3),   # vertex, fragment, compute
    ("NVMTLObjects.m", r"\bnvmtl_hz_bind_storage\(", 3), ("NVMTLObjects.m", r"\bnvmtl_hz_bind_texel\(", 3),
    ("NVMTLObjects.m", r"\bnvmtl_hz_use\(_cb", 6),        # render x4, compute x2
    ("NVMTLObjects.m", r"NVMTL_HZ_BLIT\(", 9),             # every blit command
    ("NVMTLObjects.m", r"\[self nvmtlHzForceFull\]", 5),  # fetch pipeline, patch, ICB, two indirect draws
    ("NVMTLObjects.m", r"\bnvmtlHzRestart:", 4),
    ("NVMTLObjects.m", r"\bnvmtl_hz_shadow\((?!0\))", 3),   # owed flush, compute dispatch, compute end (the shadow going on)
    ("NVMTLObjects.m", r"_hzAttSkip \|= ", 2), ("NVMTLObjects.m", r"hz_force_full = _hzFull", 1),
    ("NVMTLObjects.m", r"nvmtl_hz_declare_buffer\(&_cb->_c, &b->_b, 0, 1\)", 1),   # the index buffer of a draw
    ("NVMTLObjects.m", r"hazard census \(", 1),
    ("NVMTLObjects.m", r"nvmtl_hz_dispatch_decides\(\)\) kind = 2", 1),   # NVMTL_HAZARD_DISPATCH: later dispatches decided by the model
    ("NVMTLDevice.m", r"nvmtl_pipeline_key_log\(", 4),                       # vertex, fragment, kernel + the definition
]
src = {"NVMTLObjects.m": objc, "nvmtl_vk.c": vk, "NVMTLDevice.m": dev}
short = []; total = 0
for f, pat, least in hooks:
    n = len(re.findall(pat, src[f])); total += n
    if n < least: short.append(f"{f}: {pat} found {n}, needs {least}")
if not hdr: short.append("plugin/nvmtl_hazard.h is missing")
print(f"hazard hooks: {len(hooks)} entry points checked, {total} call sites, {len(short)} short")
for s in short: print("SHORT", s)
sys.exit(1 if short else 0)
