#!/bin/bash
# build_kms620.sh: NVIDIA's modeset core compiled for macOS (libnvkms.a) from the work tree build_rm620.sh made, which
# already carries every patch in kexts/NVRM/patches. Same transform as RM, for macOS 15.0.
# Usage: build_kms620.sh <work dir> (the one build_rm620.sh filled).
set -uo pipefail
B=$(cd "${1:?work dir}" && pwd) || exit 2
JOBS=${JOBS:-6}; V=${NV_VERSION:-620.70.06}
[ "$V" = 620.70.06 ] || { echo "STOP: this recipe is pinned to NVIDIA 620.70.06"; exit 2; }
[ -f "$B/ogkm-$V/.nm-patches" ] || { echo "STOP: $B/ogkm-$V has no record of the patches applied to it; run build_rm620.sh in a new work dir"; exit 2; }
cd "$B/ogkm-$V/src/nvidia-modeset" || exit 2
O=_out/Darwin_x86_64; mkdir -p "$O"
make -B -n TARGET_OS=Darwin TARGET_ARCH=x86_64 NV_VERBOSE=1 > "$B/kms-make-n.txt" 2> "$B/kms-make-n.err" \
  || { echo "STOP: NVIDIA KMS command generation failed"; tail -10 "$B/kms-make-n.err"; exit 3; }
grep " -c " "$B/kms-make-n.txt" | python3 -c '
import sys
for l in sys.stdin:
    l = l.rstrip("\n")
    cc, rest = l.split(" ", 1)
    tool = {"cc": "clang", "c++": "clang++"}[cc]   # the DisplayPort library is C++
    # NVIDIA probes a Linux external return thunk even for TARGET_OS=Darwin.
    # XNU exports no __x86_return_thunk. Use the native compiler return sequence
    # rather than introducing a reference to the Linux kernel interface.
    rest = rest.replace(" -mfunction-return=thunk-extern", "")
    print(tool + " -arch x86_64 -mmacosx-version-min=15.0 -mkernel -DNV_DARWIN -fcf-protection=branch " + rest.replace(" -mcmodel=kernel", "").replace(" -fno-pic", ""))' > "$O/compile_cmds.sh" \
  || { echo "STOP: KMS command transform failed"; exit 3; }
n=$(wc -l < "$O/compile_cmds.sh" | tr -d " "); echo "modeset compile commands: $n"
[ "$n" -gt 0 ] || { echo "STOP: no KMS compile commands generated"; exit 3; }
# the identity string is a make step (user@host and the date); write the shipped wording instead
printf '%s\n' 'const char  NV_KMS_ID[] = "nvidia id: NVIDIA UNIX Open Kernel Mode Setting Driver for x86_64 '"$V"' Release Build (NullMoth Systems)";' \
  'const char *const pNV_KMS_ID =  NV_KMS_ID + 11;' > "$O/g_nvid_string.c"
rm -rf "$O/.err" "$B/kcmds"; mkdir -p "$O/.err" "$B/kcmds"
awk -v d="$B/kcmds" '{ f = d "/" NR ".sh"; print > f; close(f) }' "$O/compile_cmds.sh"
seq 1 "$n" | xargs -P "$JOBS" -I{} bash -c 'bash "$1/kcmds/$2.sh" 2> "$3/.err/$2.txt" || echo "FAIL $2"' _ "$B" '{}' "$O" > "$B/kfails.txt" \
  || { echo "STOP: KMS compile workers failed"; exit 5; }
nf=$(grep -c FAIL "$B/kfails.txt"); echo "failed: $nf of $n"
[ "$nf" = 0 ] || { for i in $(grep FAIL "$B/kfails.txt" | head -3 | awk '{print $2}'); do grep -oE " -c [^ ]+" "$B/kcmds/$i.sh"; grep -m3 "error:" "$O/.err/$i.txt"; done; exit 5; }
rm -f "$O/libnvkms.a"; ls "$O"/*.o > "$B/kobjs.txt"
xcrun libtool -static -o "$O/libnvkms.a" -filelist "$B/kobjs.txt" 2>"$B/klibtool.err" || { tail -5 "$B/klibtool.err"; exit 4; }
echo "libnvkms.a $(stat -f%z "$O/libnvkms.a") B from $(wc -l < "$B/kobjs.txt" | tr -d " ") objects"
undefined=$(xcrun nm -u "$O/libnvkms.a" 2> "$B/kms-imports.err") || exit 5
case "$undefined" in *___x86_return_thunk*) echo "STOP: KMS still imports a Linux return thunk"; exit 5;; esac
local_names=(-iFw -e "$(id -un)" -e "$(hostname -s)")
left=$(strings -a "$O/libnvkms.a" | grep -c "${local_names[@]}" || true)   # the building account or host
[ "$left" = 0 ] || { echo "STOP: $left personal string(s) in libnvkms.a"; strings -a "$O/libnvkms.a" | grep "${local_names[@]}" | head -3; exit 6; }
echo "personal strings in libnvkms.a: 0"
shasum -a 256 "$O/libnvkms.a" > "$B/ogkm-$V/.nm-kms-archive.sha256"
