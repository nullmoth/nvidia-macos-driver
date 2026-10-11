#!/usr/bin/env python3
"""patch_xdna.py <xdna-driver source root>: AMD's XDNA runtime shim (libxrt_driver_xdna) for macOS x86_64.

The shim is XRT's driver plugin for AMD NPUs. On Linux it finds its device node and attributes through sysfs and
maps buffers by mmap(2) on /dev/accel/accelN; on macOS both go through the accel node's ioctls
(compat-xrt/nmx_xrt_sysfs.hpp). Every edit is an exact anchor with an asserted count. Run once on a pristine tree,
after patch_xrt.py has patched the xrt submodule.
"""
import os
import sys

root = sys.argv[1]


def edit(rel, pairs):
    p = os.path.join(root, rel)
    t = open(p).read()
    for old, new in pairs:
        assert t.count(old) == 1, (rel, old[:70], t.count(old))
        t = t.replace(old, new)
    open(p + ".tmp", "w").write(t)
    os.replace(p + ".tmp", p)


INC = "#ifdef __APPLE__\n# include \"nmx_xrt_sysfs.hpp\"\n#endif\n"
S = "src/shim/"

# the device node of a PCI function: the kernel names it (no sysfs accel/ directory to list)
edit(S + "pcidrv.cpp", [
    ("#include <dirent.h>\n", "#include <dirent.h>\n" + INC),
    ("  const std::string dev_path_dir = dev_sysfs_root(sysfs_name) + \"/\" + sysfs_dev_node_dir();\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    nmx_xrt::node n;\n"
     "    if (nmx_xrt::find(sysfs_name, n))\n"
     "      return n.path;\n"
     "    throw std::invalid_argument(std::string(\"Bad sysfs name: \") + sysfs_name.c_str());\n"
     "  }\n"
     "#endif\n"
     "  const std::string dev_path_dir = dev_sysfs_root(sysfs_name) + \"/\" + sysfs_dev_node_dir();\n"),
])

# the queue model (KMQ, UMQ, PF) is the kernel driver's device_type attribute, read through the node
edit(S + "host/pcidrv_amdxdna.cpp", [
    ("#include <fstream>\n", "#include <fstream>\n" + INC),
    ("  const std::string dev_type_path = shim_xdna::dev_sysfs_root(sysfs) + \"/device_type\";\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    std::string v;\n"
     "    if (nmx_xrt::read_attr(sysfs, \"device_type\", v) != 0)\n"
     "      throw std::invalid_argument(sysfs + \" device_type is missing?\");\n"
     "    return static_cast<int>(std::stoi(v));\n"
     "  }\n"
     "#endif\n"
     "  const std::string dev_type_path = shim_xdna::dev_sysfs_root(sysfs) + \"/device_type\";\n"),
])

# every sysfs query and setting of the shim goes through these two
edit(S + "host/platform_host.cpp", [
    ("#include <sys/ioctl.h>\n", "#include <sys/ioctl.h>\n#include <cstring>\n" + INC),
    ("  std::string path = sysfs_root() + \"/\" + arg.sysfs_node;\n  std::ifstream ifs(path, std::ios::binary);\n",
     "  std::string path = sysfs_root() + \"/\" + arg.sysfs_node;\n"
     "#ifdef __APPLE__\n"
     "  {\n"
     "    std::string v;\n"
     "    int e = nmx_xrt::read_sys(path, v);\n"
     "    if (e)\n"
     "      shim_err(-e, \"Failed to open %s\", path.c_str());\n"
     "    arg.real_size = std::min(v.size(), arg.data.size());\n"
     "    std::memcpy(arg.data.data(), v.data(), arg.real_size);\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  std::ifstream ifs(path, std::ios::binary);\n"),
    ("  std::string path = sysfs_root() + \"/\" + arg.sysfs_node;\n  std::ofstream ofs(path, std::ios::binary);\n",
     "  std::string path = sysfs_root() + \"/\" + arg.sysfs_node;\n"
     "#ifdef __APPLE__\n"
     "  {\n"
     "    int e = nmx_xrt::write_sys(path, arg.data.data(), arg.data.size());\n"
     "    if (e)\n"
     "      shim_err(-e, \"Failed to write %s\", path.c_str());\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  std::ofstream ofs(path, std::ios::binary);\n"),
])

# a PF's instance number is its node's (Linux lists the accel/ directory under the device)
edit(S + "umq/pcidev.cpp", [
    ("#include <memory>\n", "#include <memory>\n" + INC),
    ("  const std::string accel_dir = \"/sys/bus/pci/devices/\" + m_sysfs_name + \"/accel\";\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    nmx_xrt::node n;\n"
     "    if (!nmx_xrt::find(m_sysfs_name, n))\n"
     "      throw std::invalid_argument(\"No accel node for: \" + m_sysfs_name);\n"
     "    m_instance = n.instance;\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  const std::string accel_dir = \"/sys/bus/pci/devices/\" + m_sysfs_name + \"/accel\";\n"),
])

