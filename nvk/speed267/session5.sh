#!/bin/bash
# session5.sh: correctness then speed for the two NAK changes.
#  probe: GB7-shaped particle n-body (tgmem), forces hashed - old NVK (pinwhole) == new NVK (predload) == new with
#  NVMTL_NAK_NO_PREDLOAD=1 is the correctness gate; the per-dispatch time is the particle speed.
#  GB7: pick (current dev NVK) vs pinwhole vs predload, one run each, VM-guarded by ab.sh.
P=~/Desktop/tmp-2026-09-23/pick; A=~/Desktop/tmp-2026-09-23/gb7ab/ab.sh; B=~/Desktop/tmp-2026-09-23/gb7ab/lf.bundle
SH=$HOME/nvmtl-tests/g3-20260918/cand_shim2.dylib
probe() { # <tag> <nvk> [ENV=...]
  local t=$1 n=$2 rc; shift 2; mkdir -p $P/t.$t
  env "$@" NVMTL_PLUGIN=$B/Contents/MacOS/NVMTLDriver NVMTL_ICD_LIB=$n NVMTL_NVK_DYLIB=$n MESA_SHADER_CACHE_DISABLE=true \
    DYLD_INSERT_LIBRARIES=$SH TMPDIR=$P/t.$t/ perl -e 'alarm 120; exec @ARGV' $P/tgmem.x86 $P/tgmem.metallib > $P/$t.probe 2>&1
  rc=$?
  echo "probe $t rc=$rc $(grep -m1 'tg   64' $P/$t.probe | grep -oE 'median +[0-9.]+ us') $(grep -oE 'fnv [0-9a-f]+' $P/$t.probe) $(tail -1 $P/$t.probe)"
}
for n in nvk-pinwhole nvk-predload; do codesign -f -s - $P/$n.dylib 2>/dev/null; done
probe old  $P/nvk-pinwhole.dylib
probe new  $P/nvk-predload.dylib
probe neg  $P/nvk-predload.dylib NVMTL_NAK_NO_PREDLOAD=1
for spec in "pick:nvk-pick" "pinw:nvk-pinwhole" "pred:nvk-predload"; do
  t=${spec%%:*}; n=$P/${spec#*:}.dylib
  $A s5$t lf.bundle NVMTL_ICD_LIB=$n NVMTL_NVK_DYLIB=$n MESA_SHADER_CACHE_DISABLE=true | grep -oE "^s5.*"
done
