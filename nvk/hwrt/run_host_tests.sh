#!/bin/bash
# run_host_tests.sh: every hardware ray-traversal check that needs no GPU and no NVK build (the machine-code checks of the NAK
# and NIR patches are run_cpu_checks.sh, which needs a built NVK tree).
set -eu
H=$(cd "$(dirname "$0")" && pwd); O=$(mktemp -d /tmp/hwrt-host.XXXXXX); rc=0
step() { echo "== $1"; shift; "$@" || { echo "STEP FAILED: $*"; rc=1; }; }
cc -O2 -Wall -Wextra -I"$H" -o "$O/model" "$H/hwrt_model.c" "$H/hwrt_model_test.c" -lm
cc -O2 -Wall -Wextra -I"$H" -o "$O/ref" "$H/hwrt_model.c" "$H/hwrt_ref.c" -lm
cc -O2 -Wno-misleading-indentation -Wno-unused-function -I"$H" -c -o "$O/wm.o" "$H/hwrt_model.c"
cc -O2 -Wno-misleading-indentation -Wno-unused-function -I"$H" -c -o "$O/ww.o" "$H/hwrt_wide.c"
c++ -O2 -std=c++17 -I"$H" -c -o "$O/ws.o" "$H/hwrt_wide_shader.cpp"
cc -o "$O/wide" "$O/wm.o" "$O/ww.o" "$O/ws.o" -lc++ -lm
step "ray-query model against the brute-force reference" "$O/model"
step "encode mapping and walk against the model" "$O/ref"
step "8-wide node option against the model" "$O/wide"
step "query request and result words against recorded device output" python3 "$H/check_ttu_abi.py"
step "query region judge and its controls" python3 "$H/check_ttu_region.py" --selftest
step "layout rows judge and its controls" python3 "$H/hwrt_layout_check.py" --selftest
rm -rf "$O"
exit $rc
