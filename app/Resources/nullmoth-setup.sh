#!/bin/bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin
B=7C436110-AB2A-4BBB-A880-FE41995C9F82
# ipc_control_port_options=0: with AMFI relaxed, Firefox and other hardened apps crash at launch on their task control
# port (10-10 chat: Firefox "instant crash", fixed by this argument); OpenCore Legacy Patcher sets it for the same reason.
WANT_ARGS="nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80 ipc_control_port_options=0"
# The NVIDIA display card's PCI device id (4 hex digits), read from the registry.
# Prefer a VGA controller (class 0300); fall back to a 3D controller (0302) only when no NVIDIA VGA card is present.
# A muxless Turing laptop dGPU (no display outputs, e.g. a Quadro T500/GA107M) enumerates as 0302, so matching only
# 0300 left NVDEV empty and that card never got nvrmsettle=10000 (#119). A compute card beside a real display card
# still does not decide: the 0300 card wins.
nv_device_id() {
  ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | LC_ALL=C awk '
    function emit() { if (v && d != "") { id = substr(d,3,2) substr(d,1,2)
                                          if (c == 300 && vga == "") vga = id
                                          else if (c == 302 && d3d == "") d3d = id } }
    /\+-o / { emit(); v=0; c=0; d="" }
    /"vendor-id" = <de100000>/ { v=1 }
    /"class-code" = <00000300>/ { c=300 }
    /"class-code" = <00000302>/ { c=302 }
    /"device-id" = </ { if (match($0, /<[0-9a-f]+>/)) d=substr($0, RSTART+1, 4) }
    END { emit(); print (vga != "" ? vga : d3d) }'
}
# Turing (TU102/104/106/117 = 1E00-1FFF, TU116 = 2180-21FF) needs longer for GSP to settle before the display arms:
# a GTX 1660 SUPER stuck at boot on the 0.5 s default and started with nvrmsettle=10000, as an RTX 2070 SUPER had
# (Discord 10-11, after 1.12). Only added when the user has not chosen a value.
NVDEV=$(nv_device_id)
case "$NVDEV" in 1[eEfF]??|21[89a-fA-F]?) TURING=1;; *) TURING=0;; esac
[ "$TURING" = 1 ] && WANT_ARGS="$WANT_ARGS nvrmsettle=10000"
DROP_ARGS="nv_disable=1 -wegnoegpu"          # both hide the NVIDIA card from macOS
SIP_BITS=$((0x0A43))
TOOL_NAME="1401: Remove NVIDIA driver"; TOOL_FILE=NullMothSafe.efi
# the same tool with Arguments "once": macOS starts one time without the driver (to reach Recovery, or to get past a bad
# start), and the driver is back after that - OpenCore rewrites boot-args at every start. Asked for on Discord 10-11.
ONCE_NAME="1401: Start once without NVIDIA driver"
# Optional cosmetic EFI driver; default installations keep the standard boot display.
# Reported firmware hangs make this an explicit opt-in rather than an installation prerequisite.
LOGO=off; LOGO_FILE=NullMothLogo.efi
ST=${NULLMOTH_STATE_DIR:-/Library/NullMoth}; STATE=$ST/state
AGENT=/Library/LaunchAgents/com.nullmoth.crashcheck.plist
RECOVER=/Library/LaunchDaemons/com.nullmoth.recover.plist
KNOBS=(); PROFILE=""; COLLECT=""; UPD=""; VERB=""; PKG=""; SHA=""; CFG=""; EFI=auto; DRY=0; REMOVE=0; USBMAP=""; TOOL=""; APPBIN=""; MOUNTED=""; T=""; PKGDIR=""
while [ $# -gt 0 ]; do case "$1" in
  --pkg) PKG=$2; shift;; --sha) SHA=$2; shift;; --config) CFG=$2; shift;; --efi) EFI=$2; shift;;
  --tool) TOOL=$2; shift;; --usbmap) USBMAP=$2; shift;; --app) APPBIN=$2; shift;; --dry) DRY=1;; --remove) REMOVE=1;; --verbose) VERB=$2; shift;; --update) UPD=$2; shift;; --collect-logs) COLLECT=$2; shift;;
  --knob) KNOBS+=("$2"); shift;; --profile) PROFILE=$2; shift;; --logo) LOGO=$2; shift;; --set-logo) SETLOGO=$2; shift;; --gpu-test) GPUTEST=$2; shift;; --backlight) BACKLIGHT=$2; shift;;
  *) echo "STOP unknown option $1"; echo "RESULT stop"; exit 2;; esac; shift; done
step() { echo "STEP $*"; }; ok() { echo "OK $*"; }; note() { echo "NOTE $*"; }
KT=""
cleanup() { for d in $MOUNTED; do diskutil unmount "$d" >/dev/null 2>&1; done; [ -n "$T" ] && rm -rf "$T"; [ -n "${KT:-}" ] && rm -rf "$KT"; [ -n "${PKGDIR:-}" ] && rm -rf "$PKGDIR"; }
stop() { echo "STOP $*"; cleanup; echo "RESULT stop"; exit 1; }
[ "$(id -u)" = 0 ] || { echo "STOP needs administrator rights"; echo "RESULT stop"; exit 1; }
[ "$UPD" = finish ] && [ ! -f "$ST/update-pending" ] && { echo "RESULT ok"; exit 0; }
has() { plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }
get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }
mnt() { diskutil info "$1" 2>/dev/null | awk -F': *' '/Mount Point/{print $2}'; }
ro_why() {      # $1 = a file; explains a failed write when macOS mounted its partition read-only
  # A 1.14.7 setup log showed "cp: ... Read-only file system", and the stop said only "could not back up". macOS mounts a
  # FAT partition read-only when its repair check fails (left unclean by Windows Fast Startup or a forced power-off) or
  # when the stick is write-locked.
  local m; m=$(df -P "$(dirname "$1")" 2>/dev/null | awk 'NR==2{print $NF}')
  [ -n "$m" ] && mount | grep -F " on $m (" | grep -q "read-only" || return 0
  echo " - macOS mounted $m read-only, so nothing on it can be changed. Run First Aid on that partition in Disk Utility (if it was left unclean by Windows, turn off Fast Startup there and shut Windows down once), check the stick's write-lock switch, then try again"; }
mount_efi() {   # $1 = device or partition UUID; sets MOUNT_POINT in this shell
  local mp; mp=$(mnt "$1")
  if [ -z "$mp" ]; then diskutil mount "$1" >/dev/null 2>&1 || return 1; MOUNTED="$MOUNTED $1"; mp=$(mnt "$1"); fi
  MOUNT_POINT=$mp; [ -n "$MOUNT_POINT" ]; }
owned_amfi_from_prior_record() (
  # Inherit only explicitly recorded shared-token ownership from the unchanged
  # configuration on the same identified partition. Legacy or changed records
  # do not establish ownership. Read the record without altering this shell.
  local record=$1 expected_uuid=$2 expected_path=$3 expected_ocrel=$4 expected_sha=$5 expected_args=$6 a
  [ -n "$expected_uuid" ] && [ -f "$record" ] || exit 0
  EFI_UUID=""; CONFIG_PATH=""; OCREL=""; CONFIG_SHA_AFTER=""; ADDED_ARGS=""
  . "$record" >/dev/null 2>&1 || exit 0
  [ "$EFI_UUID" = "$expected_uuid" ] && [ "$CONFIG_PATH" = "$expected_path" ] &&
    [ "$OCREL" = "$expected_ocrel" ] && [ "$CONFIG_SHA_AFTER" = "$expected_sha" ] || exit 0
  for a in $ADDED_ARGS; do
    case "$a" in amfi=0x80|amfi_get_out_of_my_way=0x1)
      case " $expected_args " in *" $a "*) printf '%s\n' "$a";; esac;;
    esac
  done
)
ocrel_in() {    # $1 = mount point; prints where OpenCore lives on it (EFI/OC, or EFI/BOOT when OpenCore.efi is BOOTx64.efi)
  # A config made for another Mac model (a rescue stick, another machine's EFI) is never this Mac's: OpenCore sets the
  # model macOS reports from PlatformInfo, so the config that started this Mac names hw.model. (Measured 10-07: the boot-path
  # variable was absent and a Mac Pro rescue config on a second stick was edited instead of the 1401 stick's.)
  local d m; m=$(sysctl -n hw.model 2>/dev/null)
  for d in EFI/OC EFI/BOOT; do
    [ -f "$1/$d/config.plist" ] || continue
    [ "$d" = EFI/BOOT ] && [ ! -d "$1/$d/Kexts" ] && continue
    local c="$1/$d/config.plist" cm="" k
    # the model this config gives the Mac: SMBIOS spoof (Generic / SMBIOS / DataHub), else OCLP's own record of a real Mac
    for k in PlatformInfo.Generic.SystemProductName PlatformInfo.SMBIOS.SystemProductName PlatformInfo.DataHub.SystemProductName \
             NVRAM.Add.4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102.OCLP-Model; do
      cm=$(plutil -extract "$k" raw -o - "$c" 2>/dev/null); [ -n "$cm" ] && break; done
    [ -n "$m" ] && [ -n "$cm" ] && [ "$cm" != "$m" ] && continue
    echo $d; return; done; }
# Kexts the driver's OpenCore setup needs, pinned to the same files 1401 builds with (p1401/mirror.json).
kurl() { case $1 in
  Lilu.kext) echo "https://github.com/dortania/build-repo/releases/download/Lilu-0515f40/Lilu-1.7.3-RELEASE.zip 261aebb9dc83adb6515c96405653f53c3c50b09c89babdb91fb41f7b33a1cd2a";;
  AMFIPass.kext) echo "https://github.com/dortania/OpenCore-Legacy-Patcher/raw/main/payloads/Kexts/Acidanthera/AMFIPass-v1.4.1-RELEASE.zip 07b266145906db41f4b13a7938fbb173ea28888cc1fa65f84417f8820adc961e";;
  USBToolBox.kext) echo "https://github.com/USBToolBox/kext/releases/download/1.2.0/USBToolBox-1.2.0-RELEASE.zip c315a3a5acfd496dd97d0d19b4fbd1d487103d2fd541c5651583d4c9cebcfe07";;
  RestrictEvents.kext) echo "https://github.com/dortania/build-repo/releases/download/RestrictEvents-9265e78/RestrictEvents-1.1.7-RELEASE.zip e5a8c15adeba9251be2709899214fe7fd847f6d38fca6c09adb65e9b2af64f0a";;
esac; }
# Downloads $1 into $KT/$1.x (checked against its SHA-256) before anything is changed; stops on any failure. $2 = where it goes.
fetch_kext() {
  local k=$1 u s
  [ -n "$KT" ] || KT=$(mktemp -d) || stop "could not create a download folder"
  read -r u s <<<"$(kurl $k)"
  # GitHub first, then the same bytes from nullmothsystems.com/mirror/<sha256> (the mirror 1401 uses). Four 1.14.7 setups
  # stopped on "curl: (56) Recv failure: Operation timed out" at raw.githubusercontent.com, which some networks block.
  # The SHA-256 below is checked whichever source answered.
  curl -fsSL --connect-timeout 20 --max-time 120 --retry 2 --retry-delay 3 -o "$KT/$k.zip" "$u" \
    || curl -fsSL --connect-timeout 20 --max-time 180 --retry 3 --retry-delay 3 -o "$KT/$k.zip" "https://nullmothsystems.com/mirror/$s" \
    || stop "$k could not be downloaded ($u, or the NullMoth mirror). Check the internet connection and try again, or put $k in $2"
  [ "$(shasum -a 256 "$KT/$k.zip" | awk '{print $1}')" = "$s" ] || stop "$k download did not match its SHA-256; nothing was changed. Try again"
  ditto -x -k "$KT/$k.zip" "$KT/$k.x" && [ -f "$KT/$k.x/$k/Contents/Info.plist" ] || stop "$k download could not be unpacked; nothing was changed"
}
booted_part() { # the partition OpenCore started this Mac from, read from OpenCore's boot-path variable
  local bp u d
  bp=$(nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:boot-path 2>/dev/null | cut -f2-)
  u=$(printf '%s' "$bp" | grep -oiE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | head -1)
  [ -n "$u" ] || return 1
  for d in $(diskutil list | grep -oE 'disk[0-9]+s[0-9]+$' | sort -u); do
    diskutil info "$d" 2>/dev/null | grep -qi "Partition UUID: *$u" && { echo "$d"; return 0; }; done
  return 1; }
tool_index() {  # index of our remove entry in Misc.Tools, or nothing (the once entry shares the file, not the Arguments)
  local i=0 p
  while p=$(plutil -extract Misc.Tools.$i.Path raw -o - "$C" 2>/dev/null); do
    [ "$p" = "$TOOL_FILE" ] && [ "$(plutil -extract Misc.Tools.$i.Arguments raw -o - "$C" 2>/dev/null)" != once ] && { echo $i; return; }
    i=$((i+1)); done; }
once_index() {  # index of the start-once entry in Misc.Tools, or nothing
  local i=0 p
  while p=$(plutil -extract Misc.Tools.$i.Path raw -o - "$C" 2>/dev/null); do
    [ "$p" = "$TOOL_FILE" ] && [ "$(plutil -extract Misc.Tools.$i.Arguments raw -o - "$C" 2>/dev/null)" = once ] && { echo $i; return; }
    i=$((i+1)); done; }
picker_edits() {  # $1 = config.plist. Prints one "key|type|value" edit per setting that is keeping the "Remove NVIDIA
  # driver" boot entry out of reach, so the way back is always visible even on a Mac the driver stopped lighting a screen
  # on. The caller adds each to EDITS and shows a CHANGE line; nothing is printed for a setting already correct.
  local c=$1 tool=${TOOL_FILE:-NullMothSafe.efi} i=0 p
  # The picker must appear on its own. With ShowPicker off, OpenCore boots the default entry straight away and the user
  # has to know to hold a key to reach the Remove entry - which a user staring at a black desktop does not (30 reports,
  # N10). OCLP builds leave it on; some configs turn it off.
  [ "$(plutil -extract Misc.Boot.ShowPicker raw -o - "$c" 2>/dev/null)" = true ] || echo "Misc.Boot.ShowPicker|-bool|true"
  # Apple's own picker (PickerMode Apple) lists only bootable OSes, never OpenCore tools, so the Remove entry cannot show
  # in it at all ("remove nvidia driver doesnt exist even when i press space"). Builtin and External (OpenCanopy) both
  # list tools; only Apple is forced off, to the always-available Builtin text picker.
  [ "$(plutil -extract Misc.Boot.PickerMode raw -o - "$c" 2>/dev/null)" = Apple ] && echo "Misc.Boot.PickerMode|-string|Builtin"
  # HideAuxiliary keeps auxiliary entries hidden until Space is pressed. Several users recovered only after setting it
  # false by hand (chat: "Make HideAuxiliary false", "god knows why hideauxilary is set to true"). The Remove and
  # start-once entries are forced non-auxiliary below so they show regardless, but turning the hiding off is the setting
  # people actually reach for, and it also unhides Reset NVRAM and Recovery for the same user who is already stuck.
  [ "$(plutil -extract Misc.Boot.HideAuxiliary raw -o - "$c" 2>/dev/null)" = true ] && echo "Misc.Boot.HideAuxiliary|-bool|false"
  # Both NullMoth tool entries must be shown (Auxiliary false) and readable (TextMode true) whatever an earlier install or
  # a 1401 base config set. An entry left Auxiliary true is hidden under HideAuxiliary even when its file is present.
  while p=$(plutil -extract Misc.Tools.$i.Path raw -o - "$c" 2>/dev/null); do
    if [ "$p" = "$tool" ]; then
      [ "$(plutil -extract Misc.Tools.$i.Auxiliary raw -o - "$c" 2>/dev/null)" = false ] || echo "Misc.Tools.$i.Auxiliary|-bool|false"
      [ "$(plutil -extract Misc.Tools.$i.TextMode raw -o - "$c" 2>/dev/null)" = true ] || echo "Misc.Tools.$i.TextMode|-bool|true"
    fi
    i=$((i+1)); done; }
logo_index() {  # index of the boot-logo driver in UEFI.Drivers, or nothing
  local i=0 p
  while p=$(plutil -extract UEFI.Drivers.$i.Path raw -o - "$C" 2>/dev/null); do
    [ "$p" = "$LOGO_FILE" ] && { echo $i; return; }; i=$((i+1)); done; }