# buffer and doorbell mappings: the node's mmap ioctl (MAP_FIXED included: the shim places buffers in an aligned
# range it reserved, which the kernel replaces atomically)
edit(S + "platform.cpp", [
    ("#include <fcntl.h>\n", "#include <fcntl.h>\n" + INC),
    ("  void* ret = mmap(addr, len, prot, flags, m_dev_fd, offset);\n",
     "#ifdef __APPLE__\n"
     "  void* ret = nmx_xrt::map(m_dev_fd, addr, len, prot, flags, offset);\n"
     "#else\n"
     "  void* ret = mmap(addr, len, prot, flags, m_dev_fd, offset);\n"
     "#endif\n"),
    ("  munmap(addr, len);\n",
     "#ifdef __APPLE__\n"
     "  if (nmx_xrt::unmap(m_dev_fd, addr, len) != 0)\n"
     "    shim_err(-errno, \"munmap(addr=%p, len=%ld) failed\", addr, len);\n"
     "#else\n"
     "  munmap(addr, len);\n"
     "#endif\n"),
])

# macOS's linker rejects GNU -z options (undefined symbols are already errors there)
edit(S + "CMakeLists.txt", [
    ("target_link_options(${XDNA_TARGET} PRIVATE\n  \"-Wl,-z,defs\"\n  )\n",
     "if (NOT APPLE)\ntarget_link_options(${XDNA_TARGET} PRIVATE\n  \"-Wl,-z,defs\"\n  )\nendif()\n"),
])
# packaging: no /etc/os-release on macOS; a plain archive (NullMoth's package installs the files itself)
edit("CMake/pkg.cmake", [
    ('else()\n  message(FATAL_ERROR "Unknown Linux package flavor: ',
     'elseif(APPLE)\n  set(CPACK_GENERATOR "TGZ")\n  set(CPACK_ARCHIVE_COMPONENT_INSTALL ON)\n'
     'else()\n  message(FATAL_ERROR "Unknown Linux package flavor: '),
])
# build settings on macOS: the virtio transport (a Linux guest in a VM talking to a host's NPU over virtgpu) has no
# macOS counterpart; USDT probes need Linux's <sys/sdt.h>; and -Werror stays with the GCC builds upstream tests, since
# clang flags more (missing 'override', non-virtual destructors) - the warnings still print
edit(S + "CMakeLists.txt", [
    ("aux_source_directory(${CMAKE_CURRENT_SOURCE_DIR}/virtio VIRTIO_SOURCES)\n",
     "aux_source_directory(${CMAKE_CURRENT_SOURCE_DIR}/virtio VIRTIO_SOURCES)\nif (APPLE)\n  set(VIRTIO_SOURCES)\nendif()\n"),
    ("  XRT_ENABLE_USDT\n", "  $<$<NOT:$<PLATFORM_ID:Darwin>>:XRT_ENABLE_USDT>\n"),
    ('  "-Werror"\n', "  $<$<NOT:$<PLATFORM_ID:Darwin>>:-Werror>\n"),
])

# the L1 data cache line for userspace cache flushes: Linux sysconf, macOS hw.cachelinesize
edit(S + "pcidev.cpp", [
    ("#include <unistd.h>\n", "#include <unistd.h>\n#ifdef __APPLE__\n#include <sys/sysctl.h>\n#endif\n"),
    ("const long cacheline_size = sysconf(_SC_LEVEL1_DCACHE_LINESIZE);\n",
     "#ifdef __APPLE__\n"
     "const long cacheline_size = [] {\n"
     "  long v = 0;\n"
     "  size_t n = sizeof(v);\n"
     "  // a zero would make the flush loop's line mask wrong, so a failed query keeps x86's 64-byte line\n"
     "  return (sysctlbyname(\"hw.cachelinesize\", &v, &n, nullptr, 0) == 0 && v > 0) ? v : 64L;\n"
     "}();\n"
     "#else\n"
     "const long cacheline_size = sysconf(_SC_LEVEL1_DCACHE_LINESIZE);\n"
     "#endif\n"),
])

# importing a buffer another process exported, by (pid, fd): Linux takes the fd with pidfd_getfd. macOS has no way to
# take a descriptor out of another process, so it fails with ENOTSUP; the buffer's fd can be sent over a UNIX socket
edit(S + "device.cpp", [
    ("  auto pidfd = syscall(SYS_pidfd_open, pid, 0);\n",
     "#ifdef __APPLE__\n"
     "  throw xrt_core::system_error(ENOTSUP, \"importing a buffer from another process by pid is not available on \"\n"
     "    \"macOS (no pidfd_getfd); send the buffer's fd over a UNIX socket instead\");\n"
     "#else\n"
     "  auto pidfd = syscall(SYS_pidfd_open, pid, 0);\n"),
    ("      throw xrt_core::system_error(errno, \"pidfd_getfd failed\");\n    }\n  }\n  return fd;\n",
     "      throw xrt_core::system_error(errno, \"pidfd_getfd failed\");\n    }\n  }\n  return fd;\n#endif\n"),
])
# xrt-smi (examine, validate): Linux installs it from XRT's own xrt-base package, so xdna-driver leaves XRT's tools
# out; macOS has no such package, so the tools are built here
edit("CMake/xrt.cmake", [
    ("  src/runtime_src/core/tools\n  src/runtime_src/core/edge\n", "  src/runtime_src/core/edge\n"),
    ("  tests/validate\n  )\n",
     "  tests/validate\n  )\nif (NOT APPLE)\n  list(APPEND XRT_EXCLUDE_SUB_DIRECTORY src/runtime_src/core/tools)\nendif()\n"),
])
print("xdna patched")
