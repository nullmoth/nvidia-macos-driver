#!/bin/bash
# full_scan.sh <release> <InstallAssistant package>: read a full installer without installing it.
# Unpacks the support image into AUDIT/full-work/<release>, attaches it read-only, lists the installed system from
# its bill of materials (files.txt), and extracts only the named graphics bundles from the payloads into keep/.
# The support image and working copies are removed at the end; the package itself is never touched.
A=${AUDIT:?}; R=$1; P=$2; W=$A/full-work/$R; mkdir -p "$W/keep"; cd "$W" || exit 2
[ -f SharedSupport.dmg ] || xar -xf "$P" SharedSupport.dmg || { echo "unpack failed"; exit 1; }
M=$(echo Y | PAGER=cat hdiutil attach -nobrowse -readonly -noverify SharedSupport.dmg 2>/dev/null | awk -F'\t' '/\/Volumes\//{print $NF}' | tail -1)
[ -d "$M" ] || { echo "attach failed"; exit 1; }
Z=$(find "$M" -name '*.zip' -path '*MacSoftwareUpdate*' | head -1)
unzip -l "$Z" > zip-list.txt
unzip -o -q "$Z" AssetData/post.bom -d . && lsbom -s AssetData/post.bom > files.txt
echo "$R files in the installed system: $(grep -c . files.txt)"
RX='^System/Library/Extensions/(GeForceMTLDriver|AppleIntelKBLGraphicsMTLDriver|AMDRadeonX6000MTLDriver|AppleMetalOpenGLRenderer|GeForceGLDriver)\.bundle(/|$)|^System/Library/SystemProfiler/SPDisplaysReporter\.spreporter(/|$)|^System/Library/PrivateFrameworks/SkyLight\.framework/Versions/A/Resources/Configuration\.plist$|^System/Library/Frameworks/ScreenCaptureKit\.framework(/|$)'
for p in $(awk '{print $4}' zip-list.txt | grep -E '^AssetData/payloadv2/payload\.[0-9]+$' | sort); do
  unzip -p "$Z" "$p" | aa extract -d keep -include-regex "$RX" > /dev/null 2>> aa.err
done
hdiutil detach -force "$M" > /dev/null 2>&1
rm -f SharedSupport.dmg; rm -rf AssetData
echo "$R kept: $(du -sh keep | cut -f1), $(find keep -type f | wc -l | tr -d ' ') files"; echo DONE > scan.done
