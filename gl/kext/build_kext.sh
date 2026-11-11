#!/bin/bash
# build_kext.sh <audit base (holds sdk/ and kdk/)> <out dir> <repo root (for kexts/compat and build/legacy)>
# Compiles and links the accelerator against the kernel headers of every release from 10.6 to 10.13 (the era build pattern of
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
    -DNM_ERA=$era -include "$ROOT/kexts/compat/nm_era.h" -I"$H" -I"$ROOT/kexts/NVRM" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
    -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -Wall -Wextra -Wno-unused-parameter -Wno-everything -ferror-limit=0 -O2 \
    -c "$H/NMLegacyAccel.cpp" -o "$O/NMLegacyAccel.o" > "$O/compile.log" 2>&1; rc=$?
  e=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/compile.log")
  echo "release $mv: compile rc=$rc errors=$e"; [ $rc = 0 ] || { fail=1; grep -E 'error:' "$O/compile.log" | head -6 | cut -c1-220; continue; }
  # the module information (identifier, version, start and stop), without which the kernel's kext linker refuses the kext
  xcrun clang -arch x86_64 -mmacosx-version-min=$mv -mkernel -nostdinc -I"$B/sdk/$khdr" -idirafter "$RES" -DKERNEL -DKERNEL_PRIVATE -DAPPLE -DNeXT \
    -Wno-everything -O2 -c "$H/nm_kmod_info.c" -o "$O/nm_kmod_info.o" >> "$O/compile.log" 2>&1 || { echo "release $mv: module info compile FAILED"; tail -4 "$O/compile.log"; fail=1; continue; }
  xcrun clang++ -arch x86_64 -mmacosx-version-min=$mv -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$O/NMLegacyAccel.o" "$O/nm_kmod_info.o" \
    -o "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" > "$O/link.log" 2>&1; rc=$?
  [ $rc = 0 ] || { echo "release $mv: LINK FAILED"; grep -v 'built for newer' "$O/link.log" | head -6; fail=1; continue; }
  cp "$H/Info.plist" "$O/NMLegacyAccel.kext/Contents/"
  xcrun nm -arch x86_64 -u "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" > "$O/NMLegacyAccel.imports.txt"
  xcrun nm -arch x86_64 "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" | grep -q ' _kmod_info$' || { echo "release $mv: no module information in the binary"; fail=1; }
  # the personality must not be able to match anything
  grep -q 'NMLegacyAccelStubNeverMatches' "$O/NMLegacyAccel.kext/Contents/Info.plist" || { echo "release $mv: personality is NOT gated"; fail=1; }
  echo "release $mv: linked, $(wc -l < "$O/NMLegacyAccel.imports.txt" | xargs) imports, classes $(xcrun nm -arch x86_64 "$O/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" | grep -c 'gMetaClassE$')"
  python3 "$ROOT/build/legacy/era_symcheck.py" "$O" "$B/kdk/$kit" "$tag" > "$O/symcheck.log" 2>&1 || fail=1
  grep -E "^NMLegacyAccel:|^kit|^    " "$O/symcheck.log" | cut -c1-150
  # control for the import check: one invented import must be reported
  mkdir -p "$O/control"; (cat "$O/NMLegacyAccel.imports.txt"; echo __ZN20NotARealKernelSymbolEv) > "$O/control/Control.imports.txt"
  python3 "$ROOT/build/legacy/era_symcheck.py" "$O/control" "$B/kdk/$kit" "$tag" 2>&1 | grep -E "^Control|NotAReal" | cut -c1-120
done <<LIST
1006 10.6 10.6 xnu-1504.15.3 Xcode-3-2-6/MacOSX10.6.x/Payload/SDKs/MacOSX10.6.sdk/$K
1007 10.7 10.7 xnu-1699.32.7 Command-Line-Tools-OS-X-Lion-for-Xcode-November-2012/DevSDK/Payload/$K
1008 10.8 10.8 xnu-2050.48.11 Command-Line-Tools-OS-X-Mountain-Lion-for-Xcode-April-2014/DevSDK/Payload/$K
1009 10.9 10.9 xnu-2422.115.4 Command-Line-Tools-OS-X-Mavericks-for-Xcode-April-2014/MacOSX10_9_SDK/Payload/$K
1010 10.10 10.10 xnu-2782.40.9 Command-Line-Tools-OS-X-10-10-for-Xcode-6-4/DevSDK_OSX1010/$K
1011 10.11 10.11 xnu-3248.60.10 Command-Line-Tools-OS-X-10-11-for-Xcode-7-3-1/DevSDK_OSX1011/$K
1012 10.12 10.12 xnu-3789.70.16 Command-Line-Tools-macOS-10-12-for-Xcode-8-3-2/DevSDK_OSX1012.pkg/Payload/$K
1013 10.13 10.13 xnu-4570.71.2 Command-Line-Tools-macOS-10-13-for-Xcode-9-4-1/DevSDK_macOS1013_Public.pkg/Payload/$K
LIST
# host test of the ownership and grant rules, and its control: the same test built with the first version's logic
# (global ids, any caller) must fail
mkdir -p "$OUT/host"
if clang -std=c11 -Wall -Wextra -Werror -o "$OUT/host/table_test" "$H/test/table_test.c" > "$OUT/host/build.log" 2>&1 &&
   clang -std=c11 -Wall -Wextra -Werror -DNM_TABLE_OLD_LOGIC -o "$OUT/host/table_test_old" "$H/test/table_test.c" >> "$OUT/host/build.log" 2>&1; then
  "$OUT/host/table_test" || fail=1
  "$OUT/host/table_test_old" > "$OUT/host/old.out" 2>&1; orc=$?
  echo "control (first version's logic): exit $orc, $(grep -c '^FAIL' "$OUT/host/old.out") arms fail: $(grep '^FAIL' "$OUT/host/old.out" | cut -c7-60 | tr '\n' ';')"
  [ $orc != 0 ] && [ "$(grep -c '^FAIL' "$OUT/host/old.out")" -ge 3 ] || { echo "control did not fail on the old logic"; fail=1; }
else echo "host test build failed"; head -5 "$OUT/host/build.log"; fail=1; fi
# host test of the copy into a scanout, and its control: the same arms against a copy without clipping must fail
# (it writes outside the scanout, so it may also crash: any non-zero exit counts)
if clang -std=c11 -Wall -Wextra -Werror -o "$OUT/host/present_test" "$H/test/present_test.c" >> "$OUT/host/build.log" 2>&1 &&
   clang -std=c11 -Wall -Wextra -Werror -DNM_PRESENT_NO_CLIP -o "$OUT/host/present_test_noclip" "$H/test/present_test.c" >> "$OUT/host/build.log" 2>&1; then
  "$OUT/host/present_test" || fail=1
  "$OUT/host/present_test_noclip" > "$OUT/host/noclip.out" 2>&1; orc=$?
  echo "control (no clipping): exit $orc, $(grep -c '^FAIL' "$OUT/host/noclip.out") arms fail"
  [ $orc != 0 ] || { echo "control did not fail without clipping"; fail=1; }
else echo "present test build failed"; tail -5 "$OUT/host/build.log"; fail=1; fi
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
