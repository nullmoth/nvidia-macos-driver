# patch_openvino.py <openvino tree>: OpenVINO's NPU plugin on macOS x86_64 (Intel NPUs driven by NMXNPU through the
# Level Zero loader and Intel's NPU user driver in /Library/NullMoth/npu/lib). Anchors exact and asserted; atomic writes.
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

# the plugin is offered on x86_64 Windows, Linux and Android; macOS x86_64 now has an NPU stack too
edit("cmake/features.cmake", [
    ('ov_dependent_option (ENABLE_INTEL_NPU "NPU plugin for OpenVINO runtime" ON "X86_64;WIN32 OR LINUX OR ANDROID" OFF)',
     'ov_dependent_option (ENABLE_INTEL_NPU "NPU plugin for OpenVINO runtime" ON "X86_64;WIN32 OR LINUX OR ANDROID OR APPLE" OFF)'),
])
# uint64_t is "unsigned long long" on macOS but "unsigned long" (= size_t) on Linux, where the option printer covers it
edit("src/plugins/intel_npu/src/al/include/intel_npu/config/config.hpp", [
    ("TYPE_PRINTER(std::size_t)\n", "TYPE_PRINTER(std::size_t)\n#ifdef __APPLE__\nTYPE_PRINTER(uint64_t)\n#endif\n"),
])
# the compiler interface (vcl.h) takes uint64_t sizes, the Level Zero wrapper size_t: the same type on Linux, two on macOS
edit("src/plugins/intel_npu/src/utils/include/intel_npu/utils/vcl/vcl_allocator.hpp", [
    ("    static uint8_t* allocate(vcl_allocator2_t* allocator, size_t size) noexcept {\n",
     "    static uint8_t* allocate(vcl_allocator2_t* allocator, uint64_t size) noexcept {\n"),
])
edit("src/plugins/intel_npu/src/compiler_adapter/src/weightless_graph.cpp", [("        uint64_t blobSize;\n", "        size_t blobSize;\n")])
# token hashes are uint64_t everywhere else in the prefix cache (size_t is the same type only on Linux)
edit("src/plugins/intel_npu/src/plugin/npuw/llm_prefix_caching.cpp", [
    ("        std::vector<size_t> token_hashes(block_size);\n", "        std::vector<uint64_t> token_hashes(block_size);\n"),
])
# the Level Zero loader on macOS: the NullMoth NPU userland's, by its installed path (the Linux rule would ask for
# "libze_loader.dylib.1")
edit("src/common/zero_loader/src/zero.api.cpp", [
    ("#if !defined(_WIN32) && !defined(ANDROID)\n    libpath += LIB_ZE_LOADER_SUFFIX;\n#endif\n",
     "#if defined(__APPLE__)\n    libpath = \"/Library/NullMoth/npu/lib/libze_loader.1.dylib\";\n"
     "#elif !defined(_WIN32) && !defined(ANDROID)\n    libpath += LIB_ZE_LOADER_SUFFIX;\n#endif\n"),
])
# OpenVINO's own Level Zero loader (built for the NPU plugin's build; the package ships NullMoth's): its log-directory
# code calls getpwuid/getuid on every non-Windows system, but includes their headers only on Linux
edit("thirdparty/level_zero/level-zero/source/utils/logging.h", [
    ("#ifdef __linux__\n#include <unistd.h>\n#include <sys/types.h>\n#include <pwd.h>\n#endif // __linux__\n",
     "#if defined(__linux__) || defined(__APPLE__)\n#include <unistd.h>\n#include <sys/types.h>\n#include <pwd.h>\n#endif\n"),
])
# NPU plugin options declared as size_t (NPUW_ONLINE_MIN_SIZE, ...): Linux parses them with OptionParser<uint64_t>, the
# same type there; on macOS size_t is unsigned long and uint64_t unsigned long long, so size_t gets a parser forwarding
# to the uint64_t one (same width, same text)
edit("src/plugins/intel_npu/src/al/include/intel_npu/config/config.hpp", [
    ("template <>\nstruct OptionParser<uint64_t> final {\n    static uint64_t parse(std::string_view val);\n};\n",
     "template <>\nstruct OptionParser<uint64_t> final {\n    static uint64_t parse(std::string_view val);\n};\n\n"
     "#ifdef __APPLE__\n"
     "template <>\n"
     "struct OptionParser<std::size_t> final {\n"
     "    static std::size_t parse(std::string_view val) {\n"
     "        return static_cast<std::size_t>(OptionParser<uint64_t>::parse(val));\n"
     "    }\n"
     "};\n"
     "#endif\n"),
])
# the compiler's option list size: vclGetCompilerSupportedOptions takes uint64_t*; declared with the API's type (size_t
# is a different type on macOS)
edit("src/plugins/intel_npu/src/compiler_adapter/src/compiler_impl.cpp", [
    ('    _logger.debug("get_supported_options start");\n    size_t str_size = 0;\n',
     '    _logger.debug("get_supported_options start");\n    uint64_t str_size = 0;\n'),
])
print("openvino patched")
