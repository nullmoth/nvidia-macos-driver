#!/bin/bash
# build_nvencvt.sh <git-rev> <out-dir>: libnvencvt.dylib from vendor/nvenc at <git-rev>, built on the build Mac
# (ssh host $NULLMOTH_BUILD_HOST) for x86_64 and macOS 15, and copied back to <out-dir>. The package puts it in
# NVMTLDriver.bundle/Contents/MacOS/, root-owned; the plugin refuses it otherwise.
# The headers it needs are fetched once into $NULLMOTH_BUILD_ROOT/build/nvencvt-headers at the pinned revisions
# listed in vendor/nvenc/SOURCE.
set -euo pipefail
REV=${1:?git revision}; OUT=${2:?output directory}
H=${NULLMOTH_BUILD_HOST:?set NULLMOTH_BUILD_HOST to the build Mac ssh host}
BASE=${NULLMOTH_BUILD_ROOT:?set NULLMOTH_BUILD_ROOT to the build folder on that Mac}
HERE=$(cd "$(dirname "$0")/.." && pwd)
R=$BASE/build/nvencvt
HDR=$BASE/build/nvencvt-headers
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
git -C "$HERE" archive "$REV" vendor/nvenc | tar x -C "$TMP"
VER=$(git -C "$HERE" describe --tags --always "$REV" 2>/dev/null || git -C "$HERE" rev-parse --short "$REV")
ssh "$H" "rm -rf $R/src $R/out && mkdir -p $R/src $R/out $HDR"
rsync -a "$TMP/vendor/nvenc/" "$H:$R/src/"
ssh "$H" "set -e; cd $HDR
  fetch() {   # fetch <dir> <url> <rev> <sparse paths...>
    d=\$1; u=\$2; r=\$3; shift 3
    [ -d \$d/.git ] || { git clone -q --filter=blob:none --no-checkout \$u \$d; [ \$# -gt 0 ] && git -C \$d sparse-checkout set \$@; }
    git -C \$d fetch -q --tags origin; git -C \$d checkout -q \$r
  }
  fetch ogkm https://github.com/NVIDIA/open-gpu-kernel-modules.git 610.57.04 src/common/sdk/nvidia/inc src/nvidia/arch/nvalloc/unix/include
  fetch ogd https://github.com/NVIDIA/open-gpu-doc.git 9fdf5c4062007929d9f4e6cbad9c9771fe61b880 classes/video
  mkdir -p webkit-spi
  for f in CMBaseObjectSPI.h VTVideoDecoderSPI.h; do
    [ -s webkit-spi/\$f ] || curl -fsSL -o webkit-spi/\$f https://raw.githubusercontent.com/WebKit/WebKit/3bfbc200370710495ffa440abdd31b943c83ad7a/Source/ThirdParty/libwebrtc/Source/webrtc/webkit_sdk/WebKit/\$f
  done"
ssh "$H" "cd $R/src && xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -fvisibility=hidden -dynamiclib -DNVENC_VERSION='\"$VER\"' \
  -o ../out/libnvencvt.dylib vt/nvencvt.c vt/nvencvt_dec.c src/*.c -I src -I $HDR/webkit-spi \
  -I $HDR/ogkm/src/common/sdk/nvidia/inc -I $HDR/ogkm/src/nvidia/arch/nvalloc/unix/include -I $HDR/ogd/classes/video \
  -Werror=unguarded-availability-new \
  -framework IOKit -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox \
  -framework Accelerate -framework IOSurface 2>../out/build.err" \
  || { ssh "$H" "grep ' error:' $R/out/build.err | head -20"; exit 1; }
mkdir -p "$OUT"
scp -q "$H:$R/out/libnvencvt.dylib" "$H:$R/out/build.err" "$OUT/"
bash "$HERE/build/warn_ceiling.sh" nvencvt "$(grep -c ': warning:' "$OUT/build.err" || true)" "$OUT/build.err" || exit 4
minos=$(vtool -show-build "$OUT/libnvencvt.dylib" | awk '/minos/{print $2; exit}')
[ "$minos" = 15.0 ] || { echo "STOP: minos $minos, not 15.0"; exit 1; }
# only the four entry points leave the library: the plugin looks up two, VideoToolbox is handed the other two
want="_NVDecVT_CreateInstance _NVEncVT_CreateInstance _NVEncVT_Register _NVEncVT_RegisterDecoders"
have=$(nm -gU "$OUT/libnvencvt.dylib" | awk '{print $3}' | sort | tr '\n' ' ' | sed 's/ $//')
[ "$have" = "$want" ] || { echo "STOP: exports are [$have], want [$want]"; exit 1; }
echo "libnvencvt $(git -C "$HERE" rev-parse --short "$REV") sha $(shasum -a 256 "$OUT/libnvencvt.dylib" | cut -c1-8) minos $minos warnings $(grep -c ': warning:' "$OUT/build.err" || true)"
