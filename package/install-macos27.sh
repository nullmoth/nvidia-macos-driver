#!/bin/bash
# Installs the NVIDIA driver onto a macOS 27 system on Intel.
#
# Why this is not install.sh: on macOS 27, kmutil and kernelmanagerd refuse x86_64 collections ("unsupported arch: x86_64"),
# so /Library/Extensions alone loads nothing and kernelmanagerd deletes an auxiliary collection at boot unless it is
# immutable. The measured working layout is:
#   NVRM                              injected by OpenCore (config.plist Kernel>Add, MinKernel 27.0.0, kext in the ESP)
#   NVRMFB NVAccel NVRMAGDC NMXAudio  one auxiliary collection built on macOS 26 against the 27 collections, chflags schg,
#                                     AND the same four bundles in 27's /Library/Extensions (else 0xdc008013 not loadable)
#   firmware                          /usr/share/nvfw on the 27 system volume (NVRM falls back there; /Library is not
#                                     reachable at the point the boot collection starts)
#   Metal plugin + nvmtl-allow.txt    /Library/GPUBundles (the allow list needs WindowServer above the catch-all)
# The ESP cannot be mounted from 27 (diskutil mount hangs) and bless cannot be used from 27, so the full install runs
# from macOS 26 against the mounted 27 volumes. Run from 27 only the parts that need no ESP and no collection build.
#
# Never run `kmutil install --update-all` against the 27 volume: it once rebuilt the boot collection from this
# repository alone and shrank it from 65 MB to 18 MB. This script never calls kmutil install.
#
# usage:
#   install-macos27.sh [--payload DIR] --system SYSTEM_VOL --data DATA_VOL --esp ESP [--config REL] [--dry-run]
#   install-macos27.sh [--payload DIR] --system SYSTEM_VOL --data DATA_VOL --esp ESP --rollback [BACKUP_DIR]
#   install-macos27.sh [--payload DIR] --system / --data / --local27 [--dry-run]    (from 27: no ESP, no collection)
# SYSTEM_VOL and DATA_VOL are the mount points of the 27 system and Data volumes, for example /Volumes/m27 and
# "/Volumes/m27 - Data". The system volume must be mounted read-write for the firmware; if it is not, the script stops
# before changing anything.
set -u
# runs as root: tools resolve through a fixed PATH, never one the caller exported
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
PB=/usr/libexec/PlistBuddy
HERE=$(cd "$(dirname "$0")" && pwd)

AUX_KEXTS="NVRMFB NVAccel NVRMAGDC NMXAudio"   # exactly these; NVRM would put an unpatched copy in the collection
ESP_KEXT=NVRM
GB_ITEMS="NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt"
MINKERNEL=27.0.0
FWVER=615.78.08
CSR_NEEDED=3          # CSR_ALLOW_UNTRUSTED_KEXTS (0x1) | CSR_ALLOW_UNRESTRICTED_FS (0x2)
CSR_SEALED=2048       # CSR_ALLOW_UNAUTHENTICATED_ROOT (0x800): writing the sealed system volume
MIN_DATA_KB=524288
MIN_ESP_KB=20480

DRY=0; ROLLBACK=0; LOCAL27=0; SYS=""; DATA=""; ESP=""; CONFIG_REL=""; BKARG=""; PAYLOAD=""
usage() { sed -n '/^# usage:/,/^# SYSTEM_VOL/p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
while [ "$#" -gt 0 ]; do
  case "$1" in
    --payload) [ "$#" -ge 2 ] && [ -d "$2" ] || { echo "STOP: --payload needs an existing directory" >&2; exit 2; }; PAYLOAD=$(cd "$2" && pwd); shift 2;;
    --system) [ "$#" -ge 2 ] || usage; SYS=$2; shift 2;;
    --data) [ "$#" -ge 2 ] || usage; DATA=$2; shift 2;;
    --esp) [ "$#" -ge 2 ] || usage; ESP=$2; shift 2;;
    --config) [ "$#" -ge 2 ] || usage; CONFIG_REL=$2; shift 2;;
    --dry-run) DRY=1; shift;;
    --local27) LOCAL27=1; shift;;
    --rollback) ROLLBACK=1; shift; if [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; then BKARG=$1; shift; fi;;
    *) usage;;
  esac
