#!/bin/bash
# build_release_inputs.sh <out dir>: everything a release package takes from this tree that is built on the build Mac
# itself, from the tree this script sits in, laid out the way the release step reads it:
#   <out>/kexts/{nvrm,fb,accel,accel26,agdc,nmsmn}/<Name>.kext   <out>/userland/nvmtl-air-dis   <out>/nvk/libvulkan_nouveau.dylib
#   <out>/dlss-pkg/   <out>/boost/nvmtl-boost   <out>/vulkan-loader/
# The Metal plugin, the shader translator and the video codec library are built by their own scripts
# (build_plugin_release.sh, build_xlate_release.sh, build_nvencvt.sh), which take a git revision; the audio driver comes
# from its own repository. Run this from a checkout of the revision being released: the first line of <out>/BUILD.txt
# records it, and a tree with uncommitted changes is refused.
#
# Where the build Mac keeps what is not in this tree (set NULLMOTH_BUILD_ROOT, or each one):
#   NM_OGKM620           NVIDIA open-gpu-kernel-modules 620.70.06 with the RM core and NVKMS archives built
#                        (build_rm620.sh, build_kms620.sh)                   default $NULLMOTH_BUILD_ROOT/build/rm620/ogkm-620.70.06
#   NM_OGKM620_PRISTINE  the same release, untouched (the patch chain is checked against it)
#                                                                            default .../build/ogkm620/open-gpu-kernel-modules-620.70.06
#   NM_OGKM610           open-gpu-kernel-modules 610.57.04 (headers NVK is built against)
#                                                                            default .../build/ogkm620/open-gpu-kernel-modules-610.57.04
#   NM_MESA_GIT          a Mesa git clone holding nvk/MESA_BASE              default .../build/gpu-src/mesa
#   NM_LILU_DEBUG_KEXT, NM_VIRTUALSMC_SRC, NM_MACKERNELSDK   the Lilu DEBUG kext, the VirtualSMC source and MacKernelSDK
#                        at the commits the EFI's VirtualSMC set is built from; with all three set the keyboard backlight
#                        plugin and the display-audio plugin are built too (<out>/kexts/nmkbl, nmhda) with their EFI package, otherwise skipped
# build_nvk.sh reads its own tool paths from the environment (NVK_PATH, NVK_SYSROOT, PKG_CONFIG_PATH, LIBCLANG_PATH).
# STEPS="nvrm fb ..." builds only those. Each step logs to <out>/logs/<step>.log; the first failure stops the run.
set -uo pipefail
OUT=${1:?out dir}
HERE=$(cd "$(dirname "$0")/.." && pwd)
R=${NULLMOTH_BUILD_ROOT:-}
OGKM=${NM_OGKM620:-$R/build/rm620/ogkm-620.70.06}
PRISTINE=${NM_OGKM620_PRISTINE:-$R/build/ogkm620/open-gpu-kernel-modules-620.70.06}
OGKM610=${NM_OGKM610:-$R/build/ogkm620/open-gpu-kernel-modules-610.57.04}
MESA=${NM_MESA_GIT:-$R/build/gpu-src/mesa}
STEPS=${STEPS:-nvrm fb accel agdc nmsmn nmkbl nmhda efizip airdis dlss nvk boost vulkan}
for d in "$OGKM/src/nvidia" "$PRISTINE/src/nvidia" "$OGKM610/src/common" "$MESA/.git"; do
  [ -e "$d" ] || { echo "STOP: $d does not exist (see the paths at the top of this script)"; exit 2; }
done
if git -C "$HERE" rev-parse HEAD >/dev/null 2>&1; then
  [ -z "$(git -C "$HERE" status --porcelain --untracked-files=no)" ] || { echo "STOP: $HERE has uncommitted changes; release inputs are built from a committed revision"; exit 2; }
  REV=$(git -C "$HERE" rev-parse HEAD)
else
  REV=${NM_REV:?this tree is not a git checkout: set NM_REV to the revision it was exported from}
