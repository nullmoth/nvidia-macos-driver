#!/bin/bash
# build_air_dis.sh <out dir>: builds nvmtl-air-dis (plugin/air_dis.c), the plugin's out-of-process AIR decoder, for x86_64
# macOS 15 and later, signed ad hoc. package/finish_package.sh puts it into NVMTLDriver.bundle/Contents/MacOS beside
# air-runtime, where it finds the LLVM library it uses. Run on a Mac with Xcode or the command line tools.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:?out dir}
mkdir -p "$OUT"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -Wextra -Werror -o "$OUT/nvmtl-air-dis" "$HERE/plugin/air_dis.c"
codesign -f -s - "$OUT/nvmtl-air-dis" >/dev/null
echo "built $OUT/nvmtl-air-dis $(shasum -a 256 "$OUT/nvmtl-air-dis" | cut -c1-8)"
