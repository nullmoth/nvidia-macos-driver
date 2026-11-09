#!/bin/bash
# build_primary_selector.sh <out>: optional primary-GPU selector for a release package.
# Install under Library/Extensions/NVPrimarySelector.kext only when opted in.
set -euo pipefail
BASE=$(cd "$(dirname "$0")/.." && pwd)
OUT=$1; SRC="$BASE/kexts/NVPrimarySelector"
SDK=$(xcrun --show-sdk-path)
KHDR=${KHDR:-$SDK/System/Library/Frameworks/Kernel.framework/Headers}
KLIB=${KLIB:-$SDK/usr/lib}
mkdir -p "$OUT/NVPrimarySelector.kext/Contents/MacOS"
xcrun clang++ -arch x86_64 -mmacosx-version-min=10.15 -fapple-kext -mkernel -nostdinc \
 -I"$KHDR" -I"$SRC" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
 -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -fno-stack-protector \
 -O2 -Wall -Wextra -c "$SRC/NVPrimarySelector.cpp" -o "$OUT/NVPrimarySelector.o" 2> "$OUT/NVPrimarySelector.err" \
 || { cat "$OUT/NVPrimarySelector.err"; exit 1; }
bash "$BASE/build/warn_ceiling.sh" primary-selector "$(grep -c ': warning:' "$OUT/NVPrimarySelector.err" || true)" "$OUT/NVPrimarySelector.err"
xcrun clang++ -arch x86_64 -mmacosx-version-min=10.15 -fapple-kext -nostdlib -Xlinker -kext \
 "$OUT/NVPrimarySelector.o" -L"$KLIB" -lkmod -o "$OUT/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector" 2> "$OUT/link.err" \
 || { cat "$OUT/link.err"; exit 1; }
bash "$BASE/build/warn_ceiling.sh" primary-selector-link "$(grep -c 'warning' "$OUT/link.err" || true)" "$OUT/link.err"
cp "$SRC/Info.plist" "$OUT/NVPrimarySelector.kext/Contents/Info.plist"
chmod 755 "$OUT/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector"
xattr -cr "$OUT/NVPrimarySelector.kext"
codesign --force --sign - "$OUT/NVPrimarySelector.kext"
codesign --verify --strict "$OUT/NVPrimarySelector.kext"
