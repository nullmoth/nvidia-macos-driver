#!/bin/bash
# Applies the NVK series for macOS 27 to an unmodified Mesa tree at the commit in nvk/MESA_BASE.
# usage: nvk/apply-macos27.sh <mesa-tree>
#
# The macOS 27 kexts run RM 615, so nvk-620-kernel.patch and ogkm-nvos-620.patch are not part of this series;
# NVK compiles against the open-gpu-kernel-modules 610.57.04 headers as they are.
# Every patch must apply with no fuzz: a patch that needs fuzz was written against a different tree.
set -eu
T=${1:?usage: apply-macos27.sh <mesa-tree>}
N=$(cd "$(dirname "$0")" && pwd)
[ -f "$T/VERSION" ] || { echo "STOP: no Mesa tree at $T" >&2; exit 2; }
cd "$T"
p() { patch -p1 -F0 -s --no-backup-if-mismatch < "$N/$1" || { echo "STOP: $1 does not apply" >&2; exit 3; }; echo "applied $1"; }
p nvk-macos.patch
p nvk-fixes-after-def16.patch
python3 -I "$N/dlss/patch_dlss_cache.py" "$T" || { echo "STOP: dlss/patch_dlss_cache.py" >&2; exit 3; }
p nvk-recycle.patch            # command-memory recycler
p nvk-slab.patch               # command-memory slabs (replace the per-chunk recycler when on)
p nvk-slabdef.patch            # slabs on by default, idle slabs released
p nvk-macos27-sparse.patch     # 16 KB sparse pages and Apple tile shapes for Metal sparse resources
p nvk-readonly-host-import.patch       # host memory the caller may not write is imported and mapped read-only
p nvk-surface-mark-connections.patch   # completion marks and try-only surface calls never queue behind a blocking one
if [ -n "$(find . -name '*.rej' -o -name '*.orig' | head -1)" ]; then echo "STOP: rejects left in $T" >&2; exit 4; fi
echo "series complete"
