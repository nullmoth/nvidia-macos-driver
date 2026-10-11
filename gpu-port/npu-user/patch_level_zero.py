# patch_level_zero.py <level-zero tree>: the Level Zero loader on macOS - driver and layer libraries are .dylib files
# in /Library/NullMoth/npu/lib, where the NullMoth NPU user driver is installed (the loader dlopens them by name).
# Anchors are exact and asserted; the file is rewritten atomically.
import os, sys
p = os.path.join(sys.argv[1], "source/inc/ze_util.h")
t = open(p).read()
old = '#elif !defined(WIN32)\n#  define MAKE_LIBRARY_NAME(NAME, VERSION)    "lib" NAME ".so." VERSION\n'
assert t.count(old) == 1
new = ('#elif defined(__APPLE__)\n'
       '#  define MAKE_LIBRARY_NAME(NAME, VERSION)    "/Library/NullMoth/npu/lib/lib" NAME "." VERSION ".dylib"\n'
       '#  define MAKE_LAYER_NAME(NAME)    "/Library/NullMoth/npu/lib/lib" NAME "." L0_VALIDATION_LAYER_SUPPORTED_VERSION ".dylib"\n'
       + old)
t = t.replace(old, new)
open(p + ".tmp", "w").write(t)
os.replace(p + ".tmp", p)
# discovery: on macOS the known driver names are absolute paths (above), so a name is found where it says, and its
# driver type is read from the file name, not the whole path
d = os.path.join(sys.argv[1], "source/loader/linux/driver_discovery_lin.cpp")
s = open(d).read()
old1 = "static bool libraryExistsInSearchPaths(const std::string& filename) {\n  auto paths = getLibrarySearchPaths();\n"
assert s.count(old1) == 1
s = s.replace(old1, "static bool libraryExistsInSearchPaths(const std::string& filename) {\n"
                    "  if (!filename.empty() && filename[0] == '/')\n    return fileExistsReadable(filename);\n"
                    "  auto paths = getLibrarySearchPaths();\n")
old2 = "        std::string libName = path;\n"
assert s.count(old2) == 1
s = s.replace(old2, "        std::string libName = path;\n"
                    "        if (libName.find_last_of('/') != std::string::npos)\n"
                    "          libName = libName.substr(libName.find_last_of('/') + 1);\n")
open(d + ".tmp", "w").write(s)
os.replace(d + ".tmp", d)
print("level-zero patched")
