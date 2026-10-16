#!/bin/bash
# kext/build.sh <out dir>: links NMXGPU.kext from the compiled port (obj = AMD/DRM/TTM, obj-i915 = Intel i915, obj-xe = Intel xe, lkobj = LinuxKPI runtime and
# the port's LinuxKPI-side files, srcobj = FreeBSD-on-xnu layer, xnuobj = IOKit glue and the kext itself).
set -euo pipefail
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
OUT=$1
cd "$X"
./ccxx.sh xnu/nmx_kext.cpp > xlog_nmx_kext.txt 2>&1 || { grep -A3 error: xlog_nmx_kext.txt | head -20; exit 1; }
# xe carries its own build of the Intel display code (compiled for xe_device), which would collide with i915's at link.
# So xe is first folded into one object that exports nothing: every xe symbol becomes local, xe's calls bind to its
# own copies, and it registers itself through the linker sets (SYSINIT/module params) like the other drivers.
: > xe.exp
xcrun ld -r -arch x86_64 obj-xe/*.o -exported_symbols_list xe.exp -o xe_all.o
K="$OUT/NMXGPU.kext/Contents"
# rt.sh compiles every xnu/*.cpp, so NMXNPU's kext glue (its own kmod_info) is in xnuobj too: never part of this link
XNU=$(ls xnuobj/*.o | grep -v "/nmx_npu_kext\.o$")
mkdir -p "$K/MacOS"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext \
  obj/*.o obj-i915/*.o xe_all.o lkobj/*.o srcobj/*.o $XNU -lkmodc++ -lkmod -lcc_kext -o "$K/MacOS/NMXGPU" 2> link.log || {
    grep -E "error|Undefined|duplicate" link.log | head -40; exit 1; }
cp kext/Info.plist "$K/Info.plist"
echo "NMXGPU $(shasum -a 256 "$K/MacOS/NMXGPU" | cut -c1-8) $(stat -f %z "$K/MacOS/NMXGPU") B"
