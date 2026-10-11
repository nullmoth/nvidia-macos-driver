#!/bin/bash
# build.sh <out dir>  ->  <out>/NMDRM.kext, against the Kernel.framework headers of the active Xcode SDK.
set -euo pipefail
cd "$(dirname "$0")"
OUT=$1
SDK=$(xcrun --sdk macosx --show-sdk-path)
K="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
[ -d "$K" ] || { echo "STOP: no Kernel.framework headers in $SDK"; exit 1; }
mkdir -p "$OUT/NMDRM.kext/Contents/MacOS"
FLAGS=(-arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -mkernel -nostdinc -isysroot "$SDK" -I"$K"
       -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT
       -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -O2 -Wall -Werror -Wno-unused-parameter -g0
       # Kernel.framework's own headers: IOPCIDevice is marked deprecated (PCIDriverKit) and omits 'override'
       -Wno-deprecated-declarations -Wno-inconsistent-missing-override)
xcrun clang++ "${FLAGS[@]}" -c nmdrm.cpp -o "$OUT/nmdrm.o"
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext \
  "$OUT/nmdrm.o" -o "$OUT/NMDRM.kext/Contents/MacOS/NMDRM"
cp Info.plist "$OUT/NMDRM.kext/Contents/Info.plist"
echo "NMDRM $(shasum -a 256 "$OUT/NMDRM.kext/Contents/MacOS/NMDRM" | cut -c1-8) $(stat -f %z "$OUT/NMDRM.kext/Contents/MacOS/NMDRM") B"
