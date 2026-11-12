#!/bin/bash
# install-legacy.sh: install the NullMoth NVIDIA driver kit for one older macOS release (10.7 to 14). EXPERIMENTAL.
#
#     sudo sh install-legacy.sh [--run <NVIDIA-Linux-x86_64-620.70.06.run>] [--check]
#
# Each kit is built for exactly one release; this script refuses any other. It runs with the tools every release
# since 10.7 has (bash 3.2, shasum, tar, kextcache or kmutil), so it needs no app.
#
# What it does, in order, and it stops before changing anything if a step up to 3 fails:
#   1. checks the kit's files against SHA256SUMS, the release, and that an NVIDIA card is present
#   2. test-links the kexts against this system's own kernel (kextutil -n -t, or a test kernel collection on 11+)
#   3. unpacks the GPU firmware from NVIDIA's own driver package (the 1401 stick carries it; or --run FILE)
#   4. copies the kexts, the Metal files (10.11 and later) and the firmware into place, keeping a copy of what it replaced
#   5. rebuilds the kernel cache
# It does NOT change boot arguments. The display driver starts with nvfb=1; the accelerator stays off until nvaccel=1
# is added too. That order is deliberate: confirm the desktop on the display driver first.
# --check runs steps 1 and 2 only.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)
# Tests (tools/test_install_legacy.py) run the whole script against a scratch folder with stand-in system tools. Only
# when not root: R is the folder that stands for /, and its bin/ is searched first.
R=""; TESTING=0
if [ "$(id -u)" != 0 ] && [ -n "${NM_LEGACY_TEST_ROOT:-}" ] && [ -d "$NM_LEGACY_TEST_ROOT/bin" ]; then
  R=$NM_LEGACY_TEST_ROOT; TESTING=1; PATH=$R/bin:$PATH
fi
RUN=""; CHECK=0
while [ $# -gt 0 ]; do
  case "$1" in
    --run) RUN="${2:-}"; shift 2;;
    --check) CHECK=1; shift;;
    *) echo "unknown option: $1"; exit 2;;
  esac
done
die() { echo "STOP $*"; echo "RESULT stopped"; exit 1; }
step() { echo; echo "== $*"; }
ok() { echo "   ok  $*"; }

REL=$(tr -cd '0-9.' < "$HERE/RELEASE" 2>/dev/null)
[ -n "$REL" ] || die "this folder is not a complete kit (no RELEASE file)"
v=$(sw_vers -productVersion)
case "$v" in 10.*) HAVE=$(echo "$v" | cut -d. -f1-2);; *) HAVE=${v%%.*};; esac
[ "$HAVE" = "$REL" ] || die "this kit is built for macOS $REL and this is macOS $v; nothing was changed"
[ "$(id -u)" = 0 ] || [ $TESTING = 1 ] || die "run it with sudo"
[ "$(uname -m)" = x86_64 ] || die "the driver is 64-bit only and this kernel is $(uname -m); on 10.7 start the 64-bit kernel (boot argument arch=x86_64)"
case "$REL" in 10.*) MINOR=${REL#10.};; *) MINOR=99;; esac
MAJOR=${REL%%.*}

step "1. kit"
( cd "$HERE" && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1 ) || die "the kit's files do not match SHA256SUMS; copy the kit again"
ok "files match SHA256SUMS"
ioreg -l -w 0 | grep -q '"vendor-id" = <de100000>' || die "no NVIDIA card found on this computer"
ok "NVIDIA card present"
KEXTS=""; for k in NVRM NVRMFB NVRMAGDC NVAccel; do [ -f "$HERE/kexts/$k.kext/Contents/MacOS/$k" ] && KEXTS="$KEXTS $k"; done
for need in NVRM NVRMFB; do case " $KEXTS " in *" $need "*) ;; *) die "the kit lacks the display kext $need";; esac; done
# /Library/Extensions exists from 10.9; before that third-party kexts live with the system's
if [ "$MAJOR" = 10 ] && [ "$MINOR" -lt 9 ]; then EXT=$R/System/Library/Extensions; else EXT=$R/Library/Extensions; fi
# releases that refuse unsigned kexts: 10.10 needs the boot argument, 10.11 and later the protection switched off
if [ "$MAJOR" = 10 ] && [ "$MINOR" = 10 ]; then
  nvram boot-args 2>/dev/null | grep -q 'kext-dev-mode=1' || die "macOS 10.10 loads these kexts only with the boot argument kext-dev-mode=1 (the 1401 stick sets it; start from the stick)"
