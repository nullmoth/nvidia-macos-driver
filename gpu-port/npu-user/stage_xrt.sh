#!/bin/bash
# stage_xrt.sh <xdna build dir> <out dir>: AMD's NPU runtime as it installs under /Library/NullMoth/npu/xrt.
#   bin/xrt-smi                        examine / validate
#   lib/libxrt_coreutil.2.dylib        XRT's API (apps link this)
#   lib/libxrt_core.2.dylib            XRT's device layer (loaded by coreutil)
#   lib/libxrt_driver_xdna.2.dylib     the XDNA plug-in (found beside libxrt_core by name)
# The build tree's absolute rpaths are replaced with relative ones: XRT locates its root from where coreutil was loaded
# (dladdr), so the directory can live anywhere. Ad-hoc signed after install_name_tool changed them.
set -euo pipefail
B=$1
OUT=$2
rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/lib"
cp -L "$B/xrt/src/runtime_src/core/common/libxrt_coreutil.2.dylib" \
      "$B/xrt/src/runtime_src/core/pcie/linux/libxrt_core.2.dylib" \
      "$B/src/shim/libxrt_driver_xdna.2.dylib" "$OUT/lib/"
cp "$B/xrt/src/runtime_src/core/tools/xbutil2/xrt-smi" "$OUT/bin/"
strip_rpaths() {
    local f=$1 r
    for r in $(otool -l "$f" | awk '/LC_RPATH/ {f=1} f && / path / {print $2; f=0}'); do
        install_name_tool -delete_rpath "$r" "$f"
    done
}
strip_rpaths "$OUT/bin/xrt-smi"
install_name_tool -add_rpath @executable_path/../lib "$OUT/bin/xrt-smi"
for l in "$OUT"/lib/*.dylib; do
    strip_rpaths "$l"
    install_name_tool -add_rpath @loader_path "$l"
done
codesign -f -s - "$OUT/bin/xrt-smi" "$OUT"/lib/*.dylib
# nothing may still point into the build machine
if grep -l -a "$B" "$OUT/bin/xrt-smi" "$OUT"/lib/*.dylib; then
    echo "stage_xrt: a staged file still names the build tree" >&2
    exit 1
fi
echo "staged $OUT"
