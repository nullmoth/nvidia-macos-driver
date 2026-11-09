#!/bin/bash
# Curve geometry checks that need no GPU and no NVK build. The NIR form (curve_nir_cpu.c) needs a built NVK tree:
#   python3 nvk/curves/build_cpu.py <tree> <builddir> $PWD/nvk/curves/curve_nir_cpu.c /tmp/curve_nir_cpu && /tmp/curve_nir_cpu x
set -eu
H=$(cd "$(dirname "$0")" && pwd); O=$(mktemp -d /tmp/curve-host.XXXXXX)
cc -O2 -Wall -Wextra -o "$O/model" "$H/curve_model_test.c" -lm
"$O/model"; rc=$?
rm -rf "$O"; exit $rc
