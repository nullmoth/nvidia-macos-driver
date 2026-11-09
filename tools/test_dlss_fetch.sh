#!/bin/bash
# Arms for package/dlss_fetch.sh, run unprivileged against local fixtures (file:// URLs, a stub dlss_cache, a fake
# install root). Nothing is downloaded and nothing outside a temp folder is touched.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT=$ROOT/package/dlss_fetch.sh
[ "$(id -u)" != 0 ] || { echo "run as a normal user"; exit 2; }
W=$(mktemp -d /tmp/test-dlss-fetch.XXXXXX); trap 'rm -rf "$W"' EXIT
fails=0
arm() { if [ "$2" = 0 ]; then echo "ok   $1"; else echo "FAIL $1${3:+ ($3)}"; fails=$((fails + 1)); fi; }
sha() { shasum -a 256 "$1" | awk '{print $1}'; }
shastr() { printf '%s\n' "$1" | shasum -a 256 | awk '{print $1}'; }
# t <command...>: prints 0 when the command succeeds, 1 otherwise (for use inside arm)
t() { if "$@"; then echo 0; else echo 1; fi; }

F=$W/files; mkdir -p "$F" "$W/tools"
printf 'dlss runtime\n' > "$F/dlss.so"; printf 'dlssd runtime\n' > "$F/dlssd.so"; printf 'dlssg runtime\n' > "$F/dlssg.so"
# stand-in for NVIDIA's makeself .run: a shell header with skip=N, then a zstd-compressed tar (as the real one is)
ZSTD=${ZSTD:-$(command -v zstd || echo /opt/homebrew/bin/zstd)}   # only the fixture needs the compressor; the script under test never does
[ -x "$ZSTD" ] || { echo "test needs a zstd command (set ZSTD=) to build its fixture"; exit 2; }
mkdir -p "$W/payload" && printf 'ngx core\n' > "$W/payload/libnvidia-ngx.so.620.70.06" && printf 'other\n' > "$W/payload/other.bin"
{ printf '#!/bin/sh\n# makeself stand-in\nCRCsum=0\nskip=6\n'; printf 'exit 1\n'; tar -C "$W/payload" -cf - . | "$ZSTD" -q -c; } > "$F/driver.run"
clang -O1 -DZSTD_DISABLE_ASM -o "$W/tools/nm_unzstd" "$ROOT/dlss/nm_unzstd.c" "$ROOT"/dlss/zstd/common/*.c "$ROOT"/dlss/zstd/decompress/*.c || { echo "cannot build nm_unzstd"; exit 2; }
python3 -I - "$F" <<'PY'
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1] + "/nvjitlink.whl", "w")
z.writestr("nvidia/nvjitlink/lib/libnvJitLink.so.12", b"jitlink\n")
z.close()
PY
cat > "$W/tools/dlss_cache" <<'STUB'
#!/bin/bash
# stub: dlss_cache <jitlink> <sm> <outdir> <trap> <runtime>...
[ -f "$1" ] && [ -f "$4" ] && [ -f "$5" ] || exit 2
[ "${STUB_FAIL:-0}" = 0 ] || exit 1
printf 'cubin\n' > "$3/0123abcd.sm$2.cubin"; echo "stub: built"
STUB
chmod +x "$W/tools/dlss_cache"; echo '.version 8.8' > "$W/tools/assertfail_trap.ptx"

mkpins() { # mkpins <file> <root> [dlss-sha-override]
  mkdir -p "$(dirname "$2")"
  cat > "$1" <<P
URL_DLSS=file://$F/dlss.so; SHA_DLSS=${3:-$(sha "$F/dlss.so")}
URL_DLSSD=file://$F/dlssd.so; SHA_DLSSD=$(sha "$F/dlssd.so")
URL_DLSSG=file://$F/dlssg.so; SHA_DLSSG=$(sha "$F/dlssg.so")
URL_RUN=file://$F/driver.run; SHA_RUN=$(sha "$F/driver.run")
SHA_NGX=$(shastr 'ngx core')
URL_WHEEL=file://$F/nvjitlink.whl; SHA_WHEEL=$(sha "$F/nvjitlink.whl")
SHA_JITLINK=$(shastr 'jitlink')
DLSS_ROOT=$2; CURL_PROTO='=file'; DLSS_TOOLS=$W/tools
P
}
run() { # run <pins> [args]: stdin is not a terminal
  local p=$1; shift
  DLSS_TEST_PINS=$p bash "$SCRIPT" "$@" </dev/null 2>&1
}
good() { [ $rc = 0 ] && [ -f "$R1/ngx/libnvidia-ngx.so.620.70.06" ] && [ -f "$R1/snippets/libnvidia-ngx-dlss.so.310.9.16" ] &&
  [ -f "$R1/snippets/libnvidia-ngx-dlssd.so.310.9.16" ] && [ -f "$R1/snippets/libnvidia-ngx-dlssg.so.310.9.16" ] && [ -f "$R1/kernels/0123abcd.sm120.cubin" ] &&
  grep -q '^sm: 120$' "$R1/manifest.txt"; }
perms() { [ "$(stat -f %Lp "$R1/kernels/0123abcd.sm120.cubin")" = 644 ] && [ -z "$(find "$R1" \( -perm -g+w -o -perm -o+w \) -print -quit)" ]; }
nostage() { [ -z "$(find "$R1" \( -name '.new.*' -o -name '*.old' \) -print -quit)" ]; }

R1=$W/r1/dlss; mkpins "$W/p1" "$R1"
out=$(run "$W/p1" --yes --sm 120); rc=$?
arm "a good download installs ngx, snippets, kernels and a manifest" "$(t good)" "$rc"
arm "installed kernels are 0644 and nothing is group or other writable" "$(t perms)"
arm "no staging folder or temp file is left behind" "$(t nostage)"

ino=$(stat -f %i "$R1/manifest.txt")
out=$(run "$W/p1" --yes --sm 120); rc=$?
again() { [ $rc = 0 ] && echo "$out" | grep -q 'already installed' && [ "$(stat -f %i "$R1/manifest.txt")" = "$ino" ]; }
arm "a second run with nothing changed does nothing" "$(t again)"
out=$(run "$W/p1" --yes --sm 89); rc=$?
other() { [ $rc = 0 ] && [ -f "$R1/kernels/0123abcd.sm89.cubin" ] && [ -f "$R1/kernels/0123abcd.sm120.cubin" ]; }
arm "another card's SM rebuilds and keeps the earlier SM's kernels" "$(t other)" "$rc"

R2=$W/r2/dlss; mkpins "$W/p2" "$R2" 0000000000000000000000000000000000000000000000000000000000000000
out=$(run "$W/p2" --yes --sm 120); rc=$?
badhash() { [ $rc != 0 ] && echo "$out" | grep -q 'does not match its pinned SHA-256' && [ ! -e "$R2/ngx" ] && [ ! -e "$R2/manifest.txt" ]; }
arm "NEGATIVE: a download with the wrong SHA-256 is refused and nothing is installed" "$(t badhash)" "rc=$rc"

R3=$W/r3/dlss; mkpins "$W/p3" "$R3"; sed -i '' "s/^SHA_NGX=.*/SHA_NGX=$(shastr x)/" "$W/p3"
out=$(run "$W/p3" --yes --sm 120); rc=$?
badngx() { [ $rc != 0 ] && echo "$out" | grep -q 'libnvidia-ngx.so.620.70.06 does not match' && [ ! -e "$R3/manifest.txt" ]; }
arm "NEGATIVE: a wrong file inside the NVIDIA package is refused" "$(t badngx)" "rc=$rc"

