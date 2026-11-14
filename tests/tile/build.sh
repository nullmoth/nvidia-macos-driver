#!/bin/bash
# build.sh <outdir>: compile the tile oracles. The metallib and the arm64 binary build here (the laptop is the Apple
# reference); the x86_64 binaries are what runs on the NVIDIA machine, next to the same metallib.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); OUT=${1:?output directory}
mkdir -p "$OUT"
xcrun metal -target air64-apple-macos15.7 -c "$HERE/tile.metal" -o "$OUT/tile.air"
xcrun metallib "$OUT/tile.air" -o "$OUT/tile.metallib"
for src in "$HERE"/*_oracle*.m; do
  n=$(basename "$src" .m)
  min=15.0; case "$n" in *mtl4*) min=26.0;; esac   # the Metal 4 oracle needs the macOS 26 API
  for arch in arm64 x86_64; do
    xcrun clang -fobjc-arc -O1 -arch $arch -mmacosx-version-min=$min -framework Foundation -framework Metal \
      -I"$HERE" "$src" -o "$OUT/$n.$arch"
  done
done
echo "built in $OUT"
