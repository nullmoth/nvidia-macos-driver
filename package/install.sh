#!/bin/bash
set -u
# runs as root: tools resolve through a fixed PATH, never one the caller exported
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)
if [ "${1:-}" = --payload ]; then
  [ "$#" = 2 ] && [ -d "$2" ] || { echo "STOP: --payload requires an existing package directory" >&2; exit 2; }
  HERE=$(cd "$2" && pwd) || exit 2
elif [ "$#" != 0 ]; then echo "STOP: expected --payload directory or no arguments" >&2; exit 2; fi
KEXTS="NVRM NVAccel NVRMFB NVRMAGDC"
EXT=/Library/Extensions; GB=/Library/GPUBundles; FW=/Library/NullMoth/nvfw; FWOLD=/Users/Shared/nvfw
KC=/Library/KernelCollections/AuxiliaryKernelExtensions.kc
K=/System/Library/Kernels/kernel
# macOS 13+ kmutil refuses to build any kernel collection without a Kernel Debug Kit matching the build unless it is told
# --allow-missing-kdk. The auxiliary collection only links against the boot and system collections already on disk.
KARG=(--allow-missing-kdk); [ -f "$K" ] && KARG+=(--kernel "$K")
KB=/System/Library/KernelCollections/BootKernelExtensions.kc
KS=/System/Library/KernelCollections/SystemKernelExtensions.kc
BK=""; LOCKDIR=/Library/NullMoth/install.lock; LOCK_HELD=0; LOCK_RELEASE_OK=1; LOCK_ID=""
INSTALLING=0; NEWKC=""; STAGE=""
release_install_lock() {
  if [ "$LOCK_HELD" = 1 ] && [ "$LOCK_RELEASE_OK" = 1 ]; then
    if [ -z "$LOCK_ID" ] || [ -L "$LOCKDIR" ] || [ ! -d "$LOCKDIR" ] ||
       [ "$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null)" != "$LOCK_ID" ]; then
      echo "   NOTE: installer guard identity changed; its current path is retained for review." >&2
      return
    fi
    rm -f "$LOCKDIR/owner" "$LOCKDIR/installing"
    rmdir "$LOCKDIR" || echo "   NOTE: installer lock could not be released; review it before another install." >&2
    LOCK_HELD=0
  elif [ "$LOCK_HELD" = 1 ]; then
    echo "   NOTE: installer guard retained because restoration was incomplete; recovery review is required." >&2
  fi
}

rollback_install() {
  local failed=0 k b restore_kexts="$KEXTS"
  # A later process recovering a stale guard has not detected PRIMARY yet.
  if [ -f "$BK/primary-selector-used" ]; then
    case " $restore_kexts " in *' NVPrimarySelector '*) ;; *) restore_kexts="$restore_kexts NVPrimarySelector";; esac
  fi
  [ -z "$NEWKC" ] || rm -f "$NEWKC" || failed=1
  for k in $restore_kexts; do
    rm -rf "$EXT/$k.kext" || failed=1
    [ ! -e "$BK/$k.kext" ] || ditto "$BK/$k.kext" "$EXT/$k.kext" || failed=1
  done
  for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do
    rm -rf "$GB/$b" || failed=1
    [ ! -e "$BK/$b" ] || ditto "$BK/$b" "$GB/$b" || failed=1
  done
  rm -rf /Library/NullMoth/kexts "$FW" || failed=1
  [ ! -d "$BK/kexts" ] || ditto "$BK/kexts" /Library/NullMoth/kexts || failed=1
  # Restore the firmware to both locations: a driver older than this one looks for it in /Users/Shared,
  # and root writing that directory is harmless (root-owned, inside a sticky parent).
  [ ! -d "$BK/nvfw" ] || ditto "$BK/nvfw" "$FW" || failed=1
  [ ! -d "$BK/nvfw" ] || { mkdir -p "$FWOLD" && ditto "$BK/nvfw" "$FWOLD"; } || failed=1
  if [ -f "$BK/AuxiliaryKernelExtensions.kc" ]; then cp -p "$BK/AuxiliaryKernelExtensions.kc" "$KC" || failed=1
  else rm -f "$KC" || failed=1; fi
  if [ -f "$BK/os-major" ]; then cp -p "$BK/os-major" /Library/NullMoth/os-major || failed=1
  else rm -f /Library/NullMoth/os-major || failed=1; fi
  # the version record follows the files it describes, or Send logs names a version that is not installed
  if [ -f "$BK/driver-version" ]; then cp -p "$BK/driver-version" /Library/NullMoth/driver-version || failed=1
  else rm -f /Library/NullMoth/driver-version || failed=1; fi
  return "$failed"
}
step() { echo; echo "== $*"; }; ok() { echo "   ok  $*"; }
boot_sec() { sysctl -n kern.boottime 2>/dev/null | sed -E 's/.*sec = ([0-9]+),.*/\1/'; }
# kern.boottime is derived from the clock and moves when the clock is corrected after boot, so "the same boot" allows
# 60 s of drift (review: an exact match could take a live install for a dead one and roll its files back mid-install).
same_boot() { local a=$1 b; b=$(boot_sec); [ -n "$a" ] && [ -n "$b" ] && [ $((a > b ? a - b : b - a)) -le 60 ]; }
# the pid is still the installer (pid numbers are reused): its command line names this script
is_installer_pid() { ps -o command= -p "$1" 2>/dev/null | grep -Eq '(nullmoth-install|/install)\.sh'; }
# A guard left by an install that was killed outright (power loss, a forced quit, a restart mid-install) stopped every
# later install with "installer guard already exists" until someone removed it by hand (3 reports and chat, 10-12).
# The guard now records its owner (pid, boot) and, once files start changing, the backup it can be restored from.
# A guard whose owner is gone is recovered: rolled back from that backup if it had started changing files, then cleared.
# A live install, or one whose own restore failed (restore-incomplete), still stops.
recover_stale_guard() {
  local g=$LOCKDIR o=$LOCKDIR/owner pid="" boot="" bk=""
  [ -d "$g" ] && [ ! -L "$g" ] && [ "$(stat -f '%u' "$g" 2>/dev/null)" = 0 ] || return 0
  [ ! -e "$g/restore-incomplete" ] || die "the last install could not restore the previous driver (its backup is kept in /Library/NullMoth); use Remove NVIDIA driver, then install again, or send logs (no driver changes made)"
  if [ -f "$o" ] && [ ! -L "$o" ]; then
    pid=$(sed -n 's/^pid=\([0-9][0-9]*\)$/\1/p' "$o" | head -1); boot=$(sed -n 's/^boot=\([0-9][0-9]*\)$/\1/p' "$o" | head -1)
    bk=$(sed -n 's/^bk=\(.*\)$/\1/p' "$o" | head -1)
    if [ -n "$pid" ] && same_boot "$boot" && kill -0 "$pid" 2>/dev/null && is_installer_pid "$pid"; then return 0; fi
  else
    # a guard from 1.14.7 or earlier has no owner record: an install cannot outlive a restart, so one older than this boot is stale
    [ "$(stat -f '%m' "$g" 2>/dev/null || echo 0)" -lt $(( $(boot_sec) - 60 )) ] || return 0
  fi
  echo "   NOTE: an earlier install was interrupted and left its guard behind; recovering it"
  if [ -e "$g/installing" ]; then
    case "$bk" in /Library/NullMoth/backup-*) ;; *) die "an interrupted install had changed driver files and its backup is not recorded; use Remove NVIDIA driver, then install again (no driver changes made)";; esac
    [ -d "$bk" ] && [ ! -L "$bk" ] || die "the interrupted install's backup $bk is missing; use Remove NVIDIA driver, then install again (no driver changes made)"
    BK=$bk
    if ! rollback_install; then : > "$g/restore-incomplete"; die "restoring the interrupted install failed; its backup is kept at $bk"; fi
    echo "   ok  the interrupted install was rolled back from $bk"; BK=""
  fi
  rm -f "$o" "$g/installing"; rmdir "$g" 2>/dev/null || die "could not clear the interrupted install's guard"
}
die() {
  echo "   STOP: $*" >&2
  if [ "$INSTALLING" = 1 ]; then
    if rollback_install; then echo "   Previous installation restored; backup retained at $BK." >&2
    else LOCK_RELEASE_OK=0; : > "$LOCKDIR/restore-incomplete" 2>/dev/null; echo "   Restore incomplete; recovery files remain at $BK." >&2; fi
  fi
  exit 1
}