csr_of() { local h; h=$(get NVRAM.Add.$B.csr-active-config | base64 -D 2>/dev/null | xxd -p)
  [ ${#h} = 8 ] && echo $(( 0x${h:6:2}${h:4:2}${h:2:2}${h:0:2} )) || echo 0; }
csr_data() { printf '%08x' "$1" | sed -E 's/(..)(..)(..)(..)/\4\3\2\1/' | xxd -r -p | base64; }
boot_esp() {    # the EFI partition on the physical disk this macOS runs from
  local c p w
  c=$(diskutil info / | awk -F': *' '/Part of Whole/{print $2}')
  p=$(diskutil info "$c" 2>/dev/null | awk -F': *' '/APFS Physical Store/{print $2}'); [ -n "$p" ] || p=$c
  w=$(diskutil info "$p" | awk -F': *' '/Part of Whole/{print $2}')
  diskutil list "$w" | awk '/ EFI /{print $NF}' | grep -E '^disk[0-9]+s[0-9]+$' | head -1; }
on_usb() {      # true when the partition is on an external or removable disk (a 1401 stick)
  diskutil info "$1" 2>/dev/null | grep -q -E 'Removable Media: *(Removable|Yes)|Device Location: *External|Protocol: *USB'; }
bidx() { local i=0 p; while p=$(plutil -extract Kernel.Block.$i.Identifier raw -o - "$C" 2>/dev/null); do [ "$p" = com.apple.iokit.IONDRVSupport ] && { echo $i; return; }; i=$((i+1)); done; }

# Every log query has its own time cap. Measured 10-12 on a 15.8.1 machine: three "--last 3d" queries took 85 s, 120 s
# and over 230 s, so the 180 s collection limit killed the job before the kernel log was written (36 of 94 reports since
# 1.14.0 arrived without it). A capped query keeps what it printed and says that it was cut.
lshow() {  # <seconds> <log show arguments...>
  local secs=$1 p w rc; shift
  /usr/bin/log show "$@" & p=$!
  ( sleep "$secs" >/dev/null 2>&1; kill "$p" 2>/dev/null && echo "(log query stopped after ${secs} s)" ) & w=$!
  wait "$p"; rc=$?
  kill "$w" 2>/dev/null; wait "$w" 2>/dev/null
  return $rc
}
# Keeps the first $1 and the last $2 lines of its input, with a marker between: a boot's start (where a GPU driver
# says whether it started) and its end (what the user saw last). WAS a plain tail: on an RX 7900 XTX + RTX 2060 machine
# Per-allocation lines filled the 512 KB upload, and the test driver's start-up lines were gone.
headtail() { awk -v h="$1" -v t="$2" 'NR <= h { print; next } { r[NR % t] = $0 }
  END { if (NR > h) { s = (NR - t > h) ? NR - t + 1 : h + 1; if (s > h + 1) printf "... %d lines left out ...\n", s - h - 1
        for (i = s; i <= NR; i++) print r[i % t] } }'; }
# Copy newest files without splitting volume or file names at whitespace.
collect_recent_logs() {
  local prefix=$1 limit=$2 f i; shift 2
  local recent=()
  for f in "$@"; do
    [ -f "$f" ] || continue
    i=0
    while [ $i -lt ${#recent[@]} ] && [ ! "$f" -nt "${recent[$i]}" ]; do i=$((i+1)); done
    [ $i -lt "$limit" ] || continue
    recent=("${recent[@]:0:$i}" "$f" "${recent[@]:$i}")
    recent=("${recent[@]:0:$limit}")
  done
  for f in "${recent[@]}"; do
    if cp "$f" "$COLLECT/$prefix-$(basename "$f")"; then n=$((n+1))
    else note "could not copy diagnostic log: $f"; collection_errors=$((collection_errors+1)); fi
  done
}

if [ -n "$COLLECT" ]; then
  trap cleanup EXIT HUP INT TERM
  # "Send logs": gather what only root can read into $COLLECT for the app to upload. Read-only on the system: it
  # copies files and prints state, and unmounts any EFI partition it mounted. Every OpenCore partition is checked,
  # sticks included, for OpenCore's own log (opencore-*.txt) and macOS panics it saved (panic-*.txt).
  mkdir -p "$COLLECT" || { echo "RESULT stop"; exit 1; }
  # "Record flicker": 20 s of display timing taken while the user sees the flicker, before anything else runs. The
  # accelerator's zero-copy flip counters and the framebuffer's commit/flip properties every 0.25 s, the driver's
  # kernel messages, WindowServer's CPU and the refresh rate. Flicker never shows on a 60 Hz single-display test Mac,
  # so the users' own machines are the measurement.
  if [ "${NULLMOTH_SAMPLE:-}" = flicker ]; then
    # mark the moment in the display tracker, so its timeline shows where the user saw the flicker
    [ -d /Library/NullMoth/display-trace ] && echo "== USER: flicker seen now $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> /Library/NullMoth/display-trace/trace.txt
    { echo "== display timing sample, $(date -u +%Y-%m-%dT%H:%M:%SZ), 20 s at 0.25 s"
      system_profiler SPDisplaysDataType 2>/dev/null | grep -E "Resolution|Refresh|UI Looks|Display Type|Connection Type|Online"
      /usr/bin/log stream --style compact --predicate 'sender == "NVRMFB" OR sender == "NVRM" OR sender == "NVAccel"' > "$COLLECT/.flicker-kmsg" 2>&1 &
      lp=$!
      i=0; while [ $i -lt 80 ]; do
        echo "-- t=$(( i * 250 ))ms"
        sysctl debug 2>/dev/null | grep -E "nvaccel_(iop|flip|vbl|swap|present|async|crc)|nvrmfb" | tr '\n' ' '; echo
        ioreg -r -c NVRMNVDAFramebuffer -d 1 2>/dev/null | grep -E '"NVRM(RejectedCommits|Flip|Vbl|Present)|"IOFBCurrentPixelClock' | tr -s ' ' | tr '\n' ' '; echo
        ps -A -o %cpu=,comm= 2>/dev/null | grep -E "WindowServer$" | head -1
        sleep 0.25; i=$((i + 1))
      done
      kill "$lp" 2>/dev/null; wait "$lp" 2>/dev/null
      echo; echo "== driver kernel messages during the sample"; tail -c 200000 "$COLLECT/.flicker-kmsg"; rm -f "$COLLECT/.flicker-kmsg"
    } > "$COLLECT/display-flicker-sample.txt" 2>&1
  fi
  # Plain file copies first: the collection is stopped at 360 s and the log queries below can use most of it. 72 1.15
  # uploads hit that limit and arrived without the panics, crash reports, EFI configs and the previous boot, which
  # were collected last.
  n=0; collection_errors=0
  collect_recent_logs macos 3 /Library/Logs/DiagnosticReports/*.panic /Library/Logs/DiagnosticReports/Retired/*.panic
  # the display tracker (every boot, from the first second): current and previous file, newest 3 MB of each
  for f in trace.txt trace.txt.1 kernel.txt kernel.txt.1 ws.txt ws.txt.1; do
    [ -f "/Library/NullMoth/display-trace/$f" ] && tail -c 3145728 "/Library/NullMoth/display-trace/$f" > "$COLLECT/display-$f"
  done
  # WindowServer can fail outside a driver frame. Include the complete recent reports
  # during an explicit Send logs request; automatic crash notifications stay selective.
  collect_recent_logs macos 3 /Library/Logs/DiagnosticReports/WindowServer*.ips /Library/Logs/DiagnosticReports/Retired/WindowServer*.ips
  collect_recent_logs macos 1 /Library/Logs/DiagnosticReports/firefox*.ips /Library/Logs/DiagnosticReports/Firefox*.ips /Library/Logs/DiagnosticReports/Retired/firefox*.ips /Library/Logs/DiagnosticReports/Retired/Firefox*.ips
  collect_recent_logs macos 1 /Library/Logs/DiagnosticReports/plugin-container*.ips /Library/Logs/DiagnosticReports/Retired/plugin-container*.ips
  collect_recent_logs macos 1 /Library/Logs/DiagnosticReports/Blender*.ips /Library/Logs/DiagnosticReports/Retired/Blender*.ips
  for d in $(diskutil list | awk '/ EFI | DOS_FAT_32 | Windows_FAT_32 | Microsoft Basic Data /{print $NF}' | grep -E '^disk[0-9]+s[0-9]+$'); do
    mount_efi "$d" || continue; mp=$MOUNT_POINT
    collect_recent_logs "$d" 3 "$mp"/opencore-*.txt
    collect_recent_logs "$d" 5 "$mp"/panic-*.txt
    # The EFI itself: most machines that fail run their own OpenCore config, and without it every fix was a
    # guess from symptoms. The config goes up with the machine's identity removed (serial, board serial, UUID, ROM),
    # plus the list of kexts, drivers and ACPI files beside it, so the working and the broken EFIs can be compared.
    # The live config and its two newest backups only: every install leaves a backup, and 35 of them on one test machine
    # filled the report's 48 file slots before the panics, crash reports and GPU logs were reached.
    cfs=("$mp"/EFI/OC/config.plist)   # a volume name can hold spaces ("NO NAME"): read paths line by line, never split them
    while IFS= read -r cf; do cfs+=("$cf"); done < <(ls -t "$mp"/EFI/OC/config.plist.nullmoth-* 2>/dev/null | head -n 2)
    for cf in "${cfs[@]}"; do
      [ -f "$cf" ] || continue
      out="$COLLECT/$d-efi-$(basename "$cf").txt"
      cp "$cf" "$out.plist" || { collection_errors=$((collection_errors+1)); continue; }
      for k in SystemSerialNumber MLB SystemUUID ROM; do plutil -remove "PlatformInfo.Generic.$k" "$out.plist" >/dev/null 2>&1; done
      plutil -convert xml1 -o "$out" "$out.plist" 2>/dev/null || mv "$out.plist" "$out"; rm -f "$out.plist"
      n=$((n+1))
    done
    [ -d "$mp/EFI/OC" ] && { echo "== $d EFI/OC"; (cd "$mp/EFI/OC" && ls -1 Kexts Drivers ACPI Tools 2>/dev/null; ls -la OpenCore.efi 2>/dev/null); } >> "$COLLECT/$d-efi-files.txt"
  done
  for f in "$ST"/*.log "$ST/state"; do [ -f "$f" ] && cp "$f" "$COLLECT/driver-$(basename "$f").txt"; done
  { echo "macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion))   model $(sysctl -n hw.model)"
    echo "boot-args: $(nvram boot-args 2>/dev/null | cut -f2-)"; echo "SIP: $(csrutil status 2>/dev/null)"
    echo; echo "== install record"; cat "$ST/state" 2>/dev/null || echo "(none recorded)"
    echo; echo "== NullMoth kexts loaded"; kmutil showloaded --list-only 2>/dev/null | grep -i nullmoth
    echo; echo "== auxiliary collection"; kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null | grep -i nullmoth
    echo; echo "== driver files"; ls -la /Library/Extensions/NV*.kext /Library/GPUBundles 2>/dev/null
    echo; echo "== installed component fingerprints"
    [ ! -f "$ST/driver-version" ] || { printf 'installed driver version: '; cat "$ST/driver-version"; }
    for f in /Library/Extensions/NVRM.kext/Contents/MacOS/NVRM /Library/Extensions/NVAccel.kext/Contents/MacOS/NVAccel /Library/Extensions/NVRMFB.kext/Contents/MacOS/NVRMFB /Library/Extensions/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC /Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver /Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib; do
      [ ! -f "$f" ] || shasum -a 256 "$f"
    done
    echo; echo "== processor"; sysctl machdep.cpu.vendor machdep.cpu.brand_string machdep.cpu.family machdep.cpu.model machdep.cpu.stepping 2>&1
    echo; echo "== NVRM"; ioreg -r -n NVRM -d 1 -l 2>/dev/null | grep -E '"nvrm-'
    echo; echo "== system profile and per-system rules"; cat "$ST/system-profile.json" 2>/dev/null || echo "(none recorded)"
    sed -n '/^# --- 1401 per-system rules ---$/,/^# --- end 1401 per-system rules ---$/p' /Library/GPUBundles/nvmtl/nvrm610.conf 2>/dev/null
    # 10-07: a second user's install failed and nothing sent named the card. The GPU model and PCI ID
    # are what decide which code path the driver takes (Turing/Ampere/Ada/Blackwell).
    echo; echo "== graphics"; system_profiler SPDisplaysDataType 2>/dev/null | grep -E "Chipset Model|Type:|Bus:|VRAM|Vendor|Device ID|Revision ID|Metal|Resolution|Display Type|Online"
    echo; echo "== NVIDIA PCI devices"; ioreg -r -c IOPCIDevice -d 1 -l 2>/dev/null | awk '
      /\+-o / { if (nv) print block; block=$0; nv=0; next }
      /"vendor-id" = <de100000>/ { nv=1 }
      /"(vendor-id|device-id|subsystem-vendor-id|subsystem-id|class-code|model|assigned-addresses|reg|nvrm-[^"]*)"/ { block=block "\n" $0 }
      END { if (nv) print block }'
    echo; echo "== display bring-up"; sysctl kern.boottime debug.nvaccel_heads_published debug.nvaccelfb debug.nvrmfb_agdc debug.nvaccel_iop 2>&1
    } > "$COLLECT/driver-state.txt" 2>&1
  # the kernel's own words from the last boots: NVRM/NVAccel/NVRMFB print why they stopped (GSP boot, BAR, display).
  # The AMD/Intel test driver (NMXGPU, NMXAccel, amdgpu/i915/xe and DRM inside it) is in here too: before 1.14.4 none of
  # its lines were collected, so three machines that turned it on (one an RX 7900 XTX, 10-12) told us nothing.
  # A boot that hung early may not have reached the log store; a later boot's panic report then carries it.
  boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
  lshow 45 --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "nvrm" OR eventMessage CONTAINS[c] "nvaccel" OR eventMessage CONTAINS[c] "nvidia" OR eventMessage CONTAINS[c] "nullmoth" OR eventMessage CONTAINS[c] "gsp" OR eventMessage CONTAINS[c] "nmx" OR eventMessage CONTAINS[c] "amdgpu" OR eventMessage CONTAINS[c] "i915" OR eventMessage CONTAINS "[drm]" OR eventMessage CONTAINS "xe " OR sender == "NMXGPU" OR sender == "NMXAccel" OR sender == "NMXNPU" OR sender == "NMIntelFB")' 2>&1 \
    | grep -v -e 'NVRM-fb: kapi event type 5$' -e 'NVRM-xnu: >os_map_kernel_space' -e 'NVRM-fb: flipToMemory #' \
             -e 'NVAccel: newVidMemory -> ' -e 'NVVidMemory::allocPhysical(.*bytes of REAL VRAM' \
    | headtail 1500 1500 > "$COLLECT/driver-kernel-log.txt"
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-kernel-log.txt"
  # 1.9.0 report (10-10, 2K 165 Hz monitor stuck at 1080p 60 Hz): the 6000-line tail above was all os_map_kernel_space
  # and flipToMemory lines, so the boot-time mode list, the EDID and why each mode was or was not offered were gone.
  # Everything the display decision used, from THIS boot only, never tailed away.
  { echo "== monitors (EDID as macOS sees it)"
    ioreg -l -w0 -r -c IODisplayConnect 2>/dev/null | grep -E '"(IODisplayEDID|DisplayProductName|DisplayVendorID|DisplayProductID)"'
    echo; echo "== NVIDIA framebuffers"
    ioreg -l -w0 -r -c NVRMNVDAFramebuffer 2>/dev/null | grep -E '"(IOFB[A-Za-z]*(Mode|Timing|Pixel|Display)[A-Za-z]*|nvrm[A-Za-z-]*|NVRM[A-Za-z]*|IOFBDependentIndex)"'
    echo; echo "== framebuffer messages since this boot"
    boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    lshow 30 --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact \
      --predicate 'process == "kernel" AND (eventMessage CONTAINS "NVRM-fb" OR eventMessage CONTAINS "nvkms")' 2>&1 \
      | grep -v -e 'kapi event type 5$' -e 'flipToMemory #' | head -n 4000
  } > "$COLLECT/driver-display.txt" 2>&1
  # A boot that hung or went black never writes a panic: its kernel messages are only in the log store. Users could not
  # send them (10-10 chat: "the logs from the kernel don't get saved to a file"), so keep the whole kernel log of the boots
  # before this one, up to this boot's start: the tail is how the last failed boot ended.
  { boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    echo "== kernel messages before this boot (started $(date -r "$boot" '+%Y-%m-%d %H:%M:%S'))"
    lshow 45 --start "$(date -r $((boot - 43200)) '+%Y-%m-%d %H:%M:%S')" --end "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" \
      --style compact --predicate 'process == "kernel"' 2>&1 \
      | grep -v -e 'NVRM-fb: kapi event type 5$' -e 'NVRM-xnu: >os_map_kernel_space' -e 'NVRM-fb: flipToMemory #' \
      | awk -v end="$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" '
          # log show runs past --end (measured 10-10: this file ended in the current boot), so cut at the boot time here;
          # a line without a timestamp belongs to the line above it
          /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] / { keep = ($1 " " substr($2, 1, 8)) < end }
          keep' | tail -n 8000
  } > "$COLLECT/previous-boot-kernel-log.txt" 2>&1
  # Every GPU and the processor, whatever the vendor and age, supported by NullMoth or not (requested broad hardware coverage). The sections above only name NVIDIA; Intel iGPUs (HD to Xe and Arc), Radeon cards and Ryzen iGPUs
  # need the same facts: what macOS bound to them, whether Metal sees them, and what the boot loader injected.
  { echo "== every display device (any vendor)"
    ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | LC_ALL=C awk '
      /\+-o / { if (gpu) print block "\n"; block=$0; gpu=0; next }
      /"class-code" = <[0-9a-f][0-9a-f][0-9a-f][0-9a-f]0300>/ { gpu=1 }   # class 03xx: VGA (0300), XGA, 3D controller (0302: many laptop dGPUs)
      /"(vendor-id|device-id|subsystem-vendor-id|subsystem-id|revision-id|class-code|model|acpi-path|pcidebug|AAPL,ig-platform-id|AAPL,snb-platform-id|framebuffer-[^"]*|device-id-spoof|built-in|hda-gfx|IOName|IOPCIExpressLinkStatus|IOPCIResourced)"/ { block=block "\n" $0 }
      END { if (gpu) print block }'
    echo; echo "== accelerators (a GPU macOS accelerates has one, with its Metal plugin)"
    ioreg -r -c IOAccelerator -d 1 -w0 2>/dev/null | LC_ALL=C grep -aoE '\+-o [A-Za-z0-9_]+|"(MetalPluginName|MetalPluginClassName|IOGLBundleName|IOClass|GPUConfigurationVariable|model)" = [^,}]{0,120}'
    echo; echo "== framebuffers (which driver owns each screen)"
    ioreg -r -c IOFramebuffer -d 1 -w0 2>/dev/null | LC_ALL=C grep -aoE '\+-o [A-Za-z0-9_@,]+|"(IOFBCurrentPixelClock|IOFBCurrentPixelCount|IOFBDependentID|IOFBDependentIndex|AAPL,boot-display|IOFBScalerInfo)" = [^,}]{0,80}'
    echo; echo "== graphics kexts loaded"
    kmutil showloaded --list-only 2>/dev/null | grep -iE 'graphic|intel|amd|radeon|ati|nvidia|nullmoth|lilu|whatevergreen|noot|gfx|framebuffer|accel' | awk '{print $NF, $(NF-1)}'
    echo; echo "== Metal"; system_profiler SPDisplaysDataType 2>/dev/null
    echo; echo "== processor"
    sysctl machdep.cpu hw.physicalcpu hw.logicalcpu hw.memsize hw.cpufrequency hw.packages 2>/dev/null
    echo; echo "== graphics messages since this boot (non-NVIDIA drivers)"
    boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    lshow 20 --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact \
      --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "igfx" OR eventMessage CONTAINS[c] "IntelAccel" OR eventMessage CONTAINS[c] "AppleIntel" OR eventMessage CONTAINS[c] "AMDRadeon" OR eventMessage CONTAINS[c] "ATIController" OR eventMessage CONTAINS[c] "AMDFramebuffer" OR eventMessage CONTAINS[c] "NMIntelFB" OR eventMessage CONTAINS[c] "WhateverGreen" OR eventMessage CONTAINS[c] "IOAccel")' 2>&1 | head -n 3000
  } > "$COLLECT/gpu-all.txt" 2>&1
  # The current kernel message ring can retain early GSP/BAR failures absent from the log store.
  { echo; echo "== current kernel message ring"; dmesg 2>&1 | grep -iE 'nvrm|nvaccel|nvidia|nullmoth|gsp|nmx|amdgpu|i915|\[drm\]' | grep -v -e 'newVidMemory -> ' -e 'bytes of REAL VRAM' | headtail 300 300; } >> "$COLLECT/driver-kernel-log.txt"
  # Keep update/snapshot context separate so APFS traffic cannot crowd out GPU errors.
  lshow 20 --last 1d --style compact --predicate '(process == "kernel" AND eventMessage CONTAINS[c] "apfs") OR process == "MobileSoftwareUpdate" OR process == "softwareupdated"' 2>&1 \
    | tail -n 2000 > "$COLLECT/driver-update-log.txt"
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-update-log.txt"
  { echo; echo "== current update message ring"; dmesg 2>&1 | grep -iE 'apfs|MobileSoftwareUpdate|softwareupdated' | tail -n 1000; } >> "$COLLECT/driver-update-log.txt"
  # NPUs (Intel NPU, AMD Ryzen AI): every layer from the PCI function up to the runtimes, so one report names the layer
  # that failed. The runtime probes run as the logged-in user (apps do; root would also hide node-permission faults
  # and make XRT ignore its debug ini) with their own debug logging on, each bounded in time.
  npu_run() {  # <seconds> <command...>: runs it as the console user; kills it past the bound and says so
    local secs=$1 p w rc; shift
    if [ -z "${console_user:-}" ] || [ "$console_user" = root ] || [ "$console_user" = loginwindow ]; then
      echo "(skipped: nobody is logged in to run it as)"; return 0
    fi
    sudo -u "$console_user" "$@" 2>&1 & p=$!
    ( sleep "$secs"; kill -9 "$p" 2>/dev/null && echo "(killed after ${secs} s)" ) & w=$!
    wait "$p"; rc=$?
    kill "$w" 2>/dev/null; wait "$w" 2>/dev/null
    echo "(exit $rc)"
  }
  console_user=$(stat -f %Su /dev/console 2>/dev/null)
  NPUDIR=/Library/NullMoth/npu
  # The runtime probes below (compiler check, OpenVINO, XRT) and the userland hashes ran on every Mac with the NPU
  # userland installed, NPU or not, and alone could outlast the whole collection limit: 7 of 28 1.14.7 reports came
  # back without their kernel log (status 124). They now run only where an NPU PCI function exists, each bounded short.
  npu_pci=$(ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | LC_ALL=C awk '
      function out() { if ((cls=="12" && v=="intel") || (cls=="1180" && v=="amd")) print block "\n" }
      /\+-o / { out(); block=$0; cls=""; v=""; next }
      /"vendor-id" = <8680/ { v="intel" } /"vendor-id" = <2210/ { v="amd" }
      /"class-code" = <[0-9a-f][0-9a-f][0-9a-f][0-9a-f]1200>/ { cls="12" }
      /"class-code" = <[0-9a-f][0-9a-f]801100>/ { cls="1180" }
      /"(vendor-id|device-id|subsystem-vendor-id|subsystem-id|revision-id|class-code|IOName|IOPCIExpressLinkStatus|IOPCIResourced|assigned-addresses|acpi-path)"/ { block=block "\n" $0 }
      END { out() }')
  {
    echo "== NPU PCI functions (Intel class 12xxxx, AMD signal-processing 1180xx) and their properties"
    # ioreg lists a device's properties alphabetically (class-code before vendor-id), so each device is judged at its end
    if [ -n "$npu_pci" ]; then printf '%s\n' "$npu_pci"; else echo "(no NPU PCI function: runtime probes skipped)"; fi
    echo; echo "== what attached to them (NMXNPU is the NPU driver)"
    ioreg -r -c NMXNPU -l -w0 2>/dev/null | head -60 || true
    echo; echo "== NPU boot arguments"; nvram boot-args 2>/dev/null | tr ' \t' '\n\n' | grep -E 'nmx|npu' || echo "(none)"
    echo; echo "== boot guards (set = the last attach never returned; the driver stays parked)"
    nvram nullmoth-npu-attaching 2>/dev/null || echo "nullmoth-npu-attaching (not set)"
    nvram nullmoth-gpu-attaching 2>/dev/null || echo "nullmoth-gpu-attaching (not set)"
    echo; echo "== NMXNPU kext: loaded, in the auxiliary collection, installed file"
    kmutil showloaded --list-only 2>/dev/null | grep -i nmxnpu || echo "(not loaded)"
    kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null | grep -i nmxnpu || echo "(not in the auxiliary collection)"
    [ ! -f /Library/Extensions/NMXNPU.kext/Contents/MacOS/NMXNPU ] || shasum -a 256 /Library/Extensions/NMXNPU.kext/Contents/MacOS/NMXNPU
    echo; echo "== NPU firmware"; ls -la /Library/NullMoth/firmware/intel/vpu /Library/NullMoth/firmware/amdnpu/* 2>&1 | head -40
    echo; echo "== device nodes"; ls -la /dev/accel 2>&1
    echo; echo "== each node: driver, PCI identity, driver attributes (nmx-npu-probe)"
    if [ -n "$npu_pci" ] && [ -x "$NPUDIR/bin/nmx-npu-probe" ]; then npu_run 20 "$NPUDIR/bin/nmx-npu-probe"; else echo "(nmx-npu-probe not installed)"; fi
    echo; echo "== NPU userland installed (sha256)"
    if [ -d "$NPUDIR" ]; then find "$NPUDIR" -type f ! -name '*.a' 2>/dev/null | head -200 | tr '\n' '\0' | xargs -0 shasum -a 256 2>&1 | sed "s|$NPUDIR/||"; ls -la "$NPUDIR/xrt/share/amdxdna/bins" 2>&1
    else echo "(no $NPUDIR)"; fi
    echo; echo "== Intel: the NPU compiler on a small network (Meteor/Arrow Lake 3720, Lunar Lake 4000; needs no NPU)"
    if [ -n "$npu_pci" ] && [ -x "$NPUDIR/bin/nmx-npu-compile-check" ]; then npu_run 60 "$NPUDIR/bin/nmx-npu-compile-check"
    else echo "(nmx-npu-compile-check not installed)"; fi
    echo; echo "== Intel: OpenVINO devices with Level Zero, the NPU driver and the NPU plugin logging"
    if [ -n "$npu_pci" ] && [ -x "$NPUDIR/openvino/bin/hello_query_device" ]; then
      npu_run 45 env ZE_ENABLE_LOADER_DEBUG_TRACE=1 ZE_INTEL_NPU_LOGLEVEL=INFO OV_NPU_LOG_LEVEL=LOG_INFO \
        "$NPUDIR/openvino/bin/hello_query_device"
    else echo "(no NPU, or OpenVINO not installed)"; fi
    echo; echo "== AMD: xrt-smi examine, then examine and validate on each AMD NPU, with XRT's runtime log on"
    if [ -n "$npu_pci" ] && [ -x "$NPUDIR/xrt/bin/xrt-smi" ]; then
      ini=$(sudo -u "$console_user" mktemp -d /tmp/nullmoth-xrt.XXXXXX 2>/dev/null)/xrt.ini
      if [ -n "$console_user" ] && [ "$ini" != /xrt.ini ]; then
        printf '[Runtime]\nverbosity=7\nruntime_log=console\n' | sudo -u "$console_user" tee "$ini" > /dev/null
      fi
      npu_run 60 env XRT_INI_PATH="$ini" "$NPUDIR/xrt/bin/xrt-smi" examine
      for bdf in $("$NPUDIR/bin/nmx-npu-probe" 2>/dev/null | awk '/^  driver amdxdna/ {a=1} a && /^  pci / {print $2; a=0}'); do
        echo; echo "-- examine -d $bdf -r all"; npu_run 60 env XRT_INI_PATH="$ini" "$NPUDIR/xrt/bin/xrt-smi" examine -d "$bdf" -r all
        echo; echo "-- validate -d $bdf"; npu_run 60 env XRT_INI_PATH="$ini" "$NPUDIR/xrt/bin/xrt-smi" validate -d "$bdf"
      done
      [ "$ini" = /xrt.ini ] || rm -rf "$(dirname "$ini")"
    else echo "(XRT not installed)"; fi
    echo; echo "== NPU kernel messages since this boot"
    boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    lshow 15 --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact \
      --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "NMXNPU" OR eventMessage CONTAINS[c] "ivpu" OR eventMessage CONTAINS[c] "intel_vpu" OR eventMessage CONTAINS[c] "amdxdna" OR eventMessage CONTAINS[c] "xdna" OR eventMessage CONTAINS "NullMoth drm")' 2>&1 | head -n 4000
    echo; echo "== NPU messages in the kernel ring"; dmesg 2>&1 | grep -iE 'nmxnpu|ivpu|intel_vpu|amdxdna|xdna|nullmoth drm' | tail -n 1000
    echo; echo "== app messages from the NPU runtimes, since this boot"
    lshow 15 --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact --predicate 'process != "kernel" AND (eventMessage CONTAINS[c] "ze_intel_npu" OR eventMessage CONTAINS[c] "level_zero" OR eventMessage CONTAINS[c] "openvino" OR eventMessage CONTAINS[c] "xrt" OR eventMessage CONTAINS[c] "amdxdna")' 2>&1 | tail -n 2000
  } > "$COLLECT/npu.txt" 2>&1
  # crash reports of processes that had the NPU runtimes loaded (newest three)
  n_npu=0
  for ips in $(ls -t /Library/Logs/DiagnosticReports/*.ips /Users/*/Library/Logs/DiagnosticReports/*.ips 2>/dev/null | head -n 200); do
    [ "$n_npu" -lt 3 ] || break
    grep -q "/Library/NullMoth/npu/" "$ips" 2>/dev/null || continue
    # the uploader takes only plain file names
    cp "$ips" "$COLLECT/npu-crash-$(basename "$ips" | LC_ALL=C tr -c 'A-Za-z0-9._\n-' '_')" && n_npu=$((n_npu+1))
  done
  # Release builds report failures to syslog rather than nvmtl.log. Debug builds may use either
  # the system temp directory or the console user's temp directory.
  lshow 40 --last 1d --style compact --predicate 'process != "kernel" AND (eventMessage CONTAINS[c] "NVMTL" OR eventMessage CONTAINS[c] "NullMoth" OR eventMessage CONTAINS[c] "nvk-reason")' 2>&1 \
    | tail -n 3000 > "$COLLECT/driver-plugin-log.txt"
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-plugin-log.txt"
  console_user=$(stat -f %Su /dev/console 2>/dev/null)
  console_tmp=""
  if [ -n "$console_user" ] && [ "$console_user" != root ] && [ "$console_user" != loginwindow ]; then
    console_tmp=$(sudo -u "$console_user" getconf DARWIN_USER_TEMP_DIR 2>/dev/null)
  fi
  for f in /private/tmp/nvmtl.log "${console_tmp:+${console_tmp%/}/nvmtl.log}"; do
    [ -f "$f" ] || continue
    # /tmp is writable by every account: read as nobody so a planted link to a root-only file stays unreadable
    reader=nobody; [ -n "$console_tmp" ] && [ "$f" = "${console_tmp%/}/nvmtl.log" ] && reader=$console_user
    { echo; echo "== plugin file log"; sudo -u "$reader" tail -c 262144 "$f"; } >> "$COLLECT/driver-plugin-log.txt"
  done
  # The rolling logs above keep only the newest lines, so the boot where WindowServer actually crashed was usually
  # gone by the time the user sent logs: eight crash reports in one day named the abort ("Failed to create
  # MetalDevice") but none carried the plugin's reason. For each recent WindowServer crash, keep the driver's own
  # lines from two minutes before it to just after it. The report's timestamp is local time, as log show expects.
  {
    for ips in $(ls -t /Library/Logs/DiagnosticReports/WindowServer*.ips /Library/Logs/DiagnosticReports/Retired/WindowServer*.ips 2>/dev/null | head -n 3); do
      stamp=$(head -n 1 "$ips" 2>/dev/null | sed -n 's/.*"timestamp" *: *"\([0-9-]* [0-9:]*\)[^"]*".*/\1/p')
      echo; echo "== WindowServer crash ${ips##*/} at ${stamp:-unknown time}"
      [ -n "$stamp" ] || { echo "(no timestamp in the report)"; continue; }
      at=$(date -j -f "%Y-%m-%d %H:%M:%S" "$stamp" "+%s" 2>/dev/null) || { echo "(timestamp not understood: $stamp)"; continue; }
      from=$(date -j -r $((at - 120)) "+%Y-%m-%d %H:%M:%S"); to=$(date -j -r $((at + 5)) "+%Y-%m-%d %H:%M:%S")
      # log show runs past --end (measured: an 11:26:47 end returned lines to 11:28:20), so lines are cut at "to" here.
      # Separate budgets: on a busy GPU the kernel lines alone filled a shared 1500-line tail and pushed out the
      # plugin's and WindowServer's own reason (measured: 1482 of 1500 lines were kernel lines).
      echo "-- plugin and WindowServer errors"
      lshow 20 --start "$from" --end "$to" --style compact --predicate '(process != "kernel" AND (eventMessage CONTAINS[c] "NVMTL" OR eventMessage CONTAINS[c] "NullMoth" OR eventMessage CONTAINS[c] "nvk-reason" OR eventMessage CONTAINS[c] "MetalDevice")) OR (process == "WindowServer" AND (messageType == error OR messageType == fault))' 2>&1 \
        | awk -v to="$to" '!/^20[0-9][0-9]-/ || substr($0, 1, 19) <= to' \
        | tail -n 700
      echo "log show exit: ${PIPESTATUS[0]}"
      echo "-- kernel driver lines"
      lshow 20 --start "$from" --end "$to" --style compact --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "nvrm" OR eventMessage CONTAINS[c] "nvaccel" OR eventMessage CONTAINS[c] "gsp")' 2>&1 \
        | awk -v to="$to" '!/^20[0-9][0-9]-/ || substr($0, 1, 19) <= to' \
        | tail -n 800
      echo "log show exit: ${PIPESTATUS[0]}"
    done
  } > "$COLLECT/driver-crash-window.txt" 2>&1
  { echo; echo "== NullMoth NVRAM flags (a remove flag that survives every boot removes the driver at login)"
    nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove 2>&1; } >> "$COLLECT/driver-state.txt"
  # -h: act on a symbolic link itself, never on what it points at. Without it
  # root followed any link planted in the collection directory and made the
  # target world readable. The directory comes from --collect-logs, so it is
  # the caller's choice of path.
  chmod -R -h a+rX "$COLLECT"; ok "collected driver state and $n diagnostic log(s)"; cleanup
  if [ "$collection_errors" -gt 0 ]; then note "$collection_errors diagnostic log(s) could not be copied"; echo "RESULT partial"; exit 1; fi
  echo "RESULT ok"; exit 0
fi

if [ $REMOVE = 1 ]; then
  step "Removing the NullMoth driver"
  if [ -f "$STATE" ]; then
    . "$STATE" || stop "the install record is invalid - nothing changed"
  else
    # A driver installed from the 1401 stick, by hand, or by an app whose record was lost has no install record, and
    # removal used to stop here (1.8 log 10-10), leaving the user with a driver they could not take out. Without a
    # record nothing is restored from a backup: the driver's own settings come out of the OpenCore config this Mac
    # started from (or the one selected), and the uninstaller removes its files.
    note "no install record in $STATE; removing the driver from the OpenCore config this Mac started from"
    CONFIG_SHA_AFTER=""; ADDED_ARGS=""; REMOVED_ARGS=""; CONFIG_BACKUP_REL=""
    if [ -z "$CFG" ] && [ "$EFI" = auto ]; then
      d=$(booted_part) || d=$(boot_esp)
      [ -n "$d" ] || stop "no install record, and the OpenCore partition this Mac started from was not found; select its config.plist - nothing changed"
      mount_efi "$d" || stop "could not mount the OpenCore partition $d - nothing changed"
      r=$(ocrel_in "$MOUNT_POINT"); [ -n "$r" ] || stop "no OpenCore config for this Mac on $d; select its config.plist - nothing changed"
      CONFIG_PATH="$MOUNT_POINT/$r/config.plist"; OCREL=$r
    fi
  fi
  C=""; MP=""
  if [ -n "$CFG" ]; then
    C=$CFG; [ -f "$C" ] || stop "the selected OpenCore config is missing - nothing changed"
    MP=$(cd "$(dirname "$C")/../.." && pwd) || stop "cannot resolve the selected OpenCore partition"
  elif [ "$EFI" != auto ]; then
    mount_efi "$EFI" || stop "could not mount the selected OpenCore partition - nothing changed"; MP=$MOUNT_POINT
    C="$MP/${OCREL:-EFI/OC}/config.plist"
  elif [ -n "${EFI_UUID:-}" ] && mount_efi "$EFI_UUID"; then
    MP=$MOUNT_POINT; C="$MP/${OCREL:-EFI/OC}/config.plist"
  elif [ -n "${CONFIG_PATH:-}" ] && [ -f "$CONFIG_PATH" ]; then
    C=$CONFIG_PATH
    MP=$(cd "$(dirname "$C")/../.." && pwd) || stop "cannot resolve the recorded OpenCore partition"
  fi
  # The boot picker's "Remove driver" runs this at the next start, before anything has mounted the EFI partition, so a
  # record holding only a config path (/Volumes/EFI/...) or a partition that is no longer attached stopped here and the
  # driver stayed off but installed, start after start (1.6-1.9 recover logs: "the recorded config is missing", "could
  # not mount the recorded OpenCore partition"; Discord 10-11). The partition OpenCore started this Mac from is the one
  # whose picker asked for the removal, so it is used instead.
  if [ -z "$C" ] && [ -z "$CFG" ] && [ "$EFI" = auto ] && { [ -n "${EFI_UUID:-}" ] || [ -n "${CONFIG_PATH:-}" ]; }; then
    note "the recorded OpenCore config was not found; using the partition this Mac started from"
    d=$(booted_part) || d=$(boot_esp)
    [ -n "$d" ] || stop "the recorded OpenCore config is missing and the partition this Mac started from was not found; select its config.plist - nothing changed"
    mount_efi "$d" || stop "could not mount the OpenCore partition $d - nothing changed"
    r=$(ocrel_in "$MOUNT_POINT"); [ -n "$r" ] || stop "no OpenCore config on $d; select its config.plist - nothing changed"
    MP=$MOUNT_POINT; C="$MP/$r/config.plist"; OCREL=$r
  fi
  [ -n "$C" ] || stop "the install record has no OpenCore partition; select the original config - nothing changed"
  if [ -n "$C" ]; then
    [ -f "$C" ] && plutil -lint "$C" >/dev/null || stop "the OpenCore config is missing or invalid - nothing changed"
    # The backup is used only when the config is unchanged since the install; otherwise the driver's settings are taken
    # out of the current config in place. A missing backup stopped removal outright (user reports 10-08), although the
    # in-place path below needs no backup at all - so it is a note, and that path runs.
    [ -f "$MP/${CONFIG_BACKUP_REL:-}" ] || note "the recorded OpenCore backup is not on this partition; the driver's settings are removed from the current config instead"
  fi
  if [ -n "$C" ]; then
    step "Checking the OpenCore removal settings"
    ORIGINAL_CONFIG=$C
    C="$ORIGINAL_CONFIG.nullmoth-remove-new"
    cp -p "$ORIGINAL_CONFIG" "$C" || stop "could not stage the OpenCore removal settings - nothing changed"
    if [ "$(shasum -a 256 "$C" | awk '{print $1}')" = "$CONFIG_SHA_AFTER" ] && [ -f "$MP/$CONFIG_BACKUP_REL" ]; then
      cp -p "$MP/$CONFIG_BACKUP_REL" "$C" || stop "could not stage the original config - nothing changed"
    else
      note "the config changed after the install, so unrelated settings are preserved"
    fi
    # An upgrade backup may itself contain an older driver configuration. Clear
    # the driver settings on both paths before deleting the recovery tool.
      i=0
      while identifier=$(plutil -extract Kernel.Block.$i.Identifier raw -o - "$C" 2>/dev/null); do
        if [ "$identifier" = com.nullmoth.NVAccel ] && [ "$(get Kernel.Block.$i.Comment)" = "park the OS-specific accelerator during a macOS update" ]; then
          plutil -remove Kernel.Block.$i "$C" || stop "could not stage removal of the update parking entry - nothing changed"
          continue
        fi
        i=$((i+1))
      done
      args=$(get NVRAM.Add.$B.boot-args); new=""
      for a in $args; do
        case "$a" in nvfb=*|nvaccel=*|nvfbheads=*|-nvkmsnosmooth|-nvoff) continue;; esac
        # AMFI settings may belong to another driver or an earlier system setup.
        # Remove shared settings only when the install record owns the token.
        case " $ADDED_ARGS " in *" $a "*) ;; *) new="$new $a";; esac
      done
      for a in $REMOVED_ARGS; do
        case "$a" in nvfb=*|nvaccel=*|nvfbheads=*|-nvkmsnosmooth|-nvoff) continue;; esac
        case " $new " in *" $a "*) ;; *) new="$new $a";; esac
      done
      plutil -replace NVRAM.Add.$B.boot-args -string "${new# }" "$C" || stop "could not stage boot arguments - nothing changed"
      if [ "$(csr_of)" = "${NEW_CSR:-x}" ]; then plutil -replace NVRAM.Add.$B.csr-active-config -data "$(csr_data "$OLD_CSR")" "$C" || stop "could not stage the recorded SIP setting - nothing changed"; fi
      if [ -n "${OLD_SBM:-}" ] && [ "$(get Misc.Security.SecureBootModel)" = Disabled ]; then plutil -replace Misc.Security.SecureBootModel -string "$OLD_SBM" "$C" || stop "could not stage SecureBootModel - nothing changed"; fi
      i=$(tool_index); if [ -n "$i" ]; then plutil -remove Misc.Tools.$i "$C" || stop "could not stage removal of the picker tool - nothing changed"; fi
      # the start-once entry goes with the driver (it only switches the driver off); the boot logo stays, it is not the driver's
      i=$(once_index); if [ -n "$i" ]; then plutil -remove Misc.Tools.$i "$C" || stop "could not stage removal of the start-once entry - nothing changed"; fi
      # put the boot picker back the way it was before install, when this install changed it (SET_*) and nothing has
      # changed it away from our value since. An empty recorded value means the key was absent before, so remove it.
      restore_boot() {  # $1 key, $2 type, $3 our-target, $4 owned(0/1), $5 recorded-old
        [ "$4" = 1 ] || return 0
        [ "$(plutil -extract "$1" raw -o - "$C" 2>/dev/null)" = "$3" ] || return 0
        if [ -n "$5" ]; then plutil -replace "$1" "$2" "$5" "$C" 2>/dev/null || plutil -insert "$1" "$2" "$5" "$C"
        else plutil -remove "$1" "$C" 2>/dev/null || true; fi; }
      restore_boot Misc.Boot.ShowPicker   -bool   true    "${SET_SHOWPICKER:-0}"  "${OLD_SHOWPICKER:-}"  || stop "could not stage the recorded picker setting - nothing changed"
      restore_boot Misc.Boot.HideAuxiliary -bool  false   "${SET_HIDEAUX:-0}"     "${OLD_HIDEAUX:-}"     || stop "could not stage the recorded auxiliary setting - nothing changed"
      restore_boot Misc.Boot.PickerMode   -string Builtin "${SET_PICKERMODE:-0}"  "${OLD_PICKERMODE:-}"  || stop "could not stage the recorded picker mode - nothing changed"
      # back to the installer-safe settings 1401 wrote: small BAR for macOS, firmware framebuffer allowed
      plutil -replace UEFI.Quirks.ResizeGpuBars -integer -1 "$C" && plutil -replace Booter.Quirks.ResizeAppleGpuBars -integer 0 "$C" || stop "could not stage installer-safe BAR settings - nothing changed"
      bi=$(bidx 2>/dev/null); if [ -n "$bi" ]; then plutil -replace Kernel.Block.$bi.Enabled -bool false "$C" || stop "could not stage firmware display support - nothing changed"; fi

    plutil -lint "$C" >/dev/null || stop "the staged removal config is invalid - nothing changed"
  fi
  if [ "${NULLMOTH_CONFIG_ONLY:-0}" != 1 ]; then
    # the app carries the current uninstaller: a Mac that installed an older driver kept that version's copy in $ST
    # (1.0.0's could not remove the driver when no other kext was installed)
    UN="$(cd "$(dirname "$0")" && pwd)/nullmoth-uninstall.sh"; [ -x "$UN" ] || UN=$ST/uninstall.sh
    [ -x "$UN" ] || stop "no uninstaller in $ST"
    [ "$UN" != "$ST/uninstall.sh" ] && cp "$UN" "$ST/uninstall.sh" 2>/dev/null
    "$UN" "${DRIVER_BACKUP:-}" 2>&1 | sed 's/^/NOTE /'
    rc=${PIPESTATUS[0]}; [ "$rc" = 0 ] || stop "the driver uninstaller failed (exit $rc)"
    ok "driver files removed and the kernel collection rebuilt"
    # With the driver gone the Mac falls back to the Intel iGPU or the firmware framebuffer, but WindowServer still has
    # the resolution, refresh rate and arrangement it saved while the NVIDIA card drove the screen; re-applying a mode
    # the fallback cannot show left the boot after "remove driver" at a black screen (a GTX 1650 after a refresh-rate
    # change, chat). Move those preferences aside so macOS rebuilds them at a safe default on the next start.
    # Kept as a dated backup so nothing is lost; never fatal - the driver is already out.
    bak="$ST/display-prefs-$(date +%Y%m%d-%H%M%S)"; moved=0
    for f in /Library/Preferences/com.apple.windowserver*.plist /Library/Preferences/ByHost/com.apple.windowserver*.plist; do
      [ -e "$f" ] || continue
      [ "$moved" = 0 ] && { mkdir -p "$bak" || { note "could not back up the saved display settings; left them in place"; break; }; }
      mv "$f" "$bak/" 2>/dev/null && moved=1
    done
    [ "$moved" = 1 ] && ok "cleared the saved display settings so macOS starts at a safe screen mode (backup in $bak)"
  fi
  if [ -n "$C" ]; then
    mv -f "$C" "$ORIGINAL_CONFIG" || stop "could not publish OpenCore removal settings; recovery remains available"
    C=$ORIGINAL_CONFIG
    rm -f "$MP/${OCREL:-EFI/OC}/Tools/$TOOL_FILE" || stop "could not remove the picker tool; recovery remains available"
    ok "OpenCore removal settings published"
  fi
  if [ -f "$STATE" ]; then mv "$STATE" "$STATE.removed-$(date +%Y%m%d-%H%M%S)" || stop "could not archive the install record"; fi
  rm -f "$AGENT" "$RECOVER" "$ST/nullmoth-recover.sh"
  ok "done - restart to finish"; cleanup; echo "RESULT ok"; exit 0
