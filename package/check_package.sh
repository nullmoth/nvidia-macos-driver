#!/bin/bash
# check_package.sh <pkgroot>: the last step of a package build. Fails (exit 1) if the tree lacks the video codec library, if
# it is not signed, if SHA256SUMS does not carry a line that matches it, or if the Ryzen power parts (stage_nmsmn.sh) are missing or unlisted.
set -uo pipefail
ROOT=${1:?pkgroot}
REL="Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
fail=0
if [ ! -f "$ROOT/$REL" ]; then echo "STOP: $REL is missing from the package"; exit 1; fi
codesign -v "$ROOT/$REL" >/dev/null 2>&1 || { echo "STOP: libnvencvt.dylib is not signed"; fail=1; }
want=$(shasum -a 256 "$ROOT/$REL" | cut -d' ' -f1)
grep -qE "^$want  (\./)?$REL\$" "$ROOT/SHA256SUMS" 2>/dev/null || { echo "STOP: SHA256SUMS has no matching line for libnvencvt.dylib"; fail=1; }
# The Vulkan loader is optional, but if the package carries one it must be x86_64, ship with its licence and be listed
V="Library/NullMoth/vulkan"
if [ -d "$ROOT/$V" ]; then
  lipo -archs "$ROOT/$V/libvulkan.1.dylib" 2>/dev/null | grep -qx x86_64 || { echo "STOP: $V/libvulkan.1.dylib is missing or not x86_64"; fail=1; }
  [ -s "$ROOT/$V/LICENSE.txt" ] || { echo "STOP: $V has no LICENSE.txt (Apache-2.0 requires it to travel with the loader)"; fail=1; }
  vw=$(shasum -a 256 "$ROOT/$V/libvulkan.1.dylib" 2>/dev/null | cut -d' ' -f1)
  grep -qE "^$vw  (\./)?$V/libvulkan.1.dylib\$" "$ROOT/SHA256SUMS" 2>/dev/null || { echo "STOP: SHA256SUMS has no matching line for the Vulkan loader"; fail=1; }
fi
# The Ryzen power parts (package/stage_nmsmn.sh): required, so a release cannot go out without them. The kext is carried only
# (install.sh never loads it; the Mac app installs it on request), the RyzenAdj build must be the one the app pins.
N="Library/NullMoth/kexts/NMSMN.kext"; R="Library/NullMoth/ryzenadj/ryzenadj"
if [ ! -f "$ROOT/$N/Contents/MacOS/NMSMN" ] || [ ! -f "$ROOT/$R" ]; then
  echo "STOP: the Ryzen power parts are missing (run package/stage_nmsmn.sh)"; fail=1
else
  [ "$(plutil -extract CFBundleIdentifier raw -o - "$ROOT/$N/Contents/Info.plist" 2>/dev/null)" = com.nullmoth.NMSMN ] || { echo "STOP: $N is not the NullMoth NMSMN kext"; fail=1; }
  [ -z "$(find "$ROOT/$N" "$ROOT/Library/NullMoth/ryzenadj" -type l 2>/dev/null)" ] || { echo "STOP: the Ryzen power parts hold a link"; fail=1; }
  while IFS= read -r f; do
    w=$(shasum -a 256 "$ROOT/$f" | cut -d' ' -f1)
    grep -qE "^$w  (\./)?$f\$" "$ROOT/SHA256SUMS" 2>/dev/null || { echo "STOP: SHA256SUMS has no matching line for $f"; fail=1; }
  done < <(cd "$ROOT" && find "$N" "Library/NullMoth/ryzenadj" -type f | LC_ALL=C sort)
fi
[ "$fail" = 0 ] && echo "package carries libnvencvt.dylib ${want:0:8} and the Ryzen power parts"
exit $fail
