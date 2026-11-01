#!/bin/bash
# Build mickys/nvenc's VideoToolbox plug-in from the vendored sources and pinned headers.
# Usage: build/nvenc_build.sh <output-directory>
# This is a local compile/link only. Packaging/signing/install/runtime validation happen elsewhere.
set -euo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
SRC="$HERE/vendor/nvenc"
OUT=${1:?usage: build/nvenc_build.sh <output-directory>}
UPSTREAM_TAG=v0.9.5
UPSTREAM_COMMIT=1e91845d6f37d8bce671d62c14f4390324da3659
MIN_OS=15.0

OGKM="$SRC/vendor/open-gpu-kernel-modules"
OGD="$SRC/vendor/open-gpu-doc"
WEBKIT="$SRC/vendor/webkit-spi"
for path in \
    "$SRC/LICENSE" "$SRC/src/enc.c" "$SRC/src/dec.c" "$SRC/vt/nvencvt.c" "$SRC/vt/nvencvt_dec.c" \
    "$OGKM/COPYING" "$OGKM/src/common/sdk/nvidia/inc/nvtypes.h" \
    "$OGKM/src/nvidia/arch/nvalloc/unix/include/nv-unix-nvos-params-wrappers.h" \
    "$OGD/LICENSE.md" "$OGD/classes/video/nvenc_drv.h" "$OGD/classes/video/nvdec_drv.h" \
    "$WEBKIT/CMBaseObjectSPI.h" "$WEBKIT/VTVideoDecoderSPI.h"; do
    [ -f "$path" ] || { echo "missing pinned NVENC build input: $path" >&2; exit 1; }
done
grep -q "$UPSTREAM_COMMIT" "$SRC/SOURCE.json" || { echo "upstream source pin does not match $UPSTREAM_COMMIT" >&2; exit 1; }

mkdir -p "$OUT"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/nvencvt-build.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
LIB="$TMP/libnvencvt.dylib"
BUILD_LOG="$TMP/build.log"

if ! xcrun clang -arch x86_64 -mmacosx-version-min="$MIN_OS" -O2 -Wall -dynamiclib \
    -DNVENC_VERSION='"v0.9.5"' -install_name @rpath/libnvencvt.dylib \
    -I"$SRC/src" -I"$WEBKIT" \
    -I"$OGKM/src/common/sdk/nvidia/inc" \
    -I"$OGKM/src/nvidia/arch/nvalloc/unix/include" \
    -I"$OGD/classes/video" \
    -o "$LIB" "$SRC/vt/nvencvt.c" "$SRC/vt/nvencvt_dec.c" "$SRC"/src/*.c \
    -framework IOKit -framework CoreFoundation -framework CoreMedia -framework CoreVideo \
    -framework VideoToolbox -framework Accelerate -framework IOSurface >"$BUILD_LOG" 2>&1; then
    cat "$BUILD_LOG" >&2
    exit 1
fi

xcrun lipo "$LIB" -verify_arch x86_64
MINOS_FOUND=$(xcrun vtool -show-build "$LIB" | awk '/minos/{print $2; exit}')
[ "$MINOS_FOUND" = "$MIN_OS" ] || { echo "built minos $MINOS_FOUND, expected $MIN_OS" >&2; exit 1; }
xcrun nm -gU "$LIB" | grep -q ' _NVEncVT_Register$' || { echo "NVEncVT_Register is not exported" >&2; exit 1; }

SHA256=$(shasum -a 256 "$LIB" | awk '{print $1}')
SDK=$(xcrun --show-sdk-version)
cp "$LIB" "$OUT/libnvencvt.dylib"
cp "$BUILD_LOG" "$OUT/nvencvt-build.log"
python3 - "$OUT/nvencvt-build-info.json" "$SHA256" "$SDK" <<'PY'
import json
import sys

path, sha256, sdk = sys.argv[1:]
info = {
    "schema_version": 1,
    "artifact": "libnvencvt.dylib",
    "sha256": sha256,
    "arch": "x86_64",
    "min_os": "15.0",
    "sdk": sdk,
    "upstream_repo": "mickys/nvenc",
    "upstream_tag": "v0.9.5",
    "upstream_commit": "1e91845d6f37d8bce671d62c14f4390324da3659",
}
with open(path, "w", encoding="utf-8") as f:
    json.dump(info, f, indent=2, sort_keys=True)
    f.write("\n")
PY

echo "built $OUT/libnvencvt.dylib (v0.9.5, x86_64, macOS $MINOS_FOUND, sha256 $SHA256)"
