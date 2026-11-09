#!/usr/bin/env python3
"""The AMD/Intel video decoder registration in the plugin (nvmtl_nmxvt_register): off by default, only for the AMD and Intel
vendors, never in WindowServer, only a root-owned library, and the NVIDIA registration is untouched.
  usage: test_nmxvt_register.py [--old]   (--old checks the plugin of the revision before this change)
"""
import pathlib, subprocess, sys

root = pathlib.Path(__file__).resolve().parents[1]
bad = 0


def check(name, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name)


src = (subprocess.run(["git", "-C", str(root), "show", "rel-29:plugin/NVMTLDevice.m"], capture_output=True, text=True).stdout
       if "--old" in sys.argv else (root / "plugin/NVMTLDevice.m").read_text())
i = src.find("static void nvmtl_nmxvt_register(void)")
fn = src[i:src.find("\n}\n", i)] if i >= 0 else ""
check("a registration function for the AMD/Intel decoders exists", bool(fn))
check("it does nothing unless NMXVT=1 or nmxvt=1 is set (off by default)", 'getenv("NMXVT")' in fn and '"nmxvt=1"' in fn and "if (!on) return;" in fn)
check("it stays out of WindowServer", 'strcmp(getprogname(), "WindowServer")' in fn)
check("it loads only a root-owned library inside a root-owned directory", "nvmtl_loadable_root_only(path)" in fn)
check("the library is looked for beside the plugin, then in the AMD/Intel package's video directory", "/Library/NullMoth/gpu-userland/video/libnmxvt.dylib" in fn)
check("it registers at rating 1100, above the system's own decoders", "r(1100)" in fn)
check("it is called only for the AMD (0x1002) and Intel (0x8086) vendors, before the NVIDIA registrations",
      "if (nvmtl_vk_vendor() == 0x1002 || nvmtl_vk_vendor() == 0x8086) nvmtl_nmxvt_register();" in src)
check("the NVIDIA codec registrations are unchanged and still NVIDIA-only",
      "if (nvmtl_vk_vendor() == 0x10de) {   /* NVDEC/NVENC are NVIDIA engines reached through NVRM */\n        nvmtl_nvdec_register();" in src)
if "--old" in sys.argv:
    print(f"old revision: {'FAILED' if bad else 'all passed'} ({bad} failing)")
sys.exit(1 if bad else 0)
