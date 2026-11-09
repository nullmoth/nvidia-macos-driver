#!/bin/bash
# run_oracle.sh <out dir>: builds and runs the curve oracle on THIS Mac's GPU and compares it with curve_math.h. Refuses when the Mac is
# under memory pressure (swap above 70 percent of the live file, free memory under 25 percent, load above 12): it is the machine of
# whoever is working on it. Needs the Metal toolchain (xcrun metal) and an Apple GPU with curve support.
set -eu
H=$(cd "$(dirname "$0")" && pwd); O=${1:?out dir}; mkdir -p "$O"
read -r total used < <(sysctl -n vm.swapusage | awk '{gsub("M","",$3); gsub("M","",$6); print $3, $6}')
free_pct=$(memory_pressure 2>/dev/null | awk '/free percentage/ {gsub("%","",$5); print $5}')
load=$(sysctl -n vm.loadavg | awk '{print int($2)}')
swap_pct=$(awk -v u="$used" -v t="$total" 'BEGIN { if (t > 0) printf "%d", 100 * u / t; else print 0 }')
if [ "$swap_pct" -gt 70 ] || [ "${free_pct:-100}" -lt 25 ] || [ "$load" -gt 12 ]; then
  echo "REFUSED: swap ${swap_pct}% of the live file, free ${free_pct:-?}%, load $load: close to swap. Save work and reboot, then run again."; exit 5
fi
xcrun metal -target air64-apple-macos26.0 -c "$H/curve_oracle.metal" -o "$O/o.air"
xcrun metallib "$O/o.air" -o "$O/curve_oracle.metallib"
clang -fobjc-arc -O1 -I"$H" -o "$O/curve_oracle" "$H/curve_oracle.m" -framework Foundation -framework Metal
"$O/curve_oracle" "$O/curve_oracle.metallib" "$O/dump.txt" || { echo "the oracle stopped (a REFUSED line in $O/dump.txt names the error)"; }
cc -O2 -I"$H/.." -o "$O/compare_oracle" "$H/compare_oracle.c" -lm
"$O/compare_oracle" "$O/dump.txt"
