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
arm "the pins in the script are the three real files" "$(grep -c '^    [a-z0-9_.]*) *echo [0-9a-f]\{64\};;' package/dmc_fetch.sh | grep -q '^3$' && echo 1 || echo 0)"
echo "$fails failed"; [ "$fails" = 0 ]
