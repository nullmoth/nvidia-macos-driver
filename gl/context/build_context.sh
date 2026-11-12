#!/bin/bash
# build_context.sh <folder holding the 10.11 framework headers> <Mesa build dir (build_mesa_gl.sh)> <out dir> [library root for the import check ...]
#
# Builds the engine bundle WITH the context layer for x86_64 at deployment target 10.11 and proves it offline:
#   1. the 974-slot table file is generated from the 10.11 header and Mesa's entry stubs;
#   2. everything compiles with warnings as errors: the engine and the contract side against the 10.11 OpenGL and
#      IOSurface headers, the Mesa side with Mesa's own compile flags (read from its build);
#   3. two bundles are linked: the PRODUCT build (no test macro) and the TEST build (context layer forced on with
#      the software backend, software stand-in for kernel surfaces);
#   4. the product bundle's exports are compared with the measured 10.11 lists and its imports with the libraries
#      of a real 10.11.6 system (the checker runs its own controls);
#   5. the product bundle must not contain the test hooks or read the environment;
#   6. the host tests run: the engine tests of the first stage against the product-shaped build, and the end-to-end
#      test that draws through the table on Mesa's CPU rasteriser and counts pixels. No GPU is touched.
# The 10.11 SDK held by the build host has headers but no link libraries, so the link uses the current SDK's
# libraries at deployment target 10.11 and step 4 is what ties the result to 10.11.6.
set -uo pipefail
GLH=${1:?OpenGL headers of the release}; IOSH=${2:?IOSurface headers}; MB=${3:?}; OUT=${4:?}; REL=${5:?release, e.g. 10.11}; TGT=${6:?deployment target}; SLOTS=${7:?table slots of the release}; shift 7; H=$(cd "$(dirname "$0")" && pwd); G=$(dirname "$H"); mkdir -p "$OUT"; fail=0
ERR='^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:'
MS=$(cd "$MB/.." && pwd)   # Mesa source root: the build dir sits inside it
nm -arch x86_64 -g "$MB/src/mesa/glapi/shared-glapi/libglapi.a" | grep ' T __dispatch_stub_' > "$OUT/mesa-stubs.txt"
python3 "$G/glue/gen_dispatch.py" "$GLH/gliDispatch.h" "$OUT/mesa-stubs.txt" "$OUT/nm_dispatch.c" "$OUT/dispatch.report.txt" || fail=1
[ "$SLOTS" = auto ] && SLOTS=$(sed -n 's/^table entries \([0-9]*\).*/\1/p' "$OUT/dispatch.report.txt" | head -1)
# Mesa's compile flags for a gallium source, from its own build description
python3 - "$MB" > "$OUT/mesa.flags" <<'PY'
import json, shlex, sys, os
mb = sys.argv[1]; cc = json.load(open(os.path.join(mb, "compile_commands.json")))
e = next(c for c in cc if c["file"].endswith("softpipe/sp_screen.c"))
out = []
for a in shlex.split(e["command"]):
    if a.startswith("-I"):
        p = a[2:]; out.append("-I" + (p if os.path.isabs(p) else os.path.normpath(os.path.join(mb, p))))
    elif a.startswith("-D") or a.startswith("-std="): out.append(a)
print(" ".join(shlex.quote(x) for x in out))
PY
# the 10.11 headers are reached as <OpenGL/...> and <IOSurface/...> through a private include root (the unpacked
# frameworks have no top-level Headers link, so they cannot be used with -F)
mkdir -p "$OUT/inc"; ln -sfn "$GLH" "$OUT/inc/OpenGL"; ln -sfn "$IOSH" "$OUT/inc/IOSurface"
A="-arch x86_64 -mmacosx-version-min=$TGT"
# The 10.6 floor: no compiler thread-local storage (per-thread keys instead, the same switch as the Mesa build) and
# no newer C++ library on the system (the nine symbols it would supply are in nm_cxx_min.cpp; the rest come from the
# older library, linked through the 10.6 SDK's stub named by NM_LIBSTDCXX).
X6=""; CXXLIB=""; LINKER=clang++
if [ "$TGT" = 10.6 ]; then X6="-DNM_NO_COMPILER_TLS"; LINKER=clang; CXXLIB="$OUT/nm_cxx_min.o ${NM_LIBSTDCXX:?path of the 10.6 SDK libstdc++.6.dylib}"; fi
W="-Wall -Wextra -Werror -Wno-deprecated-declarations -fvisibility=hidden -O2"
cc_apple() { clang $A -std=c11 $W -I"$OUT/inc" -I"$G/glue" "$@"; }
obj() { # <name> <log> <command...>
  local n=$1; shift; "$@" > "$OUT/$n.log" 2>&1; local rc=$?
  echo "compile $n: rc=$rc errors=$(grep -cE "$ERR" "$OUT/$n.log")"; [ $rc = 0 ] || { fail=1; grep -E 'error' "$OUT/$n.log" | head -6 | cut -c1-230; }; }
