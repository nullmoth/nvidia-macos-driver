#!/bin/bash
# build_dlss_cache.sh <out-dir>: builds dlss_cache (x86_64) from dlss/ and puts it, with assertfail_trap.ptx, in
# <out-dir>/dlss/ (also nm_unzstd, the Zstandard decoder). The release package carries that folder beside install.sh; package/dlss_fetch.sh runs it on the
# user's Mac after downloading NVIDIA's DLSS files. Build on a Mac with the Xcode command line tools.
set -euo pipefail
OUT=${1:?output directory}
SRC=$(cd "$(dirname "$0")/../dlss" && pwd)
mkdir -p "$OUT/dlss"
clang -O2 -arch x86_64 -mmacosx-version-min=12.0 \
  -DBLAKE3_NO_SSE2 -DBLAKE3_NO_SSE41 -DBLAKE3_NO_AVX2 -DBLAKE3_NO_AVX512 -DBLAKE3_USE_NEON=0 \
  -o "$OUT/dlss/dlss_cache" "$SRC/dlss_cache.c" "$SRC/elfhost.c" "$SRC/lnxlibc.c" \
  "$SRC/blake3/blake3.c" "$SRC/blake3/blake3_dispatch.c" "$SRC/blake3/blake3_portable.c" -liconv -lcompression
strip -S "$OUT/dlss/dlss_cache"
# nm_unzstd: Zstandard reference decoder (stdin -> stdout) so dlss_fetch.sh can unpack NVIDIA's driver package
clang -O2 -arch x86_64 -mmacosx-version-min=12.0 -DZSTD_DISABLE_ASM -o "$OUT/dlss/nm_unzstd" "$SRC/nm_unzstd.c" \
  "$SRC"/zstd/common/*.c "$SRC"/zstd/decompress/*.c
strip -S "$OUT/dlss/nm_unzstd"
cp "$SRC/zstd/LICENSE" "$OUT/dlss/nm_unzstd.LICENSE"
cp "$SRC/assertfail_trap.ptx" "$OUT/dlss/assertfail_trap.ptx"
cp "$(dirname "$0")/../package/dlss_fetch.sh" "$OUT/dlss_fetch.sh"
codesign -f -s - "$OUT/dlss/dlss_cache" >/dev/null && codesign -f -s - "$OUT/dlss/nm_unzstd" >/dev/null   # every program the package carries is signed (ad hoc)
chmod 755 "$OUT/dlss/dlss_cache" "$OUT/dlss/nm_unzstd" "$OUT/dlss_fetch.sh"
echo "dlss_cache $(shasum -a 256 "$OUT/dlss/dlss_cache" | cut -c1-8) arch $(lipo -archs "$OUT/dlss/dlss_cache")"
