#!/bin/bash
# import_matrix.sh: every binary built for a floor against the real libraries of the release with that floor.
# AUDIT = the audit folder. Output: logs/import-matrix.txt
A=${AUDIT:?}; cd "$A" || exit 2; O=logs/import-matrix.txt; : > $O
echo "controls" >> $O
python3 tools/importcheck.py $A/sys/10.11 $A/floors-fix/NVMTLDriver-15.0 | head -1 | sed 's/^/  positive (must report missing): /' >> $O
python3 tools/importcheck.py $A/dsc26 $A/floors-fix/NVMTLDriver-15.0 $A/nvkfix-15.0/libvulkan_nouveau.dylib $A/userland/loader-15.0/libvulkan.1.dylib $A/userland/xlate-10.12/libnvmtl_translate.dylib $A/userland/small-15.0/libnvencvt.dylib $A/userland/small-15.0/libnvmtl_osver.dylib $A/userland/small-15.0/nvmtl-air-dis | sed 's/^/  negative (the newest system): /' >> $O
for pair in 14:14.0 13:13.0 12:12.0 11:11.0 10.15:10.15 10.14:10.14 10.13:10.13 10.12:10.12 10.11:10.11; do
  R=${pair%%:*}; F=${pair##*:}; echo "== release $R (binaries built for floor $F)" >> $O
  B="$A/floors-fix/NVMTLDriver-$F"
  for x in "$A/nvkfix-$F/libvulkan_nouveau.dylib" "$A/userland/loader-$F/libvulkan.1.dylib" "$A/userland/xlate-10.12/libnvmtl_translate.dylib" "$A/userland/small-$F/libnvencvt.dylib" "$A/userland/small-$F/libnvmtl_osver.dylib" "$A/userland/small-$F/nvmtl-air-dis"; do
    [ -f "$x" ] && B="$B $x" || echo "  not built: ${x#$A/}" >> $O
  done
  python3 tools/importcheck.py $A/sys/$R $B >> $O
done
grep -c . $O
