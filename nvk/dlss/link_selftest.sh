#!/bin/bash
# usage: link_selftest.sh <patched mesa tree> <cubin>...      (cubins as extracted from the fatbins, with program headers)
set -eu
T=${1:?patched mesa tree}; shift
D=$(mktemp -d)
python3 -I - "$T/src/nouveau/vulkan/nvk_cubin.c" "$D/link_fn.inc" <<'PY'
import sys
s = open(sys.argv[1]).read()
a = s.index('/* Raw ELF64 reads')
b = s.index('static bool\nnvk_cubin_link_find')
open(sys.argv[2], 'w').write(s[a:b])
PY
cp "$(dirname "$0")/link_selftest.c" "$D/"
clang -O1 -g -fsanitize=address,undefined -Wall -Wno-unused -o "$D/selftest" "$D/link_selftest.c"
"$D/selftest" "$@"
rm -rf "$D"
