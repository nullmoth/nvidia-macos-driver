#!/bin/bash
# Builds NMIntelFB, the framebuffer that adopts the mode the firmware already lit on an Intel laptop panel.
# Flags are the discrete framebuffer's, minus the NVIDIA resource-manager includes, which this file does not use.
# Both personalities are built: the real one, and the virtual test build whose panel is attached on demand
# through a sysctl (the one to try first on a machine whose screen must not go dark).
set -u
SRC=$(cd "$(dirname "$0")" && pwd)
OUT=${1:-$SRC/out}
mkdir -p "$OUT" || exit 1

SDK=$(xcrun --show-sdk-path)
KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
CFLAGS=( -arch x86_64 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$SRC"
  -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT
  -mmacosx-version-min=15.0 -g0
  -ffile-prefix-map="$SRC"=driver -ffile-prefix-map="$HOME"=/build
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common
  -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 )

for VARIANT in real virtual; do
    if [ "$VARIANT" = virtual ]; then
        DEFS=( -DNMINTELFB_VIRTUAL ); PLIST=$SRC/Info-virtual.plist; NAME=NMIntelFBVirtual
    else
        # -U rather than an empty array: this shell is 3.2, where "${EMPTY[@]}" under set -u is an error
        DEFS=( -UNMINTELFB_VIRTUAL ); PLIST=$SRC/Info.plist; NAME=NMIntelFB
    fi
    echo "=== $NAME"
    E="$OUT/$NAME.err"
    xcrun clang++ "${CFLAGS[@]}" "${DEFS[@]}" -c "$SRC/intelfb.cpp" -o "$OUT/$NAME.o" 2> "$E"
    rc=$?
    if [ $rc -ne 0 ]; then
        echo "  COMPILE FAILED (rc=$rc), $(grep -c "error:" "$E") errors, $(grep -c "warning:" "$E") warnings"
        grep "error:" "$E" | head -12
        continue
    fi
    echo "  compiled: $(grep -c "warning:" "$E") warnings"
    K="$OUT/$NAME.kext"
    mkdir -p "$K/Contents/MacOS"
    xcrun clang++ -arch x86_64 -mmacosx-version-min=15.0 -fapple-kext -nostdlib \
        -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$OUT/$NAME.o" -o "$K/Contents/MacOS/$NAME" 2> "$OUT/$NAME.link.err"
    lrc=$?
    if [ $lrc -ne 0 ]; then
        echo "  LINK FAILED (rc=$lrc)"; head -12 "$OUT/$NAME.link.err"; continue
    fi
    cp "$PLIST" "$K/Contents/Info.plist"
    # A kext without _kmod_info is silently left out of a kernel collection, and the metaclasses are what prove
    # the IOKit classes survived the link.
    echo "  $(shasum -a 256 "$K/Contents/MacOS/$NAME" | cut -c1-16)  $(stat -f %z "$K/Contents/MacOS/$NAME") B"
    echo "  kmod_info:    $(nm "$K/Contents/MacOS/$NAME" | grep -c "_kmod_info")"
    echo "  metaclasses:  $(nm "$K/Contents/MacOS/$NAME" | grep -c "gMetaClass")"
    echo "  undefined:    $(nm -u "$K/Contents/MacOS/$NAME" | wc -l | tr -d ' ') symbols"
    plutil -lint "$K/Contents/Info.plist" | sed 's/^/  /'
done
