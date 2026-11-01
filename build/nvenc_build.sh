#!/bin/bash
# Build the local NVEncVT artifact from vendored sources, pinned headers, and GA10x patches.
# Usage: build/nvenc_build.sh <output-directory>
# This is a local compile/link only. Packaging/signing/install/runtime validation happen elsewhere.
set -euo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
SRC="$HERE/vendor/nvenc"
OUT=${1:?usage: build/nvenc_build.sh <output-directory>}
UPSTREAM_TAG=v0.9.5
UPSTREAM_COMMIT=1e91845d6f37d8bce671d62c14f4390324da3659
ARTIFACT_VERSION=v0.9.5-ga10x1
LOCAL_REVISION=ga10x-encoding-1
MIN_OS=15.0

OGKM="$SRC/vendor/open-gpu-kernel-modules"
OGD="$SRC/vendor/open-gpu-doc"
WEBKIT="$SRC/vendor/webkit-spi"
for path in \
    "$SRC/LICENSE" "$SRC/src/enc.c" "$SRC/src/enc_iface.h" "$SRC/src/ga10x_policy.h" \
    "$SRC/src/dec.c" "$SRC/vt/nvencvt.c" "$SRC/vt/nvencvt_dec.c" \
    "$OGKM/COPYING" "$OGKM/src/common/sdk/nvidia/inc/nvtypes.h" \
    "$OGKM/src/nvidia/arch/nvalloc/unix/include/nv-unix-nvos-params-wrappers.h" \
    "$OGD/LICENSE.md" "$OGD/classes/video/nvenc_drv.h" "$OGD/classes/video/nvdec_drv.h" \
    "$WEBKIT/CMBaseObjectSPI.h" "$WEBKIT/VTVideoDecoderSPI.h"; do
    [ -f "$path" ] || { echo "missing pinned NVENC build input: $path" >&2; exit 1; }
done
python3 - "$SRC/SOURCE.json" "$UPSTREAM_TAG" "$UPSTREAM_COMMIT" "$ARTIFACT_VERSION" "$LOCAL_REVISION" <<'PY'
import json
import sys

path, upstream_tag, upstream_commit, artifact_version, local_revision = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as source:
        manifest = json.load(source)
except (OSError, UnicodeError, json.JSONDecodeError) as exc:
    raise SystemExit(f"cannot read NVENC source manifest: {exc}")
expected = {
    "upstream_tag": upstream_tag,
    "upstream_commit": upstream_commit,
    "artifact_version": artifact_version,
    "local_revision": local_revision,
}
for key, value in expected.items():
    if manifest.get(key) != value:
        raise SystemExit(f"NVENC source manifest {key} mismatch: expected {value!r}, got {manifest.get(key)!r}")
PY

mkdir -p "$OUT"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/nvencvt-build.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
LIB="$TMP/libnvencvt.dylib"
BUILD_LOG="$TMP/build.log"

if ! xcrun clang -arch x86_64 -mmacosx-version-min="$MIN_OS" -O2 -Wall -dynamiclib \
    -DNVENC_VERSION="\"$ARTIFACT_VERSION\"" -install_name @rpath/libnvencvt.dylib \
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
python3 - "$OUT/nvencvt-build-info.json" "$SHA256" "$SDK" \
    "$ARTIFACT_VERSION" "$LOCAL_REVISION" "$UPSTREAM_TAG" "$UPSTREAM_COMMIT" <<'PY'
import json
import sys

path, sha256, sdk, artifact_version, local_revision, upstream_tag, upstream_commit = sys.argv[1:]
info = {
    "schema_version": 2,
    "artifact": "libnvencvt.dylib",
    "artifact_version": artifact_version,
    "local_revision": local_revision,
    "sha256": sha256,
    "arch": "x86_64",
    "min_os": "15.0",
    "sdk": sdk,
    "upstream_repo": "mickys/nvenc",
    "upstream_tag": upstream_tag,
    "upstream_commit": upstream_commit,
}
with open(path, "w", encoding="utf-8") as f:
    json.dump(info, f, indent=2, sort_keys=True)
    f.write("\n")
PY

echo "built $OUT/libnvencvt.dylib ($ARTIFACT_VERSION, local revision $LOCAL_REVISION, x86_64, macOS $MINOS_FOUND, sha256 $SHA256)"