fi
if [ "$MAJOR" != 10 ] || [ "$MINOR" -ge 11 ]; then
  csrutil status 2>/dev/null | grep -qiE 'disabled|unknown|Kext Signing: disabled' || die "System Integrity Protection is on, so macOS will not load these kexts (the 1401 stick switches it off; start from the stick)"
fi

step "2. link test against this system"
T=$(mktemp -d "${TMPDIR:-/var/tmp}/nullmoth-legacy.XXXXXX") && [ -n "$T" ] || die "cannot create a private work folder"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/repo"
for k in $KEXTS; do cp -R "$HERE/kexts/$k.kext" "$T/repo/" || die "copy $k"; done
own() { [ $TESTING = 1 ] || chown -R root:wheel "$@"; }
own "$T/repo" && chmod -R go-w "$T/repo" || die "kext permissions"
if [ "$MAJOR" = 10 ] && ! command -v kextutil >/dev/null 2>&1; then
  echo "   note: this system has no kextutil (a recovery system?); the link test is skipped"
elif [ "$MAJOR" = 10 ]; then
  for k in $KEXTS; do
    # -n: do not load; -t: full tests; the other kexts of the kit are named as dependencies
    DEPS=""; for d in $KEXTS; do [ "$d" != "$k" ] && DEPS="$DEPS -d $T/repo/$d.kext"; done
    # shellcheck disable=SC2086
    kextutil -n -t $DEPS "$T/repo/$k.kext" >"$T/link-$k.log" 2>&1 || { cat "$T/link-$k.log"; die "this system's linker refused $k.kext (log above); nothing was changed"; }
  done
  ok "this system's linker accepts:$KEXTS"
else
  KB=$R/System/Library/KernelCollections/BootKernelExtensions.kc; KS=$R/System/Library/KernelCollections/SystemKernelExtensions.kc
  KC=$R/Library/KernelCollections/AuxiliaryKernelExtensions.kc
  KARG=""; [ "$MAJOR" -ge 13 ] && KARG="--allow-missing-kdk"
  for x in "$EXT"/*.kext; do [ -d "$x" ] || continue; n=$(basename "$x" .kext); case " $KEXTS " in *" $n "*) ;; *) cp -R "$x" "$T/repo/" || die "stage $n";; esac; done
  own "$T/repo" && chmod -R go-w "$T/repo"
  # shellcheck disable=SC2086
  kmutil create -n aux --volume-root "${R:-/}" $KARG -B $KB -S $KS --repository "$T/repo" -A "$T/aux.kc" -z >"$T/kmutil.log" 2>&1 || { tail -20 "$T/kmutil.log"; die "the test kernel collection could not be built (log above); nothing was changed"; }
  INS=$(kmutil inspect -a x86_64 -A "$T/aux.kc" 2>/dev/null) || die "cannot read the test kernel collection"
  for k in $KEXTS; do printf '%s\n' "$INS" | grep -q "com\.nullmoth\.$k" || { tail -20 "$T/kmutil.log"; die "the kernel collection tool left out $k (log above); nothing was changed"; }; done
  ok "test kernel collection holds:$KEXTS"
fi
[ "$CHECK" = 1 ] && { echo; echo "RESULT check passed, nothing installed"; exit 0; }

step "3. GPU firmware"
FW=$R/Library/NullMoth/nvfw
if [ -z "$RUN" ]; then for c in "$HERE"/NVIDIA-Linux-x86_64-*.run "$HERE"/../NVIDIA-Linux-x86_64-*.run "$R"/Volumes/*/NullMoth/NVIDIA-Linux-x86_64-*.run; do [ -f "$c" ] && { RUN=$c; break; }; done; fi
if [ -n "$RUN" ]; then sh "$HERE/fw_fetch.sh" --dest "$T/fw" --run "$RUN" || die "the GPU firmware could not be prepared; nothing was changed"
else sh "$HERE/fw_fetch.sh" --dest "$T/fw" || die "the GPU firmware could not be prepared (no NVIDIA package found; pass --run FILE); nothing was changed"; fi

