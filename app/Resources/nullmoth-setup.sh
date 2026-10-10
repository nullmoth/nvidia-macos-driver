#!/bin/bash
PATH=/usr/bin:/bin:/usr/sbin:/sbin
B=7C436110-AB2A-4BBB-A880-FE41995C9F82
# ipc_control_port_options=0: with AMFI relaxed, Firefox and other hardened apps crash at launch on their task control
# port (10-10 chat: Firefox "instant crash", fixed by this argument); OpenCore Legacy Patcher sets it for the same reason.
WANT_ARGS="nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80 ipc_control_port_options=0"
DROP_ARGS="nv_disable=1 -wegnoegpu"          # both hide the NVIDIA card from macOS
SIP_BITS=$((0x0A43))
TOOL_NAME="1401: Remove NVIDIA driver"; TOOL_FILE=NullMothSafe.efi
ST=${NULLMOTH_STATE_DIR:-/Library/NullMoth}; STATE=$ST/state
AGENT=/Library/LaunchAgents/com.nullmoth.crashcheck.plist
RECOVER=/Library/LaunchDaemons/com.nullmoth.recover.plist
KNOBS=(); PROFILE=""; COLLECT=""; UPD=""; VERB=""; PKG=""; SHA=""; CFG=""; EFI=auto; DRY=0; REMOVE=0; USBMAP=""; TOOL=""; APPBIN=""; MOUNTED=""; T=""
while [ $# -gt 0 ]; do case "$1" in
  --pkg) PKG=$2; shift;; --sha) SHA=$2; shift;; --config) CFG=$2; shift;; --efi) EFI=$2; shift;;
  --tool) TOOL=$2; shift;; --usbmap) USBMAP=$2; shift;; --app) APPBIN=$2; shift;; --dry) DRY=1;; --remove) REMOVE=1;; --verbose) VERB=$2; shift;; --update) UPD=$2; shift;; --collect-logs) COLLECT=$2; shift;;
  --knob) KNOBS+=("$2"); shift;; --profile) PROFILE=$2; shift;;
  *) echo "STOP unknown option $1"; echo "RESULT stop"; exit 2;; esac; shift; done