done
[ -n "$PAYLOAD" ] || PAYLOAD=$HERE
[ -n "$SYS" ] && [ -n "$DATA" ] || usage
[ -d "$SYS" ] || { echo "STOP: --system $SYS is not a directory" >&2; exit 2; }
[ -d "$DATA/Library" ] || { echo "STOP: --data $DATA has no Library: pass the Data volume of the macOS 27 install" >&2; exit 2; }
if [ "$LOCAL27" = 1 ]; then [ -z "$ESP" ] || { echo "STOP: --local27 cannot touch the ESP" >&2; exit 2; }
else [ -n "$ESP" ] && [ -d "$ESP" ] || { echo "STOP: --esp needs the mounted EFI system partition (mount it from macOS 26)" >&2; exit 2; }; fi
SYS=${SYS%/}; DATA=${DATA%/}; ESP=${ESP%/}; [ -n "$SYS" ] || SYS=/; [ -n "$DATA" ] || DATA=/
# path inside a volume; "/" as a volume root must not produce "//"
vp() { case "$1" in /) printf '%s' "$2";; *) printf '%s%s' "$1" "$2";; esac; }

EXT=$(vp "$DATA" /Library/Extensions); GB=$(vp "$DATA" /Library/GPUBundles)
KCDIR=$(vp "$DATA" /Library/KernelCollections); KC=$KCDIR/AuxiliaryKernelExtensions.kc
NMDIR=$(vp "$DATA" /Library/NullMoth); BKROOT=$NMDIR/backups
RECEIPT=$NMDIR/macos27-install.receipt
FW=$(vp "$SYS" /usr/share/nvfw)
KB=$(vp "$SYS" /System/Library/KernelCollections/BootKernelExtensions.kc)
KS=$(vp "$SYS" /System/Library/KernelCollections/SystemKernelExtensions.kc)
KERNEL=$(vp "$SYS" /System/Library/Kernels/kernel)
SVPLIST=$(vp "$SYS" /System/Library/CoreServices/SystemVersion.plist)

T=""; BK=""; WRITING=0; STAMP=$(date +%Y%m%d-%H%M%S)
say() { echo "   $*"; }
step() { echo "== $*"; }
act() { if [ "$DRY" = 1 ]; then echo "   DRY-RUN would: $*"; fi; }   # a described write; the caller guards the real work
cleanup() { [ -z "$T" ] || rm -rf "$T"; }
trap cleanup EXIT

# ---------- restore from a backup directory (rollback and failure path share this) ----------
restore_item() {  # <backup item (file or dir)> <live path> : live path becomes exactly the backup, or absent if none
  local src=$1 dst=$2
  if [ -e "$src" ]; then rm -rf "$dst" && mkdir -p "$(dirname "$dst")" && ditto "$src" "$dst" || return 1
  else rm -rf "$dst" || return 1; fi
}
restore_all() {  # <backup dir>
  local b=$1 failed=0 k n
  [ -f "$b/COMPLETE" ] || { echo "STOP: $b is not a complete backup" >&2; return 1; }
  local esp config_rel; esp=$(sed -n 's/^esp=//p' "$b/state"); config_rel=$(sed -n 's/^config=//p' "$b/state")
  for k in $AUX_KEXTS; do restore_item "$b/ext/$k.kext" "$EXT/$k.kext" || failed=1; done
  for n in $GB_ITEMS; do restore_item "$b/gb/$n" "$GB/$n" || failed=1; done
  restore_item "$b/ext/nvmtl-allow.txt" "$EXT/nvmtl-allow.txt" || failed=1
  restore_item "$b/nvfw" "$FW" || failed=1
  if [ -e "$KC" ]; then chflags noschg "$KC" || failed=1; fi
  if [ -f "$b/aux.kc" ]; then
    mkdir -p "$KCDIR" && cp -p "$b/aux.kc" "$KC" || failed=1
    if [ "$(sed -n 's/^aux_schg=//p' "$b/state")" = 1 ]; then chflags schg "$KC" || failed=1; fi
  else rm -f "$KC" || failed=1; fi
  if [ -n "$esp" ] && [ -d "$esp" ]; then
    restore_item "$b/esp-kext/$ESP_KEXT.kext" "$esp/$(sed -n 's/^kextsdir=//p' "$b/state")/$ESP_KEXT.kext" || failed=1
    cp -p "$b/config.plist" "$esp/$config_rel" || failed=1
    rm -f "$esp/$(dirname "$config_rel")/.config.plist.nm27" || failed=1   # the staged edit of an interrupted run
  fi
  rm -f "$RECEIPT" || failed=1
  return "$failed"
}
die() {
  echo "STOP: $*" >&2
  if [ "$WRITING" = 1 ]; then
    if restore_all "$BK"; then echo "Everything this run changed was restored from $BK." >&2
    else echo "RESTORE INCOMPLETE: run this script again with --rollback $BK" >&2; fi
  fi
  exit 1
}

