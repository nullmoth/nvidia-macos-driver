#!/bin/bash
# run_cpu_checks.sh <patched-tree> <builddir>: builds the two CPU programs against a Mesa tree that has ttu-nak.patch and
# ttu-rq.patch applied (and a build directory with its static libraries), then judges the machine code. No GPU is touched.
#   1. ttu_nir_cpu: one query written directly in NIR -> region must match the field table
#   2. ttu_rq_cpu: a ray-query shader through nvk_nir_lower_ray_queries, with and without NVK_HWRT_QUERY=1
set -eu
T=${1:?tree}; B=${2:?builddir}; H=$(cd "$(dirname "$0")" && pwd); O=$(mktemp -d /tmp/hwrt-cpu.XXXXXX)
python3 "$H/build_ttu_cpu.py" "$T" "$B" "$H/ttu_nir_cpu.c" "$O/ttu_nir_cpu"
"$O/ttu_nir_cpu" "$O/direct" && python3 "$H/check_ttu_region.py" --selftest | tail -1
python3 "$H/check_ttu_region.py" "$O/direct.sass.bin" | tail -1
python3 "$H/build_ttu_rq_cpu.py" "$T" "$B" "$H/ttu_rq_cpu.c" "$O/ttu_rq_cpu"
NVK_HWRT_QUERY=1 "$O/ttu_rq_cpu" "$O/on"
env -u NVK_HWRT_QUERY "$O/ttu_rq_cpu" "$O/off"
python3 "$H/check_ttu_rq.py" "$O/on.sass.bin" "$O/off.sass.bin"
