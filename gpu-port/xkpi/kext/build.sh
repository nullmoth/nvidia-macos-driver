#!/bin/bash
# kext/build.sh <out dir>: links NMXAMD.kext from the compiled port (obj = AMD/DRM/TTM, lkobj = LinuxKPI runtime and
# the port's LinuxKPI-side files, srcobj = FreeBSD-on-xnu layer, xnuobj = IOKit glue and the kext itself).
set -euo pipefail
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
OUT=$1
cd "$X"
./ccxx.sh xnu/nmx_kext.cpp > xlog_nmx_kext.txt 2>&1 || { grep -A3 error: xlog_nmx_kext.txt | head -20; exit 1; }
K="$OUT/NMXAMD.kext/Contents"
mkdir -p "$K/MacOS"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext \
  obj/*.o lkobj/*.o srcobj/*.o xnuobj/*.o -lkmodc++ -lkmod -lcc_kext -o "$K/MacOS/NMXAMD" 2> link.log || {
    grep -E "error|Undefined|duplicate" link.log | head -40; exit 1; }
cp kext/Info.plist "$K/Info.plist"
echo "NMXAMD $(shasum -a 256 "$K/MacOS/NMXAMD" | cut -c1-8) $(stat -f %z "$K/MacOS/NMXAMD") B"
