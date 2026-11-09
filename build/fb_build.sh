#!/bin/bash
# fb_build.sh <kexts dir> <out>: build NVRMFB.kext (the framebuffer) from kexts/NVRMFB.
# NVRMFB includes only NVKMS's interface headers and NVIDIA's basic types, and finds the NVKMS function table at run
# time, so it needs no RM build: OGKM=<an open-gpu-kernel-modules checkout with src/nvidia-modeset and src/common>.
# The source has no per-OS branches: one binary serves macOS 15 and 26 (MINVER sets the deployment target).
set -uo pipefail
SRC=$1; OUT=$2; mkdir -p "$OUT/NVRMFB.kext/Contents/MacOS"
OGKM=${OGKM:?set OGKM to the open-gpu-kernel-modules tree}; KMS="$OGKM/src/nvidia-modeset"
SDK=$(xcrun --show-sdk-path); KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
INC=( -I"$KHDR" -I"$SRC/NVRMFB" -I"$SRC/NVRMFB/fb"
      -I"$OGKM/src/nvidia/arch/nvalloc/unix/include" -I"$OGKM/src/common/sdk/nvidia/inc" -I"$OGKM/src/common/inc" -I"$OGKM/src/common/shared/inc"
      -I"$KMS/os-interface/include" -I"$KMS/kapi/interface" -I"$KMS/kapi/include" -I"$KMS/interface" -I"$KMS/include"
      -I"$OGKM/src/common/unix/common/inc" -I"$OGKM/src/common/modeset" -I"$OGKM/src/common/unix/common/utils/interface" )
# NVCPU_X86_64 comes from NVIDIA's cpuopsys.h for an x86_64 target; defining it here too only redefined it
CFLAGS=( -arch x86_64 -mmacosx-version-min=${MINVER:-15.0} -fapple-kext -mkernel -nostdinc "${INC[@]}" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 ${FB_DEFS:-} )
xcrun clang++ "${CFLAGS[@]}" -c "$SRC/NVRMFB/fb/nvrm-fb.cpp" -o "$OUT/nvrm-fb.o" 2>"$OUT/fb.err" || { grep error "$OUT/fb.err" | head -20; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvrmfb "$(grep -c ': warning:' "$OUT/fb.err")" "$OUT/fb.err" || exit 4
xcrun clang++ -arch x86_64 -mmacosx-version-min=${MINVER:-15.0} -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$OUT/nvrm-fb.o" \
  -o "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB" 2>"$OUT/fbld.err" || { head "$OUT/fbld.err"; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvrmfb-link "$(grep -c 'warning' "$OUT/fbld.err")" "$OUT/fbld.err" || exit 4
cp "$SRC/NVRMFB/fb/Info.plist" "$OUT/NVRMFB.kext/Contents/Info.plist"
echo "NVRMFB $(md5 -q "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB") $(stat -f %z "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB") B, warnings $(grep -c ': warning:' "$OUT/fb.err")"
