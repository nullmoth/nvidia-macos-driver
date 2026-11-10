#!/bin/bash
# stage_boost.sh <pkgroot> <nvmtl-boost binary built by tools/nvmtl-boost/build.sh>: put the opt-in GPU boost daemon into a
# package tree (boost_daemon.sh writes the LaunchDaemon plist itself). Nothing installs it by default; the user runs
#   sudo env NULLMOTH_GPU_BOOST=1 ./install.sh
set -euo pipefail
ROOT=${1:?pkgroot}; BIN=${2:?nvmtl-boost binary}
HERE=$(cd "$(dirname "$0")" && pwd)
lipo -archs "$BIN" | grep -qx x86_64 || { echo "STOP: nvmtl-boost is not x86_64"; exit 1; }
mkdir -p "$ROOT/Library/NullMoth/boost"
cp "$BIN" "$ROOT/Library/NullMoth/boost/nvmtl-boost"
codesign -f -s - "$ROOT/Library/NullMoth/boost/nvmtl-boost" >/dev/null 2>&1 || { echo "STOP: could not sign nvmtl-boost"; exit 1; }
bash "$HERE/sums_refresh.sh" "$ROOT" Library/NullMoth/boost/nvmtl-boost
echo "staged the GPU boost daemon $(shasum -a 256 "$ROOT/Library/NullMoth/boost/nvmtl-boost" | cut -c1-8)"