# ---------- small checks ----------
sha_of() { shasum -a 256 "$1" | awk '{print $1}'; }
tree_sha() {  # content digest of a directory tree, independent of timestamps and ownership
  (cd "$1" && find . -type f -print | LC_ALL=C sort | while IFS= read -r f; do printf '%s  %s\n' "$(shasum -a 256 < "$f" | awk '{print $1}')" "$f"; done) | shasum -a 256 | awk '{print $1}'
}
same_tree() { [ -d "$1" ] && [ -d "$2" ] && [ "$(tree_sha "$1")" = "$(tree_sha "$2")" ]; }
plist_get() { "$PB" -c "Print :$2" "$1" 2>/dev/null; }

# =====================================================================================================================
# ROLLBACK MODE
# =====================================================================================================================
if [ "$ROLLBACK" = 1 ]; then
  [ "$(id -u)" = 0 ] || { echo "STOP: run as root" >&2; exit 2; }
  if [ -n "$BKARG" ]; then BK=$BKARG
  else BK=$(ls -d "$BKROOT"/macos27-* 2>/dev/null | LC_ALL=C sort | head -n 1)   # the OLDEST: the state before the first install
  fi
  [ -n "$BK" ] && [ -d "$BK" ] || die "no macOS 27 backup found under $BKROOT"
  step "rollback from $BK"
  [ -f "$BK/COMPLETE" ] || die "$BK is not a complete backup"
  (cd "$BK" && shasum -a 256 --strict -c SHA256SUMS --quiet) || die "backup $BK does not match its SHA256SUMS; refusing to restore from it"
  say "backup verified against its SHA256SUMS"
  if [ "$DRY" = 1 ]; then act "restore every item recorded in $BK (extensions, GPUBundles, firmware, aux collection, ESP kext, config.plist)"; exit 0; fi
  restore_all "$BK" || die "rollback incomplete"
  say "restored. Nothing in the ESP backup tar was needed: $BK/esp.tar is kept for a manual full restore."
  exit 0
fi

# =====================================================================================================================
# PREFLIGHT (reads only)
# =====================================================================================================================
step "preflight"
[ "$(id -u)" = 0 ] || [ "$DRY" = 1 ] || die "run as root (use --dry-run to see the plan)"
[ "$(uname -m)" = x86_64 ] || die "this installer is for x86_64 hosts, this is $(uname -m)"
HOSTVER=$(sw_vers -productVersion); HOSTMAJ=${HOSTVER%%.*}
TGTVER=$(plist_get "$SVPLIST" ProductVersion) || die "cannot read $SVPLIST: $SYS is not a macOS system volume"
TGTBUILD=$(plist_get "$SVPLIST" ProductBuildVersion) || die "no ProductBuildVersion in $SVPLIST"
case "$TGTVER" in 27.*) ;; *) die "the target is macOS $TGTVER, this installer is for macOS 27 only (use package/install.sh)";; esac
[ -n "$TGTBUILD" ] || die "empty 27 build string"
say "target macOS $TGTVER build $TGTBUILD, host macOS $HOSTVER"
if [ "$LOCAL27" = 1 ]; then
  [ "$HOSTMAJ" = 27 ] || die "--local27 is for running on macOS 27 itself (this host is $HOSTVER)"
  DO_AUX=0; DO_ESP=0
  say "SKIPPED from 27: ESP edit (the ESP cannot be mounted here) and the auxiliary collection (kmutil cannot build x86_64 here)"
  say "         run the full install from macOS 26 for those"
else
  [ "$HOSTMAJ" = 26 ] || die "the collection build and ESP edit need macOS 26 (kmutil builds x86_64 there); this host is $HOSTVER. From 27 use --local27"
  DO_AUX=1; DO_ESP=1
fi

