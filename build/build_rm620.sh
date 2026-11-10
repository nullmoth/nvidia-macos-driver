#!/bin/bash
# build_rm620.sh: NVIDIA's RM core 615.71.10 compiled for macOS (libnvkernel.a), the way the 610 archive was made:
# NVIDIA's own Makefile dry-run with TARGET_OS=Darwin, then cc -> clang -mkernel, -DNV_DARWIN, -fcf-protection=branch,
# without -mcmodel=kernel / -fno-pic, for macOS 15.0 like the kext that links it (built for the SDK's macOS instead, every
# object drew a linker note against NVRM's 15.0 target). Four headers learn NV_DARWIN; the patch chain is applied first.
# Usage: build_rm620.sh <work dir> <pristine open-gpu-kernel-modules-<version> tree>; the work dir gets ogkm-<version>.
set -uo pipefail
B=$(cd "${1:?work dir}" && pwd); V=${NV_VERSION:-620.70.06}
SRC=${2:?pristine open-gpu-kernel-modules tree}
T=$B/ogkm-$V
JOBS=${JOBS:-6}
HERE=$(cd "$(dirname "$0")/.." && pwd)
# The work tree is the pristine tree plus every patch in kexts/NVRM/patches, applied here, in file-name order, with no
# fuzz. They used to be applied by hand: the tree the archives were built from had five of the seven, and the two newest
# (hardware YUV 4:2:0, the colour-range control) were in the repository but in no binary. The names applied are recorded in
# the tree; a tree without that record was patched by hand and is refused.
if [ ! -d "$T" ]; then
  cp -R "$SRC" "$T" || exit 2
  : > "$T/.nm-patches"
  for f in "$HERE"/kexts/NVRM/patches/*.patch; do
    ok=0
    for root in "" "src/nvidia-modeset"; do
      ( cd "$T/$root" && patch -p1 --fuzz=0 --dry-run < "$f" >/dev/null 2>&1 && patch -p1 --fuzz=0 --no-backup-if-mismatch < "$f" >/dev/null ) && { ok=1; break; }
    done
    [ $ok = 1 ] || { echo "STOP: $(basename "$f") does not apply to $SRC"; rm -rf "$T"; exit 2; }
    basename "$f" >> "$T/.nm-patches"
  done
  find "$T/src" -name '*.orig' -delete
fi
[ -f "$T/.nm-patches" ] || { echo "STOP: $T has no record of the patches applied to it (patched by hand?); use a new work dir"; exit 2; }
want=$(cd "$HERE/kexts/NVRM/patches" && ls *.patch | LC_ALL=C sort | tr '\n' ' ')
have=$(LC_ALL=C sort "$T/.nm-patches" | tr '\n' ' ')
[ "$want" = "$have" ] || { echo "STOP: $T was built with [$have], this tree has [$want]; use a new work dir"; exit 2; }
echo "patch chain applied: $have"
python3 - "$T" <<'PY' || exit 3
import os, sys
t = sys.argv[1]
def edit(rel, old, new):
    p = os.path.join(t, rel); s = open(p).read()
    if new in s: return
    assert s.count(old) == 1, f"{rel}: anchor found {s.count(old)} times"
    s = s.replace(old, new); open(p + ".tmp", "w").write(s); os.replace(p + ".tmp", p)
edit("src/common/sdk/nvidia/inc/cpuopsys.h", "defined(__INTEGRITY) ) && !defined(nvmacosx)", "defined(__INTEGRITY) || defined(NV_DARWIN) ) && !defined(nvmacosx)")
for rel in ("src/common/inc/nvVer.h", "src/common/inc/nvUnixVersion.h"):
    edit(rel, "defined(NV_QNX) || defined(NV_INTEGRITY) || \\", "defined(NV_QNX) || defined(NV_INTEGRITY) || defined(NV_DARWIN) || \\")
edit("src/common/inc/nvBldVer.h", "#if defined(NV_LINUX) || defined(NV_BSD) || defined(NV_SUNOS)\n", "#if defined(NV_LINUX) || defined(NV_BSD) || defined(NV_SUNOS) || defined(NV_DARWIN)\n")
# the OS-timer fix comes from the patch chain (610-fsp-timer-double-destroy.patch); it must be there
assert "pTmrEvent->super.pOSTmrCBdata = NULL;" in open(os.path.join(t, "src/nvidia/src/kernel/gpu/timer/timer_ostimer.c")).read(), "the OS-timer fix is missing"
print("source edits applied")
PY
cd "$T/src/nvidia" || exit 2
O=_out/Darwin_x86_64; mkdir -p "$O"
make -n TARGET_OS=Darwin TARGET_ARCH=x86_64 NV_VERBOSE=1 2>/dev/null > "$B/make-n.txt"
# the generated version header and string file are make steps, not compiles: run those first
grep -E "^echo .*version\.h" "$B/make-n.txt" > "$B/pre.sh"; bash "$B/pre.sh"
# NVIDIA's rule bakes user@host and the date into the identity string; the shipped string names no machine or person.
printf '%s\n' 'const char  NVRM_ID[] = "nvidia id: NVIDIA UNIX Open Kernel Module for x86_64 '"$V"' Release Build (NullMoth Systems)";' \
  'const char *const pNVRM_ID =  NVRM_ID + 11;' > "$O/g_nvid_string.c"
grep " -c " "$B/make-n.txt" | python3 -c '
import sys
for l in sys.stdin:
    l = l.rstrip("\n")
    assert l.startswith("cc "), l[:40]
    l = "clang -arch x86_64 -mmacosx-version-min=15.0 -mkernel -DNV_DARWIN -fcf-protection=branch " + l[3:]
    l = l.replace(" -mcmodel=kernel", "").replace(" -fno-pic", "")
    print(l)' > "$O/compile_cmds.sh"
n=$(wc -l < "$O/compile_cmds.sh"); echo "compile commands: $n"
rm -rf "$O/.err"; mkdir -p "$O/.err"
rm -rf "$B/cmds"; mkdir -p "$B/cmds"; awk -v d="$B/cmds" '{ f = d "/" NR ".sh"; print > f; close(f) }' "$O/compile_cmds.sh"
seq 1 "$n" | xargs -P "$JOBS" -I{} bash -c 'bash "'"$B"'/cmds/{}.sh" 2> "'"$O"'/.err/{}.txt" || echo "FAIL {}"' > "$B/fails.txt"
nf=$(grep -c FAIL "$B/fails.txt"); echo "failed: $nf of $n"
[ "$nf" = 0 ] || { head -5 "$B/fails.txt"; for i in $(grep FAIL "$B/fails.txt" | head -3 | awk '{print $2}'); do sed -n "${i}p" "$O/compile_cmds.sh" | grep -oE " -c [^ ]+"; grep -m3 "error:" "$O/.err/$i.txt"; done; }
rm -f "$O/libnvkernel.a"
ls "$O"/*.o > "$B/objs.txt"; xcrun libtool -static -o "$O/libnvkernel.a" -filelist "$B/objs.txt" 2>"$B/libtool.err" || { tail -5 "$B/libtool.err"; exit 4; }
echo "libnvkernel.a $(stat -f%z "$O/libnvkernel.a") B from $(wc -l < "$B/objs.txt") objects"
left=$(strings -a "$O/libnvkernel.a" | grep -ciE "$(id -un)|$(hostname -s)" || true)   # the building account or host
[ "$left" = 0 ] || { echo "STOP: $left personal string(s) in libnvkernel.a"; strings -a "$O/libnvkernel.a" | grep -iE "$(id -un)|$(hostname -s)" | head -3; exit 6; }
echo "personal strings in libnvkernel.a: 0"