fi

# Preparation is for an upgrade from macOS 15. On macOS 26, parking the driver would
# leave update-pending waiting for a major-version change that cannot happen.
if [ "$UPD" = prepare ]; then
  PREPARE_MAJOR=$(sw_vers -productVersion); PREPARE_MAJOR=${PREPARE_MAJOR%%.*}
  [ "$PREPARE_MAJOR" = 15 ] || stop "Tahoe preparation requires macOS 15; this Mac already runs macOS $PREPARE_MAJOR. Use the normal driver install or update."
fi

# "--update prepare" with a package is the one-button Tahoe path: install or update the driver first, then prepare.
INSTALL_THEN_PREPARE=0; [ "$UPD" = prepare ] && [ -n "$PKG" ] && INSTALL_THEN_PREPARE=1
# the app's boot-logo and test-driver switches run setup without a package; 1.14 sent them through the package check
# ("STOP package not found:", 4 uploads in the first hour of 1.14), so neither switch worked
if [ -z "$USBMAP" ] && [ -z "$VERB" ] && [ -z "${SETLOGO:-}" ] && [ -z "${GPUTEST:-}" ] && [ -z "${BACKLIGHT:-}" ] && { [ -z "$UPD" ] || [ $INSTALL_THEN_PREPARE = 1 ]; }; then
step "Checking the driver package"
[ -f "$PKG" ] || stop "package not found: $PKG"
# the app's copy is writable by the user: hash and unpack a root-owned copy, so it cannot be swapped after the check
PKGDIR=$(mktemp -d /var/tmp/nullmoth-pkg.XXXX) || stop "could not make a folder to check the package in"
if ! cperr=$(cp "$PKG" "$PKGDIR/" 2>&1); then
  # nine 1.13-1.15 uploads: the app (and its package) sat on an external drive and macOS's privacy protection for
  # removable volumes refused the read ("Operation not permitted"); the stop said "checksum does not match, download it
  # again", which could never help
  case "$cperr" in *"Operation not permitted"*)
    stop "macOS did not let the installer read the driver package on $(df "$PKG" 2>/dev/null | awk 'NR==2{print $NF}') (privacy protection for external drives). Move the 1401 app to the Applications folder on this Mac and try again" ;;
  esac
  stop "could not copy the package for checking: $cperr"
