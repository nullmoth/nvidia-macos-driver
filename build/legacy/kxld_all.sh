#!/bin/bash
# kxld_all.sh [release ...]: link test of our kexts with each release's own kext linker (10.6 to 10.15; from 11 on
# the kernel no longer links kexts itself). Reads out/<release>/ and, where staged, the OpenGL accelerator kext.
B=${B:?audit folder}; K=$B/kxld; mkdir -p "$K/results"
while read rel tag; do
  [ $# -gt 0 ] && { case " $* " in *" $rel "*) ;; *) continue;; esac; }
  [ -x "$K/bin/$tag/kxlink" ] || { echo "$rel: no linker built for $tag"; continue; }
  O=$K/ours/$rel; rm -rf "$O"; mkdir -p "$O"
  for k in "$B"/out/$rel/NV*.kext; do [ -d "$k" ] && cp -R "$k" "$O/"; done
  g=$(printf '%s' "$rel" | awk -F. '{printf "%d.%02d", $1, $2}')
  for k in $(find "$B/gl/stage/$g" -name "*.kext" -maxdepth 4 2>/dev/null); do cp -R "$k" "$O/"; done
  roots="$B/kdk/$rel"; [ -d "$K/sys/$rel" ] && roots="$K/sys/$rel $B/kdk/$rel"
  st=0; [ "$rel" = 10.6 ] && st=1
  KX_STATE=$st python3 "$K/kxld_run.py" $rel "$K/bin/$tag/kxlink" ${roots%% *} "$O" ${roots#* } > "$K/results/$rel.txt" 2>&1
  grep -E "RESULT|STOP" "$K/results/$rel.txt" || tail -2 "$K/results/$rel.txt"
done <<LIST
10.6 xnu-1504.15.3
10.7 xnu-1699.32.7
10.8 xnu-2050.48.11
10.9 xnu-2422.115.4
10.10 xnu-2782.40.9
10.11 xnu-3248.60.10
10.12 xnu-3789.70.16
10.13 xnu-4570.71.2
10.14 xnu-4903.270.47
10.15 xnu-6153.141.1
LIST
