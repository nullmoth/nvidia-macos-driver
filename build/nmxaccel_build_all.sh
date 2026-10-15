#!/bin/bash
# nmxaccel_build_all.sh <src kexts/NVRM> <out>: build the AMD/Intel Metal accelerator NMXAccel for every macOS the
# package carries, into <out>/<major>/NMXAccel.kext -- the Library/NullMoth/kexts/<major> layout install.sh reads.
# NMXAccel is the one NMX kext that is built per-OS, for the same reason NVAccel is (accel_build_all.sh): Tahoe (26)
# made 27 IOAcceleratorFamily2 methods private, so the 26 build routes those inherited vtable slots through the base
# class's exported vtable (NM_TAHOE=1, kexts/NVRM/accel/tahoe_fwd.h). NMXGPU and NMXNPU are the same binaries on 15 and
# 26 (LinuxKPI over xnu KPIs that did not change); the per-OS link is proven by tools/prove_tahoe_kmutil.sh.
set -uo pipefail
SRC=$1; OUT=$2; HERE=$(cd "$(dirname "$0")" && pwd)
for major in 15 26; do
    rm -rf "$OUT/$major"; mkdir -p "$OUT/$major"
    if [ "$major" = 26 ]; then NM_TAHOE=1 bash "$HERE/nmxaccel_build.sh" "$SRC" "$OUT/$major" || exit 1
    else env -u NM_TAHOE bash "$HERE/nmxaccel_build.sh" "$SRC" "$OUT/$major" || exit 1; fi
done
echo "NMXAccel for macOS 15 and 26 built from $(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo "$SRC")"