fi
PKG=$PKGDIR/$(basename "$PKG")
got=$(shasum -a 256 "$PKG" | awk '{print $1}')
[ "$got" = "$SHA" ] || stop "package checksum $got does not match $SHA - download it again"
ok "package matches its SHA-256"
INSTALLER="$(cd "$(dirname "$0")" && pwd)/nullmoth-install.sh"
[ -x "$INSTALLER" ] || stop "the audited installer is missing from the app"
[ -n "$TOOL" ] && [ -f "$TOOL" ] || stop "the boot-picker tool is missing from the app"
# This must precede EFI discovery/copy, SIP edits and driver staging.
T=$(mktemp -d /var/tmp/nullmoth.XXXX) || stop "could not create runtime preflight directory"
tar -xzf "$PKG" -C "$T" || stop "could not unpack the package for runtime compatibility checking"
RUNTIME_PREFLIGHT="$(dirname "$INSTALLER")/nullmoth-runtime-check.sh"
[ -x "$RUNTIME_PREFLIGHT" ] || stop "the runtime compatibility checker is missing from the app"
/bin/bash "$RUNTIME_PREFLIGHT" "$T/pkgroot" "$(sw_vers -productVersion)" || stop "the userland runtime cannot load on this macOS version; EFI and driver files were not changed"
rm -rf "$T"; T=""
fi

