#!/bin/bash
# fb_build.sh <kexts dir> <out>: build NVRMFB.kext (the framebuffer) from kexts/NVRMFB, the way build/accel_build.sh builds
# NVAccel. NVRMFB includes only NVKMS's interface headers (nvkms.h, nvkms-kapi.h) and NVIDIA's basic types from
# open-gpu-kernel-modules 615.78.08 (the RM version of NullMoth 2.5.0; 610.57.04 for 2.4.x), and links against nothing of NVRM's (it finds the NVKMS function table at run time),
# so it needs no RM build: OGKM=<a 615.78.08 checkout with src/nvidia-modeset and src/common>.
set -uo pipefail
SRC=$1; OUT=$2; mkdir -p "$OUT/NVRMFB.kext/Contents/MacOS"
OGKM=${OGKM:-$HOME/ogkm615}; KMS="$OGKM/src/nvidia-modeset"
SDK=$(xcrun --show-sdk-path); KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
INC=( -I"$KHDR" -I"$SRC/NVRMFB" -I"$SRC/NVRMFB/fb"
      -I"$OGKM/src/nvidia/arch/nvalloc/unix/include" -I"$OGKM/src/common/sdk/nvidia/inc" -I"$OGKM/src/common/inc" -I"$OGKM/src/common/shared/inc"
      -I"$KMS/os-interface/include" -I"$KMS/kapi/interface" -I"$KMS/kapi/include" -I"$KMS/interface" -I"$KMS/include"
      -I"$OGKM/src/common/unix/common/inc" -I"$OGKM/src/common/modeset" -I"$OGKM/src/common/unix/common/utils/interface" )
CFLAGS=( -arch x86_64 -fapple-kext -mkernel -nostdinc "${INC[@]}" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT
  -DNVCPU_X86_64 -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 ${FB_DEFS:-} )
xcrun clang++ "${CFLAGS[@]}" -c "$SRC/NVRMFB/fb/nvrm-fb.cpp" -o "$OUT/nvrm-fb.o" 2>"$OUT/fb.err" || { grep error "$OUT/fb.err" | head -20; exit 1; }
xcrun clang++ -arch x86_64 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$OUT/nvrm-fb.o" \
  -o "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB" 2>"$OUT/fbld.err" || { head "$OUT/fbld.err"; exit 1; }
cp "$SRC/NVRMFB/fb/Info.plist" "$OUT/NVRMFB.kext/Contents/Info.plist"
echo "NVRMFB $(md5 -q "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB") $(stat -f %z "$OUT/NVRMFB.kext/Contents/MacOS/NVRMFB") B, warnings $(grep -c ': warning:' "$OUT/fb.err")  ($(xcrun clang++ --version | head -1))"
