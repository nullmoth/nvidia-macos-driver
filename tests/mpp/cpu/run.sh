#!/bin/bash
# run.sh <translator dir> <work dir>   (AIR_TARGET='-target air64-apple-macos26.0' for the AIR 2.8 form; default is 2.9): lowers the four kernels in ../mpp_matmul.metal with the translator's tensor pass and
# runs the plain IR on the host against a reference. Needs the Metal toolchain (to make AIR), llvm-dis and clang from LLVM 22,
# and cargo. The IR is compiled for the host CPU, so this runs anywhere, with no GPU.
set -euo pipefail
T=${1:?translator crate dir}; W=${2:?work dir}; HERE=$(cd "$(dirname "$0")" && pwd)
LLVM=${LLVM:-/opt/homebrew/Cellar/llvm/22.1.8/bin}
mkdir -p "$W"; cd "$W"
[ -f k_t_access.air ] || python3 - "$HERE/../mpp_matmul.metal" <<'PY'
import os, re, subprocess, sys
s = open(sys.argv[1]).read()
head = s[:s.index('// one tile')]
for p in re.split(r'(?=^kernel void )', s[s.index('// one tile'):], flags=re.M):
    m = re.search(r'kernel void (\w+)', p)
    if m:
        open(f'k_{m.group(1)}.metal', 'w').write(head + p)
        subprocess.check_call(['xcrun', 'metal', '-std=metal4.0'] + os.environ.get('AIR_TARGET', '').split() + ['-c', f'k_{m.group(1)}.metal', '-o', f'k_{m.group(1)}.air'])
PY
for k in mm_tile mm_sliced mm_acc_tr t_access; do
  "$LLVM/llvm-dis" k_$k.air -o k_$k.san.ll
  (cd "$T" && NV_TENSOR_IN="$W/k_$k.san.ll" NV_TENSOR_OUT="$W/k_$k.low.ll" cargo test --lib dump_lowered_text -- --ignored >/dev/null)
  grep -v '^target \|^source_filename\|^; ModuleID' k_$k.low.ll > k_$k.cpu.ll
  "$LLVM/clang" -O1 -c -x ir k_$k.cpu.ll -o k_$k.o
done
"$LLVM/clang" -O1 "$HERE/harness.c" k_*.o -lm -o harness
./harness