if [ -n "$CFG" ]; then C=$CFG; [ -f "$C" ] || stop "no config at $C"; MP=$(cd "$(dirname "$C")/../.." && pwd); OCREL="EFI/$(basename "$(dirname "$C")")"; ok "OpenCore config: $C (given)"
else
  step "Finding the OpenCore partition"
  if [ "$EFI" = auto ]; then
    found=""; BOOT_BOUND=0
    if d=$(booted_part); then mount_efi "$d" && mp=$MOUNT_POINT && [ -n "$(ocrel_in "$mp")" ] && { found=$d; BOOT_BOUND=1; ok "OpenCore started this Mac from $d"; }; fi
    if [ -z "$found" ]; then
      # OpenCore can live on an EFI partition or on any FAT32 partition (a 1401 stick is a FAT32 data partition)
      for d in $(diskutil list | awk '/ EFI | DOS_FAT_32 | Windows_FAT_32 | Microsoft Basic Data /{print $NF}' | grep -E '^disk[0-9]+s[0-9]+$'); do
        mount_efi "$d" || continue; mp=$MOUNT_POINT; [ -n "$(ocrel_in "$mp")" ] && found="$found $d"; done
    fi
    n=$(echo $found | wc -w | tr -d ' ')
    if [ "$n" = 0 ]; then
      # 10-07 (an iMac20,1): the stop said only "no OpenCore config" - not whether the Mac runs Clover or
      # whether a config for ANOTHER model was there. Say what each partition holds, so the user (and the report) can tell.
      clover=""; for d in $(diskutil list | awk '/ EFI | DOS_FAT_32 | Windows_FAT_32 | Microsoft Basic Data /{print $NF}' | grep -E '^disk[0-9]+s[0-9]+$'); do
        mount_efi "$d" || { echo "NOTE $d: could not be mounted"; continue; }; mp=$MOUNT_POINT
        [ -d "$mp/EFI/CLOVER" ] && { clover=1; echo "NOTE $d: Clover (EFI/CLOVER)"; }
        for c in "$mp/EFI/OC/config.plist" "$mp/EFI/BOOT/config.plist"; do [ -f "$c" ] || continue
          cm=""; for k in PlatformInfo.Generic.SystemProductName PlatformInfo.SMBIOS.SystemProductName PlatformInfo.DataHub.SystemProductName; do
            cm=$(plutil -extract "$k" raw -o - "$c" 2>/dev/null); [ -n "$cm" ] && break; done
          echo "NOTE $d: OpenCore config ${c#$mp/} for ${cm:-no model set}"; done; done
      nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version >/dev/null 2>&1 || echo "NOTE OpenCore did not start this Mac (no opencore-version in NVRAM)"
      # 10-10 (iMacPro1,1, MacBookPro16,1, iMac20,2): this stop printed no NOTE at all - no partition was even listed. Show
      # what macOS sees, so the next report names the cause: where OpenCore says it started from, and every partition.
      echo "NOTE boot-path: $(nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:boot-path 2>/dev/null | cut -f2- | head -c 300)"
      diskutil list 2>&1 | grep -E '^ +[0-9]+:|^/dev/' | head -n 40 | sed 's/^/NOTE disks: /'
      [ -n "$clover" ] && ! nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version >/dev/null 2>&1 && \
        stop "this Mac starts with Clover, not OpenCore - the NVIDIA driver's settings are made for OpenCore. Make an OpenCore setup (1401 on Windows builds one), start from it, then run this again"
      # 10-12 (seven Macs, 1401 Mac 1.10 to 1.14.5): the stick's config was there and plutil answered "Code=257" (no
      # permission). macOS checks its Removable Volumes privacy setting against the 1401 app even for this root shell,
      # so a stick's files cannot be opened until the user allows it. Say that, not "no OpenCore config".
      denied=""; for d in $(diskutil list | awk '/ EFI | DOS_FAT_32 | Windows_FAT_32 | Microsoft Basic Data /{print $NF}' | grep -E '^disk[0-9]+s[0-9]+$'); do
        mount_efi "$d" || continue; mp=$MOUNT_POINT
        for c in "$mp/EFI/OC/config.plist" "$mp/EFI/BOOT/config.plist"; do
          [ -e "$c" ] && ! head -c 1 "$c" >/dev/null 2>&1 && { denied="$denied $d"; break; }; done; done
      # six of the 10-12 reports: OpenCore's boot path was a USB device (the 1401 stick) that was no longer connected
      nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:boot-path 2>/dev/null | grep -q 'USB(' && [ -z "$denied" ] && \
        stop "this Mac started OpenCore from a USB stick that is not connected now - plug that stick back in (the one you start the Mac from), then try again"
      [ -n "$denied" ] && stop "macOS is not letting 1401 read the OpenCore config on$denied. Open System Settings > Privacy & Security > Files & Folders, turn on Removable Volumes for 1401 (or add 1401 under Full Disk Access), then try again"
      stop "no OpenCore config for this Mac ($(sysctl -n hw.model)) on any connected disk - plug in the disk or USB stick OpenCore started from, then try again (the NOTE lines above show what each partition holds)"
    fi
    # A shared SMBIOS model or the macOS disk does not identify the booted EFI.
    # Retain candidate discovery, but require explicit selection without boot-path proof.
    # File modification time does not identify the firmware startup partition: copies and clocks can match.
    # Require the actual boot-path/GPT identity or an explicit verified selection before changing OpenCore.
    # Without boot-path (some configs hide it): the running Mac's serial number IS the config's SystemSerialNumber -
    # OpenCore wrote it into this boot - so a candidate whose config holds this serial is the one that started the Mac.
    # WAS: stopped and asked (15 Macs on 1.1-1.2, most with a 1401 stick plugged in next to the internal EFI).
    if [ "$BOOT_BOUND" != 1 ]; then
      serial=$(ioreg -rd1 -c IOPlatformExpertDevice | awk -F'"' '/IOPlatformSerialNumber/{print $4}')
      match=""
      if [ -n "$serial" ]; then
        for d in $found; do mount_efi "$d" || continue; mp=$MOUNT_POINT; rel=$(ocrel_in "$mp")
          s=$(plutil -extract PlatformInfo.Generic.SystemSerialNumber raw -o - "$mp/$rel/config.plist" 2>/dev/null)
          [ "$s" = "$serial" ] && match="$match $d"; done
      fi
      if [ "$(echo $match | wc -w | tr -d ' ')" = 1 ]; then
        found=${match# }; BOOT_BOUND=1; ok "OpenCore started this Mac from $found (its config sets this Mac's serial number)"
      elif [ -n "$match" ]; then
        # The usual tie: OpenCore was copied from the 1401 stick to the macOS disk, so both configs set this serial.
        # The copy on the disk macOS runs from is the one in use once a Mac starts without the stick (measured on a
        # Mac with both: the internal copy is what started it).
        # Only when one match is on that disk: with two OpenCore partitions on the macOS disk itself (an EFI partition and
        # a FAT one labelled EFI, issue #81) the ESP is not necessarily the one that started it; the boot-args test below decides.
        esp=$(boot_esp); edisk=${esp%s[0-9]*}; ondisk=0
        for d in $match; do [ "${d%s[0-9]*}" = "$edisk" ] && ondisk=$((ondisk + 1)); done
        [ "$ondisk" = 1 ] && for d in $match; do [ "$d" = "$esp" ] && { found=$d; BOOT_BOUND=1; ok "OpenCore started this Mac from $d (on the macOS disk; the copy on$(echo " $match" | sed "s/ $d//") sets the same serial)"; }; done
        [ "$ondisk" -gt 1 ] && found=${match# }
      fi
    fi
    # Still two or more (user reports 10-10, Mac 1.5 and 1.6: disk0s1 + disk2s1, neither config's serial matched): OpenCore
    # deletes and rewrites boot-args from its config at every start (NVRAM > Delete + Add), so the running boot-args are
    # the started config's own, word for word. A candidate whose config sets exactly these boot-args and this Mac's model
    # is the one that started it - when only one does.
    if [ "$BOOT_BOUND" != 1 ]; then
      live=$(nvram boot-args 2>/dev/null | cut -f2-); model=$(sysctl -n hw.model); match=""
      if [ -n "$live" ]; then
        for d in $found; do mount_efi "$d" || continue; mp=$MOUNT_POINT; rel=$(ocrel_in "$mp"); c="$mp/$rel/config.plist"
          a=$(plutil -extract NVRAM.Add.7C436110-AB2A-4BBB-A880-FE41995C9F82.boot-args raw -o - "$c" 2>/dev/null)
          m=$(plutil -extract PlatformInfo.Generic.SystemProductName raw -o - "$c" 2>/dev/null)
          [ "$a" = "$live" ] && [ "$m" = "$model" ] && match="$match $d"; done
      fi
      if [ "$(echo $match | wc -w | tr -d ' ')" = 1 ]; then
        found=${match# }; BOOT_BOUND=1; ok "OpenCore started this Mac from $found (its config sets exactly this boot's boot-args and model)"
      fi
    fi
    # Only ONE OpenCore partition is connected, and OpenCore started this Mac (it writes opencore-version every boot):
    # then that partition is the one (Mac 1.7 logs 10-10: "NOTE candidate disk0s2" alone, still stopped and asked).
    if [ "$BOOT_BOUND" != 1 ] && [ "$(echo $found | wc -w | tr -d ' ')" = 1 ] && \
       nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version >/dev/null 2>&1; then
      found=${found# }; BOOT_BOUND=1; ok "OpenCore started this Mac and $found holds the only OpenCore on any connected disk"
    fi
    if [ "$BOOT_BOUND" != 1 ]; then
      for d in $found; do echo "NOTE candidate $d"; done
      # Mac 1.9/1.10 logs 10-10 (5 uploads, one candidate each): OpenCore hides boot-path and its version when
      # Misc > Security > ExposeSensitiveData lacks bits 0x1/0x2, so nothing proves which partition started the Mac, and
      # editing one that did not would install the driver without its settings or the removal tool. Say how to fix it.
      nvram 4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version >/dev/null 2>&1 || \
        echo "NOTE OpenCore hides where it started from: set Misc > Security > ExposeSensitiveData to 7 in this config.plist (bit 1 publishes boot-path; 1401 builds use 7), restart, then try again - or choose the partition in the app"
      stop "OpenCore's startup partition could not be confirmed - select the partition this Mac started from"
    fi
    EFI=${found# }
  fi
  mount_efi "$EFI" || stop "could not mount $EFI"; MP=$MOUNT_POINT
  OCREL=$(ocrel_in "$MP"); [ -n "$OCREL" ] || stop "$EFI has no OpenCore config (EFI/OC/config.plist or EFI/BOOT/config.plist)"
  C="$MP/$OCREL/config.plist"
  ok "OpenCore config: $EFI ($C)"
  # Installation uses the verified or explicitly selected startup partition.
  # Implicit migration cannot establish ownership of another disk's boot files.
  if [ $DRY = 0 ] && [ -z "$VERB" ] && [ "${UPD:-}" != finish ] && on_usb "$EFI"; then
    note "OpenCore remains on the selected startup stick. Keep it attached for every restart; automatic copying onto another EFI partition is disabled"
    note "Existing internal Windows and vendor boot files were left untouched. Review the internal boot setup separately before removing the stick"
    # The stranding case: this Mac started OpenCore from the removable stick, and the internal disk macOS runs from has
    # no OpenCore of its own. Without the stick it will not start at all, so the "Remove NVIDIA driver" entry is gone the
    # moment the stick is unplugged. An automatic copy onto the internal EFI left Macs unbootable, so it stays disabled;
    # instead, say the one exact thing to do. Reading the internal EFI is a mount and a lookup, never a write.
    iesp=$(boot_esp)
    if [ -n "$iesp" ] && ! on_usb "$iesp" && mount_efi "$iesp"; then
      [ -n "$(ocrel_in "$MOUNT_POINT")" ] || note "This Mac starts OpenCore only from the stick ($EFI); its internal disk ($iesp) has no OpenCore, so without the stick it will not start and \"$TOOL_NAME\" will not appear. Keep the stick plugged in for every restart. To start from the internal disk on its own, copy the EFI folder from the stick to the internal disk's EFI partition and set it to boot by following the OpenCore Install Guide (dortania.github.io/OpenCore-Install-Guide); this app does not copy it for you, because an automatic copy has left Macs unbootable."
    fi
  fi

fi
plutil -lint "$C" >/dev/null || stop "$C is not a valid plist - fix it before installing"

if [ -n "$VERB" ]; then
  # Verbose startup = boot argument -v. It goes in the config (OpenCore rewrites boot-args every boot when NVRAM Delete
  # lists it) AND in NVRAM now (a config without that Delete entry keeps whatever NVRAM already holds).
  case "$VERB" in on|off) ;; *) stop "--verbose takes on or off";; esac
  step "Turning verbose startup $VERB"
  BKC="$C.nullmoth-verbose-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"
  args=$(get NVRAM.Add.$B.boot-args); new=""
  for a in $args; do [ "$a" = -v ] || new="$new $a"; done
  [ "$VERB" = on ] && new="$new -v"; new=${new# }
  if has NVRAM.Add.$B.boot-args; then plutil -replace NVRAM.Add.$B.boot-args -string "$new" "$C"
  else plutil -insert NVRAM.Add.$B.boot-args -string "$new" "$C"; fi
  plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
  ok "OpenCore boot-args: $new"
  cur=$(nvram boot-args 2>/dev/null | cut -f2-); nv=""
  for a in $cur; do [ "$a" = -v ] || nv="$nv $a"; done
  [ "$VERB" = on ] && nv="$nv -v"; nv=${nv# }
  nvram boot-args="$nv" && ok "NVRAM boot-args: $nv" || note "could not write NVRAM boot-args; the config change still applies"
  ok "verbose startup $VERB - takes effect at the next restart"; cleanup; echo "RESULT ok"; exit 0
fi

if [ -n "${SETLOGO:-}" ]; then
  # The app's "Boot logo on/off": only the logo driver's entry changes (backup first, restored on any failure).
  # -nmlogooff in boot-args turns it off for one start without touching the config.
  case "$SETLOGO" in on|off) ;; *) stop "--set-logo takes on or off";; esac
  step "Turning the NullMoth boot logo $SETLOGO"
  BKC="$C.nullmoth-logo-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"
  LOGOSRC="$(dirname "${TOOL:-/nonexistent}")/$LOGO_FILE"; li=$(logo_index); fail=0
  if [ "$SETLOGO" = on ]; then
    ocl=$(ls -t "$MP"/opencore-*.txt 2>/dev/null | head -n 1)
    [ -n "$ocl" ] && grep -qE "NullMothLogo\.efi at [0-9]+ cannot be loaded" "$ocl" && \
      stop "this PC's firmware refused the boot logo driver at the last start (OpenCore's log on this EFI), so it stays off"
    # also when an entry exists: enabling it without the file left OpenCore pointing at a driver that was not there
    [ -f "$LOGOSRC" ] || stop "this copy of the app has no boot logo file"
    if [ -z "$li" ]; then
      has UEFI.Drivers || plutil -insert UEFI.Drivers -array "$C" || fail=1
      hv=""; plutil -extract UEFI.Drivers.0.HideVerbose raw -o - "$C" >/dev/null 2>&1 && hv=',"HideVerbose":false'
      plutil -insert UEFI.Drivers -json "{\"Arguments\":\"\",\"Comment\":\"NullMoth boot logo\",\"Enabled\":true,\"LoadEarly\":false,\"Path\":\"$LOGO_FILE\"$hv}" -append "$C" || fail=1
    else plutil -replace UEFI.Drivers.$li.Enabled -bool true "$C" || fail=1; fi
    [ ! -f "$LOGOSRC" ] || { mkdir -p "$MP/$OCREL/Drivers" && cp "$LOGOSRC" "$MP/$OCREL/Drivers/$LOGO_FILE"; } || fail=1
  elif [ -n "$li" ]; then plutil -replace UEFI.Drivers.$li.Enabled -bool false "$C" || fail=1; fi
  if [ $fail = 1 ] || ! plutil -lint "$C" >/dev/null; then cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; fi
  ok "boot logo $SETLOGO - takes effect at the next restart"; cleanup; echo "RESULT ok"; exit 0
fi

if [ -n "${BACKLIGHT:-}" ]; then
  # The app's "Built-in screen brightness": boot-arg nvrmacpibl=1 lets RM read the laptop panel's backlight through the
  # GPU's ACPI _DSM. It stays opt-in because that call hung GPU start on some PCs in 1.14; if it does, the boot picker's
  # "Start once without NVIDIA driver" entry starts macOS without the driver so this can be turned off again.
  case "$BACKLIGHT" in on|off) ;; *) stop "--backlight takes on or off";; esac
  step "Turning built-in screen brightness $BACKLIGHT"
  BKC="$C.nullmoth-backlight-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"
  new=""; for a in $(get NVRAM.Add.$B.boot-args); do [ "$a" = nvrmacpibl=1 ] || new="$new $a"; done
  [ "$BACKLIGHT" = on ] && new="$new nvrmacpibl=1"; new=${new# }
  if has NVRAM.Add.$B.boot-args; then plutil -replace NVRAM.Add.$B.boot-args -string "$new" "$C"
  else plutil -insert NVRAM.Add.$B.boot-args -string "$new" "$C"; fi
  plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
  nv=""; for a in $(nvram boot-args 2>/dev/null | cut -f2-); do [ "$a" = nvrmacpibl=1 ] || nv="$nv $a"; done
  [ "$BACKLIGHT" = on ] && nv="$nv nvrmacpibl=1"
  nvram boot-args="${nv# }" || note "could not write NVRAM boot-args; the config change still applies"
  ok "built-in screen brightness $BACKLIGHT - takes effect at the next restart"; cleanup; echo "RESULT ok"; exit 0
fi

if [ -n "${GPUTEST:-}" ]; then
  # The app's "AMD / Intel GPU driver (test)": NMXGPU (amdgpu, i915 and xe for macOS) for the AMD and Intel GPUs this Mac
  # has, opt-in per vendor with nmxamd=1 / nmxintel=1. On: the GPU firmware from linux-firmware at the commit the
  # manifest pins, every file SHA-256 checked; the kext into /Library/Extensions; a new auxiliary kernel collection,
  # checked to contain it; the boot argument. Off undoes each step. The "Start once without NVIDIA driver" entry
  # (-nvoff) parks this driver too, and taking a GPU that lights the screen resets that screen, so it stays opt-in.
  case "$GPUTEST" in on|off) ;; *) stop "--gpu-test takes on or off";; esac
  # the cached kexts live under the running macOS; NMXAccel is built per-OS (Tahoe privatised IOAcceleratorFamily2),
  # NMXGPU and NMXNPU are the same binaries on 15 and 26. A driver with no kexts for this macOS stops below.
  KMAJ=$(sw_vers -productVersion); KMAJ=${KMAJ%%.*}; KD=/Library/NullMoth/kexts/$KMAJ
  GK=$KD/NMXGPU.kext; GFW=/Library/NullMoth/firmware; EXT=/Library/Extensions
  GA=$KD/NMXAccel.kext; GU=/Library/NullMoth/gpu-userland; GB=/Library/GPUBundles
  GN=$KD/NMXNPU.kext
  MAN="$(dirname "${TOOL:-/nonexistent}")/nullmoth-gpu-firmware.txt"; [ -f "$MAN" ] || MAN="$(dirname "$0")/nullmoth-gpu-firmware.txt"
  # vendors of the display-class PCI functions present (class 03xxxx; ioreg prints vendor-id/class-code little-endian)
  # (the two keys come in either order, so each device's pair is printed when the next device starts)
  VENDORS=$(ioreg -r -c IOPCIDevice -l -w0 | LC_ALL=C awk -F'<|>' '/\+-o /{if (c=="03") print v; c=""; v=""}
    /"vendor-id" =/{v=substr($2,3,2) substr($2,1,2)} /"class-code" =/{c=substr($2,5,2)} END{if (c=="03") print v}' | sort -u)
  WANT=""; for v in $VENDORS; do case $v in 1002) WANT="$WANT -nmxamd";; 8086) WANT="$WANT -nmxintel";; esac; done
  # NPUs: Intel's is a processing accelerator (class 12xxxx), AMD's Ryzen AI NPU a signal-processing controller (1180xx)
  NPUS=$(ioreg -r -c IOPCIDevice -l -w0 | LC_ALL=C awk -F'<|>' 'function out(){if ((c=="12" && v=="8086") || (c=="11" && s=="80" && v=="1022")) print v}
    /\+-o /{out(); c=""; s=""; v=""} /"vendor-id" =/{v=substr($2,3,2) substr($2,1,2)} /"class-code" =/{c=substr($2,5,2); s=substr($2,3,2)} END{out()}' | sort -u)
  [ -n "$NPUS" ] && [ -d "$GN" ] && WANT="$WANT -nmxnpu"
  BKC="$C.nullmoth-gputest-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"
  ga() {  # $1 = add|del, $2 = boot argument; the config and NVRAM together
    local a new="" nv=""
    for a in $(get NVRAM.Add.$B.boot-args); do [ "$a" = "$2" ] || new="$new $a"; done
    [ "$1" = add ] && new="$new $2"; new=${new# }
    if has NVRAM.Add.$B.boot-args; then plutil -replace NVRAM.Add.$B.boot-args -string "$new" "$C"; else plutil -insert NVRAM.Add.$B.boot-args -string "$new" "$C"; fi
    for a in $(nvram boot-args 2>/dev/null | cut -f2-); do [ "$a" = "$2" ] || nv="$nv $a"; done
    [ "$1" = add ] && nv="$nv $2"; nvram boot-args="${nv# }" || note "could not write NVRAM boot-args"; }
  auxkc() {
    K=/System/Library/Kernels/kernel; KA=(--allow-missing-kdk); [ -f "$K" ] && KA+=(--kernel "$K")
    kmutil create -n aux --volume-root / "${KA[@]}" -B /System/Library/KernelCollections/BootKernelExtensions.kc \
      -S /System/Library/KernelCollections/SystemKernelExtensions.kc --repository "$EXT" \
      -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc -z > "$ST/gputest-kmutil.log" 2>&1; }
  if [ "$GPUTEST" = on ]; then
    [ -n "$WANT" ] || stop "this Mac has no AMD or Intel GPU for this driver"
    # A Mac with only an AMD or Intel GPU never ran the NVIDIA install (it stops at "no NVIDIA GPU"), so the test driver
    # was never on disk and this switch could not work for the very Macs it is for. The app passes the downloaded,
    # checksum-verified package: its AMD/Intel kexts, GPU userland and NPU files are taken from a root-owned copy.
    if [ ! -d "$GK" ] && [ -n "$PKG" ] && [ -n "$SHA" ] && [ -f "$PKG" ]; then
      step "Taking the AMD/Intel test driver from the downloaded package"
      PKGDIR=$(mktemp -d /var/tmp/nullmoth-pkg.XXXX) || stop "could not make a folder to check the package in"
      cp "$PKG" "$PKGDIR/" || stop "could not copy the package for checking"
      PK=$PKGDIR/$(basename "$PKG")
      [ "$(shasum -a 256 "$PK" | awk '{print $1}')" = "$SHA" ] || stop "package checksum does not match - download it again"
      tar -xzf "$PK" -C "$PKGDIR" || stop "could not unpack the package"
      PR="$PKGDIR/pkgroot"; (cd "$PR" && shasum -a 256 -c SHA256SUMS --quiet) || stop "the package's files do not match its SHA256SUMS"
      [ -d "$PR/Library/NullMoth/kexts/$KMAJ/NMXGPU.kext" ] || stop "this driver package has no AMD/Intel GPU driver for macOS $KMAJ"
      mkdir -p /Library/NullMoth
      for d in kexts gpu-userland npu; do [ ! -d "$PR/Library/NullMoth/$d" ] || ditto "$PR/Library/NullMoth/$d" "/Library/NullMoth/$d" || stop "could not copy $d"; done
      chown -R root:wheel /Library/NullMoth/kexts && chmod -R go-w /Library/NullMoth/kexts
      ok "AMD/Intel test driver taken from the package (no NVIDIA driver installed)"
    fi
    [ -d "$GK" ] || stop "this driver package has no AMD/Intel GPU driver (NMXGPU) - download the driver package first"
    [ -f "$MAN" ] || stop "this copy of the app has no GPU firmware list"
    step "Downloading the GPU firmware (linux-firmware, every file checked)"
    COMMIT=$(awk 'NR==2{print}' "$MAN" | grep -oE 'raw/[0-9a-f]{40}' | cut -d/ -f2); [ -n "$COMMIT" ] || stop "the GPU firmware list names no commit"
    FT="$GFW.new"; rm -rf "$FT"; mkdir -p "$FT" || stop "cannot create $FT"
    # Only this Mac's hardware's firmware (the list holds every vendor's: 699 AMD GPU files alone). 1.14 test, 10-11: all
    # 847 files 8 at a time drew HTTP 429 from GitLab on an NPU-only Mac that needed 3 of them.
    FWDIRS="LICENSES/"
    for v in $VENDORS; do case $v in 1002) FWDIRS="$FWDIRS amdgpu/";; 8086) FWDIRS="$FWDIRS i915/ xe/";; esac; done
    for v in $NPUS; do case $v in 8086) FWDIRS="$FWDIRS intel/vpu/";; 1022) FWDIRS="$FWDIRS amdnpu/";; esac; done
    # shasum -c reads "<sha>  <path>" (two spaces); the list has one. WAS: the list was fed as is, so every check failed
    # and --gpu-test on could never finish (1.14 test on an NPU-only Mac, 10-11)
    FWLIST="$FT.list"
    { grep -v '^#' "$MAN" | awk -v dirs="$FWDIRS" 'BEGIN{n=split(dirs,d," ")} {for(i=1;i<=n;i++) if (index($2,d[i])==1) {print $1 "  " $2; next}}' > "$FWLIST" \
      && [ -s "$FWLIST" ]; } || { rm -rf "$FT" "$FWLIST"; stop "the GPU firmware list has nothing for this Mac's hardware"; }
    # One archive per folder first: fetching hundreds of files one by one drew HTTP 429 from GitLab and 503 from
    # kernel.org on every try (1.14.1 reports NM-PN3THF98 and five more, an AMD GPU needs ~700 files). Only the listed
    # files are kept from an archive, and every file is still checked against the list below.
    FA="$FT.arc"
    for d in $FWDIRS; do
      rm -rf "$FA"; mkdir -p "$FA" || break
      curl -fsSL --retry 4 --retry-delay 5 "https://gitlab.com/kernel-firmware/linux-firmware/-/archive/$COMMIT/linux-firmware-$COMMIT.tar.gz?path=${d%/}" \
        | tar -xzf - -C "$FA" --strip-components 1 2>/dev/null || { note "folder archive for $d not available, fetching its files one by one"; continue; }
      awk -v d="$d" 'index($2,d)==1{print $2}' "$FWLIST" | while IFS= read -r p; do
        [ -f "$FA/$p" ] && mkdir -p "$(dirname "$FT/$p")" && mv -f "$FA/$p" "$FT/$p"; done
    done
    rm -rf "$FA"
    # what is still missing: each file from GitLab, else from kernel.org's mirror of the same commit, 2 at a time;
    # curl's retries back off on 429 and 5xx. The file is the script's $2 (macOS xargs -I refuses a command over
    # 255 bytes, so no {} substitution)
    awk '{print $2}' "$FWLIST" | while IFS= read -r p; do [ -f "$FT/$p" ] || printf '%s\n' "$p"; done | xargs -P 2 -n 1 sh -c 'mkdir -p "$(dirname "$0/$2")" &&
      { curl -fsSL --retry 5 --retry-delay 2 -o "$0/$2" "https://gitlab.com/kernel-firmware/linux-firmware/-/raw/$1/$2" ||
        curl -fsSL --retry 5 --retry-delay 2 -o "$0/$2" "https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/plain/$2?id=$1"; }' "$FT" "$COMMIT" \
      || { rm -rf "$FT" "$FWLIST"; stop "downloading the GPU firmware failed (network?) - nothing was changed"; }
    (cd "$FT" && shasum -a 256 -c --quiet < "$FWLIST") >/dev/null 2>&1 || { rm -rf "$FT" "$FWLIST"; stop "a GPU firmware file did not match its checksum - nothing was changed"; }
    # the names drivers ask for that linux-firmware publishes as links (WHENCE "Link:"), within one directory only
    grep '^#link ' "$MAN" | while read -r _ n _ tgt; do
      case "$n/$tgt" in *..*|/*|*//*) continue;; esac
      case "$tgt" in */*) continue;; esac
      [ -f "$FT/$(dirname "$n")/$tgt" ] && ln -sfh "$tgt" "$FT/$n"
    done
    rm -rf "$GFW" && mv "$FT" "$GFW" && chown -R root:wheel "$GFW" && chmod -R go-w,a+rX "$GFW" || stop "cannot install the GPU firmware"
    ok "GPU firmware installed ($(wc -l < "$FWLIST" | tr -d ' ') files for this Mac, $(du -sh "$GFW" | cut -f1))"; rm -f "$FWLIST"
    step "Installing the AMD/Intel GPU driver"
    rm -rf "$EXT/NMXGPU.kext" && ditto "$GK" "$EXT/NMXGPU.kext" && chown -R root:wheel "$EXT/NMXGPU.kext" && chmod -R 755 "$EXT/NMXGPU.kext" || stop "cannot copy NMXGPU.kext"
    # Metal on the AMD/Intel GPU: its accelerator (NMXAccel) and the Vulkan drivers the Metal plugin runs on it (RADV, ANV)
    if [ -d "$GA" ] && [ -d "$GU" ]; then
      rm -rf "$EXT/NMXAccel.kext" && ditto "$GA" "$EXT/NMXAccel.kext" && chown -R root:wheel "$EXT/NMXAccel.kext" && chmod -R 755 "$EXT/NMXAccel.kext" || stop "cannot copy NMXAccel.kext"
      ditto "$GU" "$GB/nvmtl" && chown -R root:wheel "$GB/nvmtl" && chmod -R go-w,a+rX "$GB/nvmtl" || stop "cannot copy the AMD/Intel Vulkan drivers"
    else
      note "this driver package has no AMD/Intel Metal accelerator: the GPU gets a screen, apps get no Metal on it"
    fi
    # the NPU driver (NMXNPU: Intel NPU, AMD Ryzen AI), when this Mac has an NPU
    if [ -n "$NPUS" ] && [ -d "$GN" ]; then
      rm -rf "$EXT/NMXNPU.kext" && ditto "$GN" "$EXT/NMXNPU.kext" && chown -R root:wheel "$EXT/NMXNPU.kext" && chmod -R 755 "$EXT/NMXNPU.kext" || stop "cannot copy NMXNPU.kext"
    fi
    # AMD NPU: the validation archive xrt-smi validate runs on this NPU's family (AMD's VTD, pinned, every file checked).
    # Only the family present is fetched (the three together are 150 MB). Without it the NPU still works, only
    # validate cannot, so a failed download is a note, not a stop.
    AMDNPU=$(ioreg -r -c IOPCIDevice -l -w0 | LC_ALL=C awk -F'<|>' 'function out(){if (c=="11" && s=="80" && v=="1022") print d}
      /\+-o /{out(); c=""; s=""; v=""; d=""} /"vendor-id" =/{v=substr($2,3,2) substr($2,1,2)} /"device-id" =/{d=substr($2,3,2) substr($2,1,2)}
      /"class-code" =/{c=substr($2,5,2); s=substr($2,3,2)} END{out()}' | sort -u)
    VT="$(dirname "$MAN")/nullmoth-npu-tests.txt"
    VB=/Library/NullMoth/npu/xrt/share/amdxdna/bins
    if [ -n "$AMDNPU" ] && [ -d "$EXT/NMXNPU.kext" ] && [ -f "$VT" ]; then
      VC=$(awk 'NR==2{print}' "$VT" | grep -oE 'VTD/[0-9a-f]{40}' | cut -d/ -f2)
      for d in $AMDNPU; do case $d in 1502) echo phx;; 17f0) echo strx;; 17f1|17f2|17f3|1b0a|1b0b|1b0c) echo npu3;; esac; done | sort -u | while read -r fam; do
        grep -v '^#' "$VT" | awk -v f="$fam" '$2==f || $2=="license"' | while read -r sum _ p; do
          case "$p" in *..*|/*) continue;; esac
          if mkdir -p "$VB" && curl -fsSL --retry 3 -o "$VB/.dl" "https://raw.githubusercontent.com/Xilinx/VTD/$VC/$p" \
             && [ "$(shasum -a 256 "$VB/.dl" | cut -d' ' -f1)" = "$sum" ]; then mv -f "$VB/.dl" "$VB/$(basename "$p")"
          else rm -f "$VB/.dl"; note "could not fetch the NPU validation archive $p (xrt-smi validate needs it; the NPU works without it)"; fi
        done
      done
      [ ! -d "$VB" ] || { chown -R root:wheel "$VB"; chmod -R go-w,a+rX "$VB"; }
    fi
    if ! auxkc || ! kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null | grep -q com.nullmoth.NMXGPU \
       || { [ -d "$EXT/NMXAccel.kext" ] && ! kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null | grep -q com.nullmoth.NMXAccel; } \
       || { [ -d "$EXT/NMXNPU.kext" ] && ! kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null | grep -q com.nullmoth.NMXNPU; }; then
      rm -rf "$EXT/NMXGPU.kext" "$EXT/NMXAccel.kext" "$EXT/NMXNPU.kext"; auxkc; cp -p "$BKC" "$C"; stop "the kernel collection did not take NMXGPU/NMXAccel/NMXNPU (log: $ST/gputest-kmutil.log) - removed again"
    fi
    for a in $WANT; do ga add "${a#-}=1"; done
    # the drivers' boot guards: an attach that never returned parks the driver until on/off clears this
    nvram -d nullmoth-npu-attaching 2>/dev/null; nvram -d nullmoth-gpu-attaching 2>/dev/null
  else
    step "Removing the AMD/Intel GPU driver"
    ga del nmxamd=1; ga del nmxintel=1; ga del nmxnpu=1
    nvram -d nullmoth-npu-attaching 2>/dev/null; nvram -d nullmoth-gpu-attaching 2>/dev/null
    if [ -d "$EXT/NMXGPU.kext" ] || [ -d "$EXT/NMXAccel.kext" ] || [ -d "$EXT/NMXNPU.kext" ]; then
      rm -rf "$EXT/NMXGPU.kext" "$EXT/NMXAccel.kext" "$EXT/NMXNPU.kext" && auxkc || note "the kernel collection rebuild reported an error (log: $ST/gputest-kmutil.log)"
    fi
    # the AMD/Intel Vulkan drivers come out of nvmtl by the names the cache holds; NVK's files are never touched
    if [ -d "$GU" ]; then (cd "$GU" && find . -type f) | while read -r f; do rm -f "$GB/nvmtl/${f#./}"; done; rmdir "$GB/nvmtl/share/libdrm" "$GB/nvmtl/share" 2>/dev/null; fi
    rm -rf "$GFW" /Library/NullMoth/npu/xrt/share/amdxdna/bins
  fi
  plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
  ok "AMD/Intel GPU driver $GPUTEST - takes effect at the next restart"; cleanup; echo "RESULT ok"; exit 0
fi

# Macs prepared for macOS 26 by 1.13 to 1.15 never ran "finish": the helper's launchd file could not be read, so the
# driver stayed parked (-nvoff) on the new macOS. Any setup run on the new macOS finishes that update first.
if [ -z "$UPD" ] && [ $DRY = 0 ] && [ -f "$ST/update-pending" ]; then
  from0=$(sed -n 's/^FROM=//p' "$ST/update-pending"); now0=$(sw_vers -productVersion); now0=${now0%%.*}
  if [ -n "$from0" ] && [ "$from0" != "$now0" ]; then
    note "this Mac was updated from macOS $from0 to $now0 but the driver was never set up for it - doing that first"
    UPD=finish
  fi
fi
if [ -n "$UPD" ]; then
  # A macOS upgrade boots Apple's installer and then the new macOS before our driver matches it. Measured 10-07 on the
  # RTX 5060: the installer's screen freezes with the full BAR, and with the driver loaded on the small BAR the screen
  # sticks. So "prepare" parks the driver (-nvoff) and puts the installer settings back (small BAR, firmware framebuffer);
  # the first start of the new macOS runs "finish" from the LaunchDaemon: the matching NVAccel, a new kernel collection,
  # the driver settings back, one restart.
  MAJ=$(sw_vers -productVersion); MAJ=${MAJ%%.*}; PEND="$ST/update-pending"
  args_set() {  # $1 = add|del, $2 = boot argument; edits the config and NVRAM together
    local a new="" cur nv=""
    for a in $(get NVRAM.Add.$B.boot-args); do [ "$a" = "$2" ] || new="$new $a"; done
    [ "$1" = add ] && new="$new $2"; new=${new# }
    if has NVRAM.Add.$B.boot-args; then plutil -replace NVRAM.Add.$B.boot-args -string "$new" "$C"; else plutil -insert NVRAM.Add.$B.boot-args -string "$new" "$C"; fi
    cur=$(nvram boot-args 2>/dev/null | cut -f2-); for a in $cur; do [ "$a" = "$2" ] || nv="$nv $a"; done
    [ "$1" = add ] && nv="$nv $2"; nvram boot-args="${nv# }" || note "could not write NVRAM boot-args"; }
  setq() { if has "$1"; then plutil -replace "$1" -integer "$2" "$C"; else plutil -insert "$1" -integer "$2" "$C"; fi; }
  backup() { BKC="$C.nullmoth-update-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"; }
  do_prepare() {
    step "Preparing this Mac for the macOS 26 Tahoe update"
    ls /Library/NullMoth/kexts/*/NVAccel.kext >/dev/null 2>&1 || stop "this Mac still has driver 1.0, which has no macOS 26 kexts"
    backup
    # finish puts back this machine's own BAR settings, not fixed values: a fixed resize hung some firmware (1.14.7)
    bar0=$(get UEFI.Quirks.ResizeGpuBars); abar0=$(get Booter.Quirks.ResizeAppleGpuBars)
    setq UEFI.Quirks.ResizeGpuBars -1; setq Booter.Quirks.ResizeAppleGpuBars 0
    bi=$(bidx); [ -n "$bi" ] && plutil -replace Kernel.Block.$bi.Enabled -bool false "$C"
    args_set add -nvoff
    plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
    printf 'FROM=%s\nBAR0=%s\nABAR0=%s\n' "$MAJ" "$bar0" "$abar0" > "$PEND"
    cat > /Library/LaunchDaemons/com.nullmoth.osupdate.plist <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.osupdate</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>-c</string><string>/bin/bash $ST/nullmoth-setup.sh --update finish &gt;&gt; $ST/osupdate.log 2&gt;&amp;1</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PL
    # a plist launchd cannot parse never runs, and the driver would stay parked on the new macOS
    plutil -lint /Library/LaunchDaemons/com.nullmoth.osupdate.plist >/dev/null || {
      cp -p "$BKC" "$C"; rm -f "$PEND" /Library/LaunchDaemons/com.nullmoth.osupdate.plist; args_set del -nvoff
      stop "the update helper's launchd file is not valid; the config is restored and nothing was changed for the update"; }
    chmod 644 /Library/LaunchDaemons/com.nullmoth.osupdate.plist; chown root:wheel /Library/LaunchDaemons/com.nullmoth.osupdate.plist
    cp "$0" "$ST/nullmoth-setup.sh" 2>/dev/null; chmod 755 "$ST/nullmoth-setup.sh"
    ok "driver parked (-nvoff), installer screen settings set - now update macOS from System Settings"
    ok "after the update, the first start sets the driver up for the new macOS and restarts once by itself"; }
  case "$UPD" in
  prepare)
    if [ $INSTALL_THEN_PREPARE = 1 ]; then :   # handled after the install below
    else do_prepare; cleanup; echo "RESULT ok"; exit 0; fi;;
  finish|cancel)
    . "$PEND" 2>/dev/null
    if [ "$UPD" = finish ] && [ "${FROM:-}" = "$MAJ" ] && [ "$MAJ" = 15 ]; then echo "$(date) still on macOS $MAJ - waiting for the update"; cleanup; echo "RESULT ok"; exit 0; fi
    step "Setting the driver up for macOS $MAJ"; backup   # only once the update has happened: no backup per waiting boot
    if [ -d "/Library/NullMoth/kexts/$MAJ/NVAccel.kext" ]; then
      rm -rf /Library/Extensions/NVAccel.kext && ditto "/Library/NullMoth/kexts/$MAJ/NVAccel.kext" /Library/Extensions/NVAccel.kext
      chown -R root:wheel /Library/Extensions/NV*.kext; chmod -R go-w /Library/Extensions/NV*.kext
      K=/System/Library/Kernels/kernel; KARG=(--allow-missing-kdk); [ -f "$K" ] && KARG+=(--kernel "$K")
      kmutil create -n aux --volume-root / "${KARG[@]}" -B /System/Library/KernelCollections/BootKernelExtensions.kc \
        -S /System/Library/KernelCollections/SystemKernelExtensions.kc --repository /Library/Extensions \
        -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc -z > "$ST/osupdate-kmutil.log" 2>&1
      INS=$(kmutil inspect -a x86_64 -A /Library/KernelCollections/AuxiliaryKernelExtensions.kc 2>/dev/null)
      for k in NVRM NVAccel NVRMFB NVRMAGDC; do echo "$INS" | grep -q "com.nullmoth.$k" || { cp -p "$BKC" "$C"; stop "com.nullmoth.$k is not in the new kernel collection - the driver stays parked (log: $ST/osupdate-kmutil.log)"; }; done
      echo "$MAJ" > "$ST/os-major"; ok "macOS $MAJ kernel collection built with all four NullMoth kexts"
    else
      cp -p "$BKC" "$C"; stop "this driver has no kexts for macOS $MAJ - it stays parked; install a driver that supports macOS $MAJ"
    fi
    setq UEFI.Quirks.ResizeGpuBars "${BAR0:-13}"; setq Booter.Quirks.ResizeAppleGpuBars "${ABAR0:--1}"
    bi=$(bidx); [ -n "$bi" ] && plutil -replace Kernel.Block.$bi.Enabled -bool true "$C"
    args_set del -nvoff
    plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
    rm -f "$PEND" /Library/LaunchDaemons/com.nullmoth.osupdate.plist
    ok "driver settings restored for macOS $MAJ"
    cleanup; echo "RESULT ok"
    [ "$UPD" = finish ] && { sleep 2; /sbin/shutdown -r now; }
    exit 0;;
  *) stop "--update takes prepare, finish or cancel";;
  esac
