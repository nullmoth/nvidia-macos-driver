#!/bin/bash
# build_kext.sh <audit base (holds sdk/ and kdk/)> <out dir> <repo root (for kexts/compat and build/legacy)>
# Compiles and links the accelerator stub against the kernel headers of 10.7 and 10.11 (the era build pattern of
# build/legacy/era_kexts.sh: the release's own Kernel.framework headers, the compiler's builtin headers after them,
# kexts/compat/nm_era.h force-included), then checks every import against that release's real kernel and graphics
# family with build/legacy/era_symcheck.py. The static assertions in the source compare the contract header with
# each release's kernel headers, so a clean compile is also the proof that the two agree.
set -uo pipefail
B=${1:?}; OUT=${2:?}; ROOT=${3:?}; H=$(cd "$(dirname "$0")" && pwd); fail=0
K=System/Library/Frameworks/Kernel.framework/Versions/A/Headers; RES="$(xcrun clang -print-resource-dir)/include"
while read era mv kit tag khdr; do
  O=$OUT/$mv; rm -rf "$O"; mkdir -p "$O/NMLegacyAccel.kext/Contents/MacOS"
  xcrun clang++ -arch x86_64 -mmacosx-version-min=$mv -fapple-kext -mkernel -nostdinc -I"$B/sdk/$khdr" -idirafter "$RES" \
    -DNM_ERA=$era -include "$ROOT/kexts/compat/nm_era.h" -I"$H" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
    -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -Wall -Wextra -Wno-unused-parameter -Wno-everything -ferror-limit=0 -O2 \
    -c "$H/NMLegacyAccel.cpp" -o "$O/NMLegacyAccel.o" > "$O/compile.log" 2>&1; rc=$?
  e=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/compile.log")
  echo "release $mv: compile rc=$rc errors=$e"; [ $rc = 0 ] || { fail=1; grep -E 'error:' "$O/compile.log" | head -6 | cut -c1-220; continue; }
  xcrun clang++ -arch x86_64 -mmacosx-version-min=$mv -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$O/NMLegacyAccel.o" \
    -o "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" > "$O/link.log" 2>&1; rc=$?
  [ $rc = 0 ] || { echo "release $mv: LINK FAILED"; grep -v 'built for newer' "$O/link.log" | head -6; fail=1; continue; }
  cp "$H/Info.plist" "$O/NMLegacyAccel.kext/Contents/"
  xcrun nm -arch x86_64 -u "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" > "$O/NMLegacyAccel.imports.txt"
  echo "release $mv: linked, $(wc -l < "$O/NMLegacyAccel.imports.txt" | xargs) imports, classes $(xcrun nm -arch x86_64 "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" | grep -c 'gMetaClassE$')"
  python3 "$ROOT/build/legacy/era_symcheck.py" "$O" "$B/kdk/$kit" "$tag" 2>&1 | cut -c1-160 || fail=1
  # control for the import check: one invented import must be reported
  mkdir -p "$O/control"; (cat "$O/NMLegacyAccel.imports.txt"; echo __ZN20NotARealKernelSymbolEv) > "$O/control/Control.imports.txt"
  python3 "$ROOT/build/legacy/era_symcheck.py" "$O/control" "$B/kdk/$kit" "$tag" 2>&1 | grep -E "^Control|NotAReal" | cut -c1-120
done <<LIST
1007 10.7 10.7 xnu-1699.32.7 Command-Line-Tools-OS-X-Lion-for-Xcode-November-2012/DevSDK/Payload/$K
1011 10.11 10.11 xnu-3248.60.10 Command-Line-Tools-OS-X-10-11-for-Xcode-7-3-1/DevSDK_OSX1011/$K
LIST
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