step "4. install"
NM=$R/Library/NullMoth
BK=$NM/legacy/backup-$(date +%Y%m%d-%H%M%S); mkdir -p "$BK" || die "cannot create $BK"
for k in $KEXTS; do
  [ -d "$EXT/$k.kext" ] && { mv "$EXT/$k.kext" "$BK/" || die "cannot set aside the old $k.kext"; }
  cp -R "$T/repo/$k.kext" "$EXT/" || die "cannot copy $k.kext (restore from $BK)"
done
mkdir -p "$FW" && cp -R "$T/fw/"* "$FW/" && chmod -R go-w "$FW" || die "cannot place the firmware (restore from $BK)"
if [ -d "$HERE/userland/payload/Library" ]; then
  [ -d "$R/Library/GPUBundles" ] && cp -R "$R/Library/GPUBundles" "$BK/GPUBundles"
  ( cd "$HERE/userland/payload" && tar -cf - Library ) | ( cd "${R:-/}" && tar -xf - ) || die "cannot copy the Metal files (restore from $BK)"
  own "$R/Library/GPUBundles" && chmod -R go-w "$R/Library/GPUBundles"
  ok "Metal files placed"
fi
# the parts that are not switched on by this script stay beside the driver, with their notes
mkdir -p "$NM/legacy/$REL"
for d in opengl options README.txt held-back uninstall-legacy.sh; do [ -e "$HERE/$d" ] && cp -R "$HERE/$d" "$NM/legacy/$REL/"; done
chmod -R go-w "$NM"; chmod 755 "$NM"
printf '%s\n' "${EXT#$R}" > "$NM/legacy/$REL/kext-folder"
[ -f "$HERE/VERSION" ] && tr -cd '0-9.\n' < "$HERE/VERSION" | head -n 1 > "$NM/driver-version"
ok "kexts in $EXT, firmware in $FW"

step "5. kernel cache"
if [ "$MAJOR" = 10 ]; then
  touch "$EXT"
  if [ "$MINOR" -ge 11 ]; then kextcache -i "${R:-/}" >"$T/cache.log" 2>&1 || { tail -5 "$T/cache.log"; die "the kernel cache could not be rebuilt (restore from $BK)"; }
  else kextcache -u "${R:-/}" >"$T/cache.log" 2>&1 || kextcache -system-prelinked-kernel >>"$T/cache.log" 2>&1 || { tail -5 "$T/cache.log"; die "the kernel cache could not be rebuilt (restore from $BK)"; }; fi
else
  [ -f "$KC" ] && cp -p "$KC" "$BK/"
  # shellcheck disable=SC2086
  mkdir -p "$(dirname "$KC")"
  kmutil create -n aux --volume-root "${R:-/}" $KARG -B $KB -S $KS --repository "$EXT" -A "$KC.nullmoth-new" -z >"$T/kmutil2.log" 2>&1 || { tail -20 "$T/kmutil2.log"; die "the kernel collection could not be built (restore from $BK)"; }
  mv -f "$KC.nullmoth-new" "$KC" || die "cannot publish the kernel collection (restore from $BK)"
fi
ok "rebuilt"

echo
echo "Installed for macOS $REL. Restart from the 1401 stick."
echo "The display driver starts with the boot argument nvfb=1 (the stick sets it). Confirm the desktop first."
case " $KEXTS " in *" NVAccel "*) echo "Then, to switch Metal acceleration on:   sudo nvram boot-args=\"\$(nvram boot-args | cut -f2-) nvaccel=1\"   and restart."
  echo "To undo that: choose Reset NVRAM in the boot picker.";; esac
[ -d "$HERE/opengl" ] && echo "The OpenGL accelerator is copied to /Library/NullMoth/legacy/$REL/opengl and is not switched on; its notes say what it needs."
echo "To remove everything:   sudo sh /Library/NullMoth/legacy/$REL/uninstall-legacy.sh"
echo "RESULT installed"
