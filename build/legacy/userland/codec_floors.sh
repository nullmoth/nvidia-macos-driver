#!/bin/bash
# codec_floors.sh <tree dir> <tag>: the video codec library at every floor, with every newer-API warning kept.
# Environment: AUDIT, NVENC_HDR. Also builds the release line (floor 15, -Werror=unguarded-availability-new).
A=${AUDIT:?}; H=${NVENC_HDR:?}; T=$1; O=$A/codec-$2; mkdir -p "$O"
INC="-I src -I $H/webkit-spi -I $H/ogkm/src/common/sdk/nvidia/inc -I $H/ogkm/src/nvidia/arch/nvalloc/unix/include -I $H/ogd/classes/video"
FW="-framework IOKit -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework Accelerate -framework IOSurface"
cd "$T/vendor/nvenc" || exit 2
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -fvisibility=hidden -dynamiclib -DNVENC_VERSION='"test"' -o "$O/release-15.dylib" \
  vt/nvencvt.c vt/nvencvt_dec.c src/*.c $INC -Werror=unguarded-availability-new $FW > "$O/release-15.err" 2>&1
rc=$?
w=$(grep -c ': warning:' "$O/release-15.err")
sha=$(shasum -a 256 "$O/release-15.dylib" | cut -c1-16)
echo "release line rc=$rc warnings=$w sha=$sha"
for F in ${FLOORS:-15.0 14.0 13.0 12.0 11.0 10.15 10.14 10.13 10.12 10.11}; do
  xcrun clang -arch x86_64 -mmacosx-version-min=$F -O2 -Wall -fvisibility=hidden -dynamiclib -DNVENC_VERSION='"test"' -o "$O/libnvencvt-$F.dylib" \
    vt/nvencvt.c vt/nvencvt_dec.c src/*.c $INC -Wunguarded-availability -ferror-limit=0 $FW > "$O/build-$F.err" 2>&1
  rc=$?
  e=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/build-$F.err")
  a=$(grep -c 'is only available on macOS' "$O/build-$F.err")
  o=$(grep -E '^[^ ]+:[0-9]+:[0-9]+: warning:' "$O/build-$F.err" | grep -vc 'only available')
  k=$(nm -m -arch x86_64 "$O/libnvencvt-$F.dylib" 2>/dev/null | grep -c 'weak external')
  echo "floor $F rc=$rc errors=$e avail=$a other=$o weak=$k"
done
