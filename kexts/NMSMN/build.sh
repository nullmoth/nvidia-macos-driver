#!/bin/bash
# Builds NMSMN, the opt-in SMU mailbox accessor for Ryzen power-limit tools. Usage: build.sh [outdir] [min-macos: 15.0 default, or 26.0]
set -u
SRC=$(cd "$(dirname "$0")" && pwd)
OUT=${1:-$SRC/out}
MIN=${2:-15.0}
mkdir -p "$OUT" || exit 1
SDK=$(xcrun --show-sdk-path)
KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
CFLAGS=( -arch x86_64 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$SRC" 
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT
  -mmacosx-version-min="$MIN" -g0
  -ffile-prefix-map="$SRC"=driver -ffile-prefix-map="$HOME"=/build
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 )
NAME=NMSMN
E="$OUT/$NAME.err"
xcrun clang++ "${CFLAGS[@]}" -c "$SRC/nmsmn.cpp" -o "$OUT/$NAME.o" 2> "$E"
rc=$?
if [ $rc -ne 0 ]; then
    echo "COMPILE FAILED (rc=$rc), $(grep -c "error:" "$E") errors, $(grep -c "warning:" "$E") warnings"
    grep "error:" "$E" | head -12
    exit 1
fi
echo "compiled: $(grep -c "warning:" "$E") warnings"
bash "$SRC/../../build/warn_ceiling.sh" nmsmn "$(grep -c ": warning:" "$E")" "$E" || exit 4
K="$OUT/$NAME.kext"
mkdir -p "$K/Contents/MacOS"
xcrun clang++ -arch x86_64 -mmacosx-version-min="$MIN" -fapple-kext -nostdlib \
    -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$OUT/$NAME.o" -o "$K/Contents/MacOS/$NAME" 2> "$OUT/$NAME.link.err"
lrc=$?
if [ $lrc -ne 0 ]; then echo "LINK FAILED (rc=$lrc)"; head -12 "$OUT/$NAME.link.err"; exit 1; fi
bash "$SRC/../../build/warn_ceiling.sh" nmsmn-link "$(grep -c 'warning' "$OUT/$NAME.link.err")" "$OUT/$NAME.link.err" || exit 4
cp "$SRC/Info.plist" "$K/Contents/Info.plist"
echo "$(shasum -a 256 "$K/Contents/MacOS/$NAME" | cut -c1-16)  $(stat -f %z "$K/Contents/MacOS/$NAME") B"
echo "kmod_info:    $(nm "$K/Contents/MacOS/$NAME" | grep -c "_kmod_info")"
echo "metaclasses:  $(nm "$K/Contents/MacOS/$NAME" | grep -c "gMetaClass")"
echo "undefined:    $(nm -u "$K/Contents/MacOS/$NAME" | wc -l | tr -d ' ') symbols"
plutil -lint "$K/Contents/Info.plist"
