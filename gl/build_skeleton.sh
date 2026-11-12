#!/bin/bash
# build_skeleton.sh <10.6 SDK> <out dir> [reference dir]
# Builds the two export skeletons for every release whose entry-point names were read from Apple's binaries
# (gl/names), for x86_64 and i386, against the 10.6 SDK at deployment target 10.6, then compares the exports of each
# bundle with the name list it was generated from and, where the reference folder holds it, with the export table of
# Apple's software renderer of the same release (<reference dir>/<release>.softrenderer_exports.txt).
# A control build with one name deliberately left out must be reported as exactly one missing name.
set -uo pipefail
SDK=${1:?10.6 SDK}; OUT=${2:?out dir}; REF=${3:-}; H=$(cd "$(dirname "$0")" && pwd); mkdir -p "$OUT"
exports() { nm -arch "$1" -gU "$2" | awk '{print $NF}' | sed 's/^_//' | sort; }
gen() { # <names file> <macro> <typed name> <out header> [name to drop]
  awk -v m="$2" -v t="$3" -v d="${5:-}" '$1 != "" && $1 != d { print ($1 == t ? m "_TYPED(" $1 ")" : m "(" $1 ")") }' "$1" > "$4"; }
fail=0
for f in "$H"/names/gld-*.txt; do
  r=$(basename "$f" .txt); r=${r#gld-}; era=$(echo "$r" | awk -F. '{printf "%d%02d", $1, $2}')
  gen "$f" GLD gldGetVersion "$OUT/gld_names_$era.h"
  gen "$H/names/gli-framework-$r.txt" GLI gliGetVersion "$OUT/gli_names_$era.h"
  for kind in gld gli; do
    names=$H/names/$kind-$r.txt; [ $kind = gli ] && names=$H/names/gli-framework-$r.txt
    for a in x86_64 i386; do
      b=$OUT/nm_${kind}_skeleton-$r-$a.bundle; K=$(echo $kind | tr a-z A-Z)
      clang -arch $a -mmacosx-version-min=10.6 -isysroot "$SDK" -std=c99 -Wall -Wextra -Werror -fvisibility=hidden -O1 \
        -DNM_GL_ERA=$era "-DNM_${K}_NAMES=\"$OUT/${kind}_names_$era.h\"" -I"$H/skeleton" -bundle -o "$b" "$H/skeleton/${kind}_skeleton.c" \
        > "$OUT/$kind-$r-$a.log" 2>&1; rc=$?
      nerr=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: error:' "$OUT/$kind-$r-$a.log")
      if [ $rc != 0 ]; then echo "$kind $r $a: BUILD FAILED rc=$rc errors=$nerr"; fail=1; continue; fi
      exports $a "$b" > "$OUT/$kind-$r-$a.exports"
      miss=$(comm -23 <(sort "$names") "$OUT/$kind-$r-$a.exports" | wc -l | xargs); extra=$(comm -13 <(sort "$names") "$OUT/$kind-$r-$a.exports" | wc -l | xargs)
      line="$kind $r $a: built, exports $(wc -l < "$OUT/$kind-$r-$a.exports" | xargs) of $(wc -l < "$names" | xargs) names, missing $miss, extra $extra"
      if [ $kind = gld ] && [ -n "$REF" ] && [ -f "$REF/$r.softrenderer_exports.txt" ]; then
        s=$REF/$r.softrenderer_exports.txt
        line="$line; against Apple's software renderer ($(wc -l < "$s" | xargs) exports): missing $(comm -23 <(sort "$s") "$OUT/$kind-$r-$a.exports" | wc -l | xargs), extra $(comm -13 <(sort "$s") "$OUT/$kind-$r-$a.exports" | wc -l | xargs)"
      fi
      echo "$line"; [ "$miss" = 0 ] && [ "$extra" = 0 ] || fail=1
    done
  done
done
# control: the comparison must notice one missing entry point
f=$H/names/gld-10.07.txt; gen "$f" GLD gldGetVersion "$OUT/gld_names_control.h" gldFlushContext
clang -arch x86_64 -mmacosx-version-min=10.6 -isysroot "$SDK" -std=c99 -fvisibility=hidden -DNM_GL_ERA=1007 "-DNM_GLD_NAMES=\"$OUT/gld_names_control.h\"" \
  -I"$H/skeleton" -bundle -o "$OUT/control.bundle" "$H/skeleton/gld_skeleton.c" > "$OUT/control.log" 2>&1
c=$(comm -23 <(sort "$f") <(exports x86_64 "$OUT/control.bundle") | xargs)
echo "control (gldFlushContext left out on purpose): comparison reports missing = [$c]"; [ "$c" = gldFlushContext ] || fail=1
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