step() { echo "STEP $*"; }; ok() { echo "OK $*"; }; note() { echo "NOTE $*"; }
KT=""
cleanup() { for d in $MOUNTED; do diskutil unmount "$d" >/dev/null 2>&1; done; [ -n "$T" ] && rm -rf "$T"; [ -n "${KT:-}" ] && rm -rf "$KT"; }
stop() { echo "STOP $*"; cleanup; echo "RESULT stop"; exit 1; }
[ "$(id -u)" = 0 ] || { echo "STOP needs administrator rights"; echo "RESULT stop"; exit 1; }
[ "$UPD" = finish ] && [ ! -f "$ST/update-pending" ] && { echo "RESULT ok"; exit 0; }
has() { plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }
get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }
mnt() { diskutil info "$1" 2>/dev/null | awk -F': *' '/Mount Point/{print $2}'; }
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
esac; }
# Downloads $1 into $KT/$1.x (checked against its SHA-256) before anything is changed; stops on any failure. $2 = where it goes.
fetch_kext() {
  local k=$1 u s
  [ -n "$KT" ] || KT=$(mktemp -d) || stop "could not create a download folder"
  read -r u s <<<"$(kurl $k)"
  curl -fsSL --max-time 180 -o "$KT/$k.zip" "$u" \
    || stop "$k could not be downloaded ($u). Check the internet connection and try again, or put $k in $2"
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
tool_index() {  # index of our entry in Misc.Tools, or nothing
  local i=0 p
  while p=$(plutil -extract Misc.Tools.$i.Path raw -o - "$C" 2>/dev/null); do
    [ "$p" = "$TOOL_FILE" ] && { echo $i; return; }; i=$((i+1)); done; }
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
  for f in "$ST"/*.log "$ST/state"; do [ -f "$f" ] && cp "$f" "$COLLECT/driver-$(basename "$f").txt"; done
  { echo "macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion))   model $(sysctl -n hw.model)"
    echo "boot-args: $(nvram boot-args 2>/dev/null | cut -f2-)"; echo "SIP: $(csrutil status 2>/dev/null)"
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
  # A boot that hung early may not have reached the log store; a later boot's panic report then carries it.
  log show --last 3d --style compact --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "nvrm" OR eventMessage CONTAINS[c] "nvaccel" OR eventMessage CONTAINS[c] "nvidia" OR eventMessage CONTAINS[c] "nullmoth" OR eventMessage CONTAINS[c] "gsp")' 2>&1 \
    | grep -v -e 'NVRM-fb: kapi event type 5$' -e 'NVRM-xnu: >os_map_kernel_space' -e 'NVRM-fb: flipToMemory #' \
    | tail -n 6000 > "$COLLECT/driver-kernel-log.txt"
  # 1.9.0 report (10-10, 2K 165 Hz monitor stuck at 1080p 60 Hz): the 6000-line tail above was all os_map_kernel_space
  # and flipToMemory lines, so the boot-time mode list, the EDID and why each mode was or was not offered were gone.
  # Everything the display decision used, from THIS boot only, never tailed away.
  { echo "== monitors (EDID as macOS sees it)"
    ioreg -l -w0 -r -c IODisplayConnect 2>/dev/null | grep -E '"(IODisplayEDID|DisplayProductName|DisplayVendorID|DisplayProductID)"'
    echo; echo "== NVIDIA framebuffers"
    ioreg -l -w0 -r -c NVRMNVDAFramebuffer 2>/dev/null | grep -E '"(IOFB[A-Za-z]*(Mode|Timing|Pixel|Display)[A-Za-z]*|nvrm[A-Za-z-]*|NVRM[A-Za-z]*|IOFBDependentIndex)"'
    echo; echo "== framebuffer messages since this boot"
    boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    log show --start "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" --style compact \
      --predicate 'process == "kernel" AND (eventMessage CONTAINS "NVRM-fb" OR eventMessage CONTAINS "nvkms")' 2>&1 \
      | grep -v -e 'kapi event type 5$' -e 'flipToMemory #' | head -n 4000
  } > "$COLLECT/driver-display.txt" 2>&1
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-kernel-log.txt"
  # The current kernel message ring can retain early GSP/BAR failures absent from the log store.
  { echo; echo "== current kernel message ring"; dmesg 2>&1 | grep -iE 'nvrm|nvaccel|nvidia|nullmoth|gsp' | tail -n 2000; } >> "$COLLECT/driver-kernel-log.txt"
  # Keep update/snapshot context separate so APFS traffic cannot crowd out GPU errors.
  log show --last 3d --style compact --predicate '(process == "kernel" AND eventMessage CONTAINS[c] "apfs") OR process == "MobileSoftwareUpdate" OR process == "softwareupdated"' 2>&1 \
    | tail -n 2000 > "$COLLECT/driver-update-log.txt"
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-update-log.txt"
  { echo; echo "== current update message ring"; dmesg 2>&1 | grep -iE 'apfs|MobileSoftwareUpdate|softwareupdated' | tail -n 1000; } >> "$COLLECT/driver-update-log.txt"
  # Release builds report failures to syslog rather than nvmtl.log. Debug builds may use either
  # the system temp directory or the console user's temp directory.
  log show --last 3d --style compact --predicate 'process != "kernel" AND (eventMessage CONTAINS[c] "NVMTL" OR eventMessage CONTAINS[c] "NullMoth" OR eventMessage CONTAINS[c] "nvk-reason")' 2>&1 \
    | tail -n 3000 > "$COLLECT/driver-plugin-log.txt"
  log_status=${PIPESTATUS[0]}; echo "log show exit: $log_status" >> "$COLLECT/driver-plugin-log.txt"
  console_user=$(stat -f %Su /dev/console 2>/dev/null)
  console_tmp=""
  if [ -n "$console_user" ] && [ "$console_user" != root ] && [ "$console_user" != loginwindow ]; then
    console_tmp=$(sudo -u "$console_user" getconf DARWIN_USER_TEMP_DIR 2>/dev/null)
  fi
  for f in /private/tmp/nvmtl.log "${console_tmp:+${console_tmp%/}/nvmtl.log}"; do
    [ -f "$f" ] || continue
    { echo; echo "== plugin file log"; tail -c 262144 "$f"; } >> "$COLLECT/driver-plugin-log.txt"
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
      log show --start "$from" --end "$to" --style compact --predicate '(process != "kernel" AND (eventMessage CONTAINS[c] "NVMTL" OR eventMessage CONTAINS[c] "NullMoth" OR eventMessage CONTAINS[c] "nvk-reason" OR eventMessage CONTAINS[c] "MetalDevice")) OR (process == "WindowServer" AND (messageType == error OR messageType == fault))' 2>&1 \
        | awk -v to="$to" '!/^20[0-9][0-9]-/ || substr($0, 1, 19) <= to' \
        | tail -n 700
      echo "log show exit: ${PIPESTATUS[0]}"
      echo "-- kernel driver lines"
      log show --start "$from" --end "$to" --style compact --predicate 'process == "kernel" AND (eventMessage CONTAINS[c] "nvrm" OR eventMessage CONTAINS[c] "nvaccel" OR eventMessage CONTAINS[c] "gsp")' 2>&1 \
        | awk -v to="$to" '!/^20[0-9][0-9]-/ || substr($0, 1, 19) <= to' \
        | tail -n 800
      echo "log show exit: ${PIPESTATUS[0]}"
    done
  } > "$COLLECT/driver-crash-window.txt" 2>&1
  n=0; collection_errors=0
  collect_recent_logs macos 3 /Library/Logs/DiagnosticReports/*.panic /Library/Logs/DiagnosticReports/Retired/*.panic
  # A boot that hung or went black never writes a panic: its kernel messages are only in the log store. Users could not
  # send them (10-10 chat: "the logs from the kernel don't get saved to a file"), so keep the whole kernel log of the boots
  # before this one, up to this boot's start: the tail is how the last failed boot ended.
  { boot=$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+),.*/\1/')
    echo "== kernel messages before this boot (started $(date -r "$boot" '+%Y-%m-%d %H:%M:%S'))"
    log show --start "$(date -r $((boot - 259200)) '+%Y-%m-%d %H:%M:%S')" --end "$(date -r "$boot" '+%Y-%m-%d %H:%M:%S')" \
      --style compact --predicate 'process == "kernel"' 2>&1 \
      | grep -v -e 'NVRM-fb: kapi event type 5$' -e 'NVRM-xnu: >os_map_kernel_space' -e 'NVRM-fb: flipToMemory #' | tail -n 8000
  } > "$COLLECT/previous-boot-kernel-log.txt" 2>&1
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
    # The EFI itself (10-09): most machines that fail run their own OpenCore config, and without it every fix was a
    # guess from symptoms. The config goes up with the machine's identity removed (serial, board serial, UUID, ROM),
    # plus the list of kexts, drivers and ACPI files beside it, so the working and the broken EFIs can be compared.
    for cf in "$mp"/EFI/OC/config.plist "$mp"/EFI/OC/config.plist.nullmoth-*; do
      [ -f "$cf" ] || continue
      out="$COLLECT/$d-efi-$(basename "$cf").txt"
      cp "$cf" "$out.plist" || { collection_errors=$((collection_errors+1)); continue; }
      for k in SystemSerialNumber MLB SystemUUID ROM; do plutil -remove "PlatformInfo.Generic.$k" "$out.plist" >/dev/null 2>&1; done
      plutil -convert xml1 -o "$out" "$out.plist" 2>/dev/null || mv "$out.plist" "$out"; rm -f "$out.plist"
      n=$((n+1))
    done
    [ -d "$mp/EFI/OC" ] && { echo "== $d EFI/OC"; (cd "$mp/EFI/OC" && ls -1 Kexts Drivers ACPI Tools 2>/dev/null; ls -la OpenCore.efi 2>/dev/null); } >> "$COLLECT/$d-efi-files.txt"
  done
  { echo; echo "== NullMoth NVRAM flags (a remove flag that survives every boot removes the driver at login)"
    nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove 2>&1; } >> "$COLLECT/driver-state.txt"
  chmod -R a+rX "$COLLECT"; ok "collected driver state and $n diagnostic log(s)"; cleanup
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
  elif [ -n "${EFI_UUID:-}" ]; then
    mount_efi "$EFI_UUID" || stop "could not mount the recorded OpenCore partition; attach the original boot disk or select its replacement - nothing changed"; MP=$MOUNT_POINT
    C="$MP/${OCREL:-EFI/OC}/config.plist"
  elif [ -n "${CONFIG_PATH:-}" ]; then
    C=$CONFIG_PATH; [ -f "$C" ] || stop "the recorded config is missing - nothing changed"
    MP=$(cd "$(dirname "$C")/../.." && pwd) || stop "cannot resolve the recorded OpenCore partition"
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
if [ -z "$USBMAP" ] && [ -z "$VERB" ] && { [ -z "$UPD" ] || [ $INSTALL_THEN_PREPARE = 1 ]; }; then
step "Checking the driver package"
[ -f "$PKG" ] || stop "package not found: $PKG"
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
        esp=$(boot_esp)
        for d in $match; do [ "$d" = "$esp" ] && { found=$d; BOOT_BOUND=1; ok "OpenCore started this Mac from $d (on the macOS disk; the copy on$(echo " $match" | sed "s/ $d//") sets the same serial)"; }; done
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
  fi

fi
plutil -lint "$C" >/dev/null || stop "$C is not a valid plist - fix it before installing"

if [ -n "$VERB" ]; then
  # Verbose startup = boot argument -v. It goes in the config (OpenCore rewrites boot-args every boot when NVRAM Delete
  # lists it) AND in NVRAM now (a config without that Delete entry keeps whatever NVRAM already holds).
  case "$VERB" in on|off) ;; *) stop "--verbose takes on or off";; esac
  step "Turning verbose startup $VERB"
  BKC="$C.nullmoth-verbose-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C"
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
  backup() { BKC="$C.nullmoth-update-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C"; }
  do_prepare() {
    step "Preparing this Mac for the macOS 26 Tahoe update"
    ls /Library/NullMoth/kexts/*/NVAccel.kext >/dev/null 2>&1 || stop "this Mac still has driver 1.0, which has no macOS 26 kexts"
    backup
    setq UEFI.Quirks.ResizeGpuBars -1; setq Booter.Quirks.ResizeAppleGpuBars 0
    bi=$(bidx); [ -n "$bi" ] && plutil -replace Kernel.Block.$bi.Enabled -bool false "$C"
    args_set add -nvoff
    plutil -lint "$C" >/dev/null || { cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; }
    echo "FROM=$MAJ" > "$PEND"
    cat > /Library/LaunchDaemons/com.nullmoth.osupdate.plist <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.nullmoth.osupdate</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>-c</string><string>/bin/bash $ST/nullmoth-setup.sh --update finish >> $ST/osupdate.log 2>&1</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PL
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
    setq UEFI.Quirks.ResizeGpuBars 13; setq Booter.Quirks.ResizeAppleGpuBars -1
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
  BKC="$C.nullmoth-usb-$(date +%Y%m%d-%H%M%S)"; cp -p "$C" "$BKC" || stop "could not back up $C"; ok "backed up the config to $BKC"
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
for a in $WANT_ARGS; do case " $new " in *" $a "*) ;; *) new="$new $a"; ADDED="$ADDED $a"; echo "CHANGE boot-args: add $a";; esac; done
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
# The macOS installer boots with a small GPU BAR (ResizeAppleGpuBars 0, so its fallback screen survives PCI setup); the
# driver was tested with the card's full 8 GB BAR, so the installed system gets that back.
bar=$(get UEFI.Quirks.ResizeGpuBars); abar=$(get Booter.Quirks.ResizeAppleGpuBars)
# 13 = 8 GB: the full BAR of an 8 GB card, measured 10-07 on the RTX 5060 (NVRM moves BAR1 out of the console, display armed).
[ "$bar" != 13 ] && { echo "CHANGE ResizeGpuBars: ${bar:-unset} -> 13 (8 GB BAR, full memory bandwidth)"; EDITS+=("UEFI.Quirks.ResizeGpuBars|-integer|13"); }
[ "$abar" != -1 ] && { echo "CHANGE ResizeAppleGpuBars: ${abar:-unset} -> -1 (macOS sees the full BAR)"; EDITS+=("Booter.Quirks.ResizeAppleGpuBars|-integer|-1"); }
# boot.efi stops with STOP 0x16 (no room in low memory for the kernel) on some boards even with a valid custom slide, and
# the full BAR set above moves memory around. AllowRelocationBlock loads the kernel through a scratch block in the lower
# 4 GB and is used only when no slide fits (OpenCore Configuration, Booter > Quirks); it needs ProvideCustomSlide and
# AvoidRuntimeDefrag, so it is only turned on where both are on.
if [ "$(get Booter.Quirks.ProvideCustomSlide)" = true ] && [ "$(get Booter.Quirks.AvoidRuntimeDefrag)" = true ] && [ "$(get Booter.Quirks.AllowRelocationBlock)" != true ]; then
  echo "CHANGE AllowRelocationBlock: on (boot.efi can still place the kernel when low memory is full)"; EDITS+=("Booter.Quirks.AllowRelocationBlock|-bool|true")
fi
# The installer runs on the firmware framebuffer (IONDRVSupport); once the driver is in, that framebuffer would take
# display index 0 from NVRMFB, so the installed system excludes it (as on the tested RTX 5060 setup).
NEEDBLOCK=0; bi=$(bidx)
if [ -z "$bi" ]; then NEEDBLOCK=1; echo "CHANGE Kernel -> Block: exclude IONDRVSupport (the firmware framebuffer would take NVRMFB's display)"
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
if nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove >/dev/null 2>&1 && ! echo "$del" | grep -q "<string>nullmoth-remove</string>"; then
  DELADD+=("nullmoth-remove"); echo "CHANGE NVRAM Delete: add nullmoth-remove (a leftover remove flag this Mac cannot clear from macOS)"
fi
NEEDTOOL=0; [ -z "$(tool_index)" ] && { NEEDTOOL=1; echo "CHANGE boot picker: add \"$TOOL_NAME\" (the way back if the driver ever stops macOS starting)"; }
[ ${#EDITS[@]} = 0 ] && [ ${#DELADD[@]} = 0 ] && [ $NEEDTOOL = 0 ] && [ $NEEDBLOCK = 0 ] && [ $NEEDAMFIPASS = 0 ] && ok "OpenCore already has every setting the driver needs"

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
  cp -p "$C" "$BKC" || stop "could not back up $C"
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
    plutil -insert Misc.Tools -json "{\"Arguments\":\"\",\"Auxiliary\":false,\"Comment\":\"NullMoth: remove the NVIDIA driver at the next start\",\"Enabled\":true,\"Flavour\":\"Auto\",\"FullNvramAccess\":true,\"Name\":\"$TOOL_NAME\",\"Path\":\"$TOOL_FILE\",\"RealPath\":false,\"TextMode\":false}" -append "$C" || fail=1
    mkdir -p "$MP/$OCREL/Tools" && cp "$TOOL" "$MP/$OCREL/Tools/$TOOL_FILE" || fail=1
  fi
  if [ $fail = 1 ] || ! plutil -lint "$C" >/dev/null; then cp -p "$BKC" "$C"; stop "editing the config failed - the original is restored"; fi
  ok "OpenCore config updated"
  mkdir -p "$ST"
  { printf '# NullMoth install record %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf '%s=%q\n' EFI_UUID "$UUID" CONFIG_PATH "$C" OCREL "$OCREL" CONFIG_BACKUP_REL "$OCREL/$(basename "$BKC")" CONFIG_BACKUP "$BKC" \
      CONFIG_SHA_AFTER "$(shasum -a 256 "$C" | awk '{print $1}')" ADDED_ARGS "${ADDED# }" REMOVED_ARGS "${REMOVED# }" \
      OLD_CSR "$cur" NEW_CSR "$want" OLD_SBM "$OLDSBM"
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
  cp "$PROFILE" "$ST/system-profile.json.tmp" && chmod 644 "$ST/system-profile.json.tmp" && mv "$ST/system-profile.json.tmp" "$ST/system-profile.json"
  rm -f "$PROFILE"
fi
# the version the app compares with the newest release ("Update driver"); world-readable, the app runs as the user
DV=$(basename "$PKG" | sed -n 's/^nullmoth-nvidia-\([0-9][0-9.]*\)\.tar\.gz$/\1/p')
[ -n "$DV" ] && { echo "$DV" > "$ST/driver-version.tmp" && chmod 644 "$ST/driver-version.tmp" && mv "$ST/driver-version.tmp" "$ST/driver-version"; }
ok "install record written to $STATE"
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
# Only ever at the login window. 1.9.0 (10-10): with auto-login the session already existed when this fired, so the
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
if [ $INSTALL_THEN_PREPARE = 1 ]; then do_prepare; else ok "driver installed - restart to load it"; fi
cleanup; echo "RESULT ok"