fi

if [ -n "$USBMAP" ]; then
  step "Installing the USB map"
  # 10-07: 1401 Mac 1.0.1 passed the .kext itself, so "$USBMAP/UTBMap.kext" never existed and every map
  # stopped as "not valid". Take the folder holding the kext or the kext itself.
  case "$USBMAP" in */UTBMap.kext|*/UTBMap.kext/) K="${USBMAP%/}";; *) K="$USBMAP/UTBMap.kext";; esac
  [ -f "$K/Contents/Info.plist" ] || stop "the USB map the app wrote is missing ($K)"
  plutil -lint "$K/Contents/Info.plist" >/dev/null || stop "the USB map the app wrote is not valid"
  KD="$(dirname "$C")/Kexts"
  kidx() { local i=0 p; while p=$(plutil -extract Kernel.Add.$i.BundlePath raw -o - "$C" 2>/dev/null); do [ "$p" = "$1" ] && { echo $i; return; }; i=$((i+1)); done; }
  # The map is a USBToolBox map. 1.10 stopped when the EFI had no USBToolBox (10-10, an EFI not built by 1401); fetch it.
  NEEDUTB=0; [ -d "$KD/USBToolBox.kext" ] || { fetch_kext USBToolBox.kext "$KD"; NEEDUTB=1; echo "CHANGE Kexts: add USBToolBox.kext (downloaded, SHA-256 checked)"; }
  BKC="$C.nullmoth-usb-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"; ok "backed up the config to $BKC"
  [ $NEEDUTB = 0 ] || ditto "$KT/USBToolBox.kext.x/USBToolBox.kext" "$KD/USBToolBox.kext" || stop "could not copy USBToolBox.kext"
  [ -n "$(kidx USBToolBox.kext)" ] || { plutil -insert Kernel.Add -json '{"Arch":"Any","BundlePath":"USBToolBox.kext","Comment":"USBToolBox (NullMoth USB map)","Enabled":true,"ExecutablePath":"Contents/MacOS/USBToolBox","MaxKernel":"","MinKernel":"","PlistPath":"Contents/Info.plist"}' -append "$C" \
    || { cp -p "$BKC" "$C"; stop "could not add USBToolBox.kext to the config"; }; echo "CHANGE Kernel -> Add: USBToolBox.kext"; }
  [ -d "$KD/UTBMap.kext" ] && { mv "$KD/UTBMap.kext" "$KD/UTBMap.kext.nullmoth-$(date +%Y%m%d-%H%M%S)" || stop "could not move the old map aside"; note "the old UTBMap.kext was kept beside it"; }
  cp -R "$K" "$KD/UTBMap.kext" || { cp -p "$BKC" "$C"; stop "could not copy the map"; }
  fail=0
  i=$(kidx USBToolBox.kext); plutil -replace Kernel.Add.$i.Enabled -bool true "$C" || fail=1
  i=$(kidx UTBDefault.kext); [ -n "$i" ] && { plutil -replace Kernel.Add.$i.Enabled -bool false "$C" || fail=1; echo "CHANGE UTBDefault.kext: off (the all-ports map)"; }
  i=$(kidx UTBMap.kext)
  if [ -n "$i" ]; then plutil -replace Kernel.Add.$i.Enabled -bool true "$C" || fail=1
  else plutil -insert Kernel.Add -json '{"Arch":"Any","BundlePath":"UTBMap.kext","Comment":"NullMoth USB map","Enabled":true,"ExecutablePath":"","MaxKernel":"","MinKernel":"","PlistPath":"Contents/Info.plist"}' -append "$C" || fail=1
    echo "CHANGE Kernel -> Add: UTBMap.kext (after USBToolBox.kext)"; fi
  if [ $fail = 1 ] || ! plutil -lint "$C" >/dev/null; then cp -p "$BKC" "$C"; rm -rf "$KD/UTBMap.kext"; stop "editing the config failed - the original is restored"; fi
  ok "USB map installed - restart to use it"; cleanup; echo "RESULT ok"; exit 0
fi

if [ $DRY = 0 ]; then
  step "Test-building the driver (nothing is changed yet)"
  T=$(mktemp -d /var/tmp/nullmoth.XXXX)
  tar -xzf "$PKG" -C "$T" || stop "could not unpack the package"
  [ -x "$T/pkgroot/install.sh" ] || stop "the package has no install.sh"
  out=$(CHECK=1 /bin/bash "$INSTALLER" --payload "$T/pkgroot" 2>&1); rc=$?
  echo "$out" | sed -E '/^$/d;s/^== /NOTE /;s/^   ok  /NOTE /;s/^   STOP: /STOP /;s/^   NOTE: /NOTE /'
  [ "$rc" = 0 ] || stop "the driver test build failed (exit $rc) - OpenCore and macOS were not changed"
  rm -rf "$T"; T=""
fi
# SIP is read from the running system: a config value only counts once OpenCore has applied it at a boot.
SIPON=0; csrutil status 2>/dev/null | grep -q 'status: enabled\.' && SIPON=1

step "Checking the OpenCore settings"
EDITS=(); ADDED=""; REMOVED=""
args=$(get NVRAM.Add.$B.boot-args); new=""
for a in $args; do case " $DROP_ARGS " in *" $a "*) echo "CHANGE boot-args: remove $a (it hides the NVIDIA card)"; REMOVED="$REMOVED $a";; *) new="$new $a";; esac; done
for a in $WANT_ARGS; do
  case "$a" in nvrmsettle=*) case " $new " in *" nvrmsettle="*) continue;; esac;; esac   # the user's own value wins
  case " $new " in *" $a "*) ;; *) new="$new $a"; ADDED="$ADDED $a"; echo "CHANGE boot-args: add $a";; esac; done
new=${new# }
[ "$new" != "$args" ] && EDITS+=("NVRAM.Add.$B.boot-args|-string|$new")
cur=$(csr_of); want=$(( cur | SIP_BITS ))
if [ $want != $cur ]; then
  echo "CHANGE csr-active-config: $(printf '0x%04X' $cur) -> $(printf '0x%04X' $want) (the SIP bits the driver needs)"
  EDITS+=("NVRAM.Add.$B.csr-active-config|-data|$(csr_data $want)")
fi
sbm=$(get Misc.Security.SecureBootModel); OLDSBM=""
if [ "$sbm" != Disabled ]; then
  echo "CHANGE SecureBootModel: ${sbm:-unset} -> Disabled (Apple Secure Boot refuses kexts Apple did not sign)"
  EDITS+=("Misc.Security.SecureBootModel|-string|Disabled"); OLDSBM=${sbm:-Default}
fi
# AMFIPass (a Lilu plugin) is what lets WindowServer, a platform binary, load the ad hoc signed driver: with it off and
# the same SIP bits and amfi boot-args, 1.9.0 logs (10-10, Ryzen 9 9900X) show "mapping process is a platform binary,
# but mapped file is not", no Metal device and a black screen with a cursor. Every working machine has it on.
kpos() { local i=0 p; while p=$(get Kernel.Add.$i.BundlePath); do [ "$p" = "$1" ] && { echo $i; return; }; i=$((i+1)); done; }
KX="$(dirname "$C")/Kexts"; ADDK=(); FETCH=()
# 1.10 stopped here when either kext was missing, and 1401 had only built AMFIPass for OCLP Wi-Fi, so updating users were
# sent to rebuild an EFI that still lacked it (10-10: 9 uploads, several chat reports). Setup now fetches them itself,
# from the same pinned files 1401 builds with (p1401/mirror.json), and checks each SHA-256 before anything is changed.
for k in Lilu.kext AMFIPass.kext; do
  ki=$(kpos $k)
  [ -d "$KX/$k" ] || { FETCH+=($k); echo "CHANGE Kexts: add $k to $KX (downloaded, SHA-256 checked)"; }
  if [ -n "$ki" ]; then
    [ "$(get Kernel.Add.$ki.Enabled)" = true ] || { echo "CHANGE Kernel -> Add: turn $k on (the driver cannot load without it)"; EDITS+=("Kernel.Add.$ki.Enabled|-bool|true"); }
  else
    ADDK+=($k); echo "CHANGE Kernel -> Add: add $k (the driver cannot load without it)"
  fi
done
for k in "${FETCH[@]+"${FETCH[@]}"}"; do fetch_kext $k "$KX"; done
lp=$(kpos Lilu.kext); ap=$(kpos AMFIPass.kext)
[ -n "$ap" ] && [ -n "$lp" ] && [ "$ap" -lt "$lp" ] && stop "AMFIPass.kext loads before Lilu.kext in Kernel -> Add; move it below Lilu in the config, then try again"
NEEDAMFIPASS=$(( ${#ADDK[@]} + ${#FETCH[@]} ))
# Tahoe preparation: macOS 26 refuses Mac models it dropped ("This version of Mac OS X is not supported on this platform").
# Every desktop 1401 built before 1.14 is iMacPro1,1, one of them, so the update stopped there on every such PC (10-12, the
# the test machine). Desktops take MacPro7,1, which runs macOS 15 and 26, with RestrictEvents (its memory and PCI slot pages),
# as 1401 1.14 builds them; a laptop on a dropped model is told to rebuild its EFI with 1401 1.14 (its model needs the
# laptop's own SMBIOS choice). The serial numbers stay: macOS does not check them against the model.
if [ "$UPD" = prepare ]; then
  MK=""; for k in PlatformInfo.Generic.SystemProductName PlatformInfo.SMBIOS.SystemProductName PlatformInfo.DataHub.SystemProductName; do
    has "$k" && { MK=$k; break; }; done
  MODEL=$( [ -n "$MK" ] && get "$MK" ); [ -n "$MODEL" ] || MODEL=$(sysctl -n hw.model)
  case " iMac20,1 iMac20,2 MacPro7,1 MacBookPro16,1 MacBookPro16,2 MacBookPro16,4 " in
    *" $MODEL "*) ok "Mac model $MODEL runs macOS 26" ;;
    *)
      case "$MODEL" in MacBook*) stop "this Mac is set up as $MODEL, which macOS 26 does not support; make a new EFI with 1401 1.14 (it picks a laptop model macOS 26 runs), then prepare again";; esac
      [ -n "$MK" ] || stop "the OpenCore config sets no Mac model (PlatformInfo); macOS 26 needs MacPro7,1 here - make a new EFI with 1401 1.14"
      # Every section that names the model changes, not only the first: with PlatformInfo.Automatic off, macOS reads the
      # SMBIOS/DataHub sections, and 1.14.1 changed Generic alone - the Mac stayed iMacPro1,1 (chat 10-12, botn0ught).
      # Those sections also carry the board ID the installer checks, so it follows the model.
      for k in PlatformInfo.Generic.SystemProductName PlatformInfo.SMBIOS.SystemProductName PlatformInfo.DataHub.SystemProductName; do
        has "$k" || continue
        v=$(get "$k"); [ "$v" = MacPro7,1 ] && continue
        echo "CHANGE $k: $v -> MacPro7,1 (macOS 26 does not run $v; MacPro7,1 runs macOS 15 and 26)"
        EDITS+=("$k|-string|MacPro7,1")
      done
      for k in PlatformInfo.SMBIOS.BoardProduct PlatformInfo.DataHub.BoardProduct; do
        has "$k" || continue
        v=$(get "$k"); [ "$v" = Mac-27AD2F918AE68F61 ] && continue
        echo "CHANGE $k: $v -> Mac-27AD2F918AE68F61 (MacPro7,1's board)"
        EDITS+=("$k|-string|Mac-27AD2F918AE68F61")
      done
      if [ -z "$(kpos RestrictEvents.kext)" ]; then
        [ -d "$KX/RestrictEvents.kext" ] || { fetch_kext RestrictEvents.kext "$KX"; FETCH+=(RestrictEvents.kext); }
        ADDK+=(RestrictEvents.kext); echo "CHANGE Kernel -> Add: add RestrictEvents.kext (MacPro7,1's memory and PCI pages)"
      fi ;;
  esac
fi
# Existing BAR and kernel-placement settings belong to the EFI that started this system.
# A fixed 8 GB resize hung reported firmware before the picker, and updates must not
# replace a working firmware configuration with one tested on a different board.
bar=$(get UEFI.Quirks.ResizeGpuBars); abar=$(get Booter.Quirks.ResizeAppleGpuBars)
note "preserving this OpenCore config's GPU BAR and kernel-placement settings"
# The installer runs on the firmware framebuffer (IONDRVSupport); once the driver is in, that framebuffer would take
# display index 0 from NVRMFB, so the installed system excludes it (as on the tested RTX 5060 setup).
NEEDBLOCK=0; bi=$(bidx)
# Laptops whose built-in panel is wired to an iGPU macOS has no driver for (Intel Tiger Lake and newer): 1401 1.12 keeps
# that GPU on (no disable-gpu, no framebuffer properties) so the panel shows macOS through the firmware framebuffer.
# Excluding IONDRVSupport there leaves the built-in screen dark (10-10: 86 of 184 scans had the panel on the Intel GPU).
IGPU="PciRoot(0x0)/Pci(0x2,0x0)"; INTELPANEL=0
# LC_ALL=C: ioreg prints raw bytes, and awk in a UTF-8 locale stops on them
if ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | LC_ALL=C awk '/\+-o /{v=0;c=0} /"vendor-id" = <86800000>/{v=1} /"class-code" = <00000300>/{c=1} v&&c{f=1} END{exit !f}' \
   && ! has "DeviceProperties.Add.$IGPU.disable-gpu" && ! has "DeviceProperties.Add.$IGPU.AAPL,ig-platform-id"; then
  INTELPANEL=1; note "the built-in screen runs on the Intel GPU through the firmware framebuffer; IONDRVSupport stays on"
fi
if [ $INTELPANEL = 1 ]; then
  [ -n "$bi" ] && [ "$(get Kernel.Block.$bi.Enabled)" = true ] && { EDITS+=("Kernel.Block.$bi.Enabled|-bool|false"); echo "CHANGE Kernel -> Block: turn the IONDRVSupport exclude off (it would leave the built-in screen dark)"; }
elif [ -z "$bi" ]; then NEEDBLOCK=1; echo "CHANGE Kernel -> Block: exclude IONDRVSupport (the firmware framebuffer would take NVRMFB's display)"
elif [ "$(get Kernel.Block.$bi.Enabled)" != true ]; then EDITS+=("Kernel.Block.$bi.Enabled|-bool|true"); echo "CHANGE Kernel -> Block: turn the IONDRVSupport exclude on"; fi
# AMD (AMD Vanilla patches): with Shaneee's "Fix PAT" on, the driver stopped at "RmInitAdapter failed! (0x25:0x40:1310)"
# right after a valid 8 GB BAR1 (user report 10-10, Ryzen 5 7600 + RTX 4060); switching to Algrey's "Fix PAT" - the one
# 1401 builds with - reached the desktop and nothing else changed. Swap them where Algrey's entry for the same kernel range
# is present but off; a config without it is left alone.
i=0
while cm=$(plutil -extract Kernel.Patch.$i.Comment raw -o - "$C" 2>/dev/null); do
  case "$cm" in
    *[Ss]haneee*[Ff]ix\ PAT*)
      if [ "$(get Kernel.Patch.$i.Enabled)" = true ]; then
        mn=$(get Kernel.Patch.$i.MinKernel); j=0
        while cj=$(plutil -extract Kernel.Patch.$j.Comment raw -o - "$C" 2>/dev/null); do
          case "$cj" in *[Aa]lgrey*[Ff]ix\ PAT*)
            if [ "$(get Kernel.Patch.$j.MinKernel)" = "$mn" ] && [ "$(get Kernel.Patch.$j.Enabled)" != true ]; then
              EDITS+=("Kernel.Patch.$i.Enabled|-bool|false" "Kernel.Patch.$j.Enabled|-bool|true")
              echo "CHANGE Kernel -> Patch: Fix PAT from Shaneee's to Algrey's (Shaneee's stops the driver at RmInitAdapter on AMD)"
            fi;;
          esac
          j=$((j+1))
        done
      fi;;
  esac
  i=$((i+1))
