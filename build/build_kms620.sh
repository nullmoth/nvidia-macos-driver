#!/bin/bash
# build_kms620.sh: NVIDIA's modeset core compiled for macOS (libnvkms.a) from the work tree build_rm620.sh made, which
# already carries every patch in kexts/NVRM/patches. Same transform as RM, for macOS 15.0.
# Usage: build_kms620.sh <work dir> (the one build_rm620.sh filled).
set -uo pipefail
B=$(cd "${1:?work dir}" && pwd); JOBS=${JOBS:-6}; V=${NV_VERSION:-620.70.06}
[ -f "$B/ogkm-$V/.nm-patches" ] || { echo "STOP: $B/ogkm-$V has no record of the patches applied to it; run build_rm620.sh in a new work dir"; exit 2; }
cd "$B/ogkm-$V/src/nvidia-modeset" || exit 2
O=_out/Darwin_x86_64; mkdir -p "$O"
make -n TARGET_OS=Darwin TARGET_ARCH=x86_64 NV_VERBOSE=1 2>/dev/null > "$B/kms-make-n.txt"
grep " -c " "$B/kms-make-n.txt" | python3 -c '
import sys
for l in sys.stdin:
    l = l.rstrip("\n")
    cc, rest = l.split(" ", 1)
    tool = {"cc": "clang", "c++": "clang++"}[cc]   # the DisplayPort library is C++
    print(tool + " -arch x86_64 -mmacosx-version-min=15.0 -mkernel -DNV_DARWIN -fcf-protection=branch " + rest.replace(" -mcmodel=kernel", "").replace(" -fno-pic", ""))' > "$O/compile_cmds.sh"
n=$(wc -l < "$O/compile_cmds.sh" | tr -d " "); echo "modeset compile commands: $n"
# the identity string is a make step (user@host and the date); write the shipped wording instead
printf '%s\n' 'const char  NV_KMS_ID[] = "nvidia id: NVIDIA UNIX Open Kernel Mode Setting Driver for x86_64 '"$V"' Release Build (NullMoth Systems)";' \
  'const char *const pNV_KMS_ID =  NV_KMS_ID + 11;' > "$O/g_nvid_string.c"
rm -rf "$O/.err" "$B/kcmds"; mkdir -p "$O/.err" "$B/kcmds"
awk -v d="$B/kcmds" '{ f = d "/" NR ".sh"; print > f; close(f) }' "$O/compile_cmds.sh"
seq 1 "$n" | xargs -P "$JOBS" -I{} bash -c 'bash "'"$B"'/kcmds/{}.sh" 2> "'"$O"'/.err/{}.txt" || echo "FAIL {}"' > "$B/kfails.txt"
nf=$(grep -c FAIL "$B/kfails.txt"); echo "failed: $nf of $n"
[ "$nf" = 0 ] || { for i in $(grep FAIL "$B/kfails.txt" | head -3 | awk '{print $2}'); do grep -oE " -c [^ ]+" "$B/kcmds/$i.sh"; grep -m3 "error:" "$O/.err/$i.txt"; done; exit 5; }
rm -f "$O/libnvkms.a"; ls "$O"/*.o > "$B/kobjs.txt"
xcrun libtool -static -o "$O/libnvkms.a" -filelist "$B/kobjs.txt" 2>"$B/klibtool.err" || { tail -5 "$B/klibtool.err"; exit 4; }
echo "libnvkms.a $(stat -f%z "$O/libnvkms.a") B from $(wc -l < "$B/kobjs.txt" | tr -d " ") objects"
left=$(strings -a "$O/libnvkms.a" | grep -ciE "$(id -un)|$(hostname -s)" || true)   # the building account or host
[ "$left" = 0 ] || { echo "STOP: $left personal string(s) in libnvkms.a"; strings -a "$O/libnvkms.a" | grep -iE "$(id -un)|$(hostname -s)" | head -3; exit 6; }
echo "personal strings in libnvkms.a: 0"
