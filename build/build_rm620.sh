#!/bin/bash
# build_rm620.sh: NVIDIA's RM core 615.71.10 compiled for macOS (libnvkernel.a), the way the 610 archive was made:
# NVIDIA's own Makefile dry-run with TARGET_OS=Darwin, then cc -> clang -mkernel, -DNV_DARWIN, -fcf-protection=branch,
# without -mcmodel=kernel / -fno-pic, for macOS 15.0 like the kext that links it (built for the SDK's macOS instead, every
# object drew a linker note against NVRM's 15.0 target). Four headers learn NV_DARWIN; our OS-timer fix is applied.
# Usage: build_rm620.sh <work dir> <pristine open-gpu-kernel-modules-<version> tree>; the work dir gets ogkm-<version>.
set -uo pipefail
B=$(cd "${1:?work dir}" && pwd); V=${NV_VERSION:-620.70.06}
SRC=${2:?pristine open-gpu-kernel-modules tree}
T=$B/ogkm-$V
JOBS=${JOBS:-6}
if [ ! -d "$T" ]; then cp -R "$SRC" "$T" || exit 2; fi
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
edit("src/nvidia/src/kernel/gpu/timer/timer_ostimer.c",
     "        status = osDestroyNanoTimer(pGpu->pOsGpuInfo, pTmrEvent->super.pOSTmrCBdata);\n        pTmrEvent->super.flags &= ~TMR_FLAG_OS_TIMER_QUEUED;",
     "        status = osDestroyNanoTimer(pGpu->pOsGpuInfo, pTmrEvent->super.pOSTmrCBdata);\n"
     "        // A second destroy of the same event (FSP boot cleanup runs twice on a failed boot) faulted on the stale handle.\n"
     "        pTmrEvent->super.pOSTmrCBdata = NULL;\n        pTmrEvent->super.flags &= ~TMR_FLAG_OS_TIMER_QUEUED;")
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
