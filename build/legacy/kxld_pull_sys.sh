#!/bin/bash
# kxld_pull_sys.sh: for each release, fetch Apple's recovery image, copy out (read-only attach) the kernel, the
# kernel-interface kexts (System.kext) and the families our kexts link against into $K/sys/<release>/, then delete
# the image. Everything the link test reads then comes from one build of the release.
B=${B:?audit folder}; K=$B/kxld; cd $B/macrecovery || exit 2
while read name board mlb; do
  D=$K/sys/$name; [ -f "$D/done" ] && { echo "$name already pulled"; continue; }
  R=$K/rec/$name; mkdir -p "$R" "$D"
  python3 macrecovery.py -b $board -m $mlb -o "$R" download > "$R/fetch.log" 2>&1 || { echo "$name fetch failed"; continue; }
  dmg=$(ls -S $R/*.dmg | head -1); mp=$(mktemp -d /tmp/kxA.XXXX); mp2=""
  ( echo Y | PAGER=cat hdiutil attach -nobrowse -readonly -noverify -mountpoint "$mp" "$dmg" >/dev/null 2>&1 ) & hp=$!
  ( sleep 180; kill $hp 2>/dev/null ) & wp=$!; wait $hp; arc=$?; kill $wp 2>/dev/null
  [ $arc = 0 ] || { echo "$name attach failed"; continue; }
  S=$mp
  if [ ! -d "$S/System/Library/Extensions" ]; then
    inner=$(find "$mp" -maxdepth 3 -name BaseSystem.dmg 2>/dev/null | head -1)
    if [ -n "$inner" ]; then mp2=$(mktemp -d /tmp/kxB.XXXX); hdiutil attach -nobrowse -readonly -noverify -mountpoint "$mp2" "$inner" >/dev/null 2>&1 && S=$mp2; fi
  fi
  n=0
  rm -rf "$D"/*.kext; mkdir -p "$D/Extensions"
  /bin/cp -R "$S/System/Library/Extensions/" "$D/Extensions/" 2>/dev/null
  n=$(ls "$D/Extensions" | wc -l | tr -d ' ')
  kern=none
  for kp in System/Library/Kernels/kernel mach_kernel; do [ -f "$S/$kp" ] && { /bin/cp "$S/$kp" "$D/kernel"; kern=$kp; break; }; done
  /bin/cp "$S/System/Library/CoreServices/SystemVersion.plist" "$D/" 2>/dev/null
  [ -n "$mp2" ] && { hdiutil detach -force "$mp2" >/dev/null 2>&1; rmdir "$mp2" 2>/dev/null; }
  hdiutil detach -force "$mp" >/dev/null 2>&1; rmdir "$mp" 2>/dev/null
  rm -f $R/*.dmg $R/*.chunklist
  [ $n -gt 50 ] && touch "$D/done"
  echo "$name: $n kexts, $(du -sh "$D" | cut -f1), kernel=$kern, build $(/usr/libexec/PlistBuddy -c 'Print :ProductVersion' -c 'Print :ProductBuildVersion' $D/SystemVersion.plist 2>/dev/null | tr '\n' ' ')"
done <<LIST
10.13 Mac-7BA5B2D9E42DDD94 00000000000J80300
10.7 Mac-2E6FAB96566FE58C 00000000000F25Y00
10.8 Mac-7DF2A3B5E5D671ED 00000000000F65100
10.9 Mac-F60DEB81FF30ACF6 00000000000FNN100
10.10 Mac-E43C1C25D4880AD6 00000000000GDVW00
10.11 Mac-FFE5EF870D7BA81A 00000000000GQRX00
10.12 Mac-77F17D7DA9285301 00000000000J0DX00
10.14 Mac-7BA5B2DFE22DDD8C 00000000000KXPG00
10.15 Mac-CFF7D910A743CAAF 00000000000PHCD00
LIST