# csr-active-config is read from NVRAM: the unrestricted-fs and untrusted-kext bits are needed to write another volume's
# /Library and to load unsigned kexts; the unauthenticated-root bit is needed to write the sealed system volume (firmware)
CSRRAW=$(nvram csr-active-config 2>/dev/null | sed -n 's/^csr-active-config[[:space:]]*//p')
case "$CSRRAW" in %[0-9a-fA-F][0-9a-fA-F]%[0-9a-fA-F][0-9a-fA-F]*) ;; *) die "cannot read csr-active-config from NVRAM (got '$CSRRAW')";; esac
CSR=$((16#${CSRRAW:4:2}${CSRRAW:1:2}))
[ $((CSR & CSR_NEEDED)) = "$CSR_NEEDED" ] || die "csr-active-config is $(printf '0x%x' "$CSR"): needs untrusted kexts (0x1) and unrestricted fs (0x2)"
[ $((CSR & CSR_SEALED)) = "$CSR_SEALED" ] || die "csr-active-config is $(printf '0x%x' "$CSR"): needs unauthenticated root (0x800) to write the firmware into the sealed 27 system volume"
say "csr-active-config $(printf '0x%x' "$CSR") ok"

# free space (backups hold the aux collection, bundles and the ESP kext)
FREE=$(df -k "$DATA" | awk 'NR==2{print $4}'); [ "${FREE:-0}" -ge "$MIN_DATA_KB" ] || die "Data volume has ${FREE:-0} KB free, need $MIN_DATA_KB"
if [ "$DO_ESP" = 1 ]; then
  FREE=$(df -k "$ESP" | awk 'NR==2{print $4}'); [ "${FREE:-0}" -ge "$MIN_ESP_KB" ] || die "ESP has ${FREE:-0} KB free, need $MIN_ESP_KB"
fi

# system volume must be writable for the firmware; the probe file is removed at once
if [ "$DRY" = 0 ]; then
  mkdir -p "$(dirname "$FW")" 2>/dev/null && : > "$(dirname "$FW")/.nm27-write-probe" 2>/dev/null && rm -f "$(dirname "$FW")/.nm27-write-probe" ||
    die "the 27 system volume is read-only at $SYS. Mount it read-write first (sealed snapshot volumes are not writable; mount the live system volume writable with authenticated root disabled), then run again. Nothing was changed."
fi

# ESP: locate the OpenCore config. Two candidates and no --config is ambiguous, so stop.
if [ "$DO_ESP" = 1 ]; then
  if [ -n "$CONFIG_REL" ]; then :
  else
    FOUND=""; N=0
    for c in EFI/BOOT/config.plist EFI/OC/config.plist; do [ -f "$ESP/$c" ] && { FOUND=$c; N=$((N+1)); }; done
    [ "$N" = 1 ] || die "found $N OpenCore config.plist candidates in $ESP (EFI/BOOT, EFI/OC); pass --config to choose"
    CONFIG_REL=$FOUND
  fi
  [ -f "$ESP/$CONFIG_REL" ] || die "no OpenCore config at $ESP/$CONFIG_REL"
  plutil -lint "$ESP/$CONFIG_REL" >/dev/null || die "$ESP/$CONFIG_REL is not a valid plist"
  KEXTSDIR=$(dirname "$CONFIG_REL")/Kexts
  [ -d "$ESP/$KEXTSDIR" ] || die "no Kexts directory at $ESP/$KEXTSDIR"
  "$PB" -c "Print :Kernel:Add" "$ESP/$CONFIG_REL" >/dev/null 2>&1 || die "config has no Kernel > Add array"
  say "ESP config $CONFIG_REL, kexts in $KEXTSDIR"
fi

# payload
for k in $AUX_KEXTS $ESP_KEXT; do [ -d "$PAYLOAD/Library/Extensions/$k.kext" ] || die "payload is missing Library/Extensions/$k.kext"; done
for n in $GB_ITEMS; do [ -e "$PAYLOAD/Library/GPUBundles/$n" ] || die "payload is missing Library/GPUBundles/$n"; done
[ -d "$PAYLOAD/Users/Shared/nvfw/nvidia/$FWVER" ] || die "payload firmware has no nvidia/$FWVER directory"
[ -f "$PAYLOAD/SHA256SUMS" ] && { (cd "$PAYLOAD" && shasum -a 256 --strict -c SHA256SUMS --quiet) || die "SHA256SUMS mismatch: re-download the package"; }
ALLOW=$PAYLOAD/Library/GPUBundles/nvmtl-allow.txt
allow_ok() {  # WindowServer must be an exact line, and above the catch-all "*" line
  local w s; w=$(grep -nx 'WindowServer' "$1" | head -n 1 | cut -d: -f1); s=$(grep -nx '\*' "$1" | head -n 1 | cut -d: -f1)
  [ -n "$w" ] && [ -n "$s" ] && [ "$w" -lt "$s" ]
}
allow_ok "$ALLOW" || die "payload nvmtl-allow.txt needs a WindowServer line above the * line"
PAYLOAD_SHA=$(tree_sha "$PAYLOAD")
say "payload $PAYLOAD_SHA"
[ -x "$KERNEL" ] || [ "$DO_AUX" = 0 ] || [ "$DRY" = 1 ] || die "no 27 kernel at $KERNEL"
for f in "$KB" "$KS"; do [ -f "$f" ] || [ "$DO_AUX" = 0 ] || [ "$DRY" = 1 ] || die "missing 27 collection $f"; done

# ---------- the post-install checks; also the "already installed" test ----------
VERIFY_MSG=""
check() { VERIFY_MSG="$*"; }
verify_all() {  # <aux sha expected or empty>; returns 1 and sets VERIFY_MSG on the first failure
  local k n want=$1
  same_tree "$PAYLOAD/Users/Shared/nvfw" "$FW" || { check "firmware at $FW differs from the payload"; return 1; }
  for k in $AUX_KEXTS; do same_tree "$PAYLOAD/Library/Extensions/$k.kext" "$EXT/$k.kext" || { check "$EXT/$k.kext differs from the payload"; return 1; }; done
  for n in $GB_ITEMS; do
    if [ -d "$PAYLOAD/Library/GPUBundles/$n" ]; then same_tree "$PAYLOAD/Library/GPUBundles/$n" "$GB/$n" || { check "$GB/$n differs from the payload"; return 1; }
    else cmp -s "$PAYLOAD/Library/GPUBundles/$n" "$GB/$n" || { check "$GB/$n differs from the payload"; return 1; }; fi
  done
  cmp -s "$ALLOW" "$EXT/nvmtl-allow.txt" || { check "$EXT/nvmtl-allow.txt differs from the payload"; return 1; }
  allow_ok "$GB/nvmtl-allow.txt" || { check "installed allow list lost its WindowServer line"; return 1; }
  sudo -u _windowserver /bin/test -r "$GB/nvmtl-allow.txt" || { check "WindowServer cannot read $GB/nvmtl-allow.txt"; return 1; }
  if [ "$DO_AUX" = 1 ]; then
    [ -f "$KC" ] || { check "no auxiliary collection at $KC"; return 1; }
    [ -z "$want" ] || [ "$(sha_of "$KC")" = "$want" ] || { check "auxiliary collection sha changed"; return 1; }
  fi
  if [ "$DO_ESP" = 1 ]; then
    same_tree "$PAYLOAD/Library/Extensions/$ESP_KEXT.kext" "$ESP/$KEXTSDIR/$ESP_KEXT.kext" || { check "ESP $ESP_KEXT.kext differs from the payload"; return 1; }
    plutil -lint "$ESP/$CONFIG_REL" >/dev/null || { check "config.plist no longer parses"; return 1; }
    local i=0 hits=0 b
    while b=$(plist_get "$ESP/$CONFIG_REL" "Kernel:Add:$i:BundlePath"); do
      case "$b" in
        "$ESP_KEXT.kext")
          hits=$((hits+1))
          [ "$(plist_get "$ESP/$CONFIG_REL" "Kernel:Add:$i:MinKernel")" = "$MINKERNEL" ] || { check "entry $i ($b) MinKernel is not $MINKERNEL"; return 1; }
          [ "$(plist_get "$ESP/$CONFIG_REL" "Kernel:Add:$i:Arch")" = x86_64 ] || { check "entry $i ($b) Arch is not x86_64"; return 1; }
          [ "$(plist_get "$ESP/$CONFIG_REL" "Kernel:Add:$i:Enabled")" = true ] || { check "entry $i ($b) is not Enabled"; return 1; };;
        NVRMFB.kext|NVAccel.kext|NVRMAGDC.kext|NMXAudio.kext)
          [ "$(plist_get "$ESP/$CONFIG_REL" "Kernel:Add:$i:Enabled")" != true ] || { check "config injects $b, which is aux-collection only and would load twice"; return 1; };;
      esac
      i=$((i+1))
    done
    [ "$hits" = 1 ] || { check "config has $hits $ESP_KEXT.kext entries, expected exactly 1"; return 1; }
  fi
  return 0
}

