#!/bin/bash
# build.sh <Lilu DEBUG kext> <VirtualSMC source> <MacKernelSDK> <out dir>  ->  <out>/NMKbdBacklight.kext
# The keyboard backlight plugin, x86_64, linked the way VirtualSMC's own sensor plugins are: against the Lilu SDK the DEBUG
# kext carries (Contents/Resources), VirtualSMC's SDK headers and acidanthera's MacKernelSDK (all BSD-3-Clause). A plugin
# must match the VirtualSMC it loads into, so build it against the commits the EFI's VirtualSMC set is built from.
# OpenCore injects it from the EFI after VirtualSMC; it does nothing unless nmkbl=1 is on the boot line.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
LILU=${1:?Lilu DEBUG kext}; VSMC=${2:?VirtualSMC source}; MKSDK=${3:?MacKernelSDK}; OUT=${4:?out dir}
SDK="$LILU/Contents/Resources"
[ -f "$SDK/Headers/kern_util.hpp" ] || { echo "STOP: no Lilu SDK in $LILU (use the DEBUG Lilu.kext)"; exit 1; }
[ -f "$VSMC/VirtualSMCSDK/kern_vsmcapi.hpp" ] || { echo "STOP: $VSMC is not a VirtualSMC source tree"; exit 1; }
[ -f "$MKSDK/Library/x86_64/libkmod.a" ] || { echo "STOP: no MacKernelSDK at $MKSDK"; exit 1; }
VER=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$HERE/Info.plist")
K="$OUT/NMKbdBacklight.kext/Contents"
rm -rf "$OUT/NMKbdBacklight.kext"; mkdir -p "$K/MacOS"; : > "$OUT/kbl.err"
FLAGS=(-arch x86_64 -mmacosx-version-min=10.13 -mkernel -fno-builtin -nostdinc -isysroot "$(xcrun --sdk macosx --show-sdk-path)"
       -Wall -Wextra -Wno-unused-parameter -Werror=return-type -O2 -g0
       -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT -DPRODUCT_NAME=NMKbdBacklight -DMODULE_VERSION="$VER"
       -I "$MKSDK/Headers" -I "$SDK" -I "$VSMC" -I "$HERE")
for s in NMKbdBacklight NMKbdBacklightHID; do
  xcrun clang++ "${FLAGS[@]}" -std=c++17 -fno-rtti -fno-exceptions -nostdinc++ -fapple-kext -c "$HERE/$s.cpp" -o "$OUT/$s.o" 2>> "$OUT/kbl.err" \
    || { cat "$OUT/kbl.err"; exit 1; }
done
# the module record Xcode would generate: start and stop are the plugin's own, as in VirtualSMC's sensor plugins
cat > "$OUT/NMKbdBacklight_info.c" <<INFO
#include <mach/mach_types.h>
extern kern_return_t _start(kmod_info_t *ki, void *data);
extern kern_return_t _stop(kmod_info_t *ki, void *data);
__private_extern__ kern_return_t NMKbdBacklight_kern_start(kmod_info_t *ki, void *data);
__private_extern__ kern_return_t NMKbdBacklight_kern_stop(kmod_info_t *ki, void *data);
__attribute__((visibility("default"))) KMOD_EXPLICIT_DECL(com.nullmoth.NMKbdBacklight, "$VER", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = NMKbdBacklight_kern_start;
__private_extern__ kmod_stop_func_t *_antimain = NMKbdBacklight_kern_stop;
__private_extern__ int _kext_apple_cc = __APPLE_CC__ ;
INFO
xcrun clang "${FLAGS[@]}" -c "$OUT/NMKbdBacklight_info.c" -o "$OUT/info.o" 2>> "$OUT/kbl.err" || { cat "$OUT/kbl.err"; exit 1; }
bash "$HERE/../../build/warn_ceiling.sh" nmkbdbacklight "$(grep -c ': warning:' "$OUT/kbl.err" || true)" "$OUT/kbl.err"
xcrun clang++ -arch x86_64 -mmacosx-version-min=10.13 -fapple-kext -nostdlib -Xlinker -kext -L"$MKSDK/Library/x86_64" -lkmodc++ -lkmod -lcc_kext \
  "$OUT/NMKbdBacklight.o" "$OUT/NMKbdBacklightHID.o" "$OUT/info.o" -o "$K/MacOS/NMKbdBacklight" 2> "$OUT/link.err" || { cat "$OUT/link.err"; exit 1; }
bash "$HERE/../../build/warn_ceiling.sh" nmkbdbacklight-link "$(grep -c 'warning' "$OUT/link.err" || true)" "$OUT/link.err"
cp "$HERE/Info.plist" "$K/Info.plist"
bash "$HERE/../../build/check_kext_libraries.sh" "$OUT/NMKbdBacklight.kext"
nm "$K/MacOS/NMKbdBacklight" | grep -q ' _kmod_info$' || { echo "STOP: the kext has no _kmod_info"; exit 1; }
echo "NMKbdBacklight $VER $(shasum -a 256 "$K/MacOS/NMKbdBacklight" | cut -c1-8) $(stat -f %z "$K/MacOS/NMKbdBacklight") B"
