#!/bin/bash
# Prints what macOS itself reports about the Mac features that depend on hardware and drivers, one line per feature:
#   feature <TAB> state <TAB> evidence          state is yes, no or unknown
# It only reads (system_profiler, fdesetup, pmset, csrutil, ifconfig); run it on a real Mac and on a NullMoth Mac and
# compare the two outputs. A row that is yes on the real Mac and no here is a parity gap with its evidence beside it.
# usage: tools/mac_parity_probe.sh [--only <prefix>]
set -u
PATH=$PATH:/usr/bin:/usr/sbin:/bin:/sbin
only=${2:-}
[ "${1:-}" = "--only" ] || only=""

row() { case "$1" in "$only"*) printf '%s\t%s\t%s\n' "$1" "$2" "$3" ;; esac; }
# yesno <feature> <text> <regex-for-yes> <label>: yes when the regex matches, unknown when the section is empty.
yesno() {
  if [ -z "$2" ]; then row "$1" unknown "no report from macOS"
  elif printf '%s\n' "$2" | grep -Eq "$3"; then row "$1" yes "$4"
  else row "$1" no "$4 not reported"; fi
}

wifi=$(system_profiler SPAirPortDataType 2>/dev/null)
bt=$(system_profiler SPBluetoothDataType 2>/dev/null)
gpu=$(system_profiler SPDisplaysDataType 2>/dev/null)
nvme=$(system_profiler SPNVMeDataType SPSerialATADataType 2>/dev/null)

# Wi-Fi driver features: each line is macOS's own statement about the installed Wi-Fi driver and card. macOS 26 prints
# AirDrop and Auto Unlock there but not Handoff or Instant Hot Spot, so those two have no row; awdl0 is what they share.
yesno wifi.interface "$wifi" '^ *en[0-9]+:$' "a Wi-Fi interface"
yesno wifi.awdl0 "$wifi" '^ *awdl0:$' "awdl0 listed"
yesno wifi.airdrop "$wifi" 'AirDrop: Supported' "AirDrop: Supported"
yesno wifi.auto_unlock "$wifi" 'Auto Unlock: Supported' "Auto Unlock: Supported"

# Bluetooth: Continuity advertises over Bluetooth LE, and LE audio/GATT shows in the supported services.
yesno bluetooth.controller "$bt" 'State: On' "controller state On"
yesno bluetooth.le "$bt" 'Supported services:.*GATT' "GATT in the supported services"

# Graphics.
yesno gpu.metal "$gpu" 'Metal Support: Metal [0-9]' "$(printf '%s\n' "$gpu" | awk -F': ' '/Metal Support/{print $2; exit}')"
yesno gpu.metal4 "$gpu" 'Metal Support: Metal 4' "Metal 4"

# Security.
fv=$(fdesetup status 2>/dev/null)
yesno security.filevault_usable "$fv" 'FileVault is (On|Off)' "$(printf '%s' "$fv" | head -1)"
sip=$(csrutil status 2>/dev/null)
yesno security.sip_reported "$sip" 'System Integrity Protection status' "$(printf '%s' "$sip" | head -1)"

# Power.
pw=$(pmset -g custom 2>/dev/null)
yesno power.pmset "$pw" ' sleep ' "pmset reports settings"
yesno power.powernap "$pw" ' powernap +1' "powernap 1"
batt=$(pmset -g batt 2>/dev/null)
yesno power.battery "$batt" 'InternalBattery' "an internal battery"

# Storage: trim on the boot drive's controller.
yesno storage.trim "$nvme" 'TRIM Support: Yes' "TRIM Support: Yes"

# Identity that Apple's services read.
serial=$(ioreg -rd1 -c IOPlatformExpertDevice 2>/dev/null | awk -F'"' '/IOPlatformSerialNumber/{print $4; exit}')
if [[ "$serial" =~ ^[A-Z0-9]{10,12}$ ]]; then row identity.serial yes "an Apple-shaped serial (10 characters on Apple silicon, 11 or 12 on Intel)"; else row identity.serial no "serial '${serial:-none}'"; fi
exit 0