# no-op detection: same payload, same target build, every check green
if [ -f "$RECEIPT" ] && [ "$(sed -n 's/^payload=//p' "$RECEIPT")" = "$PAYLOAD_SHA" ] && [ "$(sed -n 's/^build=//p' "$RECEIPT")" = "$TGTBUILD" ] &&
   verify_all "$(sed -n 's/^aux=//p' "$RECEIPT")"; then
  step "already installed"; say "payload $PAYLOAD_SHA is in place on build $TGTBUILD and every check passes; nothing to do"
  exit 0
fi

# =====================================================================================================================
# PLAN / BACKUPS
# =====================================================================================================================
BK=$BKROOT/macos27-$STAMP
step "backups to $BK"
if [ "$DRY" = 1 ]; then
  act "mkdir $BK and copy: ESP tar, config.plist, ESP $ESP_KEXT.kext, the four /Library/Extensions bundles, GPUBundles items, the extensions allow list, the firmware dir, the aux collection (with its schg flag), then write SHA256SUMS and COMPLETE"
else
  mkdir -p "$BK/ext" "$BK/gb" "$BK/esp-kext" || die "cannot create $BK"
  BKSTATE=$BK/state; : > "$BKSTATE"
  for k in $AUX_KEXTS; do [ ! -e "$EXT/$k.kext" ] || ditto "$EXT/$k.kext" "$BK/ext/$k.kext" || die "backup $k.kext"; done
  [ ! -e "$EXT/nvmtl-allow.txt" ] || cp -p "$EXT/nvmtl-allow.txt" "$BK/ext/nvmtl-allow.txt" || die "backup extensions allow list"
  for n in $GB_ITEMS; do [ ! -e "$GB/$n" ] || ditto "$GB/$n" "$BK/gb/$n" || die "backup $n"; done
  [ ! -d "$FW" ] || ditto "$FW" "$BK/nvfw" || die "backup firmware"
  if [ -f "$KC" ]; then
    cp -p "$KC" "$BK/aux.kc" || die "backup aux collection"
    case "$(stat -f %Sf "$KC")" in *schg*) echo aux_schg=1 >> "$BKSTATE";; *) echo aux_schg=0 >> "$BKSTATE";; esac
  fi
  echo "esp=$ESP" >> "$BKSTATE"; echo "config=$CONFIG_REL" >> "$BKSTATE"; echo "kextsdir=${KEXTSDIR:-}" >> "$BKSTATE"
  echo "build=$TGTBUILD" >> "$BKSTATE"
  if [ "$DO_ESP" = 1 ]; then
    (cd "$ESP" && tar -cf "$BK/esp.tar" .) || die "ESP tar"
    cp -p "$ESP/$CONFIG_REL" "$BK/config.plist" || die "backup config.plist"
    [ ! -e "$ESP/$KEXTSDIR/$ESP_KEXT.kext" ] || ditto "$ESP/$KEXTSDIR/$ESP_KEXT.kext" "$BK/esp-kext/$ESP_KEXT.kext" || die "backup ESP $ESP_KEXT.kext"
    [ -s "$BK/esp.tar" ] || die "ESP tar is empty"
    cmp -s "$ESP/$CONFIG_REL" "$BK/config.plist" || die "config.plist backup differs from the original"
  fi
  (cd "$BK" && find . -type f ! -name SHA256SUMS ! -name COMPLETE -print | LC_ALL=C sort | while IFS= read -r f; do shasum -a 256 "$f"; done > SHA256SUMS) || die "backup manifest"
  (cd "$BK" && shasum -a 256 --strict -c SHA256SUMS --quiet) || die "backup does not verify against its own manifest"
  : > "$BK/COMPLETE"
  say "backup complete and verified: $(wc -l < "$BK/SHA256SUMS" | tr -d ' ') files"
