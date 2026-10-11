# patch_npu_umd.py <linux-npu-driver tree>: Intel's NPU user driver (Level Zero) on macOS x86_64.
# Each edit is an exact anchor with an asserted count; files are rewritten atomically. The driver's own code paths are
# changed in one place only (its OS interface), so the rest of Intel's tree stays as published.
import os, sys
root = sys.argv[1]

def edit(rel, pairs):
    p = os.path.join(root, rel)
    t = open(p).read()
    for old, new in pairs:
        assert t.count(old) == 1, (rel, old[:60], t.count(old))
        t = t.replace(old, new)
    open(p + ".tmp", "w").write(t)
    os.replace(p + ".tmp", p)

# ELF-only hardening flags: Mach-O has no stack-clash pass in clang and no -z options; ld64 always binds relocations
# read-only after load and never makes the stack executable, which is what -z relro/now/noexecstack ask for.
edit("cmake/compiler_flags.cmake", [
    ("  $<$<CONFIG:Release>:-fstack-clash-protection>\n",
     "  $<$<AND:$<CONFIG:Release>,$<NOT:$<PLATFORM_ID:Darwin>>>:-fstack-clash-protection>\n"),
    ("add_link_options(\n", "if(NOT APPLE)\nadd_link_options(\n"),
    ('  "SHELL:$<$<CONFIG:Release>:-z noexecstack>"\n)\n', '  "SHELL:$<$<CONFIG:Release>:-z noexecstack>"\n)\nendif()\n'),
])
# -Werror stays on Linux. On macOS uint64_t is "unsigned long long" (Linux: "unsigned long"), so Intel's %lu formats
# warn there although the widths are identical; and two of Linux's distinct 64-bit types are one type here, which
# trips the "same type" assertion in safe_cast (a style check, not a correctness one).
edit("cmake/compiler_flags.cmake", [
    ("  -Wall -Wextra -Werror -Wno-unused-parameter", "  -Wall -Wextra $<$<NOT:$<PLATFORM_ID:Darwin>>:-Werror> -Wno-unused-parameter"),
])
edit("umd/vpu_driver/include/umd_common.hpp", [
    ('    static_assert(!std::is_same_v<To, From>, "safe_cast is not required for same type");\n',
     '#ifndef __APPLE__\n    static_assert(!std::is_same_v<To, From>, "safe_cast is not required for same type");\n#endif\n'),
])
# size_t and uint64_t are distinct 64-bit types on macOS: the compiler-library calls (VCL, loaded only when a compiler
# library is installed) take uint64_t *, and std::min needs one type
edit("umd/level_zero_driver/source/ext/compiler.cpp", [
    ("                                                                           &graphSize,\n", "                                                                           reinterpret_cast<uint64_t *>(&graphSize),\n"),
    ("                                                                           &graphSize));\n", "                                                                           reinterpret_cast<uint64_t *>(&graphSize)));\n"),
    ("    return vclToL0Err(Vcl::sym().queryNetwork(query, pSupportedLayers, pSize));\n",
     "    return vclToL0Err(Vcl::sym().queryNetwork(query, pSupportedLayers, reinterpret_cast<uint64_t *>(pSize)));\n"),
    ("    ret = vclToL0Err(Vcl::sym().getCompilerSupportedOptions(compiler, pSupportedOptions, pSize));\n",
     "    ret = vclToL0Err(Vcl::sym().getCompilerSupportedOptions(compiler, pSupportedOptions, reinterpret_cast<uint64_t *>(pSize)));\n"),
])
edit("umd/level_zero_driver/source/metric_streamer.cpp", [
    ("    *pRawDataSize = std::min(*pRawDataSize, maxReportCount * sampleSize);\n",
     "    *pRawDataSize = std::min<size_t>(*pRawDataSize, maxReportCount * sampleSize);\n"),
])
# ld64 takes an exported-symbols list where GNU ld takes a version script (compat/ze.exported_symbols: _ze*)
edit("umd/level_zero_driver/api/CMakeLists.txt", [
    ("target_link_options(${TARGET_NAME_L0} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/ze.exports)\n",
     "if(APPLE)\n  target_link_options(${TARGET_NAME_L0} PRIVATE -Wl,-exported_symbols_list,${NMX_COMPAT_DIR}/ze.exported_symbols)\n"
     "else()\n  target_link_options(${TARGET_NAME_L0} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/ze.exports)\nendif()\n"),
])
# The one functional change: mapping NPU buffers. A macOS kext cannot implement mmap(2) on a device node, so the port
# maps a buffer with NMX_DRM_IOCTL_MMAP at the same offset mmap would take and unmaps it with NMX_DRM_IOCTL_MUNMAP
# (compat/nmx_drm_uapi.h, identical to the kext's). munmap has no fd, so each mapping remembers the fd it came from.
edit("umd/vpu_driver/source/os_interface/os_interface_imp.cpp", [
    ("void *OsInterfaceImp::osiMmap(void *addr, size_t size, int prot, int flags, int fd, off_t offset) {\n"
     "    return mmap(addr, size, prot, flags, fd, offset);\n}\n\n"
     "int OsInterfaceImp::osiMunmap(void *addr, size_t size) {\n    return munmap(addr, size);\n}\n",
     "#ifdef __APPLE__\n"
     "} // namespace VPU\n"
     "#include <map>\n#include <mutex>\n#include \"nmx_drm_uapi.h\"\n"
     "namespace VPU {\n"
     "static std::mutex nmxMapLock;\n"
     "static std::map<uintptr_t, int> nmxMaps;\n"
     "#endif\n\n"
     "void *OsInterfaceImp::osiMmap(void *addr, size_t size, int prot, int flags, int fd, off_t offset) {\n"
     "#ifdef __APPLE__\n"
     "    if (fd >= 0 && !(flags & MAP_ANONYMOUS)) {\n"
     "        struct nmx_drm_mmap req = {static_cast<uint64_t>(offset), size, static_cast<uint32_t>(prot), 0, 0};\n"
     "        if (ioctl(fd, NMX_DRM_IOCTL_MMAP, &req) != 0)\n"
     "            return MAP_FAILED;\n"
     "        std::lock_guard<std::mutex> lock(nmxMapLock);\n"
     "        nmxMaps[static_cast<uintptr_t>(req.addr)] = fd;\n"
     "        return reinterpret_cast<void *>(req.addr);\n"
     "    }\n"
     "#endif\n"
     "    return mmap(addr, size, prot, flags, fd, offset);\n}\n\n"
     "int OsInterfaceImp::osiMunmap(void *addr, size_t size) {\n"
     "#ifdef __APPLE__\n"
     "    {\n"
     "        std::lock_guard<std::mutex> lock(nmxMapLock);\n"
     "        auto it = nmxMaps.find(reinterpret_cast<uintptr_t>(addr));\n"
     "        if (it != nmxMaps.end()) {\n"
     "            struct nmx_drm_munmap req = {reinterpret_cast<uintptr_t>(addr), size};\n"
     "            int fd = it->second;\n"
     "            nmxMaps.erase(it);\n"
     "            return ioctl(fd, NMX_DRM_IOCTL_MUNMAP, &req);\n"
     "        }\n"
     "    }\n"
     "#endif\n"
     "    return munmap(addr, size);\n}\n"),
])
# the driver compiler beside the UMD: Linux libopenvino_intel_npu_compiler_loader.so, macOS .dylib
edit("umd/level_zero_driver/source/ext/vcl_symbols.hpp", [
    ('                                               "libopenvino_intel_npu_compiler_loader.so"};\n',
     '#ifdef __APPLE__\n'
     '                                               "libopenvino_intel_npu_compiler_loader.dylib"};\n'
     '#else\n'
     '                                               "libopenvino_intel_npu_compiler_loader.so"};\n'
     '#endif\n'),
])
print("npu umd patched")
