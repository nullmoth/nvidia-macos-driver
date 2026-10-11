#!/bin/bash
# ccxx.sh <source.cpp>: compiles one IOKit-side file of the port with Apple's Kernel.framework headers only
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
mkdir -p $X/xnuobj
exec xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -isysroot "$SDK" -I"$KF" \
  -I$X/bsdcompat/include/nmxxnu -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -O2 -g0 -Wall -Wno-unused-parameter \
  -Wno-deprecated-declarations -Wno-inconsistent-missing-override -c "$1" -o $X/xnuobj/$(basename "${1%.cpp}").o
