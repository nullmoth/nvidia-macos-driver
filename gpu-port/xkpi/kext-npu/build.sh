#!/bin/bash
# kext-npu/build.sh <out dir>: links NMXNPU.kext (GPL-2.0) - Intel's NPU driver (obj-npu: ivpu, DRM accel core, GEM shmem
# helper) on its own DRM core built with accel minors (obj-npucore) and its own copy of the port layer (lkobj, srcobj,
# xnuobj without NMXGPU's kext and display glue).
set -euo pipefail
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
OUT=$1
cd "$X"
mkdir -p obj-npu-glue
./ccxx.sh xnu/nmx_npu_kext.cpp > xlog_nmx_npu_kext.txt 2>&1 || { grep -A3 error: xlog_nmx_npu_kext.txt | head -20; exit 1; }
mv xnuobj/nmx_npu_kext.o obj-npu-glue/      # never part of NMXGPU's link
# the DRM glue without TTM (NMXNPU has none); the NPU-only glue (override/npu) is compiled into obj-npu by all-npu.sh
NMX_OBJ=$X/obj-npu-glue/nmx_drm_os_npu.o ./cclk.sh override/src/nmx_drm_os.c -DNMX_NO_TTM > xlog_npu_drm_os.txt 2>&1 || { grep error: xlog_npu_drm_os.txt | head; exit 1; }
XNU=$(ls xnuobj/*.o | grep -vE "/(nmx_kext|nmx_fb)\.o$")
LK=$(ls lkobj/*.o | grep -vE "/(nmx_fb_bridge|nmx_intel_stolen|nmx_drm_os)\.o$")
K="$OUT/NMXNPU.kext/Contents"
mkdir -p "$K/MacOS"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext \
  obj-npucore/*.o obj-npu/*.o obj-npu-glue/*.o $LK srcobj/*.o $XNU -lkmodc++ -lkmod -lcc_kext -o "$K/MacOS/NMXNPU" 2> link-npu.log || {
    grep -E "error|Undefined|duplicate|referenced from" link-npu.log | head -60; exit 1; }
cp kext-npu/Info.plist "$K/Info.plist"
echo "NMXNPU $(shasum -a 256 "$K/MacOS/NMXNPU" | cut -c1-8) $(stat -f %z "$K/MacOS/NMXNPU") B"
