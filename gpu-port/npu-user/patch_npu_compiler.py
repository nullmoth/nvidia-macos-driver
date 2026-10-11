#!/usr/bin/env python3
"""patch_npu_compiler.py <npu_compiler source root>: Intel's NPU model compiler (openvino_intel_npu_compiler) for
macOS x86_64, deployment target 15.0. Every edit is an exact anchor with an asserted count; run once on a pristine tree.
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


# libc++ marks floating-point std::from_chars "introduced in macOS 26.0" (_LIBCPP_AVAILABILITY_FROM_CHARS_FLOATING_POINT),
# so a 15.0 deployment cannot call it. strtod_l in the C locale parses the same text and, like from_chars, ignores the
# user's locale (a comma-decimal locale must not change how a compiler option reads).
edit("src/vpux_utils/src/ov/config.cpp", [
    ("#include <cstdlib>\n", "#include <cstdlib>\n#ifdef __APPLE__\n#include <cerrno>\n#include <xlocale.h>\n#endif\n"),
    ("double OptionParser<double>::parse(std::string_view val) {\n    double value;\n"
     "    const auto result = std::from_chars(val.data(), val.data() + val.size(), value);\n"
     "    if (result.ec == std::errc()) {\n        return value;\n    }\n",
     "double OptionParser<double>::parse(std::string_view val) {\n"
     "#ifdef __APPLE__\n"
     "    const std::string text(val);\n"
     "    char* end = nullptr;\n"
     "    errno = 0;\n"
     "    const double value = strtod_l(text.c_str(), &end, LC_C_LOCALE);\n"
     "    if (!text.empty() && end == text.c_str() + text.size() && errno == 0) {\n"
     "        return value;\n"
     "    }\n"
     "#else\n"
     "    double value;\n"
     "    const auto result = std::from_chars(val.data(), val.data() + val.size(), value);\n"
     "    if (result.ec == std::errc()) {\n        return value;\n    }\n"
     "#endif\n"),
])

# LLVM's tablegen() runs ${NPU_TABLEGEN_EXE} as a bare command; with the tool built in this tree that bare name is
# looked up on PATH and the step fails (exit 127). The generator expression names the built tool's own path.
edit("src/vpux_compiler/cmake/vpux_tblgen.cmake", [
    ('  set(NPU_TABLEGEN_EXE "npureg-tblgen")\n', '  set(NPU_TABLEGEN_EXE "$<TARGET_FILE:npureg-tblgen>")\n'),
])
# std::max over a size_t and a uint64_t: the same 64-bit width, but distinct types on macOS (unsigned long vs
# unsigned long long), so template deduction fails; the comparison is made in size_t
edit("src/vpux_compiler/src/dialect/ELF/IR/ops/create_logical_section.cpp", [
    ("            totalSize = std::max(totalSize, span);\n",
     "            totalSize = std::max<size_t>(totalSize, span);\n"),
])
# std::distance returns ptrdiff_t (long); getInputClusteredOpForVFOp takes an int64_t& (long long& on macOS), which a
# long cannot bind to. The index is declared int64_t, its type on Linux.
edit("src/vpux_compiler/src/dialect/VPU/utils/distributed_tensor_utils.cpp", [
    ("    auto operandIdx = std::distance(op->getOperands().begin(), iter);\n",
     "    int64_t operandIdx = std::distance(op->getOperands().begin(), iter);\n"),
])
# ov::CoordinateDiff holds std::ptrdiff_t (long); the convolution shape helper takes ArrayRef<int64_t> (long long on
# macOS), and three call sites pass CoordinateDiff paddings. On Linux the two are one type; on macOS an overload takes
# the ptrdiff_t paddings and forwards them, copied into int64_t (same width, same values).
edit("src/vpux_compiler/include/vpux/compiler/utils/infer_output_shape.hpp", [
    ("                                          ArrayRef<int64_t> windowDilations);\n\n/**\n"
     " * @brief                        Infers the output shape for a GroupConvolution operation\n",
     "                                          ArrayRef<int64_t> windowDilations);\n\n"
     "#ifdef __APPLE__\n"
     "inline ShapeInfo inferConvolutionOutputShapeInfo(const ShapeInfo& inShapeInfo, const ShapeInfo& filterShapeInfo,\n"
     "                                                 NDTypeInterface filterType, ArrayRef<int64_t> windowStrides,\n"
     "                                                 ArrayRef<std::ptrdiff_t> dataPaddingBelow,\n"
     "                                                 ArrayRef<std::ptrdiff_t> dataPaddingAbove,\n"
     "                                                 ArrayRef<int64_t> windowDilations) {\n"
     "    const std::vector<int64_t> below(dataPaddingBelow.begin(), dataPaddingBelow.end());\n"
     "    const std::vector<int64_t> above(dataPaddingAbove.begin(), dataPaddingAbove.end());\n"
     "    return inferConvolutionOutputShapeInfo(inShapeInfo, filterShapeInfo, filterType, windowStrides, below, above,\n"
     "                                           windowDilations);\n"
     "}\n"
     "#endif\n\n/**\n"
     " * @brief                        Infers the output shape for a GroupConvolution operation\n"),
])
# the driver-compiler library the NPU user-mode driver loads: export only vcl* (Linux: a GNU version script) and
# refuse undefined symbols, in macOS linker terms (ld64 refuses undefined symbols by default)
edit("src/vpux_driver_compiler/src/vpux_compiler_l0/CMakeLists.txt", [
    ("if(UNIX)\n    target_link_options(${TARGET_NAME} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/vcl.exports)\n",
     "if(APPLE)\n"
     "    file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/vcl.exported_symbols \"_vcl*\\n\")\n"
     "    target_link_options(${TARGET_NAME} PRIVATE\n"
     "        -Wl,-exported_symbols_list,${CMAKE_CURRENT_BINARY_DIR}/vcl.exported_symbols)\n"
     "elseif(UNIX)\n"
     "    target_link_options(${TARGET_NAME} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/vcl.exports)\n"),
])
# reserved-memory bounds: size_t against uint64_t operands (one type on Linux, two on macOS); computed in size_t
edit("src/vpux_compiler/src/dialect/config/IR/resources.cpp", [
    ("        bottomEnd = std::max(bottomEnd, static_cast<size_t>(resource.getOffset().value()) + resource.getByteSize());\n",
     "        bottomEnd = std::max<size_t>(bottomEnd, static_cast<size_t>(resource.getOffset().value()) + resource.getByteSize());\n"),
    ("        offset = alignValDown(topStart - size, alignment);\n",
     "        offset = alignValDown<size_t>(topStart - size, alignment);\n"),
])
# the compiler library exports only CreateNPUCompiler (Linux: a GNU version script; macOS: an exported-symbols list)
edit("src/vpux_compiler/CMakeLists.txt", [
    ("if(UNIX)\n    target_link_options(${COMPILER_LIB} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/compiler.exports)\nendif()\n",
     "if(APPLE)\n"
     "    file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/compiler.exported_symbols \"_CreateNPUCompiler\\n\")\n"
     "    target_link_options(${COMPILER_LIB} PRIVATE\n"
     "        -Wl,-exported_symbols_list,${CMAKE_CURRENT_BINARY_DIR}/compiler.exported_symbols)\n"
     "elseif(UNIX)\n"
     "    target_link_options(${COMPILER_LIB} PRIVATE -Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/compiler.exports)\n"
     "endif()\n"),
])
print("npu_compiler patched")
