#!/bin/bash
# test_osver.sh - libnvmtl_osver.dylib end to end, on this Mac, no GPU. Builds the library (for this Mac's architecture,
# with the test-only NVMTL_OSVER_TEST_REAL switch) and a stand-in program linked with a weak load command for it, inside
# app bundles named like the game and like another program, then checks what the program's FIRST read sees.
#   bash tools/test_osver.sh          exit 0 when every arm holds
set -uo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
fails=0
arm() {   # arm <name> <1 = holds> <what the program printed>
  if [ -z "$3" ]; then echo "FAIL  $1"; echo "      the program printed nothing - it did not run"; fails=$((fails + 1)); return; fi
  if [ "$2" = 1 ]; then echo "ok    $1"; else echo "FAIL  $1"; echo "      got: $3"; fails=$((fails + 1)); fi
}
mkdir -p "$T/lib"
xcrun clang -dynamiclib -fobjc-arc -O2 -Wall -Werror -DNVMTL_OSVER_TEST -install_name /Library/GPUBundles/nvmtl/libnvmtl_osver.dylib \
  -I"$HERE/plugin" -o "$T/lib/libnvmtl_osver.dylib" "$HERE/plugin/nvmtl_osver.m" -framework Foundation -framework CoreFoundation || exit 2
app() {   # app <dir name> <executable> <bundle id>: prints the executable's path
  local d="$T/$1.app/Contents"; mkdir -p "$d/MacOS"
  printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>%s</string><key>CFBundleExecutable</key><string>%s</string></dict></plist>\n' "$3" "$2" > "$d/Info.plist"
  xcrun clang -fobjc-arc -O1 -Wall -Werror -o "$d/MacOS/$2" "$HERE/tools/test_osver_probe.m" -framework Foundation \
    -Wl,-weak_library,"$T/lib/libnvmtl_osver.dylib" || exit 2
  echo "$d/MacOS/$2"
}
GAME=$(app Cyberpunk2077 Cyberpunk2077 com.cdprojektred.cyberpunk.steam) || exit 2
OTHER=$(app Other Cyberpunk2077 com.example.other) || exit 2
# clean <VAR=value>... <program> [args]: runs the program with exactly that environment plus HOME, nothing inherited
clean() { /usr/bin/python3 -c 'import os, sys
env = {"HOME": os.environ.get("HOME", "/")}
a = sys.argv[1:]
while a and "=" in a[0] and not a[0].startswith("/"):
    k, v = a.pop(0).split("=", 1); env[k] = v
os.execve(a[0], a, env)' "$@"; }
run() { clean DYLD_LIBRARY_PATH="$T/lib" "$@"; }
REAL=$(sw_vers -productVersion)

lc=$(otool -l "$GAME" | grep -A2 LC_LOAD_WEAK_DYLIB)
[[ $lc == *libnvmtl_osver.dylib* ]]; arm "the stand-in carries a WEAK load command for the library, as the game's build will" $((! $?)) "${lc:-none}"
o=$(run NVMTL_OSVER_TEST_REAL=26.1 "$GAME")
[[ $o == "version 27.0.0 atleast27 1 atleast99 0 "*" lib answered" ]]; arm "the game on (pretend) macOS 26.1: its first read in main() says 27.0.0, at-least 27 YES, at-least 99 NO" $((! $?)) "$o"
o=$(run NVMTL_OSVER_TEST_REAL=15.7.3 "$GAME")
[[ $o == "version 27.0.0 "*" lib answered" ]]; arm "and on (pretend) macOS 15.7.3" $((! $?)) "$o"
o=$(run NVMTL_OSVER_TEST_REAL=26.1 "$OTHER")
# "loaded", not "answered": the library ran and installed nothing (the version printed is this Mac's real one)
[[ $o == "version ${REAL}"* || $o == "version ${REAL}.0 "* ]] && [[ $o == *" lib loaded" ]]; arm "the same executable name in another bundle loads the library and is told nothing" $((! $?)) "$o"
o=$(run "$GAME")
# What the game is told on this Mac itself depends on this Mac: at 27 or later the real answer stands (it is never lowered),
# below that it is raised like the pretend versions above.
if [ "${REAL%%.*}" -ge 27 ]; then
  [[ $o == *" lib loaded" ]]; arm "on this Mac's real macOS $REAL the game keeps the real answer (never lowered)" $((! $?)) "$o"
else
  [[ $o == "version 27.0.0 "*" lib answered" ]]; arm "on this Mac's real macOS $REAL the game is told 27.0.0, as on the pretend versions" $((! $?)) "$o"
fi
o=$(run NVMTL_OSVER_TEST_REAL=26.1 NVMTL_PROCESS_ENV=1 NVMTL_APP_PROFILE=0 "$GAME")
[[ $o == *" lib loaded" ]]; arm "NVMTL_APP_PROFILE=0 in the kept environment turns it off" $((! $?)) "$o"
o=$(run NVMTL_OSVER_TEST_REAL=26.1 NVMTL_PROCESS_ENV=1 NVMTL_OS_VERSION=0 "$GAME")
[[ $o == *" lib loaded" ]]; arm "NVMTL_OS_VERSION=0 in the kept environment turns it off" $((! $?)) "$o"
o=$(run NVMTL_PROCESS_ENV=1 NVMTL_OS_VERSION=99.1.2 "$OTHER")
[[ $o == "version 99.1.2 atleast27 1 atleast99 1 "*" lib answered" ]]; arm "NVMTL_OS_VERSION with NVMTL_PROCESS_ENV=1 is used as given, by any program that loads the library" $((! $?)) "$o"
o=$(run NVMTL_OS_VERSION=99.1.2 "$OTHER")
[[ $o == *" lib loaded" ]]; arm "without NVMTL_PROCESS_ENV=1 that knob is forgotten, as the release plugin forgets knobs" $((! $?)) "$o"
o=$(clean NVMTL_OSVER_TEST_REAL=26.1 "$GAME" 2>&1)
[[ $o == *" lib absent" ]]; arm "with the library not installed the weak load is skipped and the game still starts" $((! $?)) "$o"
o=$(run NVMTL_OSVER_TEST_REAL=26.1 "$GAME" base-only)
[[ $o == "version 27.0.0 "* ]]; arm "the library's answer holds where replacing NSProcessInfo alone does not (next arm)" $((! $?)) "$o"
o=$(clean "$GAME" base-only)
[[ $o != "version 99."* && $o != *"class NSProcessInfo "* ]]; arm "NEGATIVE: replacing the method on NSProcessInfo alone (the plugin's old way) is not what the real instance answers" $((! $?)) "$o"
echo "$( [ $fails = 0 ] && echo PASS || echo FAIL ): $fails failed"
exit $((fails != 0))
