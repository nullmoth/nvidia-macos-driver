#!/bin/bash
# build.sh <out-dir>: compile the shader library and the oracle for arm64 (runs on Apple hardware for the reference result)
# and x86_64 (runs on the NVIDIA driver). Needs the Metal toolchain, which is on the build Mac only.
set -euo pipefail
OUT=${1:?output directory}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
xcrun metal -std=metal4.0 -target air64-apple-macos26.0 -c "$HERE/mpp_matmul.metal" -o "$OUT/mpp_matmul.air"
xcrun metallib "$OUT/mpp_matmul.air" -o "$OUT/mpp_matmul.metallib"
for a in arm64 x86_64; do
  xcrun clang -arch $a -fobjc-arc -mmacosx-version-min=26.0 -Wall "$HERE/mpp_matmul.m" -o "$OUT/mpp_matmul_$a" -framework Metal -framework Foundation
done
echo "run: $OUT/mpp_matmul_arm64 $OUT/mpp_matmul.metallib   (exit 0 and 'N arms, 0 failed' on success)"
