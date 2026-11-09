#!/bin/bash
# package/dmc_fetch.sh against local files (no network): pins, refusal of a changed file, reuse of an earlier install.
#   bash test_dmc_fetch.sh      -> 0 failed
cd "$(dirname "$0")/.." || exit 1
fails=0
arm() { if [ "$2" = 1 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails+1)); fi; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
[ "$(id -u)" = 0 ] && { echo "skip: tests may not replace the pins as root"; exit 0; }
mkdir "$T/src"; for f in tgl_dmc_ver2_12.bin adlp_dmc_ver2_16.bin mtl_dmc.bin; do head -c 4000 /dev/urandom > "$T/src/$f"; done
cat > "$T/pins" <<PINS
BASE="file://$T/src"
CURL_PROTO='=file'
COMMIT=x
dmc_sha() { /usr/bin/shasum -a 256 "$T/src/\$1" | cut -d' ' -f1; }
PINS
export DMC_TEST_PINS="$T/pins"
# the script appends ?id=COMMIT to the URL; a file:// URL ignores a query, so name the files plainly
out=$(bash package/dmc_fetch.sh --dest "$T/d1" --installed "$T/none" 2>&1); rc=$?
arm "all files fetched and verified: DMC_OK" "$([ $rc = 0 ] && echo "$out" | grep -q '^DMC_OK ' && [ -f "$T/d1/i915/mtl_dmc.bin" ] && echo 1 || echo 0)"
out=$(bash package/dmc_fetch.sh --dest "$T/d2" --installed "$T/d1" 2>&1); rc=$?
arm "an earlier install with matching files is reused without a download" "$([ $rc = 0 ] && echo "$out" | grep -q 'reused' && echo 1 || echo 0)"
cat > "$T/pins2" <<PINS
BASE="file://$T/src"
CURL_PROTO='=file'
COMMIT=x
dmc_sha() { echo 0000000000000000000000000000000000000000000000000000000000000000; }
PINS
export DMC_TEST_PINS="$T/pins2"
out=$(bash package/dmc_fetch.sh --dest "$T/d3" --installed "$T/none" 2>&1); rc=$?
arm "a file that does not match its pin is refused" "$([ $rc = 1 ] && echo "$out" | grep -q 'does not match its pinned SHA-256' && echo 1 || echo 0)"
arm "and nothing is left in the destination" "$([ ! -e "$T/d3/i915" ] && echo 1 || echo 0)"
out=$(bash package/dmc_fetch.sh 2>&1); rc=$?
arm "no destination is a usage error" "$([ $rc = 2 ] && echo 1 || echo 0)"
arm "the pins are the three real files" "$(grep -c '^    [a-z0-9_.]*) *echo [0-9a-f]\{64\};;' package/dmc_pins.sh | grep -q '^3$' && echo 1 || echo 0)"
arm "the fetch script and the loader share one set of pins" "$(grep -q 'dmc_pins.sh' package/dmc_fetch.sh && grep -q 'dmc_pins.sh' package/nm-dmc-load.sh && ! grep -q 'echo [0-9a-f]\{64\};;' package/dmc_fetch.sh && echo 1 || echo 0)"

# ---- the loader: stub sysctl and stub push
export DMC_TEST_PINS="$T/pins" NM_DMC_TEST=1
mkdir -p "$T/dest/i915"; cp "$T/src/mtl_dmc.bin" "$T/dest/i915/"
printf '#!/bin/bash\necho "$STUB_WANT"\n' > "$T/sysctl"; chmod +x "$T/sysctl"
printf '#!/bin/bash\necho PUSHED "$1" >> "$STUB_LOG"\n' > "$T/push"; chmod +x "$T/push"
export NM_DMC_SYSCTL="$T/sysctl" NM_DMC_PUSH="$T/push" STUB_LOG="$T/log"
: > "$T/log"; STUB_WANT=mtl_dmc.bin bash package/nm-dmc-load.sh --dest "$T/dest" >/dev/null 2>&1; rc=$?
arm "the file the driver names is pushed after its pin is checked" "$([ $rc = 0 ] && grep -q "PUSHED $T/dest/i915/mtl_dmc.bin" "$T/log" && echo 1 || echo 0)"
: > "$T/log"; STUB_WANT=none bash package/nm-dmc-load.sh --dest "$T/dest" >/dev/null 2>&1; rc=$?
arm "a GPU that needs no file pushes nothing and succeeds" "$([ $rc = 0 ] && [ ! -s "$T/log" ] && echo 1 || echo 0)"
: > "$T/log"; STUB_WANT=../../etc/passwd bash package/nm-dmc-load.sh --dest "$T/dest" >/dev/null 2>&1; rc=$?
arm "a file name the pins do not list is refused" "$([ $rc = 1 ] && [ ! -s "$T/log" ] && echo 1 || echo 0)"
echo tampered >> "$T/dest/i915/mtl_dmc.bin"; : > "$T/log"; STUB_WANT=mtl_dmc.bin bash package/nm-dmc-load.sh --dest "$T/dest" >/dev/null 2>&1; rc=$?
arm "a file that no longer matches its pin is fetched again, and is pushed only when the new copy matches" "$([ $rc = 0 ] && grep -q PUSHED "$T/log" && cmp -s "$T/dest/i915/mtl_dmc.bin" "$T/src/mtl_dmc.bin" && echo 1 || echo 0)"
echo "$fails failed"; [ "$fails" = 0 ]
