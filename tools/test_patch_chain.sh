#!/bin/bash
# test_patch_chain.sh <pristine open-gpu-kernel-modules 620.70.06 tree>: every patch in kexts/NVRM/patches applies with --fuzz=0, in order.
# The tree is copied first; the original is not touched. Patches name their files from the tree root (a/src/...) or from src/nvidia-modeset.
set -u
SRC=${1:?pristine tree}; HERE=$(cd "$(dirname "$0")/.." && pwd); P="$HERE/kexts/NVRM/patches"
W=$(mktemp -d); cp -R "$SRC" "$W/t"; fail=0
for f in "$P"/*.patch; do
  ok=0
  for root in "" "src/nvidia-modeset"; do
    ( cd "$W/t/$root" 2>/dev/null && patch -p1 --fuzz=0 --dry-run < "$f" >/dev/null 2>&1 && patch -p1 --fuzz=0 < "$f" >/dev/null ) && { echo "OK   $(basename "$f")"; ok=1; break; }
  done
  [ $ok = 1 ] || { echo "FAIL $(basename "$f")"; fail=1; }
done
rm -rf "$W"; exit $fail
