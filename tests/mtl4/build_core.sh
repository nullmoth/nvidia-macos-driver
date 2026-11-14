#!/bin/bash
# build.sh <out-dir>: compile the shader library and the oracle for arm64 (runs on Apple hardware for the reference
# result) and x86_64 (runs on the NVIDIA driver). Needs the Metal toolchain, which is on the build Mac only.
set -euo pipefail
OUT=${1:?output directory}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
xcrun metal -target air64-apple-macos15.7 -c "$HERE/mtl4core.metal" -o "$OUT/mtl4core.air"
xcrun metallib "$OUT/mtl4core.air" -o "$OUT/mtl4core.metallib"
for a in arm64 x86_64; do
  xcrun clang -arch $a -fobjc-arc -mmacosx-version-min=26.0 -Wall "$HERE/mtl4core.m" -o "$OUT/mtl4core_$a" -framework Metal -framework Foundation -framework QuartzCore
done
echo "run: $OUT/mtl4core_arm64 $OUT/mtl4core.metallib   (exit 0 and 'N arms, 0 failed' on success)"
