#!/bin/bash
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
if [ "${1:-}" = --payload ]; then
  [ "$#" = 2 ] && [ -d "$2" ] || { echo "STOP: --payload requires an existing package directory" >&2; exit 2; }
  HERE=$(cd "$2" && pwd) || exit 2
elif [ "$#" != 0 ]; then echo "STOP: expected --payload directory or no arguments" >&2; exit 2; fi
KEXTS="NVRM NVAccel NVRMFB NVRMAGDC"
EXT=/Library/Extensions; GB=/Library/GPUBundles; FW=/Users/Shared/nvfw
KC=/Library/KernelCollections/AuxiliaryKernelExtensions.kc
K=/System/Library/Kernels/kernel
# macOS 13+ kmutil refuses to build any kernel collection without a Kernel Debug Kit matching the build unless it is told
# --allow-missing-kdk. The auxiliary collection only links against the boot and system collections already on disk.
KARG=(--allow-missing-kdk); [ -f "$K" ] && KARG+=(--kernel "$K")
KB=/System/Library/KernelCollections/BootKernelExtensions.kc
KS=/System/Library/KernelCollections/SystemKernelExtensions.kc
BK=""; LOCKDIR=/Library/NullMoth/install.lock; LOCK_HELD=0; LOCK_RELEASE_OK=1; LOCK_ID=""
INSTALLING=0; NEWKC=""
release_install_lock() {
  if [ "$LOCK_HELD" = 1 ] && [ "$LOCK_RELEASE_OK" = 1 ]; then
    if [ -z "$LOCK_ID" ] || [ -L "$LOCKDIR" ] || [ ! -d "$LOCKDIR" ] ||
       [ "$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null)" != "$LOCK_ID" ]; then
      echo "   NOTE: installer guard identity changed; its current path is retained for review." >&2
      return
    fi
    rmdir "$LOCKDIR" || echo "   NOTE: installer lock could not be released; review it before another install." >&2
    LOCK_HELD=0
  elif [ "$LOCK_HELD" = 1 ]; then
    echo "   NOTE: installer guard retained because restoration was incomplete; recovery review is required." >&2
  fi
}