fi

# =====================================================================================================================
# WRITES (after this line a failure restores from the backup)
# =====================================================================================================================
[ "$DRY" = 1 ] || WRITING=1
mkdir_p() { [ "$DRY" = 1 ] || mkdir -p "$@"; }

step "aux collection (build on 26 against the 27 collections; only $AUX_KEXTS)"
AUXSHA=""
if [ "$DO_AUX" = 1 ]; then
  CMD_DESC="kmutil create -n aux -a x86_64 --allow-missing-kdk --build $TGTBUILD -B $KB -S $KS -A <tmp>/aux.kc -k $KERNEL -r <tmp>/repo -x $(for k in $AUX_KEXTS; do printf '%s ' "-b com.nullmoth.$k"; done)--no-variant-extension -z --no-authentication"
  if [ "$DRY" = 1 ]; then act "stage the four kexts into a private repository and run: $CMD_DESC"
  else
    T=$(mktemp -d "${TMPDIR:-/tmp}/nullmoth-install27.XXXXXX") || die "no scratch directory"
    mkdir "$T/repo" || die "scratch repo"
    for k in $AUX_KEXTS; do ditto "$PAYLOAD/Library/Extensions/$k.kext" "$T/repo/$k.kext" || die "stage $k.kext"; done
    chown -R root:wheel "$T/repo" || die "chown staged kexts (kmutil skips kexts not owned by root)"
    BARGS=(); for k in $AUX_KEXTS; do BARGS+=(-b "com.nullmoth.$k"); done
    kmutil create -n aux -a x86_64 --allow-missing-kdk --build "$TGTBUILD" -B "$KB" -S "$KS" -A "$T/aux.kc" -k "$KERNEL" -r "$T/repo" \
      -x "${BARGS[@]}" --no-variant-extension -z --no-authentication >"$T/kmutil.log" 2>&1 || { tail -20 "$T/kmutil.log" >&2; die "kmutil create failed (log above)"; }
    [ -s "$T/aux.kc" ] || die "kmutil produced no collection"
    GOT=$(kmutil inspect -a x86_64 -A "$T/aux.kc" 2>/dev/null) || die "cannot inspect the new collection"
    GOTIDS=$(printf '%s\n' "$GOT" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | LC_ALL=C sort -u | tr '\n' ' ')
    WANTIDS=$(for k in $AUX_KEXTS; do echo "com.nullmoth.$k"; done | LC_ALL=C sort -u | tr '\n' ' ')
    [ "$GOTIDS" = "$WANTIDS" ] || die "the new collection holds [$GOTIDS], expected exactly [$WANTIDS]"
    say "collection built: $(wc -c < "$T/aux.kc" | tr -d ' ') bytes, exactly the four kexts"
    AUXSHA=$(sha_of "$T/aux.kc")
  fi
