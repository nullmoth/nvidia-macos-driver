#!/bin/bash
# check_package.sh <pkgroot>: the last step of a package build. Fails (exit 1) if the tree lacks the video codec library, if
# it is not signed, if SHA256SUMS does not carry a line that matches it, if the Ryzen power parts (stage_nmsmn.sh) are missing or
# unlisted, or if any regular file in the tree is not listed in SHA256SUMS.
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
# The plugin's AIR decoder must be the package's own (build/build_air_dis.sh).
[ -x "$ROOT/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/nvmtl-air-dis" ] || { echo "STOP: the plugin bundle lacks nvmtl-air-dis"; fail=1; }
# Every executable file we ship is ours or built from licensed open source. A program signed by Apple (the Metal toolchain's
# air-opt shipped in 2.0 to 2.8.1 this way) is not ours to redistribute, and no library may load from outside the system
# and the folders this package installs (a build machine's path, Homebrew or MacPorts would load whatever is there).
# NVIDIAShared.bundle (in packages up to 2.8.1) held two of NVIDIA's closed compiler libraries, one of them altered. They are
# Linux ELF files, which the Mach-O checks below never looked at; nothing this package builds is ELF, so any ELF file is
# somebody else's binary carried in from an older package tree.
[ ! -e "$ROOT/Library/GPUBundles/NVIDIAShared.bundle" ] || { echo "STOP: the package carries NVIDIAShared.bundle (NVIDIA's closed compiler libraries); it cannot ship"; fail=1; }
# A file added to the plugin bundle after it was sealed breaks the seal; finish_package.sh seals it again.
PB="Library/GPUBundles/NVMTLDriver.bundle"
if [ -f "$ROOT/$PB/Contents/MacOS/NVMTLDriver" ]; then
  codesign --verify --deep --strict "$ROOT/$PB" >/dev/null 2>&1 || { echo "STOP: the plugin bundle's seal does not verify (a file changed after it was sealed)"; fail=1; }
fi
while IFS= read -r -d '' f; do
  magic=$(head -c 4 "$f" 2>/dev/null | od -An -tx1 | tr -d ' \n')
  [ "$magic" != 7f454c46 ] || { echo "STOP: ${f#"$ROOT/"} is a Linux ELF binary; nothing built here is, so it cannot ship"; fail=1; continue; }
  printf '%s' "$magic" | grep -qE '^(cffaedfe|cefaedfe|cafebabe|feedfacf)$' || continue
  sig=$(codesign -dv "$f" 2>&1)
  # every program and library outside the kexts is signed (ad hoc): four went out unsigned, which an Intel Mac runs, but
  # a signature is what lets the system notice a file changed after it was built
  case "$f" in *.kext/Contents/MacOS/*) ;; *) codesign -v "$f" >/dev/null 2>&1 || { echo "STOP: ${f#"$ROOT/"} is not signed"; fail=1; };; esac
  case "$sig" in *"Identifier=com.apple."*|*"TeamIdentifier=59GAB85EFG"*|*"Authority=Apple Code Signing"*|*"Authority=Software Signing"*)
    echo "STOP: ${f#"$ROOT/"} is signed by Apple; it cannot ship"; fail=1;; esac
  own=$(otool -D "$f" 2>/dev/null | tail -n +2)
  while IFS= read -r dep; do
    case "$dep" in ''|/usr/lib/*|/System/Library/*|@rpath/*|@loader_path/*|@executable_path/*|/Library/GPUBundles/*|/Library/NullMoth/*) ;;
      *) [ "$dep" = "$own" ] || { echo "STOP: ${f#"$ROOT/"} loads $dep, outside the system and this package"; fail=1; };; esac
  done < <(otool -L "$f" 2>/dev/null | awk 'NR>1 {print $1}')
done < <(find "$ROOT" -type f -print0)
HERE=$(cd "$(dirname "$0")" && pwd)
# Every kext names the IOKit families it imports from, or it never loads (build/check_kext_libraries.sh).
while IFS= read -r -d '' k; do
  n=$(basename "$k" .kext)
  head -c 4 "$k/Contents/MacOS/$n" 2>/dev/null | od -An -tx1 | tr -d ' \n' | grep -qE '^(cffaedfe|cafebabe)$' || continue   # a test tree's stand-in
  bash "$HERE/../build/check_kext_libraries.sh" "$k" >/dev/null || { bash "$HERE/../build/check_kext_libraries.sh" "$k" | grep STOP; fail=1; }
done < <(find "$ROOT/Library" -type d -name '*.kext' -print0 2>/dev/null)
# Every regular file must be listed: install.sh refuses a package whose SHA256SUMS leaves a file out (sums_complete.sh).
bash "$HERE/sums_complete.sh" "$ROOT" || fail=1
[ -f "$ROOT/sums_complete.sh" ] || { echo "STOP: the package lacks sums_complete.sh, which install.sh runs"; fail=1; }
[ "$fail" = 0 ] && echo "package carries libnvencvt.dylib ${want:0:8} and the Ryzen power parts"
exit $fail
