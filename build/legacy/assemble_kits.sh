#!/bin/bash
# assemble_kits.sh: gather, per release, everything built offline for it into one folder:
#   $B/kit/<release>/kexts      the kernel side (out/<release>)
#   $B/kit/<release>/userland   the Metal userland set (metal/stage/<release>), where the release has Metal
#   $B/kit/<release>/opengl     the OpenGL engine and its accelerator (gl/stage/<release>), where one is staged
#   $B/kit/<release>/options    install-time choices, none applied
#   README.txt, MANIFEST.sha256
# It copies build outputs only. Nothing is installed, loaded or run.
B=${B:?audit folder}; K=$B/kit; mkdir -p "$K"
#       release  kexts-dir  metal-dir  gl-dir
LIST="10.6 10.6 - 10.06
10.7 10.7 - 10.07
10.8 10.8 - 10.08
10.9 10.9 - 10.09
10.10 10.10 - 10.10
10.11 10.11 10.11 10.11
10.12 10.12 10.12 10.12
10.13 10.13 10.13 10.13
10.14 10.14 10.14 -
10.15 10.15 10.15 -
11 11.0 11 -
12 12.0 12 -
13 13.0 13 -
14 14.0 14 -"
echo "$LIST" | while read rel kd md gd; do
  D=$K/$rel; rm -rf "$D"; mkdir -p "$D/kexts"
  have_k=""; for x in NVRM NVRMFB NVRMAGDC NVAccel; do
    [ -f "$B/out/$kd/$x.kext/Contents/MacOS/$x" ] && { cp -R "$B/out/$kd/$x.kext" "$D/kexts/"; have_k="$have_k $x"; }
  done
  # 10.8 has no graphics-control class at all: the kext cannot link there. It is kept out of kexts/ so nobody loads
  # it by copying the folder. (On 10.9 and 10.10 it links since the reserved slots are no longer named there.)
  case $rel in 10.8) [ -d "$D/kexts/NVRMAGDC.kext" ] && { mkdir -p "$D/held-back"; mv "$D/kexts/NVRMAGDC.kext" "$D/held-back/"; have_k=${have_k/ NVRMAGDC/}; } ;; esac
  for x in NVRM NVRMFB NVRMAGDC NVAccel; do [ -f "$B/out/$kd/$x.missing.txt" ] && cp "$B/out/$kd/$x.missing.txt" "$D/kexts/$x.not-exported.txt"; done
  have_m=no; [ "$md" != "-" ] && [ -d "$B/metal/stage/$md/payload" ] && { mkdir -p "$D/userland"; cp -R "$B/metal/stage/$md/payload" "$B/metal/stage/$md/MANIFEST.txt" "$B/metal/stage/$md/IMPORTS.txt" "$D/userland/"; have_m=yes; }
  [ "$md" != "-" ] && [ -d "$B/metal/stage/$md/options" ] && cp -R "$B/metal/stage/$md/options" "$D/options"
  [ -d "$B/out-options/$kd/route-B" ] && { mkdir -p "$D/options/compositor-route-B-service-name-and-preference"; cp -R "$B/out-options/$kd/route-B/NVRMFB.kext" "$D/options/compositor-route-B-service-name-and-preference/"; }
  have_g=no; [ "$gd" != "-" ] && [ -d "$B/gl/stage/$gd" ] && { cp -R "$B/gl/stage/$gd" "$D/opengl"; have_g=yes; }
  # boot-config changes for releases below the builder's floor (written by the boot converter; a list, not a config)
  [ -f "$B/boot/variants/config-$rel.changes.txt" ] && cp "$B/boot/variants/config-$rel.changes.txt" "$D/boot-config-changes.txt"
  slots=""; [ -f "$B/vt/ours-$kd.json" ] && [ -f "$B/vt/fam-$kd.json" ] && slots=$(python3 "$B/vtfit.py" "$B/vt/ours-$kd.json" "$B/vt/fam-$kd.json" $( [ -f "$B/vt/base-$kd.json" ] && echo "$B/vt/base-$kd.json" ) 2>/dev/null | tail -1)
  {
    echo "macOS $rel: driver kit built offline"
    echo "======================================"
    echo "STATE: every file here was compiled and linked for this release and checked against this release's own kernel,"
    echo "libraries and class tables. NOTHING here has been installed, loaded or run on any machine."
    echo
    echo "kexts/     :$have_k"
    case " $have_k " in *" NVAccel "*) echo "             NVAccel is the Metal-side accelerator (subclasses Apple's accelerator family of this release).";;
      *) echo "             No NVAccel: this release has no Metal; acceleration is the OpenGL accelerator under opengl/.";; esac
    [ -n "$slots" ] && echo "             accelerator table check: $slots"
    # link test with this release's own kext linker, built from the release's published kernel source (10.6 to 10.15)
    if [ -f "$B/kxld/results/$rel.txt" ]; then
      echo "             link test with this release's own kernel kext linker (Apple's kexts as controls, then ours):"
      grep -E "^  com\.nullmoth\.[A-Za-z.]+: (LINKED|REFUSED)" "$B/kxld/results/$rel.txt" | sed -E 's/ \(linker return.*//; s/^  /               /'
      grep -E "controls linked|negative control" "$B/kxld/results/$rel.txt" | sed 's/^-- /               /'
      cp "$B/kxld/results/$rel.txt" "$D/kexts/link-test.txt"
    fi
    # the symbol check that writes the not-exported lists sees fewer libraries than the link test; where the linker
    # accepted the kext, its verdict stands and the stale list is dropped
    for x in NVRM NVRMFB NVRMAGDC NVAccel; do f="$D/kexts/$x.not-exported.txt"
      [ -f "$f" ] && grep -qE "^  com\.nullmoth\.$x: LINKED" "$B/kxld/results/$rel.txt" 2>/dev/null && rm -f "$f"; done
    for x in NVRM NVRMFB NVRMAGDC NVAccel; do f="$D/kexts/$x.not-exported.txt"; [ -s "$f" ] && echo "             $x: $(wc -l < "$f" | tr -d ' ') names the kernel or Apple's kexts do not export on this release (listed in kexts/$x.not-exported.txt)"; done
    case " $have_k " in *" NVRMAGDC "*) ;; *) echo "             No NVRMAGDC: the graphics-control family it attaches to does not exist on this release.";; esac
    echo "userland/  : $( [ $have_m = yes ] && echo "Metal plugin, translator, NVK, loader, codec library, daemon; payload/ mirrors the install paths (MANIFEST.txt)" || echo "none (Metal does not exist on this release)")"
    [ $have_m = yes ] && echo "             The data files in payload (bundle property list, ICD manifest, settings, allow list) are reference copies from a test package; the allow list must not ship."
    echo "opengl/    : $( [ $have_g = yes ] && echo "OpenGL engine + its accelerator kext (MANIFEST.json says where each file goes and the two steps before placing)" || echo "none staged here (10.14 and later use Apple's OpenGL-on-Metal over our Metal device; see options/)")"
    [ -f "$D/boot-config-changes.txt" ] && echo "boot       : boot-config-changes.txt lists what an OpenCore config for a current release must change for this one, and this release's limits (disk, USB, processor)"
    echo "options/   : $( [ -d "$D/options" ] && ls "$D/options" | tr '\n' ' ' || echo none)"
    echo
    echo "DESKTOP COMPOSITOR ON THIS RELEASE"
    case $rel in
      10.6|10.7|10.8|10.9|10.10|10.11) echo "  The window server composites with OpenGL only. A desktop with acceleration needs opengl/ working.";;
      10.12) echo "  OpenGL by default; Metal only with the window server's BootToMetalCompositor key.";;
      10.13|10.14) echo "  Metal only with route A (configuration key) or route B (service name + preference): options/. Owner's choice.";;
      *) echo "  Metal only. The Metal userland set carries it.";;
    esac
    echo
    echo "ORDER ON A TEST DISK (first command of each step; stop at the first one that fails and keep its output)"
    echo "  1. Kernel side: place kexts/ and test-load without caches, NVRM first:  kextutil -t -n NVRM.kext"
    echo "     If a kext is refused for an undefined name, the name is in its .not-exported.txt: that is the finding."
    echo "  2. Display: NVRMFB alone gives an unaccelerated desktop at the panel's resolution. Confirm that before anything else."
    [ $have_g = yes ] && echo "  3. OpenGL: opengl/MANIFEST.json, steps in before_placing, then its kext gate."
    [ $have_m = yes ] && echo "  4. Metal: copy userland/payload into place;  ioreg -c IOAccelerator -l | grep -E 'MetalPluginName|IOGLBundleName'"
    echo
    echo "KNOWN OPEN ITEMS FOR THIS RELEASE"
    case $rel in
      10.6) echo "  - OpenGL: the GPU path (Vulkan backend and driver) is linked for this release and import-clean, never executed; only the CPU rasteriser path has run. 64-bit kernel only."
            echo "  - AMD processors need a patched kernel below 10.13 (not part of this kit).";;
      10.7|10.8|10.9|10.10) echo "  - Two accelerators exist for 10.11 to 10.13 only; here the OpenGL accelerator is the only one."
            echo "  - AMD processors need a patched kernel below 10.13 (not part of this kit).";;
      10.11|10.12|10.13) echo "  - NVAccel (Metal) and the OpenGL accelerator both target the same card. Which one attaches, or how they share it,"
            echo "    is not decided: the OpenGL one ships gated (its personality cannot match until edited)."
            [ $rel = 10.11 ] && echo "  - 10.11: resource width/height offsets are inferred from 10.12 (no witness method on 10.11)."
            [ $rel != 10.13 ] && echo "  - AMD processors need a patched kernel below 10.13 (not part of this kit).";;
      10.14) echo "  - Route A or B must be chosen for a Metal desktop. OpenGL apps: Apple's OpenGL-on-Metal renderer, presence on 10.14 not verified.";;
      10.15) echo "  - Accelerator declared per release; two diagnostic-only offsets are known wrong (they only affect a log line).";;
      *) echo "  - Accelerator differs from the shipping one only by reserved pads in the tables; fields and sizes match the shipping release.";;
    esac
    case $rel in
      11|12|13|14) echo "  - No link test here: from 11 on kexts are linked into a kernel collection by a tool that needs an Intel Mac"
             echo "    running that release. First step on the test disk is that collection build; it stops before changing anything.";;
    esac
    case $rel in
      11|12) echo "  - The accelerator's long not-exported list is not a defect list: Apple ships no standalone accelerator family"
             echo "    binary for this release, so the kit check cannot see the family's names. Tables, sizes and fields were"
             echo "    checked against the release's kernel collection instead (map, section 2j/2k).";;
      10.8) echo "  - NVRMAGDC is in held-back/, not kexts/: this release has no graphics-control class, so it cannot link here."
             echo "    It only adds GPU selection and power policy; the display and the accelerator do not depend on it.";;
      10.6) echo "  - Link test: the ACPI family binary is the one from Apple's 10.6.8 update, which ships it without its property"
             echo "    list; the test used a stand-in list (identifier and libraries stated by hand). All three kexts linked.";;
    esac
  } > "$D/README.txt"
  ( cd "$D" && find . -type f ! -name MANIFEST.sha256 -print0 | sort -z | xargs -0 shasum -a 256 > MANIFEST.sha256 )
  echo "$rel: kexts[$have_k ] userland=$have_m opengl=$have_g files=$(wc -l < "$D/MANIFEST.sha256" | tr -d ' ') size=$(du -sh "$D" | cut -f1)"
done
{
  echo "Kits built offline, one folder per release. Nothing installed or run. See each README.txt."
  echo
  echo "release  what the kit holds"
  for d in 10.6 10.7 10.8 10.9 10.10 10.11 10.12 10.13 10.14 10.15 11 12 13 14; do [ -d "$K/$d" ] && echo "$d  $(ls "$K/$d" | tr '\n' ' ')"; done
  echo
  echo "Not here, on purpose:"
  echo "  10.5      the kernel is 32-bit only; the driver core is 64-bit only. Display through the firmware framebuffer only."
  echo "  15, 26    the shipping driver (release 2.9) is the build for these."
  echo "  27        its own lane (interpreter route and platform drivers); not built from this tree."
} > "$K/INDEX.txt"
cat "$K/INDEX.txt"
