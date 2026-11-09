#!/bin/bash
# stage_nvencvt.sh <pkgroot> <libnvencvt.dylib>: puts the video codec library beside the plugin in a package tree,
# signs it ad hoc, and replaces its line in SHA256SUMS. build/build_nvencvt.sh makes the library.
# Fails if the library is not an x86_64 dylib exporting the entry points the plugin looks up, or if the bundle is missing.
set -euo pipefail
ROOT=${1:?pkgroot}; LIB=${2:?libnvencvt.dylib}
M="$ROOT/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS"
[ -d "$M" ] || { echo "STOP: $M does not exist (not a package tree)"; exit 1; }
[ -f "$LIB" ] || { echo "STOP: $LIB does not exist"; exit 1; }
for s in _NVEncVT_Register _NVEncVT_RegisterDecoders _NVEncVT_CreateInstance _NVDecVT_CreateInstance; do
  nm -gU "$LIB" | awk '{print $3}' | grep -qx "$s" || { echo "STOP: $LIB does not export $s"; exit 1; }
done
lipo -archs "$LIB" | grep -qx x86_64 || { echo "STOP: $LIB is not x86_64"; exit 1; }
install -m 755 "$LIB" "$M/libnvencvt.dylib"
codesign -f -s - "$M/libnvencvt.dylib" >/dev/null 2>&1 || { echo "STOP: could not sign libnvencvt.dylib"; exit 1; }
if [ -f "$ROOT/SHA256SUMS" ]; then
  REL="Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
  grep -vF "  ./$REL" "$ROOT/SHA256SUMS" | grep -vF "  $REL" > "$ROOT/SHA256SUMS.new" || true
  (cd "$ROOT" && printf '%s  ./%s\n' "$(shasum -a 256 "$REL" | cut -d' ' -f1)" "$REL") >> "$ROOT/SHA256SUMS.new"
  mv -f "$ROOT/SHA256SUMS.new" "$ROOT/SHA256SUMS"
fi
echo "staged $(shasum -a 256 "$M/libnvencvt.dylib" | cut -c1-8) into $M"
