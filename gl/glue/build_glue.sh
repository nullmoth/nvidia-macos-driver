#!/bin/bash
# build_glue.sh <SDK of the release (holds OpenGL/gliDispatch.h)> <Mesa build dir> <out dir> <release label>
# 1. Generates the function-table file for the release from its own header and Mesa's entry-stub symbols.
# 2. Compiles it and the Apple-extension entries for x86_64 at deployment target 10.7 (Mesa's own floor).
# 3. Link check without a context layer: every Mesa entry stub the two objects call must be defined in Mesa's API
#    archive; the only other undefined symbols allowed are the two context-layer functions and the C library.
# 4. Builds and runs the host test of the Apple-extension entries.
set -uo pipefail
SDK=${1:?}; MB=${2:?}; OUT=${3:?}; REL=${4:?}; H=$(cd "$(dirname "$0")" && pwd); mkdir -p "$OUT"; fail=0
HD=$SDK/System/Library/Frameworks/OpenGL.framework/Versions/A/Headers
A=$MB/src/mesa/glapi/shared-glapi/libglapi.a
nm -arch x86_64 -g "$A" | grep ' T __dispatch_stub_' > "$OUT/mesa-stubs.txt"
python3 "$H/gen_dispatch.py" "$HD/gliDispatch.h" "$OUT/mesa-stubs.txt" "$OUT/nm_dispatch_$REL.c" "$OUT/dispatch-$REL.report.txt" || fail=1
# the release's headers are reached as <OpenGL/...> through a private include root, not through its whole SDK
mkdir -p "$OUT/inc"; ln -sfn "$HD" "$OUT/inc/OpenGL"
CF="-arch x86_64 -mmacosx-version-min=10.7 -std=c11 -Wall -Wextra -Werror -Wno-deprecated-declarations -fvisibility=hidden -O2 -I$OUT/inc -I$H"
for f in "$OUT/nm_dispatch_$REL.c" "$H/nm_apple_ext.c"; do
  o=$OUT/$(basename "$f" .c).o; clang $CF -c "$f" -o "$o" > "$o.log" 2>&1; rc=$?
  echo "compile $(basename "$f"): rc=$rc errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$o.log")"; [ $rc = 0 ] || { fail=1; grep -E 'error:' "$o.log" | head -5 | cut -c1-220; }
done
if [ $fail = 0 ]; then
  ld -r -arch x86_64 "$OUT/nm_dispatch_$REL.o" "$OUT/nm_apple_ext.o" -o "$OUT/glue-$REL.o" 2> "$OUT/ld-r.log"
  nm -arch x86_64 -u "$OUT/glue-$REL.o" | awk '{print $NF}' | sort -u > "$OUT/und.txt"
  nm -arch x86_64 -g "$A" | awk '$2 == "T" {print $3}' | sort -u > "$OUT/mesa-def.txt"
  want=$(grep -c '^__dispatch_stub_' "$OUT/und.txt"); miss=$(grep '^__dispatch_stub_' "$OUT/und.txt" | comm -23 - "$OUT/mesa-def.txt" | wc -l | xargs)
  other=$(grep -v '^__dispatch_stub_' "$OUT/und.txt" | grep -vE '^_(nm_gl_enter|nm_gl_refuse|pthread_mutex_lock|pthread_mutex_unlock|__stack_chk_fail|__stack_chk_guard|__bzero)$' | xargs)
  echo "link check: Mesa entry stubs called $want, not defined in Mesa's API archive $miss; other undefined beyond the context layer and libc: [$other]"
  c=$( (cat "$OUT/und.txt"; echo __dispatch_stub_NotARealEntry) | grep '^__dispatch_stub_' | sort -u | comm -23 - "$OUT/mesa-def.txt" | xargs)
  echo "control (one invented stub added): not defined = [$c]"
  [ "$miss" = 0 ] && [ -z "$other" ] && [ "$c" = __dispatch_stub_NotARealEntry ] || fail=1
  echo "table object: $(nm -arch x86_64 "$OUT/nm_dispatch_$REL.o" | grep -c ' t _nm_t_') thunks, $(nm -arch x86_64 "$OUT/nm_dispatch_$REL.o" | grep -c ' t _nm_r_') refusals"
fi
if clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -Wno-deprecated-declarations -I"$OUT/inc" -I"$H" -o "$OUT/apple_ext_test" "$H/test/apple_ext_test.c" "$H/nm_apple_ext.c" > "$OUT/test-build.log" 2>&1; then
  "$OUT/apple_ext_test" || fail=1
else fail=1; head -8 "$OUT/test-build.log"; fi
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
