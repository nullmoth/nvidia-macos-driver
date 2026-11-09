#!/bin/bash
# accel_build_all.sh <src> <out>: build NVAccel for every macOS the package carries, from one tree, into
# <out>/<major>/NVAccel.kext -- the Library/NullMoth/kexts/<major> layout install.sh reads. Building them
# together keeps the macOS 26 accelerator on the same VRAM/flip ABI as NVRMFB; a stale kexts/26 build
# leaves the desktop black ("nvAllocVram: ABI mismatch", issue #51).
set -uo pipefail
SRC=$1; OUT=$2; HERE=$(cd "$(dirname "$0")" && pwd)
ABI=$(sed -n 's/^#define NVRM_VRAM_ABI_VERSION \([0-9]*\)u.*/\1/p' "$SRC/nvrm_vram_abi.h")
[ -n "$ABI" ] || { echo "cannot read NVRM_VRAM_ABI_VERSION from $SRC/nvrm_vram_abi.h"; exit 1; }
for major in 15 26; do
    rm -rf "$OUT/$major"
    if [ "$major" = 26 ]; then NM_TAHOE=1 bash "$HERE/accel_build.sh" "$SRC" "$OUT/$major" || exit 1
    # /usr/bin/env by path: a PATH that puts ~/.local/bin first finds uv's "env" helper there, which ignores its
    # arguments and exits 0, so the macOS 15 build silently did nothing and the script still reported success
    else /usr/bin/env -u NM_TAHOE bash "$HERE/accel_build.sh" "$SRC" "$OUT/$major" || exit 1; fi
    [ -f "$OUT/$major/NVAccel.kext/Contents/MacOS/NVAccel" ] || { echo "STOP: the macOS $major NVAccel build produced no binary" >&2; exit 1; }
done
echo "NVAccel for macOS 15 and 26 built from $(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo "$SRC") (VRAM ABI $ABI)"