fi
mkdir -p "$OUT/logs"; OUT=$(cd "$OUT" && pwd)
echo "revision $REV" > "$OUT/BUILD.txt"
step() {   # <name> <command...>: run, log, stop on failure, record the last line of the log
  local n=$1; shift
  case " $STEPS " in *" $n "*) ;; *) return 0;; esac
  echo "== $n"
  if "$@" > "$OUT/logs/$n.log" 2>&1; then echo "$n: $(tail -1 "$OUT/logs/$n.log")" | tee -a "$OUT/BUILD.txt"
  else echo "STOP: $n failed (see $OUT/logs/$n.log)"; tail -15 "$OUT/logs/$n.log"; exit 1; fi
}
accel() {
  OGKM=$OGKM bash "$HERE/build/accel_build_all.sh" "$HERE/kexts/NVRM" "$OUT/accel-all" || return 1
  rm -rf "$OUT/kexts/accel" "$OUT/kexts/accel26"; mkdir -p "$OUT/kexts/accel" "$OUT/kexts/accel26"
  cp -R "$OUT/accel-all/15/NVAccel.kext" "$OUT/kexts/accel/" && cp -R "$OUT/accel-all/26/NVAccel.kext" "$OUT/kexts/accel26/"
}
nvk() {
  bash "$HERE/build/build_nvk.sh" "$MESA" "$OGKM610" "$OUT/nvk-build" 2.9 || return 1
  mkdir -p "$OUT/nvk" && cp "$OUT/nvk-build/libvulkan_nouveau.dylib" "$OUT/nvk/" && cat "$OUT/nvk-build/BUILD.txt"
}
boost() {
  [ -d "$OUT/nvk-build/mesa" ] || { echo "the boost daemon is built against NVK's patched tree: run the nvk step first"; return 1; }
  mkdir -p "$OUT/boost" && bash "$HERE/tools/nvmtl-boost/build.sh" "$OUT/nvk-build/mesa" "$OUT/boost/nvmtl-boost"
}
mkdir -p "$OUT/kexts" "$OUT/userland"
step nvrm   env NVRM_PRISTINE_OGKM="$PRISTINE" bash "$HERE/build/build_nvrm.sh" "$HERE/kexts/NVRM" "$OGKM" "$OUT/kexts/nvrm"
step fb     env OGKM="$OGKM" bash "$HERE/build/fb_build.sh" "$HERE/kexts" "$OUT/kexts/fb"
step accel  accel
step agdc   bash "$HERE/build/build_agdc.sh" "$HERE/kexts/NVRMAGDC" "$OUT/kexts/agdc"
step nmsmn  bash "$HERE/kexts/NMSMN/build.sh" "$OUT/kexts/nmsmn" 15.0
if [ -n "${NM_LILU_DEBUG_KEXT:-}" ] && [ -n "${NM_VIRTUALSMC_SRC:-}" ] && [ -n "${NM_MACKERNELSDK:-}" ]; then
  step nmkbl bash "$HERE/kexts/NMKbdBacklight/build.sh" "$NM_LILU_DEBUG_KEXT" "$NM_VIRTUALSMC_SRC" "$NM_MACKERNELSDK" "$OUT/kexts/nmkbl"
  # NVIDIA display audio through Apple's HD Audio driver (macOS 15), a Lilu plugin, and the package of both EFI drivers
  # that the 1401 builder pins (<out>/efi-drivers/NullMoth-EFI-drivers.zip)
  step nmhda bash "$HERE/kexts/NMHDA/build.sh" "$NM_LILU_DEBUG_KEXT" "$NM_MACKERNELSDK" "$OUT/kexts/nmhda"
  efizip() { mkdir -p "$OUT/efi-drivers" && python3 "$HERE/build/make_efi_drivers_zip.py" "$OUT/efi-drivers/NullMoth-EFI-drivers.zip" \
    "$OUT/kexts/nmkbl/NMKbdBacklight.kext" "$OUT/kexts/nmhda/NMHDA.kext" | head -1; }
  step efizip efizip
else echo "nmkbl, nmhda, efizip: skipped (NM_LILU_DEBUG_KEXT, NM_VIRTUALSMC_SRC and NM_MACKERNELSDK are not all set)" | tee -a "$OUT/BUILD.txt"; fi
step airdis bash "$HERE/build/build_air_dis.sh" "$OUT/userland"
step dlss   bash "$HERE/build/build_dlss_cache.sh" "$OUT/dlss-pkg"
step nvk    nvk
step boost  boost
# cmake and ninja for the loader are among NVK's build tools
step vulkan env PATH="${NVK_PATH:+$NVK_PATH:}$PATH" bash "$HERE/build/build_vulkan_loader.sh" "$OUT/vulkan-loader"
# every kext a step built must be whole: the binary and its Info.plist
want() { case " $STEPS " in *" $1 "*) shift; for k in "$@"; do n=${k##*/}
  [ -f "$OUT/kexts/$k.kext/Contents/MacOS/$n" ] && [ -f "$OUT/kexts/$k.kext/Contents/Info.plist" ] || { echo "STOP: $OUT/kexts/$k.kext is not complete"; exit 1; }
  done;; esac; }
want nvrm nvrm/NVRM; want fb fb/NVRMFB; want accel accel/NVAccel accel26/NVAccel; want agdc agdc/NVRMAGDC; want nmsmn nmsmn/NMSMN
echo "release inputs built from $REV in $OUT"
