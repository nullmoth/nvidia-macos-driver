#!/bin/bash
# check_kext_libraries.sh <X.kext>...: a kext that calls into one of Apple's IOKit families must name that family in its
# Info.plist (OSBundleLibraries). Without the entry the kernel linker leaves the family's symbols unresolved and the kext
# never loads, on every machine; the build and the link both succeed, so nothing else shows it. The keyboard backlight
# plugin subclasses IOHIDDevice and had no IOHIDFamily entry. Checked from the import table, not the source: including a
# family's header for a constant needs no entry. Exit 1 naming each missing library.
set -uo pipefail
# imported class prefix (regex) -> the bundle identifier(s), any one of which satisfies it
TABLE='IOHIDDevice|IOHIDInterface|IOHIDEvent:com.apple.iokit.IOHIDFamily
IOAudio:com.apple.iokit.IOAudioFamily
IONDRV:com.apple.iokit.IONDRVSupport
IOACPIPlatform:com.apple.iokit.IOACPIFamily
IOPCIDevice|IOPCIBridge:com.apple.iokit.IOPCIFamily
IOFramebuffer|IODisplay|IOBacklight|IOI2C:com.apple.iokit.IOGraphicsFamily
IOAccel|IOGraphicsAccelerator:com.apple.iokit.IOAcceleratorFamily2|com.apple.iokit.IOGraphicsFamily'
fail=0
for K in "$@"; do
  n=$(basename "$K" .kext); b="$K/Contents/MacOS/$n"; p="$K/Contents/Info.plist"
  [ -f "$b" ] && [ -f "$p" ] || { echo "STOP: $K is not a complete kext"; fail=1; continue; }
  libs=" $(plutil -extract OSBundleLibraries xml1 -o - "$p" 2>/dev/null | sed -n 's|.*<key>\(.*\)</key>.*|\1|p' | tr '\n' ' ')"
  und=$(nm -arch x86_64 -u "$b" 2>/dev/null) || { echo "STOP: nm could not read $b"; fail=1; continue; }
  [ -n "$und" ] || { echo "STOP: $b imports nothing (not a kext binary?)"; fail=1; continue; }
  while IFS=: read -r pat ids; do
    printf '%s\n' "$und" | grep -Eq "__Z(N|NK|TV)[0-9]+($pat)" || continue
    ok=0; for id in $(printf '%s' "$ids" | tr '|' ' '); do case "$libs" in *" $id "*) ok=1;; esac; done
    [ "$ok" = 1 ] || { echo "STOP: $n imports ${pat%%|*} symbols but its Info.plist does not list ${ids%%|*} in OSBundleLibraries"; fail=1; }
  done <<< "$TABLE"
done
[ "$fail" = 0 ] && echo "every imported IOKit family is declared ($# kext(s))"
exit $fail
