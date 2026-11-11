#!/bin/bash
# era_route_b.sh <release>...: build the framebuffer kext variant that also publishes a service of class NVDA (the
# install-time option "compositor route B" for 10.13 and 10.14), keep it under out-options/<release>/route-B/, then
# rebuild the release's normal set so out/<release> is the default again.
B=${B:?audit folder}; H=$(cd "$(dirname "$0")" && pwd)
for rel in "$@"; do
  NVRMFB_EXTRA=-DNVRMFB_PUBLISH_CLASS_NVDA ONLY=$rel bash "$H/era_all.sh" > "$B/out/build-$rel-routeB.log" 2>&1
  K="$B/out/$rel/NVRMFB.kext"; D="$B/out-options/$rel/route-B"
  [ -f "$K/Contents/MacOS/NVRMFB" ] || { echo "$rel: the variant did not link (see out/build-$rel-routeB.log)"; continue; }
  nm -arch x86_64 "$K/Contents/MacOS/NVRMFB" | grep -q "__ZTV4NVDA" || { echo "$rel: the variant has no NVDA class"; continue; }
  rm -rf "$D"; mkdir -p "$D"; cp -R "$K" "$D/"; P="$D/NVRMFB.kext/Contents/Info.plist"; PB=/usr/libexec/PlistBuddy
  id=$($PB -c "Print :CFBundleIdentifier" "$P")
  $PB -c "Add :IOKitPersonalities:NVDAMarker dict" -c "Add :IOKitPersonalities:NVDAMarker:CFBundleIdentifier string $id" \
      -c "Add :IOKitPersonalities:NVDAMarker:IOClass string NVDA" -c "Add :IOKitPersonalities:NVDAMarker:IOProviderClass string NVRMNVDAFramebuffer" \
      -c "Add :IOKitPersonalities:NVDAMarker:IOMatchCategory string NVDAMarker" "$P" || { echo "$rel: personality not added"; continue; }
  plutil -lint "$P" >/dev/null || { echo "$rel: property list invalid"; continue; }
  grep -A3 NVRMFB "$B/out/build-$rel-routeB.log" | grep -E "NVRMFB: [0-9]+ imports" | head -1
  echo "$rel: route-B framebuffer kept at $D ($(stat -f %z "$D/NVRMFB.kext/Contents/MacOS/NVRMFB") bytes)"
  ONLY=$rel bash "$H/era_all.sh" > "$B/out/build-$rel.log" 2>&1
  nm -arch x86_64 "$B/out/$rel/NVRMFB.kext/Contents/MacOS/NVRMFB" | grep -q "__ZTV4NVDA" && echo "$rel: STOP, the default set still carries the NVDA class"
  echo "$rel: default set rebuilt, accelerator errors $(grep -cE '^[^ ]+:[0-9]+:[0-9]+: error:' "$B/out/$rel/NVAccel.err")"
done
