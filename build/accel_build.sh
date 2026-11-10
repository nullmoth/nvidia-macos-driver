#!/bin/bash
set -uo pipefail
SRC=$1; OUT=$2; mkdir -p "$OUT/NVAccel.kext/Contents/MacOS"
OGKM=${OGKM:-$HOME/ogkm610}; RE=${RE:-$SRC/accel/re}   # the reconstructed IOGraphicsAccelerator2 / IOAccelerator headers ship in the repo
SDK=$(xcrun --show-sdk-path); KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"; NV="$OGKM/src/nvidia"; KMS="$OGKM/src/nvidia-modeset"
RMINC=( -I"$NV/arch/nvalloc/unix/include" -I"$NV/arch/nvalloc/common/inc" -I"$NV/arch/nvalloc/common/inc/gsp"
  -I"$OGKM/src/common/sdk/nvidia/inc" -I"$OGKM/src/common/sdk/nvidia/inc/hw" -I"$OGKM/src/common/inc"
  -I"$OGKM/src/common/shared/inc" -I"$NV/inc" -I"$NV/inc/os" -I"$NV/inc/kernel" -I"$NV/kernel/inc" -I"$NV/interface"
  -I"$OGKM/src/common/nvlink/interface" -I"$OGKM/src/common/inc/swref" -I"$OGKM/src/common/inc/swref/published"
  -I"$NV/inc/libraries" -I"$NV/src/libraries" -I"$NV/generated" -I"$NV/src/mm/uvm/interface"
  -I"$OGKM/src/common/uproc/os/libos-v2.0.0/include" -I"$OGKM/src/common/uproc/os/common/include"
  -I"$OGKM/src/common/inc/displayport" -I"$OGKM/src/common/nvlink/inband/interface" )
KMSINC=( -I"$KMS/os-interface/include" -I"$KMS/kapi/interface" -I"$KMS/kapi/include" -I"$OGKM/src/common/unix/nvidia-push/interface" -I"$KMS/interface" -I"$KMS/include"
         -I"$OGKM/src/common/unix/common/inc" -I"$OGKM/src/common/modeset" -I"$OGKM/src/common/unix/common/utils/interface" )
RMDEFS=(); while read -r d; do RMDEFS+=("$d"); done < <(head -1 "$NV/_out/Darwin_x86_64/compile_cmds.sh" | tr ' ' '\n' | grep -E '^-D' | sed -e 's/"//g' | grep -v -E '^-D(NVRM|_LANGUAGE_C)$')
CFLAGS=( -arch x86_64 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$SRC" "${RMINC[@]}" "${KMSINC[@]}"
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT "${RMDEFS[@]}" ${NM_TAHOE:+-DNM_TAHOE}   # NM_TAHOE=1: the macOS 26 build
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 )
xcrun clang++ "${CFLAGS[@]}" -I"$SRC/accel" -I"$SRC/accel/iofam" -I"$RE" -c "$SRC/accel/nvrm-accel.cpp" -o "$OUT/nvrm-accel.o" 2>"$OUT/accel.err" || { grep error "$OUT/accel.err" | head; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvaccel "$(grep -c ': warning:' "$OUT/accel.err")" "$OUT/accel.err" || exit 4
SMCINC=( -I"$SRC/accel/smc/vendor" -I"$SRC/accel/smc/vendor/Lilu" )
xcrun clang++ "${CFLAGS[@]}" "${SMCINC[@]}" -c "$SRC/accel/nvrm_smc_bridge.cpp" -o "$OUT/nvrm-smc.o" 2>"$OUT/smc.err" || { cat "$OUT/smc.err"; exit 1; }
xcrun clang++ "${CFLAGS[@]}" "${SMCINC[@]}" -c "$SRC/accel/smc/nvrm_vsmc_value.cpp" -o "$OUT/nvrm-smc-value.o" 2>"$OUT/smc-value.err" || { cat "$OUT/smc-value.err"; exit 1; }
xcrun clang++ -arch x86_64 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$OUT/nvrm-accel.o" "$OUT/nvrm-smc.o" "$OUT/nvrm-smc-value.o" -o "$OUT/NVAccel.kext/Contents/MacOS/NVAccel" 2>"$OUT/accelld.err" || { cat "$OUT/accelld.err" | head; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvaccel-link "$(grep -c 'warning' "$OUT/accelld.err")" "$OUT/accelld.err" || exit 4
cp "$SRC/accel/Info.plist" "$OUT/NVAccel.kext/Contents/Info.plist"
echo "NVAccel $(md5 -q "$OUT/NVAccel.kext/Contents/MacOS/NVAccel") $(stat -f %z "$OUT/NVAccel.kext/Contents/MacOS/NVAccel") B  ($(xcrun clang++ --version | head -1))"