R4=$W/r4/dlss; mkpins "$W/p4" "$R4"; sed -i '' "s/^SHA_JITLINK=.*/SHA_JITLINK=$(shastr x)/" "$W/p4"
out=$(run "$W/p4" --yes --sm 120); rc=$?
badjl() { [ $rc != 0 ] && echo "$out" | grep -q 'libnvJitLink.so.12 does not match' && [ ! -e "$R4/manifest.txt" ]; }
arm "NEGATIVE: a wrong nvJitLink inside the wheel is refused" "$(t badjl)" "rc=$rc"

before=$(cat "$R1/manifest.txt")
out=$(STUB_FAIL=1 run "$W/p1" --yes --sm 120 --force); rc=$?
failbuild() { [ $rc != 0 ] && [ "$(cat "$R1/manifest.txt")" = "$before" ] && [ -f "$R1/kernels/0123abcd.sm120.cubin" ] && nostage; }
arm "NEGATIVE: a failing kernel build leaves the earlier install untouched" "$(t failbuild)" "rc=$rc"

R5=$W/r5/dlss; mkpins "$W/p5" "$R5"
out=$(run "$W/p5" --sm 120); rc=$?
optin() { [ $rc = 0 ] && [ ! -e "$R5" ] && echo "$out" | grep -q 'not requested'; }
arm "NEGATIVE: without --yes and without a terminal nothing is downloaded" "$(t optin)" "$rc"

R6=$W/r6/dlss; mkpins "$W/p6" "$R6"
for pair in 2D05:120 2684:89 2503:86 1F08:75 21C4:75 2B85:120; do
  id=${pair%:*}; want=${pair#*:}
  rm -rf "$R6"; { cat "$W/p6"; echo "DEVICE_IDS=$id"; } > "$W/p6x"
  out=$(run "$W/p6x" --yes); rc=$?
  card() { [ $rc = 0 ] && grep -q "^sm: $want\$" "$R6/manifest.txt"; }
  arm "card $id builds for sm_$want" "$(t card)" "rc=$rc"
done
rm -rf "$R6"; { cat "$W/p6"; echo "DEVICE_IDS=2E13"; } > "$W/p6x"
out=$(run "$W/p6x" --yes); rc=$?
unk() { [ $rc != 0 ] && echo "$out" | grep -q 'pass --sm' && [ ! -e "$R6" ]; }
arm "NEGATIVE: an unclassified card is not guessed" "$(t unk)" "rc=$rc"

grep -q 'dlss_fetch.sh' "$ROOT/package/install.sh"; arm "install.sh offers the DLSS step" $?
grep -q '/Library/NullMoth/dlss' "$ROOT/package/uninstall.sh"; arm "uninstall.sh removes /Library/NullMoth/dlss" $?
echo "$fails failed"; [ "$fails" = 0 ]