eval "obj nm_mesa clang $A $X6 -DNM_WITH_ZINK -I'$G/mesa' $W -Wno-unused-parameter -Wno-missing-field-initializers $(cat "$OUT/mesa.flags") -I'$MS/src/gallium/winsys' -I'$MS/src/gallium/drivers' -I'$MS/src/mesa' -I'$MS/src/mapi' -c '$H/nm_mesa.c' -o '$OUT/nm_mesa.o'"
obj nm_dispatch   cc_apple -c "$OUT/nm_dispatch.c" -o "$OUT/nm_dispatch.o"
obj nm_apple_ext  cc_apple -c "$G/glue/nm_apple_ext.c" -o "$OUT/nm_apple_ext.o"
obj nm_compat     clang $A -std=c11 -Wall -Wextra -Werror -fvisibility=hidden -O2 -c "$H/nm_compat.c" -o "$OUT/nm_compat.o"
[ "$TGT" = 10.6 ] && obj nm_cxx_min clang++ $A -std=c++17 -Wall -Wextra -Werror -fvisibility=hidden -O2 -c "$H/nm_cxx_min.cpp" -o "$OUT/nm_cxx_min.o"
for v in product test; do
  D="-DNM_ENGINE_OWNER"; [ $v = test ] && D="$D -DNM_ENGINE_TEST"
  obj nm_context-$v cc_apple $D -c "$H/nm_context.c" -o "$OUT/nm_context-$v.o"
  obj nm_engine-$v  cc_apple $D -c "$G/engine/nm_engine.c" -o "$OUT/nm_engine-$v.o"
done
[ $fail = 0 ] || { echo "result: FAIL"; exit 1; }
# every static library of the Mesa build: API layer, state tracker, CPU rasteriser and zink (GL on Vulkan)
LIBS=$(find "$MB/src" -name '*.a' | sort | tr '\n' ' ')
echo "Mesa static libraries linked: $(echo $LIBS | wc -w | xargs)"
# only the engine entry points leave the bundle (Mesa's own symbols stay inside); the test build adds its two hooks
(sed 's/^/_/' "$G/names/gli-framework-$REL.txt"; echo _gliSwapBuffers; echo _gliCreateContextWithShared; echo _gliGetMemoryAnnotationList) | sort -u > "$OUT/exports-product.txt"
(cat "$OUT/exports-product.txt"; echo _nm_test_own_software; echo _nm_test_register_surface) > "$OUT/exports-test.txt"
for v in product test; do
  $LINKER $A -bundle -Wl,-exported_symbols_list,"$OUT/exports-$v.txt" -o "$OUT/GLEngine-$v" $CXXLIB "$OUT/nm_engine-$v.o" "$OUT/nm_context-$v.o" "$OUT/nm_dispatch.o" "$OUT/nm_apple_ext.o" "$OUT/nm_mesa.o" "$OUT/nm_compat.o" \
    $LIBS -framework IOSurface -framework IOKit -framework CoreFoundation -lz -Wl,-dead_strip -Wl,-no_warn_duplicate_libraries > "$OUT/link-$v.log" 2>&1; rc=$?
  echo "link $v bundle: rc=$rc, undefined symbols reported $(grep -c '^  "_' "$OUT/link-$v.log"), size $(stat -f %z "$OUT/GLEngine-$v" 2>/dev/null || echo 0) bytes"
  [ $rc = 0 ] || { fail=1; grep -A1 '^  "_' "$OUT/link-$v.log" | head -24 | cut -c1-200; }
