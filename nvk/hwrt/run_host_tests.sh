#!/bin/bash
# run_host_tests.sh: every hardware ray-traversal check that needs no GPU and no NVK build (the machine-code checks of the NAK
# and NIR patches are run_cpu_checks.sh, which needs a built NVK tree).
set -eu
H=$(cd "$(dirname "$0")" && pwd); O=$(mktemp -d /tmp/hwrt-host.XXXXXX); rc=0
step() { echo "== $1"; shift; "$@" || { echo "STEP FAILED: $*"; rc=1; }; }
cc -O2 -Wall -Wextra -I"$H" -o "$O/model" "$H/hwrt_model.c" "$H/hwrt_model_test.c" -lm
cc -O2 -Wall -Wextra -I"$H" -o "$O/ref" "$H/hwrt_model.c" "$H/hwrt_ref.c" -lm
step "ray-query model against the brute-force reference" "$O/model"
step "encode mapping and walk against the model" "$O/ref"
step "query request and result words against recorded device output" python3 "$H/check_ttu_abi.py"
step "query region judge and its controls" python3 "$H/check_ttu_region.py" --selftest
step "layout rows judge and its controls" python3 "$H/hwrt_layout_check.py" --selftest
rm -rf "$O"
exit $rc
