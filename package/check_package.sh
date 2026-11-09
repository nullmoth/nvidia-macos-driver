#!/bin/bash
# check_package.sh <pkgroot>: the last step of a package build. Fails (exit 1) if the tree lacks the video codec library, if
# it is not signed, or if SHA256SUMS does not carry a line that matches it.
set -uo pipefail
ROOT=${1:?pkgroot}
REL="Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
fail=0
if [ ! -f "$ROOT/$REL" ]; then echo "STOP: $REL is missing from the package"; exit 1; fi
codesign -v "$ROOT/$REL" >/dev/null 2>&1 || { echo "STOP: libnvencvt.dylib is not signed"; fail=1; }
want=$(shasum -a 256 "$ROOT/$REL" | cut -d' ' -f1)
grep -qE "^$want  (\./)?$REL\$" "$ROOT/SHA256SUMS" 2>/dev/null || { echo "STOP: SHA256SUMS has no matching line for libnvencvt.dylib"; fail=1; }
[ "$fail" = 0 ] && echo "package carries libnvencvt.dylib ${want:0:8}"
exit $fail
