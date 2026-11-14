#!/bin/bash
# build.sh <out-dir>: builds the machine learning oracle for arm64 (runs on Apple hardware for the reference result) and
# x86_64 (runs on the NVIDIA driver), and packages the test networks. Packaging needs the Metal toolchain and a Python with
# coremltools (a venv; Python 3.9 has the native extensions), so it only runs when the .mtlpackage files are missing.
# usage on a Mac: ./build.sh out && out/mlpipe_arm64 mlp.mtlpackage weights.txt io2.mtlpackage
set -euo pipefail
OUT=${1:?output directory}; HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
for a in arm64 x86_64; do
  xcrun clang -arch $a -fobjc-arc -mmacosx-version-min=26.0 -Wall "$HERE/mlpipe.m" -o "$OUT/mlpipe_$a" -framework Metal -framework Foundation
done
cp -R "$HERE/mlp.mtlpackage" "$HERE/io2.mtlpackage" "$HERE/weights.txt" "$OUT/"
echo "run: $OUT/mlpipe_arm64 $OUT/mlp.mtlpackage $OUT/weights.txt $OUT/io2.mtlpackage   (exit 0 and 'N arms, 0 failed' on success)"
