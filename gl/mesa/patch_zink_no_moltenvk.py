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
    "zink_screen.c": [("#ifdef __APPLE__\n#include \"MoltenVK/mvk_vulkan.h\"", "#if defined(__APPLE__) && !defined(ZINK_NO_MOLTENVK)\n#include \"MoltenVK/mvk_vulkan.h\"", 1),
        # Without MoltenVK there is no Vulkan loader on the system either: zink opens the Vulkan driver itself, from
        # the folder of the image that contains zink, and enters it through the driver's own entry point.
        ("#define VK_LIBNAME \"@rpath/libvulkan.1.dylib\"\n",
         "#ifdef ZINK_NO_MOLTENVK\n#define VK_LIBNAME \"@loader_path/libvulkan_nouveau.dylib\"\n#define ZINK_DIRECT_DRIVER 1\n#else\n#define VK_LIBNAME \"@rpath/libvulkan.1.dylib\"\n#endif\n", 1),
        ("   screen->vk_GetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)util_dl_get_proc_address(screen->loader_lib, \"vkGetInstanceProcAddr\");\n   screen->vk_GetDeviceProcAddr = (PFN_vkGetDeviceProcAddr)util_dl_get_proc_address(screen->loader_lib, \"vkGetDeviceProcAddr\");\n   if (!screen->vk_GetInstanceProcAddr ||\n       !screen->vk_GetDeviceProcAddr) {\n",
         "#ifdef ZINK_DIRECT_DRIVER\n   /* a driver exports its instance entry point under the loader-interface name; the device entry point is asked\n    * of the instance once it exists */\n   screen->vk_GetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)util_dl_get_proc_address(screen->loader_lib, \"vk_icdGetInstanceProcAddr\");\n   screen->vk_GetDeviceProcAddr = NULL;\n   if (!screen->vk_GetInstanceProcAddr) {\n#else\n   screen->vk_GetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)util_dl_get_proc_address(screen->loader_lib, \"vkGetInstanceProcAddr\");\n   screen->vk_GetDeviceProcAddr = (PFN_vkGetDeviceProcAddr)util_dl_get_proc_address(screen->loader_lib, \"vkGetDeviceProcAddr\");\n   if (!screen->vk_GetInstanceProcAddr ||\n       !screen->vk_GetDeviceProcAddr) {\n#endif\n", 1),
        ("   screen->instance = instance;\n",
         "   screen->instance = instance;\n#ifdef ZINK_DIRECT_DRIVER\n   screen->vk_GetDeviceProcAddr = (PFN_vkGetDeviceProcAddr)screen->vk_GetInstanceProcAddr(instance, \"vkGetDeviceProcAddr\");\n   if (!screen->vk_GetDeviceProcAddr) {\n      mesa_loge(\"ZINK: the driver has no device entry point\");\n      goto fail;\n   }\n#endif\n", 1)],
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
