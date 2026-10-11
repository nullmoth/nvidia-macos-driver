# patch_xrt.py <xrt/src>: AMD's XRT (NPU mode) on macOS x86_64, for Ryzen AI NPUs driven by NMXNPU (amdxdna).
# Anchors exact and asserted; files rewritten atomically. Build-system changes first; source changes follow as found.
import os, sys
root = sys.argv[1]

def edit(rel, pairs):
    p = os.path.join(root, rel)
    t = open(p).read()
    for old, new in pairs:
        assert t.count(old) == 1, (rel, old[:70], t.count(old))
        t = t.replace(old, new)
    open(p + ".tmp", "w").write(t)
    os.replace(p + ".tmp", p)

# no /etc/os-release on macOS: the "flavor" is macos, the version the OS release (packaging only reads them)
edit("CMake/nativeLnx.cmake", [
    ('execute_process(\n  COMMAND awk -F= "$1==\\"ID\\" {print $2}" /etc/os-release\n',
     'if (APPLE)\n  set(LINUX_FLAVOR "macos")\n  execute_process(COMMAND sw_vers -productVersion OUTPUT_VARIABLE LINUX_VERSION OUTPUT_STRIP_TRAILING_WHITESPACE)\nelse()\n'
     'execute_process(\n  COMMAND awk -F= "$1==\\"ID\\" {print $2}" /etc/os-release\n'),
])
p = os.path.join(root, "CMake/nativeLnx.cmake")
t = open(p).read()
# close the APPLE branch after the version is read (the block ends at the first endif() after the else/VERSION_ID read)
anchor = '    COMMAND awk -F= "$1==\\"VERSION_ID\\" {print $2}" /etc/os-release\n'
i = t.index(anchor)
j = t.index("endif()\n", i) + len("endif()\n")
t = t[:j] + "endif(APPLE)\n" + t[j:]
open(p + ".tmp", "w").write(t)
os.replace(p + ".tmp", p)

