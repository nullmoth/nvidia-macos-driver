#!/bin/bash
# gen_release.sh <release e.g. 10.13> <era MMmm> [newer release whose names may be borrowed]: read that release's accelerator family (and Apple's vendor drivers
# for the names of its pure slots) and write the regenerated headers to gen-<era>.
B=${B:?audit folder}; cd $B; rel=$1; era=$2
fam=$(ls gl/full/$rel/Extensions/IOAcceleratorFamily2.kext/Contents/MacOS/IOAcceleratorFamily2 kdk/$rel/vendor/IOAcceleratorFamily2.kext/Contents/MacOS/IOAcceleratorFamily2 kdk/$rel/from-recovery/IOAcceleratorFamily2.kext/Contents/MacOS/IOAcceleratorFamily2 2>/dev/null | head -1)
[ -n "$fam" ] || { echo "$rel: no accelerator family binary"; exit 1; }
python3 vtdump.py "$fam" vt/fam-$rel.json | cut -c1-110
V=""
for k in gl/full/$rel/Extensions/GeForce.kext kdk/$rel/vendor/GeForce.kext kdk/$rel/vendor/AMDRadeonX4000.kext kdk/$rel/vendor/AMDRadeonX5000.kext kdk/$rel/vendor/AppleIntelKBLGraphics.kext kdk/$rel/vendor/AppleIntelSKLGraphics.kext kdk/$rel/vendor/AppleIntelHD5000Graphics.kext; do
  b="$k/Contents/MacOS/$(basename $k .kext)"; [ -f "$b" ] || continue
  o=vt/v$era-$(basename $k .kext).json; python3 vtdump.py "$b" $o >/dev/null && V="$V $o"
done
[ -n "${KEEP_NAMED:-}" ] && [ -f vt/named-$rel.json ] || python3 named.py vt/fam-$rel.json vt/named-$rel.json $V vt/fam-$rel.json | head -1
kern=$(find kdk/$rel -type f \( -name kernel -o -name mach_kernel \) -not -path "*dSYM*" -size +1000k | head -1)
gfx=$(find kdk/$rel -type f -name IOGraphicsFamily -not -path "*dSYM*" | head -1)
python3 vtdump.py "$kern" vt/kern-$rel.json >/dev/null; python3 vtdump.py "$gfx" vt/gfx-$rel.json >/dev/null
python3 - "$rel" <<'PY'
import json, sys
r = sys.argv[1]; k = json.load(open(f"vt/kern-{r}.json")); g = json.load(open(f"vt/gfx-{r}.json")); k["IOAccelerator"] = g["IOAccelerator"]
json.dump({c: k[c] for c in ("OSObject", "IOService", "IOUserClient", "IOAccelerator", "IORegistryEntry")}, open(f"vt/base-{r}.json", "w"))
PY
[ -n "${3:-}" ] && [ -f "vt/named-$3.json" ] && python3 borrow_names.py vt/named-$rel.json vt/named-$3.json | cut -c1-200
nm -arch x86_64 -gU "$fam" | awk '{print $NF}' > vt/famsyms-$rel.txt
rm -f vt/revealed-$rel.json; rm -rf gen-$era; python3 gen_era_headers.py vt/named-$rel.json vt/base-$rel.json legacy-src/kexts/NVRM/accel gen-$era $era vt/famsyms-$rel.txt "$fam" vt/revealed-$rel.json >/dev/null 2>&1
rm -rf gen-$era; python3 gen_era_headers.py vt/named-$rel.json vt/base-$rel.json legacy-src/kexts/NVRM/accel gen-$era $era vt/famsyms-$rel.txt "$fam" vt/revealed-$rel.json 2>&1 | grep -E "^era|unnamed pure [1-9]|left out|inline from|Traceback|Error|STOP" | cut -c1-260
