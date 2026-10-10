#!/bin/bash
set -u
KEXTS="NVRM NVAccel NVRMFB NVRMAGDC"; EXT=/Library/Extensions; GB=/Library/GPUBundles
KC=/Library/KernelCollections/AuxiliaryKernelExtensions.kc
K=/System/Library/Kernels/kernel
KARG=(--allow-missing-kdk); [ -f "$K" ] && KARG+=(--kernel "$K")   # without a matching Kernel Debug Kit, kmutil needs --allow-missing-kdk
KB=/System/Library/KernelCollections/BootKernelExtensions.kc
KS=/System/Library/KernelCollections/SystemKernelExtensions.kc
REMOVING=0; RB=""; NEWKC=""; REMOVE_KC=0; CANDIDATE=""
rollback_removal() {
  local failed=0 k b
  for k in $KEXTS; do [ ! -e "$RB/$k.kext" ] || cp -Rp "$RB/$k.kext" "$EXT/" || failed=1; done
  for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do [ ! -e "$RB/$b" ] || cp -Rp "$RB/$b" "$GB/" || failed=1; done
  [ ! -e "$RB/kexts" ] || cp -Rp "$RB/kexts" /Library/NullMoth/ || failed=1
  [ ! -e "$RB/driver-version" ] || cp -p "$RB/driver-version" /Library/NullMoth/driver-version || failed=1
  [ ! -e "$RB/update-pending" ] || cp -p "$RB/update-pending" /Library/NullMoth/update-pending || failed=1
  [ ! -e "$RB/com.nullmoth.osupdate.plist" ] || cp -p "$RB/com.nullmoth.osupdate.plist" /Library/LaunchDaemons/ || failed=1
  if [ -e "$RB/AuxiliaryKernelExtensions.kc" ]; then cp -p "$RB/AuxiliaryKernelExtensions.kc" "$KC" || failed=1
  else rm -f "$KC" || failed=1; fi
  return "$failed"
}
die() {
  echo "STOP: $*" >&2
  if [ "$REMOVING" = 1 ]; then
    if rollback_removal; then echo "Previous driver files and collection restored." >&2
    else echo "Removal rollback incomplete; backup retained at $RB." >&2; exit 1; fi
  fi
  [ -z "$RB" ] || rm -rf "$RB"
  [ -z "$NEWKC" ] || rm -f "$NEWKC"
  exit 1
}
trap 'die "uninstaller interrupted (signal HUP)"' HUP
trap 'die "uninstaller interrupted (signal INT)"' INT
trap 'die "uninstaller interrupted (signal TERM)"' TERM
[ "$(id -u)" -eq 0 ] || die "run with sudo"
BK=${1:-}
[ -z "$BK" ] || [ -d "$BK" ] || die "no backup at $BK"
BUILD=$(sw_vers -buildVersion)
printf '%s\n' "$BUILD" | grep -Eq '^[0-9]+[A-Za-z][A-Za-z0-9]+$' || die "cannot identify the current macOS build"
# Rebuild from the current third-party repository. A same-build backup can
# still predate unrelated driver changes and is not a removal candidate.
left=$(find "$EXT" -maxdepth 1 -name '*.kext' 2>/dev/null | while read -r x; do
  n=$(basename "$x" .kext); if [[ " $KEXTS " == *" $n "* ]]; then continue; fi
  [ -d "$x/Contents/MacOS" ] && echo "$x"
  done | wc -l | tr -d ' ')
if [ "$left" = 0 ]; then
  REMOVE_KC=1
else
  RB=$(mktemp -d /var/tmp/nullmoth-remove.XXXX) && [ -n "$RB" ] || die "cannot create the removal preflight directory"
  mkdir -p "$RB/repo" || die "cannot create the removal repository"
  for x in "$EXT"/*.kext; do
    [ -d "$x" ] || continue
    n=$(basename "$x" .kext); if [[ " $KEXTS " == *" $n "* ]]; then continue; fi
    cp -Rp "$x" "$RB/repo/" || die "cannot stage $n for the removal collection"
  done
  NEWKC="$KC.nullmoth-remove-new"; rm -f "$NEWKC" || die "cannot clear the removal candidate"
  kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$RB/repo" -A "$NEWKC" -z >/dev/null 2>&1 || die "kmutil could not rebuild the collection"
  [ -s "$NEWKC" ] || die "kmutil produced no removal collection"
  INS=$(kmutil inspect -a x86_64 -A "$NEWKC" 2>/dev/null) || die "cannot inspect the removal collection"
  printf '%s\n' "$INS" | grep -q com.nullmoth && die "the removal collection still lists the NVIDIA driver"
  CANDIDATE=$NEWKC
fi
# Finish all collection checks before changing installed files.
if [ -z "$RB" ]; then RB=$(mktemp -d /var/tmp/nullmoth-remove.XXXX) && [ -n "$RB" ] || die "cannot create the removal backup"; fi
for k in $KEXTS; do [ ! -e "$EXT/$k.kext" ] || cp -Rp "$EXT/$k.kext" "$RB/" || die "cannot back up $k"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do [ ! -e "$GB/$b" ] || cp -Rp "$GB/$b" "$RB/" || die "cannot back up $b"; done
for f in /Library/NullMoth/kexts /Library/NullMoth/driver-version /Library/NullMoth/update-pending /Library/LaunchDaemons/com.nullmoth.osupdate.plist "$KC"; do
  [ ! -e "$f" ] || cp -Rp "$f" "$RB/" || die "cannot back up the removal state"
done
REMOVING=1
for k in $KEXTS; do rm -rf "$EXT/$k.kext" || die "cannot remove $k"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do rm -rf "$GB/$b" || die "cannot remove $b"; done
rm -rf /Library/NullMoth/kexts /Library/NullMoth/driver-version /Library/NullMoth/update-pending /Library/LaunchDaemons/com.nullmoth.osupdate.plist || die "cannot remove the cached driver or update switcher"
if [ "$REMOVE_KC" = 1 ]; then
  rm -f "$KC" "$KC.nullmoth-install-new" "$KC.nullmoth-update-new" "$KC.nullmoth-remove-new" || die "cannot remove the auxiliary collection"
elif [ "$CANDIDATE" = "$NEWKC" ]; then
  mv -f "$NEWKC" "$KC" || die "cannot publish the removal collection"
else
  cp -p "$CANDIDATE" "$KC" || die "cannot publish the verified backup collection"
fi
if [ -f "$KC" ]; then
  FINAL_INS=$(kmutil inspect -a x86_64 -A "$KC" 2>/dev/null) || die "cannot verify the published removal collection"
  printf '%s\n' "$FINAL_INS" | grep -q com.nullmoth && die "the kernel collection still lists the NVIDIA driver"
fi
REMOVING=0
rm -rf "$RB"
echo "Driver removed. Reboot now: sudo shutdown -r now"