# xclbinutil is a host tool for building FPGA images (needs RapidJSON); the NPU runtime does not use it. ld64 has no
# -Bsymbolic or -z defs (undefined symbols are already errors when linking a dylib on macOS).
edit("runtime_src/CMakeLists.txt", [
    ("xrt_add_subdirectory(tools/xclbinutil)\nxrt_add_subdirectory(xocl)\nxrt_add_subdirectory(xrt)\n",
     "if (NOT APPLE)\nxrt_add_subdirectory(tools/xclbinutil)\nendif()\nxrt_add_subdirectory(xocl)\nxrt_add_subdirectory(xrt)\n"),
    ('set (CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-Bsymbolic")\nset (CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-z,defs")\n',
     'if (NOT APPLE)\nset (CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-Bsymbolic")\nset (CMAKE_SHARED_LINKER_FLAGS "${CMAKE_SHARED_LINKER_FLAGS} -Wl,-z,defs")\nendif()\n'),
])
# Python bindings: built against the host Python, which on the build machine is not the x86_64 one apps would use
edit("python/CMakeLists.txt", [("add_subdirectory(pybind11)\n", "if (NOT APPLE)\nadd_subdirectory(pybind11)\nendif()\n")])
# macOS has the userland these Linux branches use (POSIX, <uuid/uuid.h>, visibility attributes); the kernel-only
# branches (__KERNEL__) stay Linux-only
I = "runtime_src/core/include/"
edit(I + "xrt/detail/config.h", [("#ifdef __linux__\n# ifdef XRT_API_SOURCE\n", "#if defined(__linux__) || defined(__APPLE__)\n# ifdef XRT_API_SOURCE\n")])
edit(I + "xrt/detail/ert.h", [("#ifdef __linux__\n", "#if defined(__linux__) || defined(__APPLE__)\n")])
edit(I + "xrt/detail/xclbin.h", [
    ("#if defined(__linux__)\n", "#if defined(__linux__) || defined(__APPLE__)\n"),
    ("#elif defined(__linux__) && !defined(__KERNEL__)\n", "#elif (defined(__linux__) || defined(__APPLE__)) && !defined(__KERNEL__)\n"),
])
edit(I + "xrt/deprecated/xrt.h", [("#ifdef __linux__\n# define XCL_DRIVER_DLLESPEC", "#if defined(__linux__) || defined(__APPLE__)\n# define XCL_DRIVER_DLLESPEC")])
# the NPU runtime needs neither the OpenCL FPGA runtime (xocl, built on the Linux OpenCL ICD) nor the profiling
# plugins (xdp, loaded only when profiling is asked for)
edit("runtime_src/CMakeLists.txt", [
    ("endif()\nxrt_add_subdirectory(xocl)\nxrt_add_subdirectory(xrt)\n", "xrt_add_subdirectory(xocl)\nendif()\nxrt_add_subdirectory(xrt)\n"),
    ("\nxrt_add_subdirectory(xdp)\n\n# Attach", "\nif (NOT APPLE)\nxrt_add_subdirectory(xdp)\nendif()\n\n# Attach"),
])
# XRT's userland code forks POSIX (written "__linux__") from Windows; macOS takes the POSIX branch. Every widened gate
# is listed with its expected count. Not widened: module_loader.cpp's secure_getenv (glibc only; its getenv fallback
# is right on macOS).
W = "#if defined(__linux__) || defined(__APPLE__)\n"
for rel, n in [("runtime_src/core/common/thread.cpp", 2), ("runtime_src/core/common/message.cpp", 2),
               ("runtime_src/core/common/config.h", 2), ("runtime_src/core/common/config_reader.cpp", 2),
               ("runtime_src/core/common/scheduler.cpp", 1), ("runtime_src/core/common/api/xrt_kernel.cpp", 1),
               ("runtime_src/core/common/api/xrt_profile.cpp", 1), ("runtime_src/xrt/config.h", 2),
               ("runtime_src/core/common/aiebu/src/cpp/include/aiebu/detail/config.h", 1)]:
    q = os.path.join(root, rel)
    s = open(q).read()
    assert s.count("#ifdef __linux__\n") == n, (rel, s.count("#ifdef __linux__\n"))
    s = s.replace("#ifdef __linux__\n", W)
    open(q + ".tmp", "w").write(s)
    os.replace(q + ".tmp", q)
edit("runtime_src/core/common/module_loader.cpp", [("#ifdef __linux__\n# include <unistd.h>\n#endif\n", W + "# include <unistd.h>\n#endif\n")])
edit("runtime_src/core/common/memalign.h", [("#if defined(__linux__)\n", W)])
# std::min over mixed 64-bit types (uint64_t and size_t are distinct on macOS)
edit("runtime_src/core/common/memaccess.cpp", [("    uint64_t bytes_to_edit = std::min(available_bank_size, remaining_bytes_to_see);\n",
                                               "    uint64_t bytes_to_edit = std::min<uint64_t>(available_bank_size, remaining_bytes_to_see);\n")])
edit("runtime_src/core/common/aiebu/src/cpp/tools/opcode_information.cpp", [("    region_end  = std::min(sec_size, page_base + k_page_length);\n",
                                                                            "    region_end  = std::min<size_t>(sec_size, page_base + k_page_length);\n")])
# the PCIe shim's Linux kernel AIO wrappers (the Alveo queue-DMA path): macOS has no such syscalls, so each reports
# ENOSYS instead of issuing a raw syscall number that means something else on xnu
S = "runtime_src/core/pcie/linux/shim.cpp"
def nosys(call):
    return ("  return syscall(" + call + ");\n",
            "#ifdef __APPLE__\n  errno = ENOSYS;\n  return -1;\n#else\n  return syscall(" + call + ");\n#endif\n")
edit(S, [nosys("__NR_io_setup, nr, ctxp"), nosys("__NR_io_destroy, ctx"), nosys("__NR_io_submit, ctx, nr, iocbpp"),
         nosys("__NR_io_getevents, ctx, min_nr, max_nr, events, timeout")])