done
del=$(plutil -extract NVRAM.Delete.$B xml1 -o - "$C" 2>/dev/null); DELADD=()
for k in boot-args csr-active-config; do echo "$del" | grep -q "<string>$k</string>" || { DELADD+=("$k"); echo "CHANGE NVRAM Delete: add $k (so OpenCore rewrites it every boot)"; }; done
# A remove flag still set while installing is stale: installing means the driver is wanted. On some laptops and boards a
# delete made from macOS never reaches the firmware (user report 10-09: the driver was removed 2 seconds after every
# login, and "nvram -d" did not help), so the flag came back at every boot, kept the kexts off and ran the removal again.
# OpenCore writes through the firmware before macOS starts, so it clears the flag there. The picker tool still works on
# these machines when macOS is chosen in the same picker session, because the tool runs after OpenCore's delete.
# Booter > Quirks > DisableVariableWrite (1401 1.14 sets it on Ryzen 7000/8000/9000 desktops, which froze 19 minutes into
# the install) blocks every NVRAM write from macOS, so on those machines the flag is cleared by OpenCore from the start.
# Ryzen 7000/8000/9000 (Zen 4/5, AM5 and their laptops): 1401 1.14 turns DisableVariableWrite on for new EFIs, but 173 of
# 175 Ryzen EFIs uploaded before it had it off, and those installs kept letting macOS write the firmware's NVRAM through a
# rewritten memory map. On a TUF B650M a restart from macOS froze and the board needed an RMA (chat 10-12). Existing EFIs
# get the same setting here, the moment the driver is installed or updated. It needs OpenRuntime.efi (it is the quirk's
# host); without it nothing is changed and the reason is said. Pattern = 1401 policy.py _ZEN45_NAME.
DVW_NEW=0
CPUB=$(sysctl -n machdep.cpu.brand_string 2>/dev/null)
# The risk is not the CPU's name but macOS writing NVRAM through a rewritten firmware memory map: an AMD config with
# SetupVirtualMap or DevirtualiseMmio on. Boards that stopped POSTing (chat 10-10..10-15: AM5 X870/B650, an AM4 5600X)
# all ran such configs without this quirk. Zen 4/5 by name stays covered, and so does every remapped AMD config.
AMD_REMAP=0
if printf '%s' "$CPUB" | grep -q AMD && { [ "$(get Booter.Quirks.SetupVirtualMap)" = true ] || [ "$(get Booter.Quirks.DevirtualiseMmio)" = true ]; }; then
  AMD_REMAP=1; fi
if { printf '%s' "$CPUB" | grep -Eiq 'Ryzen AI([^A-Za-z0-9]|$)|Ryzen [3579] ([79][0-9][0-9]0(X3D|X|F)?|8[0-9][0-9]0[FG])([^A-Za-z0-9]|$)|Ryzen [3579] [78][0-9][45][0-9](HX3D|HX|HS|H|U)([^A-Za-z0-9]|$)' \
     || [ $AMD_REMAP = 1 ]; } \
   && [ "$(get Booter.Quirks.DisableVariableWrite)" != true ]; then
  ort=0; i=0
  while p=$(plutil -extract UEFI.Drivers.$i.Path raw -o - "$C" 2>/dev/null); do
    [ "$p" = OpenRuntime.efi ] && [ "$(get UEFI.Drivers.$i.Enabled)" != false ] && ort=1; i=$((i+1)); done
  if [ $ort = 1 ]; then
    DVW_NEW=1; EDITS+=("Booter.Quirks.DisableVariableWrite|-bool|true")
    echo "CHANGE Booter -> Quirks: DisableVariableWrite on ($CPUB: macOS NVRAM writes through the firmware froze AM5 boards)"
  else
    note "$CPUB: DisableVariableWrite should be on, but OpenRuntime.efi is not loaded - left as is; rebuild the EFI with 1401 1.14"
  fi
fi
if { nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove >/dev/null 2>&1 || [ "$(get Booter.Quirks.DisableVariableWrite)" = true ] || [ $DVW_NEW = 1 ]; } \
   && ! echo "$del" | grep -q "<string>nullmoth-remove</string>"; then
  DELADD+=("nullmoth-remove"); echo "CHANGE NVRAM Delete: add nullmoth-remove (a leftover remove flag this Mac cannot clear from macOS)"
fi
NEEDTOOL=0; [ -z "$(tool_index)" ] && { NEEDTOOL=1; echo "CHANGE boot picker: add \"$TOOL_NAME\" (the way back if the driver ever stops macOS starting)"; }
# Make the Remove entry reachable no matter how this config was built: the picker is shown (ShowPicker), it is a
# picker that lists tools (not Apple's), nothing is hidden behind Space (HideAuxiliary), and both NullMoth entries are
# non-auxiliary and in text mode so their on-screen question is visible. picker_edits prints only what needs changing.
# The prior ShowPicker/HideAuxiliary/PickerMode values are recorded so removal can put the user's boot picker back.
OLD_SHOWPICKER=$(get Misc.Boot.ShowPicker); OLD_HIDEAUX=$(get Misc.Boot.HideAuxiliary); OLD_PICKERMODE=$(get Misc.Boot.PickerMode)
SET_SHOWPICKER=0; SET_HIDEAUX=0; SET_PICKERMODE=0
while IFS= read -r e; do
  [ -n "$e" ] || continue
  EDITS+=("$e")
  case "$e" in
    Misc.Boot.ShowPicker\|*)   SET_SHOWPICKER=1; echo "CHANGE boot picker: show the OpenCore picker, so \"$TOOL_NAME\" can be chosen";;
    Misc.Boot.PickerMode\|*)   SET_PICKERMODE=1; echo "CHANGE boot picker: use the Builtin picker (Apple's picker never lists OpenCore tools like \"$TOOL_NAME\")";;
    Misc.Boot.HideAuxiliary\|*) SET_HIDEAUX=1; echo "CHANGE boot picker: stop hiding auxiliary entries, so \"$TOOL_NAME\" is not hidden behind Space";;
    Misc.Tools.*.Auxiliary\|*)  echo "CHANGE boot picker: always show \"$TOOL_NAME\" (not only after pressing Space)";;
    Misc.Tools.*.TextMode\|*)   echo "CHANGE boot picker: show \"$TOOL_NAME\" in text mode, so its question is visible";;
  esac
done < <(picker_edits "$C")
# Carry picker ownership across re-runs: a later install or update finds ShowPicker/HideAuxiliary/PickerMode already at
# our value and changes nothing, but the earlier run's record still owns the user's original - inherit it so removal can
# still put it back. The prior record is read in a subshell so only these values reach this shell.
eval "$(
  [ -f "$STATE" ] && . "$STATE" 2>/dev/null
  printf 'P_SET_SHOWPICKER=%q P_OLD_SHOWPICKER=%q P_SET_HIDEAUX=%q P_OLD_HIDEAUX=%q P_SET_PICKERMODE=%q P_OLD_PICKERMODE=%q\n' \
    "${SET_SHOWPICKER:-0}" "${OLD_SHOWPICKER:-}" "${SET_HIDEAUX:-0}" "${OLD_HIDEAUX:-}" "${SET_PICKERMODE:-0}" "${OLD_PICKERMODE:-}"
)"
[ "$SET_SHOWPICKER" = 0 ] && [ "${P_SET_SHOWPICKER:-0}" = 1 ] && { SET_SHOWPICKER=1; OLD_SHOWPICKER=$P_OLD_SHOWPICKER; }
[ "$SET_HIDEAUX" = 0 ] && [ "${P_SET_HIDEAUX:-0}" = 1 ] && { SET_HIDEAUX=1; OLD_HIDEAUX=$P_OLD_HIDEAUX; }
[ "$SET_PICKERMODE" = 0 ] && [ "${P_SET_PICKERMODE:-0}" = 1 ] && { SET_PICKERMODE=1; OLD_PICKERMODE=$P_OLD_PICKERMODE; }
ti=$(tool_index)
REFRESHTOOL=0; [ -n "$ti" ] && [ -f "$TOOL" ] && ! cmp -s "$TOOL" "$(dirname "$C")/Tools/$TOOL_FILE" && REFRESHTOOL=1
NEEDONCE=0; [ -z "$(once_index)" ] && { NEEDONCE=1; echo "CHANGE boot picker: add \"$ONCE_NAME\" (start macOS or Recovery one time without the driver)"; }
LOGOSRC="$(dirname "${TOOL:-/nonexistent}")/$LOGO_FILE"; NEEDLOGO=0; REFRESHLOGO=0; li=$(logo_index)
# The logo driver is refused by some firmware: a Gigabyte AORUS board logged "NullMothLogo.efi at 3 cannot be loaded -
# Unsupported!" and OpenCore went no further until the entry was turned off by hand (chat 10-13). OpenCore's own log on
# this EFI decides: a logo that failed to load or was not found at the last start is turned off and not turned on again.
LOGOREFUSED=0; ocl=$(ls -t "$MP"/opencore-*.txt 2>/dev/null | head -n 1)
[ -n "$ocl" ] && grep -qE "NullMothLogo\.efi at [0-9]+ cannot be (loaded|found)" "$ocl" && LOGOREFUSED=1
if [ $LOGOREFUSED = 1 ] && [ -n "$li" ] && [ "$(get UEFI.Drivers.$li.Enabled)" = true ]; then
  echo "CHANGE boot logo: turn the NullMoth boot logo off (this PC's firmware refused it at the last start)"; EDITS+=("UEFI.Drivers.$li.Enabled|-bool|false")
elif [ $LOGOREFUSED = 1 ]; then
  :
elif [ "$LOGO" != off ] && [ -f "$LOGOSRC" ]; then
  if [ -z "$li" ]; then NEEDLOGO=1; echo "CHANGE boot logo: add the NullMoth boot logo (turn it off in the app, or with -nmlogooff)"
  else
    [ "$(get UEFI.Drivers.$li.Enabled)" = true ] || { echo "CHANGE boot logo: turn the NullMoth boot logo on"; EDITS+=("UEFI.Drivers.$li.Enabled|-bool|true"); }
    cmp -s "$LOGOSRC" "$(dirname "$C")/Drivers/$LOGO_FILE" || { REFRESHLOGO=1; echo "CHANGE boot logo: update the NullMoth boot logo driver to this version"; }
  fi
elif [ "$LOGO" = off ] && [ -n "$li" ] && [ "$(get UEFI.Drivers.$li.Enabled)" = true ]; then
  echo "CHANGE boot logo: turn the NullMoth boot logo off (the Apple logo shows again)"; EDITS+=("UEFI.Drivers.$li.Enabled|-bool|false")
elif [ -n "$li" ] && [ "$(get UEFI.Drivers.$li.Enabled)" = true ] && [ ! -f "$(dirname "$C")/Drivers/$LOGO_FILE" ]; then
  # two OpenCore logs (1.14.3, 1.15) end at "NullMothLogo.efi at 3 cannot be found!": an enabled entry whose file was
  # never copied to this EFI, and this app has no file to put there
  echo "CHANGE boot logo: turn the NullMoth boot logo off (its driver file is missing from this EFI)"; EDITS+=("UEFI.Drivers.$li.Enabled|-bool|false")
fi
[ ${#EDITS[@]} = 0 ] && [ ${#DELADD[@]} = 0 ] && [ $NEEDTOOL = 0 ] && [ $NEEDONCE = 0 ] && [ $NEEDLOGO = 0 ] && [ $NEEDBLOCK = 0 ] && [ $NEEDAMFIPASS = 0 ] && ok "OpenCore already has every setting the driver needs"

if [ $SIPON = 1 ]; then
  note "SIP is still fully on in this boot: only SIP, Secure Boot, boot arguments and the boot picker entry change now"
  KEEP=(); for e in "${EDITS[@]}"; do case "$e" in UEFI.Quirks.*|Booter.Quirks.*|Kernel.Block.*) ;; *) KEEP+=("$e");; esac; done
  EDITS=("${KEEP[@]+"${KEEP[@]}"}"); NEEDBLOCK=0