# Keep diagnostic output bounded while retaining the copy command's exit status.
backup_copy() {
  local component=$1; shift
  "$@" 2>&1 | tail -c 4096 > "$T/backup-error.log"
  local status=("${PIPESTATUS[@]}")
  if [ "${status[0]}" != 0 ] || [ "${status[1]}" != 0 ]; then
    printf '   BACKUP-DIAGNOSTIC component=%s command-status=%s capture-status=%s stderr-tail-limit=4096\n' "$component" "${status[0]}" "${status[1]}" >&2
    [ ! -f "$T/backup-error.log" ] || cat "$T/backup-error.log" >&2
    echo >&2
    die "back up $component"
  fi
}
finish_install() { [ -z "$STAGE" ] || rm -rf "$STAGE"; release_install_lock; }
trap finish_install EXIT
trap 'die "installer interrupted (signal HUP)"' HUP
trap 'die "installer interrupted (signal INT)"' INT
trap 'die "installer interrupted (signal TERM)"' TERM

[ "$(id -u)" -eq 0 ] || die "run with sudo"
step "1. this Mac"
[ "$(uname -m)" = x86_64 ] || die "Intel/x86_64 only"
v=$(sw_vers -productVersion); MAJ=${v%%.*}
BUILD=$(sw_vers -buildVersion)
printf '%s\n' "$BUILD" | grep -Eq '^[0-9]+[A-Za-z][A-Za-z0-9]+$' || die "cannot identify the current macOS build"
case $MAJ in 15|26) ;; *) die "This driver is built for macOS 15 and 26 (this is $v); no changes made";; esac
# NVAccel is the one kext built per macOS: Tahoe made the IOAcceleratorFamily2 methods it inherits private (see
# kexts/NVRM/accel/gen_tahoe_fwd.py). The other three kexts are the same binaries on 15 and 26.
VAR="$HERE/Library/NullMoth/kexts/$MAJ"; [ -d "$VAR/NVAccel.kext" ] || die "this package has no NVAccel for macOS $MAJ"
kpath() {
  case "$1" in
    NVAccel) echo "$VAR/NVAccel.kext";;
    NVPrimarySelector) echo "$SELECTOR_SOURCE";;
    *) echo "$HERE/Library/Extensions/$1.kext";;
  esac
}
ioreg -r -c IOPCIDevice -d 1 | grep -q '"vendor-id" = <de100000>' || die "no NVIDIA GPU found on PCI"
# The driver runs the cards NVIDIA's GSP firmware supports (Turing and later). The same list the Windows builder uses
# (open-gpu-kernel-modules 615.78.08, p1401/nvidia_gsp_ids.json). Installing it for an older card (GTX 10 and before)
# would switch the firmware screen off and leave nothing to draw: those PCs run macOS in basic display instead.
SUPPORTED_IDS="
1E02 1E04 1E07 1E30 1E36 1E78 1E81 1E82 1E84 1E87 1E89 1E90 1E91 1E93 1EB0 1EB1 1EB5 1EB6 1EC2 1EC7 1ED0 1ED1 1ED3
1EF5 1F02 1F03 1F06 1F07 1F08 1F0A 1F10 1F11 1F12 1F14 1F15 1F36 1F42 1F47 1F50 1F51 1F54 1F55 1F76 1F82 1F83 1F91
1F95 1F96 1F97 1F98 1F99 1F9C 1F9D 1F9F 1FA0 1FB0 1FB1 1FB2 1FB6 1FB7 1FB8 1FB9 1FBA 1FBB 1FBC 1FDD 1FF0 1FF2 1FF9
2182 2184 2187 2188 2191 2192 21C4 21D1 2203 2204 2206 2207 2208 220A 2216 2230 2231 2232 2233 2414 2420 2438 2460
2482 2484 2486 2487 2488 2489 249C 249D 24A0 24B0 24B1 24B6 24B7 24B8 24B9 24BA 24BB 24C7 24C9 24DC 24DD 24E0 24FA
2503 2504 2507 2508 2520 2521 2523 2531 2544 2560 2563 2571 2582 2584 25A0 25A2 25A5 25A6 25A7 25A9 25AA 25AB 25AC
25AD 25B0 25B2 25B8 25B9 25BA 25BB 25BC 25BD 25E0 25E2 25E5 25EC 25ED 25F9 25FA 25FB 2684 2685 2689 26B1 26B2 26B3
2702 2704 2705 2709 2717 2730 2757 2770 2782 2783 2786 2788 27A0 27B0 27B1 27B2 27BA 27BB 27E0 27FB 2803 2805 2808
2820 2822 2838 2860 2882 28A0 28A1 28A3 28B0 28B8 28B9 28BA 28BB 28E0 28E1 28E3 28F8 2B85 2B87 2B8C 2BB1 2BB3 2BB4
2BB5 2BB9 2C02 2C05 2C09 2C18 2C19 2C31 2C33 2C34 2C38 2C39 2C3A 2C58 2C59 2C77 2C79 2D04 2D05 2D18 2D19 2D30 2D39
2D58 2D59 2D79 2D83 2D98 2DB8 2DB9 2DD8 2DF9 2E03 2E06 2E12 2F04 2F06 2F18 2F38 2F58
"
NV_IDS=$(ioreg -r -c IOPCIDevice -d 1 | awk '/"vendor-id" = <de100000>/{nv=1} /"device-id" = </{ if (match($0, /<[0-9a-f]+>/)) d=substr($0, RSTART+1, 4) }
  /^\+-o|^ *\}/{ if (nv && d != "") print toupper(substr(d,3,2) substr(d,1,2)); nv=0; d="" }' | sort -u)
