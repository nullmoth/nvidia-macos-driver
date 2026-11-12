#!/bin/bash
# stage_legacy.sh [release...]: assemble the userland set of each release into AUDIT/stage/<release>/. It only builds and
# copies into that folder: nothing is installed, loaded or run.
# Needs, under AUDIT: tree-fix/ (the driver tree), nvkfix2-<floor>/, userland/loader-<floor>/, userland/xlate-10.12/,
# userland/xlate-10.11s/, codec-fix/, userland/small-<floor>/, nvk-src-fix/ (patched Mesa, for the boost daemon),
# sys/<release>/ (the release's real libraries). MESA_INC = Mesa include folder, REF = a release package root to take
# the small data files from (bundle Info.plist, ICD manifest, settings file, allow list).
set -u
A=${AUDIT:?}; M=${MESA_INC:?}; REF=${REF:?}; T=$A/tree-fix
floor() { case $1 in 14|13|12|11) echo $1.0;; *) echo $1;; esac; }
sha() { shasum -a 256 "$1" | cut -c1-16; }
minos() { xcrun vtool -show-build "$1" 2>/dev/null | awk '/minos|version /{print $2; exit}'; }
for R in ${@:-14 13 12 11 10.15 10.14 10.13 10.12 10.11}; do
  F=$(floor $R); S=$A/stage/$R; rm -rf "$S"; P=$S/payload; L=$S/build-logs
  B=$P/Library/GPUBundles/NVMTLDriver.bundle/Contents; N=$P/Library/GPUBundles/nvmtl; V=$P/Library/NullMoth
  mkdir -p "$B/MacOS" "$N" "$V/vulkan" "$V/boost" "$L"; st=ok
  # 1. the plugin, release flags at this floor, MetalFX weak (it does not exist before macOS 13)
  ( cd "$T/plugin" && xcrun clang -arch x86_64 -mmacosx-version-min=$F -bundle -fobjc-arc -O1 -DNVMTL_RELEASE=1 -Wno-protocol \
      -Wno-incomplete-implementation -Wunguarded-availability -ferror-limit=0 -I$M -o "$B/MacOS/NVMTLDriver" NVMTLDevice.m nvmtl_dlss.c dlss/elfhost.c dlss/lnxlibc.c \
      -framework Foundation -framework Metal -framework IOSurface -framework IOKit -framework CoreFoundation -weak_framework MetalFX -liconv ) > "$L/plugin.err" 2>&1 || st="plugin build failed"
  # 2..6 the pieces built per floor earlier
  cp "$A/nvkfix2-$F/libvulkan_nouveau.dylib" "$N/" 2>/dev/null && strip -S "$N/libvulkan_nouveau.dylib" 2>/dev/null || st="no NVK for $F"
  cp "$A/userland/loader-$F/libvulkan.1.dylib" "$V/vulkan/" 2>/dev/null || st="no loader for $F"
  if [ "$R" = 10.11 ]; then cp "$A/userland/xlate-10.11s/libnvmtl_translate.dylib" "$B/MacOS/" || st="no translator"
  else cp "$A/userland/xlate-10.12/libnvmtl_translate.dylib" "$B/MacOS/" && strip -S "$B/MacOS/libnvmtl_translate.dylib" && install_name_tool -id @rpath/libnvmtl_translate.dylib "$B/MacOS/libnvmtl_translate.dylib" 2>/dev/null || st="no translator"; fi
  cp "$A/codec-fix/libnvencvt-$F.dylib" "$B/MacOS/libnvencvt.dylib" 2>/dev/null || st="no codec library for $F"
  cp "$A/userland/small-$F/nvmtl-air-dis" "$B/MacOS/" 2>/dev/null || st="no air-dis for $F"
  cp "$A/userland/small-$F/libnvmtl_osver.dylib" "$N/" 2>/dev/null || st="no version library for $F"
  # 7. the boost daemon, from the patched NVK tree at this floor
  NV=$A/nvk-src-fix/mesa/src/nouveau/vulkan/nvkmd/nvrm; O=$NV/open-gpu-kernel-modules; H=$T/tools/nvmtl-boost
  xcrun clang -arch x86_64 -mmacosx-version-min=$F -O1 -Wunguarded-availability -o "$V/boost/nvmtl-boost" "$H/nvmtl_boost.c" "$NV/nvRmApi.c" "$NV/nvrm_xnu.c" "$NV/nvrm_floor_shim.c" \
    -I"$H" -I"$NV" -I"$O/src/nvidia/arch/nvalloc/common/inc" -I"$O/src/nvidia/arch/nvalloc/unix/include" -I"$O/src/common/sdk/nvidia/inc" \
    -I"$O/src/nvidia/inc/kernel" -framework IOKit -framework CoreFoundation > "$L/boost.err" 2>&1 || st="boost daemon build failed"
  # 8. data files
  cp "$REF/Library/GPUBundles/NVMTLDriver.bundle/Contents/Info.plist" "$B/Info.plist"
  cp "$REF/Library/GPUBundles/nvmtl/nvk_icd.json" "$REF/Library/GPUBundles/nvmtl/nvrm610.conf" "$N/"
  cp "$REF/Library/GPUBundles/nvmtl-allow.txt" "$P/Library/GPUBundles/"
  cp "$T/package/vulkan-for-apps.sh" "$V/vulkan/"; cp "$T/licenses/Vulkan-Loader-LICENSE.txt" "$V/vulkan/LICENSE.txt" 2>/dev/null
  cp "$H/com.nullmoth.nvmtl-boost.plist" "$V/boost/" 2>/dev/null
  for x in "$B/MacOS/NVMTLDriver" "$B/MacOS/libnvmtl_translate.dylib" "$B/MacOS/libnvencvt.dylib" "$B/MacOS/nvmtl-air-dis" "$N/libvulkan_nouveau.dylib" "$N/libnvmtl_osver.dylib" "$V/vulkan/libvulkan.1.dylib" "$V/boost/nvmtl-boost"; do
    [ -f "$x" ] && codesign -f -s - "$x" > /dev/null 2>&1; done
  # 9. imports against this release's real libraries, with a control in each direction
  MO=$(find "$P" -type f \( -name '*.dylib' -o -name NVMTLDriver -o -name nvmtl-air-dis -o -name nvmtl-boost \))
  { echo "positive control (a macOS 15 build on OS X 10.11 must report missing):"; python3 "$A/tools/importcheck.py" "$A/sys/10.11" "$A/floors-fix/NVMTLDriver-15.0" | head -1
    echo "negative control (this release's own C++ library on this release must report 0):"; python3 "$A/tools/importcheck.py" "$A/sys/$R" "$A/sys/$R/usr/lib/libc++.1.dylib" | head -1
    echo "staged binaries on $R:"; python3 "$A/tools/importcheck.py" "$A/sys/$R" $MO; } > "$S/IMPORTS.txt" 2>&1
  miss=$(sed -n '/^staged binaries/,$p' "$S/IMPORTS.txt" | grep -E 'missing [1-9]|unread [1-9]' | wc -l | tr -d ' ')
  pos=$(sed -n 2p "$S/IMPORTS.txt" | grep -cE 'missing [1-9]'); neg=$(sed -n 4p "$S/IMPORTS.txt" | grep -c 'missing 0, unread 0')
  [ "$pos" = 1 ] && [ "$neg" = 1 ] || st="import controls did not behave (positive $pos, negative $neg)"
  [ "$miss" = 0 ] || st="imports missing or unread in $miss binaries"
  # 10. the manifest
  { echo "Userland set for macOS $R (binaries built for floor $F). Assembled only; nothing here has been installed or run."
    echo "status: $st"
    echo
    echo "file | goes to on macOS $R | built for | sha256 (first 16) | size"
    ( cd "$P" && find . -type f | LC_ALL=C sort | while read -r f; do f=${f#./}
        case "$f" in *.dylib|*/NVMTLDriver|*/nvmtl-air-dis|*/nvmtl-boost) mv=$(minos "$f");; *) mv=-;; esac
        echo "$f | /$f | ${mv:-?} | $(sha "$f") | $(stat -f %z "$f")"; done )
    echo
    case $R in
      10.11|10.12) echo "Plugin lookup on $R: Metal builds the path /System/Library/Extensions/<MetalPluginName>.bundle and nothing else."
                   echo "The accelerator's MetalPluginName is the relative name ../../../Library/GPUBundles/NVMTLDriver, which that path arithmetic"
                   echo "turns into /Library/GPUBundles/NVMTLDriver.bundle (INFERRED, not run). If it is not honoured on $R, place the bundle at"
                   echo "/System/Library/Extensions/NVMTLDriver.bundle and publish the plain name NVMTLDriver (options/plugin-in-system-extensions).";;
      *) echo "Plugin lookup on $R: the trusted search covers /Library/GPUBundles and /System/Library/Extensions; the bundle goes to /Library/GPUBundles as on 15.";;
    esac
    echo "Not built here, carried unchanged from the release package: Library/NullMoth/nvfw (GPU firmware), the DLSS cache package."
    echo "Kexts for $R: the kernel lane's output folder (out/$F or out/$R under the audit folder); not part of this set."
    echo "Data files (bundle Info.plist, nvk_icd.json, nvrm610.conf, nvmtl-allow.txt) are reference copies from an earlier package; the release step supplies its own."
    echo "Imports: IMPORTS.txt. Options the owner chooses at install time: options/."
  } > "$S/MANIFEST.txt"
  pw=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: error:' "$L/plugin.err"); bw=$(grep -c 'only available on macOS' "$L/boost.err")
  echo "$R floor $F: $st | plugin errors $pw | boost newer-API sites $bw | binaries $(echo "$MO" | wc -l | tr -d ' ') | $(du -sh "$S" | cut -f1)"
done