rollback_install() {
  local failed=0 k b
  [ -z "$NEWKC" ] || rm -f "$NEWKC" || failed=1
  for k in $KEXTS; do
    rm -rf "$EXT/$k.kext" || failed=1
    [ ! -e "$BK/$k.kext" ] || ditto "$BK/$k.kext" "$EXT/$k.kext" || failed=1
  done
  for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do
    rm -rf "$GB/$b" || failed=1
    [ ! -e "$BK/$b" ] || ditto "$BK/$b" "$GB/$b" || failed=1
  done
  rm -rf /Library/NullMoth/kexts "$FW" || failed=1
  [ ! -d "$BK/kexts" ] || ditto "$BK/kexts" /Library/NullMoth/kexts || failed=1
  [ ! -d "$BK/nvfw" ] || ditto "$BK/nvfw" "$FW" || failed=1
  if [ -f "$BK/AuxiliaryKernelExtensions.kc" ]; then cp -p "$BK/AuxiliaryKernelExtensions.kc" "$KC" || failed=1
  else rm -f "$KC" || failed=1; fi
  if [ -f "$BK/os-major" ]; then cp -p "$BK/os-major" /Library/NullMoth/os-major || failed=1
  else rm -f /Library/NullMoth/os-major || failed=1; fi
  return "$failed"
}
step() { echo; echo "== $*"; }; ok() { echo "   ok  $*"; }
die() {
  echo "   STOP: $*" >&2
  if [ "$INSTALLING" = 1 ]; then
    if rollback_install; then echo "   Previous installation restored; backup retained at $BK." >&2
    else LOCK_RELEASE_OK=0; echo "   Restore incomplete; recovery files remain at $BK." >&2; fi
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
trap release_install_lock EXIT
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
kpath() { [ "$1" = NVAccel ] && echo "$VAR/NVAccel.kext" || echo "$HERE/Library/Extensions/$1.kext"; }
ioreg -r -c IOPCIDevice -d 1 | grep -q '"vendor-id" = <de100000>' || die "no NVIDIA GPU found on PCI"
# The driver runs the cards NVIDIA's GSP firmware supports (Turing and later). The same list the Windows builder uses
# (open-gpu-kernel-modules 610.57.04, p1401/nvidia_gsp_ids.json). Installing it for an older card (GTX 10 and before)
# would switch the firmware screen off and leave nothing to draw: those PCs run macOS in basic display instead.
SUPPORTED_IDS="
1E02 1E04 1E07 1E30 1E36 1E78 1E81 1E82 1E84 1E87 1E89 1E90 1E91 1E93 1EB0 1EB1 1EB5 1EB6 1EC2 1EC7 1ED0 1ED1 1ED3
1EF5 1F02 1F03 1F06 1F07 1F08 1F0A 1F10 1F11 1F12 1F14 1F15 1F36 1F42 1F47 1F50 1F51 1F54 1F55 1F76 1F82 1F83 1F91
1F95 1F96 1F97 1F98 1F99 1F9C 1F9D 1F9F 1FA0 1FB8 1FB9 1FDD 1FF9 2182 2184 2187 2188 2191 2192 21C4 21D1 2203 2204
2206 2207 2208 220A 2216 2230 2231 2232 2233 2414 2420 2438 2460 2482 2484 2486 2487 2488 2489 249C 249D 24A0 24B0
24B1 24B6 24B7 24B8 24B9 24BA 24BB 24C7 24C9 24DC 24DD 24E0 24FA 2503 2504 2507 2508 2520 2521 2523 2531 2544 2560
2563 2571 2582 2584 25A0 25A2 25A5 25A6 25A7 25A9 25AA 25AB 25AC 25AD 25B0 25B2 25B8 25B9 25BA 25BB 25BC 25BD 25E0
25E2 25E5 25EC 25ED 25F9 25FA 25FB 2684 2685 2689 26B1 26B2 26B3 2702 2704 2705 2709 2717 2730 2757 2770 2782 2783
2786 2788 27A0 27B0 27B1 27B2 27BA 27BB 27E0 27FB 2803 2805 2808 2820 2822 2838 2860 2882 28A0 28A1 28A3 28B0 28B8
28B9 28BA 28BB 28E0 28E1 28E3 28F8 2B85 2B87 2B8C 2BB1 2BB3 2BB4 2BB5 2BB9 2C02 2C05 2C18 2C19 2C31 2C33 2C34 2C38
2C39 2C3A 2C58 2C59 2C77 2C79 2D04 2D05 2D18 2D19 2D30 2D39 2D58 2D59 2D79 2D83 2D98 2DB8 2DB9 2DD8 2DF9 2E03 2E06
2F04 2F06 2F18 2F38 2F58
"
NV_IDS=$(ioreg -r -c IOPCIDevice -d 1 | awk '/"vendor-id" = <de100000>/{nv=1} /"device-id" = </{ if (match($0, /<[0-9a-f]+>/)) d=substr($0, RSTART+1, 4) }
  /^\+-o|^ *\}/{ if (nv && d != "") print toupper(substr(d,3,2) substr(d,1,2)); nv=0; d="" }' | sort -u)
CARD=""; for i in $NV_IDS; do case " $(echo $SUPPORTED_IDS) " in *" $i "*) CARD=$i;; esac; done
[ -n "$CARD" ] || die "this NVIDIA card (device $(echo $NV_IDS)) is older than the driver supports (RTX 20 / Turing and newer); macOS keeps running on it in basic display, with no changes made"
ok "macOS $v, x86_64, NVIDIA GPU present"

