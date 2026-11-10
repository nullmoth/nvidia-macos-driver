#!/bin/bash
# build_agdc.sh [src dir] [out dir]: NVRMAGDC.kext (macOS 15 and later, x86_64) from kexts/NVRMAGDC of this tree into
# <out>/NVRMAGDC.kext. With no arguments it builds this tree's source into build-out/agdc.
set -u
HERE=$(cd "$(dirname "$0")/.." && pwd)
D=${1:-$HERE/kexts/NVRMAGDC}; B=${2:-$HERE/build-out/agdc}
[ -f "$D/nvrm-agdc.cpp" ] && [ -f "$D/Info.plist" ] || { echo "STOP: $D is not the NVRMAGDC source folder"; exit 2; }
rm -rf "$B"; mkdir -p "$B/NVRMAGDC.kext/Contents/MacOS"
SDK="$(xcrun --show-sdk-path)"; KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$D" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 \
  -c "$D/nvrm-agdc.cpp" -o "$B/k1.o" 2>"$B/cc.err" || { grep error "$B/cc.err" | head; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvrmagdc "$(grep -c ': warning:' "$B/cc.err")" "$B/cc.err" || exit 4
xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$B/k1.o" -o "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC" 2>"$B/ld.err" || { head "$B/ld.err"; exit 1; }
bash "$(dirname "$0")/warn_ceiling.sh" nvrmagdc-link "$(grep -c 'warning' "$B/ld.err")" "$B/ld.err" || exit 4
cp "$D/Info.plist" "$B/NVRMAGDC.kext/Contents/Info.plist"
echo "NVRMAGDC $(md5 -q "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC") $(dwarfdump --uuid "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC" | awk '{print $2}')"
