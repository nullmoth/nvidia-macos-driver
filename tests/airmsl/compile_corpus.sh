#!/bin/bash
# compile_corpus.sh <outdir>: compile every src/*.metal at each MSL standard for the default target and for the
# air64-apple-macos15.7 target, to .air and .metallib. Needs the Metal toolchain (the laptop). Writes
# <outdir>/compile.tsv: case, std, target, status, air version, triple.
set -u
# Only used to read the AIR version and triple back out; the translator input is made on the build Mac by run_corpus.sh.
AIROPT=$(dirname "$(xcrun -f metal)")/air-opt
[ -x "$AIROPT" ] || { echo "air-opt not found next to metal"; exit 9; }
HERE=$(cd "$(dirname "$0")" && pwd); OUT=${1:?outdir}; mkdir -p "$OUT"
TSV="$OUT/compile.tsv"; : > "$TSV"
for src in "$HERE"/src/*.metal; do
  n=$(basename "$src" .metal)
  for std in metal3.1 metal3.2 metal4.0 metal4.1; do
    for tg in default macos15.7; do
      case=$n.$std.$tg; tgt=(); [ $tg = default ] || tgt=(-target air64-apple-macos15.7)
      if xcrun metal -std=$std ${tgt[@]+"${tgt[@]}"} -I"$HERE/src" -c "$src" -o "$OUT/$case.air" 2> "$OUT/$case.cerr" &&
         "$AIROPT" -S "$OUT/$case.air" -o "$OUT/$case.meta.ll" 2>> "$OUT/$case.cerr" &&
         xcrun metallib "$OUT/$case.air" -o "$OUT/$case.metallib" 2>> "$OUT/$case.cerr"; then
        vn=$(grep -m1 '^!air.version' "$OUT/$case.meta.ll" | sed 's/.*!{!\([0-9]*\)}.*/\1/'); ver=$(grep -m1 "^!$vn = " "$OUT/$case.meta.ll" | sed 's/.*!{//; s/}//; s/i32 //g; s/, /./g'); trip=$(grep -m1 '^target triple' "$OUT/$case.meta.ll" | sed 's/.*= //')
        printf '%s\t%s\t%s\tcompiled\t%s\t%s\n' "$n" $std $tg "$ver" "$trip" >> "$TSV"; rm -f "$OUT/$case.cerr" "$OUT/$case.meta.ll"
      else
        printf '%s\t%s\t%s\tcompile-fail\t%s\t\n' "$n" $std $tg "$(grep -m1 'error' "$OUT/$case.cerr" | tr '\t' ' ' | cut -c1-110)" >> "$TSV"
        rm -f "$OUT/$case.air" "$OUT/$case.meta.ll" "$OUT/$case.metallib"
      fi
    done
  done
done
echo "compiled: $(grep -c compiled "$TSV") failed: $(grep -c compile-fail "$TSV")"
