#!/usr/bin/env python3
# patch_zink_no_moltenvk.py <mesa tree>: let zink build on this platform without the MoltenVK headers.
# Upstream zink assumes that its Vulkan driver here is MoltenVK: the build stops without -Dmoltenvk-dir, and three
# sources include its headers. Our Vulkan driver is NVK, so with no moltenvk-dir the build now defines
# ZINK_NO_MOLTENVK and those includes are skipped. Every MoltenVK workaround in zink is already inside
# "#if defined(MVK_VERSION)", a macro only those headers define, so none of them is compiled.
# The define goes to the C and the C++ sources: the generated instance header is included by both.
# Anchors are exact strings with asserted counts; a tree that does not match is left untouched.
import os, sys
T = sys.argv[1]; Z = os.path.join(T, "src/gallium/drivers/zink")
EDITS = {
    "meson.build": [("  else\n    error('moltenvk-dir is required but not set.')\n  endif\n",
                     "  else\n    # no MoltenVK: the Vulkan driver underneath is not a Metal translation layer\n    zink_c_args += ['-DZINK_NO_MOLTENVK']\n    zink_cpp_args += ['-DZINK_NO_MOLTENVK']\n  endif\n", 1),
                    ("zink_c_args = []\n", "zink_c_args = []\nzink_cpp_args = []\n", 1),
                    ("  c_args: zink_c_args,\n)", "  c_args: zink_c_args,\n  cpp_args: zink_cpp_args,\n)", 1)],
    "zink_instance.py": [("#ifdef __APPLE__\n#include \"MoltenVK/mvk_vulkan.h\"", "#if defined(__APPLE__) && !defined(ZINK_NO_MOLTENVK)\n#include \"MoltenVK/mvk_vulkan.h\"", 1)],
    "zink_resource.c": [("#ifdef __APPLE__\n#include \"MoltenVK/mvk_vulkan.h\"", "#if defined(__APPLE__) && !defined(ZINK_NO_MOLTENVK)\n#include \"MoltenVK/mvk_vulkan.h\"", 1)],
    "zink_screen.c": [("#ifdef __APPLE__\n#include \"MoltenVK/mvk_vulkan.h\"", "#if defined(__APPLE__) && !defined(ZINK_NO_MOLTENVK)\n#include \"MoltenVK/mvk_vulkan.h\"", 1)],
}
new = {}
for f, edits in EDITS.items():
    p = os.path.join(Z, f); s = open(p + ".orig" if os.path.exists(p + ".orig") else p).read()  # always from the pristine copy
    for old, rep, n in edits:
        if s.count(old) != n: sys.exit(f"STOP: {f}: anchor found {s.count(old)} times, expected {n}; nothing was changed")
        s = s.replace(old, rep)
    new[p] = s
for p, s in new.items():
    if not os.path.exists(p + ".orig"): os.replace(p, p + ".orig")
    tmp = p + ".tmp"; open(tmp, "w").write(s); os.replace(tmp, p)
print(f"patched {len(new)} files under {Z}")
