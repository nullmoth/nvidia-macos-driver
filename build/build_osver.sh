#!/bin/bash
# build_osver.sh <out-dir>: libnvmtl_osver.dylib from plugin/nvmtl_osver.m. It needs only the macOS SDK, so it builds on
# any Mac with the command line tools. x86_64, minos 15.0 like the plugin; the install name is the path the package
# installs it at, which is also the path a relinked game names in its weak load command.
set -euo pipefail
OUT=${1:?output directory}
HERE=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$OUT"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -dynamiclib -fobjc-arc -O2 -Wall -Werror \
  -install_name /Library/GPUBundles/nvmtl/libnvmtl_osver.dylib -I"$HERE/plugin" \
  -o "$OUT/libnvmtl_osver.dylib" "$HERE/plugin/nvmtl_osver.m" -framework Foundation -framework CoreFoundation
codesign -f -s - "$OUT/libnvmtl_osver.dylib"
minos=$(vtool -show-build "$OUT/libnvmtl_osver.dylib" | awk '/minos/{print $2; exit}')
[ "$minos" = 15.0 ] || { echo "STOP: minos $minos, not 15.0"; exit 1; }
echo "libnvmtl_osver.dylib $(git -C "$HERE" rev-parse --short HEAD) sha $(shasum -a 256 "$OUT/libnvmtl_osver.dylib" | cut -c1-8) minos $minos"
