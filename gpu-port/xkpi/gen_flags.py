# gen_flags.py <drm-kmod> <out>: amdgpu compile flags for the macOS port, read from drm-kmod's own Makefiles so they track
# upstream. KCONFIG is drm-kmod's amd64 set minus the features that must not be built on macOS.
import re, sys, pathlib
dk = pathlib.Path(sys.argv[1]); out = pathlib.Path(sys.argv[2])
EXCLUDE = {
    "DRM_PANIC", "DRM_PANIC_FOREGROUND_COLOR", "DRM_PANIC_BACKGROUND_COLOR", "DRM_PANIC_SCREEN", "DRM_DRAW",  # Apple's panic screen
    "DEBUG_FS", "PROC_FS",                          # no debugfs/procfs on macOS
    "COMPAT",                                       # 32-bit ioctls: macOS 15 runs 64-bit processes only
    "FB", "DRM_FBDEV_EMULATION", "DRM_FBDEV_OVERALLOC", "DRM_CLIENT_DEFAULT_FBDEV", "DRM_CLIENT_DEFAULT",
    "DRM_CLIENT", "DRM_CLIENT_SETUP", "VIDEO_CMDLINE",  # in-kernel fbdev clients: macOS uses IOFramebuffer
    "DRM_MIPI_DSI", "DRM_PANEL_ORIENTATION_QUIRKS", "DRM_PANEL_BACKLIGHT_QUIRKS", "DRM_DISPLAY_DP_TUNNEL",
}
txt = (dk / "kconfig.mk").read_text()
# amd64 branch, FBDEV branch dropped (excluded anyway)
items = []
for m in re.finditer(r"KCONFIG\+?=\s*((?:[^\n]*\\\n)*[^\n]*)", txt):
    body = m.group(1).replace("\\\n", " ")
    items += [t for t in re.findall(r"[A-Z0-9_]+(?:=(?:'[^']*'|\"(?:\\.|[^\"])*\"|[^\s]+))?", body)]
skip_arch = {"ARM64", "PPC64", "RISCV", "AGP"}
flags = []
seen = set()
for it in items:
    name = it.split("=")[0]
    if name in EXCLUDE or name in skip_arch or name in seen: continue
    seen.add(name)
    # values are written for a shell (DRM_I915_FORCE_PROBE='"*"'); cc.sh passes flags as words, so drop the quoting
    if "='" in it and it.endswith("'"):
        it = it.replace("='", "=", 1)[:-1]
    flags.append("-DCONFIG_" + it)
mk = (dk / "amd/amdgpu/Makefile").read_text()
src = dk / "drivers/gpu/drm/amd"
incs = []
for rel in re.findall(r"CFLAGS\+=\s*-I\$\{SRCDIR\}(/[^\s]*)?\n", mk):
    incs.append("-I" + str(src) + rel)
incs.append("-I" + str(dk / "drivers/gpu/drm/scheduler"))
flags += ['-DKBUILD_MODNAME="amdgpu"', "-DLINUXKPI_PARAM_PREFIX=amdgpu_", "-DDRM_SYSCTL_PARAM_PREFIX=_amdgpu"]
out.write_text("\n".join(flags + incs) + "\n")
print(len([f for f in flags if f.startswith("-DCONFIG_")]), "CONFIG defines,", len(incs), "include dirs ->", out)
