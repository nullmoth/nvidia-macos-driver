#!/bin/bash
# build_plugin_release.sh <git-rev> <out-dir>: the release NVMTLDriver from plugin/ at <git-rev>, built on the build Mac
# (ssh host $NULLMOTH_BUILD_HOST) and copied back to <out-dir>. The package installs one plugin on macOS 15 and 26, so it is built
# for macOS 15: a plugin built against the 26 SDK without a deployment target carries minos 26.0 and dyld refuses it on
# Sequoia, which stopped every Sequoia install of 2.3.0 and 2.3.1. The build stops if the result is not minos 15.0 or
# if the compiler reports an API newer than macOS 15 used without an availability check.
set -euo pipefail
REV=${1:?git revision}; OUT=${2:?output directory}
HERE=$(cd "$(dirname "$0")/.." && pwd)
R=$NULLMOTH_BUILD_ROOT/build/plugin-release
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
git -C "$HERE" archive "$REV" plugin | tar x -C "$TMP"
ssh "$H" "rm -rf $R/src $R/out && mkdir -p $R/src $R/out"
rsync -a "$TMP/plugin/" "$H:$R/src/"
ssh "$H" "cd $R/src && xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -bundle -fobjc-arc -O1 -DNVMTL_RELEASE=1 \
  -Wno-protocol -Wno-incomplete-implementation -Werror=unguarded-availability-new \
  -I$NULLMOTH_BUILD_ROOT/build/gpu-src/mesa/include -o ../out/NVMTLDriver NVMTLDevice.m \
  -framework Foundation -framework Metal -framework IOSurface -framework IOKit -framework CoreFoundation 2>../out/build.err" \
  || { ssh "$H" "grep ' error:' $R/out/build.err | head -20"; exit 1; }
mkdir -p "$OUT"
scp -q "$H:$R/out/NVMTLDriver" "$H:$R/out/build.err" "$OUT/"
minos=$(vtool -show-build "$OUT/NVMTLDriver" | awk '/minos/{print $2; exit}')
[ "$minos" = 15.0 ] || { echo "STOP: minos $minos, not 15.0"; exit 1; }
echo "NVMTLDriver $(git -C "$HERE" rev-parse --short "$REV") sha $(shasum -a 256 "$OUT/NVMTLDriver" | cut -c1-8) minos $minos warnings $(grep -c ': warning:' "$OUT/build.err" || true)"