# macOS has no sysfs and no mmap(2) on device nodes: device discovery, attribute reads/writes and the instance number
# come from the accel node through nmx_xrt_sysfs.hpp (compat-xrt), which asks NullMoth's kernel port
P = "runtime_src/core/pcie/linux/"
APPLE_INC = "#ifdef __APPLE__\n# include \"nmx_xrt_sysfs.hpp\"\n#endif\n"
edit(P + "pcidrv.cpp", [
    ("#include <filesystem>\n", "#include <filesystem>\n" + APPLE_INC),
    ("  const std::string drv_name = name();\n\n  for (const auto& [bus_path, is_pci] : bus_roots) {\n",
     "  const std::string drv_name = name();\n\n"
     "#ifdef __APPLE__\n"
     "  // every accel node whose kernel driver has this name, by its PCI address (the sysfs name Linux uses)\n"
     "  for (auto& n : nmx_xrt::nodes()) {\n"
     "    if (n.driver != drv_name)\n"
     "      continue;\n"
     "    try {\n"
     "      auto pf = create_pcidev(n.bdf);\n"
     "      if (!pf)\n"
     "        continue;\n"
     "      if (pf->m_is_ready)\n"
     "        ready_list.push_back(std::move(pf));\n"
     "      else\n"
     "        nonready_list.push_back(std::move(pf));\n"
     "    }\n"
     "    catch (const std::exception&) {\n"
     "      continue;\n"
     "    }\n"
     "  }\n"
     "  return;\n"
     "#endif\n\n"
     "  for (const auto& [bus_path, is_pci] : bus_roots) {\n"),
])
edit(P + "pcidev.cpp", [
    ("#include <unistd.h>\n", "#include <unistd.h>\n" + APPLE_INC),
    ("static std::fstream\nopen_path(",
     "#ifdef __APPLE__\n"
     "// the attribute path below the device: \"entry\", or \"group/entry\" for a named attribute group\n"
     "static std::string\n"
     "nmx_sys_path(const std::string& name, const std::string& subdev, const std::string& entry)\n"
     "{\n"
     "  return get_dev_path(name) + \"/\" + (subdev.empty() ? \"\" : subdev + \"/\") + entry;\n"
     "}\n\n"
     "static void\n"
     "nmx_get(const std::string& name, const std::string& subdev, const std::string& entry,\n"
     "        std::string& err, std::string& value)\n"
     "{\n"
     "  err.clear();\n"
     "  auto path = nmx_sys_path(name, subdev, entry);\n"
     "  int e = nmx_xrt::read_sys(path, value);\n"
     "  if (e)\n"
     "    err = \"Failed to open \" + path + \" for reading: \" + strerror(e) + \"\\n\";\n"
     "}\n\n"
     "static void\n"
     "nmx_put(const std::string& name, const std::string& subdev, const std::string& entry,\n"
     "        std::string& err, const char* data, size_t len)\n"
     "{\n"
     "  err.clear();\n"
     "  auto path = nmx_sys_path(name, subdev, entry);\n"
     "  int e = nmx_xrt::write_sys(path, data, len);\n"
     "  if (e)\n"
     "    err = \"Failed to write \" + path + \": \" + strerror(e) + \"\\n\";\n"
     "}\n"
     "#endif\n\n"
     "static std::fstream\nopen_path("),
    ("  std::fstream fs = open(name, subdev, entry, err, false, false);\n  if (!err.empty())\n    return;\n\n  sv.clear();\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    std::string value;\n"
     "    nmx_get(name, subdev, entry, err, value);\n"
     "    if (!err.empty())\n"
     "      return;\n"
     "    sv.clear();\n"
     "    std::istringstream is(value);\n"
     "    std::string l;\n"
     "    while (std::getline(is, l))\n"
     "      sv.push_back(l);\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  std::fstream fs = open(name, subdev, entry, err, false, false);\n  if (!err.empty())\n    return;\n\n  sv.clear();\n"),
    ("  std::fstream fs = open(name, subdev, entry, err, false, true);\n  if (!err.empty())\n    return;\n\n  buf.clear();\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    std::string value;\n"
     "    nmx_get(name, subdev, entry, err, value);\n"
     "    if (!err.empty())\n"
     "      return;\n"
     "    buf.assign(value.begin(), value.end());\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  std::fstream fs = open(name, subdev, entry, err, false, true);\n  if (!err.empty())\n    return;\n\n  buf.clear();\n"),
    ("  std::fstream fs = open(name, subdev, entry, err, true, true);\n",
     "#ifdef __APPLE__\n"
     "  nmx_put(name, subdev, entry, err, buf.data(), buf.size());\n"
     "  return;\n"
     "#endif\n"
     "  std::fstream fs = open(name, subdev, entry, err, true, true);\n"),
    ("    m_instance = get_render_value(\n      sysfs_path + \"/\" + m_driver->sysfs_dev_node_dir(),\n      m_driver->dev_node_prefix());\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    nmx_xrt::node n;\n"
     "    m_instance = nmx_xrt::find(m_sysfs_name, n) ? n.instance : INVALID_ID;\n"
     "  }\n"
     "#else\n"
     "    m_instance = get_render_value(\n      sysfs_path + \"/\" + m_driver->sysfs_dev_node_dir(),\n      m_driver->dev_node_prefix());\n"
     "#endif\n"),
])
# put(string) and put(unsigned int) share their body text; both write the value's decimal/text form
q = os.path.join(root, P + "pcidev.cpp")
t = open(q).read()
old = "  std::fstream fs = open(name, subdev, entry, err, true, false);\n  if (!err.empty())\n    return;\n  fs << input;\n"
assert t.count(old) == 2, t.count(old)
t = t.replace(old, "#ifdef __APPLE__\n  {\n    std::ostringstream os;\n    os << input;\n    auto s = os.str();\n"
                   "    nmx_put(name, subdev, entry, err, s.data(), s.size());\n    return;\n  }\n#endif\n" + old)
