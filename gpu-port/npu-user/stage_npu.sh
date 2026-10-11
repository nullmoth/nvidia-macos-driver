#!/bin/bash
# stage_npu.sh <out dir>: the NPU userland as it installs at /Library/NullMoth/npu, from the builds under
# $NULLMOTH_BUILD_ROOT/build/npu-user (see BUILD-NPU-USER.md for how each was built).
#
#   lib/        Level Zero loader, Intel's NPU user-mode driver, and the NPU compiler it loads from beside itself
#               (absolute install names: apps and the loader find them at /Library/NullMoth/npu/lib)
#   openvino/   OpenVINO runtime with its NPU, CPU and AUTO plugins and IR frontend; hello_query_device,
#               benchmark_app, compile_tool. OpenVINO finds its plugins beside libopenvino.
#   xrt/        AMD's runtime: XRT, the XDNA plug-in, xrt-smi (stage_xrt.sh)
#   bin/        nmx-npu-probe (what the kernel says about each NPU node) and nmx-npu-compile-check (the Intel NPU
#               compiler on a small network, no NPU needed), both run by "Send logs"
#   licenses/   every licence and third-party notice of the above
# Build-tree rpaths are replaced with relative ones and every Mach-O is ad-hoc signed afterwards.
set -euo pipefail
R=$NULLMOTH_BUILD_ROOT/build/npu-user
OUT=$1
HERE=$(cd "$(dirname "$0")" && pwd -P)
OVB=$R/openvino-pin/bin/intel64/Release   # build_openvino.sh
rm -rf "$OUT"
mkdir -p "$OUT/lib" "$OUT/openvino/lib" "$OUT/openvino/bin" "$OUT/bin" "$OUT/licenses"

strip_rpaths() {
    local f=$1 r
    for r in $(otool -l "$f" | awk '/LC_RPATH/ {f=1} f && / path / {print $2; f=0}'); do
        install_name_tool -delete_rpath "$r" "$f"
    done
}
# one file by its real name, renamed; fails loudly when the build did not produce it
take() {
    local src=$1 dst=$2
    [ -f "$src" ] || { echo "stage_npu: missing $src" >&2; exit 1; }
    cp -L "$src" "$dst"
}

# ---- lib: Level Zero, the Intel NPU driver and its compiler -------------------------------------------------------
take "$R/level-zero/build-x86/lib/libze_loader.1.34.0.dylib" "$OUT/lib/libze_loader.1.dylib"
take "$(ls "$R"/linux-npu-driver/build-x86/lib/libze_intel_npu.1.*.*.dylib | head -1)" "$OUT/lib/libze_intel_npu.1.dylib"
take "$(ls "$OVB"/libopenvino_intel_npu_compiler_loader*.dylib | head -1)" "$OUT/lib/libopenvino_intel_npu_compiler_loader.dylib"
# the compiler itself, under the name the loader asks OpenVINO for (make_plugin_library_name: lib<name>.so)
take "$OVB/libopenvino_intel_npu_compiler.so" "$OUT/lib/libopenvino_intel_npu_compiler.so"
for f in "$OUT"/lib/*; do
    strip_rpaths "$f"
    install_name_tool -id "/Library/NullMoth/npu/lib/$(basename "$f")" "$f"
done

# ---- openvino --------------------------------------------------------------------------------------------------------
for n in libopenvino.2640.dylib libopenvino_c.2640.dylib libopenvino_ir_frontend.2640.dylib \
         libopenvino_intel_npu_plugin.so libopenvino_intel_cpu_plugin.so libopenvino_auto_plugin.so \
         libopenvino_auto_batch_plugin.so libopenvino_hetero_plugin.so; do
    take "$OVB/$n" "$OUT/openvino/lib/$n"
    strip_rpaths "$OUT/openvino/lib/$n"
    install_name_tool -add_rpath @loader_path "$OUT/openvino/lib/$n"
done
ln -s libopenvino.2640.dylib "$OUT/openvino/lib/libopenvino.dylib"
ln -s libopenvino_c.2640.dylib "$OUT/openvino/lib/libopenvino_c.dylib"
for n in hello_query_device benchmark_app compile_tool; do
    take "$OVB/$n" "$OUT/openvino/bin/$n"
    strip_rpaths "$OUT/openvino/bin/$n"
    install_name_tool -add_rpath @executable_path/../lib "$OUT/openvino/bin/$n"
done

# ---- xrt, probe ------------------------------------------------------------------------------------------------------
"$HERE/stage_xrt.sh" "$R/xdna-build-x86" "$OUT/xrt" > /dev/null
take "$R/tools-x86/nmx-npu-probe" "$OUT/bin/nmx-npu-probe"
take "$R/tools-x86/nmx-npu-compile-check" "$OUT/bin/nmx-npu-compile-check"

# ---- licences --------------------------------------------------------------------------------------------------------
L=$OUT/licenses
take "$R/level-zero/LICENSE" "$L/LICENSE.level-zero"
take "$R/linux-npu-driver/LICENSE.md" "$L/LICENSE.linux-npu-driver.md"
take "$R/linux-npu-driver/third-party-programs.txt" "$L/third-party-programs.linux-npu-driver.txt"
take "$R/openvino/LICENSE" "$L/LICENSE.openvino"
for n in third-party-programs runtime-third-party-programs onednn_third-party-programs onetbb_third-party-programs; do
    take "$R/openvino/licensing/$n.txt" "$L/$n.openvino.txt"
done
take "$R/npu_compiler/LICENSE" "$L/LICENSE.npu-compiler"
take "$R/npu_compiler/thirdparty/llvm-project/LICENSE.TXT" "$L/LICENSE.llvm"
take "$R/npu_compiler/thirdparty/vpucostmodel/LICENSE" "$L/LICENSE.vpucostmodel"
take "$R/xdna-driver/xrt/LICENSE" "$L/LICENSE.xrt"
take "$R/xdna-driver/xrt/NOTICE" "$L/NOTICE.xrt"
take "$R/deps-src/boost_1_86_0/LICENSE_1_0.txt" "$L/LICENSE.boost"

# ---- sign, then check nothing names a home directory ---------------------------------------------
find "$OUT" -type f ! -path "*/licenses/*" -print0 | while IFS= read -r -d '' f; do
    if file -b "$f" | grep -q Mach-O; then codesign -f -s - "$f" 2> /dev/null; fi
done
if grep -rl -a "/Users/" "$OUT" --exclude-dir=licenses; then
    echo "stage_npu: a staged file names a home directory" >&2
    exit 1
fi
echo "staged $OUT"
