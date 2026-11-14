#!/bin/bash
# run_corpus.sh <corpus-dir> <libnvmtl_translate.dylib> [spirv-val]: translate every <case>.air in the corpus (made by
# compile_corpus.sh) with the translator through the plugin's entry points, validate each result, and write
# <corpus-dir>/result.tsv: case, verdict (TRANSLATED | REFUSED | INVALID-SPV | CRASH), detail.
#   - mesh stages go through nvmtl_lower_mesh and both halves are translated and validated;
#   - <name>_caller / <name>_callee pairs go through nvmtl_visible_refs + nvmtl_link_visible (the callee is not a case
#     of its own);
#   - a module that reads a visible or intersection function table goes through nvmtl_link_runtime with every
#     non-entry function of the module as a table candidate;
#   - a mesh with an <name>_object sibling goes through nvmtl_lower_mesh2 with the object stage linked;
#   - a shader with src/<name>.fc goes through nvmtl_translate_fc with the "<index>=<hex bytes>" values in that file;
#   - everything else goes through nvmtl_translate.
# The text comes from LLVM_DIS (default: the build Mac's LLVM 22 llvm-dis), which reads Apple's bitcode into the same
# opaque-pointer text the plugin gets from the LLVM it bundles. Apple's own air-opt prints typed pointers, which the
# translator never sees from the plugin.
# Exit status is non-zero when any case is INVALID-SPV or CRASH, and when a case is REFUSED that is not listed in
# expected_refusals.txt next to this script (one shader name per line).
set -u
# LLVM_DIS: "llvm-dis in -o out" compatible reader. To use the exact libLLVM the plugin ships, build bc2ll.c with
# -arch x86_64, point NVMTL_LIBLLVM at air-runtime/libLLVM.dylib, set LLVM_DIS to that tool and RUN_PREFIX="arch -x86_64"
# (the tool, the harness and the translator dylib are then all the x86_64 builds the driver ships).
LLVM_DIS=${LLVM_DIS:-/opt/homebrew/Cellar/llvm/22.1.8/bin/llvm-dis}
RUN_PREFIX=${RUN_PREFIX:-}
HERE=$(cd "$(dirname "$0")" && pwd); C=${1:?corpus dir}; LIB=${2:?dylib}; VAL=${3:-spirv-val}
[ -x "$C/harness" ] || cc -O1 ${HARNESS_CFLAGS:-} -o "$C/harness" "$HERE/xlate_harness.c" || exit 9
: > "$C/result.tsv"; bad=0
for air in "$C"/*.air; do
  case=$(basename "$air" .air); shader=${case%%.*}; suffix=${case#*.}
  ll="$C/$case.ll"; $RUN_PREFIX "$LLVM_DIS" "$air" -o "$ll" || { printf '%s\tCRASH\tllvm-dis failed\n' "$case" >> "$C/result.tsv"; bad=$((bad+1)); continue; }
  case $shader in *_callee|*_object) continue;; esac
  if grep -q '^!air.mesh = ' "$ll"; then st=mesh; elif grep -q '^!air.vertex = ' "$ll"; then st=vertex
  elif grep -q '^!air.fragment = ' "$ll"; then st=fragment; else st=kernel; fi
  spv="$C/$case.spv"; vals="$spv"
  entry_of() { sed -n 's/^define [^@]*@\([A-Za-z0-9_]*\)(.*/\1/p' "$1" | head -1; }
  if [ $st = mesh ] && [[ $shader == *_mesh ]]; then
    obj="$C/${shader%_mesh}_object.$suffix.ll"; [ -f "$obj" ] || $RUN_PREFIX "$LLVM_DIS" "${obj%.ll}.air" -o "$obj"
    out=$($RUN_PREFIX "$C/harness" "$LIB" mesh2 "$ll" "$(entry_of "$ll")" "$C/$case" "$obj" "$(entry_of "$obj")" 2>&1)
    vals="$C/$case.object.spv $C/$case.kernel.spv $C/$case.vertex.spv"
  elif [ $st = mesh ]; then
    out=$($RUN_PREFIX "$C/harness" "$LIB" mesh "$ll" "$(entry_of "$ll")" "$C/$case" 2>&1); vals="$C/$case.kernel.spv $C/$case.vertex.spv"
  elif [[ $shader == *_caller ]]; then
    callee="$C/${shader%_caller}_callee.$suffix.ll"; [ -f "$callee" ] || $RUN_PREFIX "$LLVM_DIS" "${callee%.ll}.air" -o "$callee"
    out=$($RUN_PREFIX "$C/harness" "$LIB" link "$ll" $st "$spv" "addone=$callee" 2>&1)
  elif grep -q '!"air.visible_function_table"\|!"air.intersection_function_table"\|@air.intersect.intersection_function_buffer' "$ll"; then
    cands=$(sed -n 's/^define [^@]*@\([A-Za-z0-9_]*\)(.*/\1/p' "$ll" | grep -v "^k_" )
    out=$($RUN_PREFIX "$C/harness" "$LIB" table "$ll" $st "$spv" $cands 2>&1)
  elif [ -f "$HERE/src/$shader.fc" ]; then
    out=$($RUN_PREFIX "$C/harness" "$LIB" fc "$ll" $st "$spv" $(cat "$HERE/src/$shader.fc") 2>&1)
  else
    out=$($RUN_PREFIX "$C/harness" "$LIB" translate "$ll" $st "$spv" 2>&1)
  fi
  rc=$?
  if [ $rc = 0 ]; then
    verdict=TRANSLATED; detail=$(echo "$out" | tail -1)
    for f in $vals; do
      if ! v=$("$VAL" --target-env vulkan1.3 "$f" 2>&1); then verdict=INVALID-SPV; detail=$(echo "$v" | head -1); fi
    done
  elif [ $rc = 2 ]; then verdict=REFUSED; detail=$out
  else verdict=CRASH; detail="rc=$rc $out"; fi
  printf '%s\t%s\t%s\n' "$case" "$verdict" "$(echo "$detail" | head -1 | cut -c1-200)" >> "$C/result.tsv"
  case $verdict in
    INVALID-SPV|CRASH) bad=$((bad+1));;
    REFUSED) grep -qx "$shader" "$HERE/expected_refusals.txt" || { bad=$((bad+1)); echo "UNEXPECTED REFUSAL: $case"; };;
  esac
done
for v in TRANSLATED REFUSED INVALID-SPV CRASH; do echo "$v: $(grep -c "	$v	" "$C/result.tsv")"; done
exit $((bad > 0))
