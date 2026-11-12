#!/bin/bash
# build_engine.sh <10.6 SDK> <out dir> [library root for the import check ...]
# 1. Builds the engine bundle for x86_64 and i386 against the 10.6 SDK at deployment target 10.6 (one binary serves
#    every release: it exports the union of the entry names and answers 0 for one the original engine lacks).
# 2. Compares its exports with the measured name list of every release (gl/names): none may be missing.
# 3. Builds the bundle and a stand-in engine for the host and runs the host tests (forwarding, logging, failure).
#    The forwarding tests need the stand-in, so they use the test build (NM_ENGINE_TEST). The default build is
#    then proved to ignore the two test variables, with the test build as the control that the check can see them,
#    and the packaged bundles are searched for the variable names.
# 4. Checks the 10.6 build's imports against the given library roots (SDK stub libraries or libraries copied from
#    a release image); the checker carries its own controls.
set -uo pipefail
SDK=${1:?10.6 SDK}; OUT=${2:?out dir}; shift 2; H=$(cd "$(dirname "$0")" && pwd); G=$(dirname "$H"); mkdir -p "$OUT"
fail=0; CF="-std=c11 -Wall -Wextra -Werror -fvisibility=hidden -O2"
for a in x86_64 i386; do
  clang -arch $a -mmacosx-version-min=10.6 -isysroot "$SDK" $CF -bundle -o "$OUT/GLEngine-$a" "$H/nm_engine.c" > "$OUT/engine-$a.log" 2>&1; rc=$?
  echo "engine $a: rc=$rc errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: error:' "$OUT/engine-$a.log") warnings=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: warning:' "$OUT/engine-$a.log")"
  [ $rc = 0 ] || { fail=1; head -5 "$OUT/engine-$a.log"; continue; }
  nm -arch $a -gU "$OUT/GLEngine-$a" | awk '{print $NF}' | sed 's/^_//' | sort > "$OUT/engine-$a.exports"
  for f in "$G"/names/gli-framework-*.txt "$G"/names/gli-windowserver-*.txt; do
    miss=$(comm -23 <(sort "$f") "$OUT/engine-$a.exports" | xargs)
    echo "  $a exports against $(basename "$f" .txt): $(wc -l < "$f" | xargs) names required, missing $(echo $miss | wc -w | xargs) $miss"
    [ -z "$miss" ] || fail=1
  done
done
# control for the export comparison: a list with one invented name must be reported as one missing
c=$(comm -23 <( (cat "$G/names/gli-framework-10.11.txt"; echo gliNotAnEntry) | sort) "$OUT/engine-x86_64.exports" | xargs)
echo "  control (one invented name added to the 10.11 list): missing = [$c]"; [ "$c" = gliNotAnEntry ] || fail=1
if [ -f "$OUT/GLEngine-x86_64" ] && [ -f "$OUT/GLEngine-i386" ]; then lipo -create "$OUT/GLEngine-x86_64" "$OUT/GLEngine-i386" -o "$OUT/GLEngine" && echo "universal bundle: $(lipo -archs "$OUT/GLEngine")"; fi
# host tests: same sources, built for x86_64 with the current SDK so they can run on the build host
T=$OUT/host; mkdir -p "$T"
clang -arch x86_64 $CF -DNM_ENGINE_TEST -bundle -o "$T/GLEngine" "$H/nm_engine.c" > "$T/build.log" 2>&1 &&
clang -arch x86_64 $CF -bundle -o "$T/GLEngine-default" "$H/nm_engine.c" >> "$T/build.log" 2>&1 &&
clang -arch x86_64 $CF -DNM_ENGINE_SYSLOG -bundle -o "$T/GLEngine-syslog" "$H/nm_engine.c" >> "$T/build.log" 2>&1 &&
clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -o "$T/env_ignore_test" "$H/test/env_ignore_test.c" >> "$T/build.log" 2>&1 &&
clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -fvisibility=hidden -bundle -o "$T/fake_engine" "$H/test/fake_engine.c" >> "$T/build.log" 2>&1 &&
clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -o "$T/engine_test" "$H/test/engine_test.c" >> "$T/build.log" 2>&1 &&
clang -arch x86_64 -std=c11 -Wall -Wextra -Werror -o "$T/no_engine_test" "$H/test/no_engine_test.c" >> "$T/build.log" 2>&1
rc=$?; [ $rc = 0 ] || { echo "host test build failed"; head -20 "$T/build.log"; fail=1; }
if [ $rc = 0 ]; then
  "$T/engine_test" "$T/GLEngine" "$T/fake_engine" "$T/engine.log" || fail=1
  "$T/no_engine_test" "$T/GLEngine" || fail=1
  "$T/env_ignore_test" "$T/GLEngine-default" "$T/fake_engine" "$T/env.log" ignored || fail=1
  "$T/env_ignore_test" "$T/GLEngine-syslog" "$T/fake_engine" "$T/env.log" ignored || fail=1
  "$T/env_ignore_test" "$T/GLEngine" "$T/fake_engine" "$T/env.log" honoured || fail=1
  "$T/env_ignore_test" "$T/GLEngine" "$T/fake_engine" "$T/env.log" nolog || fail=1
fi
# no build that could be packaged may even contain the variable names or a call that reads the environment
for b in "$OUT/GLEngine-x86_64" "$OUT/GLEngine-i386" "$T/GLEngine-default" "$T/GLEngine-syslog"; do
  [ -f "$b" ] || continue
  n=$(strings -a "$b" | grep -cE 'NM_GL_LOG|NM_GL_APPLE_ENGINE'); g=$(nm -u "$b" 2>/dev/null | grep -cE '_getenv$|_open(\$|$)')
  echo "  $(basename "$b"): variable names in the binary $n, imports of getenv or open $g"; [ "$n" = 0 ] && [ "$g" = 0 ] || fail=1
done
n=$(strings -a "$T/GLEngine" | grep -cE 'NM_GL_LOG|NM_GL_APPLE_ENGINE'); g=$(nm -u "$T/GLEngine" | grep -cE '_getenv$|_open(\$|$)')
echo "  control, test build: variable names $n, imports of getenv or open $g"; [ "$n" -ge 2 ] && [ "$g" -ge 2 ] || fail=1
if [ $# -gt 0 ]; then python3 "$G/tools/usersym.py" "$OUT/GLEngine-x86_64" "given libraries" "$@" | tee "$OUT/imports.txt"; grep -q "missing 0$" "$OUT/imports.txt" || fail=1; fi
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