fi
if [ $DRY = 0 ]; then
  mkdir -p "$ST" || stop "could not create the install state directory"
  OLD_STATE="$ST/state.before-install"; HAD_STATE=0
  if [ -f "$STATE" ]; then cp -p "$STATE" "$OLD_STATE" || stop "could not preserve the existing recovery record"; HAD_STATE=1; fi
  UUID=""; [ -z "$CFG" ] && UUID=$(diskutil info "$EFI" | awk -F': *' '/Partition UUID/{print $2}')
  if [ "$HAD_STATE" = 1 ]; then
    inherited=$(owned_amfi_from_prior_record "$OLD_STATE" "$UUID" "$C" "$OCREL" "$(shasum -a 256 "$C" | awk '{print $1}')" "$(get NVRAM.Add.$B.boot-args)")
    for a in $inherited; do case " $ADDED " in *" $a "*) ;; *) ADDED="$ADDED $a";; esac; done
  fi
  BKC="$C.nullmoth-$(date +%Y%m%d-%H%M%S)"
  cp -p "$C" "$BKC" || stop "could not back up $C$(ro_why "$C")"
  ok "backed up the config to $BKC"
  fail=0
  for e in "${EDITS[@]}"; do IFS='|' read -r k t v <<<"$e"
    if has "$k"; then plutil -replace "$k" $t "$v" "$C" || fail=1; else plutil -insert "$k" $t "$v" "$C" || fail=1; fi; done
  if [ $NEEDBLOCK = 1 ]; then
    has Kernel.Block || plutil -insert Kernel.Block -array "$C" || fail=1
    plutil -insert Kernel.Block -json '{"Arch":"Any","Comment":"boot framebuffer IONDRVFramebuffer steals index 0 from NVRMFB","Enabled":true,"Identifier":"com.apple.iokit.IONDRVSupport","MaxKernel":"","MinKernel":"","Strategy":"Exclude"}' -append "$C" || fail=1
  fi
  for k in "${FETCH[@]+"${FETCH[@]}"}"; do ditto "$KT/$k.x/$k" "$KX/$k" || fail=1; done
  for k in "${ADDK[@]+"${ADDK[@]}"}"; do
    n=${k%.kext}
    j="{\"Arch\":\"Any\",\"BundlePath\":\"$k\",\"Comment\":\"$n (NullMoth driver)\",\"Enabled\":true,\"ExecutablePath\":\"Contents/MacOS/$n\",\"MaxKernel\":\"\",\"MinKernel\":\"\",\"PlistPath\":\"Contents/Info.plist\"}"
    # Lilu loads before every plugin, so it goes first; AMFIPass is a Lilu plugin and goes after it (end of the list)
    if [ $k = Lilu.kext ]; then plutil -insert Kernel.Add.0 -json "$j" "$C" || fail=1
    else plutil -insert Kernel.Add -json "$j" -append "$C" || fail=1; fi
  done
  if [ ${#DELADD[@]} -gt 0 ]; then
    has NVRAM.Delete || plutil -insert NVRAM.Delete -dictionary "$C" || fail=1
    plutil -extract NVRAM.Delete.$B xml1 -o - "$C" >/dev/null 2>&1 || plutil -insert NVRAM.Delete.$B -array "$C" || fail=1
    for k in "${DELADD[@]}"; do plutil -insert NVRAM.Delete.$B -string "$k" -append "$C" || fail=1; done
  fi
  if [ $NEEDTOOL = 1 ]; then
    plutil -extract Misc.Tools xml1 -o - "$C" >/dev/null 2>&1 || plutil -insert Misc.Tools -array "$C" || fail=1
    plutil -insert Misc.Tools -json "{\"Arguments\":\"\",\"Auxiliary\":false,\"Comment\":\"NullMoth: remove the NVIDIA driver at the next start\",\"Enabled\":true,\"Flavour\":\"Auto\",\"FullNvramAccess\":true,\"Name\":\"$TOOL_NAME\",\"Path\":\"$TOOL_FILE\",\"RealPath\":false,\"TextMode\":true}" -append "$C" || fail=1
    mkdir -p "$MP/$OCREL/Tools" && cp "$TOOL" "$MP/$OCREL/Tools/$TOOL_FILE" || fail=1
  fi
  [ "${REFRESHTOOL:-0}" = 1 ] && { cp "$TOOL" "$MP/$OCREL/Tools/$TOOL_FILE" || fail=1; }   # the current tool (drains stale keys)
  if [ $NEEDONCE = 1 ]; then
    plutil -extract Misc.Tools xml1 -o - "$C" >/dev/null 2>&1 || plutil -insert Misc.Tools -array "$C" || fail=1
    plutil -insert Misc.Tools -json "{\"Arguments\":\"once\",\"Auxiliary\":false,\"Comment\":\"NullMoth: start macOS once without the NVIDIA driver\",\"Enabled\":true,\"Flavour\":\"Auto\",\"FullNvramAccess\":true,\"Name\":\"$ONCE_NAME\",\"Path\":\"$TOOL_FILE\",\"RealPath\":false,\"TextMode\":true}" -append "$C" || fail=1
    [ -f "$MP/$OCREL/Tools/$TOOL_FILE" ] || { mkdir -p "$MP/$OCREL/Tools" && cp "$TOOL" "$MP/$OCREL/Tools/$TOOL_FILE"; } || fail=1
  fi
  if [ $NEEDLOGO = 1 ]; then
    has UEFI.Drivers || plutil -insert UEFI.Drivers -array "$C" || fail=1
    # the entry carries the keys this config's own driver entries carry, so an older OpenCore never meets a key it lacks
    hv=""; plutil -extract UEFI.Drivers.0.HideVerbose raw -o - "$C" >/dev/null 2>&1 && hv=',"HideVerbose":false'
    plutil -insert UEFI.Drivers -json "{\"Arguments\":\"\",\"Comment\":\"NullMoth boot logo\",\"Enabled\":true,\"LoadEarly\":false,\"Path\":\"$LOGO_FILE\"$hv}" -append "$C" || fail=1
    mkdir -p "$MP/$OCREL/Drivers" && cp "$LOGOSRC" "$MP/$OCREL/Drivers/$LOGO_FILE" || fail=1
  fi
  [ "${REFRESHLOGO:-0}" = 1 ] && { cp "$LOGOSRC" "$MP/$OCREL/Drivers/$LOGO_FILE" || fail=1; }
  if [ $fail = 1 ] || ! plutil -lint "$C" >/dev/null; then cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; fi
  ok "OpenCore config updated"
  mkdir -p "$ST"
  { printf '# NullMoth install record %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s=%q\n' EFI_UUID "$UUID" CONFIG_PATH "$C" OCREL "$OCREL" CONFIG_BACKUP_REL "$OCREL/$(basename "$BKC")" CONFIG_BACKUP "$BKC" \
      CONFIG_SHA_AFTER "$(shasum -a 256 "$C" | awk '{print $1}')" ADDED_ARGS "${ADDED# }" REMOVED_ARGS "${REMOVED# }" \
      OLD_CSR "$cur" NEW_CSR "$want" OLD_SBM "$OLDSBM" \
      SET_SHOWPICKER "${SET_SHOWPICKER:-0}" OLD_SHOWPICKER "${OLD_SHOWPICKER:-}" \
      SET_HIDEAUX "${SET_HIDEAUX:-0}" OLD_HIDEAUX "${OLD_HIDEAUX:-}" \
      SET_PICKERMODE "${SET_PICKERMODE:-0}" OLD_PICKERMODE "${OLD_PICKERMODE:-}"
  } > "$STATE.tmp" && chmod 600 "$STATE.tmp" && mv "$STATE.tmp" "$STATE" || {
    cp -p "$BKC" "$C"
    [ "$HAD_STATE" != 1 ] || cp -p "$OLD_STATE" "$STATE"
    stop "could not save the install record - OpenCore changes restored"
  }
fi
[ "${NULLMOTH_CONFIG_ONLY:-0}" = 1 ] && { cleanup; echo "RESULT ok"; exit 0; }
if [ $SIPON = 1 ] && [ $DRY = 0 ]; then
  if [ "${HAD_STATE:-0}" = 1 ]; then cp -p "$OLD_STATE" "$STATE"; else rm -f "$STATE"; fi
  ok "SIP setting written - restart, then open 1401 again to install the driver"; cleanup; echo "RESULT ok"; exit 0
fi

step "Installing the driver"
T=$(mktemp -d /var/tmp/nullmoth.XXXX)
tar -xzf "$PKG" -C "$T" || stop "could not unpack the package"
[ -x "$T/pkgroot/install.sh" ] || stop "the package has no install.sh"
if [ $DRY = 1 ]; then out=$(CHECK=1 /bin/bash "$INSTALLER" --payload "$T/pkgroot" 2>&1); rc=$?
else out=$(/bin/bash "$INSTALLER" --payload "$T/pkgroot" 2>&1); rc=$?; fi
echo "$out" | sed -E '/^$/d;s/^== /NOTE /;s/^   ok  /NOTE /;s/^   STOP: /STOP /;s/^   NOTE: /NOTE /'
if [ "$rc" != 0 ]; then
  [ $DRY = 0 ] && [ -n "${BKC:-}" ] && [ -f "$BKC" ] && cp -p "$BKC" "$C" && note "OpenCore config put back as it was before"
  if [ $DRY = 0 ]; then
    if [ "${HAD_STATE:-0}" = 1 ]; then cp -p "$OLD_STATE" "$STATE" || stop "could not restore the previous recovery record"
    else rm -f "$STATE"; fi
  fi
  stop "the driver installer stopped (exit $rc)"
fi
if [ $DRY = 1 ]; then ok "dry run: the driver would install cleanly (nothing was changed)"; cleanup; echo "RESULT ok"; exit 0; fi

UNINSTALLER="$(dirname "$INSTALLER")/nullmoth-uninstall.sh"
[ -x "$UNINSTALLER" ] && mkdir -p "$ST" && cp "$UNINSTALLER" "$ST/uninstall.sh" && chmod 755 "$ST/uninstall.sh" || stop "could not save the audited recovery uninstaller"
DBK=$(echo "$out" | sed -n 's/^Undo: sudo .\/uninstall.sh //p' | tail -1)
printf '%s=%q\n' DRIVER_BACKUP "$DBK" >> "$STATE" || stop "could not record the driver backup"
# per-system rules (nullmoth-rules.json, picked by the app from this machine's profile): their knobs go in a marked
# block of the driver's knob file, which install.sh has just written fresh. Only NVMTL_/NVK_/NVRM_ keys with plain values.
CONF=/Library/GPUBundles/nvmtl/nvrm610.conf
if [ -f "$CONF" ]; then
  sed -i '' '/^# --- 1401 per-system rules ---$/,/^# --- end 1401 per-system rules ---$/d' "$CONF"
  if [ ${#KNOBS[@]} -gt 0 ]; then
    { echo "# --- 1401 per-system rules ---"
      for kv in "${KNOBS[@]}"; do
        if [[ "$kv" =~ ^(NVMTL|NVK|NVRM)_[A-Z0-9_]+=[A-Za-z0-9._-]{0,64}$ ]]; then echo "$kv"; note "per-system rule: $kv" >&2
        else echo "NOTE refused per-system knob '$kv' (not a driver knob)" >&2; fi
      done
      echo "# --- end 1401 per-system rules ---"; } >> "$CONF"
  fi
fi
if [ -n "$PROFILE" ] && [ -f "$PROFILE" ]; then
  # read as the console user: root would follow a link in the user's temp folder to a root-only file
  sudo -u "$(stat -f %Su /dev/console)" cat "$PROFILE" > "$ST/system-profile.json.tmp" && chmod 644 "$ST/system-profile.json.tmp" && mv "$ST/system-profile.json.tmp" "$ST/system-profile.json"
  rm -f "$PROFILE"
fi
# the version the app compares with the newest release ("Update driver"); world-readable, the app runs as the user
DV=$(basename "$PKG" | sed -n 's/^nullmoth-nvidia-\([0-9][0-9.]*\)\.tar\.gz$/\1/p')
[ -n "$DV" ] && { echo "$DV" > "$ST/driver-version.tmp" && chmod 644 "$ST/driver-version.tmp" && mv "$ST/driver-version.tmp" "$ST/driver-version"; }
ok "install record written to $STATE"
# A LaunchAgent runs its program at every login. If any directory on the way to
# that program is world writable, any local user can replace what runs. Was: the
# path from --app was written in unchecked, so an app launched from its own
# staging directory under /var/tmp put a world-writable path into a login item,
# and the agent outlived the directory.
# /Applications is group-admin writable by Apple's convention, so the test is
# world-write only, not group-write.
agent_path_is_safe() {
  local p=$1
  case "$p" in /*) ;; *) return 1 ;; esac
  # a regular file, not a directory and not a dangling path: the 1.14.7 agents
  # point at a program that no longer exists, and those must fail here
  [ -f "$p" ] || return 1
  # Resolve to a physical path before walking. stat does not follow symlinks, so
  # a symlinked component pointing into a world-writable directory would
  # otherwise read as the link's own mode and pass.
  local d; d=$(cd "$(/usr/bin/dirname "$p")" 2>/dev/null && pwd -P) || return 1
  while :; do
    local mode; mode=$(/usr/bin/stat -f %Lp "$d" 2>/dev/null) || return 1
    # last octal digit is the "other" field; 2, 3, 6 and 7 all carry write
    case "$mode" in *[2367]) return 1 ;; esac
    [ "$d" = / ] && break
    d=$(/usr/bin/dirname "$d")
  done
  return 0
}

# Repair an agent an earlier version wrote. Upgrading does not rewrite this file,
# so without this every machine that ever installed from a temporary directory
# keeps its bad agent forever.
if [ -f "$AGENT" ]; then
  existing=$(/usr/bin/plutil -extract ProgramArguments.0 raw -o - "$AGENT" 2>/dev/null) || existing=""
  if [ -z "$existing" ] || ! agent_path_is_safe "$existing"; then
    rm -f "$AGENT"
    note "removed a crash-check login item that pointed somewhere unsafe ($existing)"
  fi
fi

if [ -n "$APPBIN" ] && [ -x "$APPBIN" ] && ! agent_path_is_safe "$APPBIN"; then
  note "crash check not added: $APPBIN is not in a protected location"
  APPBIN=""
fi
if [ -n "$APPBIN" ] && [ -x "$APPBIN" ]; then
  cat > "$AGENT" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.crashcheck</string>
<key>ProgramArguments</key><array><string>$APPBIN</string><string>--crash-check</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Background</string>
</dict></plist>
PL
  chmod 644 "$AGENT"; ok "crash check added (asks before sending anything)"
fi
cp "$0" "$ST/nullmoth-setup.sh" && chmod 755 "$ST/nullmoth-setup.sh"
cat > "$ST/nullmoth-recover.sh" <<'RS'
#!/bin/bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin
V=7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove
nvram "$V" >/dev/null 2>&1 || exit 0
{ date; /bin/bash /Library/NullMoth/nullmoth-setup.sh --remove; } >> /Library/NullMoth/recover.log 2>&1
rc=$?; [ "$rc" = 0 ] || exit "$rc"
nvram -d "$V" || exit 1
sleep 2; /sbin/shutdown -r now
RS
chmod 755 "$ST/nullmoth-recover.sh"
cat > "$RECOVER" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.recover</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$ST/nullmoth-recover.sh</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PL
chmod 644 "$RECOVER"; chown root:wheel "$RECOVER"
ok "boot picker way back armed (NullMoth: Remove driver)"
# Flicker stopgap (1.4): the first WindowServer of a boot composes against the driver's boot-console handoff and
# strobes for many users; a WindowServer restart clears it ("sudo killall WindowServer after login" is the workaround
# people share). This restarts it ONCE per boot, early, so the user logs into a clean compositor. The marker lives on a
# tmpfs that is empty every boot, so it fires once and never loops. Opt out by creating /Library/NullMoth/no-wsreset.
# This is a stopgap; the real fix is the boot->WindowServer surface/vblank handoff, chased with a >60 Hz/2nd display.
cat > /Library/NullMoth/nullmoth-wsreset.sh <<'WS'
#!/bin/bash
# Flicker stopgap + evidence: the first WindowServer of a boot composes against the driver's boot handoff and strobes
# for many; a restart clears it. We capture the display/driver state the flickering WindowServer leaves behind, restart
# WindowServer once, then capture the clean state. The before/after lands in wsreset.log, which "Send logs" uploads, so
# the strobe can be root-caused from a real >60 Hz / multi-monitor machine (it does not reproduce on the dev box).
PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ -e /Library/NullMoth/no-wsreset ] && exit 0
MARK=/var/run/nullmoth-wsreset.done
[ -e "$MARK" ] && exit 0
# 1401 issue #108: launchd starts this before the kexts are listed, and a single check ended the run for the whole
# boot (no display-trace directory, no capture). Wait up to 60 s for the driver, then decide.
for i in $(seq 1 60); do kextstat 2>/dev/null | grep -q com.nullmoth.NVAccel && break; sleep 1; done
kextstat 2>/dev/null | grep -q com.nullmoth.NVAccel || exit 0
nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove >/dev/null 2>&1 && exit 0
LOG=/Library/NullMoth/wsreset.log
capture() {
  { echo "===== $1 $(date -u +%Y-%m-%dT%H:%M:%SZ) ====="
    echo "WindowServer: pid $(pgrep -x WindowServer | tr '\n' ' ')  uptime $(ps -o etime= -p "$(pgrep -x WindowServer | head -1)" 2>/dev/null | tr -d ' ')"
    # current refresh per framebuffer, straight from the registry (pixelClock / pixelCount = Hz) - never ages out, so it
    # always shows the >60 Hz / multi-monitor condition even if the boot log lines have scrolled off
    echo "-- current mode per framebuffer (fb: WxH? pclk/total = Hz) --"
    ioreg -r -c NVRMNVDAFramebuffer -w0 2>/dev/null | /usr/bin/awk '
      /"IOFBDependentIndex" =/    { if (match($0, /= [0-9]+/)) idx = substr($0, RSTART+2, RLENGTH-2) }
      /"IOFBCurrentPixelCount" =/ { if (match($0, /= [0-9]+/)) cnt = substr($0, RSTART+2, RLENGTH-2) }
      /"IOFBCurrentPixelClock" =/ { if (match($0, /= [0-9]+/)) { clk = substr($0, RSTART+2, RLENGTH-2);
        if (cnt+0 > 0) printf "   fb%s: %.2f Hz (pclk %s / total %s)\n", idx, clk/cnt, clk, cnt; cnt="" } }'
    # displays, their mode, heads and connectors, from the fb boot log (wide window: the daemon runs seconds after boot)
    echo "-- displays / heads (newest first) --"
    log show --start "$SINCE" --style compact --predicate 'process == "kernel" AND eventMessage CONTAINS "NVRM-fb"' 2>/dev/null \
      | grep -aE 'ENUMERATED|dpy [0-9]|head [0-9].*mode|connected [0-9].*edid|boot pair|hw vblank|TAKEOVER|SHUTDOWN|refresh rate from NVKMS' \
      | uniq | tail -24 | sed 's/^[0-9-]* //'
    # flip / vblank / modeset activity = what strobes; drop the once-a-minute keepalive noise
    echo "-- flip / vblank / modeset (last 90s) --"
    log show --start "$SINCE" --style compact --predicate 'process == "kernel" AND (eventMessage CONTAINS "NVRM-fb" OR eventMessage CONTAINS "nvkms")' 2>/dev/null \
      | grep -aviE 'kapi event type 5|sample [0-9]' | grep -aiE 'flip|vbl|modeset|present|home|reflip|commit|latch|head' | uniq -c | tail -30 | sed 's/^[0-9-]* //'
    # the driver's own in-registry trace + any counters it publishes
    echo "-- NVRMFB registry --"
    ioreg -r -c NVRMNVDAFramebuffer -w0 2>/dev/null | grep -oaE '"(NVRMTrace|NVRMTraceCount|reflips|fVblCalls|fBarPaints)" = [^,}]{0,200}' | tail -8
    echo
  } >> "$LOG" 2>&1
}
# keep the log to the last few boots
[ -f "$LOG" ] && tail -c 262144 "$LOG" > "$LOG.keep" 2>/dev/null && mv "$LOG.keep" "$LOG"
for i in $(seq 1 40); do pgrep -x WindowServer >/dev/null 2>&1 && break; sleep 1; done
pgrep -x WindowServer >/dev/null 2>&1 || exit 0
sleep 8   # let the first WindowServer settle into the (possibly flickering) steady state before we photograph it
SINCE=$(date -r "$(sysctl -n kern.boottime | sed -E 's/.*\{ sec = ([0-9]+),.*/\1/')" '+%Y-%m-%d %H:%M:%S')
capture "BEFORE restart"
SINCE=$(date '+%Y-%m-%d %H:%M:%S')
: > "$MARK"   # claim the single shot BEFORE touching WindowServer, so a respawn can never loop
# Only ever at the login window. In 1.9.0, with auto-login the session already existed when this fired, so the
# restart logged the user out to the password screen. A restart under a logged-in user ends their session; never do it.
CONSOLE=$(stat -f%Su /dev/console 2>/dev/null)
if [ -n "$(defaults read /Library/Preferences/com.apple.loginwindow autoLoginUser 2>/dev/null)" ]; then
  echo "$(date) skipped the restart: auto-login is on (a restart would end the automatic session)" >> "$LOG" 2>&1; exit 0
fi
if [ -n "$CONSOLE" ] && [ "$CONSOLE" != root ]; then
  echo "$(date) skipped the restart: $CONSOLE is already logged in" >> "$LOG" 2>&1; exit 0
fi
P=$(pgrep -x WindowServer); [ -n "$P" ] && { kill -TERM $P 2>/dev/null; sleep 2; pgrep -x WindowServer >/dev/null 2>&1 && kill -9 $P 2>/dev/null; }
for i in $(seq 1 30); do pgrep -x WindowServer >/dev/null 2>&1 && break; sleep 1; done
sleep 8
capture "AFTER restart (should be clean)"
echo "$(date) restarted WindowServer once; before/after captured above" >> "$LOG" 2>&1
WS
chmod 755 /Library/NullMoth/nullmoth-wsreset.sh; chown root:wheel /Library/NullMoth/nullmoth-wsreset.sh
cat > /Library/LaunchDaemons/com.nullmoth.wsreset.plist <<'PL'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.wsreset</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>/Library/NullMoth/nullmoth-wsreset.sh</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Background</string>
</dict></plist>
PL
chmod 644 /Library/LaunchDaemons/com.nullmoth.wsreset.plist; chown root:wheel /Library/LaunchDaemons/com.nullmoth.wsreset.plist
ok "flicker stopgap armed (restarts WindowServer once per boot; opt out with /Library/NullMoth/no-wsreset)"
# Display tracker: flicker and black screens only happen on monitors we do not have (>60 Hz DisplayPort, two displays),
# and a "Send logs" afterwards only photographs the end state. This records from boot: the driver's flip/scan-out/AGDC
# counters every second (only lines that changed, plus a heartbeat), the refresh rate of each framebuffer, every
# WindowServer start or restart with a full snapshot, and the driver's kernel messages (repeated flip lines counted per
# second instead of dropped). Bounded: each file rotates at 8 MB, one old copy kept. "Send logs" uploads both.
cat > /Library/NullMoth/nullmoth-displaytrace.sh <<'DT'
#!/bin/bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin
# 1401 issue #108: launchd starts this before the kexts are listed, and a single check ended the run for the whole
# boot (no display-trace directory, no capture). Wait up to 60 s for the driver, then decide.
for i in $(seq 1 60); do kextstat 2>/dev/null | grep -q com.nullmoth.NVAccel && break; sleep 1; done
kextstat 2>/dev/null | grep -q com.nullmoth.NVAccel || exit 0
D=/Library/NullMoth/display-trace; mkdir -p "$D"; chmod 755 "$D"
T=$D/trace.txt; K=$D/kernel.txt; W=$D/ws.txt; CAP=8388608
rot() { local s; s=$(stat -f%z "$1" 2>/dev/null || echo 0); [ "$s" -gt $CAP ] && mv -f "$1" "$1.1"; }
rot "$T"; rot "$K"; rot "$W"
CTRS="debug.nvaccel_iop_ok debug.nvaccel_iop_fail debug.nvaccel_iop_empty debug.nvaccel_iop_blank debug.nvaccel_iop_flips debug.nvaccel_iop_flip_hit debug.nvaccel_iop_flip_miss debug.nvaccel_iop_flip_stale debug.nvaccel_iop_flip_refused debug.nvaccel_iop_flip_novram debug.nvaccel_iop_flip_lock debug.nvaccel_iop_flip_home debug.nvaccel_iop_async_flips debug.nvaccel_iop_async_refused debug.nvaccel_iop_async_drop debug.nvaccel_iop_ok_h0 debug.nvaccel_iop_ok_h1 debug.nvaccel_iop_ok_h2 debug.nvaccel_iop_ok_h3 debug.nvaccel_heads_published debug.nvaccel_heads_piped debug.nvrmfb_agdc_cmds debug.nvrmfb_agdc_refused debug.nvrmfb_agdc_last_refused debug.nvrmfb_agdc_k5_link debug.nvrmfb_agdc_k5_caps debug.nvrmfb_agdc_fbmap"
hz() { ioreg -r -c NVRMNVDAFramebuffer -w0 2>/dev/null | LC_ALL=C awk '
  /"IOFBDependentIndex" =/    { if (match($0, /= [0-9]+/)) idx = substr($0, RSTART+2, RLENGTH-2) }
  /"IOFBCurrentPixelCount" =/ { if (match($0, /= [0-9]+/)) cnt = substr($0, RSTART+2, RLENGTH-2) }
  /"IOFBCurrentPixelClock" =/ { if (match($0, /= [0-9]+/)) { clk = substr($0, RSTART+2, RLENGTH-2);
    if (cnt+0 > 0) printf "fb%s=%.2fHz ", idx, clk/cnt; cnt="" } }'; }
snapshot() {
  { echo "== $1 $(date -u +%Y-%m-%dT%H:%M:%SZ) up=$(($(date +%s) - BOOT))s"
    echo "boot-args: $(nvram boot-args 2>/dev/null | cut -f2-)"
    sysctl debug 2>/dev/null | grep -E '^debug\.(nvaccel|nvrmfb)' | grep -v -E 'nvaccel_vm_|nvaccel_res_|nvaccel_map_'
    ioreg -r -c NVRMNVDAFramebuffer -w0 2>/dev/null | grep -oaE '"(IOFB[A-Za-z]*(Mode|Timing|Pixel)[A-Za-z]*|IOFBDependentIndex|NVRMTrace|NVRMTraceCount|reflips|fVblCalls|fBarPaints)" = [^,}]{0,240}'
    echo "displays: $(hz)"
    # The monitor's own list of timings: a rate that goes black is checked against what the panel says it accepts
    ioreg -l -r -c AppleDisplay -w0 2>/dev/null | grep -oaE '"IODisplayEDID" = <[0-9a-f]+>' | sort -u
    echo
  } >> "$T" 2>&1; }
BOOT=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
echo "#### boot $(date -r "$BOOT" -u +%Y-%m-%dT%H:%M:%SZ), tracker start $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$T"
# kernel messages: the flip/vblank lines repeat many times a second, so identical lines become "Nx <line>" per second
/usr/bin/log stream --style compact --predicate 'sender == "NVRMFB" OR sender == "NVRM" OR sender == "NVAccel"' 2>/dev/null \
  | awk '{ s = substr($0, 1, 19); m = $0; sub(/^[^ ]+ [^ ]+ +[^ ]+ +[^ ]+ +/, "", m); gsub(/[0-9]+/, "#", m)
           if (s != ls || m != lm) { if (n) print (n > 1 ? n "x " : "") last; n = 0 }
           ls = s; lm = m; last = $0; n++; fflush() }' >> "$K" &
LS=$!
# WindowServer's side of a mode change (the ">60 Hz goes black" reports): our kexts log a successful commit at every
# rate, so the black is after the modeset and only WindowServer/CoreDisplay say what happened next. Same per-second
# folding, own file so it cannot crowd out the kernel lines.
/usr/bin/log stream --style compact --predicate 'process == "WindowServer" AND (eventMessage CONTAINS[c] "mode" OR eventMessage CONTAINS[c] "refresh" OR eventMessage CONTAINS[c] "reconfig" OR eventMessage CONTAINS[c] "blank" OR eventMessage CONTAINS[c] "online" OR eventMessage CONTAINS[c] "timing" OR eventMessage CONTAINS[c] "revert" OR eventMessage CONTAINS[c] "edid") AND NOT eventMessage CONTAINS "RunLoopMode"' 2>/dev/null \
  | awk '{ s = substr($0, 1, 19); m = $0; sub(/^[^ ]+ [^ ]+ +[^ ]+ +[^ ]+ +/, "", m); gsub(/[0-9]+/, "#", m)
           if (s != ls || m != lm) { if (n) print (n > 1 ? n "x " : "") last; n = 0 }
           ls = s; lm = m; last = $0; n++; fflush() }' >> "$W" &
LW=$!
trap 'kill $LS $LW 2>/dev/null' EXIT
WS=""; PREV=""; i=0
while :; do
  w=$(pgrep -x WindowServer | head -1)
  if [ "$w" != "$WS" ]; then snapshot "WindowServer ${WS:+restarted ($WS -> )}${w:-gone}"; WS=$w; fi
  cur="$(sysctl $CTRS 2>/dev/null | sed -E 's/^debug\.(nvaccel_|nvrmfb_)?//; s/: /=/' | tr '\n' ' ')$(hz)"
  if [ "$cur" != "$PREV" ] || [ $((i % 30)) = 0 ]; then echo "$(date -u +%H:%M:%S) ws=$w $cur" >> "$T"; PREV=$cur; fi
  i=$((i + 1))
  # every second for the first 15 minutes after boot (display hand-over and login), then every 5 s
  if [ $(( $(date +%s) - BOOT )) -lt 900 ]; then sleep 1; else sleep 5; fi
  [ $((i % 300)) = 0 ] && { rot "$T"; for f in "$K" "$W"; do s=$(stat -f%z "$f" 2>/dev/null || echo 0); [ "$s" -gt $CAP ] && { cp "$f" "$f.1"; : > "$f"; }; done; }
done
DT
chmod 755 /Library/NullMoth/nullmoth-displaytrace.sh; chown root:wheel /Library/NullMoth/nullmoth-displaytrace.sh
cat > /Library/LaunchDaemons/com.nullmoth.displaytrace.plist <<'PL'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.displaytrace</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>/Library/NullMoth/nullmoth-displaytrace.sh</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Background</string>
<key>LowPriorityIO</key><true/>
<key>Nice</key><integer>10</integer>
</dict></plist>
PL
chmod 644 /Library/LaunchDaemons/com.nullmoth.displaytrace.plist; chown root:wheel /Library/LaunchDaemons/com.nullmoth.displaytrace.plist
ok "display tracker armed (records the driver's display state from every boot; Send logs uploads it)"
if [ $INSTALL_THEN_PREPARE = 1 ]; then do_prepare; else ok "driver installed - restart to load it"; fi
cleanup; echo "RESULT ok"
