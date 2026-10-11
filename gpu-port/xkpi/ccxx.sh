#!/bin/bash
# ccxx.sh <source.cpp>: compiles one IOKit-side file of the port with Apple's Kernel.framework headers only
X=$NULLMOTH_BUILD_ROOT/build/gpu-src/xkpi
SDK=$(xcrun --sdk macosx --show-sdk-path)
KF=$SDK/System/Library/Frameworks/Kernel.framework/Headers
mkdir -p $X/xnuobj
# nmxxnu/ holds the two boundary headers for the IOKit side (which must not see the FreeBSD include tree); they are
# copies of bsdcompat/include's, refreshed here so a declaration added there always reaches the C++ side
cp -p $X/bsdcompat/include/nmx_port.h $X/bsdcompat/include/nmx_xnu.h $X/bsdcompat/include/nmxxnu/
exec xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -ffile-prefix-map=$NULLMOTH_BUILD_ROOT/build/gpu-src=/src -mkernel -fapple-kext -nostdinc -isysroot "$SDK" -I"$KF" \
  -I$X/bsdcompat/include/nmxxnu -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -O2 -g0 -Wall -Wno-unused-parameter \
  -Wno-deprecated-declarations -Wno-inconsistent-missing-override -c "$1" -o ${NMX_OBJ:-$X/xnuobj/$(basename "${1%.cpp}").o}