step "2. package integrity"
(cd "$HERE" && shasum -a 256 -c SHA256SUMS --quiet) || die "SHA256SUMS mismatch: re-download the package"
for target in 15 26; do
  [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/MacOS/NVAccel" ] && \
    [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/Info.plist" ] || die "this package lacks the complete macOS $target accelerator"
done
ok "every file matches SHA256SUMS and both OS accelerators are present"
RUNTIME_PREFLIGHT="$(cd "$(dirname "$0")" && pwd)/nullmoth-runtime-check.sh"
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
  mkdir -m 700 "$LOCKDIR" 2>/dev/null || die "installer guard already exists; another install is active or an interrupted guard needs review (no driver changes made)"
  LOCK_HELD=1
  LOCK_ID=$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null) || die "cannot record installer guard identity"
  printf '%s\n' "$LOCK_ID" | grep -Eq '^[0-9]+:[0-9]+$' || die "installer guard identity is unavailable"
fi

step "3. test kernel collection"
T=$(mktemp -d /var/tmp/nullmoth.XXXX) && [ -n "$T" ] && mkdir -p "$T/repo" || die "create private preflight directory"
for x in "$EXT"/*.kext; do [ -d "$x" ] || continue; n=$(basename "$x" .kext); case " $KEXTS " in *" $n "*) ;; *) cp -R "$x" "$T/repo/" || die "stage third-party kext $n";; esac; done
for k in $KEXTS; do cp -R "$(kpath $k)" "$T/repo/" || die "copy $k"; done
# macOS 26 kmutil silently skips kexts not owned by root ("No binaries or codeless kexts were provided").
chown -R root:wheel "$T/repo" && chmod -R go-w "$T/repo" || die "preflight kext permissions"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$T/repo" -A "$T/aux.kc" -z >"$T/kmutil.log" 2>&1 || { tail -20 "$T/kmutil.log"; die "test kernel collection build failed"; }
[ -s "$T/aux.kc" ] || die "test kernel collection build produced no output"
INS=$(kmutil inspect -a x86_64 -A "$T/aux.kc" 2>/dev/null) || die "cannot inspect the test kernel collection"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | grep -Fxq "com.nullmoth.$k" || { tail -20 "$T/kmutil.log"; die "kmutil refused com.nullmoth.$k (log above)"; }; done
ok "test collection holds all four kexts"
[ "${CHECK:-0}" = 1 ] && { rm -rf "$T"; echo; echo "CHECK PASS (nothing changed)"; exit 0; }

BK=$(mktemp -d "/Library/NullMoth/backup-$(date +%Y%m%d-%H%M%S).XXXXXX") && [ -n "$BK" ] && [ -d "$BK" ] || die "create unique backup directory"
step "4. back up what is there now -> $BK"
printf '%s\n' "$BUILD" > "$BK/macos-build" || die "record backup macOS build"
# ditto, not cp -Rp: cp re-applies file flags through symlinks and stopped on nvmtl/libvulkan.dylib with "chflags: Too
# many levels of symbolic links" (Mac 1.10 logs 10-10, 2 uploads); ditto keeps links, flags and xattrs, as the restore does.
for k in $KEXTS; do [ ! -e "$EXT/$k.kext" ] || backup_copy "$k" ditto "$EXT/$k.kext" "$BK/$k.kext"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do [ ! -e "$GB/$b" ] || backup_copy "$b" ditto "$GB/$b" "$BK/$b"; done
[ ! -f "$KC" ] || backup_copy "kernel collection" cp -p "$KC" "$BK/AuxiliaryKernelExtensions.kc"
[ ! -d /Library/NullMoth/kexts ] || backup_copy "cached accelerators" ditto /Library/NullMoth/kexts "$BK/kexts"
[ ! -f /Library/NullMoth/os-major ] || backup_copy "OS record" cp -p /Library/NullMoth/os-major "$BK/os-major"
[ ! -d "$FW" ] || backup_copy firmware ditto "$FW" "$BK/nvfw"
ok "backup written"

step "5. install"
INSTALLING=1
for k in $KEXTS; do rm -rf "$EXT/$k.kext" && ditto "$(kpath $k)" "$EXT/$k.kext" || die "copy $k (restore from $BK)"; done
# both NVAccel builds stay on disk, so the first start after a macOS upgrade can switch to the matching one
mkdir -p /Library/NullMoth && rm -rf /Library/NullMoth/kexts && ditto "$HERE/Library/NullMoth/kexts" /Library/NullMoth/kexts || die "copy per-macOS accelerators (restore from $BK)"
for target in 15 26; do
  dir=/Library/NullMoth/kexts/$target
  [ -f "$dir/NVAccel.kext/Contents/MacOS/NVAccel" ] && [ -f "$dir/NVAccel.kext/Contents/Info.plist" ] || die "missing macOS $target accelerator (restore from $BK)"
  (cd "$dir" && find NVAccel.kext -type f -exec shasum -a 256 '{}' \;) > "$dir/SHA256SUMS" || die "cache manifest for macOS $target"
done
chown -R root:wheel /Library/NullMoth/kexts && chmod -R go-w /Library/NullMoth/kexts || die "cached accelerator permissions"
echo "$MAJ" > /Library/NullMoth/os-major || die "record selected OS"
mkdir -p "$GB" "$FW" || die "create bundle and firmware directories"
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl; do rm -rf "$GB/$b" && ditto "$HERE/Library/GPUBundles/$b" "$GB/$b" || die "copy $b (restore from $BK)"; done
cp "$HERE/Library/GPUBundles/nvmtl-allow.txt" "$GB/" || die "copy bundle allow list"
ditto "$HERE/Users/Shared/nvfw" "$FW" || die "copy firmware"
for k in $KEXTS; do chown -R root:wheel "$EXT/$k.kext" && chmod -R 755 "$EXT/$k.kext" || die "permissions for $k"; done
chown -R root:wheel "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" "$GB/nvmtl-allow.txt" && chmod -R a+rX "$FW" || die "bundle or firmware permissions"
# WindowServer runs as _windowserver and must read the bundles and the allow list. WAS: cp under the app's privileged
# helper (umask 077) left nvmtl-allow.txt at 0600, WindowServer got no Metal device, and CoreDisplay aborted
# ("Failed to create MetalDevice") on every start: the crash loop after install on many machines.
chmod -R go-w,a+rX "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" && chmod 644 "$GB/nvmtl-allow.txt" || die "bundle permissions"
sudo -u _windowserver /bin/test -r "$GB/nvmtl-allow.txt" || die "WindowServer cannot read $GB/nvmtl-allow.txt (restore from $BK)"
NEWKC="$KC.nullmoth-install-new"; rm -f "$NEWKC"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$EXT" -A "$NEWKC" -z >"$T/kmutil2.log" 2>&1 || die "live kernel collection build failed (restore from $BK)"
[ -s "$NEWKC" ] || die "live kernel collection build produced no output (restore from $BK)"
INS=$(kmutil inspect -a x86_64 -A "$NEWKC" 2>/dev/null) || die "cannot inspect the new kernel collection (restore from $BK)"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | grep -Fxq "com.nullmoth.$k" || die "com.nullmoth.$k missing from the new collection (restore from $BK)"; done
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
ba=$(nvram boot-args 2>/dev/null | cut -f2-)
for a in nvfb=1 nvaccel=1; do case " $ba " in *" $a "*) ;; *) echo "   NOTE: boot-args lack '$a' — add it in your OpenCore config.plist (see README)";; esac; done

echo; echo "Done. Reboot now: sudo shutdown -r now"
echo "If macOS asks, allow the extensions in System Settings > Privacy & Security, then reboot once more."
echo "Undo: sudo ./uninstall.sh $BK"
