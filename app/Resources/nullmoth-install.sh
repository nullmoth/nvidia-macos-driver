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
BK=/Library/NullMoth/backup-$(date +%Y%m%d-%H%M%S)
INSTALLING=0; NEWKC=""
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
    else echo "   Restore incomplete; recovery files remain at $BK." >&2; fi
  fi
  exit 1
}

[ "$(id -u)" -eq 0 ] || die "run with sudo"
step "1. this Mac"
[ "$(uname -m)" = x86_64 ] || die "Intel/x86_64 only"
v=$(sw_vers -productVersion); MAJ=${v%%.*}
BUILD=$(sw_vers -buildVersion)
printf '%s\n' "$BUILD" | grep -Eq '^[0-9]+[A-Za-z][A-Za-z0-9]+$' || die "cannot identify the current macOS build"
case $MAJ in 15|26) ;; *) die "macOS 15 or 26 required (this is $v)";; esac
# NVAccel is the one kext built per macOS: Tahoe made the IOAcceleratorFamily2 methods it inherits private (see
# kexts/NVRM/accel/gen_tahoe_fwd.py). The other three kexts are the same binaries on 15 and 26.
VAR="$HERE/Library/NullMoth/kexts/$MAJ"; [ -d "$VAR/NVAccel.kext" ] || die "this package has no NVAccel for macOS $MAJ"
kpath() { [ "$1" = NVAccel ] && echo "$VAR/NVAccel.kext" || echo "$HERE/Library/Extensions/$1.kext"; }
ioreg -r -c IOPCIDevice -d 1 | grep -q '"vendor-id" = <de100000>' || die "no NVIDIA GPU found on PCI"
ok "macOS $v, x86_64, NVIDIA GPU present"

step "2. package integrity"
(cd "$HERE" && shasum -a 256 -c SHA256SUMS --quiet) || die "SHA256SUMS mismatch: re-download the package"
for target in 15 26; do
  [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/MacOS/NVAccel" ] && \
    [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/Info.plist" ] || die "this package lacks the complete macOS $target accelerator"
done
ok "every file matches SHA256SUMS and both OS accelerators are present"

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

step "4. back up what is there now -> $BK"
mkdir -p "$BK" || die "create backup directory"
printf '%s\n' "$BUILD" > "$BK/macos-build" || die "record backup macOS build"
for k in $KEXTS; do [ ! -e "$EXT/$k.kext" ] || cp -Rp "$EXT/$k.kext" "$BK/" || die "back up $k"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do [ ! -e "$GB/$b" ] || cp -Rp "$GB/$b" "$BK/" || die "back up $b"; done
[ ! -f "$KC" ] || cp -p "$KC" "$BK/AuxiliaryKernelExtensions.kc" || die "back up kernel collection"
[ ! -d /Library/NullMoth/kexts ] || ditto /Library/NullMoth/kexts "$BK/kexts" || die "back up cached accelerators"
[ ! -f /Library/NullMoth/os-major ] || cp -p /Library/NullMoth/os-major "$BK/os-major" || die "back up OS record"
[ ! -d "$FW" ] || ditto "$FW" "$BK/nvfw" || die "back up firmware"
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
NEWKC="$KC.nullmoth-install-new"; rm -f "$NEWKC"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$EXT" -A "$NEWKC" -z >"$T/kmutil2.log" 2>&1 || die "live kernel collection build failed (restore from $BK)"
[ -s "$NEWKC" ] || die "live kernel collection build produced no output (restore from $BK)"
INS=$(kmutil inspect -a x86_64 -A "$NEWKC" 2>/dev/null) || die "cannot inspect the new kernel collection (restore from $BK)"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | grep -Fxq "com.nullmoth.$k" || die "com.nullmoth.$k missing from the new collection (restore from $BK)"; done
mv -f "$NEWKC" "$KC" || die "cannot publish the new kernel collection (restore from $BK)"
INSTALLING=0
rm -rf "$T"
ok "installed"

step "6. boot-args"
ba=$(nvram boot-args 2>/dev/null | cut -f2-)
for a in nvfb=1 nvaccel=1; do case " $ba " in *" $a "*) ;; *) echo "   NOTE: boot-args lack '$a' — add it in your OpenCore config.plist (see README)";; esac; done

echo; echo "Done. Reboot now: sudo shutdown -r now"
echo "If macOS asks, allow the extensions in System Settings > Privacy & Security, then reboot once more."
echo "Undo: sudo ./uninstall.sh $BK"
