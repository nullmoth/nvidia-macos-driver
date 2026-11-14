#!/bin/bash
# build_plugin_27.sh <out-dir>: the macOS 27 NVMTLDriver from this tree's plugin/, built on the macOS 27.2 build Mac
# (ssh host "m6") against its SDK, minimum macOS 27.2. The build stops when the warning count is not the 104 this
# tree gives with that SDK, because a new warning or a missing one means the source, the flags or the SDK changed.
set -euo pipefail
OUT=${1:?output directory}
HERE=$(cd "$(dirname "$0")/.." && pwd)
H=${NVMTL_BUILD_HOST:-m6}
# NVMTL_DIAG=1 builds the logging variant (every process writes the driver log, WindowServer included). It is for
# measuring on a test machine and is never the build that ships; its warning count is printed, not checked.
REL=1; R=${NVMTL_BUILD_DIR:-nmbuild/build/plug272-release}
if [ "${NVMTL_DIAG:-0}" = 1 ]; then REL=0; R=${NVMTL_BUILD_DIR:-nmbuild/build/plug272-diag}; fi
INC=${NVMTL_MESA_INCLUDE:-nmbuild/nvk/k272m/mesa/include}
ssh "$H" "rm -rf $R/src $R/out && mkdir -p $R/src $R/out"
rsync -a --delete "$HERE/plugin/" "$H:$R/src/"
ssh "$H" "cd $R/src && xcrun clang -arch x86_64 -mmacosx-version-min=27.2 -bundle -fobjc-arc -O1 -DNVMTL_RELEASE=$REL \
  -Wno-protocol -Wno-incomplete-implementation -Werror=unguarded-availability-new \
  -I \$HOME/$INC -o ../out/NVMTLDriver NVMTLDevice.m nvmtl_dlss.c dlss/elfhost.c dlss/lnxlibc.c \
  -framework Foundation -framework Metal -framework IOSurface -framework IOKit -framework CoreFoundation -framework MetalFX -liconv 2>../out/build.err" \
  || { ssh "$H" "grep ' error:' $R/out/build.err | head -20"; exit 1; }
ssh "$H" "codesign --force --sign - --identifier com.nullmoth.NVMTLDriver $R/out/NVMTLDriver"
mkdir -p "$OUT"
scp -q "$H:$R/out/NVMTLDriver" "$H:$R/out/build.err" "$OUT/"
w=$(grep -c ': warning:' "$OUT/build.err" || true)
[ "$REL" = 0 ] || [ "$w" = 104 ] || { echo "STOP: $w warnings, expected 104"; exit 1; }
echo "NVMTLDriver (macOS 27) md5 $(md5 -q "$OUT/NVMTLDriver") warnings $w"
