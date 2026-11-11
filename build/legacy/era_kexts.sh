#!/bin/bash
# era_kexts.sh <era MMmm> <min version> <Kernel.framework Headers of that release> <out dir>
# Test build of NVRM, NVRMFB and NVRMAGDC objects for an older macOS: the era's own kernel headers, the current
# graphics types header ahead of them, and kexts/compat/nm_era.h force-included. Objects and logs only.
set -u
ERA=$1; MV=$2; KHDR=$3; O=$4; mkdir -p "$O/shim/IOKit/graphics"
SRC=${SRC:?kexts dir}; OGKM=${OGKM:?patched open-gpu-kernel-modules tree with an RM build}
NV=$OGKM/src/nvidia; KMS=$OGKM/src/nvidia-modeset; RES="$(xcrun clang -print-resource-dir)/include"
cp "$(xcrun --show-sdk-path)/System/Library/Frameworks/Kernel.framework/Headers/IOKit/graphics/IOGraphicsTypes.h" "$O/shim/IOKit/graphics/"
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
RX='^[^ ]+:[0-9]+:[0-9]+: (fatal )?error: '
cc() { local o=$1 f=$2; shift 2
  xcrun clang++ -arch x86_64 -mmacosx-version-min=$MV -fapple-kext -mkernel -nostdinc -I"$O/shim" -I"$KHDR" -idirafter "$RES" \
    -DNM_ERA=$ERA -include "$SRC/compat/nm_era.h" "$@" "${RMINC[@]}" "${KMSINC[@]}" \
    -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT "${RMDEFS[@]}" -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common \
    -Wno-everything -ferror-limit=0 -O2 -c "$f" -o "$O/$o.o" > "$O/$o.err" 2>&1; }
fail=0
for f in "$SRC"/NVRM/*.cpp; do cc NVRM-$(basename "$f" .cpp) "$f" -I"$SRC/NVRM"; done
cc NVRMFB "$SRC/NVRMFB/fb/nvrm-fb.cpp" -I"$SRC/NVRMFB" -I"$SRC/NVRMFB/fb"
cc NVRMAGDC "$SRC/NVRMAGDC/nvrm-agdc.cpp" -I"$SRC/NVRMAGDC"
# The three C sources of NVRM: one with the kext's own flags, two with the RM core's command line (rmcc.py).
xcrun clang -arch x86_64 -mmacosx-version-min=$MV -mkernel -nostdinc -I"$O/shim" -I"$KHDR" -idirafter "$RES" -I"$SRC/NVRM" "${RMINC[@]}" "${KMSINC[@]}" \
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT "${RMDEFS[@]}" -x c -std=gnu11 -fno-builtin -fno-common -Wno-everything -O2 \
  -c "$SRC/NVRM/nvrm_kapi_fields.c" -o "$O/NVRM-c-kapi_fields.o" > "$O/NVRM-c-kapi_fields.err" 2>&1
for c in nvrm_pageoff nvrm_pmgr; do
  OGKM="$OGKM" python3 -B "$SRC/NVRM/rmcc.py" "$SRC/NVRM/$c.c" "$O/NVRM-c-$c.o" > "$O/NVRM-c-$c.err" 2>&1 || echo "era $ERA: $c.c did not compile (see $O/NVRM-c-$c.err)"
done
for k in NVRM- NVRMFB NVRMAGDC; do
  e=$(cat "$O"/$k*.err | grep -cE "$RX"); [ "$e" = 0 ] || fail=1
  echo "era $ERA ${k%-}: errors=$e objects=$(ls "$O"/$k*.o 2>/dev/null | wc -l | tr -d ' ')"
  cat "$O"/$k*.err | grep -E "$RX" | sed -E 's|^.*/kexts[^/]*/||' | cut -c1-170 | head -${SHOW:-12}
done
# Link what compiled. The undefined-symbol list of each kext is the input to the per-release symbol check.
link() { local name=$1; shift; mkdir -p "$O/$name.kext/Contents/MacOS"
  xcrun clang++ -arch x86_64 -mmacosx-version-min=$MV -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$@" \
    -o "$O/$name.kext/Contents/MacOS/$name" > "$O/$name.ld.err" 2>&1
  local rc=$?; [ $rc = 0 ] || { echo "era $ERA $name: LINK FAILED"; grep -v "built for newer" "$O/$name.ld.err" | head -8; fail=1; return; }
  xcrun nm -arch x86_64 -u "$O/$name.kext/Contents/MacOS/$name" > "$O/$name.imports.txt"
  echo "era $ERA $name: linked, $(stat -f %z "$O/$name.kext/Contents/MacOS/$name") bytes, $(wc -l < "$O/$name.imports.txt" | tr -d ' ') imports, min version $(xcrun otool -l "$O/$name.kext/Contents/MacOS/$name" | grep -A3 -E 'LC_VERSION_MIN_MACOSX|LC_BUILD_VERSION' | awk '/minos|version /{print $2; exit}')"
}
[ "$(cat "$O"/NVRM-*.err | grep -cE "$RX")" = 0 ] && link NVRM "$O"/NVRM-*.o "$NV/_out/Darwin_x86_64/libnvkernel.a" "$KMS/_out/Darwin_x86_64/libnvkms.a"
[ "$(grep -cE "$RX" "$O/NVRMFB.err")" = 0 ] && link NVRMFB "$O/NVRMFB.o"
[ "$(grep -cE "$RX" "$O/NVRMAGDC.err")" = 0 ] && link NVRMAGDC "$O/NVRMAGDC.o"
exit $fail