fi

step "firmware to $FW"
if [ "$DRY" = 1 ]; then act "replace $FW with payload Users/Shared/nvfw"
else
  rm -rf "$FW" && ditto "$PAYLOAD/Users/Shared/nvfw" "$FW" || die "copy firmware"
  same_tree "$PAYLOAD/Users/Shared/nvfw" "$FW" || die "firmware copy does not match the payload"
  chown -R root:wheel "$FW" || die "firmware owner"
fi

step "kext bundles into 27's /Library/Extensions (else 0xdc008013 not loadable)"
for k in $AUX_KEXTS; do
  if [ "$DRY" = 1 ]; then act "replace $EXT/$k.kext (root:wheel)"; continue; fi
  mkdir -p "$EXT" && rm -rf "$EXT/$k.kext" && ditto "$PAYLOAD/Library/Extensions/$k.kext" "$EXT/$k.kext" || die "copy $k.kext"
  chown -R root:wheel "$EXT/$k.kext" || die "owner of $k.kext"
  same_tree "$PAYLOAD/Library/Extensions/$k.kext" "$EXT/$k.kext" || die "$k.kext copy does not match the payload"
done

step "Metal plugin and allow list"
if [ "$DRY" = 1 ]; then act "replace $GB items ($GB_ITEMS) and write $EXT/nvmtl-allow.txt (the plugin reads both)"
else
  mkdir -p "$GB" || die "GPUBundles"
  for n in $GB_ITEMS; do rm -rf "$GB/$n" && ditto "$PAYLOAD/Library/GPUBundles/$n" "$GB/$n" || die "copy $n"; done
  cp "$ALLOW" "$EXT/nvmtl-allow.txt" || die "extensions allow list"
  chown -R root:wheel "$GB" "$EXT/nvmtl-allow.txt" || die "GPUBundles owner"
  chmod 644 "$GB/nvmtl-allow.txt" "$EXT/nvmtl-allow.txt" || die "allow list mode"
  # WindowServer runs as _windowserver and must read the allow list
  sudo -u _windowserver /bin/test -r "$GB/nvmtl-allow.txt" || die "WindowServer cannot read $GB/nvmtl-allow.txt"
  allow_ok "$GB/nvmtl-allow.txt" || die "installed allow list lost its WindowServer line"
  for n in $GB_ITEMS; do [ ! -d "$PAYLOAD/Library/GPUBundles/$n" ] || same_tree "$PAYLOAD/Library/GPUBundles/$n" "$GB/$n" || die "$n copy does not match the payload"; done
fi

step "install the auxiliary collection (noschg, replace, schg)"
if [ "$DO_AUX" = 1 ]; then
  if [ "$DRY" = 1 ]; then act "chflags noschg $KC (if present); copy the new collection beside it and mv it over; chflags schg $KC; compare sha"
  else
    mkdir -p "$KCDIR" || die "KernelCollections dir"
    if [ -e "$KC" ]; then chflags noschg "$KC" || die "chflags noschg"; fi
    cp "$T/aux.kc" "$KC.new" && mv -f "$KC.new" "$KC" || die "replace the auxiliary collection"
    chflags schg "$KC" || die "chflags schg (kernelmanagerd would delete the collection at boot)"
    [ "$(sha_of "$KC")" = "$AUXSHA" ] || die "installed collection sha does not match the built one"
    say "collection installed, immutable, sha $AUXSHA"
  fi
fi