CARD=""; for i in $NV_IDS; do case " $(echo $SUPPORTED_IDS) " in *" $i "*) CARD=$i;; esac; done
# Every device id from 1E00 up is Turing or newer (TU102 is 1E02; Volta and older sit below it), which RM drives through GSP.
# The list above names the GeForce/RTX ids measured so far; a Quadro T1200 laptop (1FBC) was refused as "older".
[ -n "$CARD" ] || for i in $NV_IDS; do case $i in *[!0-9A-F]*|"") ;; *) [ $((16#$i)) -ge $((16#1E00)) ] && CARD=$i;; esac; done
[ -n "$CARD" ] || die "this NVIDIA card (device $(echo $NV_IDS)) is older than the driver supports (RTX 20 / Turing and newer); macOS keeps running on it in basic display, with no changes made"
ok "macOS $v, x86_64, NVIDIA GPU present"

step "2. package integrity"
# The extracted package is usually writable by the logged-in user, so a process running as that user could swap a kext
# between this check and the copy into /Library/Extensions. Check and install a root-owned private copy instead.
SELF=$(cd "$(dirname "$0")" && pwd)
STAGE=$(mktemp -d /var/tmp/nullmoth-package.XXXXXX) && [ -n "$STAGE" ] && [ -d "$STAGE" ] || die "create private package copy"
cp -cR "$HERE" "$STAGE/pkgroot" 2>/dev/null || { rm -rf "$STAGE/pkgroot"; ditto "$HERE" "$STAGE/pkgroot"; } && chown -R root:wheel "$STAGE" && chmod -R go-w "$STAGE" || die "copy package to $STAGE"
# shasum follows links: a link to an identical file outside the copy passes the check and can still change later.
while IFS= read -r -d '' link; do
  case "$(readlink "$link")" in /*|..|../*|*/..|*/../*) die "package holds a link that leaves the package: ${link#"$STAGE/pkgroot/"}";; esac
done < <(find "$STAGE/pkgroot" -type l -print0)
[ "$SELF" = "$HERE" ] && SELF="$STAGE/pkgroot"
HERE="$STAGE/pkgroot"
# VAR was set from the original folder before this step, and kpath reads the accelerator kext through it. Point it
# at the private copy too, or that one kernel extension is still taken from the folder the check no longer covers.
VAR="$HERE/Library/NullMoth/kexts/$MAJ"
# --strict: without it shasum skips a malformed line and still reports success
(cd "$HERE" && shasum -a 256 --strict -c SHA256SUMS --quiet) || die "SHA256SUMS mismatch: re-download the package"
for target in 15 26; do
  [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/MacOS/NVAccel" ] && \
    [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/Info.plist" ] || die "this package lacks the complete macOS $target accelerator"
done
ok "every file matches SHA256SUMS and both OS accelerators are present"
RUNTIME_PREFLIGHT="$SELF/nullmoth-runtime-check.sh"
[ -x "$RUNTIME_PREFLIGHT" ] || die "runtime compatibility checker is missing"
/bin/bash "$RUNTIME_PREFLIGHT" "$HERE" "$v" || die "userland runtime cannot load on this macOS version"

require_install_directory() {
  local metadata permissions entry
  [ ! -L "$1" ] && [ -d "$1" ] || die "installer parent must be a real directory (no driver changes made)"
  metadata=$(stat -f '%u %Lp' "$1" 2>/dev/null) || die "cannot verify installer parent ownership"
  case "$metadata" in
    "0 "[0-7][0-7][0-7]|"0 "[0-7][0-7][0-7][0-7]) ;;
    *) die "installer parent is not a verified root-owned directory (no driver changes made)";;
  esac
  permissions=${metadata#* }
  [ $((8#$permissions & 022)) -eq 0 ] || die "installer parent is writable by another account or group (no driver changes made)"
  entry=$(ls -lde "$1" 2>/dev/null | head -1 | awk '{print $1}')
  [ -n "$entry" ] || die "cannot inspect installer parent access controls"
  case "$entry" in *+*) die "installer parent has unreviewed access-control entries (no driver changes made)";; esac
}

# The installer owns this guard from preflight through commit/rollback. Never
# remove a pre-existing guard: it may belong to an active or interrupted install.
if [ "${CHECK:-0}" != 1 ]; then
  require_install_directory /Library
  if [ ! -e /Library/NullMoth ] && [ ! -L /Library/NullMoth ]; then
    mkdir -m 755 /Library/NullMoth || die "create installer state directory"
  fi
  require_install_directory /Library/NullMoth
  if ! mkdir -m 700 "$LOCKDIR" 2>/dev/null; then
    recover_stale_guard
    mkdir -m 700 "$LOCKDIR" 2>/dev/null || die "installer guard already exists; another install is active right now (no driver changes made)"
  fi
  LOCK_HELD=1
  printf 'pid=%s\nboot=%s\n' "$$" "$(boot_sec)" > "$LOCKDIR/owner" || die "cannot record the installer guard's owner"
  LOCK_ID=$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null) || die "cannot record installer guard identity"
  printf '%s\n' "$LOCK_ID" | grep -Eq '^[0-9]+:[0-9]+$' || die "installer guard identity is unavailable"
fi

primary_bundle_id() {
 if [ "$1" = NVPrimarySelector ]; then echo org.hackintosh.experimental.NVPrimarySelector
 else echo "com.nullmoth.$1"; fi
}
primary_marker() {
 /usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$2:IOPropertyMatch:nullmoth-auto-primary" "$1/Contents/Info.plist" 2>/dev/null
}
primary_apply() {
 local bundle=$1 name=$2 plist="$1/Contents/Info.plist" existing
 existing=$(/usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$name:IOPropertyMatch" "$plist" 2>/dev/null) || existing=''
 if [ -n "$existing" ]; then
  # Do not replace a different PCI/ACPI selection or silently widen its scope.
  [ "$(primary_marker "$bundle" "$name")" = true ] || return 1
  existing=$(/usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$name:IOPropertyMatch" "$plist") || return 1
  [ "$(printf '%s\n' "$existing" | awk '/ = /{n++}END{print n+0}')" = 1 ] || return 1
 else
  /usr/libexec/PlistBuddy -c "Add :IOKitPersonalities:$name:IOPropertyMatch dict" "$plist" || return 1
  /usr/libexec/PlistBuddy -c "Add :IOKitPersonalities:$name:IOPropertyMatch:nullmoth-auto-primary bool true" "$plist" || return 1
 fi
 codesign --force --sign - "$bundle" && codesign --verify --strict "$bundle"
}
primary_requested() {
 local rm accel
 rm=$(primary_marker "$EXT/NVRM.kext" NVRM) || rm=''
 accel=$(primary_marker "$EXT/NVAccel.kext" NVAccel) || accel=''
 if [ "$rm" = true ] || [ "$accel" = true ]; then
  [ "$rm" = true ] && [ "$accel" = true ] || return 2
  return 0
 fi
 [ "${NM_PRIMARY_GPU:-0}" = 1 ]
}

# An already configured selector is an opt-in that must survive a driver update.
# NM_PRIMARY_GPU=1 is for a first manual installation with a packaged selector.
PRIMARY=0
primary_requested; primary_status=$?
case "$primary_status" in
 0) PRIMARY=1;;
 2) die "incomplete primary-GPU matching on installed kexts; no files changed";;
esac
SELECTOR_SOURCE=""
if [ "$PRIMARY" = 1 ]; then
  SELECTOR_SOURCE="$HERE/Library/Extensions/NVPrimarySelector.kext"
  [ -d "$SELECTOR_SOURCE" ] || SELECTOR_SOURCE="$EXT/NVPrimarySelector.kext"
  [ -d "$SELECTOR_SOURCE" ] && [ ! -L "$SELECTOR_SOURCE" ] || die "primary-GPU matching needs NVPrimarySelector.kext; no files changed"
  codesign --verify --strict "$SELECTOR_SOURCE" || die "primary selector signature invalid; no files changed"
  KEXTS="$KEXTS NVPrimarySelector"
fi
step "3. test kernel collection"
T=$(mktemp -d /var/tmp/nullmoth.XXXX) && [ -n "$T" ] && mkdir -p "$T/repo" || die "create private preflight directory"
for x in "$EXT"/*.kext; do [ -d "$x" ] || continue; n=$(basename "$x" .kext); case " $KEXTS " in *" $n "*) ;; *) cp -R "$x" "$T/repo/" || die "stage third-party kext $n";; esac; done
for k in $KEXTS; do cp -R "$(kpath $k)" "$T/repo/" || die "copy $k"; done
if [ "$PRIMARY" = 1 ]; then
  primary_apply "$T/repo/NVRM.kext" NVRM && primary_apply "$T/repo/NVAccel.kext" NVAccel \
    || die "prepare primary-GPU metadata; no installed files changed"
fi
# macOS 26 kmutil silently skips kexts not owned by root ("No binaries or codeless kexts were provided").
chown -R root:wheel "$T/repo" && chmod -R go-w "$T/repo" || die "preflight kext permissions"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$T/repo" -A "$T/aux.kc" -z >"$T/kmutil.log" 2>&1 || { tail -20 "$T/kmutil.log"; die "test kernel collection build failed"; }
[ -s "$T/aux.kc" ] || die "test kernel collection build produced no output"
INS=$(kmutil inspect -a x86_64 -A "$T/aux.kc" 2>/dev/null) || die "cannot inspect the test kernel collection"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE '(com\.nullmoth\.[A-Za-z0-9]+|org\.hackintosh\.experimental\.NVPrimarySelector)' | grep -Fxq "$(primary_bundle_id "$k")" || { tail -20 "$T/kmutil.log"; die "kmutil refused com.nullmoth.$k (log above)"; }; done
ok "test collection holds all four kexts"
[ "${CHECK:-0}" = 1 ] && { rm -rf "$T"; echo; echo "CHECK PASS (nothing changed)"; exit 0; }

BK=$(mktemp -d "/Library/NullMoth/backup-$(date +%Y%m%d-%H%M%S).XXXXXX") && [ -n "$BK" ] && [ -d "$BK" ] || die "create unique backup directory"
[ "${CHECK:-0}" = 1 ] || printf 'bk=%s\n' "$BK" >> "$LOCKDIR/owner" || die "cannot record the backup in the installer guard"
step "4. back up what is there now -> $BK"
printf '%s\n' "$BUILD" > "$BK/macos-build" || die "record backup macOS build"
[ "$PRIMARY" != 1 ] || touch "$BK/primary-selector-used" || die "record primary selector backup"
# ditto, not cp -Rp: cp re-applies file flags through symlinks and stopped on nvmtl/libvulkan.dylib with "chflags: Too
# many levels of symbolic links" (Mac 1.10 logs 10-10, 2 uploads); ditto keeps links, flags and xattrs, as the restore does.
for k in $KEXTS; do [ ! -e "$EXT/$k.kext" ] || backup_copy "$k" ditto "$EXT/$k.kext" "$BK/$k.kext"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do [ ! -e "$GB/$b" ] || backup_copy "$b" ditto "$GB/$b" "$BK/$b"; done
[ ! -f "$KC" ] || backup_copy "kernel collection" cp -p "$KC" "$BK/AuxiliaryKernelExtensions.kc"
[ ! -d /Library/NullMoth/kexts ] || backup_copy "cached accelerators" ditto /Library/NullMoth/kexts "$BK/kexts"
[ ! -f /Library/NullMoth/os-major ] || backup_copy "OS record" cp -p /Library/NullMoth/os-major "$BK/os-major"
[ ! -f /Library/NullMoth/driver-version ] || backup_copy "version record" cp -p /Library/NullMoth/driver-version "$BK/driver-version"
# Back up the firmware from wherever it actually is. This version reads it from $FW, but a release
# before the move kept it in $FWOLD, so on an upgrade $FW does not exist yet and backing up only $FW
# saved nothing. Both restore paths (rollback_install and the boot-guard fallback) are gated on
# $BK/nvfw, so an empty backup made them no-ops: the fallback put the previous driver back with NO
# firmware at all and the Mac came up with no display and no way out. Measured on the test Mac.
if [ -d "$FW" ]; then backup_copy firmware ditto "$FW" "$BK/nvfw"
elif [ -d "$FWOLD" ]; then backup_copy firmware ditto "$FWOLD" "$BK/nvfw"
fi
ok "backup written"

step "5. install"
INSTALLING=1; [ "${CHECK:-0}" = 1 ] || : > "$LOCKDIR/installing"
for k in $KEXTS; do
  source=$(kpath "$k")
  if [ "$PRIMARY" = 1 ]; then source="$T/repo/$k.kext"; fi
  rm -rf "$EXT/$k.kext" && ditto "$source" "$EXT/$k.kext" || die "copy $k (restore from $BK)"
done
# both NVAccel builds stay on disk, so the first start after a macOS upgrade can switch to the matching one
mkdir -p /Library/NullMoth && rm -rf /Library/NullMoth/kexts && ditto "$HERE/Library/NullMoth/kexts" /Library/NullMoth/kexts || die "copy per-macOS accelerators (restore from $BK)"
for target in 15 26; do
  dir=/Library/NullMoth/kexts/$target
  if [ "$PRIMARY" = 1 ]; then
    primary_apply "$dir/NVAccel.kext" NVAccel || die "primary-GPU cache metadata for $target (restore from $BK)"
  fi
  [ -f "$dir/NVAccel.kext/Contents/MacOS/NVAccel" ] && [ -f "$dir/NVAccel.kext/Contents/Info.plist" ] || die "missing macOS $target accelerator (restore from $BK)"
  (cd "$dir" && find NVAccel.kext -type f -exec shasum -a 256 '{}' \;) > "$dir/SHA256SUMS" || die "cache manifest for macOS $target"
done
chown -R root:wheel /Library/NullMoth/kexts && chmod -R go-w /Library/NullMoth/kexts || die "cached accelerator permissions"
echo "$MAJ" > /Library/NullMoth/os-major || die "record selected OS"
mkdir -p "$GB" || die "create the bundle directory"
# The card runs these images and the kernel reads them, so this directory must be ours. It used to live in
# /Users/Shared, which is world-writable (1777): a directory left there by anyone else was adopted as-is,
# because the only permission step was a chmod that adds read bits and never takes write away. Refuse
# anything we did not make, the way the shader caches already refuse a cache someone else could have written.
if [ -e "$FW" ] && [ -n "$(find "$FW" -maxdepth 0 \( ! -user root -o -perm -o+w -o -perm -g+w \) -print -quit 2>/dev/null)" ]; then
  rm -rf "$FW" || die "clear a firmware directory that was not root-owned"
  echo "   note: the firmware folder was writable by other users; it has been replaced"
fi
mkdir -p "$FW" || die "create the firmware directory"
# $GB/nvmtl is replaced wholesale below, so move the shared shader caches aside first. NVMTLLibraryLoad.m
# reads them from NVMTL_AIRCACHE_DIR (/Library/GPUBundles/nvmtl/aircache) and uses that path whenever it is
# writable, which is what lets every process - WindowServer included - share one persistent cache instead of
# compiling into a private one that a reboot or a temp sweep throws away.
CACHEKEPT=""
for c in aircache spvcache; do
  [ ! -d "$GB/nvmtl/$c" ] || { mv "$GB/nvmtl/$c" "$T/keep-$c" && CACHEKEPT="$CACHEKEPT $c" || die "stash $GB/nvmtl/$c"; }
done
# A user's own settings survive updates: the lines between these two markers in nvrm610.conf are put back after the
# copy below. An RTX 3050 (GA106, GH #130) ran only with NVMTL_NO_SURFACE_PAGEOFF=1 and uncached command pools, and every
# update wrote the file fresh and dropped them. Only driver knobs (NVMTL_/NVK_/NVRM_ with plain values) are kept.
MYCONF_START='# --- my settings, kept on update ---'; MYCONF_END='# --- end my settings ---'
[ ! -f "$GB/nvmtl/nvrm610.conf" ] || sed -n "/^$MYCONF_START\$/,/^$MYCONF_END\$/p" "$GB/nvmtl/nvrm610.conf" \
  | grep -E "^($MYCONF_START|$MYCONF_END|(NVMTL|NVK|NVRM)_[A-Z0-9_]+=[A-Za-z0-9._-]{0,64})\$" > "$T/my-conf" || : > "$T/my-conf"
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl; do rm -rf "$GB/$b" && ditto "$HERE/Library/GPUBundles/$b" "$GB/$b" || die "copy $b (restore from $BK)"; done
if [ -s "$T/my-conf" ] && [ -f "$GB/nvmtl/nvrm610.conf" ]; then
  cat "$T/my-conf" >> "$GB/nvmtl/nvrm610.conf" || die "keep your settings in nvrm610.conf (restore from $BK)"
  ok "kept your settings in nvrm610.conf ($(grep -cvE '^#' "$T/my-conf") line(s))"
fi
# nvmtl was just replaced wholesale: when the separate AMD/Intel driver is installed, its Vulkan drivers (which its
# own installer put in /Library/NullMoth/gpu-userland) go back in beside NVK
if [ -d "$EXT/NMXGPU.kext" ] && [ -d /Library/NullMoth/gpu-userland ]; then
  ditto /Library/NullMoth/gpu-userland "$GB/nvmtl" || die "copy the AMD/Intel Vulkan drivers (restore from $BK)"
fi
cp "$HERE/Library/GPUBundles/nvmtl-allow.txt" "$GB/" || die "copy bundle allow list"
ditto "$HERE/Users/Shared/nvfw" "$FW" || die "copy firmware"
chown -R root:wheel "$FW" && chmod -R go-w,a+rX "$FW" || die "firmware permissions"
# nothing reads the old location any more; leaving it behind leaves a writable folder with our name on it
rm -rf "$FWOLD" 2>/dev/null || true
for k in $KEXTS; do chown -R root:wheel "$EXT/$k.kext" && chmod -R 755 "$EXT/$k.kext" || die "permissions for $k"; done
chown -R root:wheel "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" "$GB/nvmtl-allow.txt" || die "bundle permissions"
# WindowServer runs as _windowserver and must read the bundles and the allow list. WAS: cp under the app's privileged
# helper (umask 077) left nvmtl-allow.txt at 0600, WindowServer got no Metal device, and CoreDisplay aborted
# ("Failed to create MetalDevice") on every start: the crash loop after install on many machines.
chmod -R go-w,a+rX "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" && chmod 644 "$GB/nvmtl-allow.txt" || die "bundle permissions"
sudo -u _windowserver /bin/test -r "$GB/nvmtl-allow.txt" || die "WindowServer cannot read $GB/nvmtl-allow.txt (restore from $BK)"
# Create the shared shader caches the plugin looks for. Without them NVMTL_AIRCACHE_DIR never resolves, so
# WindowServer, which compiles the compositor's pipelines on demand, re-translates them from scratch at every start
# and its content renders black until the translation finishes (community patch #19; ownership below).
# NOTE: this must stay AFTER the "chmod -R go-w ... $GB/nvmtl" above. That recursive chmod would otherwise
# strip the write bits back off aircache/spvcache and the shared cache would silently stop working.
mkdir -p "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "create the shared shader caches"
# Owned by WindowServer (_windowserver), writable by nobody else: WindowServer loads what is in these caches as
# compiled shaders, so a cache any user can write (1777, as first proposed) would let a local user plant code into
# WindowServer. Other processes see the folders read-only and keep their own private caches, which the plugin already
# does when NVMTL_AIRCACHE_DIR is not writable. A cache kept from an earlier install is reused only if nobody but
# _windowserver could have written into it; otherwise it starts empty.
for c in $CACHEKEPT; do
  [ -d "$T/keep-$c" ] || continue
  if [ -z "$(find "$T/keep-$c" \( ! -user _windowserver -o -perm -o+w -o -perm -g+w \) -print -quit 2>/dev/null)" ]; then
    rm -rf "$GB/nvmtl/$c" && mv "$T/keep-$c" "$GB/nvmtl/$c" || die "restore $GB/nvmtl/$c"
  else rm -rf "$T/keep-$c"; echo "   note: $c was writable by other users; starting it empty"; fi
done
chown -R _windowserver:wheel "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "shader cache owner"
chmod 0755 "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "shared shader cache permissions"
NEWKC="$KC.nullmoth-install-new"; rm -f "$NEWKC"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$EXT" -A "$NEWKC" -z >"$T/kmutil2.log" 2>&1 || die "live kernel collection build failed (restore from $BK)"
[ -s "$NEWKC" ] || die "live kernel collection build produced no output (restore from $BK)"
INS=$(kmutil inspect -a x86_64 -A "$NEWKC" 2>/dev/null) || die "cannot inspect the new kernel collection (restore from $BK)"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE '(com\.nullmoth\.[A-Za-z0-9]+|org\.hackintosh\.experimental\.NVPrimarySelector)' | grep -Fxq "$(primary_bundle_id "$k")" || die "com.nullmoth.$k missing from the new collection (restore from $BK)"; done
mv -f "$NEWKC" "$KC" || die "cannot publish the new kernel collection (restore from $BK)"
INSTALLING=0
rm -rf "$T"
ok "installed"
# Record the version for the app's update check. Only the app's setup wrote it, so installs from the release tar left it
# empty and the app kept offering the version already installed (10-10 chat, 1.10.0).
if [ -f "$HERE/VERSION" ]; then mkdir -p /Library/NullMoth && tr -cd '0-9.\n' < "$HERE/VERSION" | head -n 1 > /Library/NullMoth/driver-version.tmp \
  && chmod 644 /Library/NullMoth/driver-version.tmp && mv /Library/NullMoth/driver-version.tmp /Library/NullMoth/driver-version; fi

step "6. boot-args"
# A remove flag left by a boot-picker removal that did not finish keeps every NullMoth kext off at boot. Installing
# again means the driver is wanted, so the flag goes; the boot picker sets it again when it is chosen.
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove 2>/dev/null || true

step "6b. falling back to the previous driver if this one cannot start"
# The kernel's boot guard (nvrm_off.h) turns the driver off after two starts in a row that never brought the display
# up, so the Mac reaches a desktop on the firmware's framebuffer. This one-shot job then puts the previous driver back
# from this install's backup and restarts once. It removes itself as soon as the new driver has shown its display
# (nullmoth-nv-ok), so nothing keeps running on a Mac where the update worked. A fresh install has no previous driver to
# go back to: the guard still keeps the Mac usable, and nothing is restored.
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-starts 2>/dev/null || true
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-ok 2>/dev/null || true
LG=/Library/NullMoth/lastgood.sh; LGP=/Library/LaunchDaemons/com.nullmoth.lastgood.plist
launchctl bootout system/com.nullmoth.lastgood 2>/dev/null || true
rm -f "$LG" "$LGP"
if [ -d "$BK/NVRM.kext" ]; then
  mkdir -p /Library/NullMoth /Library/LaunchDaemons || die "create the fallback job's folders"
  cat > "$LG.tmp" <<EOF || die "write the fallback job"
#!/bin/bash
# Written by the NVIDIA driver install: restores the driver in $BK if the installed one cannot start.
PATH=/usr/bin:/bin:/usr/sbin:/sbin
BK='$BK'; LOG=/Library/NullMoth/lastgood.log; V=7C436110-AB2A-4BBB-A880-FE41995C9F82
# no launchctl bootout here: booting out the running job kills this script before it can restart the Mac (10-17 test)
done_job() { rm -f /Library/LaunchDaemons/com.nullmoth.lastgood.plist /Library/NullMoth/lastgood.sh; }
if nvram \$V:nullmoth-nv-ok >/dev/null 2>&1; then echo "\$(date) the new driver brought its display up; keeping it" >> \$LOG; done_job; exit 0; fi
v=\$(nvram \$V:nullmoth-nv-starts 2>/dev/null | cut -f2)
case "\$v" in %0[2-9a-f]|%[1-9a-f]?) ;; *) exit 0;; esac
[ -d "\$BK/NVRM.kext" ] || { echo "\$(date) the driver turned itself off but its backup \$BK is gone" >> \$LOG; done_job; exit 0; }
echo "\$(date) the new driver did not bring the display up twice; restoring the previous driver from \$BK" >> \$LOG
for k in $KEXTS; do rm -rf "$EXT/\$k.kext"; [ ! -e "\$BK/\$k.kext" ] || ditto "\$BK/\$k.kext" "$EXT/\$k.kext"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do rm -rf "$GB/\$b"; [ ! -e "\$BK/\$b" ] || ditto "\$BK/\$b" "$GB/\$b"; done
rm -rf /Library/NullMoth/kexts "$FW"
[ ! -d "\$BK/kexts" ] || ditto "\$BK/kexts" /Library/NullMoth/kexts
[ ! -d "\$BK/nvfw" ] || ditto "\$BK/nvfw" "$FW"
# the previous driver may be one that reads the old location
[ ! -d "\$BK/nvfw" ] || { mkdir -p "$FWOLD" && ditto "\$BK/nvfw" "$FWOLD"; }
if [ -f "\$BK/AuxiliaryKernelExtensions.kc" ]; then cp -p "\$BK/AuxiliaryKernelExtensions.kc" "$KC"; else rm -f "$KC"; fi
[ ! -f "\$BK/os-major" ] || cp -p "\$BK/os-major" /Library/NullMoth/os-major
if [ -f "\$BK/driver-version" ]; then cp -p "\$BK/driver-version" /Library/NullMoth/driver-version; else rm -f /Library/NullMoth/driver-version; fi
nvram -d \$V:nullmoth-nv-starts 2>/dev/null
echo "previous" > /Library/NullMoth/driver-restored
done_job
echo "\$(date) previous driver restored; restarting" >> \$LOG
shutdown -r now
EOF
  chmod 755 "$LG.tmp" && chown root:wheel "$LG.tmp" && mv -f "$LG.tmp" "$LG" || die "install the fallback job"
  cat > "$LGP" <<EOF || die "write the fallback job's plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>com.nullmoth.lastgood</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$LG</string></array>
<key>RunAtLoad</key><true/></dict></plist>
EOF
  chmod 644 "$LGP" && chown root:wheel "$LGP" || die "fallback job permissions"
  ok "previous driver kept as the fallback ($BK)"
else
  ok "no previous driver to fall back to (first install)"
fi
step "6c. counting a start as good once the desktop is up"
# The kernel's guard only counts a start as good when the framebuffer lights a head, or when the card reports that none
# of its connectors has a display. A laptop whose panel hangs off the integrated GPU reaches that report late and does
# not always reach it: two boots that missed it switched a healthy driver off, leaving the Mac with no NVIDIA card in
# Metal at all. A desktop that comes up and stays up is also a good start, so this job says so. A boot that never
# reaches a desktop keeps its count, which is what the guard is for.
BG=/Library/NullMoth/bootguard.sh; BGP=/Library/LaunchDaemons/com.nullmoth.bootguard.plist
launchctl bootout system/com.nullmoth.bootguard 2>/dev/null || true
mkdir -p /Library/NullMoth /Library/LaunchDaemons || die "create the boot-guard job's folders"
cat > "$BG.tmp" <<'EOF' || die "write the boot-guard job"
#!/bin/bash
# Written by the NVIDIA driver install: the desktop came up, so this start counts as good.
PATH=/usr/bin:/bin:/usr/sbin:/sbin
V=7C436110-AB2A-4BBB-A880-FE41995C9F82; LOG=/Library/NullMoth/bootguard.log
n=0
while [ "$n" -lt 48 ]; do
  if pgrep -x WindowServer >/dev/null 2>&1; then
    sleep 45
    pgrep -x WindowServer >/dev/null 2>&1 || { n=$((n + 1)); continue; }
    nvram -d $V:nullmoth-nv-starts 2>/dev/null
    nvram $V:nullmoth-nv-ok=1 2>/dev/null
    echo "$(date) the desktop came up and stayed up; this start counts as good" >> $LOG
    exit 0
  fi
  n=$((n + 1)); sleep 5
done
echo "$(date) no desktop within four minutes; the start keeps its count" >> $LOG
EOF
chmod 755 "$BG.tmp" && chown root:wheel "$BG.tmp" && mv -f "$BG.tmp" "$BG" || die "install the boot-guard job"
cat > "$BGP" <<EOF || die "write the boot-guard job's plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>com.nullmoth.bootguard</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$BG</string></array>
<key>RunAtLoad</key><true/></dict></plist>
EOF
chmod 644 "$BGP" && chown root:wheel "$BGP" || die "boot-guard job permissions"
ok "a start counts as good once the desktop is up"

ba=$(nvram boot-args 2>/dev/null | cut -f2-)
for a in nvfb=1 nvaccel=1; do case " $ba " in *" $a "*) ;; *) echo "   NOTE: boot-args lack '$a' — add it in your OpenCore config.plist (README: Install — by hand), or let the 1401 Mac app set OpenCore up";; esac; done
# WhateverGreen's boot screen copy (on unless gfxrst=4) reads a console mapping macOS may already have freed when this
# driver's display first opens, and the Mac panics in IOFramebuffer::postOpen.
if kmutil showloaded --list-only --bundle-identifier as.vit9696.WhateverGreen 2>/dev/null | grep -q WhateverGreen; then
  case " $ba " in *" gfxrst=4 "*) ;; *) echo "   NOTE: WhateverGreen is loaded and boot-args lack 'gfxrst=4' — add it in your OpenCore config.plist, or the Mac can panic when the NVIDIA display first opens";; esac
fi

echo; echo "Done. Reboot now: sudo shutdown -r now"
echo "If macOS asks, allow the extensions in System Settings > Privacy & Security, then reboot once more."
echo "Undo: sudo ./uninstall.sh $BK"
