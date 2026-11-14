#!/bin/bash
# Builds the Metal 4 compiler oracle for arm64 and x86_64 into ./out and compiles its shader library.
set -e
cd "$(dirname "$0")"
mkdir -p out
xcrun metal -target air64-apple-macos15.7 -c mtl4_compiler.metal -o out/mtl4_compiler.air
xcrun metallib out/mtl4_compiler.air -o out/mtl4_compiler.metallib
for a in arm64 x86_64; do
  clang -arch $a -mmacosx-version-min=26.0 -fobjc-arc -Wall -o out/mtl4_compiler_$a mtl4_compiler.m -framework Foundation -framework Metal
done
clang -arch arm64 -mmacosx-version-min=26.0 -fobjc-arc -Wall -Wno-unused-function -o out/codec_test codec_test.m -framework Foundation -framework Metal
echo "run: out/codec_test out/mtl4_compiler.metallib; "
echo "run: out/mtl4_compiler_arm64 out/mtl4_compiler.metallib"
