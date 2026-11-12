#!/bin/bash
# stage.sh <build outputs folder (holds rel-out/, engine-out/, kext-out/)> <stage dir>
# Assembles, per release, the folder tree of what would be placed on that release, plus a manifest. It only copies
# build outputs into the stage dir: nothing is installed, no system path is written, no installer is run.
#   10.6 to 10.13: engine with the context layer and zink (x86_64) joined with the pass-through engine (i386), so
#                  32-bit applications keep Apple's renderers; the Vulkan driver of that release's floor beside
#                  it; the accelerator kext of that release.
set -uo pipefail
IN=${1:?}; ST=${2:?}; H=$(cd "$(dirname "$0")" && pwd); fail=0
E=System/Library/Frameworks/OpenGL.framework/Versions/A/Resources
for rel in 10.06 10.07 10.08 10.09 10.10 10.11 10.12 10.13; do
  short=$(echo $rel | sed 's/\.0/./'); d=$ST/$rel; rm -rf "$d"; mkdir -p "$d/$E/GLEngine.bundle" "$d/System/Library/Extensions"
  pt=$IN/engine-out/GLEngine-i386; own=$IN/rel-out/$rel/GLEngine-product; tier="context layer (x86_64) + pass-through (i386)"
  if [ -f "$own" ] && [ -f "$pt" ]; then lipo -create "$own" "$pt" -o "$d/$E/GLEngine.bundle/GLEngine" || fail=1
  else echo "$rel: missing build output"; fail=1; continue; fi
  sed "s/@REL@/$short/" "$H/GLEngine-Info.plist" > "$d/$E/GLEngine.bundle/Info.plist"
  # the Vulkan driver sits beside the engine: zink opens it by a path relative to the engine's own file
  case $rel in 10.06) fl=10.6;; 10.07|10.08|10.09|10.10) fl=10.7;; *) fl=10.11;; esac
  [ -f "$IN/nvk-out/$fl/libvulkan_nouveau.dylib" ] && cp "$IN/nvk-out/$fl/libvulkan_nouveau.dylib" "$d/$E/GLEngine.bundle/" || { echo "$rel: no Vulkan driver for floor $fl"; fail=1; }
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
  "gpu_path": {"engine": "Mesa state tracker with zink linked in; it takes the Vulkan path only when the accelerator's private client is found, otherwise the CPU rasteriser",
               "vulkan_driver": "libvulkan_nouveau.dylib beside the engine, opened by a path relative to the engine's own file",
               "kext": "surfaces in system memory by default (video memory with NMGPUSurfaces in the personality); a privileged client's flush is copied into the framebuffer kext's scanout",
               "frame_route": "rendered on the card, read back into the surface's pages at flush; no shared GPU buffer between the Vulkan driver and the accelerator yet"},
  "checked": {"linked_and_import_checked": "engine and Vulkan driver against this release's own libraries; kext against this release's kernel and graphics family",
              "run_on_the_build_host_cpu": "context and pixel tests through the CPU rasteriser; surface-table and scanout-copy tests of the kext's logic",
              "never_run": "zink, the Vulkan driver, the kext, and anything on a GPU or on this release"},
  "files": files}, indent=1))
PY
  n=$(python3 -c "import json,sys; print(len(json.load(open(sys.argv[1]))['files']))" "$d/MANIFEST.json")
  echo "$rel: staged $n files; engine $(lipo -archs "$d/$E/GLEngine.bundle/GLEngine"); kext $(lipo -archs "$d/System/Library/Extensions/NMLegacyAccel.kext/Contents/MacOS/NMLegacyAccel" 2>/dev/null); $tier"
done
echo "result: $([ $fail = 0 ] && echo PASS || echo FAIL)"; exit $fail