step "ESP: $ESP_KEXT.kext and Kernel > Add entry"
if [ "$DO_ESP" = 1 ]; then
  CFG=$ESP/$CONFIG_REL
  if [ "$DRY" = 1 ]; then act "copy $ESP_KEXT.kext to $ESP/$KEXTSDIR, edit a copy of $CONFIG_REL with PlistBuddy (entry $ESP_KEXT.kext: Arch x86_64, Enabled true, MinKernel $MINKERNEL; update in place if present), plutil -lint, re-read, mv over"
  else
    rm -rf "$ESP/$KEXTSDIR/$ESP_KEXT.kext" && ditto "$PAYLOAD/Library/Extensions/$ESP_KEXT.kext" "$ESP/$KEXTSDIR/$ESP_KEXT.kext" || die "copy $ESP_KEXT.kext to the ESP"
    same_tree "$PAYLOAD/Library/Extensions/$ESP_KEXT.kext" "$ESP/$KEXTSDIR/$ESP_KEXT.kext" || die "ESP $ESP_KEXT.kext does not match the payload"
    EXE=$(plist_get "$PAYLOAD/Library/Extensions/$ESP_KEXT.kext/Contents/Info.plist" CFBundleExecutable) || EXE=$ESP_KEXT
    NEWCFG=$(dirname "$CFG")/.config.plist.nm27
    cp -p "$CFG" "$NEWCFG" || die "stage config.plist"
    IDX=""; i=0; DUP=0
    while b=$(plist_get "$NEWCFG" "Kernel:Add:$i:BundlePath"); do
      if [ "$b" = "$ESP_KEXT.kext" ]; then [ -z "$IDX" ] && IDX=$i || DUP=1; fi
      i=$((i+1))
    done
    [ "$DUP" = 0 ] || die "config already has more than one $ESP_KEXT.kext entry; fix it by hand, nothing was edited"
    if [ -z "$IDX" ]; then IDX=$i; "$PB" -c "Add :Kernel:Add:$IDX dict" "$NEWCFG" || die "add entry"; fi
    put() {  # <key> <type> <value>
      "$PB" -c "Set :Kernel:Add:$IDX:$1 $3" "$NEWCFG" >/dev/null 2>&1 || "$PB" -c "Add :Kernel:Add:$IDX:$1 $2 $3" "$NEWCFG" || die "set $1"
    }
    put Arch string x86_64; put BundlePath string "$ESP_KEXT.kext"; put Comment string "NullMoth driver (macOS 27 only)"
    put Enabled bool true; put ExecutablePath string "Contents/MacOS/$EXE"; put MinKernel string "$MINKERNEL"
    put PlistPath string Contents/Info.plist
    # MaxKernel stays empty (no upper bound); an existing value is left alone
    plist_get "$NEWCFG" "Kernel:Add:$IDX:MaxKernel" >/dev/null || "$PB" -c "Add :Kernel:Add:$IDX:MaxKernel string" "$NEWCFG" || die "set MaxKernel"
    # The four aux-collection kexts must not also come from OpenCore: an older copy injected at boot either shadows the
    # collection's build or fails to link, and on the measured 27 box the collection's copies were the ones running while
    # the config still listed stale ESP copies. Their entries are switched off (files left in place); the config.plist
    # backup puts them back on --rollback.
    i=0
    while b=$(plist_get "$NEWCFG" "Kernel:Add:$i:BundlePath"); do
      case "$b" in
        NVRMFB.kext|NVAccel.kext|NVRMAGDC.kext|NMXAudio.kext)
          if [ "$(plist_get "$NEWCFG" "Kernel:Add:$i:Enabled")" = true ]; then
            "$PB" -c "Set :Kernel:Add:$i:Enabled false" "$NEWCFG" || die "disable entry $i ($b)"
            say "OpenCore entry $i ($b) switched off: it loads from the auxiliary collection"
          fi;;
      esac
      i=$((i+1))
    done
    plutil -lint "$NEWCFG" >/dev/null || die "edited config.plist does not parse"
    mv -f "$NEWCFG" "$CFG" || die "replace config.plist"
  fi
fi

# =====================================================================================================================
# VERIFY + RECEIPT
# =====================================================================================================================
if [ "$DRY" = 1 ]; then step "dry run complete"; say "nothing was changed"; exit 0; fi
step "verify"
verify_all "$AUXSHA" || die "post-install check failed: $VERIFY_MSG"
say "every check passed"
{ echo "payload=$PAYLOAD_SHA"; echo "build=$TGTBUILD"; echo "aux=$AUXSHA"; echo "backup=$BK"; echo "date=$STAMP"; } > "$RECEIPT.new" && mv -f "$RECEIPT.new" "$RECEIPT" || die "write receipt"
WRITING=0
step "done"
say "backup: $BK   (undo with --rollback)"
say "reboot from the macOS 27 boot entry; nothing here changes the default boot target"
