#!/bin/bash
# uninstall-legacy.sh: remove what install-legacy.sh placed on an older macOS release (10.7 to 14).
#     sudo sh uninstall-legacy.sh
# It also works from Recovery or single-user mode when the desktop does not come up:  sh <path>/uninstall-legacy.sh
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
[ "$(id -u)" = 0 ] || { echo "STOP run it with sudo"; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
EXT=$(cat "$HERE/kext-folder" 2>/dev/null); case "$EXT" in /Library/Extensions|/System/Library/Extensions) ;; *) EXT="";; esac
for e in ${EXT:-/Library/Extensions /System/Library/Extensions}; do
  for k in NVRM NVRMFB NVRMAGDC NVAccel; do
    [ -f "$e/$k.kext/Contents/Info.plist" ] && grep -q "com.nullmoth.$k" "$e/$k.kext/Contents/Info.plist" && rm -rf "$e/$k.kext" && echo "removed $e/$k.kext"
  done
  [ -d "$e" ] && touch "$e"
done
rm -rf /Library/GPUBundles/NVMTLDriver.bundle /Library/GPUBundles/nvmtl /Library/NullMoth/nvfw /Library/NullMoth/vulkan /Library/NullMoth/boost /Library/NullMoth/driver-version
v=$(sw_vers -productVersion)
case "$v" in
  10.*) kextcache -i / >/dev/null 2>&1 || kextcache -u / >/dev/null 2>&1 || echo "note: rebuild the kernel cache by hand (kextcache -i /)";;
  *) KC=/Library/KernelCollections/AuxiliaryKernelExtensions.kc; KARG=""; [ "${v%%.*}" -ge 13 ] && KARG="--allow-missing-kdk"
     # shellcheck disable=SC2086
     kmutil create -n aux --volume-root / $KARG -B /System/Library/KernelCollections/BootKernelExtensions.kc -S /System/Library/KernelCollections/SystemKernelExtensions.kc \
       --repository /Library/Extensions -A "$KC.new" -z >/dev/null 2>&1 && mv -f "$KC.new" "$KC" || echo "note: the kernel collection was not rebuilt; macOS rebuilds it at the next restart";;
esac
echo "RESULT removed; restart"
