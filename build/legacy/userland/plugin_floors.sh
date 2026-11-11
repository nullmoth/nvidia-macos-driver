#!/bin/bash
# floors_metal.sh <source dir name under the audit folder> <tag>: test-build the plugin at every floor. Test builds only.
# AUDIT = the audit folder, MESA_INC = the Mesa include folder (both from the environment).
A=${AUDIT:?}; M=${MESA_INC:?}; SRC=$A/$1; TAG=$2; OUT=$A/floors-$TAG
mkdir -p "$OUT"; cd "$SRC" || exit 2
for T in ${FLOORS:-15.0 14.0 13.0 12.0 11.0 10.15 10.14 10.13 10.12 10.11}; do
  xcrun clang -arch x86_64 -mmacosx-version-min=$T -bundle -fobjc-arc -O0 -DNVMTL_RELEASE=1 -Wno-protocol \
    -Wno-incomplete-implementation -Wunguarded-availability -ferror-limit=0 -I$M -o "$OUT/NVMTLDriver-$T" \
    NVMTLDevice.m nvmtl_dlss.c dlss/elfhost.c dlss/lnxlibc.c \
    -framework Foundation -framework Metal -framework IOSurface -framework IOKit -framework CoreFoundation -weak_framework MetalFX -liconv \
    > "$OUT/build-$T.err" 2>&1
  rc=$?
  minos=$( [ -f "$OUT/NVMTLDriver-$T" ] && xcrun vtool -show-build "$OUT/NVMTLDriver-$T" 2>/dev/null | awk '/minos/{print $2}')
  echo "floor $T rc=$rc errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$OUT/build-$T.err") warnings=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: warning:' "$OUT/build-$T.err") avail=$(grep -c 'is only available on macOS' "$OUT/build-$T.err") minos=${minos:-none} weak=$(nm -m -arch x86_64 "$OUT/NVMTLDriver-$T" 2>/dev/null | grep -c 'weak external')"
done