done
[ $fail = 0 ] || { echo "result: FAIL"; exit 1; }
P=$OUT/GLEngine-product; T=$OUT/GLEngine-test
nm -arch x86_64 -gU "$P" | awk '{print $NF}' | sed 's/^_//' | sort > "$OUT/product.exports"
for f in "$G/names/gli-framework-$REL.txt" "$G/names/gli-windowserver-$REL.txt"; do
  miss=$(comm -23 <(sort "$f") "$OUT/product.exports" | xargs)
  echo "product exports against $(basename "$f" .txt): $(wc -l < "$f" | xargs) names required, missing $(echo $miss | wc -w | xargs) $miss"; [ -z "$miss" ] || fail=1
done
extra=$(grep -cvE '^gli' "$OUT/product.exports"); echo "product exports that are not engine entry points: $extra (of $(wc -l < "$OUT/product.exports" | xargs))"; [ "$extra" = 0 ] || fail=1
n=$(strings -a "$P" | grep -cE 'NM_GL_LOG|NM_GL_APPLE_ENGINE'); h=$(nm -arch x86_64 "$P" | grep -cE 'nm_test_'); ht=$(nm -arch x86_64 "$T" | grep -cE 'nm_test_')
echo "product bundle: test variable names $n, test hooks $h (control, test bundle: $ht hooks)"; [ "$n" = 0 ] && [ "$h" = 0 ] && [ "$ht" -ge 2 ] || fail=1
if [ $# -gt 0 ]; then
  python3 "$G/tools/usersym.py" "$P" "the libraries of $REL" "$@" | tee "$OUT/imports.txt" | cut -c1-160; grep -q "missing 0$" "$OUT/imports.txt" || fail=1
  # weak imports count too: a weak symbol the release lacks is a null pointer at the call
  wm=$(grep -E 'weak +[0-9]+ missing +[1-9]' "$OUT/imports.txt" | wc -l | xargs); echo "libraries with a weak import the release lacks: $wm"; [ "$wm" = 0 ] || fail=1
fi
# the shims that replace descriptor-relative and exclusive calls: the same source with three names prefixed, run in
# a fresh directory
rm -rf "$OUT/compat-work"; mkdir -p "$OUT/compat-work"
if clang $A -std=c11 -Wall -Wextra -Werror -O2 -Dfchmodat=nm_fchmodat -Dfdopendir=nm_fdopendir -Drenamex_np=nm_renamex_np -c "$H/nm_compat.c" -o "$OUT/nm_compat_test.o" > "$OUT/compat_test.build.log" 2>&1 &&
   clang $A -std=c11 -Wall -Wextra -Werror -Dfchmodat=nm_fchmodat -Dfdopendir=nm_fdopendir -Drenamex_np=nm_renamex_np -o "$OUT/compat_test" "$H/test/compat_test.c" "$OUT/nm_compat_test.o" >> "$OUT/compat_test.build.log" 2>&1; then
  "$OUT/compat_test" "$OUT/compat-work" > "$OUT/compat_test.out" 2>&1 || { fail=1; grep '^FAIL' "$OUT/compat_test.out"; }
  echo "shims: $(tail -1 "$OUT/compat_test.out")"
else fail=1; echo "shim test build failed"; head -8 "$OUT/compat_test.build.log" | cut -c1-220; fi
clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -Wno-deprecated-declarations -Wno-four-char-constants -I"$OUT/inc" -o "$OUT/context_test" "$H/test/context_test.c" \
  -framework IOSurface -framework CoreFoundation > "$OUT/context_test.build.log" 2>&1 || { fail=1; head -12 "$OUT/context_test.build.log" | cut -c1-220; }
[ -x "$OUT/context_test" ] && { "$OUT/context_test" "$T" "$SLOTS" 2> "$OUT/context_test.stderr" | tee "$OUT/context_test.out"; rc=${PIPESTATUS[0]}; [ $rc = 0 ] || { fail=1; echo "context_test exit $rc"; tail -5 "$OUT/context_test.stderr"; }; }
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
