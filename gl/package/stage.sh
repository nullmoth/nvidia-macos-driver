#!/bin/bash
# stage.sh <build outputs folder (holds rel-out/, engine-out/, kext-out/)> <stage dir>
# Assembles, per release, the folder tree of what would be placed on that release, plus a manifest. It only copies
# build outputs into the stage dir: nothing is installed, no system path is written, no installer is run.
#   10.7 to 10.13: engine with the context layer (x86_64) joined with the pass-through engine (i386), so 32-bit
#                  applications keep Apple's renderers; the accelerator kext of that release.
#   10.6:          pass-through engine in both slices (the context layer needs thread-local storage, which the
#                  compiler does not provide below 10.7); the accelerator kext.
set -uo pipefail
IN=${1:?}; ST=${2:?}; H=$(cd "$(dirname "$0")" && pwd); fail=0
E=System/Library/Frameworks/OpenGL.framework/Versions/A/Resources
for rel in 10.06 10.07 10.08 10.09 10.10 10.11 10.12 10.13; do
  short=$(echo $rel | sed 's/\.0/./'); d=$ST/$rel; rm -rf "$d"; mkdir -p "$d/$E/GLEngine.bundle" "$d/System/Library/Extensions"
  pt=$IN/engine-out/GLEngine-i386; own=$IN/rel-out/$rel/GLEngine-product; tier="context layer (x86_64) + pass-through (i386)"
  if [ "$rel" = 10.06 ]; then cp "$IN/engine-out/GLEngine" "$d/$E/GLEngine.bundle/GLEngine"; tier="pass-through (x86_64, i386)"
  elif [ -f "$own" ] && [ -f "$pt" ]; then lipo -create "$own" "$pt" -o "$d/$E/GLEngine.bundle/GLEngine" || fail=1
  else echo "$rel: missing build output"; fail=1; continue; fi
  sed "s/@REL@/$short/" "$H/GLEngine-Info.plist" > "$d/$E/GLEngine.bundle/Info.plist"
  k=$IN/kext-out/$short/NMLegacyAccel.kext; [ -d "$k" ] && /bin/cp -R "$k" "$d/System/Library/Extensions/" || { echo "$rel: no kext"; fail=1; }
  python3 - "$d" "$rel" "$tier" > "$d/MANIFEST.json" <<'PY'
import hashlib, json, os, subprocess, sys
d, rel, tier = sys.argv[1:4]; files = []
for dp, _, fs in os.walk(d):
    for f in sorted(fs):
        p = os.path.join(dp, f); r = os.path.relpath(p, d)
        if r == "MANIFEST.json": continue
        a = subprocess.run(["lipo", "-archs", p], capture_output=True, text=True).stdout.strip()
        files.append({"destination": "/" + r, "sha256": hashlib.sha256(open(p, "rb").read()).hexdigest(), "bytes": os.path.getsize(p), "architectures": a or None})
print(json.dumps({"release": rel, "engine": tier, "status": "staged only; not installed, not loaded, never run on a GPU",
  "before_placing": ["rename the system's GLEngine.bundle to NMOriginalEngine.bundle in the same folder (the engine forwards to it by that fixed path)",
                     "on 10.11 and later the folder is covered by system integrity protection, which must be off for the copy"],
  "kext_gate": "the personality's IOProviderClass names a class that does not exist; the kext cannot attach until that is changed for an approved run",
  "files": files}, indent=1))
PY
  n=$(python3 -c "import json,sys; print(len(json.load(open(sys.argv[1]))['files']))" "$d/MANIFEST.json")
  echo "$rel: staged $n files; engine $(lipo -archs "$d/$E/GLEngine.bundle/GLEngine"); kext $(lipo -archs "$d/System/Library/Extensions/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" 2>/dev/null); $tier"
done
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
