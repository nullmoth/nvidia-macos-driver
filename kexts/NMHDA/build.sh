#!/bin/bash
# build.sh <Lilu DEBUG kext (its Contents/Resources holds the SDK)> <MacKernelSDK checkout> <out dir>  ->  <out>/NMHDA.kext
# Lilu plugins build against acidanthera's MacKernelSDK (BSD-3), not Apple's Kernel.framework headers.
set -euo pipefail
cd "$(dirname "$0")"
LILU=$1; MKSDK=$2; OUT=$3; SDK="$LILU/Contents/Resources"
[ -f "$SDK/Headers/plugin_start.hpp" ] || { echo "STOP: no Lilu SDK in $LILU (use the DEBUG Lilu.kext)"; exit 1; }
K="$MKSDK/Headers"; [ -d "$K" ] || { echo "STOP: no MacKernelSDK at $MKSDK"; exit 1; }
mkdir -p "$OUT/NMHDA.kext/Contents/MacOS"
FLAGS=(-arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -mkernel -nostdinc -I"$K" -I"$SDK"
       -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT -DPRODUCT_NAME=NMHDA -DMODULE_VERSION=1.0.0
       -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -O2 -Wall -Wno-unused-parameter -g0)
xcrun clang++ "${FLAGS[@]}" -c nmhda.cpp -o "$OUT/nmhda.o"
# Lilu's bundled startup source carries its own conversion and shadow warnings.
xcrun clang++ "${FLAGS[@]}" -Wno-sign-conversion -Wno-shadow -c "$SDK/Library/plugin_start.cpp" -o "$OUT/plugin_start.o"
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext -L"$MKSDK/Library/x86_64" -lkmodc++ -lkmod -lcc_kext \
  "$OUT/nmhda.o" "$OUT/plugin_start.o" -o "$OUT/NMHDA.kext/Contents/MacOS/NMHDA"
cp Info.plist "$OUT/NMHDA.kext/Contents/Info.plist"
echo "NMHDA $(shasum -a 256 "$OUT/NMHDA.kext/Contents/MacOS/NMHDA" | cut -c1-8) $(stat -f %z "$OUT/NMHDA.kext/Contents/MacOS/NMHDA") B"
