#!/bin/bash
# Builds the Metal 4 extras oracle for arm64 (runs on the Apple GPU) and x86_64 (runs on the NVIDIA box).
# The metallib is compiled from source with the macOS 15.7 target so both the Apple driver and the plugin load it.
set -euo pipefail
cd "$(dirname "$0")"
OUT=${1:-build}
mkdir -p "$OUT"
xcrun metal -target air64-apple-macos15.7 -c mtl4_extras.metal -o "$OUT/mtl4_extras.air"
xcrun metallib "$OUT/mtl4_extras.air" -o "$OUT/mtl4_extras.metallib"
for arch in arm64 x86_64; do
  xcrun clang -fobjc-arc -arch $arch -mmacosx-version-min=26.0 -Wno-deprecated-declarations -O1 \
    mtl4_extras_oracle.m -framework Metal -framework Foundation -o "$OUT/mtl4_extras_oracle_$arch"
done
for arch in arm64 x86_64; do
  xcrun clang -fobjc-arc -arch $arch -mmacosx-version-min=26.0 list_protocol_selectors.m -framework Metal -framework Foundation -o "$OUT/list_protocol_selectors_$arch"
done
echo "built $OUT/mtl4_extras_oracle_{arm64,x86_64} and $OUT/mtl4_extras.metallib"
