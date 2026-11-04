#!/bin/bash
# build.sh <out-dir>: compile the shader library and the oracle for arm64 (Apple reference) and x86_64 (NVIDIA driver).
# Needs the Metal toolchain. The library targets macOS 26 so both the Apple driver and the plugin load it.
set -euo pipefail
OUT=${1:?output directory}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
xcrun metal -target air64-apple-macos26.0 -c "$HERE/gaps26.metal" -o "$OUT/gaps26.air"
xcrun metallib "$OUT/gaps26.air" -o "$OUT/gaps26.metallib"
for a in arm64 x86_64; do
  xcrun clang -arch $a -fobjc-arc -mmacosx-version-min=26.0 -Wall -Wno-unguarded-availability-new "$HERE/gaps26.m" \
    -o "$OUT/gaps26_$a" -framework Metal -framework Foundation
done
echo "run: $OUT/gaps26_<arch> $OUT/gaps26.metallib   (exit 0 = every arm passed; last line RESULT)"