open(q + ".tmp", "w").write(t)
os.replace(q + ".tmp", q)
# the PCIe shim's ICD file goes to /etc/OpenCL/vendors (the Alveo OpenCL runtime, not built on macOS)
edit("CMake/nativeLnx.cmake", [("  include (CMake/icd.cmake)\n", "  if (NOT APPLE)\n  include (CMake/icd.cmake)\n  endif()\n")])
# library file names: Linux libNAME.so.<major>, macOS libNAME.<major>.dylib (CMake's SOVERSION naming), for the core
# shim, optional modules and the driver plug-ins found beside the shim (libxrt_driver_xdna.2.dylib)
M = "runtime_src/core/common/module_loader.cpp"
edit(M, [
    ('  path /= "xrt/module/lib" + module + ".so." + XRT_VERSION_MAJOR;\n',
     '#ifdef __APPLE__\n  path /= "xrt/module/lib" + module + "." + XRT_VERSION_MAJOR + ".dylib";\n#else\n'
     '  path /= "xrt/module/lib" + module + ".so." + XRT_VERSION_MAJOR;\n#endif\n'),
    ('  path /= "lib" + name + ".so." + XRT_VERSION_MAJOR;\n',
     '#ifdef __APPLE__\n  path /= "lib" + name + "." + XRT_VERSION_MAJOR + ".dylib";\n#else\n'
     '  path /= "lib" + name + ".so." + XRT_VERSION_MAJOR;\n#endif\n'),
    ('  const std::string suf = std::string(".so.") + XRT_VERSION_MAJOR;\n',
     '#ifdef __APPLE__\n  const std::string suf = std::string(".") + XRT_VERSION_MAJOR + ".dylib";\n#else\n'
     '  const std::string suf = std::string(".so.") + XRT_VERSION_MAJOR;\n#endif\n'),
])
# the host facts xrt-smi reports: Linux reads /etc/os-release, /proc/cpuinfo and DMI from /sys; macOS has none of
# them and returned before filling cores/memory/processor, which xrt-smi then failed on. macOS gives the same facts
# through sysctl.
edit("runtime_src/core/common/detail/linux/sysinfo.h", [
    ("#include <unistd.h>\n", "#include <unistd.h>\n#ifdef __APPLE__\n#include <sys/sysctl.h>\n#endif\n"),
    ("  // The file is a requirement as per latest Linux standards\n",
     "#ifdef __APPLE__\n"
     "  {\n"
     "    auto str = [](const char* name) {\n"
     "      char buf[256] = {0};\n"
     "      size_t n = sizeof(buf) - 1;\n"
     "      return sysctlbyname(name, buf, &n, nullptr, 0) == 0 ? std::string(buf) : std::string(\"unknown\");\n"
     "    };\n"
     "    uint64_t mem = 0;\n"
     "    size_t n = sizeof(mem);\n"
     "    if (sysctlbyname(\"hw.memsize\", &mem, &n, nullptr, 0) != 0)\n"
     "      mem = 0;\n"
     "    pt.put(\"distribution\", \"macOS \" + str(\"kern.osproductversion\") + \" (\" + str(\"kern.osversion\") + \")\");\n"
     "    pt.put(\"bios_vendor\", \"unknown\");\n"
     "    pt.put(\"bios_version\", \"unknown\");\n"
     "    pt.put(\"model\", str(\"hw.model\"));\n"
     "    pt.put(\"cores\", std::thread::hardware_concurrency());\n"
     "    pt.put(\"memory_bytes\", (boost::format(\"0x%llx\") % static_cast<unsigned long long>(mem)).str());\n"
     "    boost::property_tree::ptree lib;\n"
     "    lib.put(\"name\", \"libSystem\");\n"
     "    lib.put(\"version\", str(\"kern.osversion\"));\n"
     "    boost::property_tree::ptree libs;\n"
     "    libs.push_back({\"\", lib});\n"
     "    pt.put_child(\"libraries\", libs);\n"
     "    char hostname[256] = {0};\n"
     "    gethostname(hostname, sizeof(hostname) - 1);\n"
     "    pt.put(\"hostname\", std::string(hostname));\n"
     "    pt.put(\"processor\", str(\"machdep.cpu.brand_string\"));\n"
     "    return;\n"
     "  }\n"
     "#endif\n"
     "  // The file is a requirement as per latest Linux standards\n"),
])
# the NPU driver's version: Linux reads /sys/module/amdxdna/version, and a miss is reported as the kernel's release
# (an in-tree driver). On macOS the amdxdna driver is NullMoth's NMXNPU kext, so its bundle version is reported
# ("not installed" when the kext is absent) instead of the Darwin release, which would name the wrong thing.
edit("runtime_src/core/pcie/linux/system_linux.cpp", [
    ('  std::ifstream stream(path);\n  if (stream.is_open()) {\n    std::string line;\n',
     '#ifdef __APPLE__\n'
     '  if (boost::iequals(driver, "amdxdna")) {\n'
     '    ver = "not installed";\n'
     '    std::ifstream plist("/Library/Extensions/NMXNPU.kext/Contents/Info.plist");\n'
     '    std::string text((std::istreambuf_iterator<char>(plist)), std::istreambuf_iterator<char>());\n'
     '    auto k = text.find("<key>CFBundleVersion</key>");\n'
     '    auto b = k == std::string::npos ? k : text.find("<string>", k);\n'
     '    auto e = b == std::string::npos ? b : text.find("</string>", b);\n'
     '    if (e != std::string::npos)\n'
     '      ver = "NMXNPU " + text.substr(b + 8, e - b - 8);\n'
     '  }\n'
     '#endif\n'
     '  std::ifstream stream(path);\n  if (stream.is_open()) {\n    std::string line;\n'),
])
print("xrt patched")
