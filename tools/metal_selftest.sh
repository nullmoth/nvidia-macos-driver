#!/bin/bash
# metal_selftest.sh [-l] [name ...]
#
# Builds the Metal checks in tools/ and runs them against the default Metal device, then prints one table that can be pasted
# into an issue. Nothing is installed and no system file is touched; everything is built in a temporary directory. A check
# that is not in this checkout is skipped. With names, only the checks whose name contains one of them run; -l lists them.
#
# SELFTEST_TOOLS=dir reads the checks from another directory; SELFTEST_MAX=n caps every time limit at n seconds.
# Run it from a normal login session on the GPU you want to test. A broken driver can make a check hang the GPU, so each one
# has a time limit.
set -u
# bench_surface_order loads a plugin by hand when this is set; the checks here must use the installed driver.
unset NM_TEST_PLUGIN
HERE=$(cd "$(dirname "$0")" && pwd)
TOOLS=${SELFTEST_TOOLS:-$HERE}
CC=${CC:-clang}
LIBS="-framework Metal -framework Foundation -framework IOSurface -framework CoreVideo -framework IOKit -framework CoreFoundation -framework VideoToolbox -framework CoreMedia"

# name | source in tools/ | arguments | seconds allowed
CHECKS=(
"surface-order queued|bench_surface_order.m|queued|120"
"surface-order same|bench_surface_order.m|same|120"
"surface-order private|bench_surface_order.m|private|120"
"surface-order cross|bench_surface_order.m|cross|120"
"surface-order alias|bench_surface_order.m|alias|120"
"surface-order alias_read|bench_surface_order.m|alias_read|120"
"surface-order partial|bench_surface_order.m|partial|120"
"surface-order partial_alias|bench_surface_order.m|partial_alias|120"
"surface-order abandoned|bench_surface_order.m|abandoned|120"
"surface-order cpu|bench_surface_order.m|cpu|120"
"surface-order wait|bench_surface_order.m|wait|120"
"surface-wrappers rgba8|bench_surface_wrappers.m||120"
"surface-wrappers nv12|bench_surface_wrappers_nv12.m||120"
"host-copy texture|bench_host_copy_texture.m||120"
"pipeline-build|bench_pipeline_build.m||300"
"vt-transfer 4032x3024 to 1024x768|bench_vt_transfer.m|4032 3024 1024 768 20|300"
"vt-transfer 1920x1080 to 960x540|bench_vt_transfer.m|1920 1080 960 540 20|300"
)

if [ "${1:-}" = "-l" ]; then
  for c in "${CHECKS[@]}"; do
    IFS='|' read -r name src args secs <<<"$c"
    [ -f "$TOOLS/$src" ] && echo "$name" || echo "$name   (not in this checkout)"
  done
  exit 0
fi

TMP=$(mktemp -d /tmp/metal-selftest.XXXXXX) || exit 2
trap 'rm -rf "$TMP"' EXIT

# Runs "$@" with a time limit; returns its exit status, or 124 when the limit was hit.
run_limited() {
  local limit=$1; shift
  "$@" >"$TMP/out" 2>&1 &
  local pid=$! waited=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 1; waited=$((waited + 1))
    if [ "$waited" -ge "$limit" ]; then kill -9 "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; return 124; fi
  done
  wait "$pid"
}

# The device the checks will use.
cat >"$TMP/device.m" <<'OBJC'
#import <Metal/Metal.h>
#import <stdio.h>
int main(void) { @autoreleasepool { id<MTLDevice> d = MTLCreateSystemDefaultDevice(); printf("%s\n", d ? d.name.UTF8String : "(no Metal device)"); } return 0; }
OBJC
gpu="(unknown)"
if $CC -fobjc-arc -o "$TMP/device" "$TMP/device.m" -framework Metal -framework Foundation 2>/dev/null; then
  gpu=$("$TMP/device" 2>/dev/null | tail -1)
fi
drv=/Library/GPUBundles/NVMTLDriver.bundle
if [ -f "$drv/Contents/MacOS/NVMTLDriver" ]; then
  plugin="$(defaults read "$drv/Contents/Info" CFBundleShortVersionString 2>/dev/null || echo '?') ($(shasum -a 256 "$drv/Contents/MacOS/NVMTLDriver" | cut -c1-8))"
else
  plugin="(NVMTLDriver not installed)"
fi
echo "macOS   $(sw_vers -productVersion) ($(sw_vers -buildVersion))   $(uname -m)"
echo "CPU     $(sysctl -n machdep.cpu.brand_string 2>/dev/null)"
echo "Metal   $gpu"
echo "Plugin  $plugin"
echo
echo "| check | result | seconds | detail |"
echo "|---|---|---|---|"

fails=0 ran=0 skipped=0
for c in "${CHECKS[@]}"; do
  IFS='|' read -r name src args secs <<<"$c"
  if [ $# -gt 0 ]; then
    match=0; for want in "$@"; do case "$name" in *"$want"*) match=1;; esac; done
    [ $match -eq 1 ] || continue
  fi
  if [ ! -f "$TOOLS/$src" ]; then echo "| $name | skipped | | not in this checkout |"; skipped=$((skipped + 1)); continue; fi
  bin="$TMP/${src%.m}"
  if [ ! -x "$bin" ] && ! $CC -fobjc-arc -O1 -include unistd.h -o "$bin" "$TOOLS/$src" $LIBS 2>"$TMP/cc.err"; then
    echo "| $name | build failed | | $(grep -m1 error "$TMP/cc.err" | cut -c1-90) |"; fails=$((fails + 1)); continue
  fi
  if [ -n "${SELFTEST_MAX:-}" ] && [ "$secs" -gt "$SELFTEST_MAX" ]; then secs=$SELFTEST_MAX; fi
  start=$(date +%s)
  # shellcheck disable=SC2086
  run_limited "$secs" "$bin" $args
  rc=$?
  took=$(( $(date +%s) - start ))
  ran=$((ran + 1))
  detail=$(grep -E "later avg|FAIL|PASS|wrong|bad" "$TMP/out" | tail -1 | tr '|' '/' | cut -c1-100)
  [ -n "$detail" ] || detail=$(grep -v '^nvrm' "$TMP/out" | tail -1 | tr '|' '/' | cut -c1-100)
  case $rc in
    0)   result=pass ;;
    124) result="TIMEOUT"; fails=$((fails + 1)) ;;
    *)   result="FAIL ($rc)"; fails=$((fails + 1)) ;;
  esac
  echo "| $name | $result | $took | $detail |"
done
echo
echo "$ran ran, $fails failed, $skipped skipped"
[ "$fails" -eq 0 ]
